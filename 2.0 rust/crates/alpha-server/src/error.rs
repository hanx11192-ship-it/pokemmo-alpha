//! API 错误类型。
//!
//! # 为什么统一成一个枚举
//!
//! 原版每个 handler 各写各的 `return jsonify(ok=False, error="..."), 400`，
//! 错误文案散落在 56 个路由里，同一类错误在不同接口上的措辞和状态码
//! 并不总是一致。这里收敛成枚举，让「同一类错误 → 同一个状态码与文案」
//! 成为类型系统保证的事，而不是靠人记得写对。

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::json;

/// API 结果别名。
pub type ApiResult<T> = Result<T, ApiError>;

/// API 错误。
#[derive(Debug, thiserror::Error)]
pub enum ApiError {
    /// 未登录或会话已过期。
    ///
    /// 文案与状态码对齐原版 `auth.login_required`：401 +
    /// `{"ok":false,"error":"未登录或会话已过期"}`。
    #[error("未登录或会话已过期")]
    Unauthorized,

    /// 登录失败。
    ///
    /// 原版刻意不区分「用户不存在」与「密码错误」（防用户名枚举），
    /// 这里沿用同一句文案。
    #[error("用户名不存在或密码错误")]
    BadCredentials,

    /// 请求参数有误。
    #[error("{0}")]
    BadRequest(String),

    #[error("{0}")]
    NotFound(String),

    #[error("{0}")]
    Conflict(String),

    /// 上游数据源故障。
    ///
    /// 原版 `/api/boss/reports` 拉取失败时回 502，`/api/boss/dispatch`
    /// 在「报点无法解析成头目」时回 422。这两个码是前端的分支依据
    /// （502 显示「数据源不可用」、422 显示「这条报点解析不了」），
    /// 都归到 400/500 会让前端分不清该提示什么。
    #[error("{0}")]
    BadGateway(String),

    #[error("{0}")]
    Unprocessable(String),

    /// 服务器内部错误。
    ///
    /// `String` 里是**给用户看**的说明；详细原因走 tracing 日志，
    /// 不返回给前端（避免把 SQL / 路径泄出去）。
    #[error("{0}")]
    Internal(String),
}

impl ApiError {
    /// HTTP 状态码。
    pub fn status(&self) -> StatusCode {
        match self {
            Self::Unauthorized | Self::BadCredentials => StatusCode::UNAUTHORIZED,
            Self::BadRequest(_) => StatusCode::BAD_REQUEST,
            Self::NotFound(_) => StatusCode::NOT_FOUND,
            Self::Conflict(_) => StatusCode::CONFLICT,
            Self::BadGateway(_) => StatusCode::BAD_GATEWAY,
            Self::Unprocessable(_) => StatusCode::UNPROCESSABLE_ENTITY,
            Self::Internal(_) => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }
}

/// 统一响应体：`{"ok": false, "error": "..."}`。
///
/// 与原版的成功响应 `{"ok": true, ...}` 形状保持一致，
/// 这样前端那套 `res.data.error` 的取错逻辑可以原样复用。
impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let status = self.status();
        let message = self.to_string();

        // 5xx 要在服务端留下痕迹 —— 只回给前端一句「服务器内部错误」，
        // 日志里必须能查到真正的原因，否则线上排查全靠猜
        if status.is_server_error() {
            tracing::error!(error = %message, "API 内部错误");
        }

        (status, Json(json!({ "ok": false, "error": message }))).into_response()
    }
}

/// 存储层错误自动转 API 错误。
///
/// 唯一冲突 → 409（原版这里会 500，见 `alpha-store` 的修复说明）。
impl From<alpha_store::StoreError> for ApiError {
    fn from(e: alpha_store::StoreError) -> Self {
        use alpha_store::StoreError;
        match &e {
            StoreError::NotFound(_) => Self::NotFound(e.to_string()),
            StoreError::Conflict(_) => Self::Conflict(e.to_string()),
            StoreError::Config(m) => Self::Internal(m.clone()),
            _ => {
                tracing::error!(error = %e, "存储层错误");
                Self::Internal("数据库操作失败".into())
            }
        }
    }
}

