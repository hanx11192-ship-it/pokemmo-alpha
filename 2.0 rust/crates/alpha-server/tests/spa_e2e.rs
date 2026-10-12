//! 前端资源托管的端到端测试。
//!
//! # 这批测试防的是什么
//!
//! 前端**没重写**（用户明确要求保持原样）。这意味着前端的行为约束
//! 全部由「Rust 后端愿不愿意配合」决定，而这类不匹配**跑单元测试
//! 是发现不了的**：
//!
//! - 静态资源挂在受保护那一侧 → 未登录时登录页永远渲染不出来（死锁）
//! - 入口页引用的 `/static/xxx` 在磁盘上不存在 → 白屏
//! - 垫片没生效 → 所有写操作 400
//!
//! 所以这里不测「中间件的纯逻辑」（那在 `middleware_e2e` 里），
//! 而是**起真的路由器、发真的请求、断言真的字节**。

use std::sync::OnceLock;

use alpha_server::auth::layer::{CSRF_HEADER, CSRF_HEADER_VALUE};
use alpha_server::{build_router, AppState, Config};
use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use http_body_util::BodyExt;
use tower::ServiceExt;

struct Fixture {
    _dir: tempfile::TempDir,
    app: Router,
}

fn fixture() -> &'static Fixture {
    static F: OnceLock<Fixture> = OnceLock::new();
    F.get_or_init(|| {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path().to_path_buf();
        std::fs::create_dir_all(base.join("config")).unwrap();
        std::fs::write(base.join("config/sources.yaml"), "sources: []\n").unwrap();

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

        Fixture {
            _dir: dir,
            app: build_router(AppState::new(cfg, store)),
        }
    })
}

/// 把仓库自带的 `web/` 目录指给 `spa` 模块。
///
/// 必须在**任何**读取前端资源的调用之前执行。用 `set_var` 是因为
/// `alpha-server` 只提供「从环境变量读」这一条路径 —— 它要保证
/// 生产行为跟测试一致，不能有测试专用入口。
fn point_at_repo_web() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let repo = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .to_path_buf();
        std::env::set_var("ALPHA_WEB_ROOT", repo.join("web"));
    });
}

async fn raw(req: Request<Body>) -> (StatusCode, axum::http::HeaderMap, Vec<u8>) {
    point_at_repo_web();
    let resp = fixture().app.clone().oneshot(req).await.unwrap();
    let status = resp.status();
    let headers = resp.headers().clone();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes().to_vec();
    (status, headers, bytes)
}

async fn get_raw(path: &str) -> (StatusCode, axum::http::HeaderMap, Vec<u8>) {
    raw(Request::builder()
        .uri(path)
        .body(Body::empty())
        .unwrap())
    .await
}

async fn get_text(path: &str) -> (StatusCode, String) {
    let (status, _, bytes) = get_raw(path).await;
    (status, String::from_utf8_lossy(&bytes).to_string())
}

fn content_type(h: &axum::http::HeaderMap) -> String {
    h.get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_string()
}

// ===========================================================================
// 入口页
// ===========================================================================

