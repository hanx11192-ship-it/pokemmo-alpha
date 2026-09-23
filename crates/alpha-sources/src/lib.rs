//! 数据源适配器层。
//!
//! 每个源一个模块，统一实现 [`DataSource`]；主流程只认 [`FetchResult`]，
//! 所以加源不用改主流程 —— 这就是插件式设计的目的。
//!
//! # 目录
//!
//! | 模块 | 源 | 特征 |
//! |------|-----|------|
//! | [`alphapedia`] | tool.lzpoke.com `latest`/`slots` | 中英文字段齐全，英文优先 |
//! | [`lzpoke_reports`] | tool.lzpoke.com `reports` | 只有英文精灵名 + 中文技能 + 投票机制 |
//! | [`pokemmotools_landing`] | alpha.pokemmotools.org | HTML 抓取，两步（探活 + 图鉴页补技能） |
//! | [`neodex_alpha`] | neodex.tools GraphQL | 必须做「过期 + 时段」双验证 |
//! | [`lanbizi`] | pokemmo.lanbizi.com | 接口已下线，保留作适配器模板 |

pub mod alphapedia;
pub mod base;
pub mod error;
pub mod lanbizi;
pub mod location_map;
pub mod lzpoke_reports;
pub mod neodex_alpha;
pub mod pokemmotools_landing;
pub mod registry;

pub use base::{
    content_fingerprint, dedup_key, extract_reporter, parse_male_ratio, DataSource, HttpClient,
    NameKind, NameResolver,
};
pub use error::{SourceError, SourceResult};
pub use registry::{create_all, create_source, known_adapters, SourceHandle};

// 适配器实现
pub use alphapedia::AlphapediaSource;
pub use lanbizi::LanbiziSource;
pub use lzpoke_reports::LzpokeReportsSource;
pub use neodex_alpha::NeodexAlphaSource;
pub use pokemmotools_landing::PokemmotoolsLandingSource;
