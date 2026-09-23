//! 四个鉴权接口的端到端测试。
//!
//! 这里跑的是**完整链路**：真实 router → 真实中间件 → 真实路由 handler
//! → 真实 SQLite（临时文件）。不发假数据、不打桩。
//!
//! 之所以要这样测：这几个接口的行为有很多微妙之处（首登即管理员、
//! 失败文案不区分、语言偏好存库而不是会话），单测只能验序列化形状，
//! 验不了「这些行为串起来是不是对的」。

use alpha_server::auth::layer::{CSRF_HEADER, CSRF_HEADER_VALUE};
use alpha_server::{build_router, AppState, Config};
use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use http_body_util::BodyExt;
use serde_json::Value;
use tower::ServiceExt;

/// 每个测试用独立的临时目录 + 独立数据库。
struct Fixture {
    _dir: tempfile::TempDir,
    app: Router,
    state: AppState,
}

impl Fixture {
    fn new() -> Self {
        Self::with_secret("test-secret-value-at-least-32-bytes-long")
    }

    fn with_secret(secret: &str) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let cfg = Config {
            host: "127.0.0.1".into(),
            port: 0,
            secret: secret.into(),
            data_dir: dir.path().to_path_buf(),
            config_dir: dir.path().to_path_buf(),
            session_ttl: 3600,
            envs: std::collections::HashMap::new(),
        };
        let store = alpha_store::Store::open(cfg.db_path()).unwrap();
        let state = AppState::new(cfg, store);
        let app = build_router(state.clone());
        Self {
            _dir: dir,
            app,
            state,
        }
    }

    /// 发一个 GET。
    async fn get(&self, path: &str, cookie: Option<&str>) -> (StatusCode, Value) {
        let mut b = Request::builder().uri(path);
        if let Some(c) = cookie {
            b = b.header("cookie", c);
        }
        send(self.app.clone(), b.body(Body::empty()).unwrap()).await
    }

    /// 发一个带 CSRF 头的 POST。
    async fn post(
        &self,
        path: &str,
        body: Value,
        cookie: Option<&str>,
    ) -> (StatusCode, Value) {
        self.post_raw(path, Some(body), cookie, Some(CSRF_HEADER_VALUE))
            .await
    }

    async fn post_raw(
        &self,
        path: &str,
        body: Option<Value>,
        cookie: Option<&str>,
        csrf: Option<&str>,
    ) -> (StatusCode, Value) {
        let mut b = Request::builder().method("POST").uri(path);
        if let Some(c) = cookie {
            b = b.header("cookie", c);
        }
        if let Some(c) = csrf {
            b = b.header(CSRF_HEADER, c);
        }
        let payload = match body {
            Some(v) => {
                b = b.header("content-type", "application/json");
                serde_json::to_vec(&v).unwrap()
            }
            None => Vec::new(),
        };
        send(self.app.clone(), b.body(Body::from(payload)).unwrap()).await
    }

    /// 登录成功时从 `Set-Cookie` 里取出会话 cookie。
    async fn login_and_get_cookie(&self, user: &str, pass: &str) -> String {
        let req = Request::builder()
            .method("POST")
            .uri("/api/login")
            .header("content-type", "application/json")
            .header(CSRF_HEADER, CSRF_HEADER_VALUE)
            .body(Body::from(
                serde_json::to_vec(&serde_json::json!({"username": user, "password": pass}))
                    .unwrap(),
            ))
            .unwrap();

        let resp = self.app.clone().oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK, "登录应该成功");

        let raw = resp
            .headers()
            .get("set-cookie")
            .expect("登录成功必须下发 cookie")
            .to_str()
            .unwrap()
            .to_string();
        // 取 "session=..." 那一段
        raw.split(';').next().unwrap().to_string()
    }
}

async fn send(app: Router, req: Request<Body>) -> (StatusCode, Value) {
    let resp = app.oneshot(req).await.unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, value)
}

// ---------------------------------------------------------------------------
// /api/me
// ---------------------------------------------------------------------------

#[tokio::test]
async fn me_is_200_and_anonymous_before_login() {
    let f = Fixture::new();
    let (status, body) = f.get("/api/me", None).await;

    assert_eq!(status, StatusCode::OK, "未登录也必须是 200");
    assert_eq!(body["authenticated"], false);
    assert_eq!(body["user"], Value::Null);
    assert_eq!(body["role"], Value::Null);
    assert_eq!(body["lang"], "zh");
}

