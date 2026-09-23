//! 一轮轮询（`poll_once`）。
//!
//! 这是调度器的心脏，也是行为最容易跑偏的地方。五种返回状态：
//!
//! | status | 触发条件 | 副作用 |
//! |---|---|---|
//! | `paused` | 处于冷却中且非强制 | 无 |
//! | `empty` | 所有源都没报有效头目 | 写 `last_run` / `last_result` |
//! | `hit`(debug) | `debug=True` | 记一条 debug 日志，**不推送、不去重、不暂停** |
//! | `deduped` | 本时段已报过同一只头目 | 写 `current_slot`，**仍会触发自动暂停** |
//! | `notify_error` | 推送抛异常 | **不写去重标记**（下轮重试）、**不暂停** |
//! | `pushed` | 推送成功 | 写去重标记 + `current_slot` + 自动暂停 |
//!
//! # 三个非直觉的顺序细节
//!
//! ## 1. 去重检查在推送**之前**，但写标记在推送**之后**
//!
//! 顺序是「先查已报过 → 再推 → 推成功才写标记」。所以推送失败的那一轮
//! **不会**留下标记，下一轮会重试。如果把标记写在推送前，
//! 一次网络抖动就等于**永久漏报**这一只头目。
//!
//! ## 2. `deduped` 分支**依然**会触发自动暂停
//!
//! 原版在 dedup 分支里照样调 `_maybe_pause_after_detect(boss)`。
//! 初看很怪（都没推，暂停什么），但这正是想要的语义：
//! 「这个时段已经有头目了，别再打源站了」。少了这一步，
//! 面板重启后重新探测到同一只头目会一直轮询到时段结束。
//!
//! ## 3. `notify_error` 分支**不**暂停
//!
//! 反过来，推送失败时**不**暂停 —— 因为没推出去，还得接着试。
//!
//! # 强制检查会先清冷却
//!
//! `force_check_once()` 不是「绕过 `paused` 判断」，而是
//! **先 `clear_monitor_pause()` 再 `poll_once(force=True)`**。
//! 两者的区别在于副作用：只绕过判断的话，手动检查完仍处于冷却中，
//! 下一轮自动轮询还是不动。原版是清掉冷却，这里照做。

use std::sync::Arc;

use alpha_core::config::Config;
use alpha_core::dedup::{global_dedup_key, DedupStore};
use alpha_core::models::{BossData, FetchStatus};
use alpha_core::pokedex::Pokedex;
use alpha_core::time::{now_str, slot_name_en};
use alpha_plugin::PluginRegistry;
use alpha_store::Store;
use serde_json::json;

use crate::fetch::{fetch_all, reconcile_missing};
use crate::pause::{clear_pause, is_paused, maybe_pause_after_detect, monitor_state};
use crate::report::build_report;
use crate::vote::resolve_by_vote;
use crate::SchedulerError;

/// 一轮的结果。
#[derive(Debug, Clone)]
pub struct PollOutcome {
    /// `paused` / `empty` / `deduped` / `notify_error` / `pushed` / `hit`(debug)
    pub status: &'static str,
    pub message: String,
    /// 命中时的报文（仅 `pushed` / `hit`）
    pub report: Option<String>,
    /// 命中时的时段信息（`pushed` / `deduped` / `hit`）
    pub slot: Option<serde_json::Value>,
    /// 推送渠道结果（仅 `pushed`）
    pub channels: Option<serde_json::Value>,
    /// debug 命中的解析详情（仅 `hit`）
    pub resolved: Option<serde_json::Value>,
    /// 本轮耗时（秒）
    pub elapsed: f64,
}

impl PollOutcome {
    /// 是否推进了「本轮有结果」这件事（供日志与测试判断）。
    pub fn is_hit(&self) -> bool {
        matches!(self.status, "pushed" | "hit")
    }
}

/// 源工厂：把配置变成可跑的源实例。
///
/// 抽出来是为了**可测性**。生产用的是 `alpha_sources::create_all`
/// （真适配器、真 HTTP），但调度器自己的逻辑 —— 超时、投票、去重、
/// 推送、暂停 —— 不该靠打真实源站来测。测试里换成一个返回假源的工厂，
/// 一轮 `poll_once` 就是纯内存操作，毫秒级完成。
///
/// 用 `Box<dyn Fn>` 而不是泛型：`Deps` 会出现在 `Scheduler` 的字段里，
/// 泛型化会把类型参数传染到所有调用点。
pub type SourceFactory = Box<dyn Fn(&Config) -> Vec<alpha_sources::SourceHandle> + Send + Sync>;

