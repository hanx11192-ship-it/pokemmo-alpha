//! HTTP 路由。
//!
//! 原版 56 个路由分散在单个 `app.py` 里，按功能分模块是重写时最该做的整理。
//! 每个子模块对应前端的一个页面。

pub mod auth;
pub mod boss;
pub mod channels;
pub mod debug;
pub mod logs;
pub mod plugins;
pub mod scheduler;
pub mod sources;
pub mod system;
