//! 日志 3 个接口 + 系统配置 3 个接口的端到端测试。
//!
//! 这两组放一个文件里，因为它们共用同一个测试环境，
//! 而且 `POST /api/system/save` 会**真的去改进程级环境变量** ——
//! 这跟日志用例读同一份全局状态，串行化必须一起做。
//!
//! # 环境变量用例最麻烦的地方
//!
//! `save_system` 会 `std::env::set_var` / `remove_var`，这是**进程级**副作用，
//! 且 Rust 里 `set_var` 在 2024 edition 下标记为 unsafe（多线程下不保证安全）。
//! 所以：
//!
//! 1. 所有用例共用一把锁（与其它 e2e 文件同款 [`sandbox`]），
//!    保证同一时刻只有一个用例在动环境变量
//! 2. 每个用例自己恢复现场 —— 只靠锁不做清理的话，
//!    下一个用例会看到上一个留下的值
//! 3. 用 [`EnvGuard`] 做 RAII 恢复：即使断言 panic 也会复原

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

fn root() -> &'static TestRoot {
    static ROOT: OnceLock<TestRoot> = OnceLock::new();
    ROOT.get_or_init(|| {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path().to_path_buf();

        std::fs::create_dir_all(base.join("config")).unwrap();
        std::fs::write(base.join("config/sources.yaml"), "sources: []\n").unwrap();
        std::fs::write(
            base.join("config/settings.yaml"),
            "# 测试用配置\ntimezone: Asia/Shanghai\nlanguage: zh\n",
        )
        .unwrap();
        std::fs::write(base.join("panel.env"), "# Alpha 面板环境变量\n").unwrap();

        // `ALPHA_ROOT` / `PANEL_ENV_FILE` 必须在**任何**路径函数调用前设好。
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
        let state = AppState::new(cfg, store);
        let app = build_router(state.clone());

        TestRoot {
            _dir: dir,
            app,
            state,
        }
    })
}

fn env_file() -> PathBuf {
    PathBuf::from(std::env::var("PANEL_ENV_FILE").unwrap())
}

/// 拿排他锁并复位所有被这些接口改过的状态。
///
/// guard **必须**绑到变量上（`let _g = sandbox().await;`）。
///
/// # 为什么要连 `settings.yaml` 一起复位
///
/// `POST /api/system/save` 会写 `config/settings.yaml`（时区、语言）。
/// 只清数据库的话，`save_updates_timezone_in_settings_yaml` 把时区改成
/// `Asia/Tokyo` 之后，下一个用例读到的时区就不是种子里的
/// `Asia/Shanghai` —— 表现为「顺序相关的假失败」。
/// 用户表同理（`panel_lang` 是**按用户**存的，最后一个用例改了它，
/// 下一个用例读到的就是那个值）。第一版漏了，三个用例因此交叉污染。
async fn sandbox() -> tokio::sync::MutexGuard<'static, ()> {
    static LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
    let guard = LOCK.lock().await;
    {
        let conn = root().state.store.lock().unwrap();
        conn.execute("DELETE FROM logs", []).unwrap();
        conn.execute("DELETE FROM kv", []).unwrap();
        // 用户语言与其它字段一起复原（面板语言存在这里）
        conn.execute("UPDATE users SET lang='zh'", []).unwrap();
    }
    // 只改 `timezone:` / `language:` 两行，保留注释 ——
    // 有个用例正是靠「注释还在」来验证 config_mgr 的文本级编辑。
    let base = PathBuf::from(std::env::var("ALPHA_ROOT").unwrap());
    std::fs::write(
        base.join("config/settings.yaml"),
        "# 测试用配置\ntimezone: Asia/Shanghai\nlanguage: zh\n",
    )
    .unwrap();
    std::fs::write(env_file(), "# Alpha 面板环境变量\n").unwrap();
    std::env::remove_var("transfor_url");
    std::env::remove_var("WXPUSHER_APP_TOKEN_alpha");
    guard
}

