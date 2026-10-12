//! 配置加载。
//!
//! 所有路径相对项目根目录解析，不依赖运行时的 cwd。
//! 对应原版 `src/core/config.py` + `config/{settings,sources,rules}.yaml`。

use std::path::{Path, PathBuf};
use std::sync::Arc;

use once_cell::sync::OnceCell;
use serde::Deserialize;

use crate::error::{CoreError, Result};

/// 项目根目录。
///
/// 判定顺序：
/// 1. 环境变量 `ALPHA_ROOT`
/// 2. 从当前可执行文件位置向上找（含 `config/settings.yaml` 的目录）
/// 3. 当前工作目录
pub fn project_root() -> PathBuf {
    static ROOT: OnceCell<PathBuf> = OnceCell::new();
    ROOT.get_or_init(|| {
        if let Ok(p) = std::env::var("ALPHA_ROOT") {
            if !p.is_empty() {
                return PathBuf::from(p);
            }
        }
        if let Ok(exe) = std::env::current_exe() {
            let mut cur = exe.parent().map(Path::to_path_buf);
            while let Some(dir) = cur {
                if dir.join("config").join("settings.yaml").exists() {
                    return dir;
                }
                cur = dir.parent().map(Path::to_path_buf);
            }
        }
        let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        if cwd.join("config").join("settings.yaml").exists() {
            return cwd;
        }
        // 兜底：开发时的仓库根
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(|p| p.parent())
            .map(Path::to_path_buf)
            .unwrap_or(cwd)
    })
    .clone()
}

pub fn config_dir() -> PathBuf {
    project_root().join("config")
}

pub fn data_dir() -> PathBuf {
    project_root().join("data")
}

/// 把相对路径解析到项目根目录下。
pub fn abspath(parts: &[&str]) -> PathBuf {
    let mut p = project_root();
    for part in parts {
        p.push(part);
    }
    p
}

fn load_yaml<T: for<'de> Deserialize<'de>>(parts: &[&str]) -> Result<T> {
    let path = abspath(parts);
    let text = std::fs::read_to_string(&path)
        .map_err(|e| CoreError::config(format!("读取配置失败 {}: {e}", path.display())))?;
    serde_yaml::from_str(&text)
        .map_err(|e| CoreError::config(format!("解析配置失败 {}: {e}", path.display())))
}

// ---------------- settings.yaml ----------------

#[derive(Debug, Clone, Deserialize)]
pub struct DedupSettings {
    #[serde(default = "default_state_file")]
    pub state_file: String,
    /// 保留多少条已处理记录。
    #[serde(default = "default_keep")]
    pub keep: usize,
}

impl Default for DedupSettings {
    fn default() -> Self {
        Self {
            state_file: default_state_file(),
            keep: default_keep(),
        }
    }
}

fn default_state_file() -> String {
    "data/state.json".to_string()
}

fn default_keep() -> usize {
    500
}

#[derive(Debug, Clone, Deserialize)]
pub struct HttpSettings {
    #[serde(default = "default_timeout")]
    pub timeout: u64,
    #[serde(default = "default_connect_timeout")]
    pub connect_timeout: u64,
    #[serde(default = "default_retry_delays")]
    pub retry_delays: Vec<f64>,
    #[serde(default)]
    pub verify_ssl: bool,
    #[serde(default)]
    pub proxy: Option<String>,
}

impl Default for HttpSettings {
    fn default() -> Self {
        Self {
            timeout: default_timeout(),
            connect_timeout: default_connect_timeout(),
            retry_delays: default_retry_delays(),
            verify_ssl: false,
            proxy: None,
        }
    }
}

fn default_timeout() -> u64 {
    8
}
fn default_connect_timeout() -> u64 {
    5
}
fn default_retry_delays() -> Vec<f64> {
    vec![0.0, 0.5]
}

#[derive(Debug, Clone, Deserialize)]
pub struct WxPusherSettings {
    #[serde(default = "yes")]
    pub enabled: bool,
    #[serde(default = "default_token_env")]
    pub app_token_env: String,
    #[serde(default)]
    pub topic_ids: Vec<i64>,
    #[serde(default)]
    pub summary_from_slot_name: bool,
    #[serde(default = "default_summary")]
    pub fallback_summary: String,
}

impl Default for WxPusherSettings {
    fn default() -> Self {
        Self {
            enabled: yes(),
            app_token_env: default_token_env(),
            topic_ids: Vec::new(),
            summary_from_slot_name: true,
            fallback_summary: default_summary(),
        }
    }
}

fn yes() -> bool {
    true
}
fn default_token_env() -> String {
    "WXPUSHER_APP_TOKEN_alpha".to_string()
}
fn default_summary() -> String {
    "头目".to_string()
}

#[derive(Debug, Clone, Deserialize)]
pub struct NotifySettings {
    #[serde(default)]
    pub wxpusher: WxPusherSettings,
    #[serde(default = "default_on_failure")]
    pub on_failure: String,
}

