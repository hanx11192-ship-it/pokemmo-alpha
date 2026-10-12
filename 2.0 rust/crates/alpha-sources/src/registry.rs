//! 适配器注册表。
//!
//! 按 `sources.yaml` 里的 `adapter` 名构造对应的适配器。
//! 加新源时在这里登记一行即可，主流程不用动。

use alpha_core::config::{Config, SourceConfig, SourceOptions};

use crate::base::DataSource;
use crate::error::SourceError;

/// 已构造好的源实例。
pub struct SourceHandle {
    pub config: SourceConfig,
    pub adapter: Box<dyn DataSource>,
    /// 该源的优先级（数字越小越优先）
    pub priority: i64,
}

/// 支持的适配器名列表（用于配置校验 / 报错提示）。
///
/// 顺序即 `sources.yaml` 里推荐的出现顺序，**不代表**优先级 ——
/// 优先级由每条配置自己的 `priority` 字段决定。
pub const KNOWN_ADAPTERS: &[&str] = &[
    "alphapedia",
    "lzpoke_reports",
    "pokemmotools_landing",
    "neodex_alpha",
    // 接口已 410 下线，保留作适配器模板
    "lanbizi",
];

/// 支持的适配器名列表。
pub fn known_adapters() -> &'static [&'static str] {
    KNOWN_ADAPTERS
}

/// 按配置构造单个源。
pub fn create_source(
    adapter: &str,
    options: SourceOptions,
    cfg: &Config,
) -> Result<Box<dyn DataSource>, SourceError> {
    match adapter {
        "alphapedia" => Ok(Box::new(crate::alphapedia::AlphapediaSource::new(
            options, cfg,
        )?)),
        "lzpoke_reports" => Ok(Box::new(crate::lzpoke_reports::LzpokeReportsSource::new(
            options, cfg,
        )?)),
        "pokemmotools_landing" => Ok(Box::new(
            crate::pokemmotools_landing::PokemmotoolsLandingSource::new(options, cfg)?,
        )),
        "neodex_alpha" => Ok(Box::new(crate::neodex_alpha::NeodexAlphaSource::new(
            options, cfg,
        )?)),
        "lanbizi" => Ok(Box::new(crate::lanbizi::LanbiziSource::new(
            options, cfg,
        )?)),
        other => Err(SourceError::Config(format!(
            "未知的数据源适配器 `{other}`（已支持: {}）",
            KNOWN_ADAPTERS.join(", ")
        ))),
    }
}

/// 构造配置里所有**已启用**的源。
///
/// 单个源构造失败不会拖垮整体 —— 记一条 WARN 后跳过，
/// 剩下的源照常工作（这是「多源冗余」的意义所在）。
pub fn create_all(cfg: &Config) -> Vec<SourceHandle> {
    let mut out = Vec::new();
    for scfg in cfg.enabled_sources() {
        match create_source(&scfg.adapter, scfg.options.clone(), cfg) {
            Ok(adapter) => {
                let priority = scfg.priority;
                out.push(SourceHandle {
                    config: scfg.clone(),
                    adapter,
                    priority,
                });
            }
            Err(e) => {
                tracing::warn!("数据源 {} 构造失败，已跳过: {e}", scfg.name);
            }
        }
    }
    out
}
