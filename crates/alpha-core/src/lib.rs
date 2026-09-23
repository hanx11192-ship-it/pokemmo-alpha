//! `alpha-core`：领域模型、图鉴、规则、去重、配置、时间。
//!
//! 这一层**不含任何 IO 副作用**（除配置/图鉴的文件读取），
//! 是 `alpha-strategy`、`alpha-sources`、`alpha-server` 共同的地基。

pub mod config;
pub mod config_mgr;
pub mod dedup;
pub mod error;
pub mod models;
pub mod pokedex;
pub mod time;

pub use error::{CoreError, Result};
pub use models::{BossData, ExtraLine, FetchResult, FetchStatus, Gender};
pub use pokedex::{get_pokedex, norm, Pokedex};