// ---------------------------------------------------------------------------
// /api/login
// ---------------------------------------------------------------------------

/// **首登即管理员。**
///
/// `users` 表为空时，第一个登录者自动成为管理员 —— 原版行为。
/// 这是全新部署时创建第一个账号的唯一途径，所以必须保留。
#[tokio::test]
async fn first_login_creates_an_admin() {
    let f = Fixture::new();
    assert_eq!(f.state.store.count_users().unwrap(), 0);

    let (status, body) = f.post("/api/login", serde_json::json!({
        "username": "hanyx", "password": "s3cret!"
    }), None).await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["ok"], true);
    assert_eq!(body["username"], "hanyx");
    assert_eq!(body["role"], "admin");
    assert_eq!(body["is_first_admin"], true, "首登要显式标记");

    // 库里真的建了用户，而且哈希不是明文
    let u = f.state.store.get_user("hanyx").unwrap().unwrap();
    assert!(u.is_admin);
    assert_ne!(u.password_hash, "s3cret!");
    assert!(u.password_hash.starts_with("$argon2"), "新哈希应是 argon2");
}

/// 第二个用户不会因为是「新用户」就变管理员。
///
/// 具体行为：`users` 表非空时，不存在的用户名直接登录失败
/// （不是「自动注册」）。这条常被误读成「谁先登录谁是管理员」的延伸，
/// 实际原版是「只有表为空时创建」，之后没人能自己注册。
#[tokio::test]
async fn only_the_very_first_user_becomes_admin() {
    let f = Fixture::new();

    // 第一个用户 → 管理员
    let (_, body) = f.post("/api/login", serde_json::json!({
        "username": "hanyx", "password": "pw1"
    }), None).await;
    assert_eq!(body["role"], "admin");

    // 第二个不存在的用户名 → 拒绝，不会被创建
    let (status, body) = f.post("/api/login", serde_json::json!({
        "username": "intruder", "password": "pw2"
    }), None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(body["error"], "用户名不存在或密码错误");
    assert!(
        f.state.store.get_user("intruder").unwrap().is_none(),
        "表非空时不该自动注册新用户"
    );
    assert_eq!(f.state.store.count_users().unwrap(), 1);
}

/// 登录失败必须用**同一句话**，不能区分「用户不存在」和「密码错」。
///
/// 分开提示等于给了攻击者一个用户名枚举接口 ——
/// 能逐个试出哪些用户名有效，为后续撞库缩小范围。
#[tokio::test]
async fn login_failure_does_not_reveal_which_part_was_wrong() {
    let f = Fixture::new();
    // 先建一个用户
    f.post("/api/login", serde_json::json!({"username": "hanyx", "password": "right"}), None).await;

    // a) 用户存在但密码错
    let (s1, b1) = f.post("/api/login", serde_json::json!({
        "username": "hanyx", "password": "wrong"
    }), None).await;

    // b) 用户不存在
    let (s2, b2) = f.post("/api/login", serde_json::json!({
        "username": "nobody", "password": "right"
    }), None).await;

    assert_eq!(s1, StatusCode::UNAUTHORIZED);
    assert_eq!(s2, StatusCode::UNAUTHORIZED);
    assert_eq!(
        b1["error"], b2["error"],
        "两种失败的文案必须完全一致，否则可枚举用户名"
    );
    assert_eq!(b1["error"], "用户名不存在或密码错误");
}

/// 登录失败**不能下发 cookie**。
///
/// 否则一个失败的登录请求也带了副作用，等于多一个「清掉别人会话」的入口。
///
/// 注意：必须先建一个用户，否则这个请求会命中「首登即管理员」而成功。
/// 我第一版就是漏了这一步，测试直接以 200 失败 —— 这其实是个有价值的提醒：
/// **空库上任何一次登录尝试都会成功并创建管理员**，
/// 所以「部署完立刻自己登一次」是必要的收尾动作。
#[tokio::test]
async fn failed_login_sets_no_cookie() {
    let f = Fixture::new();
    // 先把库占上，让首登分支不再适用
    f.login_and_get_cookie("hanyx", "the-real-password").await;

    // 现在一次错误的登录
    let req = Request::builder()
        .method("POST")
        .uri("/api/login")
        .header("content-type", "application/json")
        .header(CSRF_HEADER, CSRF_HEADER_VALUE)
        .body(Body::from(r#"{"username":"hanyx","password":"not-the-password"}"#))
        .unwrap();

    let resp = f.app.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    assert!(
        resp.headers().get("set-cookie").is_none(),
        "失败的登录不该下发 cookie"
    );
}

/// 空库上的**任何**登录尝试都会成功 —— 记录这个风险窗口。
///
/// 这不是 bug，是「首登即管理员」的必然推论：全新部署时没有别的办法
/// 创建第一个账号。但它的安全含义值得单独钉一条测试说清楚：
/// **服务起好到第一次登录之间的窗口期，谁先来谁就是管理员。**
///
/// 所以部署流程必须以「立刻自己登一次并设好密码」收尾，
/// 而不是把服务起起来就去干别的。
#[tokio::test]
async fn on_an_empty_database_the_first_caller_wins() {
    let f = Fixture::new();
    assert_eq!(f.state.store.count_users().unwrap(), 0);

    // 随便什么凭据 —— 只要库是空的
    let (status, body) = f
        .post(
            "/api/login",
            serde_json::json!({"username": "whoever", "password": "whatever"}),
            None,
        )
        .await;

    assert_eq!(status, StatusCode::OK, "空库首登会成功");
    assert_eq!(body["role"], "admin");
    assert_eq!(body["is_first_admin"], true);

    // 这就是风险窗口：任何能访问到端口的人都能先占住管理员
    let u = f.state.store.get_user("whoever").unwrap().unwrap();
    assert!(u.is_admin, "先到者拿到了管理员");
}

#[tokio::test]
async fn empty_credentials_are_rejected() {
    let f = Fixture::new();

    for body in [
        serde_json::json!({"username": "", "password": ""}),
        serde_json::json!({"username": "  ", "password": "x"}),
        serde_json::json!({"username": "x", "password": ""}),
        serde_json::json!({}),
    ] {
        let (status, b) = f.post("/api/login", body.clone(), None).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{body} 应被拒");
        assert_eq!(b["error"], "用户名不存在或密码错误");
    }

    assert_eq!(
        f.state.store.count_users().unwrap(),
        0,
        "空凭据不该触发首登创建用户"
    );
}

/// 用户名前后空格要被裁掉 —— 原版 `username.strip()`。
///
/// 不裁的话，`"hanyx "` 会被当成另一个用户名，用户会困惑
/// 「我明明输对了密码」。
#[tokio::test]
async fn username_is_trimmed() {
    let f = Fixture::new();
    f.post("/api/login", serde_json::json!({"username": "hanyx", "password": "pw"}), None).await;

    let (status, body) = f.post("/api/login", serde_json::json!({
        "username": "  hanyx  ", "password": "pw"
    }), None).await;
    assert_eq!(status, StatusCode::OK, "带空格的用户名应能登录");
    assert_eq!(body["username"], "hanyx");
}

// ---------------------------------------------------------------------------
// 登录 → 访问受保护接口 → 登出 的完整往返
// ---------------------------------------------------------------------------

#[tokio::test]
async fn login_then_access_then_logout_round_trip() {
    let f = Fixture::new();

    // 1. 登录前 /api/me 是匿名的
    let (_, body) = f.get("/api/me", None).await;
    assert_eq!(body["authenticated"], false);

    // 2. 登录
    let cookie = f.login_and_get_cookie("hanyx", "pw").await;

    // 3. 带着 cookie，/api/me 认得出来
    let (status, body) = f.get("/api/me", Some(&cookie)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["authenticated"], true);
    assert_eq!(body["user"], "hanyx");
    assert_eq!(body["role"], "admin");
    assert_eq!(body["lang"], "zh", "默认语言");

    // 4. 登出返回 ok，并且明确让 cookie 过期
    let req = Request::builder()
        .method("POST")
        .uri("/api/logout")
        .header(CSRF_HEADER, CSRF_HEADER_VALUE)
        .header("cookie", &cookie)
        .body(Body::empty())
        .unwrap();
    let resp = f.app.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let set_cookie = resp
        .headers()
        .get("set-cookie")
        .expect("登出要下发清除 cookie 的指令")
        .to_str()
        .unwrap();
    assert!(set_cookie.contains("Max-Age=0"), "必须让浏览器立刻丢弃: {set_cookie}");
    assert!(set_cookie.starts_with("session=;"), "值要清空: {set_cookie}");
}

/// 登出接口本身不需要登录（原版白名单里有它）。
#[tokio::test]
async fn logout_works_without_being_logged_in() {
    let f = Fixture::new();
    let (status, body) = f
        .post("/api/logout", serde_json::json!({}), None)
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["ok"], true);
}

// ---------------------------------------------------------------------------
// /api/lang
// ---------------------------------------------------------------------------

#[tokio::test]
async fn language_preference_round_trips_through_the_database() {
    let f = Fixture::new();
    let cookie = f.login_and_get_cookie("hanyx", "pw").await;

    let (status, body) = f
        .post("/api/lang", serde_json::json!({"lang": "en"}), Some(&cookie))
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["ok"], true);
    assert_eq!(body["lang"], "en");

    // 关键：偏好存在**数据库**里，不是会话 cookie 里 ——
    // 所以换一个 cookie 也读得到同一个语言
    let u = f.state.store.get_user("hanyx").unwrap().unwrap();
    assert_eq!(u.lang, "en", "语言偏好应落库");

    let (_, me) = f.get("/api/me", Some(&cookie)).await;
    assert_eq!(me["lang"], "en", "/api/me 应反映库里的语言");
}

#[tokio::test]
async fn only_zh_and_en_are_accepted() {
    let f = Fixture::new();
    let cookie = f.login_and_get_cookie("hanyx", "pw").await;

    for bad in ["", "fr", "ZH", "EN", "zh-CN", "en-US", "中文"] {
        let (status, body) = f
            .post("/api/lang", serde_json::json!({ "lang": bad }), Some(&cookie))
            .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{bad:?} 应被拒");
        assert_eq!(body["error"], "语言不支持");
    }

    // 缺字段也是 400
    let (status, _) = f
        .post("/api/lang", serde_json::json!({}), Some(&cookie))
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn language_change_requires_a_session() {
    let f = Fixture::new();
    let (status, _) = f
        .post("/api/lang", serde_json::json!({"lang": "en"}), None)
        .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "未登录不能改语言");
}

/// 会话里的用户被删掉之后，改语言不该静默成功。
///
/// 原版 `set_user_lang` 是 `UPDATE ... WHERE username=?`，
/// 用户不存在时影响 0 行但**不报错**，接口返回 `ok: true` ——
/// 前端会显示「已保存」，其实什么都没存。
#[tokio::test]
async fn language_change_fails_when_the_user_no_longer_exists() {
    let f = Fixture::new();
    let cookie = f.login_and_get_cookie("hanyx", "pw").await;

    // 模拟：管理员在别处把这个用户删了，但会话 cookie 还没过期
    f.state.store.delete_user("hanyx").unwrap();

    let (status, _) = f
        .post("/api/lang", serde_json::json!({"lang": "en"}), Some(&cookie))
        .await;
    assert_eq!(
        status,
        StatusCode::UNAUTHORIZED,
        "用户已不存在时不该返回 ok:true 骗前端"
    );
}

// ---------------------------------------------------------------------------
// CSRF 覆盖这四个接口
// ---------------------------------------------------------------------------

/// 即使登录成功了，写请求没有 CSRF 头也要被拒。
#[tokio::test]
async fn csrf_protects_the_auth_write_endpoints() {
    let f = Fixture::new();

    // 登录：没有 CSRF 头 → 拒（登录本身也是写请求）
    let (status, _) = f
        .post_raw(
            "/api/login",
            Some(serde_json::json!({"username": "hanyx", "password": "pw"})),
            None,
            None,
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "登录也要带 CSRF 头");

    // 带 CSRF 头就能登录
    let cookie = f.login_and_get_cookie("hanyx", "pw").await;

    // 改语言没有 CSRF 头 → 拒
    let (status, _) = f
        .post_raw(
            "/api/lang",
            Some(serde_json::json!({"lang": "en"})),
            Some(&cookie),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "改语言要带 CSRF 头");

    // 确认那次被拒的请求真的没生效
    assert_eq!(
        f.state.store.get_user("hanyx").unwrap().unwrap().lang,
        "zh",
        "被 CSRF 拦下的请求不该改到数据"
    );
}

/// GET 不需要 CSRF 头（否则 /api/me 都没法调）。
#[tokio::test]
async fn read_endpoints_need_no_csrf_header() {
    let f = Fixture::new();
    let (status, _) = f.get("/api/me", None).await;
    assert_eq!(status, StatusCode::OK);
}
