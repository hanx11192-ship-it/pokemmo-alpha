//! 源管理 9 个接口的端到端测试。
//!
//! 这批测试与其它路由的不同之处：**它们真的会读写 `config/sources.yaml`**。
//!
//! # 沙箱策略
//!
//! 每个用例进来先调 [`sandbox()`]，它做两件事：
//!
//! 1. **重置** `config/sources.yaml` 为已知的种子内容
//! 2. 返回一个全局 `MutexGuard`，整个用例期间持有
//!
//! 之所以要重置：这些接口是**有状态**的（`edit` 能改名、`delete` 能删源），
//! 用例之间会互相破坏。第一版没重置，出现的是「删掉种子源一之后，
//! 另一个用例断言它还在」这类顺序相关的假失败 —— 比真 bug 更难查。
//!
//! 之所以要串行：`alpha_core::config_mgr` 的路径全部来自
//! `alpha_core::config::project_root()`（一个进程级 `OnceCell`），
//! 只能用 `ALPHA_ROOT` 环境变量指到临时目录，而那是个**进程级**的副作用。
//!
//! # 关于 `sandbox()` 返回的 guard
//!
//! 必须绑到变量上（`let _g = sandbox().await;`），不能写成 `sandbox();` ——
//! 后者创建的 `MutexGuard` 会在语句结束时立刻析构，锁瞬间就放了。
//! 这不是风格问题：写错了测试会**偶发**失败。

use std::path::PathBuf;
use std::sync::OnceLock;

use alpha_server::auth::layer::{CSRF_HEADER, CSRF_HEADER_VALUE};
use alpha_server::{build_router, AppState, Config};
use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use http_body_util::BodyExt;
use serde_json::Value;
use tower::ServiceExt;

/// 测试根的初始化结果。
struct TestRoot {
    _dir: tempfile::TempDir,
    app: Router,
}

/// 全局测试环境。见模块头部关于串行化的说明。
fn root() -> &'static TestRoot {
    static ROOT: OnceLock<TestRoot> = OnceLock::new();
    ROOT.get_or_init(|| {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path().to_path_buf();

        std::fs::create_dir_all(base.join("config")).unwrap();
        std::fs::write(base.join("config/sources.yaml"), SEED_SOURCES).unwrap();
        std::fs::write(
            base.join("config/settings.yaml"),
            "# 注释在顶上\ntimezone: Asia/Shanghai\nlanguage: zh\n",
        )
        .unwrap();
        std::fs::write(base.join("panel.env"), "# 环境变量\nA=1\n").unwrap();

        // `ALPHA_ROOT` 必须**在**任何 `project_root()` 调用之前设好。
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
        let app = build_router(state);

        TestRoot { _dir: dir, app }
    })
}

/// 拿排他锁并把 `sources.yaml` 重置成种子内容。
///
/// 返回的 guard **必须**绑到变量上，否则锁在语句结束时就放了。
/// 见模块头部说明。
///
/// # 为什么用 tokio 的异步 `Mutex` 而不是 `std::sync::Mutex`
///
/// `std::sync::MutexGuard` 跨 `await` 持有是被 clippy 明确警告的
/// （`await_holding_lock`）：在单线程 runtime 上，持锁期间 `await`
/// 到另一个也要抢同一把锁的任务，就会**死锁**。
/// 这里的 guard 要跨好几个 `await`（发请求、收响应），
/// 所以必须用异步感知的锁 —— 它会在 `await` 时让出，
/// 而不是把整个 executor 线程堵住。
async fn sandbox() -> tokio::sync::MutexGuard<'static, ()> {
    static LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
    let guard = LOCK.lock().await;
    root();
    if let Ok(base) = std::env::var("ALPHA_ROOT") {
        std::fs::write(
            PathBuf::from(base).join("config/sources.yaml"),
            SEED_SOURCES,
        )
        .unwrap();
    }
    guard
}

