//! 源投票仲裁。
//!
//! 多个数据源同时报点，可能各报各的、也可能报同一只头目。这里决定
//! **最终播报哪一条**。
//!
//! # 算法（逐字对齐原版 `src/main.py::resolve_by_vote`）
//!
//! 1. 按 `boss_fingerprint` 分组，同指纹的算「同一只头目」；
//! 2. 只有一组 → 就是它；
//! 3. 否则取票数最多的组；票数最多的只有一组 → 就是它；
//! 4. 还平票 → 组内「最新报点时间」比大小，再比「优先级数字最小」：
//!    排序键是 `(报点时间, -优先级)`，取最大。
//! 5. 最后在选定组内按优先级升序排，取第一个源的 `FetchResult` 来播报。
//!
//! # 第 4 步的排序键极易写反
//!
//! 「时间越新越好」和「优先级数字越小越好」是**两个相反方向**。
//! 时间直接比大小即可，优先级要取负才能统一成「键越大越好」。
//! 如果顺手把优先级也按升序排，平票时就会选中低优先级的源 ——
//! 而这恰恰是「多个源报不同头目」时才暴露的路径，平时测不到。
//! 所以这里单独给平票路径写了测试。
//!
//! # `parse_iso` 失败要退化成「最小值」
//!
//! 原版是 `_parse_iso(r.boss.reported_at) or datetime.min`。注意是
//! **`datetime.min`（naive）**，而解析成功返回的是带时区的。原版
//! Python 比较 naive 与 aware 会直接 `TypeError`——但只要有一个源
//! 报点时间非法、另一个合法，这段代码就会崩。
//!
//! Rust 侧统一用 `DateTime<Utc>::MIN_UTC`：解析失败 = 最旧，
//! 于是「格式合法的源」在平票时稳定胜出。这是**修掉了原版的一个潜在崩溃点**，
//! 行为上则与「原版没崩的那些情况」一致。

use alpha_core::models::FetchResult;
use alpha_core::dedup::boss_fingerprint;
use chrono::{DateTime, Utc};

/// 一个参与投票的源：配置里的名字 + 本轮抓取结果。
#[derive(Debug, Clone)]
pub struct Vote {
    /// 源名（用于查优先级、写日志）
    pub source_name: String,
    pub result: FetchResult,
}

/// 优先级缺失时的兜底值（数字越大越不优先）。
///
/// 原版写的是 `priority_map.get(s["name"], 999)`。这里保持一致 ——
/// 999 而不是 `i64::MAX`，因为原版是拿它做**排序**比较的，
/// 换成极大值虽然结论相同，但会掩盖「优先级缺失」这一事实。
pub const DEFAULT_PRIORITY: i64 = 999;

/// 取源的优先级，缺失时为 [`DEFAULT_PRIORITY`]。
fn priority_of(map: &[(String, i64)], name: &str) -> i64 {
    map.iter()
        .find(|(n, _)| n == name)
        .map(|(_, p)| *p)
        .unwrap_or(DEFAULT_PRIORITY)
}

/// 解析报点时间；失败或缺失 → 最旧。
fn reported_at_or_min(s: &str) -> DateTime<Utc> {
    alpha_core::time::parse_iso(s).unwrap_or(DateTime::<Utc>::MIN_UTC)
}

