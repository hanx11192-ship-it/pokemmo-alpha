//! 存储层的错误类型。

/// 存储错误。
#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("数据库错误: {0}")]
    Sqlite(#[from] rusqlite::Error),

    #[error("序列化错误: {0}")]
    Json(#[from] serde_json::Error),

    #[error("{0}")]
    NotFound(String),

    #[error("{0}")]
    Conflict(String),

    #[error("{0}")]
    Config(String),
}

pub type StoreResult<T> = Result<T, StoreError>;

impl StoreError {
    /// 是否是「唯一约束冲突」（面板据此回 409 而不是 500）。
    ///
    /// 两种形态都要算：底层 `Sqlite` 约束错，以及已经被 [`StoreError::Conflict`]
    /// 包装过的。漏掉后者会导致「`add_user` 里被包装成 Conflict，出了这个函数
    /// 再问一次就说不冲突了」—— 面板于是把重名当成服务器错误。
    pub fn is_unique_violation(&self) -> bool {
        match self {
            StoreError::Conflict(_) => true,
            StoreError::Sqlite(rusqlite::Error::SqliteFailure(e, _)) => {
                e.code == rusqlite::ErrorCode::ConstraintViolation
            }
            _ => false,
        }
    }
}