/// 拿到一个有效的会话 cookie。
///
/// 走的是**真实的登录接口**（首登即管理员），不是手搓 cookie ——
/// 这样测试才会在「登录链路坏了」时失败，而不是恰好绕过它。
///
/// # 为什么不能在这里 `block_on`
///
/// 这些是 `#[tokio::test]`，调用方已经在一个 tokio runtime 里；
/// 再起一个 runtime 会直接 panic（"Cannot start a runtime from within
/// a runtime"）。所以必须保持 `async`，用 `OnceCell` 缓存**结果**。
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
                    serde_json::to_vec(&serde_json::json!({
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

const SEED_SOURCES: &str = "\
# ============================================================
# 数据源注册表（测试种子文件）
# ============================================================
sources:
  - name: 种子源一
    adapter: lzpoke_reports
    # 这个源要走代理
    enabled: true
    priority: 10
    options:
      target: https://example.test/one
  - name: 种子源二
    adapter: neodex_alpha
    enabled: false
    priority: 20
    options:
      target: https://example.test/two
";

async fn send(req: Request<Body>) -> (StatusCode, Value) {
    let resp = root().app.clone().oneshot(req).await.unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, value)
}

async fn get(path: &str) -> (StatusCode, Value) {
    send(
        Request::builder()
            .uri(path)
            .header("cookie", cookie().await)
            .body(Body::empty())
            .unwrap(),
    )
    .await
}

async fn post(path: &str, body: Value) -> (StatusCode, Value) {
    send(
        Request::builder()
            .method("POST")
            .uri(path)
            .header("content-type", "application/json")
            .header("cookie", cookie().await)
            .header(CSRF_HEADER, CSRF_HEADER_VALUE)
            .body(Body::from(serde_json::to_vec(&body).unwrap()))
            .unwrap(),
    )
    .await
}

async fn delete(path: &str) -> (StatusCode, Value) {
    send(
        Request::builder()
            .method("DELETE")
            .uri(path)
            .header("cookie", cookie().await)
            .header(CSRF_HEADER, CSRF_HEADER_VALUE)
            .body(Body::empty())
            .unwrap(),
    )
    .await
}

/// 带会话的 GET，返回**原始响应**（要读头部/正文原文的用例用）。
async fn get_raw(path: &str) -> axum::response::Response {
    root()
        .app
        .clone()
        .oneshot(
            Request::builder()
                .uri(path)
                .header("cookie", cookie().await)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap()
}

/// 读回种子文件原文（断言「注释没丢」用）。
fn read_seed() -> String {
    let base = PathBuf::from(std::env::var("ALPHA_ROOT").unwrap());
    std::fs::read_to_string(base.join("config/sources.yaml")).unwrap()
}

/// 每个用例用独立的源名，避免共享同一个文件时的相互干扰。
fn uniq(prefix: &str) -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static N: AtomicU64 = AtomicU64::new(0);
    format!("{prefix}{}", N.fetch_add(1, Ordering::Relaxed))
}

// ---------------------------------------------------------------------------
// 鉴权：这 9 条全都要登录
// ---------------------------------------------------------------------------

#[tokio::test]
async fn sources_routes_require_login() {
    let _g = sandbox().await;

    for (method, path) in [
        ("GET", "/api/sources"),
        ("POST", "/api/sources"),
        ("POST", "/api/sources/x/enable"),
        ("POST", "/api/sources/x/priority"),
        ("POST", "/api/sources/x/edit"),
        ("DELETE", "/api/sources/x"),
        ("POST", "/api/sources/upload"),
        ("GET", "/api/sources/x/download"),
        ("GET", "/api/sources/template"),
    ] {
        let req = Request::builder()
            .method(method)
            .uri(path)
            .header("content-type", "application/json")
            .header(CSRF_HEADER, CSRF_HEADER_VALUE)
            .body(Body::from("{}"))
            .unwrap();
        let resp = root().app.clone().oneshot(req).await.unwrap();
        assert_eq!(
            resp.status(),
            StatusCode::UNAUTHORIZED,
            "{method} {path} 未登录时必须是 401"
        );
    }
}

/// 忘了 CSRF 头 → 400（状态变更是防 CSRF 的）。
#[tokio::test]
async fn state_changing_routes_reject_missing_csrf_header() {
    let _g = sandbox().await;
    let req = Request::builder()
        .method("POST")
        .uri("/api/sources")
        .header("content-type", "application/json")
        .body(Body::from("{}"))
        .unwrap();
    let resp = root().app.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

// ---------------------------------------------------------------------------
// GET /api/sources
// ---------------------------------------------------------------------------

#[tokio::test]
async fn list_sources_returns_seed_sources_with_notes() {
    let _g = sandbox().await;
    let (status, body) = get("/api/sources").await;
    assert_eq!(status, StatusCode::OK);

    let srcs = body["sources"].as_array().expect("要有 sources 数组");
    let one = srcs
        .iter()
        .find(|s| s["name"] == "种子源一")
        .expect("种子源一 应该在列表里");

    assert_eq!(one["adapter"], "lzpoke_reports");
    assert_eq!(one["enabled"], true);
    assert_eq!(one["priority"], 10);
    assert_eq!(one["options"]["target"], "https://example.test/one");
    // note = target（没有开启的附加项时只有 target）
    assert_eq!(one["note"], "https://example.test/one");

    let two = srcs.iter().find(|s| s["name"] == "种子源二").unwrap();
    assert_eq!(two["enabled"], false);

    assert!(body["adapters"].is_array(), "要带上可用适配器清单");
}

// ---------------------------------------------------------------------------
// POST /api/sources
// ---------------------------------------------------------------------------

#[tokio::test]
async fn add_source_writes_to_yaml_and_keeps_comments() {
    let _g = sandbox().await;
    let name = uniq("新增源");

    let (status, body) = post(
        "/api/sources",
        serde_json::json!({
            "name": name,
            "adapter": "lzpoke_reports",
            "target": "https://example.test/new?x=1&y=2"
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["ok"], true);

    // 文件里真的多了这个源，而且**种子文件的注释没被吃掉**
    let text = read_seed();
    assert!(text.contains(&name), "新源没写进文件:\n{text}");
    assert!(
        text.contains("# 这个源要走代理"),
        "种子里的人工注释被吃掉了:\n{text}"
    );
    assert!(
        text.contains("# ============================================================"),
        "头注释被吃掉了:\n{text}"
    );

    // 重新读回来（走的是 config_mgr 的解析路径）
    let (_, list) = get("/api/sources").await;
    let found = list["sources"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["name"] == name.as_str())
        .expect("新增的源应能读回来");
    assert_eq!(found["enabled"], true, "新增源默认启用");
    assert_eq!(found["priority"], 50, "新增源默认优先级 50");
    assert_eq!(
        found["options"]["target"], "https://example.test/new?x=1&y=2",
        "带 ? & = 的地址要原样往返"
    );
}

#[tokio::test]
async fn add_source_rejects_empty_name_or_adapter() {
    let _g = sandbox().await;
    let (status, body) = post("/api/sources", serde_json::json!({"name": "  "})).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"], "名称和适配器必填");
}

#[tokio::test]
async fn add_source_rejects_duplicate_name() {
    let _g = sandbox().await;
    let (status, body) = post(
        "/api/sources",
        serde_json::json!({"name": "种子源一", "adapter": "lzpoke_reports"}),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"], "同名源已存在");
}

// ---------------------------------------------------------------------------
// enable / priority
// ---------------------------------------------------------------------------

#[tokio::test]
async fn enable_toggles_the_seed_source_and_keeps_comments() {
    let _g = sandbox().await;
    let (status, _) = post(
        "/api/sources/种子源二/enable",
        serde_json::json!({"enabled": true}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let text = read_seed();
    assert!(text.contains("# 这个源要走代理"), "注释被吃掉了:\n{text}");
    assert!(text.contains("# 数据源注册表（测试种子文件）"), "头注释被吃掉了:\n{text}");

    let (_, list) = get("/api/sources").await;
    let two = list["sources"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["name"] == "种子源二")
        .unwrap();
    assert_eq!(two["enabled"], true, "开关该生效了");
}

/// 源不存在时**仍然返回 ok: true** —— 这是原版行为，刻意保留。
#[tokio::test]
async fn enable_on_missing_source_still_reports_ok() {
    let _g = sandbox().await;
    let (status, body) = post(
        "/api/sources/根本不存在的源/enable",
        serde_json::json!({"enabled": true}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["ok"], true, "原版此处就是静默成功");
}

#[tokio::test]
async fn priority_updates_only_that_source() {
    let _g = sandbox().await;
    let (status, _) = post(
        "/api/sources/种子源一/priority",
        serde_json::json!({"priority": 3}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let (_, list) = get("/api/sources").await;
    let arr = list["sources"].as_array().unwrap();
    let one = arr.iter().find(|s| s["name"] == "种子源一").unwrap();
    let two = arr.iter().find(|s| s["name"] == "种子源二").unwrap();
    assert_eq!(one["priority"], 3);
    assert_eq!(two["priority"], 20, "另一个源的优先级不该被动");
}

// ---------------------------------------------------------------------------
// edit（含改名）
// ---------------------------------------------------------------------------

#[tokio::test]
async fn edit_changes_target_without_touching_other_fields() {
    let _g = sandbox().await;
    let (status, body) = post(
        "/api/sources/种子源一/edit",
        serde_json::json!({"target": "https://example.test/changed", "priority": 7}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["renamed"], false);
    assert_eq!(body["name"], "种子源一");

    let (_, list) = get("/api/sources").await;
    let one = list["sources"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["name"] == "种子源一")
        .unwrap();
    assert_eq!(one["options"]["target"], "https://example.test/changed");
    assert_eq!(one["priority"], 7);
    assert_eq!(one["adapter"], "lzpoke_reports", "没提交的字段该保持原样");
    assert_eq!(one["enabled"], true);
}

#[tokio::test]
async fn edit_can_rename_and_keeps_position() {
    let _g = sandbox().await;
    let new_name = uniq("改名后");

    let (status, body) = post(
        "/api/sources/种子源一/edit",
        serde_json::json!({"name": new_name}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["renamed"], true);
    assert_eq!(body["name"], new_name.as_str());

    let (_, list) = get("/api/sources").await;
    let arr = list["sources"].as_array().unwrap();
    let names: Vec<&str> = arr.iter().filter_map(|s| s["name"].as_str()).collect();

    // 位置不变：改名后的源仍在「种子源二」之前
    let pos_new = names.iter().position(|n| *n == new_name).expect("改名后的源要在");
    let pos_two = names.iter().position(|n| *n == "种子源二").unwrap();
    assert!(pos_new < pos_two, "改名不该改变位置: {names:?}");

    assert!(!names.contains(&"种子源一"), "旧名不该还在: {names:?}");
}

#[tokio::test]
async fn edit_rejects_rename_to_existing_name() {
    let _g = sandbox().await;
    let (status, body) = post(
        "/api/sources/种子源一/edit",
        serde_json::json!({"name": "种子源二"}),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"], "已存在同名源");
}

#[tokio::test]
async fn edit_missing_source_is_404() {
    let _g = sandbox().await;
    let (status, body) = post(
        "/api/sources/压根没有/edit",
        serde_json::json!({"priority": 1}),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["error"], "源不存在");
}

// ---------------------------------------------------------------------------
// delete
// ---------------------------------------------------------------------------

#[tokio::test]
async fn delete_removes_the_block_but_keeps_the_rest() {
    let _g = sandbox().await;
    let name = uniq("待删源");

    // 先加一个再删，避免破坏其它用例依赖的种子源
    post(
        "/api/sources",
        serde_json::json!({"name": name, "adapter": "lzpoke_reports", "target": "https://x"}),
    )
    .await;

    let (status, body) = delete(&format!("/api/sources/{name}")).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["ok"], true);

    let (_, list) = get("/api/sources").await;
    let names: Vec<&str> = list["sources"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|s| s["name"].as_str())
        .collect();
    assert!(!names.contains(&name.as_str()), "删掉的源还在: {names:?}");

    let text = read_seed();
    assert!(text.contains("# 这个源要走代理"), "删一个源不该吃掉别人的注释");
}

#[tokio::test]
async fn delete_missing_source_is_404() {
    let _g = sandbox().await;
    let (status, body) = delete("/api/sources/没有这个源").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["error"], "源不存在");
}

// ---------------------------------------------------------------------------
// upload / download / template
// ---------------------------------------------------------------------------

/// 上传适配器被**明确拒绝**，且错误信息解释原因与替代路径。
///
/// 这是本版相对原版的**有意收紧**：原版会把上传的 `.py` 写进
/// `src/sources/` 并由主流程 `importlib` 执行，等于一个「上传即执行」的入口。
#[tokio::test]
async fn upload_adapter_is_refused_with_an_explanation() {
    let _g = sandbox().await;

    let boundary = "----test-boundary";
    let payload = format!(
        "--{boundary}\r\n\
         Content-Disposition: form-data; name=\"file\"; filename=\"evil.py\"\r\n\
         Content-Type: text/x-python\r\n\r\n\
         def fetch():\n    import os\n    os.system('id')\n\
         \r\n--{boundary}--\r\n"
    );

    let req = Request::builder()
        .method("POST")
        .uri("/api/sources/upload")
        .header(
            "content-type",
            format!("multipart/form-data; boundary={boundary}"),
        )
        .header("cookie", cookie().await)
        .header(CSRF_HEADER, CSRF_HEADER_VALUE)
        .body(Body::from(payload))
        .unwrap();

    let resp = root().app.clone().oneshot(req).await.unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let body: Value = serde_json::from_slice(&bytes).unwrap();

    assert_eq!(status, StatusCode::BAD_REQUEST);
    let err = body["error"].as_str().unwrap();
    assert!(err.contains("不支持上传适配器"), "错误信息要说清是什么问题: {err}");
    assert!(
        err.contains("KNOWN_ADAPTERS"),
        "要告诉用户替代路径: {err}"
    );

    // 关键：文件**没有**被写下去
    let base = PathBuf::from(std::env::var("ALPHA_ROOT").unwrap());
    assert!(
        !base.join("src/sources/evil.py").exists(),
        "上传的文件不该落盘"
    );
}

#[tokio::test]
async fn download_returns_the_config_block() {
    let _g = sandbox().await;
    let resp = get_raw("/api/sources/种子源一/download").await;

    assert_eq!(resp.status(), StatusCode::OK);
    let cd = resp
        .headers()
        .get("content-disposition")
        .expect("要有 Content-Disposition")
        .to_str()
        .unwrap()
        .to_string();
    assert!(cd.starts_with("attachment;"), "应是附件下载: {cd}");

    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let text = String::from_utf8_lossy(&bytes);
    assert!(text.contains("种子源一"), "应含该源的配置块: {text}");
    assert!(text.contains("lzpoke_reports"));
    assert!(text.contains("enabled: true"));
}

#[tokio::test]
async fn download_missing_source_is_404() {
    let _g = sandbox().await;
    let (status, body) = get("/api/sources/没有/download").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["error"], "源不存在");
}

#[tokio::test]
async fn template_is_returned_as_an_attachment() {
    let _g = sandbox().await;
    let resp = get_raw("/api/sources/template").await;

    assert_eq!(resp.status(), StatusCode::OK);
    let cd = resp.headers().get("content-disposition").unwrap().to_str().unwrap();
    assert!(cd.contains("source_template.rs"), "文件名该是 rust 版模板: {cd}");

    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let text = String::from_utf8_lossy(&bytes);
    assert!(text.contains("KNOWN_ADAPTERS"), "模板要说明登记方式");
}

/// `/api/sources/upload` 与 `/api/sources/template` 不能被
/// `/api/sources/<name>` 这条动态路由抢走。
#[tokio::test]
async fn static_source_paths_are_not_shadowed_by_the_name_route() {
    let _g = sandbox().await;

    // template 走的是 GET /api/sources/<name>/download 之外的那条，
    // 若被抢占会变成「源不存在」404 而不是附件
    let resp = get_raw("/api/sources/template").await;
    assert_eq!(resp.status(), StatusCode::OK);
    assert!(
        resp.headers().contains_key("content-disposition"),
        "template 必须是附件响应，说明没被 <name> 抢走"
    );
}
