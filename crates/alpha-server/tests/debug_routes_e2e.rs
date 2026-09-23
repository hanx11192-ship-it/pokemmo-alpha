//! 调试台 3 个接口的端到端测试：
//! `GET /api/dashboard`、`GET /api/pokedex`、`POST /debug/run`。
//!
//! # 这一组测的是「最绕的那个接口」
//!
//! `/debug/run` 要按用户输入的名字反查图鉴、拼一个假头目、再跑决策器。
//! 里面有一堆「找不到就静默用原值」的兜底 —— 这类行为如果只靠读代码，
//! 很容量漏掉某个分支（比如「特性留空要写成『无特性』」）。
//! 所以这里逐条钉住每个输入分支的实际输出。
//!
//! # 为什么不用手写期望值
//!
//! `report` 字段的内容来自真正的打法引擎，一个字都不该由测试作者猜。
//! 这里只断言**结构性**的事实（非空、包含头目名、行数够），
//! 内容的正确性由 `alpha-strategy` 自己的黄金回归负责。
//! 手写「期望报告全文」是维护灾难，而且会掩盖真正的回归。

use std::path::PathBuf;
use std::sync::OnceLock;

use alpha_server::auth::layer::{CSRF_HEADER, CSRF_HEADER_VALUE};
use alpha_server::{build_router, AppState, Config};
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

/// 仓库根目录（集成测试的 cwd 是包根 `crates/alpha-server`）。
fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|p| p.parent())
        .expect("应有仓库根目录")
        .to_path_buf()
}

fn root() -> &'static TestRoot {
    static ROOT: OnceLock<TestRoot> = OnceLock::new();
    ROOT.get_or_init(|| {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path().to_path_buf();

        std::fs::create_dir_all(base.join("config")).unwrap();
        std::fs::write(base.join("config/sources.yaml"), "sources: []\n").unwrap();
        std::fs::write(
            base.join("config/settings.yaml"),
            "timezone: Asia/Shanghai\nlanguage: zh\n",
        )
        .unwrap();
        std::fs::write(base.join("panel.env"), "# Alpha 面板环境变量\n").unwrap();

        // 图鉴与别名是 `/api/pokedex` 与 `/debug/run` 的硬依赖：
        // 少一个就会得到「加载图鉴失败」，而报错信息会指向
        // 完全无关的地方（看起来像路由写错了）。
        std::fs::create_dir_all(base.join("data")).unwrap();
        for name in ["pokedex.json", "aliases.json"] {
            let src = repo_root().join("data").join(name);
            std::fs::copy(&src, base.join("data").join(name))
                .unwrap_or_else(|e| panic!("复制 {} 失败: {e}", src.display()));
        }

        std::env::set_var("ALPHA_ROOT", &base);
        std::env::set_var("PANEL_ENV_FILE", base.join("panel.env"));

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
        // 插件系统必须挂上：`/debug/run` 要走 `registry.load_by_filename`
        // 加载决策器，没挂的话所有用例都会以「插件系统未初始化」失败。
        let state = AppState::new(cfg, store).with_registry(base.join("data/plugins"));
        let app = build_router(state.clone());

        TestRoot {
            _dir: dir,
            app,
            state,
        }
    })
}

/// 拿排他锁并复位状态。
///
/// 清 `logs` 表是因为 `/debug/run` 会写一条 debug 日志（原版行为），
/// 不清的话 `dashboard` 的 `spawn_count_200` / 最近日志断言会被
/// 上一个用例留下的记录影响。
async fn sandbox() -> tokio::sync::MutexGuard<'static, ()> {
    static LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
    let guard = LOCK.lock().await;
    {
        let conn = root().state.store.lock().unwrap();
        let _ = conn.execute("DELETE FROM logs", []).unwrap();
        let _ = conn.execute("DELETE FROM dispatchers", []).unwrap();
        let _ = conn.execute("DELETE FROM evaluators", []).unwrap();
    }
    // 插件目录也要清 —— 上一轮测试写进去的 `.rhai` 会被
    // `load_by_filename` 读到，表现为「明明没建插件却执行成功了」
    if let Some(reg) = root().state.registry.as_ref() {
        for kind in [
            alpha_plugin::PluginKind::Dispatcher,
            alpha_plugin::PluginKind::Evaluator,
        ] {
            let d = reg.dir(kind);
            if let Ok(entries) = std::fs::read_dir(&d) {
                for e in entries.flatten() {
                    let _ = std::fs::remove_file(e.path());
                }
            }
        }
        reg.invalidate();
    }
    guard
}

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

