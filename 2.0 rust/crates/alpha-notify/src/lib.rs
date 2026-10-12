//! 推送层：把生成好的播报内容发到各个渠道。
//!
//! # 三类渠道
//!
//! | 类型 | 目标 | 关键配置 |
//! |------|------|----------|
//! | [`ChannelKind::Wxpusher`] | 微信（WxPusher 公众号） | `app_token`, `topic_ids` |
//! | [`ChannelKind::Webhook`] | 任意 URL（JSON POST） | `url`, `template`, `headers` |
//! | [`ChannelKind::Serverchan`] | 微信服务号（Server 酱） | `send_key` |
//!
//! # 铁律：失败必须让调用方知道
//!
//! 原版早期实现把所有异常在 `send()` 内部吞掉，调用方照样写去重标记，
//! 结果「推送没发出去，但被标记为已处理」—— 这条头目**永远丢了**。
//!
//! 本层保证：
//! - 单渠道失败不影响其它渠道（各自独立成败）
//! - 只要有一个渠道送达，就算本轮成功
//! - 全挂时返回 [`NotifyError::AllFailed`]，调用方**不得**写去重标记

pub mod channels;
pub mod error;

pub use channels::{
    ChannelSender,
    send_serverchan, send_webhook, send_wxpusher, Channel, ChannelConfig, ChannelKind, Dispatcher,
    SendOutcome, SendReport,
};
pub use error::{NotifyError, NotifyResult};
