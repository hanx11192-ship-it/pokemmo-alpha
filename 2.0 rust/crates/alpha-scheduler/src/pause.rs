//! 监控冷却（自动暂停）。
//!
//! 抓到头目之后暂停对源站的轮询，避免「刚报完点还一直打源站」。
//! 两种模式：
//!
//! | 模式 | 解除时机 |
//! |---|---|
//! | `slot`（默认） | 下一个时段开始（02/08/14/20 点） |
//! | `fixed` | 固定分钟数后（默认 75 分钟 = 头目存活时长） |
//!
//! # 一个容易漏掉的副作用
//!
//! 原版 `set_monitor_pause_until_next_slot()` 除了写 `monitor_pause_until`，
//! **还顺手把 `monitor_pause_minutes` 改成「到下一个时段的分钟数」**：
//!
//! ```python
//! db.set_kv("monitor_pause_minutes", str(max(0, int((until - time.time())/60))))
//! ```
//!
//! 这一行看着像多余，其实有实际影响：`slot` 模式跑过一轮之后，
//! 用户再切到 `fixed` 模式，用的就是这个被改过的分钟数，
//! 而不是原来的 75。少抄这一行，「slot 之后切 fixed」的行为就和原版不一样。
//!
//! # 关开关时要顺手清状态
//!
//! `set_monitor_auto_pause(enabled=False)` 除了写 `monitor_auto_pause=0`，
//! **还要 `clear_monitor_pause()`**。否则用户关了自动暂停、当前却还在
//! 冷却期里，轮询会继续停着 —— 面板显示「自动暂停：关」但实际不轮询，
//! 是最难查的那类 bug。

use alpha_core::time::{next_slot_start, now_beijing, SLOT_START_HOURS};
use alpha_store::{Store, StoreResult};

/// kv：是否开启「抓到头目后自动暂停」
pub const KV_AUTO_PAUSE: &str = "monitor_auto_pause";
/// kv：暂停模式，`slot` | `fixed`
pub const KV_PAUSE_MODE: &str = "monitor_pause_mode";
/// kv：`fixed` 模式下的分钟数
pub const KV_PAUSE_MINUTES: &str = "monitor_pause_minutes";
/// kv：解除暂停的绝对时间（Unix 秒的**浮点字符串**）
pub const KV_PAUSE_UNTIL: &str = "monitor_pause_until";

/// 默认暂停分钟数。
///
/// 等于头目存活时间（75 分钟）—— 抓到时已经过了多久不知道，
/// 按整整 75 分钟冷却至少不会漏掉下一只。
pub const DEFAULT_PAUSE_MIN: i64 = 75;

/// 暂停模式。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PauseMode {
    /// 暂停到下一个时段开始
    Slot,
    /// 暂停固定分钟数
    Fixed,
}

impl PauseMode {
    pub fn as_str(self) -> &'static str {
        match self {
            PauseMode::Slot => "slot",
            PauseMode::Fixed => "fixed",
        }
    }

    /// 解析模式字符串；无法识别时回落到 `slot`。
    ///
    /// 原版是 `db.get_kv("monitor_pause_mode", "slot") or "slot"`，
    /// 然后 `if mode == "fixed" ... else ...` —— 也就是**任何非 `fixed`
    /// 的值都走 slot**。所以这里只认 `fixed`，其余全是 slot。
    pub fn parse(s: &str) -> Self {
        if s == "fixed" {
            PauseMode::Fixed
        } else {
            PauseMode::Slot
        }
    }
}

/// 监控冷却状态。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct MonitorState {
    pub auto_pause: bool,
    pub mode: String,
    pub pause_minutes: i64,
    pub paused: bool,
    pub pause_until: i64,
    pub pause_until_str: String,
    pub remaining_seconds: i64,
}

/// 读 `monitor_pause_until`（浮点字符串）。
///
/// 原版写这个 kv 时用的是 `str(until.timestamp())`，`until` 是 float，
/// 于是存进去的是 `"1758508800.123456"` 这种。按 `i64` 解析会失败，
/// 所以必须走 `f64`。读的时候再取整。
fn pause_until_raw(store: &Store) -> f64 {
    store
        .get_kv_or(KV_PAUSE_UNTIL, "0")
        .trim()
        .parse::<f64>()
        .unwrap_or(0.0)
}

/// 当前是否处于冷却中。
pub fn is_paused(store: &Store) -> bool {
    pause_until_raw(store) > now_secs_f64()
}

/// 当前 Unix 时间（秒，带小数）。
fn now_secs_f64() -> f64 {
    chrono::Utc::now().timestamp_millis() as f64 / 1000.0
}