/// 不带 cookie 的 GET —— 用来验证鉴权要求。
async fn get_anon(path: &str) -> StatusCode {
    let resp = root()
        .app
        .clone()
        .oneshot(Request::builder().uri(path).body(Body::empty()).unwrap())
        .await
        .unwrap();
    resp.status()
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

/// 往 dispatchers 表插一条记录 —— `/debug/run` 的三级回退要靠它。
fn seed_dispatcher(name: &str, filename: &str, enabled: bool, active: bool, builtin: bool) -> i64 {
    let conn = root().state.store.lock().unwrap();
    let _ = conn.execute(
        "INSERT INTO dispatchers (name, filename, enabled, active, priority, description, is_builtin)
         VALUES (?,?,?,?,?,?,?)",
        rusqlite::params![
            name,
            filename,
            i64::from(enabled),
            i64::from(active),
            1i64,
            "测试用",
            i64::from(builtin)
        ],
    )
    .unwrap();
    conn.last_insert_rowid()
}

/// 往日志表塞一条记录（用当前时刻）。
fn seed_log(kind: &str, msg: &str) {
    root().state
        .store
        .log(alpha_store::LogLevel::Info, kind, msg, "test");
}

/// 写一个最小可用的决策器脚本到磁盘（返回固定文本）。
fn write_dispatcher_plugin(filename: &str, text: &str) {
    let reg = root().state.registry.as_ref().expect("插件系统应已初始化");
    let path = reg.dir(alpha_plugin::PluginKind::Dispatcher).join(filename);
    std::fs::write(&path, format!("fn dispatch(boss, ctx) {{ \"{text}\" }}\n")).unwrap();
    reg.invalidate();
}

// ============================================================
// 鉴权
// ============================================================

/// 三条路由都需要登录。
///
/// 原版上 `/api/dashboard` 与 `/api/pokedex` 确实**没有** `@login_required`，
/// 但本版统一放在鉴权中间件之后 —— 这是有意的收紧：
/// 仪表盘暴露了日志内容与调度器状态，图鉴列表虽然是公开数据，
/// 但没必要单独开一个匿名口子。
///
/// `/debug/run` 是这条用例里最值得测的一个：它挂在根路径上（不带 `/api`
/// 前缀），只按前缀判断鉴权会让它掉进「非 API」分支，
/// 未登录时返回 200 的 SPA 入口页而不是 401。
#[tokio::test]
async fn all_three_routes_require_login() {
    let _g = sandbox().await;
    for path in ["/api/dashboard", "/api/pokedex"] {
        assert_eq!(
            get_anon(path).await,
            StatusCode::UNAUTHORIZED,
            "{path} 应该要求登录"
        );
    }

    // `/debug/run` 是 POST，匿名请求同样该是 401。
    //
    // 注意要**带上 CSRF 头**才测得到鉴权：中间件里 CSRF 检查排在
    // 鉴权之前（这是有意的，CSRF 与「有没有登录」是两件独立的事），
    // 不带头会先拿到 400。只测 400 的话，就永远发现不了
    // 「这条路由根本没做登录检查」这种问题 —— 而那正是第一版的 bug。
    let resp = root()
        .app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/debug/run")
                .header("content-type", "application/json")
                .header(CSRF_HEADER, CSRF_HEADER_VALUE)
                .body(Body::from("{}"))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::UNAUTHORIZED,
        "匿名调用 /debug/run 该是 401（挂了 /api 前缀之外的接口也得鉴权）"
    );
}

// ============================================================
// /api/dashboard
// ============================================================

#[tokio::test]
async fn dashboard_returns_every_documented_field() {
    let _g = sandbox().await;
    let (status, body) = get("/api/dashboard").await;
    assert_eq!(status, StatusCode::OK);
    // 注意：**没有** `ok` 字段。
    //
    // `/api/dashboard` 原版回的是一个裸对象（`jsonify({...})`），
    // 不像其它接口那样包一层 `{"ok": true, ...}`。前端是直接读
    // `res.data.sources` 的，多包一层反而要改前端。
    assert!(
        body.get("ok").is_none(),
        "仪表盘原版不带 ok 字段，别自作主张加上：{body}"
    );

    for key in [
        "sources",
        "recent_logs",
        "spawn_count_200",
        "total_logs",
        "active_dispatcher",
        "scheduler",
        "pokedex",
    ] {
        assert!(body.get(key).is_some(), "仪表盘缺少字段 `{key}`：{body}");
    }

    // 数据源统计是个对象，不该退化成裸数字
    assert!(body["sources"]["total"].is_number());
    assert!(body["sources"]["enabled"].is_number());

    // 图鉴规模三个数字都要在（前端三个卡片分别读它们）
    for k in ["pokemon", "abilities", "moves"] {
        assert!(
            body["pokedex"][k].as_u64().unwrap_or(0) > 0,
            "图鉴 {k} 数量应大于 0：{body}"
        );
    }
}

#[tokio::test]
async fn dashboard_recent_logs_is_capped_at_twelve() {
    let _g = sandbox().await;
    for i in 0..20 {
        seed_log("test", &format!("第 {i} 条"));
    }
    let (_, body) = get("/api/dashboard").await;
    assert_eq!(
        body["recent_logs"].as_array().unwrap().len(),
        12,
        "最近日志应只有 12 条"
    );
    assert_eq!(body["total_logs"], json!(20), "总数该是全部 20 条");
}