/// RAII：用例结束（含 panic）时把环境变量恢复原状。
///
/// 不能只靠 [`sandbox`] 里的清理 —— 那是在**下一个**用例开始时跑的，
/// 而这个用例自己的断言已经在中间执行完了。更要紧的是：
/// 万一 panic 发生在断言处，`sandbox` 的清理仍会跑，
/// 但读 env 的其它测试（不在这个文件里）不受锁保护。
struct EnvGuard {
    saved: Vec<(&'static str, Option<String>)>,
}

impl EnvGuard {
    fn capture(keys: &[&'static str]) -> Self {
        Self {
            saved: keys
                .iter()
                .map(|k| (*k, std::env::var(k).ok()))
                .collect(),
        }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        for (k, v) in &self.saved {
            match v {
                Some(val) => std::env::set_var(k, val),
                None => std::env::remove_var(k),
            }
        }
    }
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

/// 往日志表里塞一条**当前时刻**的记录，供查询用例用。
///
/// 时间戳必须取「现在」而不是写死。第一版写死了
/// `2026-09-22 10:00:xx`，结果 `clean` 用例里 `days=0` 算出的
/// 截止点是「今天 13:xx」，那些记录全被判为「过期」删掉 ——
/// 用例名说「不该删光」但实际删了，而根因是测试数据的时间戳是过去。
///
/// 走的是 `Store::log`（它会用当前北京时间），没有额外依赖。
fn seed_log(level: &str, kind: &str, msg: &str, source: &str) {
    let lv = match level {
        "error" => alpha_store::LogLevel::Error,
        "warning" => alpha_store::LogLevel::Warning,
        "debug" => alpha_store::LogLevel::Debug,
        _ => alpha_store::LogLevel::Info,
    };
    root().state.store.log(lv, kind, msg, source);
}

/// 塞一条**指定天数之前**的记录（给 `clean` 用例造「过期数据」）。
///
/// 这个必须直写 SQL：`Store::log` 只会用当前时间，
/// 造不出「100 天前」的记录 —— 而那正是不该被保住的那些。
fn seed_old_log(days_ago: i64, msg: &str) {
    let conn = root().state.store.lock().unwrap();
    conn.execute(
        "INSERT INTO logs (ts, level, kind, message, source) VALUES (?,?,?,?,?)",
        rusqlite::params![
            alpha_store::stamp_days_ago(days_ago),
            "info",
            "panel",
            msg,
            "panel"
        ],
    )
    .unwrap();
}

// ------------------------------------------------------------ 鉴权

#[tokio::test]
async fn all_logs_and_system_routes_require_login() {
    let _g = sandbox().await;
    let paths = [
        ("GET", "/api/logs"),
        ("POST", "/api/logs/clean"),
        ("POST", "/api/logs/state"),
        ("GET", "/api/system"),
        ("POST", "/api/system/save"),
        ("GET", "/api/about"),
    ];

    for (method, path) in paths {
        let resp = root()
            .app
            .clone()
            .oneshot(
                Request::builder()
                    .method(method)
                    .uri(path)
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
            "{method} {path} 未登录时该返回 401"
        );
    }
}

// ------------------------------------------------------------ 日志查询

#[tokio::test]
async fn logs_returns_rows_sources_and_the_realtime_flag() {
    let _g = sandbox().await;
    seed_log("info", "dispatcher", "一条普通日志", "panel");
    seed_log("error", "channel", "一条错误日志", "wxpusher");

    let (status, body) = get("/api/logs").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["logs"].as_array().unwrap().len(), 2);

    let sources = body["sources"].as_array().unwrap();
    assert!(sources.iter().any(|s| s == "panel"), "该列出 panel: {body}");
    assert!(
        sources.iter().any(|s| s == "wxpusher"),
        "该列出 wxpusher: {body}"
    );

    // 原版缺省是**开**（`get_kv("query_log", "1") == "1"`）
    assert_eq!(
        body["query_log"],
        json!(true),
        "kv 里没这个键时该默认开启"
    );
}

#[tokio::test]
async fn logs_filters_by_kind_and_source() {
    let _g = sandbox().await;
    seed_log("info", "dispatcher", "A", "panel");
    seed_log("error", "channel", "B", "wxpusher");
    seed_log("info", "channel", "C", "panel");

    let (_, body) = get("/api/logs?kind=channel").await;
    assert_eq!(body["logs"].as_array().unwrap().len(), 2);

    let (_, body) = get("/api/logs?source=wxpusher").await;
    assert_eq!(body["logs"].as_array().unwrap().len(), 1);
    assert_eq!(body["logs"][0]["message"], "B");

    // 两个一起用是 AND
    let (_, body) = get("/api/logs?kind=channel&source=panel").await;
    assert_eq!(body["logs"].as_array().unwrap().len(), 1);
    assert_eq!(body["logs"][0]["message"], "C");
}

#[tokio::test]
async fn logs_empty_filter_values_mean_no_filter() {
    let _g = sandbox().await;
    seed_log("info", "dispatcher", "A", "panel");
    seed_log("error", "channel", "B", "wxpusher");

    // 前端的下拉框默认项会提交空串 —— 那该等价于「不筛」
    let (status, body) = get("/api/logs?kind=&source=").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body["logs"].as_array().unwrap().len(),
        2,
        "空串筛选条件不该把结果清空"
    );
}

#[tokio::test]
async fn logs_garbage_limit_does_not_500() {
    let _g = sandbox().await;
    seed_log("info", "panel", "A", "panel");

    // 原版 `int("abc")` 会抛 ValueError → 500。本版回落缺省。
    for bad in ["abc", "", "-1", "0", "1e5"] {
        let (status, body) = get(&format!("/api/logs?limit={bad}")).await;
        assert_eq!(status, StatusCode::OK, "limit={bad:?} 不该 500: {body}");
        assert!(!body["logs"].as_array().unwrap().is_empty());
    }
}

#[tokio::test]
async fn logs_respects_limit() {
    let _g = sandbox().await;
    for i in 0..5 {
        seed_log("info", "panel", &format!("第{i}条"), "panel");
    }
    let (_, body) = get("/api/logs?limit=2").await;
    assert_eq!(body["logs"].as_array().unwrap().len(), 2);
}

// ------------------------------------------------------------ 清理日志

#[tokio::test]
async fn clean_deletes_only_old_rows() {
    let _g = sandbox().await;
    // 插一条 100 天前的（该被删）与一条今天的（该留下）
    seed_old_log(100, "远古日志");
    seed_log("info", "panel", "今天的", "panel");

    let (status, body) = post("/api/logs/clean", json!({"days": 3})).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["ok"], json!(true));
    assert_eq!(body["deleted"], json!(1));