/// 生产用的源工厂。
pub fn real_sources() -> SourceFactory {
    Box::new(|cfg: &Config| alpha_sources::create_all(cfg))
}

/// 推送器：把「推送一轮」这件事抽象掉。
///
/// 同样是为了可测。生产用的是 [`alpha_notify::Dispatcher::send_all`]
/// （真 HTTP），测试里换成一个可控的假实现，就能稳定地构造
/// 「推送成功」与「推送失败」两条路径 —— 这两条路径对应
/// `pushed` 与 `notify_error` 两种状态，且**分支副作用完全不同**
/// （前者写去重标记，后者不写），必须都能测到。
///
/// 异步 trait 用 `async_trait` 保持与项目其它地方一致。
#[async_trait::async_trait]
pub trait Notifier: Send + Sync {
    /// 推一轮。`Err` 表示整轮推送失败（会走 `notify_error` 分支）。
    async fn send(
        &self,
        channels: &[alpha_notify::Channel],
        content: &str,
        summary: &str,
    ) -> Result<alpha_notify::SendReport, alpha_notify::NotifyError>;
}

/// 生产推送器。
///
/// # 为什么带着 `Store`
///
/// 原版 `panel/channels.py` 的 `send_all` **逐渠道**写一条日志：
///
/// ```text
/// 成功 → info,  kind="channel", source=<渠道类型>, "推送成功：<渠道名>"
/// 失败 → error, kind="channel", source=<渠道类型>, "推送失败：<渠道名> - <原因>"
/// ```
///
/// `alpha-notify` 那一层不依赖 `alpha-store`（它是纯 HTTP + 模板，
/// 不该知道数据库的存在），所以日志只能由**装配方**补 ——
/// 而 `RealNotifier` 正好是「推送」与「存储」交汇的那一点。
///
/// 面板的「日志」页是用户排查推送问题的**唯一**入口，
/// 缺了这几条就只能去翻 stderr。
pub struct RealNotifier {
    pub dispatcher: Arc<alpha_notify::Dispatcher>,
    pub store: Store,
}

impl RealNotifier {
    pub fn new(dispatcher: Arc<alpha_notify::Dispatcher>, store: Store) -> Self {
        Self { dispatcher, store }
    }
}

#[async_trait::async_trait]
impl Notifier for RealNotifier {
    async fn send(
        &self,
        channels: &[alpha_notify::Channel],
        content: &str,
        summary: &str,
    ) -> Result<alpha_notify::SendReport, alpha_notify::NotifyError> {
        let result = self.dispatcher.send_all(channels, content, summary).await;

        match &result {
            Ok(report) => {
                for d in &report.details {
                    self.store.log(
                        if d.ok {
                            alpha_store::LogLevel::Info
                        } else {
                            alpha_store::LogLevel::Error
                        },
                        "channel",
                        &if d.ok {
                            format!("推送成功：{}", d.name)
                        } else {
                            format!(
                                "推送失败：{} - {}",
                                d.name,
                                d.error.as_deref().unwrap_or("未知错误")
                            )
                        },
                        d.kind.as_str(),
                    );
                }

                // 走环境变量兜底时 `details` 里是那条虚拟渠道，
                // 上面那个循环已经写过了，这里不重复记。
            }
            Err(e) => {
                // `AllFailed` 时 per-channel 细节在错误消息里（用「；」拼的），
                // `send_all` 内部已经 `tracing::warn!` 过。
                // 这里补一条汇总，别让面板日志页只剩空白。
                self.store.log(
                    alpha_store::LogLevel::Error,
                    "channel",
                    &format!("推送失败: {e}"),
                    "scheduler",
                );
            }
        }

        result
    }
}

/// 调度器跑一轮所需的全部外部依赖。
///
/// 打包成一个结构体而不是七个参数，是为了让 `poll_once` 的调用点
/// （HTTP handler、后台循环、调试任务）不必各自重复拼装。
pub struct Deps<'a> {
    pub store: &'a Store,
    pub config: Arc<Config>,
    pub pokedex: &'a Pokedex,
    pub registry: &'a PluginRegistry,
    pub dedup: &'a DedupStore,
    pub notify: &'a dyn Notifier,
    /// 源工厂；生产环境用 [`real_sources`]
    pub sources: &'a SourceFactory,
}

