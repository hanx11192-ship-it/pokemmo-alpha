//! 鉴权中间件的**端到端**测试。
//!
//! 单元测试证明了 `is_public` / `extract_session_token` 这些纯函数是对的，
//! 但证明不了「中间件真的接在了路由器上」—— 中间件写对了却没挂上去，
//! 单测照样全绿。这里起一个真的 axum `Router`，发真的请求，
//! 断言真的状态码。
//!
//! 用的 `tower::ServiceExt::oneshot`，不需要监听端口。

use alpha_server::auth::layer::{
    auth_middleware, AuthContext, CSRF_HEADER, CSRF_HEADER_VALUE,
};
use alpha_server::auth::session::{Session, SessionSigner};
use axum::body::Body;
use axum::extract::Extension;
use axum::http::{Request, StatusCode};
use axum::middleware;
use axum::routing::{get as route_get, post as route_post};
use axum::{Json, Router};
use tower::ServiceExt;

const KEY: &str = "test-key-for-the-middleware-suite!!";

fn app() -> Router {
    let signer = SessionSigner::new(KEY, 3600);

    // 复刻一批真实路由：公开的 + 受保护的，读的 + 写的
    let protected_reads = Router::new()
        .route("/api/channels", route_get(|| async { Json(serde_json::json!({"ok": true})) }))
        .route("/api/plugins", route_get(|| async { Json(serde_json::json!({"ok": true})) }));

    let protected_writes = Router::new()
        .route("/api/channels", route_post(|| async { Json(serde_json::json!({"ok": true})) }))
        .route("/api/plugins", route_post(|| async { Json(serde_json::json!({"ok": true})) }));

    let public = Router::new()
        .route("/", route_get(|| async { "spa" }))
        .route(
            "/api/me",
            route_get(|Extension(ctx): Extension<AuthContext>| async move {
                // 原版未登录也返回 200，这里必须一致
                Json(serde_json::json!({
                    "authenticated": ctx.is_authenticated(),
                    "user": ctx.username(),
                    "role": ctx.session.as_ref().map(|s| s.r.clone()),
                }))
            }),
        )
        .route("/api/login", route_post(|| async { Json(serde_json::json!({"ok": true})) }))
        .route("/api/logout", route_post(|| async { Json(serde_json::json!({"ok": true})) }))
        .route("/api/lang", route_post(|| async { Json(serde_json::json!({"ok": true})) }));

    public
        .merge(protected_reads)
        .merge(protected_writes)
        .layer(middleware::from_fn_with_state(signer, auth_middleware))
}

/// 发一个请求并返回状态码。
async fn status_of(req: Request<Body>) -> StatusCode {
    app().oneshot(req).await.unwrap().status()
}

/// 发一个请求并返回响应体（JSON）。
async fn body_of(req: Request<Body>) -> serde_json::Value {
    let resp = app().oneshot(req).await.unwrap();
    let bytes = http_body_util::BodyExt::collect(resp.into_body())
        .await
        .unwrap()
        .to_bytes();
    serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null)
}

fn get(path: &str) -> Request<Body> {
    Request::builder().uri(path).body(Body::empty()).unwrap()
}

/// 构造一个带 CSRF 头的写请求。
fn post_with_csrf(path: &str, cookie: Option<&str>) -> Request<Body> {
    let mut b = Request::builder()
        .method("POST")
        .uri(path)
        .header(CSRF_HEADER, CSRF_HEADER_VALUE);
    if let Some(c) = cookie {
        b = b.header("cookie", c);
    }
    b.body(Body::empty()).unwrap()
}

fn valid_cookie() -> String {
    let signer = SessionSigner::new(KEY, 3600);
    let now = chrono::Utc::now().timestamp();
    format!("session={}", signer.sign(&Session::new("hanyx", "admin", now, 3600)))
}

// ---------------------------------------------------------------------------
// 公开路由
// ---------------------------------------------------------------------------

#[tokio::test]
async fn public_routes_are_reachable_without_a_session() {
    assert_eq!(status_of(get("/")).await, StatusCode::OK);
    assert_eq!(status_of(get("/api/me")).await, StatusCode::OK);
    assert_eq!(
        status_of(post_with_csrf("/api/login", None)).await,
        StatusCode::OK
    );
}