    let remaining = root()
        .state
        .store
        .list_logs(&alpha_store::LogFilter::new(100))
        .unwrap();
    assert!(
        remaining.iter().any(|l| l.message == "今天的"),
        "今天的日志不该被删"
    );
    assert!(
        !remaining.iter().any(|l| l.message == "远古日志"),
        "100 天前的该被删掉"
    );
}

#[tokio::test]
async fn clean_writes_its_own_log_entry() {
    let _g = sandbox().await;
    post("/api/logs/clean", json!({"days": 7})).await;

    let logs = root()
        .state
        .store
        .list_logs(&alpha_store::LogFilter::new(100))
        .unwrap();
    assert!(
        logs.iter()
            .any(|l| l.message.contains("清理 7 天前日志")),
        "清理动作本身要留痕: {:?}",
        logs.iter().map(|l| &l.message).collect::<Vec<_>>()
    );
}

#[tokio::test]
async fn clean_negative_days_does_not_wipe_everything() {
    let _g = sandbox().await;
    seed_log("info", "panel", "该被保住", "panel");

    // 负数天会让时间点跑到未来，`ts < 未来` 就把所有日志删光。
    // 本版把它夹到 0。
    let (status, body) = post("/api/logs/clean", json!({"days": -30})).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(
        root()
            .state
            .store
            .list_logs(&alpha_store::LogFilter::new(100))
            .unwrap()
            .iter()
            .any(|l| l.message == "该被保住"),
        "负数 days 不该把日志全删光"
    );
}

