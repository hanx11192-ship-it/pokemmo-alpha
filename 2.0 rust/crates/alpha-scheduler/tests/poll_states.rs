//! `poll_once` 五种返回状态的端到端测试。
//!
//! 这些测试**不打网络**：源工厂与推送器都注入假实现，
//! 一轮 `poll_once` 是纯内存操作。
//!
//! 覆盖的状态：
//!
//! | 测试 | 状态 |
//! |---|---|
//! | [`paused_when_within_the_cooldown_window`] | `paused` |
//! | [`force_bypasses_the_cooldown`] | 强制检查绕过 `paused` |
//! | [`empty_when_no_source_reports_a_boss`] | `empty` |
//! | [`debug_reports_a_hit_without_pushing`] | `hit`(debug) |
//! | [`a_second_detect_in_the_same_slot_is_deduped`] | `deduped` |
//! | [`a_failed_push_does_not_mark_dedup`] | `notify_error` + 不写标记 |
//! | [`a_successful_push_marks_dedup_and_pauses`] | `pushed` + 写标记 + 暂停 |

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use alpha_core::config::SourceConfig;
use alpha_core::models::{BossData, FetchResult};
use alpha_scheduler::pause::{set_auto_pause, set_pause_minutes};
use alpha_scheduler::poll::{poll_once, Deps, Notifier, SourceFactory};
use alpha_sources::{DataSource, SourceHandle};

// ---------------------------------------------------------------- 假件

/// 一个可编程的假源：返回预设的 `FetchResult`。
struct FakeSource {
    name: &'static str,
    result: Mutex<Option<FetchResult>>,
}

#[async_trait::async_trait]
impl DataSource for FakeSource {
    fn name(&self) -> &'static str {
        self.name
    }
    async fn fetch(&self) -> FetchResult {
        self.result
            .lock()
            .unwrap()
            .take()
            .unwrap_or_else(|| FetchResult::empty("已取完"))
    }
}

/// 构造一个「一定命中」的假源。
fn hit_source(name: &'static str, boss: &str, reported_at: &str) -> SourceHandle {
    let result = FetchResult {
        status: alpha_core::models::FetchStatus::Hit,
        boss: Some(BossData {
            name: boss.to_string(),
            ability: "无关特性".into(),
            moves: vec!["撞击".into()],
            reported_at: reported_at.to_string(),
            ..Default::default()
        }),
        dedup_key: String::new(),
        slot_name: "午头".into(),
        slot_name_en: "Afternoon Alpha".into(),
        message: String::new(),
    };
    SourceHandle {
        config: SourceConfig {
            name: name.to_string(),
            adapter: "fake".into(),
            enabled: true,
            priority: 100,
            options: Default::default(),
            note: None,
        },
        adapter: Box::new(FakeSource {
            name,
            result: Mutex::new(Some(result)),
        }),
        priority: 100,
    }
}

/// 构造一个「什么都没抓到」的假源。
fn empty_source(name: &'static str) -> SourceHandle {
    SourceHandle {
        config: SourceConfig {
            name: name.to_string(),
            adapter: "fake".into(),
            enabled: true,
            priority: 100,
            options: Default::default(),
            note: None,
        },
        adapter: Box::new(FakeSource {
            name,
            result: Mutex::new(Some(FetchResult::empty("没有头目"))),
        }),
        priority: 100,
    }
}

/// 可编程的假推送器。
struct FakeNotifier {
    fail: bool,
    calls: AtomicUsize,
    last_content: Mutex<String>,
}

impl FakeNotifier {
    fn ok() -> Arc<Self> {
        Arc::new(Self {
            fail: false,
            calls: AtomicUsize::new(0),
            last_content: Mutex::new(String::new()),
        })
    }
    fn failing() -> Arc<Self> {
        Arc::new(Self {
            fail: true,
            calls: AtomicUsize::new(0),
            last_content: Mutex::new(String::new()),
        })
    }
}

#[async_trait::async_trait]
impl Notifier for FakeNotifier {
    async fn send(
        &self,
        _channels: &[alpha_notify::Channel],
        content: &str,
        _summary: &str,
    ) -> Result<alpha_notify::SendReport, alpha_notify::NotifyError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        *self.last_content.lock().unwrap() = content.to_string();
        if self.fail {
            Err(alpha_notify::NotifyError::AllFailed(
                "假装推送失败".into(),
            ))
        } else {
            Ok(alpha_notify::SendReport {
                sent: 1,
                failed: 0,
                details: Vec::new(),
                legacy: false,
            })
        }
    }
}

// ---------------------------------------------------------------- 夹具

struct Fixture {
    store: Arc<alpha_store::Store>,
    config: Arc<alpha_core::config::Config>,
    registry: alpha_plugin::PluginRegistry,
    dedup: alpha_core::dedup::DedupStore,
    _tmp: tempfile::TempDir,
    dedup_path: std::path::PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let store = Arc::new(alpha_store::Store::open_in_memory().unwrap());
        store.init().unwrap();