// `Deps` 里有闭包与 trait object，没法 derive Debug；手写一个只打关键字段的
// 版本，免得 `Scheduler` 也没法 Debug
impl std::fmt::Debug for Deps<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Deps")
            .field("config", &"<Config>")
            .field("pokedex", &"<Pokedex>")
            .finish_non_exhaustive()
    }
}

/// 评估器工厂：把「当前激活的评估器」包成引擎要的回调。
fn load_rules(config: &Config, pokedex: &Pokedex) -> Result<alpha_strategy::Rules, SchedulerError> {
    let doc: alpha_strategy::RulesDoc = serde_yaml::from_value(config.rules_raw.clone())
        .map_err(|e| SchedulerError::Config(format!("规则结构不合法: {e}")))?;
    Ok(alpha_strategy::Rules::new(doc, pokedex))
}

/// 跑一轮。
pub async fn poll_once(deps: &Deps<'_>, debug: bool, force: bool) -> PollOutcome {
    let store = deps.store;

    // ---- 0) 冷却检查 ----
    if !force && is_paused(store) {
        let st = monitor_state(store);
        let when = if st.pause_until > 0 {
            st.pause_until_str
        } else {
            "?".to_string()
        };
        tracing::info!("监控冷却中…（预计 {when} 恢复，可手动强制检查）");
        return PollOutcome {
            status: "paused",
            message: "监控冷却中（抓到头目后暂停对源站轮询），可手动强制检查".into(),
            report: None,
            slot: None,
            channels: None,
            resolved: None,
            elapsed: 0.0,
        };
    }

    // ---- 1) 准备 ----
    let sources = deps.config.enabled_sources();
    tracing::info!("轮询开始，启用源 {} 个", sources.len());

    let conc = &deps.config.settings.concurrency;
    let timeout = conc.timeout;
    let _max_workers = conc.max_workers;

    // ---- 2) 拉源（整批共用一个超时预算）----
    let handles = (deps.sources)(&deps.config);
    let mut round = fetch_all(handles, timeout).await;
    // 工厂会静默跳过构造失败的源，这里补回 error 结果，
    // 否则面板上那个源会凭空消失
    reconcile_missing(&sources, &mut round.results);
    let elapsed = round.elapsed;

    // ---- 3) 记日志 ----
    for (scfg, res) in &round.results {
        tracing::info!("{} -> {} ({})", scfg.name, res.status.as_str(), res.message);
    }
    log_queries(
        store,
        &round.results,
        elapsed,
        &round.timed_out,
    );
    store.set_kv("last_run", &now_str()).ok();

    // ---- 4) 无有效头目 ----
    let hits = round.hits();
    if hits.is_empty() {
        let _ = store.set_kv(
            "last_result",
            &json!({"status": "empty", "time": now_str(), "elapsed": elapsed}).to_string(),
        );
        return PollOutcome {
            status: "empty",
            message: "所有数据源都没有有效头目".into(),
            report: None,
            slot: None,
            channels: None,
            resolved: None,
            elapsed,
        };
    }

    // ---- 5) 投票仲裁 ----
    let priority_map = round.priority_map();
    let Some(chosen) = resolve_by_vote(&hits, &priority_map) else {
        // hits 非空而投票给出 None 是不可能的；真发生了说明上面的过滤逻辑坏了
        tracing::error!("投票仲裁未能选出结果，但 hits 非空 —— 逻辑异常");
        return PollOutcome {
            status: "empty",
            message: "投票仲裁失败".into(),
            report: None,
            slot: None,
            channels: None,
            resolved: None,
            elapsed,
        };
    };
    let boss = chosen
        .result
        .boss
        .clone()
        .expect("hits() 只保留带 boss 的结果");

    // ---- 6) 生成报告 ----
    let langs = deps.config.languages();
    let rules = match load_rules(&deps.config, deps.pokedex) {
        Ok(r) => r,
        Err(e) => {
            tracing::error!("规则加载失败: {e}");
            return PollOutcome {
                status: "notify_error",
                message: format!("规则加载失败: {e}"),
                report: None,
                slot: None,
                channels: None,
                resolved: None,
                elapsed,
            };
        }
    };
    let built = build_report(
        store,
        deps.registry,
        &boss,
        &rules,
        deps.pokedex,
        &langs,
    );

    // `summary` 是推送给渠道的一行摘要。英文模式下用英文时段名。
    //
    // `slot_name` / `slot_name_en` 是普通 `String`（不是 `Option`）——
    // 数据源没填就是空串。空串时分别兜底成 `"头目"` / `"Alpha"`，
    // 对应原版 `chosen.slot_name or "头目"`。
    let summary = if langs.first().map(|s| s.as_str()) == Some("en") {
        if chosen.result.slot_name_en.is_empty() {
            "Alpha".to_string()
        } else {
            chosen.result.slot_name_en.clone()
        }
    } else if chosen.result.slot_name.is_empty() {
        "头目".to_string()
    } else {
        chosen.result.slot_name.clone()
    };

    let slot_info = json!({
        "slot": chosen.result.slot_name.clone(),
        "slot_en": chosen.result.slot_name_en.clone(),
        "boss": boss.name,
        "source": chosen.source_name,
        "dispatcher": built.dispatcher,
        "time": now_str(),
        "reported": false,
    });

    // ---- 7) debug 模式：只探测，不推送、不去重、不暂停 ----
    if debug {
        store
            .log(
                alpha_store::LogLevel::Info,
                "debug",
                &format!(
                    "[DEBUG] 探测到 {}({}) 源={} 分发器={}",
                    boss.name,
                    chosen.result.slot_name.clone(),
                    chosen.source_name,
                    built.dispatcher
                ),
                "scheduler",
            );
        return PollOutcome {
            status: "hit",
            message: "调试命中（未推送）".into(),
            report: Some(built.report),
            resolved: Some(json!({
                "name": boss.name,
                "ability": boss.ability,
                "moves": boss.moves,
                "slot": chosen.result.slot_name.clone(),
                "source": chosen.source_name,
                "dispatcher": built.dispatcher,
            })),
            slot: None,
            channels: None,
            elapsed,
        };
    }

    // ---- 8) 去重 ----
    let dedup_key = global_dedup_key(Some(&boss));
    if !dedup_key.is_empty() && deps.dedup.is_processed(&dedup_key) {
        let mut slot = slot_info.clone();
        slot["reported"] = json!(true);
        let _ = crate::state::set_current_slot(store, &slot);
        store
            .log(
                alpha_store::LogLevel::Info,
                "scheduler",
                &format!(
                    "本时段已报点，跳过重复推送：{}({}) 源={}",
                    boss.name,
                    chosen.result.slot_name.clone(),
                    chosen.source_name
                ),
                "scheduler",
            );
        let _ = store.set_kv(
            "last_result",
            &json!({
                "status": "deduped",
                "boss": boss.name,
                "slot": chosen.result.slot_name.clone(),
                "source": chosen.source_name,
                "time": now_str(),
            })
            .to_string(),
        );
        // 注意：dedup 分支**仍然**触发暂停 —— 「这个时段已经有头目了，
        // 别再打源站了」
        if let Err(e) = maybe_pause_after_detect(store) {
            tracing::warn!("自动暂停设置失败: {e}");
        }
        return PollOutcome {
            status: "deduped",
            message: "本时段已报点，已跳过".into(),
            report: None,
            slot: Some(slot),
            channels: None,
            resolved: None,
            elapsed,
        };
    }

    // ---- 9) 推送 ----
    // 渠道列表从库里取（面板可随时改）；取不到当作「没有渠道」，
    // 让 `send_all` 走它自己的兜底/报错路径，而不是在这里造一个假渠道
    let channels = store.list_channels().unwrap_or_default();
    let send_res = match deps.notify.send(&channels, &built.report, &summary).await {
        Ok(r) => r,
        Err(e) => {
            tracing::error!("推送失败，本轮不写去重标记，下轮重试: {e}");
            store
                .log(
                    alpha_store::LogLevel::Error,
                    "scheduler",
                    &format!("推送失败: {e}"),
                    "scheduler",
                );
            return PollOutcome {
                status: "notify_error",
                message: format!("推送失败: {e}"),
                report: None,
                slot: None,
                channels: None,
                resolved: None,
                elapsed,
            };
        }
    };

    // ---- 10) 成功：写标记 + 暂停 ----
    // `mark()` 内部已经做了原子保存（写 .tmp 再 rename），不要再单独调 save()
    if let Err(e) = deps.dedup.mark(&dedup_key) {
        tracing::warn!("去重标记写入失败: {e}");
    }
    if let Err(e) = maybe_pause_after_detect(store) {
        tracing::warn!("自动暂停设置失败: {e}");
    }

    let mut slot = slot_info.clone();
    slot["reported"] = json!(true);
    let _ = crate::state::set_current_slot(store, &slot);

    let excerpt = built
        .report
        .chars()
        .take(200)
        .collect::<String>()
        .replace('\n', " ");
    store
        .log(
            alpha_store::LogLevel::Info,
            "spawn",
            &format!(
                "{} 爆点：{} · 源={} · 分发器={} · {}",
                chosen.result.slot_name.clone(),
                boss.name,
                chosen.source_name,
                built.dispatcher,
                excerpt
            ),
            &chosen.source_name,
        );

    let channels = json!({
        "sent": send_res.sent,
        "failed": send_res.failed,
    });
    let _ = store.set_kv(
        "last_result",
        &json!({
            "status": "pushed",
            "boss": boss.name,
            "slot": chosen.result.slot_name.clone(),
            "source": chosen.source_name,
            "dispatcher": built.dispatcher,
            "time": now_str(),
            "channels": channels,
        })
        .to_string(),
    );

    PollOutcome {
        status: "pushed",
        message: "已推送".into(),
        report: Some(built.report),
        slot: Some(slot),
        channels: Some(channels),
        resolved: None,
        elapsed,
    }
}