#[tokio::test]
async fn clean_defaults_to_three_days() {
    let _g = sandbox().await;
    post("/api/logs/clean", json!({})).await;
    let logs = root()
        .state
        .store
        .list_logs(&alpha_store::LogFilter::new(100))
        .unwrap();
    assert!(
        logs.iter().any(|l| l.message.contains("清理 3 天前日志")),
        "缺 days 字段时该默认 3 天"
    );
}

// ------------------------------------------------------------ 实时日志开关

#[tokio::test]
async fn log_state_toggles_the_kv_flag() {
    let _g = sandbox().await;

    let (status, body) = post("/api/logs/state", json!({"enabled": false})).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["enabled"], json!(false));
    assert_eq!(
        root().state.store.get_kv("query_log").unwrap().as_deref(),
        Some("0")
    );

    let (_, body) = post("/api/logs/state", json!({"enabled": true})).await;
    assert_eq!(body["enabled"], json!(true));
    assert_eq!(
        root().state.store.get_kv("query_log").unwrap().as_deref(),
        Some("1")
    );
}

#[tokio::test]
async fn log_state_round_trips_through_the_logs_endpoint() {
    let _g = sandbox().await;

    // 关掉之后，列表接口该读到 false
    post("/api/logs/state", json!({"enabled": false})).await;
    let (_, body) = get("/api/logs").await;
    assert_eq!(body["query_log"], json!(false), "开关该被列表接口反映出来");

    post("/api/logs/state", json!({"enabled": true})).await;
    let (_, body) = get("/api/logs").await;
    assert_eq!(body["query_log"], json!(true));
}

#[tokio::test]
async fn log_state_missing_field_defaults_to_off() {
    let _g = sandbox().await;
    // 原版 `bool(data.get("enabled", False))` —— 缺省是**关**。
    // 注意这与 `GET /api/logs` 读 KV 时的缺省（开）方向相反，是原版的事实行为。
    let (_, body) = post("/api/logs/state", json!({})).await;
    assert_eq!(body["enabled"], json!(false));
}

// ------------------------------------------------------------ 系统配置：读

#[tokio::test]
async fn system_reports_settings_and_env_state() {
    let _g = sandbox().await;
    let _env = EnvGuard::capture(&["transfor_url", "WXPUSHER_APP_TOKEN_alpha"]);

    let (status, body) = get("/api/system").await;
    assert_eq!(status, StatusCode::OK);

    assert_eq!(body["timezone"], "Asia/Shanghai");
    assert_eq!(body["push_lang"], "zh");
    assert!(body["panel_lang"].is_string());
    assert!(body["timezones"].as_array().unwrap().contains(&json!("UTC")));
    assert!(body["env_file"].is_string(), "该告诉前端 env 文件在哪: {body}");
}

#[tokio::test]
async fn system_env_never_leaks_the_plaintext_token() {
    let _g = sandbox().await;
    let _env = EnvGuard::capture(&["WXPUSHER_APP_TOKEN_alpha"]);

    std::env::set_var("WXPUSHER_APP_TOKEN_alpha", "AT_super_secret_9876");

    let resp = get_raw("/api/system").await;
    assert_eq!(resp.status(), StatusCode::OK);
    let text =
        String::from_utf8(resp.into_body().collect().await.unwrap().to_bytes().to_vec()).unwrap();

    assert!(
        !text.contains("AT_super_secret_9876"),
        "明文 token 泄漏到响应里了: {text}"
    );
    assert!(text.contains("••••9876"), "该回末四位的掩码: {text}");

    let body: Value = serde_json::from_str(&text).unwrap();
    let field = body["envs"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["key"] == "WXPUSHER_APP_TOKEN_alpha")
        .unwrap();
    assert_eq!(field["set"], json!(true), "该告诉前端「已设置」");
}