/// 投票裁决，返回获胜的那一条。
///
/// `hits` 必须非空 —— 调用方在「一个 hit 都没有」时走的是 empty 分支，
/// 不该调到这里。空输入返回 `None` 而不是 panic：调度器循环里
/// 一次 panic 会让整个后台线程静默死掉，比返回 None 难查得多。
pub fn resolve_by_vote(hits: &[Vote], priority_map: &[(String, i64)]) -> Option<Vote> {
    if hits.is_empty() {
        return None;
    }

    // 1) 按指纹分组，保持首次出现顺序（与原版 dict 的插入序一致，
    //    这样「票数相同时谁先出线」是确定的）
    let mut groups: Vec<(String, Vec<&Vote>)> = Vec::new();
    for h in hits {
        let fp = boss_fingerprint(h.result.boss.as_ref());
        match groups.iter_mut().find(|(k, _)| *k == fp) {
            Some((_, v)) => v.push(h),
            None => groups.push((fp, vec![h])),
        }
    }

    // 2) 只有一组 → 直接用它；否则先按票数筛
    let winner_idx = if groups.len() == 1 {
        0
    } else {
        let max_votes = groups.iter().map(|(_, v)| v.len()).max().unwrap_or(0);
        let winners: Vec<usize> = groups
            .iter()
            .enumerate()
            .filter(|(_, (_, v))| v.len() == max_votes)
            .map(|(i, _)| i)
            .collect();

        if winners.len() == 1 {
            winners[0]
        } else {
            // 3) 平票：键 = (最新报点时间, -最小优先级)，取最大
            let mut best: Option<((DateTime<Utc>, i64), usize)> = None;
            for &i in &winners {
                let group = &groups[i].1;
                let rep_dt = group
                    .iter()
                    .map(|v| {
                        reported_at_or_min(
                            v.result
                                .boss
                                .as_ref()
                                .map(|b| b.reported_at.as_str())
                                .unwrap_or(""),
                        )
                    })
                    .max()
                    .unwrap_or(DateTime::<Utc>::MIN_UTC);
                let prio = group
                    .iter()
                    .map(|v| priority_of(priority_map, &v.source_name))
                    .min()
                    .unwrap_or(DEFAULT_PRIORITY);
                // 时间越新、优先级数字越小 → 排序键越大
                let key = (rep_dt, -prio);
                if best.as_ref().map(|(k, _)| key > *k).unwrap_or(true) {
                    best = Some((key, i));
                }
            }
            best.map(|(_, i)| i).unwrap_or(0)
        }
    };

    // 4) 组内按优先级升序（数字小的优先），取第一个
    let mut group = groups[winner_idx].1.clone();
    group.sort_by_key(|v| priority_of(priority_map, &v.source_name));
    group.first().map(|v| (*v).clone())
}

#[cfg(test)]
mod tests {
    use super::*;
    use alpha_core::models::{BossData, FetchStatus};

    fn boss(name: &str, reported_at: &str) -> BossData {
        BossData {
            name: name.to_string(),
            reported_at: reported_at.to_string(),
            ..Default::default()
        }
    }

    fn vote(source: &str, name: &str, reported_at: &str) -> Vote {
        Vote {
            source_name: source.to_string(),
            result: FetchResult {
                status: FetchStatus::Hit,
                boss: Some(boss(name, reported_at)),
                dedup_key: String::new(),
                slot_name: String::new(),
                slot_name_en: String::new(),
                message: String::new(),
            },
        }
    }

    fn prio(pairs: &[(&str, i64)]) -> Vec<(String, i64)> {
        pairs.iter().map(|(a, b)| (a.to_string(), *b)).collect()
    }

    #[test]
    fn empty_input_yields_none() {
        assert!(resolve_by_vote(&[], &[]).is_none());
    }

    #[test]
    fn single_hit_wins() {
        let hits = vec![vote("a", "甲", "2026-09-22T10:00:00+08:00")];
        let got = resolve_by_vote(&hits, &prio(&[("a", 100)])).unwrap();
        assert_eq!(got.source_name, "a");
    }

    #[test]
    fn majority_vote_wins_regardless_of_priority() {
        // 两只头目：乙被两个源报，甲只被一个高优先级源报 → 乙胜
        let hits = vec![
            vote("s1", "甲", "2026-09-22T10:00:00+08:00"),
            vote("s2", "乙", "2026-09-22T10:00:00+08:00"),
            vote("s3", "乙", "2026-09-22T10:00:00+08:00"),
        ];
        let got = resolve_by_vote(&hits, &prio(&[("s1", 1), ("s2", 100), ("s3", 100)])).unwrap();
        assert_eq!(got.result.boss.unwrap().name, "乙");
    }

