//! 鉴权中间件。
//!
//! 把原版 `auth.login_required` 装饰器（在 `app.py` 里出现 **51 次**）
//! 收敛成一层中间件。原版那种写法的风险是：**漏加一次就是一个裸奔的接口**，
//! 而且没有任何机制能发现漏了 —— 只能靠人工比对。改成中间件之后，
//! 默认全部受保护，要放行必须显式登记到白名单里。
//!
//! # 放行名单（与原版一致）
//!
//! | 路由 | 原因 |
//! |---|---|
//! | `GET /` | 前端入口，未登录时要能返回登录页 |
//! | `GET /api/me` | 前端靠它判断是否登录；**未登录也返回 200** |
//! | `POST /api/login` | 登录本身 |
//! | `POST /api/logout` | 未登录时调用应无害 |
//! | `POST /api/lang` | 原版就放行了；它只写 `users.lang`，未登录时是空操作 |
//!
//! # 「按 API 对待」的判定
//!
//! 不只是 `/api/*` 前缀 —— 还有一条 **`POST /debug/run`**。
//!
//! 它是原版唯一一个挂在根上（不带 `/api` 前缀）的接口，但语义上完全
//! 是个 API：要登录、要 CSRF、返回 JSON。只按前缀判断会让它掉进
//! 「非 API」分支 —— 未登录时得到 200 的 SPA 入口页（前端拿到的是一坨
//! HTML 而不是 401），而且**完全没有 CSRF 校验**。所以这里两条判据都认。
//!
//! # 未登录时的响应（对齐原版）
//!
//! - `/api/*` 与 `POST /debug/run`
//!   → `401 {"ok":false,"error":"未登录或会话已过期"}`
//! - 其他 → 返回前端入口，由 SPA 自己渲染登录界面
//!
//! 原版这里有个 bug：`redirect(url_for("login_page"))`，但代码里**没有
//! `login_page` 这个端点**，所以访问受保护的非 API 页面会直接 500。
//! 本版把前端做成 SPA，未登录访问任何页面都返回同一个入口页 ——
//! 这个 500 路径自然就不存在了。

use axum::body::Body;
use axum::extract::State;
use axum::http::{header, HeaderMap, Method, Request, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};

use crate::auth::session::{Session, SessionError, SessionSigner, COOKIE_NAME};
use crate::error::ApiError;

/// 从当前请求里解析出的会话状态，注入到 `Request::extensions`。
///
/// handler 用 `Extension<AuthContext>` 取。之所以不把用户信息塞进
/// `State` 或全局变量，是为了避免「上一个请求的身份泄漏到下一个请求」——
/// 身份是**每请求**的东西，就必须活在每个请求自己的生命周期里。
#[derive(Debug, Clone)]
pub struct AuthContext {
    /// 有效的会话；未登录时为 `None`。
    pub session: Option<Session>,
}

impl AuthContext {
    pub fn is_authenticated(&self) -> bool {
        self.session.is_some()
    }

    /// 当前用户名，未登录返回 `None`。
    pub fn username(&self) -> Option<&str> {
        self.session.as_ref().map(|s| s.u.as_str())
    }

    pub fn is_admin(&self) -> bool {
        self.session.as_ref().is_some_and(Session::is_admin)
    }
}

/// 不需要登录就能访问的路由。
///
/// 用 `(方法, 路径)` 精确匹配，而不是前缀匹配 —— 前缀匹配很容易
/// 一不小心把 `/api/login-history` 之类的新路由也放行了。
fn is_public(method: &Method, path: &str) -> bool {
    // 归一化尾部斜杠，避免 `/api/channels/` 这种写法绕过判断
    let trimmed = path.trim_end_matches('/');
    let path = if trimmed.is_empty() { "/" } else { trimmed };

    matches!(
        (method, path),
        (&Method::GET, "/")
            | (&Method::GET, "/api/me")
            | (&Method::POST, "/api/login")
            | (&Method::POST, "/api/logout")
            | (&Method::POST, "/api/lang")
    )
}

/// 状态变更方法需要 CSRF 校验。
fn is_state_changing(method: &Method) -> bool {
    matches!(
        *method,
        Method::POST | Method::PUT | Method::PATCH | Method::DELETE
    )
}