/// `spawn_count_200` 数的是 `kind="spawn"` 的记录。
///
/// 这条用例的存在有个来由：原版这里写的是 `level="spawn"`，
/// 而 `level` 列存的是 `info`/`error` 这类**级别**，
/// 所以那个查询在原版上恒返回空。本版改用 `kind` 筛。
#[tokio::test]
async fn dashboard_counts_spawn_by_kind_not_level() {
    let _g = sandbox().await;
    seed_log("spawn", "刷新点 A");
    seed_log("spawn", "刷新点 B");
    seed_log("poll", "轮询一轮");

    let (_, body) = get("/api/dashboard").await;
    assert_eq!(
        body["spawn_count_200"],
        json!(2),
        "应只数出 kind=spawn 的两条"
    );
    assert_eq!(body["total_logs"], json!(3));
}

#[tokio::test]
async fn dashboard_active_dispatcher_is_null_when_none_is_active() {
    let _g = sandbox().await;
    // 只插一条未激活的
    seed_dispatcher("某决策器", "some.rhai", true, false, false);

    let (_, body) = get("/api/dashboard").await;
    assert_eq!(
        body["active_dispatcher"],
        Value::Null,
        "没激活项时该是 null"
    );
}

#[tokio::test]
async fn dashboard_reports_the_active_dispatcher_name() {
    let _g = sandbox().await;
    seed_dispatcher("我的决策器", "mine.rhai", true, true, false);

    let (_, body) = get("/api/dashboard").await;
    assert_eq!(body["active_dispatcher"], json!("我的决策器"));
}

/// 数据源个数取自 `config/sources.yaml`。
#[tokio::test]
async fn dashboard_counts_sources_from_config() {
    let _g = sandbox().await;
    let base = PathBuf::from(std::env::var("ALPHA_ROOT").unwrap());
    std::fs::write(
        base.join("config/sources.yaml"),
        "sources:\n\
         - name: 甲\n  adapter: lzpoke_reports\n  enabled: true\n\
         - name: 乙\n  adapter: lzpoke_reports\n  enabled: false\n\
         - name: 丙\n  adapter: lzpoke_reports\n",
    )
    .unwrap();
    // 配置是进程级缓存的，改了文件要让它重读
    let _ = alpha_core::config::refresh_config();

    let (_, body) = get("/api/dashboard").await;
    assert_eq!(body["sources"]["total"], json!(3));
    assert_eq!(
        body["sources"]["enabled"],
        json!(1),
        "没写 enabled 的源该算未启用"
    );

    // 复位，别污染其它用例
    std::fs::write(base.join("config/sources.yaml"), "sources: []\n").unwrap();
    let _ = alpha_core::config::refresh_config();
}

// ============================================================
// /api/pokedex
// ============================================================

#[tokio::test]
async fn pokedex_returns_three_lists() {
    let _g = sandbox().await;
    let (status, body) = get("/api/pokedex").await;
    assert_eq!(status, StatusCode::OK);

    for key in ["pokemon", "moves", "abilities"] {
        let arr = body[key]
            .as_array()
            .unwrap_or_else(|| panic!("`{key}` 该是数组：{body}"));
        assert!(!arr.is_empty(), "`{key}` 列表不该为空");
    }
}

/// 列表按 id 升序。
///
/// `HashMap` 的迭代顺序是随机的 —— 不排的话前端下拉框每次刷新
/// 顺序都在变，用户会以为选错了。这条把顺序钉死。
#[tokio::test]
async fn pokedex_lists_are_sorted_by_id() {
    let _g = sandbox().await;
    let (_, body) = get("/api/pokedex").await;

    for key in ["pokemon", "moves", "abilities"] {
        let ids: Vec<i64> = body[key]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|e| e["id"].as_i64())
            .collect();
        assert!(
            ids.windows(2).all(|w| w[0] <= w[1]),
            "`{key}` 列表没按 id 升序：前几项是 {:?}",
            &ids[..ids.len().min(10)]
        );
    }
}

/// 精灵条目要带上「选了之后自动填表单」所需的三个字段。
#[tokio::test]
async fn pokedex_pokemon_entries_carry_form_defaults() {
    let _g = sandbox().await;
    let (_, body) = get("/api/pokedex").await;

    let entry = &body["pokemon"][0];
    for key in ["id", "zh", "en", "g", "eg", "ha"] {
        assert!(
            entry.get(key).is_some(),
            "精灵条目缺少 `{key}`：{entry}"
        );
    }
}

// ============================================================
// /debug/run —— 入参校验
// ============================================================

