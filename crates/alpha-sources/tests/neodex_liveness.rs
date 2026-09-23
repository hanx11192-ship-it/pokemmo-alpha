//! NEODEX「过期 + 时段」双验证的单元测试。
//!
//! 这是整个源层最容易出错、后果最严重的一段逻辑：
//! `lastAlpha` 返回的是「最近一次出现的头目」而非「当前在线的头目」，
//! 少了任一道闸，面板就会在空档期反复推送一个已经消失的头目。
//!
//! 所以这里把 `now` 注入进去，把每个时段边界都钉死。

use chrono::{DateTime, TimeZone, Utc};

use alpha_sources::neodex_alpha::{judge, type_zh, Liveness};

fn utc(s: &str) -> DateTime<Utc> {
    s.parse::<DateTime<Utc>>()
        .unwrap_or_else(|_| panic!("非法时间字面量: {s}"))
}

// 北京时间 = UTC+8。以下注释都写北京时间，构造用 UTC。

#[test]
fn active_when_despawn_in_future_and_same_slot() {
    // 北京时间 10:00（早头 08-14），头目 09:30 出现、10:45 消失
    let now = utc("2026-09-11T02:00:00Z");
    let out = judge(
        Some("2026-09-11T01:30:00Z"), // 09:30 BJ
        Some("2026-09-11T02:45:00Z"), // 10:45 BJ
        now,
    );
    assert_eq!(out.liveness, Liveness::Active);
    assert_eq!(out.remaining_sec, 45 * 60);
    assert_eq!(out.current_slot, "早头");
}

#[test]
fn expired_when_despawn_passed() {
    // 北京时间 10:00，头目 08:30 消失 —— 已经过期 90 分钟
    let now = utc("2026-09-11T02:00:00Z"); // 10:00 BJ
    let out = judge(
        Some("2026-09-11T00:30:00Z"), // 08:30 BJ
        Some("2026-09-11T00:45:00Z"), // 08:45 BJ（早头内）
        now,
    );
    assert_eq!(out.liveness, Liveness::Expired);
    assert!(out.remaining_sec < 0);
}

#[test]
fn wrong_slot_when_carried_over_from_previous_slot() {
    // 关键回归：14:00 刚进午头，lastAlpha 还停在 13:30 早头刷的那只。
    // 它 14:45 消失（还没过期），但「出现时段=早头、消失时段=午头」——
    // 时段闸必须放行（因为消失时段是午头），否则午头开头会漏报。
    let now = utc("2026-09-11T06:00:00Z"); // 14:00 BJ
    let out = judge(
        Some("2026-09-11T05:30:00Z"), // 13:30 BJ（早头）
        Some("2026-09-11T06:45:00Z"), // 14:45 BJ（午头）
        now,
    );
    assert_eq!(
        out.liveness,
        Liveness::Active,
        "消失时段落在当前时段，应放行（跨时段 spillover 是正常的）"
    );
}

#[test]
fn wrong_slot_when_neither_slot_matches() {
    // 时段闸要挡的是「残留记录」：头目属于别的时段，而现在是另一个时段。
    //
    // 注意：只要 despawn 还没到，且「出现时段 或 消失时段」任一等于当前时段就放行
    // （见上一个用例：跨时段 spillover 是正常的）。
    // 因此真正能触发 WrongSlot 的，是**源站脏数据** —— 比如头目记录被改成了
    // 「下午刷新、跨夜消失」，而现在是凌晨，两个时段都对不上。
    //
    // 现在 = 北京时间 03:00（凌晨头）
    // 出现 = 北京时间 20:00（晚头）
    // 消失 = 北京时间 08:30（早头，还没到）
    let now = utc("2026-09-10T19:00:00Z"); // 次日 03:00 BJ
    let out = judge(
        Some("2026-09-11T12:00:00Z"), // 20:00 BJ（晚头）
        Some("2026-09-11T00:30:00Z"), // 08:30 BJ（早头）— 在 now 之后，未过期
        now,
    );
    assert!(
        out.remaining_sec > 0,
        "这个构造的前提是「未过期」，实际剩余 {}s",
        out.remaining_sec
    );
    assert_eq!(
        out.liveness,
        Liveness::WrongSlot,
        "出现=晚头、消失=早头，当前=凌晨头，三者互不相交 -> 应判为跨时段残留"
    );
    assert_eq!(out.current_slot, "凌晨头");
    assert_eq!(out.candidate_slots, vec!["早头", "晚头"]);
}

#[test]
fn no_despawn_is_treated_as_not_current() {
    let out = judge(Some("2026-09-11T02:00:00Z"), None, utc("2026-09-11T02:10:00Z"));
    assert_eq!(
        out.liveness,
        Liveness::NoDespawn,
        "缺 despawnAt 无法证明活跃，保守视为非当前"
    );
    assert_eq!(out.remaining_sec, 0);
}

#[test]
fn missing_reported_at_still_judges_by_despawn_slot() {
    // 没有 reportedAt 时，候选时段只剩「消失时段」——不应 panic
    let now = utc("2026-09-11T02:00:00Z"); // 10:00 BJ 早头
    let out = judge(None, Some("2026-09-11T02:45:00Z"), now); // 10:45 BJ 早头
    assert_eq!(out.liveness, Liveness::Active);
    assert_eq!(out.candidate_slots, vec!["早头"]);
}