/// `/api/me` 未登录也要返回 **200**，且 `authenticated: false`。
///
/// 这条是原版明确的行为：前端一加载就调它来判断该渲染登录页还是面板。
/// 如果这里改成 401，前端得把「未登录」当成错误处理，多一条分支。
#[tokio::test]
async fn api_me_returns_200_when_unauthenticated() {
    let body = body_of(get("/api/me")).await;
    assert_eq!(body["authenticated"], false);
    assert_eq!(body["user"], serde_json::Value::Null);
    assert_eq!(body["role"], serde_json::Value::Null);
}

#[tokio::test]
async fn api_me_reflects_a_valid_session() {
    let req = Request::builder()
        .uri("/api/me")
        .header("cookie", valid_cookie())
        .body(Body::empty())
        .unwrap();
    let body = body_of(req).await;
    assert_eq!(body["authenticated"], true);
    assert_eq!(body["user"], "hanyx");
    assert_eq!(body["role"], "admin");
}

// ---------------------------------------------------------------------------
// 受保护路由
// ---------------------------------------------------------------------------

/// **核心断言：未登录访问受保护接口必须 401。**
#[tokio::test]
async fn protected_routes_reject_anonymous_requests() {
    for path in ["/api/channels", "/api/plugins"] {
        assert_eq!(
            status_of(get(path)).await,
            StatusCode::UNAUTHORIZED,
            "{path} 未登录竟然放行了"
        );
    }
}

/// 401 的响应体要和原版逐字一致 —— 前端就是按这个文案判断的。
#[tokio::test]
async fn unauthorized_response_matches_the_original_shape() {
    let body = body_of(get("/api/channels")).await;
    assert_eq!(body["ok"], false);
    assert_eq!(body["error"], "未登录或会话已过期");
}

#[tokio::test]
async fn protected_routes_accept_a_valid_session() {
    let req = Request::builder()
        .uri("/api/channels")
        .header("cookie", valid_cookie())
        .body(Body::empty())
        .unwrap();
    assert_eq!(
        app().oneshot(req).await.unwrap().status(),
        StatusCode::OK
    );
}

/// **回归：伪造的会话必须被拒。**
///
/// 这就是现网那个漏洞在中间件层的体现。原版用公开默认密钥签名，
/// 任何人都能造出合法 cookie；本版密钥是随机的，伪造者签不出来。
#[tokio::test]
async fn forged_sessions_are_rejected_at_the_middleware() {
    let attacker = SessionSigner::new("alpha-panel-dev-secret", 3600);
    let now = chrono::Utc::now().timestamp();
    let forged = attacker.sign(&Session::new("hanyx", "admin", now, 3600));

    let req = Request::builder()
        .uri("/api/channels")
        .header("cookie", format!("session={forged}"))
        .body(Body::empty())
        .unwrap();

    assert_eq!(
        app().oneshot(req).await.unwrap().status(),
        StatusCode::UNAUTHORIZED,
        "用公开默认密钥伪造的会话竟然通过了"
    );
}

/// 过期会话同样 401 —— 原版 cookie 永不过期，本版会。
#[tokio::test]
async fn expired_sessions_are_rejected() {
    let signer = SessionSigner::new(KEY, 3600);
    let long_ago = chrono::Utc::now().timestamp() - 100_000;
    let stale = signer.sign(&Session::new("hanyx", "admin", long_ago, 3600));

    let req = Request::builder()
        .uri("/api/channels")
        .header("cookie", format!("session={stale}"))
        .body(Body::empty())
        .unwrap();

    assert_eq!(
        app().oneshot(req).await.unwrap().status(),
        StatusCode::UNAUTHORIZED
    );
}

/// 重复 cookie 不再一律拒绝：逐个验签，**有效会话即使排在第二位也被采用**。
/// （旧实现直接 401，这正是上线当天「下载 401」的真实成因。）
#[tokio::test]
async fn duplicate_session_cookies_the_valid_one_is_adopted() {
    let req = Request::builder()
        .uri("/api/channels")
        .header("cookie", format!("session=x; session={}", &valid_cookie()[8..]))
        .body(Body::empty())
        .unwrap();

    assert_eq!(
        app().oneshot(req).await.unwrap().status(),
        StatusCode::OK,
        "有效会话应被采用，不因重复被拒"
    );
}

// ---------------------------------------------------------------------------
// CSRF
// ---------------------------------------------------------------------------