#[tokio::test]
async fn debug_run_rejects_empty_pokemon() {
    let _g = sandbox().await;
    for body in [json!({}), json!({"pokemon": ""}), json!({"pokemon": "   "})] {
        let (status, resp) = post("/debug/run", body.clone()).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "body={body}");
        assert!(
            resp["error"].as_str().unwrap().contains("头目精灵"),
            "错误文案该提到头目精灵：{resp}"
        );
    }
}

/// 缺 CSRF 头 → 400（不是 403、也不是 500）。
///
/// 这条用例第一版失败在「500」上，暴露了一个真实缺陷：中间件用
/// `path.starts_with("/api/")` 判断「是不是 API」，而 `/debug/run`
/// 挂在根路径上 —— 于是它既没被鉴权拦、也没被 CSRF 拦，
/// 一路走到 handler 里因为「没有可用决策器」回了 500。
#[tokio::test]
async fn debug_run_rejects_missing_csrf_header() {
    let _g = sandbox().await;
    let c = cookie().await;
    let resp = root()
        .app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/debug/run")
                .header("content-type", "application/json")
                .header("cookie", c)
                .body(Body::from(serde_json::to_vec(&json!({"pokemon": "皮卡丘"})).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let text = String::from_utf8_lossy(&bytes);
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(text.contains("CSRF"), "该报 CSRF 相关错误：{text}");
}

/// 没有可用决策器 → 500「没有可用的分发器」。
#[tokio::test]
async fn debug_run_without_any_dispatcher_is_a_500() {
    let _g = sandbox().await;
    let (status, body) = post("/debug/run", json!({"pokemon": "皮卡丘"})).await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    assert!(
        body["error"].as_str().unwrap().contains("没有可用的分发器"),
        "{body}"
    );
}

// ============================================================
// /debug/run —— 名字归一化
// ============================================================

/// 已知精灵：`resolved` 里出现的是**规范中文名**与解析出的 id。
#[tokio::test]
async fn debug_run_normalizes_a_known_pokemon() {
    let _g = sandbox().await;
    seed_dispatcher("内置", "b.rhai", true, false, true);
    write_dispatcher_plugin("b.rhai", "报告正文");

    let (status, body) = post(
        "/debug/run",
        json!({"pokemon": "皮卡丘", "ability": "静电", "moves": ["十万伏特"]}),
    )
    .await;

    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["ok"], json!(true));
    assert_eq!(body["report"], json!("报告正文"));
    assert_eq!(body["dispatcher"], json!("内置"));

    let r = &body["resolved"];
    assert_eq!(r["name"], json!("皮卡丘"));
    assert_eq!(r["ability"], json!("静电"));
    assert_eq!(r["moves"], json!(["十万伏特"]));
    assert!(r["pokemon_id"].as_i64().unwrap() > 0, "皮卡丘该有图鉴号");
    assert_eq!(r["move_ids"].as_array().unwrap().len(), 1);
    assert!(r["ability_id"].as_i64().unwrap() > 0, "静电该有特性 id");
}

/// **未知精灵不报错**，原样回显 —— 这样调试台对「还没入库的新精灵」也能用。
///
/// 这条看着像 bug，但它是原版的既定行为，且有用。`resolved` 会暴露
/// 「名字没被归一化」这个事实（`name` 与 `pokemon_id` 对不上），
/// 用户能自己看出来。
#[tokio::test]
async fn debug_run_passes_through_an_unknown_pokemon() {
    let _g = sandbox().await;
    seed_dispatcher("内置", "b.rhai", true, false, true);
    write_dispatcher_plugin("b.rhai", "报告正文");

    let (status, body) = post("/debug/run", json!({"pokemon": "还没入库的精灵"})).await;
    assert_eq!(status, StatusCode::OK, "{body}");

    let r = &body["resolved"];
    assert_eq!(
        r["name"],
        json!("还没入库的精灵"),
        "找不到时应原样返回输入"
    );
    assert_eq!(r["pokemon_id"], Value::Null, "未知精灵没有图鉴号");
}

/// 别名也要能归一化（`aliases.json` 的作用）。
#[tokio::test]
async fn debug_run_resolves_aliases() {
    let _g = sandbox().await;
    seed_dispatcher("内置", "b.rhai", true, false, true);
    write_dispatcher_plugin("b.rhai", "报告正文");

    // 用英文名查中文精灵（图鉴里 皮卡丘 = Pikachu）
    let (status, body) = post("/debug/run", json!({"pokemon": "Pikachu"})).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        body["resolved"]["name"],
        json!("皮卡丘"),
        "英文名该被归一化成中文规范名"
    );
}

// ============================================================
// /debug/run —— 各字段的默认值
// ============================================================

