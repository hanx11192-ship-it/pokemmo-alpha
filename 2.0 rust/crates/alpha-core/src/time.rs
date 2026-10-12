//! 北京时间与「时段」计算。
//!
//! 游戏机制（用户给定）：
//! ```text
//! 一天分 4 个时段，每时段必定刷一只固定存在 75 分钟的头目：
//!     早头   08:00–14:00
//!     午头   14:00–20:00
//!     晚头   20:00–次日 02:00
//!     凌晨头 02:00–08:00
//! ```
//!
//! 所有时间计算统一按北京时间，不依赖宿主机本地时区。
//! 对应原版 `src/core/dedup.py` 的 `slot_of` 与 `panel/scheduler.py` 的时段窗口函数。

use chrono::{DateTime, Datelike, Duration, NaiveDateTime, TimeZone, Utc};
use chrono_tz::Asia::Shanghai;

/// 时段起点小时（北京）：早/午/晚/凌晨。
pub const SLOT_START_HOURS: [u32; 4] = [2, 8, 14, 20];

/// 头目存活时长（分钟）。PokeMMO 的 Alpha 固定存活约 75 分钟。
pub const ALPHA_LIFETIME_MIN: i64 = 75;

/// 把任意 `DateTime` 转成北京时间。
pub fn to_beijing<Tz: TimeZone>(dt: DateTime<Tz>) -> DateTime<chrono_tz::Tz> {
    dt.with_timezone(&Shanghai)
}

/// 当前北京时间。
pub fn now_beijing() -> DateTime<chrono_tz::Tz> {
    Utc::now().with_timezone(&Shanghai)
}

/// 解析 ISO-8601 时间串（兼容尾随 `Z` 与 `+00:00`），失败返回 `None`。
pub fn parse_iso(s: &str) -> Option<DateTime<Utc>> {
    if s.is_empty() {
        return None;
    }
    // chrono 的 from_rfc3339 接受 "Z" 后缀；对没有时区信息的串做兜底
    if let Ok(dt) = DateTime::parse_from_rfc3339(s) {
        return Some(dt.with_timezone(&Utc));
    }
    // 兜底：形如 "2026-09-11 04:50:53" 的裸时间，按 UTC 处理
    for fmt in ["%Y-%m-%d %H:%M:%S", "%Y-%m-%dT%H:%M:%S"] {
        if let Ok(naive) = NaiveDateTime::parse_from_str(s, fmt) {
            return Some(Utc.from_utc_datetime(&naive));
        }
    }
    None
}

/// 由北京时间小时数得出时段中文名。
///
/// 返回 `(时段名, 时段范围描述)`。
pub fn slot_name_of_hour(hour: u32) -> (&'static str, &'static str) {
    match hour {
        2..=7 => ("凌晨头", "02:00-08:00"),
        8..=13 => ("早头", "08:00-14:00"),
        14..=19 => ("午头", "14:00-20:00"),
        _ => ("晚头", "20:00-次日02:00"),
    }
}

/// 时段中文名 → 英文名。
pub fn slot_name_en(zh: &str) -> &'static str {
    match zh {
        "凌晨头" => "Dawn Alpha",
        "早头" => "Morning Alpha",
        "午头" => "Noon Alpha",
        "晚头" => "Night Alpha",
        _ => "Alpha",
    }
}

/// 由北京时间得出时段名。
pub fn slot_name_of(dt: DateTime<chrono_tz::Tz>) -> &'static str {
    use chrono::Timelike;
    slot_name_of_hour(dt.hour()).0
}

/// 返回头目所属时段的稳定标识（北京时间 6h 边界）：形如 `20260911-08` / `-14` / `-20` / `-02`。
///
/// 晚头 20:00–次日 02:00 是跨午夜的同一时段：
/// 当天 20:xx 与次日 01:xx 都归到「起始日 20:00」，即同一个 key。
///
/// 这与原版 `dedup.slot_of` 的输出**逐字一致**，保证 state.json 可平滑迁移。
pub fn slot_key_of(dt: DateTime<chrono_tz::Tz>) -> String {
    use chrono::Timelike;
    let h = dt.hour();
    let (base, sh) = match h {
        8..=13 => (dt.date_naive(), 8u32),
        14..=19 => (dt.date_naive(), 14u32),
        20..=23 => (dt.date_naive(), 20u32),
        0..=1 => (dt.date_naive() - Duration::days(1), 20u32), // 晚头跨午夜
        _ => (dt.date_naive(), 2u32),                          // 2..=7 凌晨头
    };
    format!("{}-{:02}", base.format("%Y%m%d"), sh)
}

