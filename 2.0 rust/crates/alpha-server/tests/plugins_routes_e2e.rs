//! 决策器（`/api/dispatchers*`）+ 评估器（`/api/evaluators*`）共 17 个接口的端到端测试。
//!
//! # 为什么这批测试比源管理那批更麻烦
//!
//! 源管理只碰 `config/sources.yaml`；插件路由**同时碰三处状态**：
//!
//! 1. SQLite 里的 `dispatchers` / `evaluators` 两张表
//! 2. 磁盘上的 `.rhai` 插件文件
//! 3. `PluginRegistry` 的内存缓存（按 `modified` 时间戳做的缓存）
//!
//! 任何一处没复位，用例之间就会互相污染。所以 [`sandbox()`] 除了拿锁，
//! 还要清空插件目录、清空两张表、作废注册表缓存。
//!
//! # 为什么把 `Store` 也留在 `TestRoot` 里
//!
//! 好几个断言要**绕过 HTTP 直接读库**（例如「内建插件确实被 seed 进去了」），
//! 或者直接插一条内建记录（`delete` 用例要先有内建项才测得到 400）。
//! 把 `AppState` 整份留着比每次重新 `Store::open` 简单得多，
//! 也保证读写的确实是那个被路由持有的库。

use std::path::PathBuf;
use std::sync::OnceLock;

use alpha_server::auth::layer::{CSRF_HEADER, CSRF_HEADER_VALUE};
use alpha_server::{build_router, AppState, Config};
use alpha_store::{NewPlugin, PluginTable};
use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use http_body_util::BodyExt;
use serde_json::{json, Value};
use tower::ServiceExt;

struct TestRoot {
    _dir: tempfile::TempDir,
    app: Router,
    state: AppState,
}

fn root() -> &'static TestRoot {
    static ROOT: OnceLock<TestRoot> = OnceLock::new();
    ROOT.get_or_init(|| {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path().to_path_buf();

        std::fs::create_dir_all(base.join("config")).unwrap();
        std::fs::write(base.join("config/sources.yaml"), "sources: []\n").unwrap();

        // preview 要真的跑一遍评估器，而评估器要查图鉴 ——
        // `alpha_core::pokedex` 是个进程级 `OnceCell`，第一次用到就从
        // `ALPHA_ROOT/data/pokedex.json` 读。所以这里得把仓库里那份拷过来。
        //
        // 用 `CARGO_MANIFEST_DIR` 定位仓库根而不是 cwd：
        // 集成测试的 cwd 是**包根**（`crates/alpha-server`），不是工作区根。
        let repo = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .to_path_buf();
        std::fs::create_dir_all(base.join("data")).unwrap();
        for f in ["pokedex.json", "aliases.json"] {
            let src = repo.join("data").join(f);
            if src.exists() {
                std::fs::copy(&src, base.join("data").join(f)).unwrap();
            }
        }

        // `ALPHA_ROOT` 必须**在**任何 `project_root()` 调用之前设好。
        std::env::set_var("ALPHA_ROOT", &base);

        let cfg = Config {
            host: "127.0.0.1".into(),
            port: 0,
            secret: "test-secret-value-at-least-32-bytes-long".into(),
            data_dir: base.join("data"),
            config_dir: base.join("config"),
            session_ttl: 3600,
            envs: std::collections::HashMap::new(),
        };
        let store = alpha_store::Store::open(cfg.db_path()).unwrap();
        store.init().unwrap();
        let state = AppState::new(cfg, store).with_registry(base.join("data/plugins"));
        let app = build_router(state.clone());

        TestRoot {
            _dir: dir,
            app,
            state,
        }
    })
}

/// 插件目录基址。与 `with_registry` 传进去的路径必须一致。
fn plugins_root() -> PathBuf {
    PathBuf::from(std::env::var("ALPHA_ROOT").unwrap()).join("data/plugins")
}