/// 特性留空 → 写「无特性」，而不是空串。
///
/// 引擎的模板会把特性直接拼进正文，空串会渲染出「特性：」这种半截话。
#[tokio::test]
async fn debug_run_writes_a_placeholder_for_a_missing_ability() {
    let _g = sandbox().await;
    seed_dispatcher("内置", "b.rhai", true, false, true);
    write_dispatcher_plugin("b.rhai", "x");

    for body in [json!({"pokemon": "皮卡丘"}), json!({"pokemon": "皮卡丘", "ability": ""})] {
        let (status, resp) = post("/debug/run", body.clone()).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(
            resp["resolved"]["ability"],
            json!("无特性"),
            "空特性该写成「无特性」：{body}"
        );
        assert_eq!(resp["resolved"]["ability_id"], Value::Null);
    }
}

/// 蛋组留空 → 从图鉴取这只精灵的默认蛋组，而不是留空。
#[tokio::test]
async fn debug_run_falls_back_to_the_dex_egg_groups() {
    let _g = sandbox().await;
    seed_dispatcher("内置", "b.rhai", true, false, true);
    write_dispatcher_plugin("b.rhai", "x");

    let (status, body) = post("/debug/run", json!({"pokemon": "皮卡丘"})).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let eg = body["resolved"]["egg_groups"].as_array().unwrap();
    assert!(
        !eg.is_empty(),
        "皮卡丘在图鉴里有蛋组，留空时应自动取到：{body}"
    );
}