/// 未登录访问 `/` 必须拿到**真的**入口页，而不是占位 HTML。
///
/// 这是「后端换了、前端没换」的第一道接缝：入口页内容来自
/// `web/index.html`，如果 `spa` 模块没读对，用户看到的是空白页。
#[tokio::test]
async fn the_entry_page_is_the_real_frontend_shell() {
    let (status, html) = get_text("/").await;
    assert_eq!(status, StatusCode::OK);

    assert!(
        html.contains("csrf-shim.js"),
        "入口页没有引用 CSRF 垫片 —— 部署后所有写操作都会 400"
    );
    assert!(html.contains("/static/app.js"), "入口页没有引用主脚本");
    assert!(html.contains("/static/i18n.js"), "入口页没有引用 i18n");
    assert!(
        html.contains("/static/style.css"),
        "入口页没有引用样式表 —— 会渲染成一坨纯文本"
    );
    assert!(html.contains(r#"id="app""#), "缺少 SPA 挂载点");
    assert!(html.contains(r#"id="toast-wrap""#), "缺少 toast 容器");

    // 不能还是那个占位页
    assert!(
        !html.contains("正在加载…"),
        "入口页还是占位 HTML，真实前端没接上"
    );
}

/// 入口页与 `/api/me` 一样属于放行名单：未登录也要能打开。
#[tokio::test]
async fn the_entry_page_is_reachable_without_a_session() {
    let (status, _, _) = get_raw("/").await;
    assert_eq!(status, StatusCode::OK, "未登录打开面板应该能看到登录页");
}

/// 已登录访问 `/` 拿到的是**同一份**入口页。
///
/// 原版是多页结构（服务端渲染），改 SPA 之后「登录与否」由前端自己
/// 调 `/api/me` 判断。如果这里给已登录用户返回一份不同的东西，
/// 前端那套判断就白写了。
#[tokio::test]
async fn the_entry_page_is_identical_for_anonymous_and_logged_in() {
    let (anon_status, _, anon) = get_raw("/").await;

    // 伪造一个有效会话（这个套件不关心登录流程，只关心「登录后」的分支）
    use alpha_server::auth::session::{Session, SessionSigner};
    let signer = SessionSigner::new("test-secret-value-at-least-32-bytes-long", 3600);
    let now = chrono::Utc::now().timestamp();
    let cookie = format!(
        "session={}",
        signer.sign(&Session::new("hanyx", "admin", now, 3600))
    );

    let (auth_status, _, authed) = raw(
        Request::builder()
            .uri("/")
            .header("cookie", cookie)
            .body(Body::empty())
            .unwrap(),
    )
    .await;

    assert_eq!(anon_status, StatusCode::OK);
    assert_eq!(auth_status, StatusCode::OK);
    assert_eq!(anon, authed, "登录前后拿到的入口页必须完全一致");
}

// ===========================================================================
// 静态资源
// ===========================================================================

/// **登录页要用到的资源，未登录必须能取到。**
///
/// 这条是「死锁式」故障的回归测试：如果静态资源被鉴权拦住，
/// 未登录用户打不开 `style.css` / `app.js`，登录页永远渲染不出来 ——
/// 而「要登录才能拿到登录所需的东西」是个自己把自己锁死的循环。
#[tokio::test]
async fn static_assets_are_reachable_without_a_session() {
    for (path, expect_ct) in [
        ("/static/style.css", "text/css"),
        ("/static/app.js", "javascript"),
        ("/static/i18n.js", "javascript"),
        ("/static/csrf-shim.js", "javascript"),
        ("/static/logo.png", "image/png"),
    ] {
        let (status, headers, bytes) = get_raw(path).await;
        assert_eq!(
            status,
            StatusCode::OK,
            "{path} 未登录取不到 —— 登录页会渲染不出来（死锁）"
        );
        assert!(!bytes.is_empty(), "{path} 返回了空文件");
        let ct = content_type(&headers);
        assert!(
            ct.contains(expect_ct),
            "{path} 的 Content-Type 应为 {expect_ct}，实际 {ct}"
        );
    }
}

/// 静态资源的内容必须**真的**是原版前端，不是随便一个 200。
///
/// 只断言状态码的话，「把一个空目录挂上去」也能通过。
#[tokio::test]
async fn the_static_assets_are_the_original_frontend() {
    let (_, app_js) = get_text("/static/app.js").await;
    // 原版 app.js 是立即执行的 IIFE，带这几样特征
    assert!(app_js.contains("use strict"), "app.js 不像原版前端");
    assert!(
        app_js.contains("function boot"),
        "app.js 里没有 boot 函数 —— 可能不是原版那份"
    );

    let (_, css) = get_text("/static/style.css").await;
    assert!(css.contains("toast"), "style.css 里没有 toast 样式");
}

/// 垫片必须真的把 CSRF 头补上 —— 补的是 fetch 和 XHR **两条**路。
///
/// 上传走 XHR（为了拿进度），只包 fetch 的话上传会在部署后静默坏掉。
#[tokio::test]
async fn the_shim_covers_both_fetch_and_xhr() {
    let (status, shim) = get_text("/static/csrf-shim.js").await;
    assert_eq!(status, StatusCode::OK);

    assert!(
        shim.contains("window.fetch"),
        "垫片没有包 fetch"
    );
    assert!(
        shim.contains("XMLHttpRequest.prototype.send"),
        "垫片没有包 XHR —— 上传功能会在部署后坏掉"
    );
    assert!(
        shim.contains("pokemmo-alpha-panel"),
        "垫片里的 CSRF 头值与后端不一致"
    );

    // 头名必须与后端常量逐字一致，两边对不上就是全站 400
    assert!(
        shim.contains(CSRF_HEADER),
        "垫片用的头名 `{}` 与后端不一致",
        CSRF_HEADER
    );
    assert!(
        shim.contains(CSRF_HEADER_VALUE),
        "垫片用的头值 `{}` 与后端不一致",
        CSRF_HEADER_VALUE
    );
}

/// 不存在的静态资源该是 404，不能掉进 SPA 回退里返回 HTML。
///
/// 返回 HTML 的话浏览器会把 `text/html` 当 JS 解析，报一堆语法错误，
/// 却看不出真正的问题是「文件不存在」。
#[tokio::test]
async fn a_missing_static_file_is_404_not_the_entry_page() {
    let (status, headers, _) = get_raw("/static/does-not-exist.js").await;
    // ServeDir 对不存在的文件回 404；无论如何不能是 200 的 HTML
    assert_ne!(status, StatusCode::OK, "不存在的资源不该返回 200");
    let ct = content_type(&headers);
    assert!(
        !ct.starts_with("text/html") || status == StatusCode::NOT_FOUND,
        "不存在的静态资源不该被 SPA 回退成 HTML: {status} {ct}"
    );
}

/// 目录穿越必须被挡住。
///
/// `ServeDir` 自带防护，但「自带」这件事需要被验证 —— 前端目录旁边
/// 就是 `panel.db` 和 `panel.env`（里面有会话密钥）。
#[tokio::test]
async fn path_traversal_out_of_the_static_dir_is_blocked() {
    for path in [
        "/static/../index.html",
        "/static/../../panel.env",
        "/static/..%2f..%2fpanel.env",
        "/static/%2e%2e/%2e%2e/config/settings.yaml",
    ] {
        let (status, _, bytes) = get_raw(path).await;
        let body = String::from_utf8_lossy(&bytes);
        assert_ne!(status, StatusCode::OK, "{path} 竟然成功了");
        assert!(
            !body.contains("PANEL_SECRET"),
            "{path} 读到了密钥文件的内容"
        );
    }
}

// ===========================================================================
// API 与前端的分工
// ===========================================================================

/// `/api/*` 未登录仍然是 401 JSON —— 静态资源放行**不能**顺带把 API 也放行了。
///
/// `.nest_service` 挂在 `public` 那一侧，容易一不小心放宽了判断。
#[tokio::test]
async fn serving_static_files_does_not_loosen_api_auth() {
    for path in ["/api/channels", "/api/dispatchers", "/api/dashboard"] {
        let (status, headers, _) = get_raw(path).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{path} 未登录没被拦");
        let ct = content_type(&headers);
        assert!(ct.contains("json"), "{path} 该返回 JSON 错误: {ct}");
    }
}

/// 写接口缺 CSRF 头仍然是 400 —— 垫片是**客户端**的补救，
/// 服务端的校验一点没放松。
#[tokio::test]
async fn the_shim_does_not_weaken_server_side_csrf() {
    let (status, _, _) = raw(
        Request::builder()
            .method("POST")
            .uri("/api/login")
            .header("content-type", "application/json")
            .body(Body::from(r#"{"username":"x","password":"y"}"#))
            .unwrap(),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "没有 CSRF 头的写请求仍必须被拒"
    );
}