/// 写查询日志。
///
/// 原版 `_log_queries` 的规则：
/// - `query_log` 关了的时候，**只跳过 `empty`**（`hit` 和 `error` 照记）；
/// - 级别：`error` → error，`hit`/`empty` → info，其余 → warning。
fn log_queries(
    store: &Store,
    results: &[(alpha_core::config::SourceConfig, alpha_core::models::FetchResult)],
    elapsed: f64,
    timed_out: &[String],
) {
    let verbose = store.get_kv_or("query_log", "1") == "1";

    for (scfg, res) in results {
        let st = res.status;
        if !verbose && st == FetchStatus::Empty {
            continue;
        }
        let mut line = format!("源[{}] 返回 {}", scfg.name, st.as_str());
        if let Some(b) = &res.boss {
            line.push_str(&format!(" · 头目={}", b.name));
        }
        if !res.message.is_empty() {
            line.push_str(&format!(" · {}", res.message));
        }
        let level = match st {
            FetchStatus::Error => alpha_store::LogLevel::Error,
            FetchStatus::Hit | FetchStatus::Empty => alpha_store::LogLevel::Info,
        };
        store.log(level, "query", &line, &scfg.name);
    }

    for name in timed_out {
        store
            .log(
                alpha_store::LogLevel::Error,
                "query",
                &format!("源[{name}] 本轮超时（整轮耗时 {elapsed}s）"),
                name,
            );
    }
}