impl Default for NotifySettings {
    fn default() -> Self {
        Self {
            wxpusher: WxPusherSettings::default(),
            on_failure: default_on_failure(),
        }
    }
}

fn default_on_failure() -> String {
    "raise".to_string()
}

#[derive(Debug, Clone, Deserialize)]
pub struct ConcurrencySettings {
    #[serde(default = "default_conc_timeout")]
    pub timeout: f64,
    #[serde(default = "default_workers")]
    pub max_workers: usize,
}

fn default_conc_timeout() -> f64 {
    15.0
}
fn default_workers() -> usize {
    8
}

impl Default for ConcurrencySettings {
    fn default() -> Self {
        Self {
            timeout: default_conc_timeout(),
            max_workers: default_workers(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct LoggingSettings {
    #[serde(default = "default_log_level")]
    pub level: String,
    #[serde(default)]
    pub file: Option<String>,
    #[serde(default = "default_max_bytes")]
    pub max_bytes: u64,
    #[serde(default = "default_backup_count")]
    pub backup_count: usize,
}

fn default_log_level() -> String {
    "INFO".to_string()
}
fn default_max_bytes() -> u64 {
    2_097_152
}
fn default_backup_count() -> usize {
    3
}

impl Default for LoggingSettings {
    fn default() -> Self {
        Self {
            level: default_log_level(),
            file: None,
            max_bytes: default_max_bytes(),
            backup_count: default_backup_count(),
        }
    }
}

/// `config/settings.yaml` 的结构。
#[derive(Debug, Clone, Deserialize)]
pub struct Settings {
    #[serde(default = "default_tz")]
    pub timezone: String,
    #[serde(default = "default_lang")]
    pub language: String,
    #[serde(default)]
    pub dedup: DedupSettings,
    #[serde(default)]
    pub http: HttpSettings,
    #[serde(default)]
    pub notify: NotifySettings,
    #[serde(default)]
    pub concurrency: ConcurrencySettings,
    #[serde(default)]
    pub logging: LoggingSettings,
}

fn default_tz() -> String {
    "Asia/Shanghai".to_string()
}
fn default_lang() -> String {
    "zh".to_string()
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            timezone: default_tz(),
            language: default_lang(),
            dedup: DedupSettings::default(),
            http: HttpSettings::default(),
            notify: NotifySettings::default(),
            concurrency: ConcurrencySettings::default(),
            logging: LoggingSettings::default(),
        }
    }
}

// ---------------- sources.yaml ----------------

#[derive(Debug, Clone, Deserialize, Default)]
pub struct ExtraLinesOpts {
    #[serde(default)]
    pub hms: bool,
    #[serde(default)]
    pub level: bool,
    #[serde(default)]
    pub vote: bool,
    #[serde(default)]
    pub location_notes: bool,
    #[serde(default)]
    pub notes: bool,
    #[serde(default)]
    pub tier: bool,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct SourceOptions {
    #[serde(default)]
    pub target: String,
    #[serde(default)]
    pub proxy: Option<String>,
    #[serde(default)]
    pub transform_url_env: Option<String>,
    #[serde(default)]
    pub transform_method: Option<String>,
    #[serde(default)]
    pub transform_headers: Option<serde_yaml::Value>,
    #[serde(default)]
    pub proxy_key_env: Option<String>,
    /// 源站要 Bearer 鉴权时，读哪个环境变量拿 key（key 不进配置文件）
    #[serde(default)]
    pub api_key_env: Option<String>,
    #[serde(default)]
    pub prefer_english_keys: Option<bool>,
    #[serde(default)]
    pub monster_id_divisor: Option<i64>,
    #[serde(default)]
    pub extra_lines: ExtraLinesOpts,
    /// 其余未识别字段原样保留
    #[serde(flatten)]
    pub rest: std::collections::HashMap<String, serde_yaml::Value>,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct SourceConfig {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub adapter: String,
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_priority")]
    pub priority: i64,
    #[serde(default)]
    pub options: SourceOptions,
    #[serde(default)]
    pub note: Option<String>,
}

fn default_priority() -> i64 {
    100
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct SourcesDoc {
    #[serde(default)]
    pub sources: Vec<SourceConfig>,
}

// ---------------- 全局配置 ----------------

/// 全局配置（三级缓存：settings / sources / rules 原文）。
#[derive(Debug, Clone)]
pub struct Config {
    pub settings: Settings,
    pub sources: SourcesDoc,
    /// rules.yaml 原文（由 alpha-strategy 的 Rules 解析）
    pub rules_raw: serde_yaml::Value,
}

impl Config {
    pub fn load() -> Result<Self> {
        let settings: Settings = load_yaml(&["config", "settings.yaml"])?;
        let sources: SourcesDoc = load_yaml(&["config", "sources.yaml"]).unwrap_or_default();
        let rules_raw: serde_yaml::Value =
            load_yaml(&["config", "rules.yaml"]).unwrap_or(serde_yaml::Value::Null);
        Ok(Self {
            settings,
            sources,
            rules_raw,
        })
    }

    /// zh / en / both
    pub fn language(&self) -> &str {
        &self.settings.language
    }

    /// 输出语言列表。
    pub fn languages(&self) -> Vec<String> {
        match self.settings.language.as_str() {
            "both" => vec!["zh".to_string(), "en".to_string()],
            other => vec![other.to_string()],
        }
    }

    /// 按 priority 升序返回已启用的源配置。
    pub fn enabled_sources(&self) -> Vec<&SourceConfig> {
        let mut v: Vec<&SourceConfig> = self.sources.sources.iter().filter(|s| s.enabled).collect();
        v.sort_by_key(|s| s.priority);
        v
    }

    /// 去重状态文件路径。
    pub fn state_path(&self) -> PathBuf {
        let p = &self.settings.dedup.state_file;
        let path = PathBuf::from(p);
        if path.is_absolute() {
            path
        } else {
            project_root().join(path)
        }
    }
}

/// 全局配置缓存。
///
/// # 为什么不是 `OnceCell`
///
/// 原版是 `functools.lru_cache` 包着 `get_config()`，并靠
/// `panel/config_mgr.py::reload_core_config()` 调 `cache_clear()` 清缓存 ——
/// 面板改完 `panel.env`（例如换了转发地址）之后要让新值**立即生效**，
/// 不等重启。
///
/// `OnceCell` 只能 `set` 一次、没有 `take()`，所以本版换成 `RwLock`。
/// 用 `RwLock<Option<Config>>` 而不是 `ArcSwap`，是因为配置读取本身
/// 不在超热路径上（每轮轮询读几次），省一个依赖更划算。
///
/// # 读路径的代价
///
/// [`get_config`] 返回 `Arc<Config>` 而不是 `&'static Config`。
/// 调用方大多只是读几个字段，`Arc` 克隆可忽略。
/// 值得注意的**不是**这点开销，而是：拿到 `Arc` 之后如果
/// [`reload_config`] 被调用，手上的 `Arc` 仍是旧配置 ——
/// 这是正确的（一次请求内配置应当稳定），但别把 `Arc` 存进
/// 长生命周期的结构里。
static CONFIG: std::sync::RwLock<Option<Arc<Config>>> = std::sync::RwLock::new(None);

/// 取全局配置（首次调用时加载）。
pub fn get_config() -> Result<Arc<Config>> {
    if let Some(cfg) = CONFIG.read().expect("config lock poisoned").as_ref() {
        return Ok(Arc::clone(cfg));
    }
    let cfg = Arc::new(Config::load()?);
    let mut w = CONFIG.write().expect("config lock poisoned");
    // 双重检查：两个线程可能同时走到这里，先拿锁的那个写进去，
    // 后拿锁的用已有的（丢弃自己刚加载的那份，不覆盖）
    let slot = w.get_or_insert_with(|| Arc::clone(&cfg));
    Ok(Arc::clone(slot))
}

/// 清掉配置缓存，让下一次 [`get_config`] 重新读盘。
///
/// 对应原版 `panel/config_mgr.py::reload_core_config()`。
///
/// # 与原版的语义差异
///
/// 原版那个函数在缓存清理失败时返回 `False`，面板据此在响应里回
/// `restarted: false`。本版**总是**清得掉（就是个 `Option::take`），
/// 所以调用方拿到的永远是「已重载」。原版那个 `False` 分支实际是
/// 「Python 内部结构找不到」的兜底，在本版没有对应物。
///
/// 返回是否真的清掉了一份缓存（之前有没有加载过）。
pub fn reload_config() -> bool {
    let mut w = CONFIG.write().expect("config lock poisoned");
    w.take().is_some()
}

/// 强制重新加载并写回缓存，返回新配置。
pub fn refresh_config() -> Result<Arc<Config>> {
    let cfg = Arc::new(Config::load()?);
    let mut w = CONFIG.write().expect("config lock poisoned");
    *w = Some(Arc::clone(&cfg));
    Ok(cfg)
}

/// 仅加载 settings.yaml（不依赖全局缓存，供热更新使用）。
pub fn load_settings_fresh() -> Result<Settings> {
    load_yaml(&["config", "settings.yaml"])
}

/// 仅加载 sources.yaml（不依赖全局缓存）。
pub fn load_sources_fresh() -> Result<SourcesDoc> {
    load_yaml(&["config", "sources.yaml"])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn root_is_repo() {
        let root = project_root();
        assert!(root.join("config").exists() || root.join("crates").exists());
    }

    #[test]
    fn defaults_are_sane() {
        let s = Settings::default();
        assert_eq!(s.timezone, "Asia/Shanghai");
        assert_eq!(s.language, "zh");
        assert_eq!(s.http.timeout, 8);
        assert_eq!(s.concurrency.timeout, 15.0);
        assert_eq!(s.dedup.keep, 500);
    }
}