/// 前端所有写请求都要带这个头。
///
/// 自定义头是 CSRF 的第二道防线：跨站请求可以带 cookie，但要让浏览器
/// 发出自定义头就必须经过 CORS 预检，而预检会被同源策略挡下。
/// 第一道防线是 `SameSite=Lax`（见 [`cookie_attributes`]）。
pub const CSRF_HEADER: &str = "x-requested-with";
pub const CSRF_HEADER_VALUE: &str = "pokemmo-alpha-panel";

/// 构造 `Set-Cookie` 的头部值。
///
/// - `HttpOnly`：JS 读不到，降低 XSS 的收益
/// - `SameSite=Lax`：跨站表单提交不再携带 cookie → CSRF 的第一道防线
/// - `Path=/`
/// - **不设 `Secure`**：面板在内网走 HTTP（5703），加了 `Secure`
///   浏览器会直接丢弃 cookie，登录就废了。将来上 HTTPS 要跟着改。
pub fn cookie_header_value(token: &str, max_age_secs: i64) -> String {
    format!("{COOKIE_NAME}={token}; Path=/; HttpOnly; SameSite=Lax; Max-Age={max_age_secs}")
}

/// 让浏览器立刻丢弃会话 cookie（登出用）。
///
/// 用 `Max-Age=0` 而不是只靠前端删 —— HttpOnly 的 cookie 前端根本删不掉。
pub fn clear_cookie_header_value() -> String {
    format!("{COOKIE_NAME}=; Path=/; HttpOnly; SameSite=Lax; Max-Age=0")
}

/// 从 `Cookie` 头里取出**全部**同名会话 token。
///
/// 手写解析要注意**同名 cookie 有多个**怎么办。旧实现是「出现重复就
/// 直接判无效」，理由是防 cookie tossing —— 但上线当天就被真实世界
/// 教育了：浏览器里的 cookie 按（名字, 路径, 域）匹配，**不区分端口**，
/// 原版 Flask 面板（5703）种下的 `session` 会和 Rust 版（5704）种的
/// 共存在同一条请求里，合法用户就这么被挡在门外（下载 401 的元凶）。
///
/// 所以现在改成返回全部候选，由调用方逐个验签、取有效者。这依然是
/// tossing 安全的：攻击者塞进来的伪造 cookie 过不了 HMAC 签名验证，
/// 我们只会选中真正持有密钥的一方签出的会话。
pub fn extract_session_tokens(headers: &HeaderMap) -> Result<Vec<String>, CookieError> {
    let Some(raw) = headers.get(header::COOKIE) else {
        return Ok(Vec::new());
    };
    let raw = raw.to_str().map_err(|_| CookieError::MalformedHeader)?;

    let mut found: Vec<String> = Vec::new();
    for pair in raw.split(';') {
        if let Some((name, value)) = pair.split_once('=') {
            if name.trim() == COOKIE_NAME {
                found.push(value.trim().to_string());
            }
        }
    }
    Ok(found)
}

/// cookie 解析阶段的错误。
#[derive(Debug, PartialEq, Eq)]
pub enum CookieError {
    /// `Cookie` 头不是合法的 ASCII。
    MalformedHeader,
}