/// 强制检查一轮（先清冷却）。
pub async fn force_check_once(deps: &Deps<'_>) -> Result<PollOutcome, SchedulerError> {
    clear_pause(deps.store)?;
    Ok(poll_once(deps, false, true).await)
}

/// 时段英文名（供报文摘要用）。
pub fn slot_en(zh: &str) -> &'static str {
    slot_name_en(zh)
}

/// 头目是否在「本时段」—— 供调试面板展示。
pub fn boss_of(boss: Option<&BossData>) -> String {
    boss.map(|b| b.name.clone()).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn boss_of_handles_none() {
        assert_eq!(boss_of(None), "");
        assert_eq!(
            boss_of(Some(&BossData {
                name: "甲".into(),
                ..Default::default()
            })),
            "甲"
        );
    }

    #[test]
    fn only_pushed_and_debug_hit_count_as_hits() {
        for (status, want) in [
            ("pushed", true),
            ("hit", true),
            ("empty", false),
            ("paused", false),
            ("deduped", false),
            ("notify_error", false),
        ] {
            let o = PollOutcome {
                status,
                message: String::new(),
                report: None,
                slot: None,
                channels: None,
                resolved: None,
                elapsed: 0.0,
            };
            assert_eq!(o.is_hit(), want, "status={status}");
        }
    }
}