/// 拼推送正文首行的时段字段：`午头(约止于18:14)`。
pub fn make_period(slot_zh: &str, expire: DateTime<chrono_tz::Tz>) -> String {
    format!("{}(约止于{})", slot_zh, expire.format("%H:%M"))
}

/// 给定时刻之后最近的时段起点（北京时间）。
pub fn next_slot_start(now: DateTime<chrono_tz::Tz>) -> DateTime<chrono_tz::Tz> {
    let mut cands: Vec<DateTime<chrono_tz::Tz>> = Vec::with_capacity(8);
    for off in 0..=1 {
        let d = (now + Duration::days(off)).date_naive();
        for h in SLOT_START_HOURS {
            if let Some(naive) = d.and_hms_opt(h, 0, 0) {
                let c = Shanghai.from_local_datetime(&naive).single();
                if let Some(c) = c {
                    if c > now {
                        cands.push(c);
                    }
                }
            }
        }
    }
    cands
        .into_iter()
        .min()
        .unwrap_or_else(|| now + Duration::hours(6))
}

/// 当前时段窗口 `[起点, 终点)`（北京时间）。
pub fn current_slot_window(now: DateTime<chrono_tz::Tz>) -> (DateTime<chrono_tz::Tz>, DateTime<chrono_tz::Tz>) {
    use chrono::Timelike;
    let h = now.hour();
    let start_h = match h {
        2..=7 => 2u32,
        8..=13 => 8,
        14..=19 => 14,
        _ => 20,
    };
    let mut day = now.date_naive();
    // 0~1 点时，当前窗口起点是「昨天 20:00」
    if start_h == 20 && h < 20 {
        day -= Duration::days(1);
    }
    let ws_naive = day.and_hms_opt(start_h, 0, 0).unwrap_or_else(|| now.naive_local());
    let ws = Shanghai
        .from_local_datetime(&ws_naive)
        .single()
        .unwrap_or(now);
    (ws, ws + Duration::hours(6))
}

/// 判断一个北京时间字符串 `%Y-%m-%d %H:%M:%S` 是否落在当前时段窗口内。
pub fn is_current_slot(time_str: &str) -> bool {
    let tz = Shanghai;
    let naive = match NaiveDateTime::parse_from_str(time_str, "%Y-%m-%d %H:%M:%S") {
        Ok(n) => n,
        Err(_) => return false,
    };
    let dt = match tz.from_local_datetime(&naive).single() {
        Some(d) => d,
        None => return false,
    };
    let (ws, we) = current_slot_window(now_beijing());
    dt >= ws && dt < we
}

/// 当前北京时间的标准格式串。
pub fn now_str() -> String {
    now_beijing().format("%Y-%m-%d %H:%M:%S").to_string()
}

/// 秒 → 人类可读时长（如 `1h05m` / `12m30s`）。
pub fn fmt_duration(sec: i64) -> String {
    if sec <= 0 {
        return "0s".to_string();
    }
    let h = sec / 3600;
    let r = sec % 3600;
    let m = r / 60;
    let s = r % 60;
    if h > 0 {
        format!("{}h{:02}m", h, m)
    } else {
        format!("{}m{:02}s", m, s)
    }
}

/// 判断年份是否为闰年（供需要日期运算的调用方复用）。
pub fn is_leap_year(year: i32) -> bool {
    (year % 4 == 0 && year % 100 != 0) || year % 400 == 0
}

