//! 调度器的差分基准测试。
//!
//! `fixtures/scheduler_golden.json` 由 `tools/gen_scheduler_golden.py`
//! **跑原版 Python** 生成 —— 不是手写的期望值。
//!
//! 为什么必须这样：时段窗口和投票仲裁都是「我的理解容易替代事实」的地方。
//! 比如 `-prio` 打反成 `prio`，自己写的测试会跟着一起反，
//! 一路绿灯上线，直到平票时选错了源才发现。基准必须是外部事实。
//!
//! 重新生成：
//!
//! ```text
//! python3 tools/gen_scheduler_golden.py /workspace/pokemmo_alpha_orig
//! ```

use serde::Deserialize;

const GOLDEN: &str = include_str!("../../../fixtures/scheduler_golden.json");

#[derive(Debug, Deserialize)]
struct Golden {
    slot_windows: Vec<SlotWindow>,
    is_current_slot: Vec<IsCurrentSlot>,
    vote: Vec<VoteCase>,
}

#[derive(Debug, Deserialize)]
struct SlotWindow {
    now: String,
    window_start: String,
    window_end: String,
    next_slot_start: String,
}

#[derive(Debug, Deserialize)]
struct IsCurrentSlot {
    now: String,
    probe: String,
    label: String,
    expected: bool,
}

#[derive(Debug, Deserialize)]
struct VoteCase {
    case: String,
    hits: Vec<HitEntry>,
    priorities: std::collections::HashMap<String, i64>,
    expected_source: String,
}

#[derive(Debug, Deserialize)]
struct HitEntry {
    source: String,
    boss: BossEntry,
}

#[derive(Debug, Deserialize)]
struct BossEntry {
    #[serde(default)]
    name: String,
    #[serde(default)]
    pokedex_id: Option<i64>,
    #[serde(default)]
    reported_at: String,
}

fn golden() -> Golden {
    serde_json::from_str(GOLDEN).expect("golden 文件解析失败")
}

fn parse_naive(s: &str) -> chrono::NaiveDateTime {
    chrono::NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M:%S")
        .unwrap_or_else(|e| panic!("时间解析失败 {s:?}: {e}"))
}

fn to_bj(s: &str) -> chrono::DateTime<chrono_tz::Tz> {
    use chrono::TimeZone;
    chrono_tz::Asia::Shanghai
        .from_local_datetime(&parse_naive(s))
        .single()
        .expect("本地时间不该有歧义（上海无夏令时）")
}

fn fmt(dt: chrono::DateTime<chrono_tz::Tz>) -> String {
    dt.format("%Y-%m-%d %H:%M:%S").to_string()
}

// -------------------------------------------------------------------------
// 时段窗口
// -------------------------------------------------------------------------

#[test]
fn slot_windows_match_the_original() {
    let g = golden();
    assert!(!g.slot_windows.is_empty(), "基准文件不该是空的");

    for w in &g.slot_windows {
        let now = to_bj(&w.now);
        let (ws, we) = alpha_core::time::current_slot_window(now);
        assert_eq!(
            fmt(ws),
            w.window_start,
            "now={} 的窗口起点不对",
            w.now
        );
        assert_eq!(fmt(we), w.window_end, "now={} 的窗口终点不对", w.now);

        let nxt = alpha_core::time::next_slot_start(now);
        assert_eq!(
            fmt(nxt),
            w.next_slot_start,
            "now={} 的下一时段起点不对",
            w.now
        );
    }
}

#[test]
fn is_current_slot_matches_the_original() {
    let g = golden();
    for c in &g.is_current_slot {
        // `is_current_slot` 用的是「现在」，不是基准里的 `now` ——
        // 所以只有那些「相对基准的 now 而言落在窗口内」的判定可以直接比。
        // 这里改成：用基准的 `now` 重算窗口，再看 probe 是否落在里面，
        // 与基准的 expected 对齐。
        //
        // 之所以不能直接调 `is_current_slot(probe)`：那会拿真实的当下
        // 去比，2026-09-22 的 probe 早就过期了。
        let now = to_bj(&c.now);
        let (ws, we) = alpha_core::time::current_slot_window(now);
        let probe = to_bj(&c.probe);
        let got = probe >= ws && probe < we;
        assert_eq!(
            got, c.expected,
            "now={} probe={}({}) 判定不符",
            c.now, c.probe, c.label
        );
    }
}

