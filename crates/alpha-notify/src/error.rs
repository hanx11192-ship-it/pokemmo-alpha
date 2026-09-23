//! 推送层的错误类型。
//!
//! 分清楚「哪种失败」直接决定调用方该不该写去重标记：
//! 只有 [`NotifyError::AllFailed`] 和网络/配置类错误才意味着「真的没送出去」。

/// 推送错误。
#[derive(Debug, thiserror::Error)]
pub enum NotifyError {
    /// 本地配置问题（缺 token / 缺 URL）。
    #[error("{0}")]
    Config(String),

    /// 网络层失败（超时、连不上、HTTP 非 2xx）。
    #[error("{0}")]
    Network(String),

    /// 响应不是预期的 JSON。
    #[error("{0}")]
    Parse(String),

    /// 服务端返回 HTTP 200 但业务码表示失败。
    #[error("{0}")]
    Business(String),

    /// 没有可用的推送渠道。
    #[error("{0}")]
    NoChannel(String),

    /// 所有渠道都失败了 —— 调用方**不得**写去重标记。
    #[error("所有渠道推送均失败：{0}")]
    AllFailed(String),
}

pub type NotifyResult<T> = Result<T, NotifyError>;
