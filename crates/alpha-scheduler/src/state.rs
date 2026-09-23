//! 调度器状态：kv 读写 + 给面板的状态快照。
//!
//! # kv 键一览（与原版 `panel/scheduler.py` 逐一对齐）
//!
//! | 键 | 默认 | 语义 |
//! |---|---|---|
//! | `scheduler_enabled` | `"0"` | 调度循环开关 |
//! | `scheduler_interval` | `"60"` | 轮询间隔（秒），**下限 5** |
//! | `scheduler_status` | `"stopped"` | `running` / `stopped` |
//! | `last_run` | `""` | 上次轮询时间 |
//! | `last_result` | `""` | 上次结果（JSON 字符串） |
//! | `current_slot` | `""` | 当前时段信息（JSON 字符串） |
//!
//! 全部是**字符串**值。`monitor_*` 系列在 `pause.rs`。
//!
//! # `last_result` / `current_slot` 是 JSON 字符串而不是列
//!
//! 原版就是塞进 kv 的。这里保持，因为面板前端读的就是
//! 「一个字符串，自己 `JSON.parse`」这个形状，改结构等于改接口。
//!
//! # `current_slot` 需要一个「还是不是本时段」的判断
//!
//! 存进去的 `time` 可能是上一个时段的 —— 面板据此显示
//! 「本时段是否已报点」。所以快照里要带
//! `current_slot_is_current` 和 `reported_this_slot`（后者已经
//! **与前者与过**），前端不必自己做时间运算。

use alpha_core::time::{is_current_slot, now_beijing, slot_name_of};
use alpha_store::{Store, StoreResult};
use serde::{Deserialize, Serialize};

use crate::pause::{monitor_state, MonitorState};

pub const KV_ENABLED: &str = "scheduler_enabled";
pub const KV_INTERVAL: &str = "scheduler_interval";
pub const KV_STATUS: &str = "scheduler_status";
pub const KV_LAST_RUN: &str = "last_run";
pub const KV_LAST_RESULT: &str = "last_result";
pub const KV_CURRENT_SLOT: &str = "current_slot";

/// 默认轮询间隔（秒）。
pub const DEFAULT_INTERVAL: i64 = 60;

/// 间隔下限。
///
/// 原版 `_loop` 里是 `interval = max(5, interval)`。设成 0 会让面板的
/// 输入框变成一个「疯狂打源站」的按钮，所以这个下限不是装饰。
pub const MIN_INTERVAL: i64 = 5;

/// 调度器快照（`GET /api/scheduler`）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SchedulerState {
    pub enabled: bool,
    pub interval: i64,
    pub status: String,
    pub last_run: String,
    /// 上次轮询结果（已解析的 JSON 对象；读不到或坏掉时是空对象）
    pub last_result: serde_json::Value,
    /// 上报中的时段信息（已解析的 JSON 对象）
    pub current_slot: serde_json::Value,
    /// `current_slot.time` 是否落在当前时段窗口内
    pub current_slot_is_current: bool,
    /// 本时段是否已报点（**已经与过 `current_slot_is_current`**）
    pub reported_this_slot: bool,
    pub monitor: MonitorState,
}

/// 当前时段标识（如 `"上午"`），供日志和面板提示用。
pub fn current_slot_label() -> &'static str {
    slot_name_of(now_beijing())
}

/// 读开关。
pub fn enabled(store: &Store) -> bool {
    store.get_kv_or(KV_ENABLED, "0") == "1"
}

/// 读间隔，并夹到 `[MIN_INTERVAL, ∞)`。
pub fn interval(store: &Store) -> i64 {
    let raw = store
        .get_kv_or(KV_INTERVAL, &DEFAULT_INTERVAL.to_string())
        .trim()
        .parse::<i64>()
        .unwrap_or(DEFAULT_INTERVAL);
    raw.max(MIN_INTERVAL)
}

/// 写开关与（可选的）间隔。
///
/// 原版 `set_scheduler` 对 `interval` **不做任何校验**，是 `_loop` 读的时候
/// 才 `max(5, ...)`。这里保持一致：写进去的是用户填的原始值，
/// 面板回显的也是它，只有实际使用时才夹紧。
pub fn set_enabled(store: &Store, on: bool, interval: Option<i64>) -> StoreResult<()> {
    store.set_kv(KV_ENABLED, if on { "1" } else { "0" })?;
    if let Some(i) = interval {
        store.set_kv(KV_INTERVAL, &i.to_string())?;
    }
    Ok(())
}

/// 记录一次轮询的开始时间。
pub fn set_last_run_now(store: &Store) -> StoreResult<()> {
    store.set_kv(KV_LAST_RUN, &alpha_core::time::now_str())
}

/// 写 `last_result`（对象序列化成字符串）。
pub fn set_last_result(store: &Store, v: &serde_json::Value) -> StoreResult<()> {
    store.set_kv(KV_LAST_RESULT, &serde_json::to_string(v).unwrap_or_default())
}