/// 端点的右开性：`window_end` 那一刻**不属于**该时段。
///
/// 这条单独立一个测试是因为它决定了「时段切换的瞬间会不会重复播报」。
#[test]
fn window_end_is_exclusive() {
    let g = golden();
    let w = g
        .slot_windows
        .iter()
        .find(|w| w.now == "2026-09-22 14:00:00")
        .expect("基准里应当有 14:00 这个探针");

    let now = to_bj(&w.now);
    let (ws, we) = alpha_core::time::current_slot_window(now);
    let end = to_bj(&w.window_end);
    assert!(
        !(end >= ws && end < we),
        "窗口终点必须右开：{end} 不该属于 {ws}..{we}"
    );
}

/// 跨午夜的晚头：起始日应回退一天。
#[test]
fn the_nightly_slot_folds_back_to_the_previous_day() {
    let g = golden();
    let w = g
        .slot_windows
        .iter()
        .find(|w| w.now == "2026-09-22 01:59:59")
        .expect("基准里应当有 01:59:59 这个探针");

    assert_eq!(
        w.window_start, "2026-09-21 20:00:00",
        "凌晨 01:59 属于**前一天** 20:00 开始的晚头"
    );
    assert_eq!(w.window_end, "2026-09-22 02:00:00");
}

// -------------------------------------------------------------------------
// 投票仲裁
// -------------------------------------------------------------------------

#[test]
fn vote_matches_the_original_on_every_case() {
    let g = golden();
    assert!(!g.vote.is_empty(), "基准文件不该是空的");

    for c in &g.vote {
        let hits: Vec<alpha_scheduler::Vote> = c
            .hits
            .iter()
            .map(|h| alpha_scheduler::Vote {
                source_name: h.source.clone(),
                result: alpha_core::FetchResult {
                    status: alpha_core::models::FetchStatus::Hit,
                    boss: Some(alpha_core::models::BossData {
                        name: h.boss.name.clone(),
                        pokedex_id: h.boss.pokedex_id,
                        reported_at: h.boss.reported_at.clone(),
                        ..Default::default()
                    }),
                    dedup_key: String::new(),
                    slot_name: String::new(),
                    slot_name_en: String::new(),
                    message: String::new(),
                },
            })
            .collect();

        let prios: Vec<(String, i64)> =
            c.priorities.iter().map(|(k, v)| (k.clone(), *v)).collect();

        let got = alpha_scheduler::resolve_by_vote(&hits, &prios)
            .unwrap_or_else(|| panic!("用例 {} 不该返回 None", c.case));

        assert_eq!(
            got.source_name, c.expected_source,
            "用例 {} 选中的源不对（原版选 {}）",
            c.case, c.expected_source
        );
    }
}

/// 把基准里最容易翻车的那条单独拎出来，失败信息更直白。
#[test]
fn the_priority_tiebreak_direction_has_not_been_flipped() {
    let g = golden();
    let c = g
        .vote
        .iter()
        .find(|c| c.case == "tie_broken_by_smaller_priority")
        .expect("基准里应当有这条用例");

    // 前置条件校验：这条用例确实是「平票 + 同时间」，
    // 而且优先级数字小的那个才有资格胜出
    let small = c
        .priorities
        .iter()
        .min_by_key(|(_, v)| **v)
        .expect("应当有优先级");
    assert_eq!(
        small.0, &c.expected_source,
        "基准本身自相矛盾：期望的源不是优先级数字最小的那个"
    );

    let hits: Vec<alpha_scheduler::Vote> = c
        .hits
        .iter()
        .map(|h| alpha_scheduler::Vote {
            source_name: h.source.clone(),
            result: alpha_core::FetchResult {
                status: alpha_core::models::FetchStatus::Hit,
                boss: Some(alpha_core::models::BossData {
                    name: h.boss.name.clone(),
                    pokedex_id: h.boss.pokedex_id,
                    reported_at: h.boss.reported_at.clone(),
                    ..Default::default()
                }),
                dedup_key: String::new(),
                slot_name: String::new(),
                slot_name_en: String::new(),
                message: String::new(),
            },
        })
        .collect();
    let prios: Vec<(String, i64)> =
        c.priorities.iter().map(|(k, v)| (k.clone(), *v)).collect();

    let got = alpha_scheduler::resolve_by_vote(&hits, &prios).unwrap();
    assert_eq!(
        got.source_name, c.expected_source,
        "平票决胜的优先级方向反了：期望优先级数字最小的源胜出，实际选了 {}",
        got.source_name
    );
}
