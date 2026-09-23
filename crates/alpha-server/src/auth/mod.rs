//! 鉴权：密码校验、会话签发/校验、axum 中间件。
//!
//! # 原版的三处问题
//!
//! | 原版 | 本版 |
//! |---|---|
//! | `app.secret_key` 有公开默认值 `alpha-panel-dev-secret`，而线上 systemd 没有注入 `PANEL_SECRET` → 任何人都能伪造 admin cookie | 密钥必填且过强度校验（[`crate::config`]），启动即失败 |
//! | 会话 cookie 无过期时间，签发后永久有效 | 自有格式带 `iat`/`exp`，默认 7 天（[`session::DEFAULT_TTL`]） |
//! | 没有任何 CSRF 防护 | `SameSite=Lax` + 状态变更请求校验自定义头（[`layer`]） |
//!
//! 第三点值得说明：原版的全站 POST 接口（改配置、加频道、上传插件）
//! 都只靠 cookie 鉴权，任意第三方页面都能让已登录的管理员浏览器
//! 顺手把这些操作做掉。加 `SameSite=Lax` 之后跨站表单提交不会再带 cookie，
//! 自定义头校验则是给 `SameSite=None` 场景（比如反代跨域）留的后手。

pub mod layer;
pub mod password;
pub mod session;
