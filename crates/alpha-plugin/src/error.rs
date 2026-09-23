//! 插件层的错误类型。

/// 插件错误。
#[derive(Debug, thiserror::Error)]
pub enum PluginError {
    #[error("插件脚本语法/编译错误: {0}")]
    Compile(String),

    #[error("插件运行时错误: {0}")]
    Runtime(String),

    /// 脚本跑飞了（死循环）—— 必须能中断，否则一个 `loop {}` 就能卡死整个面板。
    #[error("插件执行超时（超过 {0} 条指令），已强制中断")]
    Timeout(u64),

    /// 脚本没实现必需的入口函数。
    #[error("插件缺少入口函数 `{0}`（请确认脚本里定义了 fn {0}(boss, ctx)）")]
    MissingEntry(&'static str),

    #[error("读取插件文件失败 {path}: {source}")]
    Io {
        path: String,
        #[source]
        source: std::io::Error,
    },

    #[error("{0}")]
    NotFound(String),

    #[error("{0}")]
    Rejected(String),
}

pub type PluginResult<T> = Result<T, PluginError>;

impl PluginError {
    /// 是否是「脚本本身写得不对」——面板据此提示用户改脚本，
    /// 而不是报「服务器内部错误」。
    pub fn is_user_fault(&self) -> bool {
        matches!(
            self,
            Self::Compile(_) | Self::MissingEntry(_) | Self::Runtime(_) | Self::Timeout(_)
        )
    }
}
