//! `alpha-server`：axum Web 服务 + 调度器，替代原版的 Flask 面板。
//!
//! # 相对原版的三处安全改进
//!
//! | 原版问题 | 本版处置 |
//! |---|---|
//! | 会话密钥有公开默认值 → 可伪造 admin cookie（**现网正在被利用**） | 密钥必填，缺失/弱密钥一律拒绝启动（见 [`config`]） |
//! | 插件上传 `.py` 后 `exec_module()` → RCE | 换成 Rhai 沙箱（见 `alpha-plugin`） |
//! | 无 CSRF 防护 | SameSite=Lax + 自定义头校验 |
//!
//! # 模块地图
//!
//! | 模块 | 职责 |
//! |---|---|
//! | [`config`] | 启动配置与 `panel.env` 读写 |
//! | [`error`] | 统一的 API 错误类型与响应格式 |
//! | [`auth`] | 密码校验、会话签发、鉴权中间件 |
//! | [`spa`] | 前端资源的定位与加载（原版前端，一字未改） |
//! | [`testdata`] | 原版产出的基准数据（差分回归用） |

pub mod app;
pub mod auth;
pub mod config;
pub mod error;
pub mod routes;
pub mod spa;
pub mod testdata;

pub use app::{build_router, AppState};
pub use config::{Config, ConfigError};
pub use error::{ApiError, ApiResult};