/// 拿排他锁，并把插件侧的全部状态复位。
///
/// guard **必须**绑到变量上（`let _g = sandbox().await;`），
/// 写成 `sandbox();` 的话锁在语句结束时就放了 —— 测试会偶发失败。
///
/// 用 `tokio::sync::Mutex` 而非 `std::sync::Mutex`：guard 要跨 `await`
/// 持有，`std::sync::MutexGuard` 跨 `await` 在单线程 runtime 上会死锁
/// （clippy 的 `await_holding_lock` 警告的正是这个）。
async fn sandbox() -> tokio::sync::MutexGuard<'static, ()> {
    static LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
    let guard = LOCK.lock().await;
    let st = root();

    for sub in ["dispatchers", "evaluators"] {
        let dir = plugins_root().join(sub);
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
    }

    // 清空两张表（id 不重置 —— 用例不该依赖具体 id 值）
    {
        let conn = st.state.store.lock().unwrap();
        conn.execute("DELETE FROM dispatchers", []).unwrap();
        conn.execute("DELETE FROM evaluators", []).unwrap();
    }
    st.state.registry.as_ref().unwrap().invalidate();

    guard
}

/// 往库里塞一条插件记录，并在磁盘上落一份真实的 `.rhai` 文件。
///
/// 内建插件的定义就是「随发行版装好的」，所以这里直接写库 + 写盘，
/// **不**走 upload 接口。
///
/// 返回新记录的 id。
fn seed_builtin(side: Side, name: &str, priority: i64) -> i64 {
    seed_plugin(side, name, priority, true)
}

/// 同上，但能控制 `enabled` 与 `is_builtin` —— 给需要「库里已经有个
/// 停用插件」这类前置状态的用例用。
fn seed_plugin(side: Side, name: &str, priority: i64, enabled: bool) -> i64 {
    let st = root();
    let slug: String = name
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect();
    // 名字里的非 ASCII 会被折成 `_`，所以同一个用例里两次调用不同名字
    // 也可能撞名。加个自增序号彻底避免。
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let filename = format!("builtin_{slug}_{n}.rhai");

    let path = plugins_root().join(side.dir()).join(&filename);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, side.sample_source()).unwrap();

    st.state
        .store
        .add_plugin(
            side.table(),
            &NewPlugin {
                name: name.to_string(),
                filename,
                description: format!("内建{name}"),
                priority,
                enabled,
                active: false,
                is_builtin: true,
            },
        )
        .unwrap()
}

/// 走真实的 `/api/login` 拿会话 cookie。
///
/// 用 `OnceCell` 缓存**结果**：不能在这里 `block_on`，
/// 因为用例已经在 tokio runtime 里（会 panic「Cannot start a runtime
/// from within a runtime」）。
async fn cookie() -> String {
    use tokio::sync::OnceCell;
    static COOKIE: OnceCell<String> = OnceCell::const_new();

    COOKIE
        .get_or_init(|| async {
            let req = Request::builder()
                .method("POST")
                .uri("/api/login")
                .header("content-type", "application/json")
                .header(CSRF_HEADER, CSRF_HEADER_VALUE)
                .body(Body::from(
                    serde_json::to_vec(&json!({
                        "username": "hanyx", "password": "test-password"
                    }))
                    .unwrap(),
                ))
                .unwrap();
            let resp = root().app.clone().oneshot(req).await.unwrap();
            assert_eq!(resp.status(), StatusCode::OK, "测试登录应该成功");
            resp.headers()
                .get("set-cookie")
                .expect("登录要下发 cookie")
                .to_str()
                .unwrap()
                .split(';')
                .next()
                .unwrap()
                .to_string()
        })
        .await
        .clone()
}

async fn send(req: Request<Body>) -> (StatusCode, Value) {
    let resp = root().app.clone().oneshot(req).await.unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, value)
}

async fn get(path: &str) -> (StatusCode, Value) {
    let c = cookie().await;
    send(
        Request::builder()
            .uri(path)
            .header("cookie", c)
            .body(Body::empty())
            .unwrap(),
    )
    .await
}

async fn post(path: &str, body: Value) -> (StatusCode, Value) {
    let c = cookie().await;
    send(
        Request::builder()
            .method("POST")
            .uri(path)
            .header("content-type", "application/json")
            .header("cookie", c)
            .header(CSRF_HEADER, CSRF_HEADER_VALUE)
            .body(Body::from(serde_json::to_vec(&body).unwrap()))
            .unwrap(),
    )
    .await
}

