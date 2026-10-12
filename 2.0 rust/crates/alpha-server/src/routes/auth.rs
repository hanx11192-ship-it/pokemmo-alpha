//! 登录 / 会话 / 语言偏好四个接口。
//!
//! 对应原版 `panel/app.py` 的 `api_me` / `api_login` / `api_logout` /
//! `api_set_lang`，以及 `panel/auth.py` 的 `do_login`。
//!
//! # 与原版逐字对齐的地方
//!
//! - 登录失败的文案是 `用户名不存在或密码错误`。**刻意不区分**是哪一项错 ——
//!   分开提示等于给了用户名枚举接口。
//! - 首次登录（`users` 表为空）自动成为管理员。
//! - `/api/me` 未登录返回 200 且 `authenticated: false`。
//! - `/api/lang` 只接受 `zh` / `en`，其他返回 400。
//!
//! # 故意偏离的一处
//!
//! 原版的 `/api/lang` 在未登录时返回 401 `{"error":"未登录"}`，
//! 但它同时又挂在放行名单里（不需要登录也能进来），所以那条 401 是可达的。
//! 本版**沿用这个行为**（因为前端可能依赖），但额外做了一件事：
//! 把语言偏好写进会话 cookie 之外的地方时先确认用户真的存在 ——
//! 原版的 `set_user_lang` 对不存在的用户是静默无操作，返回 `ok: true`，
//! 这会让前端以为保存成功了。这里改成明确报错。

use axum::extract::{Extension, State};
use axum::http::header;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::auth::layer::{cookie_header_value, AuthContext};
use crate::auth::password;
use crate::auth::session::Session;
use crate::error::{ApiError, ApiResult};
use crate::AppState;

/// `/api/me` 的响应。
#[derive(Debug, Serialize)]
pub struct MeResponse {
    pub authenticated: bool,
    pub user: Option<String>,
    pub role: Option<String>,
    pub lang: String,
}

/// `GET /api/me`
///
/// 未登录时**也返回 200**，字段为 `null` / `"zh"`。
///
/// 为什么不做成 401：前端一加载就调它来决定渲染登录页还是面板。
/// 如果 401，前端就得把「还没登录」当成一种错误来处理 ——
/// 而这不是错误，是初始状态。
pub async fn me(
    State(st): State<AppState>,
    Extension(ctx): Extension<AuthContext>,
) -> ApiResult<Json<MeResponse>> {
    let Some(username) = ctx.username() else {
        return Ok(Json(MeResponse {
            authenticated: false,
            user: None,
            role: None,
            lang: "zh".to_string(),
        }));
    };

    // 语言偏好存在**数据库**里（原版就是 `users.lang`），不是会话里。
    // 所以这里要查一次库：会话可能是七天前签的，期间用户改过语言。
    //
    // 查不到用户（比如已被删除，但 cookie 还在）时退回 `zh`
    // 并让上层知道 —— 这里选择不报错，因为「删了用户」之后
    // 那个 cookie 本来就该在下一个受保护接口上被踢掉。
    let lang = st
        .store
        .get_user(username)?
        .map(|u| u.lang)
        .unwrap_or_else(|| "zh".to_string());

    Ok(Json(MeResponse {
        authenticated: true,
        user: Some(username.to_string()),
        role: ctx.session.as_ref().map(|s| s.r.clone()),
        lang,
    }))
}

/// `POST /api/login` 的请求体。
#[derive(Debug, Deserialize)]
pub struct LoginRequest {
    #[serde(default)]
    pub username: String,
    #[serde(default)]
    pub password: String,
}

/// 登录成功后的响应。
///
/// `is_first_admin` 只在「首次登录创建管理员」时出现，
/// 前端据此提示「你已成为管理员，请立即设置密码」之类的引导。
#[derive(Debug, Serialize)]
pub struct LoginResponse {
    pub ok: bool,
    pub username: String,
    pub role: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub is_first_admin: Option<bool>,
}