/// 暂停到下一个时段开始。
///
/// **副作用**：同时把 `monitor_pause_minutes` 改成到下一时段的分钟数，
/// 见本模块文档。
pub fn set_pause_until_next_slot(store: &Store) -> StoreResult<i64> {
    let nxt = next_slot_start(now_beijing());
    let until = nxt.timestamp();
    store.set_kv(KV_PAUSE_UNTIL, &format!("{}", until as f64))?;

    let mins = ((until - chrono::Utc::now().timestamp()).max(0)) / 60;
    store.set_kv(KV_PAUSE_MINUTES, &mins.to_string())?;
    Ok(until)
}

/// 暂停固定分钟数。`minutes` 为 `None` 时读 kv，读不到用默认 75。
pub fn set_pause_minutes(store: &Store, minutes: Option<i64>) -> StoreResult<i64> {
    let minutes = match minutes {
        Some(m) => m,
        None => store
            .get_kv_or(KV_PAUSE_MINUTES, &DEFAULT_PAUSE_MIN.to_string())
            .trim()
            .parse::<i64>()
            .unwrap_or(DEFAULT_PAUSE_MIN),
    };
    store.set_kv(KV_PAUSE_MINUTES, &minutes.to_string())?;
    let until = chrono::Utc::now().timestamp() + minutes * 60;
    store.set_kv(KV_PAUSE_UNTIL, &format!("{}", until as f64))?;
    Ok(until)
}

/// 立即解除冷却。
pub fn clear_pause(store: &Store) -> StoreResult<()> {
    store.set_kv(KV_PAUSE_UNTIL, "0")
}

/// 抓到头目后的自动暂停。未开启则什么都不做。
pub fn maybe_pause_after_detect(store: &Store) -> StoreResult<Option<i64>> {
    if store.get_kv_or(KV_AUTO_PAUSE, "1") != "1" {
        return Ok(None);
    }
    let mode = PauseMode::parse(&store.get_kv_or(KV_PAUSE_MODE, "slot"));
    let until = match mode {
        PauseMode::Fixed => {
            let mins = store
                .get_kv_or(KV_PAUSE_MINUTES, &DEFAULT_PAUSE_MIN.to_string())
                .trim()
                .parse::<i64>()
                .unwrap_or(DEFAULT_PAUSE_MIN);
            set_pause_minutes(store, Some(mins))?
        }
        PauseMode::Slot => set_pause_until_next_slot(store)?,
    };
    Ok(Some(until))
}

/// 面板改自动暂停设置。
///
/// 三个参数都是 `Option`：`None` 表示「这次不改这一项」。
pub fn set_auto_pause(
    store: &Store,
    enabled: Option<bool>,
    mode: Option<&str>,
    minutes: Option<i64>,
) -> StoreResult<()> {
    if let Some(e) = enabled {
        store.set_kv(KV_AUTO_PAUSE, if e { "1" } else { "0" })?;
        // 关掉自动暂停时顺手解除现有冷却，否则会「显示已关但仍在暂停」
        if !e {
            clear_pause(store)?;
        }
    }
    if let Some(m) = mode {
        store.set_kv(KV_PAUSE_MODE, m)?;
    }
    if let Some(m) = minutes {
        store.set_kv(KV_PAUSE_MINUTES, &m.to_string())?;
    }
    Ok(())
}

/// 组装给面板用的冷却状态。
pub fn monitor_state(store: &Store) -> MonitorState {
    let auto_pause = store.get_kv_or(KV_AUTO_PAUSE, "1") == "1";
    let mode = store.get_kv_or(KV_PAUSE_MODE, "slot");
    let pause_minutes = store
        .get_kv_or(KV_PAUSE_MINUTES, &DEFAULT_PAUSE_MIN.to_string())
        .trim()
        .parse::<i64>()
        .unwrap_or(DEFAULT_PAUSE_MIN);

    let raw = pause_until_raw(store);
    let until = raw as i64;
    let now = now_secs_f64();
    let paused = raw > now;

    MonitorState {
        auto_pause,
        mode,
        pause_minutes,
        paused,
        pause_until: until,
        // 北京时间展示：原版用 `time.localtime()`，现网机器 TZ 恰好是
        // Asia/Shanghai 所以是对的。这里显式转北京时间，摆脱对机器时区的依赖。
        pause_until_str: if until > 0 {
            chrono::DateTime::from_timestamp(until, 0)
                .map(|dt| {
                    dt.with_timezone(&chrono_tz::Asia::Shanghai)
                        .format("%Y-%m-%d %H:%M:%S")
                        .to_string()
                })
                .unwrap_or_default()
        } else {
            String::new()
        },
        remaining_seconds: if paused {
            ((raw - now).ceil() as i64).max(0)
        } else {
            0
        },
    }
}