/// 带会话的 GET，返回**原始响应**（要读头部/正文原文的用例用）。
async fn get_raw(path: &str) -> axum::response::Response {
    let c = cookie().await;
    root().app
        .clone()
        .oneshot(
            Request::builder()
                .uri(path)
                .header("cookie", c)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap()
}

/// 构造一个 multipart 请求体，模拟浏览器 `<input type=file>` 提交。
fn multipart_body(boundary: &str, filename: &str, content: &str) -> Body {
    let mut buf = String::new();
    buf.push_str(&format!("--{boundary}\r\n"));
    buf.push_str(&format!(
        "Content-Disposition: form-data; name=\"file\"; filename=\"{filename}\"\r\n"
    ));
    buf.push_str("Content-Type: text/plain\r\n\r\n");
    buf.push_str(content);
    buf.push_str(&format!("\r\n--{boundary}--\r\n"));
    Body::from(buf)
}

async fn upload(path: &str, filename: &str, content: &str) -> (StatusCode, Value) {
    let c = cookie().await;
    let boundary = "----test-boundary-7777";
    send(
        Request::builder()
            .method("POST")
            .uri(path)
            .header("cookie", c)
            .header(CSRF_HEADER, CSRF_HEADER_VALUE)
            .header(
                "content-type",
                format!("multipart/form-data; boundary={boundary}"),
            )
            .body(multipart_body(boundary, filename, content))
            .unwrap(),
    )
    .await
}

/// 侧别 —— 测试里用来把两套同样的用例参数化。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Side {
    Dispatcher,
    Evaluator,
}

impl Side {
    fn dir(self) -> &'static str {
        match self {
            Side::Dispatcher => "dispatchers",
            Side::Evaluator => "evaluators",
        }
    }

    fn api(self) -> &'static str {
        match self {
            Side::Dispatcher => "dispatchers",
            Side::Evaluator => "evaluators",
        }
    }

    fn table(self) -> PluginTable {
        match self {
            Side::Dispatcher => PluginTable::Dispatchers,
            Side::Evaluator => PluginTable::Evaluators,
        }
    }

    /// 一份能通过编译的插件源码（入口函数名按侧别不同）。
    fn sample_source(self) -> String {
        match self {
            Side::Dispatcher => {
                "// @name 样例决策器\nfn dispatch(boss, ctx) { `打 ${boss.name}` }\n".to_string()
            }
            Side::Evaluator => {
                "// @name 样例评估器\nfn evaluate(boss, ctx) {\n  #{ score: 80, label: \"值得打\" }\n}\n"
                    .to_string()
            }
        }
    }

    /// 能通过编译但**缺入口函数**的源码 —— 测「先编译再写盘」用。
    fn source_without_entry(self) -> String {
        match self {
            Side::Dispatcher => "fn something_else(boss, ctx) { 1 }\n".to_string(),
            Side::Evaluator => "fn something_else(boss, ctx) { 1 }\n".to_string(),
        }
    }

    /// 语法就错的源码。
    fn broken_source(self) -> String {
        "fn dispatch(boss, ctx) { ((( \n".to_string()
    }
}

// ------------------------------------------------------------ 鉴权（17 条全覆盖）

#[tokio::test]
async fn all_plugin_routes_require_login() {
    let _g = sandbox().await;
    // 未带 cookie，逐条打过去，全部该是 401。
    let paths: Vec<String> = [Side::Dispatcher, Side::Evaluator]
        .iter()
        .flat_map(|s| {
            let api = s.api();
            vec![
                format!("/api/{api}"),
                format!("/api/{api}/template"),
                format!("/api/{api}/upload"),
                format!("/api/{api}/1/enable"),
                format!("/api/{api}/1/activate"),
                format!("/api/{api}/1/edit"),
                format!("/api/{api}/1/delete"),
                format!("/api/{api}/1/download"),
            ]
        })
        .chain(std::iter::once("/api/evaluators/preview".to_string()))
        .collect();

    assert_eq!(paths.len(), 17, "17 条插件路由，一条都不能漏");

    for path in paths {
        let resp = root()
            .app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(&path)
                    .header(CSRF_HEADER, CSRF_HEADER_VALUE)
                    .header("content-type", "application/json")
                    .body(Body::from("{}"))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            resp.status(),
            StatusCode::UNAUTHORIZED,
            "{path} 未登录时该返回 401，实际 {}",
            resp.status()
        );
    }
}