/// `POST /api/login`
///
/// 首次登录（`users` 表为空）时，第一个登录者成为管理员 —— 与原版一致。
///
/// 这是个**设计上的取舍**，不是疏漏：全新部署时没有别的办法创建第一个账号。
/// 风险窗口是「服务起来了但没人登录过」这段时间，所以实践中要做的是
/// 部署完立刻登一次、或者用脚本预置账号。
/// 本版保留了它，因为它是原版明确的行为，改掉会让首次部署卡住。
pub async fn login(State(st): State<AppState>, Json(body): Json<LoginRequest>) -> Response {
    match do_login(&st, &body.username, &body.password) {
        Ok((session, is_first_admin)) => {
            let token = st.signer.sign(&session);
            let cookie = cookie_header_value(&token, st.signer.ttl());

            let payload = LoginResponse {
                ok: true,
                username: session.u.clone(),
                role: session.r.clone(),
                is_first_admin,
            };

            (
                [(header::SET_COOKIE, cookie)],
                Json(serde_json::to_value(payload).expect("简单结构不会失败")),
            )
                .into_response()
        }
        // 失败时**不发 Set-Cookie**：一个失败的登录请求不该能影响
        // 调用方原有的会话状态（否则等于多了一个「登出」的副作用入口）
        Err(e) => e.into_response(),
    }
}

/// 登录的核心逻辑，与 HTTP 层解耦，便于直接测。
///
/// 返回 `(会话, 是否是首次创建的管理员)`。
fn do_login(
    st: &AppState,
    username: &str,
    password: &str,
) -> Result<(Session, Option<bool>), ApiError> {
    let username = username.trim();

    // 空用户名或空密码直接失败，不去查库。
    // 原版同样是先挡空值再查 —— 这样 `{"username":"","password":""}`
    // 不会因为「库里没有空用户名的记录」而走首登分支。
    if username.is_empty() || password.is_empty() {
        return Err(ApiError::BadCredentials);
    }

    let now = chrono::Utc::now().timestamp();
    let ttl = st.signer.ttl();

    match st.store.get_user(username)? {
        None => {
            // 用户不存在。只有在**一个用户都没有**时才走首登创建管理员，
            // 否则一律失败 —— 用同一句文案，不泄露「用户是否存在」。
            if st.store.count_users()? == 0 {
                let hash = password::hash(password)
                    .map_err(|e| ApiError::Internal(format!("生成密码哈希失败: {e}")))?;
                st.store.add_user(username, &hash, true, "zh")?;
                tracing::info!(%username, "首次登录，已创建管理员账号");
                return Ok((Session::new(username, "admin", now, ttl), Some(true)));
            }
            Err(ApiError::BadCredentials)
        }
        Some(user) => {
            let ok = password::verify(&user.password_hash, password).map_err(|e| {
                // 这里**不能**直接返回 BadCredentials：格式不认识是
                // 「部署/数据问题」，不是「用户记错密码」。原版把这两种
                // 情况都表现成「密码错误」，运维会查错方向。
                tracing::error!(%username, error = %e, "密码哈希无法解析");
                ApiError::Internal("账号数据异常，请联系管理员".into())
            })?;

            if !ok {
                return Err(ApiError::BadCredentials);
            }

            // 校验成功之后才做哈希升级：werkzeug 的 scrypt/pbkdf2 换成 argon2。
            // 顺序很重要 —— 在校验成功之前升级，等于把「这个账号存在」
            // 这个信息通过「哈希变了」暴露出去。
            if password::needs_rehash(&user.password_hash) {
                match password::hash(password) {
                    Ok(fresh) => {
                        if let Err(e) = st.store.set_user_password(username, &fresh) {
                            // 升级失败不影响本次登录，记一笔就够了
                            tracing::warn!(%username, error = %e, "密码哈希升级失败");
                        } else {
                            tracing::info!(%username, "密码哈希已升级为 argon2");
                        }
                    }
                    Err(e) => tracing::warn!(%username, error = %e, "生成新哈希失败"),
                }
            }

            let role = if user.is_admin { "admin" } else { "user" };
            Ok((Session::new(username, role, now, ttl), None))
        }
    }
}