/// 鉴权中间件本体。
pub async fn auth_middleware(
    State(signer): State<SessionSigner>,
    mut req: Request<Body>,
    next: Next,
) -> Response {
    let path = req.uri().path().to_string();
    let method = req.method().clone();

    // 「按 API 对待」的路径集合：`/api/*` 加上一条历史遗留的 `/debug/run`。
    //
    // `/debug/run` 原版是 `@app.route("/debug/run")`，挂在根上而不是
    // `/api` 下（前端也照这个路径调）。但它在语义上完全是个 API：
    // 要登录、要 CSRF、返回 JSON。只按 `/api/` 前缀判断的话，
    // 它会掉进「非 API」分支 —— 未登录时返回 200 的 SPA 入口页
    // （前端拿到的是一坨 HTML 而不是 401），且**完全没有 CSRF 校验**。
    let is_api = path.starts_with("/api/") || (method == Method::POST && path == "/debug/run");

    // ---- 1. 解析会话 ----
    let now = chrono::Utc::now().timestamp();
    let mut session = None;
    match extract_session_tokens(req.headers()) {
        Err(e) => {
            // 畸形 Cookie 头视作未登录。
            tracing::warn!(%path, "cookie 无法解析: {e:?}");
        }
        Ok(tokens) => {
            for (i, token) in tokens.iter().enumerate() {
                match signer.verify(token, now) {
                    Ok(s) => {
                        // 多个同名 cookie 说明浏览器里存了历史残留
                        // （如原版面板经 5703 种下的），能找到一个有效的就用它。
                        if tokens.len() > 1 {
                            tracing::debug!(
                                %path, total = tokens.len(), adopted = i,
                                "存在多个同名会话 cookie（多为旧版/换端口的历史残留），已采用其中一个有效会话"
                            );
                        }
                        session = Some(s);
                        break;
                    }
                    Err(e) => {
                        // 区分「过期」和「签名不对」：前者是正常现象（用户放太久
                        // 没操作），后者可能是攻击。日志级别和用词都不一样。
                        // 唯一候选时才升级日志 —— 多候选失败属残留噪音，降为 debug。
                        match (tokens.len(), &e) {
                            (_, SessionError::Expired) if tokens.len() == 1 => {
                                tracing::debug!(%path, "会话已过期");
                            }
                            (_, SessionError::BadSignature) if tokens.len() == 1 => {
                                tracing::warn!(%path, "会话签名校验失败 —— 可能是伪造尝试");
                            }
                            (_, SessionError::Format) if tokens.len() == 1 => {
                                // 原版 itsdangerous 的三段式 cookie 走这里。
                                // 迁移期间这是**预期现象**，不该当攻击报警。
                                tracing::debug!(%path, "会话 cookie 格式无法识别（多为旧版遗留）");
                            }
                            _ => {
                                tracing::debug!(%path, candidate = i, error = ?e, "候选会话 cookie 验证失败，继续尝试下一个");
                            }
                        }
                    }
                }
            }
        }
    }

    let authenticated = session.is_some();
    req.extensions_mut().insert(AuthContext { session });

    // ---- 2. CSRF：状态变更请求必须带自定义头 ----
    //
    // 放在鉴权判断**之前**，理由是：CSRF 请求即使携带了合法 cookie
    // 也必须被拒，它跟「有没有登录」是两件独立的事。
    if is_api && is_state_changing(&method) {
        let ok = req
            .headers()
            .get(CSRF_HEADER)
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| v == CSRF_HEADER_VALUE);

        if !ok {
            tracing::warn!(%path, %method, "缺少 CSRF 自定义头，拒绝");
            return ApiError::BadRequest("缺少必要的请求头（CSRF 校验失败）".into())
                .into_response();
        }
    }

    // ---- 3. 放行名单，或被拦下 ----
    if is_public(&method, &path) {
        return next.run(req).await;
    }

    if !authenticated {
        if is_api {
            return ApiError::Unauthorized.into_response();
        }
        // 静态资源**不能**被入口页接管，必须继续往下走到 `ServeDir`。
        //
        // 这条分支一开始漏了，症状很隐蔽：`/static/style.css` 返回的是
        // 入口页的 HTML，而浏览器把 `text/html` 当 CSS 解析 ——
        // 控制台只有一句语焉不详的「样式表 MIME 类型不对」，
        // 没人会想到是中间件把请求改写掉了。
        //
        // 判据用前缀而不是文件是否存在：中间件这一层不该碰磁盘。
        // 真不存在的资源由 `ServeDir` 回 404，那才是正确的答案。
        if path.starts_with("/static/") {
            return next.run(req).await;
        }

        // 其余非 API 路径：返回前端入口页，由 SPA 渲染登录界面。
        // 原版这里是 500（`url_for("login_page")` 指向不存在的端点）。
        //
        // **故意返回 200 而不是 302**：对 SPA 入口页返回 302 会让
        // 浏览器地址栏跳走，用户刷新后落到一个不存在的路径上。
        return spa_entry();
    }

    next.run(req).await
}