#[tokio::test]
async fn write_routes_reject_missing_csrf() {
    let _g = sandbox().await;
    let c = cookie().await;

    for path in [
        "/api/dispatchers/1/enable",
        "/api/dispatchers/1/activate",
        "/api/evaluators/1/edit",
    ] {
        let (status, body) = send(
            Request::builder()
                .method("POST")
                .uri(path)
                .header("cookie", &c)
                .header("content-type", "application/json")
                .body(Body::from("{}"))
                .unwrap(),
        )
        .await;
        // 400 而不是 403：CSRF 校验在中间件里和「参数不合法」用同一个错误类型，
        // 理由是这两者都归「这个请求本身就不该被受理」。
        // 关键在于**必须被拒**，且不能是 401（cookie 是有效的）。
        assert_eq!(status, StatusCode::BAD_REQUEST, "{path} 缺 CSRF 头该被拒");
        assert_ne!(status, StatusCode::UNAUTHORIZED, "cookie 有效，不是鉴权问题");
        assert!(
            body["error"].as_str().unwrap_or("").contains("CSRF"),
            "{path} 的错误信息该说明是 CSRF：{body}"
        );
    }
}

// ------------------------------------------------------------ 列表 / 排序

#[tokio::test]
async fn list_returns_empty_arrays_when_nothing_registered() {
    let _g = sandbox().await;
    for side in [Side::Dispatcher, Side::Evaluator] {
        let (status, body) = get(&format!("/api/{}", side.api())).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body, json!([]), "{side:?} 空库该返回空数组");
    }
}

#[tokio::test]
async fn list_is_ordered_by_priority_then_id() {
    let _g = sandbox().await;
    // 故意按「非排序」的顺序插入
    let mid = seed_builtin(Side::Dispatcher, "mid", 50);
    let low = seed_builtin(Side::Dispatcher, "low", 10);
    let high = seed_builtin(Side::Dispatcher, "high", 90);

    let (status, body) = get("/api/dispatchers").await;
    assert_eq!(status, StatusCode::OK);
    let ids: Vec<i64> = body
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v["id"].as_i64().unwrap())
        .collect();
    assert_eq!(ids, vec![low, mid, high], "该按 priority 升序");
}

#[tokio::test]
async fn list_rows_expose_the_frontend_fields() {
    let _g = sandbox().await;
    seed_builtin(Side::Evaluator, "评估器甲", 5);

    let (_, body) = get("/api/evaluators").await;
    let row = &body.as_array().unwrap()[0];
    for key in [
        "id",
        "name",
        "filename",
        "description",
        "priority",
        "enabled",
        "active",
        "is_builtin",
    ] {
        assert!(row.get(key).is_some(), "列表项缺字段 {key}: {row}");
    }
    assert_eq!(row["is_builtin"], json!(true));
}

// ------------------------------------------------------------ enable 的自动激活怪癖