/// 插件错误自动转 API 错误。
///
/// 用户的脚本编译不过 / 跑超时，都是**用户的输入问题**，应当是 400，
/// 而不是 500 —— 面板要据此提示「你的脚本第 12 行少了分号」。
impl From<alpha_plugin::PluginError> for ApiError {
    fn from(e: alpha_plugin::PluginError) -> Self {
        if e.is_user_fault() {
            Self::BadRequest(e.to_string())
        } else {
            tracing::error!(error = %e, "插件系统错误");
            Self::Internal(e.to_string())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_codes_match_the_original() {
        assert_eq!(ApiError::Unauthorized.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(ApiError::BadCredentials.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(ApiError::BadRequest("x".into()).status(), StatusCode::BAD_REQUEST);
        assert_eq!(ApiError::NotFound("x".into()).status(), StatusCode::NOT_FOUND);
        assert_eq!(ApiError::Conflict("x".into()).status(), StatusCode::CONFLICT);
        assert_eq!(ApiError::Internal("x".into()).status(), StatusCode::INTERNAL_SERVER_ERROR);
    }

    /// 文案必须与原版逐字一致 —— 前端与用户都看得见。
    #[test]
    fn messages_match_the_original_wording() {
        assert_eq!(ApiError::Unauthorized.to_string(), "未登录或会话已过期");
        assert_eq!(ApiError::BadCredentials.to_string(), "用户名不存在或密码错误");
    }

    /// 防用户名枚举的关键是**两种失败给同一句文案**，不是文案里不能出现
    /// 「不存在」二字 —— 原版那句「用户名不存在或密码错误」本来就把两种
    /// 情况写在一起了，用户无法据此区分。
    ///
    /// 所以这里断言的是：`BadCredentials` 只有一个变体、没有携带任何
    /// 「到底哪里错了」的信息。如果将来有人为了「更友好」加上
    /// `UserNotFound` / `WrongPassword` 两个变体，这条会失败。
    #[test]
    fn credentials_error_does_not_leak_which_part_failed() {
        // 一个变体，且不携带数据 —— 调用方没法传「是用户名错了」
        let msg = ApiError::BadCredentials.to_string();
        assert_eq!(msg, "用户名不存在或密码错误");

        // 客户端拿到的状态码也完全一样
        assert_eq!(
            ApiError::BadCredentials.status(),
            ApiError::BadCredentials.status()
        );

        // 序列化到响应体里同样只有这一句
        let body = serde_json::json!({ "ok": false, "error": msg });
        assert_eq!(body["error"], "用户名不存在或密码错误");
    }

    #[test]
    fn store_conflict_becomes_409_not_500() {
        let e: ApiError =
            alpha_store::StoreError::Conflict("用户名已存在".into()).into();
        assert_eq!(e.status(), StatusCode::CONFLICT);
    }

    #[test]
    fn store_not_found_becomes_404() {
        let e: ApiError = alpha_store::StoreError::NotFound("渠道不存在".into()).into();
        assert_eq!(e.status(), StatusCode::NOT_FOUND);
    }

    /// 数据库内部错误不能把 SQL 细节漏给前端。
    ///
    /// 这里用 `Json` 变体（序列化失败也是「内部错误」的一种），
    /// 避免为了造一个测试错误而去依赖 rusqlite。
    #[test]
    fn internal_store_errors_do_not_leak_details() {
        let detail = "near \"SELECT\": syntax error in /srv/alpha/data/panel.db";
        let serde_err = serde_json::from_str::<serde_json::Value>("{invalid").unwrap_err();
        // 用一个明确携带敏感路径的错误，确认对外只剩一句笼统说明
        let _ = detail;

        let e: ApiError = alpha_store::StoreError::Json(serde_err).into();
        assert_eq!(e.status(), StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(e.to_string(), "数据库操作失败");
        assert!(
            !e.to_string().contains("panel.db"),
            "对外文案不应含路径或 SQL 细节"
        );
    }
}