#[tokio::test]
async fn system_marks_unset_env_vars() {
    let _g = sandbox().await;
    let _env = EnvGuard::capture(&["transfor_url"]);

    let (_, body) = get("/api/system").await;
    let field = body["envs"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["key"] == "transfor_url")
        .unwrap();
    assert_eq!(field["set"], json!(false));
    assert_eq!(field["preview"], json!(""), "没设置时不该有掩码预览");
}

#[tokio::test]
async fn system_env_mask_uses_four_dots_not_six() {
    let _g = sandbox().await;
    let _env = EnvGuard::capture(&["transfor_url"]);
    std::env::set_var("transfor_url", "https://proxy.test/abc");

    let (_, body) = get("/api/system").await;
    let p = body["envs"][0]["preview"].as_str().unwrap();
    // 原版这里是 `"••••" + v[-4:]`（四个点），渠道页是六个。别统一它们。
    assert_eq!(p, "••••/abc", "该是四个点: {p}");
    assert!(!p.starts_with("••••••"), "不是六个点: {p}");
}

// ------------------------------------------------------------ 系统配置：写

#[tokio::test]
async fn save_updates_timezone_in_settings_yaml() {
    let _g = sandbox().await;
    let (status, body) = post("/api/system/save", json!({"timezone": "Asia/Tokyo"})).await;
    assert_eq!(status, StatusCode::OK, "{body}");

    let (_, body) = get("/api/system").await;
    assert_eq!(body["timezone"], "Asia/Tokyo");

    // 注释必须还在 —— 这是 config_mgr 用文本级行编辑的意义
    let text = std::fs::read_to_string(
        PathBuf::from(std::env::var("ALPHA_ROOT").unwrap()).join("config/settings.yaml"),
    )
    .unwrap();
    assert!(text.contains("# 测试用配置"), "顶层注释被吃掉了:\n{text}");
}

#[tokio::test]
async fn save_rejects_unknown_push_lang_but_still_saves_the_rest() {
    let _g = sandbox().await;

    let (status, _) = post(
        "/api/system/save",
        json!({"timezone": "UTC", "push_lang": "klingon"}),
    )
    .await;
    // 非法推送语言被静默忽略（原版是 `if ... in (...)`），但其它字段照存
    assert_eq!(status, StatusCode::OK);

    let (_, body) = get("/api/system").await;
    assert_eq!(body["timezone"], "UTC", "时区该照常保存");
    assert_eq!(body["push_lang"], "zh", "非法语言不该写进去");
}

#[tokio::test]
async fn save_accepts_push_lang_both() {
    let _g = sandbox().await;
    post("/api/system/save", json!({"push_lang": "both"})).await;

    let (_, body) = get("/api/system").await;
    assert_eq!(body["push_lang"], "both", "both（双语）是合法值");
}

#[tokio::test]
async fn save_panel_lang_is_stored_per_user() {
    let _g = sandbox().await;

    post("/api/system/save", json!({"panel_lang": "en"})).await;
    let (_, body) = get("/api/system").await;
    assert_eq!(body["panel_lang"], "en", "面板语言该按用户存下来");

    post("/api/system/save", json!({"panel_lang": "zh"})).await;
    let (_, body) = get("/api/system").await;
    assert_eq!(body["panel_lang"], "zh");
}