/// 写 `current_slot`（对象序列化成字符串）。
pub fn set_current_slot(store: &Store, v: &serde_json::Value) -> StoreResult<()> {
    store.set_kv(KV_CURRENT_SLOT, &serde_json::to_string(v).unwrap_or_default())
}

/// 读一个 kv 并当 **JSON 对象**解析；失败或不是对象都给空对象。
///
/// 这里必须显式判 `is_object()`：`[]`、`123`、`"x"` 都是合法 JSON，
/// 但前端的 `state.current_slot.time` 这种取值会直接拿到 `undefined`，
/// 甚至 `null.time` 抛 TypeError。宁可降级成空对象。
fn kv_json(store: &Store, key: &str) -> serde_json::Value {
    let raw = store.get_kv_or(key, "");
    if raw.trim().is_empty() {
        return serde_json::json!({});
    }
    match serde_json::from_str::<serde_json::Value>(&raw) {
        Ok(v) if v.is_object() => v,
        _ => serde_json::json!({}),
    }
}

/// 组装面板要的完整状态。
pub fn snapshot(store: &Store) -> SchedulerState {
    let current_slot = kv_json(store, KV_CURRENT_SLOT);
    let slot_time = current_slot
        .get("time")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let is_current = is_current_slot(&slot_time);

    SchedulerState {
        enabled: enabled(store),
        interval: interval(store),
        status: store.get_kv_or(KV_STATUS, "stopped"),
        last_run: store.get_kv_or(KV_LAST_RUN, ""),
        last_result: kv_json(store, KV_LAST_RESULT),
        // `reported_this_slot` 必须与 `current_slot_is_current` 相与：
        // 上一个时段的 `reported=true` 不能算「本时段已报点」
        reported_this_slot: current_slot
            .get("reported")
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
            && is_current,
        current_slot_is_current: is_current,
        current_slot,
        monitor: monitor_state(store),
    }
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
    fn defaults_match_the_original() {
        let s = store();
        let st = snapshot(&s);
        assert!(!st.enabled);
        assert_eq!(st.interval, DEFAULT_INTERVAL);
        assert_eq!(st.status, "stopped");
        assert_eq!(st.last_run, "");
        assert_eq!(st.last_result, serde_json::json!({}));
        assert_eq!(st.current_slot, serde_json::json!({}));
        assert!(!st.current_slot_is_current);
        assert!(!st.reported_this_slot);
    }

    /// 间隔下限是 5，写 0 也按 5 用。
    #[test]
    fn interval_is_clamped_to_min_five() {
        let s = store();
        set_enabled(&s, true, Some(0)).unwrap();
        assert_eq!(interval(&s), MIN_INTERVAL);
        set_enabled(&s, true, Some(1)).unwrap();
        assert_eq!(interval(&s), MIN_INTERVAL);
        set_enabled(&s, true, Some(30)).unwrap();
        assert_eq!(interval(&s), 30);
    }

    /// 间隔非法时回落默认值，而不是变成 0 导致疯狂轮询。
    #[test]
    fn garbage_interval_falls_back_to_default() {
        let s = store();
        s.set_kv(KV_INTERVAL, "abc").unwrap();
        assert_eq!(interval(&s), DEFAULT_INTERVAL);
        s.set_kv(KV_INTERVAL, "").unwrap();
        assert_eq!(interval(&s), DEFAULT_INTERVAL);
    }

    /// 上一个时段的 `reported=true` 不能算本时段已报点。
    #[test]
    fn a_past_slots_reported_flag_does_not_count_as_this_slot() {
        let s = store();
        set_current_slot(
            &s,
            &serde_json::json!({"time": "2000-01-01 02:00:00", "reported": true}),
        )
        .unwrap();
        let st = snapshot(&s);
        assert!(!st.current_slot_is_current, "2000 年的时间不可能是本时段");
        assert!(
            !st.reported_this_slot,
            "过了时的 reported 标记必须被清掉，否则永远不再播报"
        );
    }

    /// 坏掉的 JSON 不能让面板 500。
    #[test]
    fn malformed_json_in_kv_degrades_to_empty_object() {
        let s = store();
        s.set_kv(KV_LAST_RESULT, "{not json").unwrap();
        s.set_kv(KV_CURRENT_SLOT, "[]").unwrap();
        let st = snapshot(&s);
        assert_eq!(st.last_result, serde_json::json!({}));
        // `[]` 是合法 JSON 但不是对象：也得降级，不能让前端拿到数组
        assert_eq!(st.current_slot, serde_json::json!({}));
        assert!(!st.reported_this_slot);
    }

    #[test]
    fn current_slot_label_is_one_of_the_four_slots() {
        let l = current_slot_label();
        assert!(
            ["凌晨头", "早头", "午头", "晚头"].contains(&l),
            "时段名 unexpected: {l}"
        );
    }
}