    // ---- 平票的两条决胜路径 ----

    /// 平票 → 报点时间新的胜出。**即使它的优先级更差。**
    #[test]
    fn tie_is_broken_by_newer_reported_at_first() {
        let hits = vec![
            // 甲：优先级好（1），但报点更旧
            vote("s1", "甲", "2026-09-22T10:00:00+08:00"),
            // 乙：优先级差（900），但报点更新
            vote("s2", "乙", "2026-09-22T10:05:00+08:00"),
        ];
        let got = resolve_by_vote(&hits, &prio(&[("s1", 1), ("s2", 900)])).unwrap();
        assert_eq!(
            got.result.boss.unwrap().name,
            "乙",
            "平票必须先比报点时间，时间更新者胜"
        );
    }

    /// 平票且报点时间相同 → 优先级数字小的胜出。
    ///
    /// 这条专门盯「`-prio` 有没有写反」：写反了这里会选到 s2。
    #[test]
    fn tie_with_equal_time_is_broken_by_smaller_priority_number() {
        let same = "2026-09-22T10:00:00+08:00";
        let hits = vec![vote("s1", "甲", same), vote("s2", "乙", same)];
        let got = resolve_by_vote(&hits, &prio(&[("s1", 10), ("s2", 20)])).unwrap();
        assert_eq!(
            got.result.boss.unwrap().name,
            "甲",
            "报点时间相同时，优先级数字更小的源胜出"
        );
    }

    /// 组内选择：同指纹的多个源里，播报优先级数字最小的那个源的结果。
    #[test]
    fn within_the_winning_group_the_best_priority_source_is_used() {
        let same = "2026-09-22T10:00:00+08:00";
        // 三个源报同一只头目；结果内容不同，用于分辨选了谁
        let mut a = vote("low", "甲", same);
        a.result.message = "来自 low".into();
        let mut b = vote("high", "甲", same);
        b.result.message = "来自 high".into();

        let got = resolve_by_vote(&[a, b], &prio(&[("low", 500), ("high", 3)])).unwrap();
        assert_eq!(got.source_name, "high");
        assert_eq!(got.result.message, "来自 high");
    }

    /// 优先级缺失的源按 999 处理，会输给任何显式低优先级的源。
    #[test]
    fn missing_priority_falls_back_to_999() {
        let same = "2026-09-22T10:00:00+08:00";
        let hits = vec![vote("unlisted", "甲", same), vote("listed", "甲", same)];
        let got = resolve_by_vote(&hits, &prio(&[("listed", 500)])).unwrap();
        assert_eq!(got.source_name, "listed");
    }

    /// 报点时间非法时退化为「最旧」而不是崩溃（原版这里会 TypeError）。
    #[test]
    fn unparsable_reported_at_degrades_to_min_instead_of_panicking() {
        let hits = vec![
            vote("broken", "甲", "不是时间"),
            vote("good", "乙", "2026-09-22T10:00:00+08:00"),
        ];
        let got = resolve_by_vote(&hits, &prio(&[("broken", 1), ("good", 999)])).unwrap();
        assert_eq!(
            got.result.boss.unwrap().name,
            "乙",
            "时间解析失败的源应被视为最旧，让合法时间胜出"
        );
    }

    /// 一边有时间、一边没有，也不能崩。
    #[test]
    fn empty_reported_at_does_not_panic() {
        let hits = vec![vote("a", "甲", ""), vote("b", "乙", "")];
        let got = resolve_by_vote(&hits, &prio(&[("a", 2), ("b", 1)])).unwrap();
        // 时间都是 MIN，退化为比优先级 → b(1) 胜
        assert_eq!(got.result.boss.unwrap().name, "乙");
    }
}