/// SPA 入口页。
///
/// 内容来自 `web/index.html`（原版 `panel/templates/index.html` 的副本），
/// 靠 [`crate::spa`] 启动时读进内存一次。**已登录与未登录返回的是同一份**
/// —— 登录与否由前端自己调 `/api/me` 判断，服务端不参与渲染。
///
/// # 读不到文件时的降级
///
/// 部署时 `web/` 目录没跟着二进制一起搬是很常见的事（尤其是手工 scp）。
/// 那种情况下面板会变成空白页，而且**没有任何提示** —— 运维只能看到
/// 浏览器白屏。所以这里降级成一段能自证问题的 HTML：
/// 它明确说出「前端资源没找到，期望在哪个路径」，并给出 `ALPHA_WEB_ROOT`
/// 这个开关。让人一眼知道该修什么，而不是去猜。
pub fn spa_entry() -> Response {
    let body = crate::spa::index_html().unwrap_or_else(crate::spa::missing_assets_page);

    (
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, "text/html; charset=utf-8"),
            // 入口页本身不缓存：它带的 `?v=` 版本号用于失效静态资源缓存，
            // 入口页自己被缓存住的话，改了版本号用户也拿不到新的。
            (header::CACHE_CONTROL, "no-cache"),
        ],
        body,
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    fn headers_with_cookie(value: &str) -> HeaderMap {
        let mut h = HeaderMap::new();
        h.insert(header::COOKIE, HeaderValue::from_str(value).unwrap());
        h
    }

    #[test]
    fn public_routes_match_the_original_whitelist() {
        // 原版 before_request 放行的 5 条
        assert!(is_public(&Method::GET, "/"));
        assert!(is_public(&Method::GET, "/api/me"));
        assert!(is_public(&Method::POST, "/api/login"));
        assert!(is_public(&Method::POST, "/api/logout"));
        assert!(is_public(&Method::POST, "/api/lang"));

        // 其余一律受保护 —— 特别是这些高危写接口
        for path in [
            "/api/channels",
            "/api/dispatchers",
            "/api/evaluators",
            "/api/plugins",
            "/api/config",
            "/api/scheduler",
            "/api/sources",
            "/api/logs",
        ] {
            assert!(!is_public(&Method::GET, path), "{path} 不该放行");
            assert!(!is_public(&Method::POST, path), "{path} 不该放行");
        }
    }

    #[test]
    fn method_must_match_not_just_the_path() {
        // 原版只放行 POST，GET 不行
        assert!(!is_public(&Method::GET, "/api/login"));
        assert!(!is_public(&Method::GET, "/api/logout"));
        assert!(!is_public(&Method::GET, "/api/lang"));
        assert!(!is_public(&Method::DELETE, "/api/me"));
        assert!(!is_public(&Method::PUT, "/api/login"));
    }

    #[test]
    fn trailing_slash_does_not_bypass_the_whitelist() {
        // 白名单里的路由加斜杠仍应放行（用户手输错不该 401）
        assert!(is_public(&Method::POST, "/api/login/"));
        assert!(is_public(&Method::GET, "/api/me/"));
        assert!(is_public(&Method::GET, ""));

        // 受保护的路由加斜杠**不能**变成放行
        assert!(!is_public(&Method::GET, "/api/channels/"));
        assert!(!is_public(&Method::GET, "/api/plugins/"));
    }

    #[test]
    fn prefix_lookalikes_are_not_whitelisted() {
        // 精确匹配的价值：这些都不该被放行
        for path in [
            "/api/login-history",
            "/api/logout-all",
            "/api/me/settings",
            "/api/lang/extra",
        ] {
            assert!(!is_public(&Method::POST, path), "{path} 不该放行");
        }
    }

    #[test]
    fn state_changing_methods_are_identified() {
        assert!(is_state_changing(&Method::POST));
        assert!(is_state_changing(&Method::PUT));
        assert!(is_state_changing(&Method::PATCH));
        assert!(is_state_changing(&Method::DELETE));
        assert!(!is_state_changing(&Method::GET));
        assert!(!is_state_changing(&Method::HEAD));
    }

    #[test]
    fn extracts_a_single_session_cookie() {
        let h = headers_with_cookie("session=abc.def");
        assert_eq!(extract_session_tokens(&h).unwrap(), vec!["abc.def"]);
    }

    #[test]
    fn handles_other_cookies_around_it() {
        let h = headers_with_cookie("theme=dark; session=abc.def; lang=zh");
        assert_eq!(extract_session_tokens(&h).unwrap(), vec!["abc.def"]);
    }

    #[test]
    fn no_cookie_header_means_no_session() {
        assert!(extract_session_tokens(&HeaderMap::new()).unwrap().is_empty());
        let h = headers_with_cookie("theme=dark");
        assert!(extract_session_tokens(&h).unwrap().is_empty());
    }

    /// **重复的同名 cookie 全部返回，选择权交给验签。**
    ///
    /// 旧实现是「重复即拒绝」，理由是防 cookie tossing —— 但浏览器里
    /// 同名 `session` 残留是真实常态（cookie 不区分端口，原版 5703 种的
    /// 会和 Rust 版 5704 种的共存），合法用户因此被挡在门外。
    /// 现在把全部候选交给调用方逐个验签：伪造的过不了 HMAC，tossing
    /// 依然无效。
    #[test]
    fn duplicate_session_cookies_are_all_collected() {
        let h = headers_with_cookie("session=attacker; session=victim");
        assert_eq!(
            extract_session_tokens(&h).unwrap(),
            vec!["attacker", "victim"]
        );

        let h = headers_with_cookie("session=a; theme=x; session=b; session=c");
        assert_eq!(extract_session_tokens(&h).unwrap(), vec!["a", "b", "c"]);

        // 名字不同的重复无所谓
        let h = headers_with_cookie("session=a; sessionother=b");
        assert_eq!(extract_session_tokens(&h).unwrap(), vec!["a"]);
    }

    #[test]
    fn cookie_name_must_match_exactly() {
        // `sessionx` / `my_session` 都不是我们要的
        let h = headers_with_cookie("sessionx=abc; my_session=def");
        assert!(extract_session_tokens(&h).unwrap().is_empty());
    }

    #[test]
    fn cookie_whitespace_is_tolerated() {
        let h = headers_with_cookie("  session = abc.def  ; theme=x");
        assert_eq!(extract_session_tokens(&h).unwrap(), vec!["abc.def"]);
    }

    #[test]
    fn malformed_cookie_header_is_an_error_not_a_panic() {
        let mut h = HeaderMap::new();
        h.insert(
            header::COOKIE,
            HeaderValue::from_bytes(&[0xff, 0xfe]).unwrap(),
        );
        assert_eq!(extract_session_tokens(&h), Err(CookieError::MalformedHeader));
    }

    #[test]
    fn empty_session_value_is_left_for_the_signature_check() {
        // 空 token 交给签名校验去拒（会得到 Format），
        // 解析这一层不该替它做决定
        let h = headers_with_cookie("session=");
        assert_eq!(extract_session_tokens(&h).unwrap(), vec![""]);
    }

    /// cookie 属性必须包含 HttpOnly 与 SameSite=Lax —— 这两项原版都没有。
    #[test]
    fn cookie_attributes_include_the_hardening_the_original_lacked() {
        let v = cookie_header_value("tok", 3600);
        assert!(v.contains("HttpOnly"), "缺少 HttpOnly: {v}");
        assert!(v.contains("SameSite=Lax"), "缺少 SameSite: {v}");
        assert!(v.contains("Max-Age=3600"), "缺少有效期: {v}");
        assert!(v.contains("Path=/"), "缺少 Path: {v}");
        assert!(v.starts_with("session=tok"), "cookie 名要沿用原版: {v}");

        // 内网走 HTTP，加 Secure 会让 cookie 被浏览器丢弃 —— 确认没加
        assert!(!v.contains("Secure"), "面板是 HTTP，加 Secure 会致登录失效: {v}");
    }

    /// 登出必须能真正删掉 cookie。
    ///
    /// HttpOnly 的 cookie 前端 JS 删不掉，只能靠服务端发 `Max-Age=0`。
    #[test]
    fn clearing_the_cookie_actually_expires_it() {
        let v = clear_cookie_header_value();
        assert!(v.starts_with("session=;"), "值应为空: {v}");
        assert!(v.contains("Max-Age=0"), "必须立即过期: {v}");
        assert!(v.contains("Path=/"), "路径要匹配才能删掉: {v}");
    }

    #[test]
    fn csrf_header_is_a_custom_one() {
        // 自定义头是 CSRF 的第二道防线，必须是浏览器默认不会自动带的
        assert_eq!(CSRF_HEADER, "x-requested-with");
        assert!(!CSRF_HEADER.starts_with("content-"), "不该用会被自动带的头");
    }

    #[test]
    fn auth_context_reports_state_correctly() {
        let anon = AuthContext { session: None };
        assert!(!anon.is_authenticated());
        assert_eq!(anon.username(), None);
        assert!(!anon.is_admin());

        let admin = AuthContext {
            session: Some(Session::new("hanyx", "admin", 0, 60)),
        };
        assert!(admin.is_authenticated());
        assert_eq!(admin.username(), Some("hanyx"));
        assert!(admin.is_admin());

        let user = AuthContext {
            session: Some(Session::new("bob", "user", 0, 60)),
        };
        assert!(user.is_authenticated());
        assert!(!user.is_admin(), "普通用户不该被当成管理员");
    }
}