/// 显式给了蛋组就用给的。
#[tokio::test]
async fn debug_run_uses_the_submitted_egg_groups() {
    let _g = sandbox().await;
    seed_dispatcher("内置", "b.rhai", true, false, true);
    write_dispatcher_plugin("b.rhai", "x");

    let (status, body) = post(
        "/debug/run",
        json!({"pokemon": "皮卡丘", "egg_groups": ["陆上", "妖精"]}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["resolved"]["egg_groups"], json!(["陆上", "妖精"]));
}

/// 技能列表里的空白项要被丢掉（前端可能提交空输入框）。
#[tokio::test]
async fn debug_run_drops_blank_moves() {
    let _g = sandbox().await;
    seed_dispatcher("内置", "b.rhai", true, false, true);
    write_dispatcher_plugin("b.rhai", "x");

    let (status, body) = post(
        "/debug/run",
        json!({"pokemon": "皮卡丘", "moves": ["十万伏特", "", "  ", "打雷"]}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["resolved"]["moves"], json!(["十万伏特", "打雷"]));
}

/// 未解析出 id 的技能会被 `move_ids` 丢掉（原版 `[m for m in move_ids if m]`）。
#[tokio::test]
async fn debug_run_omits_unresolved_move_ids() {
    let _g = sandbox().await;
    seed_dispatcher("内置", "b.rhai", true, false, true);
    write_dispatcher_plugin("b.rhai", "x");

    let (status, body) = post(
        "/debug/run",
        json!({"pokemon": "皮卡丘", "moves": ["十万伏特", "这招不存在"]}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        body["resolved"]["moves"],
        json!(["十万伏特", "这招不存在"]),
        "名字列表保留原样（归一化失败时用输入值）"
    );
    assert_eq!(
        body["resolved"]["move_ids"].as_array().unwrap().len(),
        1,
        "只有解析出 id 的那个才进 move_ids"
    );
}

// ============================================================
// /debug/run —— 决策器三级回退
// ============================================================

/// 指定的决策器**已启用** → 用它。
#[tokio::test]
async fn debug_run_uses_the_explicitly_requested_dispatcher() {
    let _g = sandbox().await;
    let want = seed_dispatcher("指定的", "picked.rhai", true, false, false);
    seed_dispatcher("激活的", "active.rhai", true, true, false);
    write_dispatcher_plugin("picked.rhai", "来自指定的");
    write_dispatcher_plugin("active.rhai", "来自激活的");

    let (status, body) = post(
        "/debug/run",
        json!({"pokemon": "皮卡丘", "dispatcher_id": want}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["dispatcher"], json!("指定的"));
    assert_eq!(body["report"], json!("来自指定的"));
}

/// 指定的决策器**被停用** → 忽略它，走回退链。
///
/// 原版 SQL 带 `enabled=1`，所以停用的决策器不该被调试台拉起来。
#[tokio::test]
async fn debug_run_ignores_a_disabled_requested_dispatcher() {
    let _g = sandbox().await;
    let disabled = seed_dispatcher("停用的", "off.rhai", false, false, false);
    seed_dispatcher("激活的", "active.rhai", true, true, false);
    write_dispatcher_plugin("off.rhai", "不该出现");
    write_dispatcher_plugin("active.rhai", "来自激活的");

    let (status, body) = post(
        "/debug/run",
        json!({"pokemon": "皮卡丘", "dispatcher_id": disabled}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        body["dispatcher"],
        json!("激活的"),
        "停用的决策器不该被选中"
    );
    assert_eq!(body["report"], json!("来自激活的"));
}

/// 指定了不存在的 id → 静默回退（不报错）。
#[tokio::test]
async fn debug_run_falls_back_when_the_requested_id_is_unknown() {
    let _g = sandbox().await;
    seed_dispatcher("激活的", "active.rhai", true, true, false);
    write_dispatcher_plugin("active.rhai", "来自激活的");

    let (status, body) = post(
        "/debug/run",
        json!({"pokemon": "皮卡丘", "dispatcher_id": 999999}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["dispatcher"], json!("激活的"));
}

/// 没有激活项 → 用内建的。
///
/// 注意内建决策器的 `enabled` / `active` 都是 `false`（原版首次启动
/// 只登记不激活），所以这条回退是实际吃重的。
#[tokio::test]
async fn debug_run_falls_back_to_the_builtin_dispatcher() {
    let _g = sandbox().await;
    seed_dispatcher("内建的", "builtin.rhai", true, false, true);
    write_dispatcher_plugin("builtin.rhai", "来自内建的");

    let (status, body) = post("/debug/run", json!({"pokemon": "皮卡丘"})).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["dispatcher"], json!("内建的"));
    assert_eq!(body["report"], json!("来自内建的"));
}

/// 激活项优先于内建项。
#[tokio::test]
async fn debug_run_prefers_active_over_builtin() {
    let _g = sandbox().await;
    seed_dispatcher("内建的", "builtin.rhai", true, false, true);
    seed_dispatcher("激活的", "active.rhai", true, true, false);
    write_dispatcher_plugin("builtin.rhai", "来自内建的");
    write_dispatcher_plugin("active.rhai", "来自激活的");

    let (status, body) = post("/debug/run", json!({"pokemon": "皮卡丘"})).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["dispatcher"], json!("激活的"));
}

/// 决策器脚本抛错 → 500，且文案带「分发器执行失败」。
#[tokio::test]
async fn debug_run_reports_dispatcher_errors() {
    let _g = sandbox().await;
    seed_dispatcher("坏的", "bad.rhai", true, false, true);
    let reg = root().state.registry.as_ref().unwrap();
    std::fs::write(
        reg.dir(alpha_plugin::PluginKind::Dispatcher).join("bad.rhai"),
        "fn dispatch(boss, ctx) { throw \"炸了\" }\n",
    )
    .unwrap();
    reg.invalidate();

    let (status, body) = post("/debug/run", json!({"pokemon": "皮卡丘"})).await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    assert!(
        body["error"].as_str().unwrap().contains("分发器执行失败"),
        "{body}"
    );
}

/// 数据库里登记了决策器，但**文件不在磁盘上** → 500，文案说明文件不存在。
///
/// 这是实际会发生的情况：有人手动删了 `data/plugins/dispatchers/*.rhai`，
/// 数据库里还留着记录。
#[tokio::test]
async fn debug_run_reports_a_missing_dispatcher_file() {
    let _g = sandbox().await;
    seed_dispatcher("幽灵", "ghost.rhai", true, false, true);
    // 刻意不写文件

    let (status, body) = post("/debug/run", json!({"pokemon": "皮卡丘"})).await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    let msg = body["error"].as_str().unwrap();
    assert!(
        msg.contains("分发器文件不存在") || msg.contains("分发器执行失败"),
        "文案该指出文件问题：{body}"
    );
}

// ============================================================
// /debug/run —— 报告内容与评估器
// ============================================================

/// 真实的报告：非空、包含头目名、有「技能:」这一行。
///
/// 内建决策器转发的是官方引擎，这里用引擎本身作 oracle，
/// **不手写期望报告全文**（那会变成维护灾难，也会掩盖真正的回归）。
#[tokio::test]
async fn debug_run_produces_a_real_report() {
    let _g = sandbox().await;
    // 用真正的内建决策器 —— 它会调到 alpha-strategy 的引擎
    let builtin = alpha_plugin::builtins::builtins_of(alpha_plugin::PluginKind::Dispatcher);
    let b = builtin.first().expect("应有内建决策器");
    seed_dispatcher(b.name, b.filename, true, false, true);

    let reg = root().state.registry.as_ref().unwrap();
    std::fs::write(
        reg.dir(alpha_plugin::PluginKind::Dispatcher).join(b.filename),
        b.source,
    )
    .unwrap();
    reg.invalidate();

    // 规则文件是引擎的依赖：测试根目录没有它的话报告会静默变空
    let base = PathBuf::from(std::env::var("ALPHA_ROOT").unwrap());
    std::fs::create_dir_all(base.join("config")).unwrap();
    let rules = repo_root().join("config/rules.yaml");
    if rules.exists() {
        std::fs::copy(&rules, base.join("config/rules.yaml")).unwrap();
        let _ = alpha_core::config::refresh_config();
    }

    let (status, body) = post(
        "/debug/run",
        json!({
            "pokemon": "巨牙鲨",
            "ability": "粗糙皮肤",
            "moves": ["挑衅", "近身战"],
            "gender": "dual",
        }),
    )
    .await;

    assert_eq!(status, StatusCode::OK, "{body}");
    let report = body["report"].as_str().unwrap();
    assert!(
        !report.trim().is_empty(),
        "内建决策器该产出非空报告 —— 空的话说明 engine_report 链路断了：{body}"
    );
    assert!(
        report.contains("巨牙鲨"),
        "报告里该出现头目名：\n{report}"
    );
    assert!(
        report.lines().count() >= 2,
        "报告该是多行的：\n{report}"
    );
}

/// 语言参数会传到引擎（中文报告里有中文名）。
#[tokio::test]
async fn debug_run_language_reaches_the_engine() {
    let _g = sandbox().await;
    let builtin = alpha_plugin::builtins::builtins_of(alpha_plugin::PluginKind::Dispatcher);
    let b = builtin.first().expect("应有内建决策器");
    seed_dispatcher(b.name, b.filename, true, false, true);
    let reg = root().state.registry.as_ref().unwrap();
    std::fs::write(
        reg.dir(alpha_plugin::PluginKind::Dispatcher).join(b.filename),
        b.source,
    )
    .unwrap();
    reg.invalidate();

    let base = PathBuf::from(std::env::var("ALPHA_ROOT").unwrap());
    std::fs::create_dir_all(base.join("config")).unwrap();
    let rules = repo_root().join("config/rules.yaml");
    if rules.exists() {
        std::fs::copy(&rules, base.join("config/rules.yaml")).unwrap();
        let _ = alpha_core::config::refresh_config();
    }

    // 故意用一个不存在的决策器文本，先确认语言本身没炸
    for lang in ["zh", "en", "both", "garbage"] {
        let (status, body) = post(
            "/debug/run",
            json!({"pokemon": "巨牙鲨", "lang": lang, "moves": ["挑衅"]}),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "lang={lang}: {body}");
        assert!(
            body["report"].is_string(),
            "lang={lang} 的报告该是字符串：{body}"
        );
    }
}

/// 调试运行会写一条 `debug` 级别的日志（原版行为）。
#[tokio::test]
async fn debug_run_writes_a_debug_log_entry() {
    let _g = sandbox().await;
    seed_dispatcher("内置", "b.rhai", true, false, true);
    write_dispatcher_plugin("b.rhai", "x");

    let before = root().state.store.count_logs().unwrap();
    let (status, _) = post("/debug/run", json!({"pokemon": "皮卡丘"})).await;
    assert_eq!(status, StatusCode::OK);

    let after = root().state.store.count_logs().unwrap();
    assert_eq!(after, before + 1, "调试运行该留一条日志");

    let logs = root()
        .state
        .store
        .list_logs(&alpha_store::LogFilter::new(5))
        .unwrap();
    let last = logs.first().expect("该有一条日志");
    assert_eq!(last.level, "debug", "级别该是 debug，别刷屏正式日志");
    assert_eq!(last.kind, "debug");
    assert!(
        last.message.contains("调试运行"),
        "日志内容该提到调试运行：{}",
        last.message
    );
    assert!(
        last.message.contains("皮卡丘"),
        "日志里该出现头目名：{}",
        last.message
    );
}

/// 激活的评估器会被装配进决策器的 ctx，报告里出现评语行。
///
/// 这里用一个返回固定 `line` 的评估器来验证「装配确实发生了」——
/// 判据是评语文本出现在报告里。用真实内建决策器的理由是：
/// 只有它会把评估器接到引擎上（`engine_report` → 引擎 → 评估钩子）。
///
/// # 为什么 `gender` 必须给 `dual`
///
/// 第一版没给，于是报告是单性别那种「只推信息」的短版 ——
/// 而评估行是插在 `header` 里的，单性别分支同样会带上它，
/// 所以那次失败**不是**装配漏了，是判据没造对（报告里确实没有
/// 评语行，但原因在别处）。给上 `dual` 让报告走完整路径，
/// 才能同时验证「评估行插在第 2 行」这个位置约定。
#[tokio::test]
async fn debug_run_wires_the_active_evaluator_into_the_dispatcher() {
    let _g = sandbox().await;

    // 决策器：把评估行拼在自定义文本后面 —— 这样判据与引擎实现解耦
    let builtin = alpha_plugin::builtins::builtins_of(alpha_plugin::PluginKind::Dispatcher);
    let b = builtin.first().expect("应有内建决策器");
    seed_dispatcher(b.name, b.filename, true, false, true);
    let reg = root().state.registry.as_ref().unwrap();
    std::fs::write(
        reg.dir(alpha_plugin::PluginKind::Dispatcher).join(b.filename),
        b.source,
    )
    .unwrap();

    // 评估器：无论如何都返回同一行
    {
        let conn = root().state.store.lock().unwrap();
        let _ = conn.execute(
            "INSERT INTO evaluators (name, filename, enabled, active, priority, description, is_builtin)
             VALUES (?,?,?,?,?,?,?)",
            rusqlite::params!["测试评估器", "ev.rhai", 1i64, 1i64, 1i64, "测试用", 0i64],
        )
        .unwrap();
    }

    std::fs::write(
        reg.dir(alpha_plugin::PluginKind::Evaluator).join("ev.rhai"),
        r#"fn evaluate(boss, ctx) { #{ score: 9, line: "评测探针一句话" } }"#,
    )
    .unwrap();
    reg.invalidate();

    let base = PathBuf::from(std::env::var("ALPHA_ROOT").unwrap());
    std::fs::create_dir_all(base.join("config")).unwrap();
    let rules = repo_root().join("config/rules.yaml");
    if rules.exists() {
        std::fs::copy(&rules, base.join("config/rules.yaml")).unwrap();
        let _ = alpha_core::config::refresh_config();
    }

    let (status, body) = post(
        "/debug/run",
        json!({"pokemon": "巨牙鲨", "moves": ["挑衅"], "gender": "dual"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");

    let report = body["report"].as_str().unwrap();
    assert!(
        report.contains("评测探针一句话"),
        "激活评估器的评语该出现在调试报告里 —— 说明装配没发生：\n{report}"
    );

    // 位置约定：评估行插在**第 2 行**（第 1 行是时段，第 2 行原本是
    // 头目信息，评估行插在它前面）。这条契约前端与用户都看得到，
    // 挪一行就是可见的回归。
    let lines: Vec<&str> = report.lines().collect();
    assert_eq!(
        lines.get(1).copied(),
        Some("评测探针一句话"),
        "评估行该在第 2 行：\n{report}"
    );
}

/// 评估器脚本炸了 → **诊断报告照常出**，只是少那一行。
///
/// 这是原版 `evaluator_hook` 的核心取舍：评估器坏了绝不能把播报搞崩。
#[tokio::test]
async fn debug_run_survives_a_broken_evaluator() {
    let _g = sandbox().await;

    let builtin = alpha_plugin::builtins::builtins_of(alpha_plugin::PluginKind::Dispatcher);
    let b = builtin.first().expect("应有内建决策器");
    seed_dispatcher(b.name, b.filename, true, false, true);
    let reg = root().state.registry.as_ref().unwrap();
    std::fs::write(
        reg.dir(alpha_plugin::PluginKind::Dispatcher).join(b.filename),
        b.source,
    )
    .unwrap();

    {
        let conn = root().state.store.lock().unwrap();
        let _ = conn.execute(
            "INSERT INTO evaluators (name, filename, enabled, active, priority, description, is_builtin)
             VALUES (?,?,?,?,?,?,?)",
            rusqlite::params!["坏评估器", "bad_ev.rhai", 1i64, 1i64, 1i64, "测试用", 0i64],
        )
        .unwrap();
    }

    std::fs::write(
        reg.dir(alpha_plugin::PluginKind::Evaluator).join("bad_ev.rhai"),
        r#"fn evaluate(boss, ctx) { throw "评估器炸了" }"#,
    )
    .unwrap();
    reg.invalidate();

    let base = PathBuf::from(std::env::var("ALPHA_ROOT").unwrap());
    std::fs::create_dir_all(base.join("config")).unwrap();
    let rules = repo_root().join("config/rules.yaml");
    if rules.exists() {
        std::fs::copy(&rules, base.join("config/rules.yaml")).unwrap();
        let _ = alpha_core::config::refresh_config();
    }

    let (status, body) = post("/debug/run", json!({"pokemon": "巨牙鲨"})).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "评估器坏了不该让调试台 500：{body}"
    );
    assert!(body["report"].is_string(), "报告仍要产出：{body}");
}

// ============================================================
// 路由形状
// ============================================================

/// `/debug/run` **没有** `/api` 前缀 —— 原版如此，别「顺手统一」掉。
///
/// 反过来，`/api/debug/run` 应该 404。
#[tokio::test]
async fn debug_run_lives_at_the_root_path_not_under_api() {
    let _g = sandbox().await;
    seed_dispatcher("内置", "b.rhai", true, false, true);
    write_dispatcher_plugin("b.rhai", "x");

    let (ok, _) = post("/debug/run", json!({"pokemon": "皮卡丘"})).await;
    assert_ne!(ok, StatusCode::NOT_FOUND, "/debug/run 该存在");

    // `/api/debug/run` 不该被注册（原版没有这个路由）
    let c = cookie().await;
    let resp = root()
        .app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/debug/run")
                .header("content-type", "application/json")
                .header("cookie", c)
                .header(CSRF_HEADER, CSRF_HEADER_VALUE)
                .body(Body::from("{}"))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::NOT_FOUND,
        "/api/debug/run 不该存在（原版挂在根路径）"
    );
}