        // `get_config()` 返回 `Arc<Config>`（配置支持热重载），直接用即可
        let config = alpha_core::config::get_config().unwrap();
        let registry = alpha_plugin::PluginRegistry::new(tmp.path().join("plugins")).unwrap();
        let dedup_path = tmp.path().join("state.json");
        let dedup = alpha_core::dedup::DedupStore::new(&dedup_path, 500);

        Self {
            store,
            config,
            registry,
            dedup,
            _tmp: tmp,
            dedup_path,
        }
    }

    fn deps<'a>(
        &'a self,
        sources: &'a SourceFactory,
        notify: &'a dyn Notifier,
    ) -> Deps<'a> {
        Deps {
            store: &self.store,
            config: Arc::clone(&self.config),
            pokedex: alpha_core::pokedex::get_pokedex().unwrap(),
            registry: &self.registry,
            dedup: &self.dedup,
            notify,
            sources,
        }
    }

    fn dedup_entries(&self) -> Vec<String> {
        alpha_core::dedup::DedupStore::new(&self.dedup_path, 500).load()
    }
}

// ---------------------------------------------------------------- 测试

/// `paused`：处于冷却窗口内且非强制，直接返回，连源都不碰。
#[tokio::test]
async fn paused_when_within_the_cooldown_window() {
    let fx = Fixture::new();
    set_pause_minutes(&fx.store, Some(30)).unwrap();

    let src: SourceFactory = Box::new(|_| panic!("冷却中不该去拉源"));
    let notify = FakeNotifier::ok();
    let out = poll_once(&fx.deps(&src, notify.as_ref()), false, false).await;

    assert_eq!(out.status, "paused", "冷却中应返回 paused，实际 {}", out.status);
    assert!(out.report.is_none());
}

/// 强制检查绕过冷却。
#[tokio::test]
async fn force_bypasses_the_cooldown() {
    let fx = Fixture::new();
    set_pause_minutes(&fx.store, Some(30)).unwrap();

    let src: SourceFactory = Box::new(|_| vec![empty_source("s1")]);
    let notify = FakeNotifier::ok();
    let out = poll_once(&fx.deps(&src, notify.as_ref()), false, true).await;

    assert_ne!(out.status, "paused", "强制检查不该被冷却挡住");
    assert_eq!(out.status, "empty");
}

/// `empty`：所有源都返回空。
#[tokio::test]
async fn empty_when_no_source_reports_a_boss() {
    let fx = Fixture::new();
    let src: SourceFactory =
        Box::new(|_| vec![empty_source("s1"), empty_source("s2")]);
    let notify = FakeNotifier::ok();
    let out = poll_once(&fx.deps(&src, notify.as_ref()), false, false).await;

    assert_eq!(out.status, "empty");
    assert_eq!(notify.calls.load(Ordering::SeqCst), 0, "没命中不该推送");
    assert!(fx.dedup_entries().is_empty(), "没命中不该写去重");
    // `last_run` 应当被写上
    assert!(!fx.store.get_kv_or("last_run", "").is_empty());
}

/// `hit`（debug）：探测到但**不推送、不去重、不暂停**。
#[tokio::test]
async fn debug_reports_a_hit_without_pushing() {
    let fx = Fixture::new();
    let src: SourceFactory = Box::new(|_| {
        vec![hit_source("s1", "测试头目", "2026-09-22 14:30:00")]
    });
    let notify = FakeNotifier::ok();
    let out = poll_once(&fx.deps(&src, notify.as_ref()), true, true).await;

    assert_eq!(out.status, "hit");
    assert!(out.report.is_some(), "debug 应当把报文带回来给面板看");
    assert!(out.resolved.is_some(), "debug 应当带回解析详情");
    assert_eq!(notify.calls.load(Ordering::SeqCst), 0, "debug 绝不能真推送");
    assert!(fx.dedup_entries().is_empty(), "debug 不该写去重标记");
    assert!(
        !alpha_scheduler::pause::is_paused(&fx.store),
        "debug 不该触发自动暂停"
    );
}

/// `pushed`：推送成功后写去重标记，并触发自动暂停。
#[tokio::test]
async fn a_successful_push_marks_dedup_and_pauses() {
    let fx = Fixture::new();
    // 明确用 fixed 模式，便于断言
    set_auto_pause(&fx.store, Some(true), Some("fixed"), Some(30)).unwrap();

    let src: SourceFactory = Box::new(|_| {
        vec![hit_source("s1", "测试头目", "2026-09-22 14:30:00")]
    });
    let notify = FakeNotifier::ok();
    let out = poll_once(&fx.deps(&src, notify.as_ref()), false, true).await;

    assert_eq!(out.status, "pushed", "message={}", out.message);
    assert_eq!(notify.calls.load(Ordering::SeqCst), 1, "应当推送一次");
    assert_eq!(fx.dedup_entries().len(), 1, "推送成功必须写去重标记");
    assert!(
        alpha_scheduler::pause::is_paused(&fx.store),
        "推送成功应当触发自动暂停"
    );
    assert!(out.channels.is_some());
    let slot = out.slot.expect("命中应带时段信息");
    assert_eq!(slot["reported"], serde_json::json!(true));
}