/// 返回一年中的第几天（1-366）。
pub fn day_of_year(dt: DateTime<chrono_tz::Tz>) -> u32 {
    dt.ordinal()
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn bj(y: i32, mo: u32, d: u32, h: u32, mi: u32) -> DateTime<chrono_tz::Tz> {
        Shanghai
            .with_ymd_and_hms(y, mo, d, h, mi, 0)
            .single()
            .unwrap()
    }

    #[test]
    fn slot_key_matches_original() {
        // 早头
        assert_eq!(slot_key_of(bj(2026, 9, 11, 8, 0)), "20260911-08");
        assert_eq!(slot_key_of(bj(2026, 9, 11, 13, 59)), "20260911-08");
        // 午头
        assert_eq!(slot_key_of(bj(2026, 9, 11, 14, 0)), "20260911-14");
        // 晚头（当天 20 点起）
        assert_eq!(slot_key_of(bj(2026, 9, 11, 20, 0)), "20260911-20");
        assert_eq!(slot_key_of(bj(2026, 9, 11, 23, 59)), "20260911-20");
        // 晚头跨午夜：次日 00:30 仍归到前一天 20 点
        assert_eq!(slot_key_of(bj(2026, 9, 12, 0, 30)), "20260911-20");
        assert_eq!(slot_key_of(bj(2026, 9, 12, 1, 59)), "20260911-20");
        // 凌晨头
        assert_eq!(slot_key_of(bj(2026, 9, 12, 2, 0)), "20260912-02");
        assert_eq!(slot_key_of(bj(2026, 9, 12, 7, 59)), "20260912-02");
    }

    #[test]
    fn slot_names() {
        assert_eq!(slot_name_of_hour(3).0, "凌晨头");
        assert_eq!(slot_name_of_hour(9).0, "早头");
        assert_eq!(slot_name_of_hour(15).0, "午头");
        assert_eq!(slot_name_of_hour(21).0, "晚头");
        assert_eq!(slot_name_of_hour(0).0, "晚头");
        assert_eq!(slot_name_en("午头"), "Noon Alpha");
    }

    #[test]
    fn next_slot_start_skips_forward() {
        // 18:40 → 下一个时段起点是当天 20:00
        let n = next_slot_start(bj(2026, 9, 11, 18, 40));
        assert_eq!(n.format("%Y-%m-%d %H:%M").to_string(), "2026-09-11 20:00");
        // 01:30 → 下一个时段起点是当天 02:00
        let n = next_slot_start(bj(2026, 9, 12, 1, 30));
        assert_eq!(n.format("%Y-%m-%d %H:%M").to_string(), "2026-09-12 02:00");
        // 21:00 → 跨天到次日 02:00
        let n = next_slot_start(bj(2026, 9, 11, 21, 0));
        assert_eq!(n.format("%Y-%m-%d %H:%M").to_string(), "2026-09-12 02:00");
    }

    #[test]
    fn current_window_handles_midnight() {
        // 00:30 属于「昨天 20:00 起」的晚头窗口
        let (ws, we) = current_slot_window(bj(2026, 9, 12, 0, 30));
        assert_eq!(ws.format("%Y-%m-%d %H:%M").to_string(), "2026-09-11 20:00");
        assert_eq!(we.format("%Y-%m-%d %H:%M").to_string(), "2026-09-12 02:00");
        // 15:00 属于当天 14:00 起的午头窗口
        let (ws, we) = current_slot_window(bj(2026, 9, 12, 15, 0));
        assert_eq!(ws.format("%Y-%m-%d %H:%M").to_string(), "2026-09-12 14:00");
        assert_eq!(we.format("%Y-%m-%d %H:%M").to_string(), "2026-09-12 20:00");
    }

    #[test]
    fn parse_iso_forms() {
        assert!(parse_iso("2026-09-11T04:50:53.351Z").is_some());
        assert!(parse_iso("2026-09-11T04:50:53+00:00").is_some());
        assert!(parse_iso("2026-09-11 04:50:53").is_some());
        assert!(parse_iso("").is_none());
        assert!(parse_iso("not-a-date").is_none());
    }

    #[test]
    fn duration_format() {
        assert_eq!(fmt_duration(0), "0s");
        assert_eq!(fmt_duration(-5), "0s");
        assert_eq!(fmt_duration(750), "12m30s");
        assert_eq!(fmt_duration(3900), "1h05m");
    }

    #[test]
    fn period_format() {
        let e = bj(2026, 9, 11, 18, 14);
        assert_eq!(make_period("午头", e), "午头(约止于18:14)");
    }
}