/// **写请求缺 CSRF 头必须被拒**，即使带了合法会话。
///
/// 这条防的是：第三方页面诱导已登录管理员的浏览器发请求。
/// 浏览器会自动带上 cookie，但不会自动带这个自定义头。
#[tokio::test]
async fn state_changing_requests_require_the_csrf_header() {
    let cookie = valid_cookie();

    // 有会话、无 CSRF 头 → 拒
    let req = Request::builder()
        .method("POST")
        .uri("/api/channels")
        .header("cookie", &cookie)
        .body(Body::empty())
        .unwrap();
    assert_eq!(
        app().oneshot(req).await.unwrap().status(),
        StatusCode::BAD_REQUEST,
        "缺 CSRF 头的写请求必须被拒"
    );

    // 有会话、有正确 CSRF 头 → 放行
    assert_eq!(
        status_of(post_with_csrf("/api/channels", Some(&cookie))).await,
        StatusCode::OK
    );
}

/// CSRF 头值必须是预期的那一个，随便填不算。
#[tokio::test]
async fn csrf_header_value_must_match_exactly() {
    let cookie = valid_cookie();
    for bad in ["", "1", "XMLHttpRequest", "pokemmo-alpha-pane"] {
        let req = Request::builder()
            .method("POST")
            .uri("/api/channels")
            .header("cookie", &cookie)
            .header(CSRF_HEADER, bad)
            .body(Body::empty())
            .unwrap();
        assert_eq!(
            app().oneshot(req).await.unwrap().status(),
            StatusCode::BAD_REQUEST,
            "CSRF 头值 {bad:?} 不该被接受"
        );
    }
}

/// 读请求不需要 CSRF 头。
#[tokio::test]
async fn read_requests_do_not_need_the_csrf_header() {
    let req = Request::builder()
        .uri("/api/channels")
        .header("cookie", valid_cookie())
        .body(Body::empty())
        .unwrap();
    assert_eq!(
        app().oneshot(req).await.unwrap().status(),
        StatusCode::OK
    );
}

/// 公开路由不受 CSRF 头要求影响 —— 否则登录本身都做不了。
#[tokio::test]
async fn public_post_routes_bypass_csrf() {
    assert_eq!(
        status_of(post_with_csrf("/api/login", None)).await,
        StatusCode::OK
    );
}

// ---------------------------------------------------------------------------
// 非 API 路径
// ---------------------------------------------------------------------------

/// 非 API 路径未登录时返回入口页（200），而不是原版那个 500。
#[tokio::test]
async fn non_api_paths_serve_the_spa_entry_instead_of_500() {
    let resp = app().oneshot(get("/some/page")).await.unwrap();
    // 原版会 500（url_for("login_page") 指向不存在的端点）
    assert_eq!(resp.status(), StatusCode::OK);
    let ct = resp
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_string();
    assert!(ct.starts_with("text/html"), "应返回 HTML 入口页: {ct}");
}

// ---------------------------------------------------------------------------
// 同名会话 cookie 重复
// ---------------------------------------------------------------------------

/// 浏览器里的同名 `session` 残留是真实常态：cookie 按（名字, 路径, 域）
/// 匹配，**不区分端口**，原版 Flask 面板（5703）种下的 cookie 会和
/// Rust 版（5704）种的共存于同一条请求 —— 上线当天真实发生的下载 401。
/// 只要其中一个是有效会话，就必须放行；伪造的过不了 HMAC，tossing 依然无效。
#[tokio::test]
async fn duplicate_cookies_with_one_valid_session_are_accepted() {
    // 有效会话在前、垃圾在后
    let req = Request::builder()
        .uri("/api/me")
        .header("cookie", format!("{}; session=garbage.tossed", valid_cookie()))
        .body(Body::empty())
        .unwrap();
    let body = body_of(req).await;
    assert_eq!(body["authenticated"], true, "有效会话在前时应放行");
    assert_eq!(body["user"], "hanyx");

    // 垃圾在前、有效会话在后（顺序无关）
    let req = Request::builder()
        .uri("/api/me")
        .header("cookie", format!("session=junk; {}", valid_cookie()))
        .body(Body::empty())
        .unwrap();
    let body = body_of(req).await;
    assert_eq!(body["authenticated"], true, "有效会话在后时也应放行");
    assert_eq!(body["user"], "hanyx");
}

/// 全部候选都无效时仍然 401 —— 放宽不能放成不设防。
#[tokio::test]
async fn duplicate_cookies_with_no_valid_session_are_rejected() {
    let req = Request::builder()
        .uri("/api/channels")
        .header("cookie", "session=aaa.bbb; session=ccc.ddd")
        .body(Body::empty())
        .unwrap();
    assert_eq!(app().oneshot(req).await.unwrap().status(), StatusCode::UNAUTHORIZED);
}