/// 时段起点小时表，便于面板提示「下一个时段是什么时候」。
pub fn slot_hours() -> &'static [u32; 4] {
    &SLOT_START_HOURS
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> Store {
        let s = Store::open_in_memory().unwrap();
        s.init().unwrap();
        s
    }

    #[test]
    fn default_state_is_not_paused() {
        let s = store();
        let st = monitor_state(&s);
        assert!(!st.paused);
        assert!(st.auto_pause, "默认应当开着自动暂停");
        assert_eq!(st.mode, "slot");
        assert_eq!(st.pause_minutes, DEFAULT_PAUSE_MIN);
        assert_eq!(st.remaining_seconds, 0);
        assert_eq!(st.pause_until_str, "");
    }

    #[test]
    fn fixed_mode_pauses_for_the_given_minutes() {
        let s = store();
        set_auto_pause(&s, None, Some("fixed"), Some(30)).unwrap();
        maybe_pause_after_detect(&s).unwrap();

        let st = monitor_state(&s);
        assert!(st.paused);
        assert!(
            (st.remaining_seconds - 30 * 60).abs() <= 2,
            "剩余秒数应约等于 30 分钟，实际 {}",
            st.remaining_seconds
        );
    }

    /// `slot` 模式必须顺带改掉 `monitor_pause_minutes` ——
    /// 这是原版里最容易被漏掉的一行副作用。
    #[test]
    fn slot_mode_rewrites_pause_minutes_as_a_side_effect() {
        let s = store();
        s.set_kv(KV_PAUSE_MINUTES, "75").unwrap();
        let until = set_pause_until_next_slot(&s).unwrap();

        let got: i64 = s
            .get_kv_or(KV_PAUSE_MINUTES, "0")
            .trim()
            .parse()
            .unwrap();
        let expect = ((until - chrono::Utc::now().timestamp()).max(0)) / 60;
        assert!(
            (got - expect).abs() <= 1,
            "slot 模式应把 pause_minutes 改成距下一时段的分钟数：期望约 {expect}，实际 {got}"
        );
        assert!(
            got > 0,
            "距下一时段的分钟数应当为正（时段每 6 小时一次）"
        );
    }

    /// 关掉自动暂停必须解除现有冷却。
    #[test]
    fn disabling_auto_pause_clears_an_active_pause() {
        let s = store();
        set_auto_pause(&s, None, Some("fixed"), Some(60)).unwrap();
        maybe_pause_after_detect(&s).unwrap();
        assert!(is_paused(&s), "前置条件：应当处于冷却中");

        set_auto_pause(&s, Some(false), None, None).unwrap();
        assert!(!is_paused(&s), "关掉自动暂停后不应继续冷却");
        let st = monitor_state(&s);
        assert!(!st.auto_pause);
        assert!(!st.paused);
    }

    #[test]
    fn auto_pause_disabled_means_detect_does_not_pause() {
        let s = store();
        set_auto_pause(&s, Some(false), None, None).unwrap();
        let r = maybe_pause_after_detect(&s).unwrap();
        assert!(r.is_none());
        assert!(!is_paused(&s));
    }

    #[test]
    fn clear_pause_resets_the_deadline() {
        let s = store();
        set_pause_minutes(&s, Some(10)).unwrap();
        assert!(is_paused(&s));
        clear_pause(&s).unwrap();
        assert!(!is_paused(&s));
    }

    #[test]
    fn unknown_mode_string_is_treated_as_slot() {
        assert_eq!(PauseMode::parse("fixed"), PauseMode::Fixed);
        assert_eq!(PauseMode::parse("slot"), PauseMode::Slot);
        assert_eq!(PauseMode::parse(""), PauseMode::Slot);
        assert_eq!(PauseMode::parse("FIXED"), PauseMode::Slot);
        assert_eq!(PauseMode::parse("garbage"), PauseMode::Slot);
    }

    /// `monitor_pause_until` 存的是浮点字符串，整数解析会失败。
    #[test]
    fn float_formatted_until_is_parsed_correctly() {
        let s = store();
        let future = now_secs_f64() + 120.0;
        s.set_kv(KV_PAUSE_UNTIL, &format!("{future}")).unwrap();
        // 先确认这确实是浮点字符串（带小数点）
        assert!(format!("{future}").contains('.'));
        assert!(is_paused(&s));
        let st = monitor_state(&s);
        assert!(st.remaining_seconds > 0 && st.remaining_seconds <= 121);
    }
}