#[cfg(test)]
mod notifier_logging_tests {
    use super::*;
    use alpha_notify::{Channel, ChannelKind};

    fn channel(name: &str, kind: ChannelKind) -> Channel {
        Channel {
            id: Some(1),
            name: name.into(),
            kind,
            enabled: true,
            // 空的配置：`send_key` / `url` 都缺，各渠道会各自报「未配置」，
            // 稳定得到一个可断言的失败，不需要任何网络桩。
            config: Default::default(),
        }
    }

    fn logs(store: &Store) -> Vec<alpha_store::LogRow> {
        store.list_logs(&alpha_store::LogFilter::new(50)).unwrap()
    }

    /// **核心断言**：每个渠道不论成败都写一条 `kind="channel"` 的日志。
    ///
    /// 这条对齐原版 `panel/channels.py` 的 `send_all`：
    /// 成功写 `info`「推送成功：<名>」、失败写 `error`「推送失败：<名> - <原因>」，
    /// 且 `source` 是**渠道类型**。
    ///
    /// 为什么要在 `RealNotifier` 这一层测：`alpha-notify` 不依赖
    /// `alpha-store`，日志只能由装配方补。少了这段，
    /// 面板日志页在推送出问题时**一片空白** —— 用户唯一的排查入口没了。
    ///
    /// # 这里只测失败路径
    ///
    /// 成功路径要真的发出 HTTP 请求。用一个指向 `127.0.0.1:1` 的
    /// 假渠道就能稳定拿到「连接被拒」这种失败，不需要任何网络桩 ——
    /// 而未配置的渠道类型会走各自分支，同样能覆盖到日志写入。
    #[tokio::test]
    async fn real_notifier_logs_every_channel_outcome() {
        let store = Store::open_in_memory().unwrap();
        store.init().unwrap();

        let dispatcher = Arc::new(
            // 关掉环境变量兜底：没有渠道时直接报错，别去读环境
            alpha_notify::Dispatcher::new(false, Vec::new()).unwrap(),
        );
        let n = RealNotifier::new(dispatcher, store.clone());

        // 一个必然失败的渠道：Server 酱的 send_key 是空的
        let chs = vec![channel("坏掉的渠道", ChannelKind::Serverchan)];
        let result = n.send(&chs, "正文", "摘要").await;

        assert!(result.is_err(), "全失败时该返回 Err");
        let got = logs(&store);
        let channel_logs: Vec<_> = got.iter().filter(|l| l.kind == "channel").collect();
        assert_eq!(
            channel_logs.len(),
            1,
            "每个渠道一条日志，实际 {:?}",
            channel_logs.iter().map(|l| &l.message).collect::<Vec<_>>()
        );
        assert_eq!(channel_logs[0].level, "error", "失败该是 error 级别");
        assert!(
            channel_logs[0].message.contains("推送失败"),
            "文案该是「推送失败：<名> - <原因>」：{}",
            channel_logs[0].message
        );
        assert!(
            channel_logs[0].message.contains("坏掉的渠道"),
            "日志里该有渠道名：{}",
            channel_logs[0].message
        );
    }

    /// 没有启用渠道时（且不允许兜底）—— 也要在日志里留下痕迹。
    ///
    /// 原版这条路径是回退到环境变量、`details` 里放一条虚拟渠道。
    /// 本版关掉兜底时直接报错，那就该有一条汇总日志 ——
    /// 否则「明明有轮询但没推出去」在面板上完全看不出来。
    #[tokio::test]
    async fn real_notifier_logs_a_summary_when_there_is_nothing_to_send() {
        let store = Store::open_in_memory().unwrap();
        store.init().unwrap();
        let dispatcher =
            Arc::new(alpha_notify::Dispatcher::new(false, Vec::new()).unwrap());
        let n = RealNotifier::new(dispatcher, store.clone());

        let result = n.send(&[], "正文", "摘要").await;
        assert!(result.is_err());

        let got = logs(&store);
        assert!(
            got.iter().any(|l| l.kind == "channel"),
            "没有可用渠道时也该留一条日志，实际：{got:?}"
        );
    }
}