/// `notify_error`：推送失败**不写去重标记**（下轮重试），也**不暂停**。
///
/// 这条是最关键的回归防线：如果标记写在推送前，一次网络抖动就会
/// 永久漏报这只头目。
#[tokio::test]
async fn a_failed_push_does_not_mark_dedup() {
    let fx = Fixture::new();
    set_auto_pause(&fx.store, Some(true), Some("fixed"), Some(30)).unwrap();

    let src: SourceFactory = Box::new(|_| {
        vec![hit_source("s1", "测试头目", "2026-09-22 14:30:00")]
    });
    let notify = FakeNotifier::failing();
    let out = poll_once(&fx.deps(&src, notify.as_ref()), false, true).await;

    assert_eq!(out.status, "notify_error", "message={}", out.message);
    assert_eq!(notify.calls.load(Ordering::SeqCst), 1);
    assert!(
        fx.dedup_entries().is_empty(),
        "推送失败绝不能写去重标记，否则下轮不会再试"
    );
    assert!(
        !alpha_scheduler::pause::is_paused(&fx.store),
        "推送失败不该暂停 —— 还没推出去呢"
    );
}

/// `deduped`：同一时段第二次探测到同一只头目时跳过推送。
#[tokio::test]
async fn a_second_detect_in_the_same_slot_is_deduped() {
    let fx = Fixture::new();
    set_auto_pause(&fx.store, Some(false), None, None).unwrap(); // 关掉暂停，方便跑第二轮

    let reported = "2026-09-22 14:30:00";

    // 第一轮：命中并推送
    let src1: SourceFactory =
        Box::new(move |_| vec![hit_source("s1", "测试头目", reported)]);
    let n1 = FakeNotifier::ok();
    let out1 = poll_once(&fx.deps(&src1, n1.as_ref()), false, true).await;
    assert_eq!(out1.status, "pushed", "前置条件：第一轮应当推送成功");

    // 第二轮：同一只头目
    let src2: SourceFactory =
        Box::new(move |_| vec![hit_source("s1", "测试头目", reported)]);
    let n2 = FakeNotifier::ok();
    let out2 = poll_once(&fx.deps(&src2, n2.as_ref()), false, true).await;

    assert_eq!(out2.status, "deduped", "message={}", out2.message);
    assert_eq!(
        n2.calls.load(Ordering::SeqCst),
        0,
        "去重命中不该再推送一次"
    );
}

/// `deduped` 分支**仍然**会触发自动暂停 —— 原版的语义是
/// 「这个时段已经有头目了，别再打源站了」。
#[tokio::test]
async fn dedup_still_triggers_the_auto_pause() {
    let fx = Fixture::new();
    set_auto_pause(&fx.store, Some(true), Some("fixed"), Some(30)).unwrap();
    let reported = "2026-09-22 14:30:00";

    // 先手工塞一个去重标记，让下一轮必然走 dedup 分支
    let boss = BossData {
        name: "测试头目".into(),
        reported_at: reported.into(),
        ..Default::default()
    };
    let key = alpha_core::dedup::global_dedup_key(Some(&boss));
    assert!(!key.is_empty(), "前置条件：去重键不该为空");
    fx.dedup.mark(&key).unwrap();

    // 清掉可能存在的暂停，观察 dedup 分支是否自己把它设上
    alpha_scheduler::pause::clear_pause(&fx.store).unwrap();
    assert!(!alpha_scheduler::pause::is_paused(&fx.store));

    let src: SourceFactory = Box::new(move |_| vec![hit_source("s1", "测试头目", reported)]);
    let notify = FakeNotifier::ok();
    let out = poll_once(&fx.deps(&src, notify.as_ref()), false, true).await;

    assert_eq!(out.status, "deduped");
    assert!(
        alpha_scheduler::pause::is_paused(&fx.store),
        "dedup 分支也必须触发自动暂停"
    );
}

/// 推送内容用的是投票选出的那条源的报文。
#[tokio::test]
async fn the_pushed_content_comes_from_the_winning_source() {
    let fx = Fixture::new();
    let src: SourceFactory = Box::new(|_| {
        vec![hit_source("s1", "呆壳兽", "2026-09-22 14:30:00")]
    });
    let notify = FakeNotifier::ok();
    let out = poll_once(&fx.deps(&src, notify.as_ref()), false, true).await;

    assert_eq!(out.status, "pushed");
    assert_eq!(notify.calls.load(Ordering::SeqCst), 1);
    let content = notify.last_content.lock().unwrap().clone();
    assert!(
        !content.is_empty(),
        "推送内容不该为空 —— 空报文推出去用户只会看到一条空白消息"
    );
    assert_eq!(content, out.report.unwrap());
}