#[tokio::test]
async fn save_writes_env_file_and_hot_reloads_the_process() {
    let _g = sandbox().await;
    let _env = EnvGuard::capture(&["transfor_url"]);

    let (status, body) = post(
        "/api/system/save",
        json!({"envs": {"transfor_url": "https://proxy.example.test"}}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        body["restarted"],
        json!(true),
        "写成功了就该重载配置缓存: {body}"
    );

    // 1. 落盘了
    let text = std::fs::read_to_string(env_file()).unwrap();
    assert!(
        text.contains("transfor_url=https://proxy.example.test"),
        "该写进 panel.env:\n{text}"
    );
    // 头部标记该被插入（原版「判定用短版、插入用长版」的行为）
    assert!(text.contains("Alpha 面板环境变量"), "该有头部注释:\n{text}");

    // 2. 当前进程也生效了（不需要重启服务）
    assert_eq!(
        std::env::var("transfor_url").unwrap(),
        "https://proxy.example.test",
        "该热更新到当前进程"
    );
}

#[tokio::test]
async fn save_empty_env_value_clears_it() {
    let _g = sandbox().await;
    let _env = EnvGuard::capture(&["transfor_url"]);

    post(
        "/api/system/save",
        json!({"envs": {"transfor_url": "https://first.test"}}),
    )
    .await;
    assert!(std::env::var("transfor_url").is_ok());

    // 空串 = 清除（原版 `os.environ.pop(k, None)`）
    post(
        "/api/system/save",
        json!({"envs": {"transfor_url": ""}}),
    )
    .await;
    assert!(
        std::env::var("transfor_url").is_err(),
        "空串该把变量从进程里清掉"
    );
}

#[tokio::test]
async fn save_null_env_value_is_skipped_not_deleted() {
    let _g = sandbox().await;
    let _env = EnvGuard::capture(&["transfor_url"]);

    post(
        "/api/system/save",
        json!({"envs": {"transfor_url": "https://keep.me"}}),
    )
    .await;

    // null = 跳过（原版 `if v is None: continue`），不是删除
    let (status, _) = post(
        "/api/system/save",
        json!({"envs": {"transfor_url": null}}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        std::env::var("transfor_url").unwrap(),
        "https://keep.me",
        "null 该被跳过，不是删除"
    );
}

#[tokio::test]
async fn save_with_no_changes_does_not_report_restarted() {
    let _g = sandbox().await;
    let _env = EnvGuard::capture(&["transfor_url"]);

    post(
        "/api/system/save",
        json!({"envs": {"transfor_url": "https://same.value"}}),
    )
    .await;

    // 再存一次同样的值 —— 文件内容没变，不该报 restarted
    let (_, body) = post(
        "/api/system/save",
        json!({"envs": {"transfor_url": "https://same.value"}}),
    )
    .await;
    assert_eq!(
        body["restarted"],
        json!(false),
        "值没变时不该重载（也不该刷新文件 mtime）: {body}"
    );
}

#[tokio::test]
async fn save_always_writes_a_system_log() {
    let _g = sandbox().await;
    post("/api/system/save", json!({"timezone": "UTC"})).await;

    let logs = root()
        .state
        .store
        .list_logs(&alpha_store::LogFilter::new(100))
        .unwrap();
    assert!(
        logs.iter().any(|l| l.message == "保存系统配置"),
        "每次保存都该留一条日志: {:?}",
        logs.iter().map(|l| &l.message).collect::<Vec<_>>()
    );
}

// ------------------------------------------------------------ 关于页

#[tokio::test]
async fn about_returns_the_bilingual_content() {
    let _g = sandbox().await;
    let (status, body) = get("/api/about").await;
    assert_eq!(status, StatusCode::OK);

    assert_eq!(body["title"]["zh"], "Alpha 分发决策面板");
    assert_eq!(body["version"], "2.0");
    assert!(
        body["project"]["zh"]["flow"].as_array().unwrap().len() >= 5,
        "该有五步流程说明: {body}"
    );
    assert!(
        !body["thanks"]["en"].as_array().unwrap().is_empty(),
        "该有致谢: {body}"
    );
    assert!(
        !body["git"].as_str().unwrap().is_empty(),
        "该带上仓库地址"
    );
}
