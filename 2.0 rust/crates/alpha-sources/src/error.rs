//! 数据源层的错误类型。
//!
//! 分清楚「谁的问题」很重要：
//! - [`SourceError::Http`] —— 源站拒绝（4xx），改配置才能解决，重试无意义
//! - [`SourceError::Request`] —— 网络层失败，重试可能有用
//! - [`SourceError::Parse`] —— 源站改了格式，得改适配器
//! - [`SourceError::Config`] —— 本地配置写错了

/// 数据源错误。
#[derive(Debug, thiserror::Error)]
pub enum SourceError {
    /// 源站返回 4xx。
    #[error("HTTP {code} {hint}: {url}")]
    Http {
        code: u16,
        url: String,
        hint: &'static str,
    },

    /// 网络层失败（超时 / 连不上 / 重试耗尽）。
    #[error("{0}")]
    Request(String),

    /// 响应格式与预期不符。
    #[error("{0}")]
    Parse(String),

    /// 本地配置有问题。
    #[error("{0}")]
    Config(String),
}

pub type SourceResult<T> = Result<T, SourceError>;