#[tokio::test]
async fn enabling_first_plugin_also_activates_it() {
    let _g = sandbox().await;
    // 库里没有任何激活项 —— 打开第一个插件时应该顺手激活它。
    // 这是原版的怪癖，用户「启用新插件」的真实意图通常就是「开始用它」。
    let id = seed_plugin(Side::Dispatcher, "alpha", 1, false);

    let (status, body) = post(
        &format!("/api/dispatchers/{id}/enable"),
        json!({"enabled": true}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["ok"], json!(true));

    let row = root()
        .state
        .store
        .get_plugin(PluginTable::Dispatchers, id)
        .unwrap()
        .unwrap();
    assert!(row.enabled, "enable 之后该是启用的");
    assert!(row.active, "库里本来没有激活项，该被顺手激活");
}

#[tokio::test]
async fn enabling_a_plugin_when_one_is_already_active_does_not_steal_active() {
    let _g = sandbox().await;
    let first = seed_builtin(Side::Dispatcher, "first", 1);
    let second = seed_builtin(Side::Dispatcher, "second", 2);

    // 先激活第一个
    let (st1, _) = post(&format!("/api/dispatchers/{first}/activate"), json!({})).await;
    assert_eq!(st1, StatusCode::OK);

    // 再启用第二个 —— 不该抢走 active
    let (st2, _) = post(
        &format!("/api/dispatchers/{second}/enable"),
        json!({"enabled": true}),
    )
    .await;
    assert_eq!(st2, StatusCode::OK);

    let active = root()
        .state
        .store
        .active_plugin(PluginTable::Dispatchers)
        .unwrap()
        .unwrap();
    assert_eq!(active.id, first, "已有激活项时，enable 不该顺手改激活项");
}

#[tokio::test]
async fn enable_defaults_to_true_when_field_missing() {
    let _g = sandbox().await;
    let id = seed_builtin(Side::Dispatcher, "x", 1);
    // 先关掉
    post(&format!("/api/dispatchers/{id}/enable"), json!({"enabled": false}))
        .await;
    // 再发一个不带 enabled 字段的请求 —— 原版 `body.get("enabled", True)`
    let (status, _) = post(&format!("/api/dispatchers/{id}/enable"), json!({})).await;
    assert_eq!(status, StatusCode::OK);
    let row = root()
        .state
        .store
        .get_plugin(PluginTable::Dispatchers, id)
        .unwrap()
        .unwrap();
    assert!(row.enabled, "缺 enabled 字段时该默认启用");
}

#[tokio::test]
async fn only_evaluators_write_logs_on_enable_and_activate() {
    let _g = sandbox().await;
    let d = seed_builtin(Side::Dispatcher, "d", 1);
    let e = seed_builtin(Side::Evaluator, "e", 1);

    // `LogFilter::default()` 的 limit 是 0，会被 clamp 成 1 —— 那样只能看到
    // 最新一条，数不出「多了几条」。所以显式给个大 limit。
    let logs = || {
        root()
            .state
            .store
            .list_logs(&alpha_store::LogFilter::new(1000))
            .unwrap()
            .len()
    };
    let before = logs();

    // 决策器的 enable + activate —— 都不该写日志
    post(&format!("/api/dispatchers/{d}/enable"), json!({"enabled": true})).await;
    post(&format!("/api/dispatchers/{d}/activate"), json!({})).await;
    let mid = logs();
    assert_eq!(mid, before, "决策器的 enable/activate 不该写日志（原版如此）");

    // 评估器的 enable + activate —— 都该写
    post(&format!("/api/evaluators/{e}/enable"), json!({"enabled": true})).await;
    post(&format!("/api/evaluators/{e}/activate"), json!({})).await;
    let after = logs();
    assert!(
        after > mid,
        "评估器的 enable/activate 该写日志（原版如此），实际 {mid} -> {after}"
    );
}

#[tokio::test]
async fn activate_unknown_id_is_404() {
    let _g = sandbox().await;
    let (status, _) = post("/api/dispatchers/999999/activate", json!({})).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

// ------------------------------------------------------------ edit

#[tokio::test]
async fn edit_only_priority_keeps_everything_else() {
    let _g = sandbox().await;
    let id = seed_builtin(Side::Dispatcher, "原名", 10);

    let (status, body) = post(&format!("/api/dispatchers/{id}/edit"), json!({"priority": 7})).await;
    assert_eq!(status, StatusCode::OK, "{body}");

    let row = root()
        .state
        .store
        .get_plugin(PluginTable::Dispatchers, id)
        .unwrap()
        .unwrap();
    assert_eq!(row.priority, 7);
    assert_eq!(row.name, "原名", "没提交 name 时不该被改");
    assert_eq!(row.description, "内建原名", "没提交 description 时不该被清空");
}

#[tokio::test]
async fn edit_can_rename_and_change_priority() {
    let _g = sandbox().await;
    let id = seed_builtin(Side::Dispatcher, "旧名", 10);

    let (status, _) = post(
        &format!("/api/dispatchers/{id}/edit"),
        json!({"name": "新名", "priority": 3}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let row = root()
        .state
        .store
        .get_plugin(PluginTable::Dispatchers, id)
        .unwrap()
        .unwrap();
    assert_eq!(row.name, "新名");
    assert_eq!(row.priority, 3);
}

#[tokio::test]
async fn edit_rejects_duplicate_name() {
    let _g = sandbox().await;
    let a = seed_builtin(Side::Dispatcher, "甲", 1);
    seed_builtin(Side::Dispatcher, "乙", 2);

    let (status, body) = post(&format!("/api/dispatchers/{a}/edit"), json!({"name": "乙"})).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert!(
        body["error"].as_str().unwrap_or("").contains("同名"),
        "该报「已存在同名」：{body}"
    );
}

#[tokio::test]
async fn edit_can_rename_to_its_own_name() {
    let _g = sandbox().await;
    let id = seed_builtin(Side::Dispatcher, "我自己", 1);
    // 同名检查必须排除自己，否则「只改优先级 + 带上原名字」的表单提交会被误拒
    let (status, body) = post(
        &format!("/api/dispatchers/{id}/edit"),
        json!({"name": "我自己", "priority": 2}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
}

#[tokio::test]
async fn edit_empty_description_clears_it() {
    let _g = sandbox().await;
    let id = seed_builtin(Side::Dispatcher, "x", 1);

    let (status, _) = post(
        &format!("/api/dispatchers/{id}/edit"),
        json!({"description": ""}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let row = root()
        .state
        .store
        .get_plugin(PluginTable::Dispatchers, id)
        .unwrap()
        .unwrap();
    assert_eq!(row.description, "", "空串该把描述清空");
}

#[tokio::test]
async fn edit_unknown_id_is_404() {
    let _g = sandbox().await;
    let (status, _) = post("/api/evaluators/999999/edit", json!({"priority": 1})).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn edit_blank_name_keeps_the_old_one() {
    let _g = sandbox().await;
    let id = seed_builtin(Side::Dispatcher, "保留我", 1);
    let (status, _) = post(&format!("/api/dispatchers/{id}/edit"), json!({"name": "   "})).await;
    assert_eq!(status, StatusCode::OK);
    let row = root()
        .state
        .store
        .get_plugin(PluginTable::Dispatchers, id)
        .unwrap()
        .unwrap();
    assert_eq!(row.name, "保留我", "全空白名字该被忽略，不是写成空白");
}

// ------------------------------------------------------------ delete

#[tokio::test]
async fn delete_removes_row_and_file() {
    let _g = sandbox().await;
    // 走上传路径造一个用户插件（这样才有真实文件）
    let (status, body) = upload(
        "/api/dispatchers/upload",
        "myplug.rhai",
        &Side::Dispatcher.sample_source(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let id = body["id"].as_i64().unwrap();
    let filename = body["filename"].as_str().unwrap().to_string();
    let path = plugins_root().join("dispatchers").join(&filename);
    assert!(path.exists(), "上传后文件该落盘: {}", path.display());

    // 原版是 `POST .../<id>/delete`，不是 REST 风格的 `DELETE .../<id>`。
    // 前端照着原版写的，所以这里也必须照样打。
    let (status, body) = post(&format!("/api/dispatchers/{id}/delete"), json!({})).await;
    assert_eq!(status, StatusCode::OK, "{body}");

    assert!(
        root().state.store.get_plugin(PluginTable::Dispatchers, id).unwrap().is_none(),
        "库里该没了"
    );
    assert!(!path.exists(), "磁盘文件也该被删掉（否则幽灵插件会复活）");
}

#[tokio::test]
async fn builtin_plugins_cannot_be_deleted() {
    let _g = sandbox().await;
    let id = seed_builtin(Side::Evaluator, "内建的", 1);

    let (status, body) = post(&format!("/api/evaluators/{id}/delete"), json!({})).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "内建插件该拒绝删除: {body}");

    assert!(
        root().state.store.get_plugin(PluginTable::Evaluators, id).unwrap().is_some(),
        "拒绝删除后记录必须还在"
    );
}

#[tokio::test]
async fn delete_unknown_id_is_404() {
    let _g = sandbox().await;
    let (status, _) = post("/api/evaluators/999999/delete", json!({})).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

// ------------------------------------------------------------ upload

#[tokio::test]
async fn upload_accepts_rhai_and_registers_it_enabled_but_inactive() {
    let _g = sandbox().await;
    let (status, body) = upload(
        "/api/evaluators/upload",
        "我的评估器.rhai",
        &Side::Evaluator.sample_source(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["ok"], json!(true));
    // 原版 INSERT 的是 `enabled=1, active=0`：
    // 「装了」但「没正在用」—— 激活是明确的用户动作。
    assert_eq!(body["enabled"], json!(true), "上传后该是启用的");
    assert_eq!(body["active"], json!(false), "上传后不该自动激活");

    let id = body["id"].as_i64().unwrap();
    let row = root()
        .state
        .store
        .get_plugin(PluginTable::Evaluators, id)
        .unwrap()
        .unwrap();
    assert!(row.enabled);
    assert!(!row.active);
    assert!(row.filename.starts_with("user_"), "上传的插件要带 user_ 前缀");
    assert!(!row.is_builtin);
}

#[tokio::test]
async fn upload_priority_is_max_plus_one() {
    let _g = sandbox().await;
    seed_builtin(Side::Evaluator, "占位", 40);

    let (_, body) = upload(
        "/api/evaluators/upload",
        "new.rhai",
        &Side::Evaluator.sample_source(),
    )
    .await;
    let id = body["id"].as_i64().unwrap();
    let row = root()
        .state
        .store
        .get_plugin(PluginTable::Evaluators, id)
        .unwrap()
        .unwrap();
    assert_eq!(row.priority, 41, "新插件该排到最后（max + 1）");
}

#[tokio::test]
async fn upload_rejects_syntax_errors_without_touching_disk() {
    let _g = sandbox().await;
    let (status, body) = upload(
        "/api/dispatchers/upload",
        "broken.rhai",
        &Side::Dispatcher.broken_source(),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");

    let dir = plugins_root().join("dispatchers");
    let files: Vec<_> = std::fs::read_dir(&dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().to_string())
        .collect();
    assert!(
        files.is_empty(),
        "编译失败时不该有任何文件落盘（原版是「先写盘、失败再删」），实际: {files:?}"
    );
}

#[tokio::test]
async fn upload_rejects_scripts_missing_the_entry_function() {
    let _g = sandbox().await;
    let (status, body) = upload(
        "/api/dispatchers/upload",
        "noentry.rhai",
        &Side::Dispatcher.source_without_entry(),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert!(
        body["error"]
            .as_str()
            .unwrap_or("")
            .contains("dispatch"),
        "错误信息该点明缺的是哪个入口函数：{body}"
    );
}

#[tokio::test]
async fn upload_rejects_python_files() {
    let _g = sandbox().await;
    // 原版用的是 `exec_module()` 直接执行上传的 .py —— 那就是个 RCE 后门。
    // 本版只认 .rhai，并且整个插件系统跑在 Rhai 沙箱里。
    let (status, body) = upload(
        "/api/dispatchers/upload",
        "evil.py",
        "import os\nos.system('id')\n",
    )
    .await;
    assert!(
        status.is_client_error(),
        "上传 .py 该被拒，实际 {status}: {body}"
    );
}

#[tokio::test]
async fn upload_without_file_is_400() {
    let _g = sandbox().await;
    let c = cookie().await;
    let boundary = "----empty-body----";
    let (status, body) = send(
        Request::builder()
            .method("POST")
            .uri("/api/dispatchers/upload")
            .header("cookie", c)
            .header(CSRF_HEADER, CSRF_HEADER_VALUE)
            .header(
                "content-type",
                format!("multipart/form-data; boundary={boundary}"),
            )
            .body(Body::from(format!("--{boundary}--\r\n")))
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert!(body["error"].as_str().unwrap_or("").contains("未收到文件"));
}

// ------------------------------------------------------------ download / template

#[tokio::test]
async fn download_returns_the_plugin_source_as_attachment() {
    let _g = sandbox().await;
    let src = Side::Evaluator.sample_source();
    let (_, body) = upload("/api/evaluators/upload", "dl.rhai", &src).await;
    let id = body["id"].as_i64().unwrap();
    let filename = body["filename"].as_str().unwrap().to_string();

    let resp = get_raw(&format!("/api/evaluators/{id}/download")).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let disposition = resp
        .headers()
        .get("content-disposition")
        .expect("该带 Content-Disposition")
        .to_str()
        .unwrap()
        .to_string();
    assert!(
        disposition.contains(&filename),
        "附件名该与库里登记的 filename 一致：{disposition}"
    );

    let text = String::from_utf8(
        resp.into_body().collect().await.unwrap().to_bytes().to_vec(),
    )
    .unwrap();
    assert_eq!(text, src, "下载的该是源码原文");
}

#[tokio::test]
async fn download_unknown_id_is_404() {
    let _g = sandbox().await;
    let resp = get_raw("/api/dispatchers/999999/download").await;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn template_is_an_attachment_and_actually_compiles() {
    let _g = sandbox().await;
    for (api, expect_entry) in [("dispatchers", "dispatch"), ("evaluators", "evaluate")] {
        let resp = get_raw(&format!("/api/{api}/template")).await;
        assert_eq!(resp.status(), StatusCode::OK, "{api} 模板该能下载");

        let disposition = resp
            .headers()
            .get("content-disposition")
            .unwrap()
            .to_str()
            .unwrap()
            .to_string();
        assert!(disposition.contains(".rhai"), "{api} 模板该是 .rhai 文件");

        let text = String::from_utf8(
            resp.into_body().collect().await.unwrap().to_bytes().to_vec(),
        )
        .unwrap();
        assert!(
            text.contains(expect_entry),
            "{api} 模板该包含入口函数 {expect_entry}"
        );
        // 模板是要给用户当起点的 —— 它自己必须能跑通编译。
        let kind = if api == "dispatchers" {
            alpha_plugin::PluginKind::Dispatcher
        } else {
            alpha_plugin::PluginKind::Evaluator
        };
        alpha_plugin::Plugin::compile(&text, kind, None)
            .unwrap_or_else(|e| panic!("{api} 的模板编译不过: {e}"));
    }
}

// ------------------------------------------------------------ preview

#[tokio::test]
async fn preview_uses_the_active_evaluator() {
    let _g = sandbox().await;
    let id = seed_builtin(Side::Evaluator, "唯一评估器", 1);
    post(&format!("/api/evaluators/{id}/activate"), json!({})).await;

    let (status, body) = post(
        "/api/evaluators/preview",
        json!({
            "boss": {
                "name": "喷火龙",
                "ability": "太阳之力",
                "moves": ["喷射火焰"],
                "location": "冠军之路",
                "period": "全天",
            }
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.get("evaluator").is_some(), "预览结果该说明用的哪个评估器: {body}");
}

#[tokio::test]
async fn preview_with_no_evaluator_at_all_is_500() {
    let _g = sandbox().await;
    // 库里一个评估器都没有 —— 原版这里会抛，Rust 版把它变成可读的 500
    let (status, body) = post(
        "/api/evaluators/preview",
        json!({"boss": {"name": "测试", "moves": []}}),
    )
    .await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR, "{body}");
    assert!(
        body["error"].as_str().unwrap_or("").contains("没有可用的评估器"),
        "该给出可读的错误：{body}"
    );
}

#[tokio::test]
async fn preview_falls_back_to_active_when_explicit_id_is_disabled() {
    let _g = sandbox().await;
    // 造两个：一个启用且激活（该被用上），一个停用（显式指定它也该回落）
    let active = seed_builtin(Side::Evaluator, "激活的", 1);
    let disabled = seed_builtin(Side::Evaluator, "停用的", 2);
    post(&format!("/api/evaluators/{active}/activate"), json!({})).await;
    post(
        &format!("/api/evaluators/{disabled}/enable"),
        json!({"enabled": false}),
    )
    .await;

    let (status, body) = post(
        "/api/evaluators/preview",
        json!({
            "evaluator_id": disabled,
            "boss": {"name": "测试", "moves": []}
        }),
    )
    .await;
    // 原版行为：显式指定一个已停用的评估器 → 静默回落到激活项，不报错。
    assert_eq!(status, StatusCode::OK, "{body}");
}

// ------------------------------------------------------------ 路由优先级

#[tokio::test]
async fn static_paths_are_not_swallowed_by_the_id_route() {
    let _g = sandbox().await;
    // `/api/dispatchers/template` 与 `/api/dispatchers/:id/download` 形状相近。
    // axum 里静态段优先，但这条测试把这个保证钉死。
    let resp = get_raw("/api/dispatchers/template").await;
    assert_eq!(resp.status(), StatusCode::OK);
    let text = String::from_utf8(
        resp.into_body().collect().await.unwrap().to_bytes().to_vec(),
    )
    .unwrap();
    assert!(text.contains("fn dispatch"), "拿到的该是模板，不是 404");
}