#[test]
fn candidate_slots_dedup_when_same_slot() {
    // 出现与消失都在早头 -> 候选集合应去重为 1 个
    let out = judge(
        Some("2026-09-11T01:00:00Z"), // 09:00 BJ 早头
        Some("2026-09-11T02:00:00Z"), // 10:00 BJ 早头
        utc("2026-09-11T01:30:00Z"),  // 09:30 BJ 早头
    );
    assert_eq!(out.liveness, Liveness::Active);
    assert_eq!(out.candidate_slots, vec!["早头"]);
}

#[test]
fn night_slot_spans_midnight() {
    // 晚头 20:00–次日 02:00 跨午夜：23:30 与次日 01:00 都算晚头
    let out_late = judge(
        Some("2026-09-11T14:30:00Z"), // 22:30 BJ
        Some("2026-09-11T15:30:00Z"), // 23:30 BJ
        utc("2026-09-11T15:00:00Z"),  // 23:00 BJ
    );
    assert_eq!(out_late.current_slot, "晚头");
    assert_eq!(out_late.liveness, Liveness::Active);

    let out_early = judge(
        Some("2026-09-11T16:00:00Z"), // 次日 00:00 BJ
        Some("2026-09-11T16:30:00Z"), // 次日 00:30 BJ
        utc("2026-09-11T16:15:00Z"),  // 次日 00:15 BJ
    );
    assert_eq!(
        out_early.current_slot, "晚头",
        "次日 00:15 仍属晚头（跨午夜）"
    );
    assert_eq!(out_early.liveness, Liveness::Active);
}

#[test]
fn all_four_slots_boundaries() {
    // 每个时段的起点/终点各验一次，确认没有偏移
    let cases = [
        ("2026-09-10T18:00:00Z", "凌晨头"), // 次日 02:00 BJ
        ("2026-09-10T19:59:00Z", "凌晨头"), // 次日 03:59 BJ
        ("2026-09-11T00:00:00Z", "早头"),   // 08:00 BJ
        ("2026-09-11T05:59:00Z", "早头"),   // 13:59 BJ
        ("2026-09-11T06:00:00Z", "午头"),   // 14:00 BJ
        ("2026-09-11T11:59:00Z", "午头"),   // 19:59 BJ
        ("2026-09-11T12:00:00Z", "晚头"),   // 20:00 BJ
        ("2026-09-11T17:59:00Z", "晚头"),   // 次日 01:59 BJ
    ];
    for (now_iso, expected_slot) in cases {
        let now = utc(now_iso);
        // 造一个「现在刷出、75 分钟后消失」的头目，保证未过期
        let despawn = now + chrono::Duration::minutes(75);
        let out = judge(
            Some(&now.to_rfc3339()),
            Some(&despawn.to_rfc3339()),
            now,
        );
        assert_eq!(
            out.current_slot, expected_slot,
            "now={now_iso}({}) 应属 {expected_slot}",
            now.with_timezone(&Utc)
        );
    }
}

// ------------------------------------------------------------- 属性译中

#[test]
fn type_translation_covers_all_18_types() {
    let pairs = [
        ("NORMAL", "一般"),
        ("FIRE", "火"),
        ("WATER", "水"),
        ("ELECTRIC", "电"),
        ("GRASS", "草"),
        ("ICE", "冰"),
        ("FIGHTING", "格斗"),
        ("POISON", "毒"),
        ("GROUND", "地面"),
        ("FLYING", "飞行"),
        ("PSYCHIC", "超能力"),
        ("BUG", "虫"),
        ("ROCK", "岩石"),
        ("GHOST", "幽灵"),
        ("DRAGON", "龙"),
        ("DARK", "恶"),
        ("STEEL", "钢"),
        ("FAIRY", "妖精"),
    ];
    for (en, zh) in pairs {
        assert_eq!(type_zh(en), zh, "属性 {en} 译中错误");
        // 源站大小写不固定，小写也要能查到
        assert_eq!(type_zh(&en.to_lowercase()), zh, "属性 {en} 小写形式译中错误");
    }
    // 未知属性原样保留，不丢信息
    assert_eq!(type_zh("COSMIC"), "COSMIC");
}

#[test]
fn judge_handles_malformed_timestamps() {
    // 脏时间不应 panic —— 源站偶尔会给空串或半截时间
    for bad in ["", "not-a-time", "2026-13-45T99:99:99Z"] {
        let out = judge(Some(bad), Some(bad), utc("2026-09-11T02:00:00Z"));
        assert_eq!(
            out.liveness,
            Liveness::NoDespawn,
            "非法 despawnAt `{bad}` 应保守判为非当前"
        );
    }
}

#[test]
fn judge_is_pure_and_repeatable() {
    let now = Utc.with_ymd_and_hms(2026, 9, 11, 2, 0, 0).unwrap();
    let a = judge(Some("2026-09-11T01:30:00Z"), Some("2026-09-11T02:45:00Z"), now);
    let b = judge(Some("2026-09-11T01:30:00Z"), Some("2026-09-11T02:45:00Z"), now);
    assert_eq!(a.liveness, b.liveness);
    assert_eq!(a.remaining_sec, b.remaining_sec);
    assert_eq!(a.current_slot, b.current_slot);
}
