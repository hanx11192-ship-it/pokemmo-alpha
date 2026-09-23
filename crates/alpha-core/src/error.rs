//! 统一错误类型。

use thiserror::Error;

pub type Result<T> = std::result::Result<T, CoreError>;

#[derive(Debug, Error)]
pub enum CoreError {
    #[error("IO 错误: {0}")]
    Io(String),

    #[error("解析错误: {0}")]
    Parse(String),

    #[error("配置错误: {0}")]
    Config(String),

    #[error("请求失败: {0}")]
    Http(String),

    #[error("网络错误: {0}")]
    Network(String),

    #[error("推送失败: {0}")]
    Notify(String),

    #[error("存储错误: {0}")]
    Store(String),

    #[error("认证失败: {0}")]
    Auth(String),

    #[error("未找到: {0}")]
    NotFound(String),

    #[error("插件错误: {0}")]
    Plugin(String),

    #[error("{0}")]
    Other(String),
}

impl CoreError {
    pub fn io(msg: impl Into<String>) -> Self {
        CoreError::Io(msg.into())
    }
    pub fn parse(msg: impl Into<String>) -> Self {
        CoreError::Parse(msg.into())
    }
    pub fn config(msg: impl Into<String>) -> Self {
        CoreError::Config(msg.into())
    }
    pub fn http(msg: impl Into<String>) -> Self {
        CoreError::Http(msg.into())
    }
    pub fn network(msg: impl Into<String>) -> Self {
        CoreError::Network(msg.into())
    }
    pub fn notify(msg: impl Into<String>) -> Self {
        CoreError::Notify(msg.into())
    }
    pub fn store(msg: impl Into<String>) -> Self {
        CoreError::Store(msg.into())
    }
    pub fn plugin(msg: impl Into<String>) -> Self {
        CoreError::Plugin(msg.into())
    }
    pub fn other(msg: impl Into<String>) -> Self {
        CoreError::Other(msg.into())
    }
}

impl From<std::io::Error> for CoreError {
    fn from(e: std::io::Error) -> Self {
        CoreError::Io(e.to_string())
    }
}

impl From<serde_json::Error> for CoreError {
    fn from(e: serde_json::Error) -> Self {
        CoreError::Parse(e.to_string())
    }
}

impl From<serde_yaml::Error> for CoreError {
    fn from(e: serde_yaml::Error) -> Self {
        CoreError::Parse(e.to_string())
    }
}