/// `POST /api/logout`
///
/// 无状态会话的「登出」就是把 cookie 清掉。服务端没有会话表可删，
/// 所以**如果 cookie 已经被人抄走，登出并不能让它失效** ——
/// 这是签名 cookie 方案的固有代价，原版也一样。
/// 真要作废全部会话，得轮换 `PANEL_SECRET`（会让所有人重新登录）。
pub async fn logout() -> Response {
    (
        [(header::SET_COOKIE, crate::auth::layer::clear_cookie_header_value())],
        Json(json!({ "ok": true })),
    )
        .into_response()
}

/// `POST /api/lang` 的请求体。
#[derive(Debug, Deserialize)]
pub struct LangRequest {
    #[serde(default)]
    pub lang: Option<String>,
}

/// `POST /api/lang`
///
/// 语言偏好存在 DB 的 `users.lang`（与原版一致），不在会话里 ——
/// 这样换设备登录也是同一个语言。
pub async fn set_lang(
    State(st): State<AppState>,
    Extension(ctx): Extension<AuthContext>,
    Json(body): Json<LangRequest>,
) -> ApiResult<Json<Value>> {
    let Some(username) = ctx.username() else {
        return Err(ApiError::Unauthorized);
    };

    // 只认 zh / en。其他一律 400 —— 前端拿它当「选项不合法」处理。
    let lang = body.lang.as_deref().unwrap_or("");
    if !matches!(lang, "zh" | "en") {
        return Err(ApiError::BadRequest("语言不支持".into()));
    }

    // 原版的 `set_user_lang` 对不存在的用户是静默成功（UPDATE 影响 0 行），
    // 前端会显示「已保存」但其实没存。这里显式确认用户存在。
    if st.store.get_user(username)?.is_none() {
        return Err(ApiError::Unauthorized);
    }

    st.store.set_user_lang(username, lang)?;

    Ok(Json(json!({ "ok": true, "lang": lang })))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn login_request_tolerates_missing_fields() {
        // 原版用 `request.get_json(silent=True) or {}` 加 `.get(k, "")`，
        // 所以空 body、缺字段都不会 400，而是走「凭据为空」的失败路径
        let r: LoginRequest = serde_json::from_str("{}").unwrap();
        assert_eq!(r.username, "");
        assert_eq!(r.password, "");

        let r: LoginRequest = serde_json::from_str(r#"{"username":"a"}"#).unwrap();
        assert_eq!(r.username, "a");
        assert_eq!(r.password, "");
    }

    #[test]
    fn lang_request_tolerates_missing_field() {
        let r: LangRequest = serde_json::from_str("{}").unwrap();
        assert_eq!(r.lang, None);

        let r: LangRequest = serde_json::from_str(r#"{"lang":"en"}"#).unwrap();
        assert_eq!(r.lang.as_deref(), Some("en"));
    }

    /// 首登响应的 `is_first_admin` 只在首次出现，普通登录不该有这个字段。
    #[test]
    fn login_response_omits_is_first_admin_unless_first() {
        let normal = LoginResponse {
            ok: true,
            username: "hanyx".into(),
            role: "admin".into(),
            is_first_admin: None,
        };
        let v = serde_json::to_value(&normal).unwrap();
        assert!(
            v.get("is_first_admin").is_none(),
            "普通登录不该带 is_first_admin: {v}"
        );

        let first = LoginResponse {
            ok: true,
            username: "hanyx".into(),
            role: "admin".into(),
            is_first_admin: Some(true),
        };
        let v = serde_json::to_value(&first).unwrap();
        assert_eq!(v["is_first_admin"], true);
    }

    #[test]
    fn me_response_serializes_with_null_fields_when_anonymous() {
        let v = serde_json::to_value(MeResponse {
            authenticated: false,
            user: None,
            role: None,
            lang: "zh".into(),
        })
        .unwrap();
        assert_eq!(v["authenticated"], false);
        assert_eq!(v["user"], Value::Null);
        assert_eq!(v["role"], Value::Null);
        assert_eq!(v["lang"], "zh");
    }
}
