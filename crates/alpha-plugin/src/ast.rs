//! 脚本可见的数据视图（AST）。
//!
//! # 为什么不让脚本直接操作 Rust 结构体
//!
//! 把 `BossData` 直接注册进 Rhai 意味着：
//!
//! 1. **每个字段都要包一层 getter**，改 Rust 侧字段名就得同步改脚本 API ——
//!    ABI 会随着内部重构而漂移；
//! 2. 脚本能拿到 `Rules`/`Pokedex` 的引用后，可以做的事情远超「读数据」，
//!    沙箱边界变得很难说清；
//! 3. 出错时的报错信息会指向 Rust 类型名，对写脚本的人毫无帮助。
//!
//! 所以这里做一层**显式的只读视图**：脚本看到的是一组 JSON 风格的
//! `Map`，字段名就是 ABI，与 Rust 内部结构解耦。
//!
//! # 技能的双重表示
//!
//! 原版脚本既能按**技能 id** 匹配（`boss.has_move_id(mid)`），也能按
//! **中文名**兜底（`if n in boss.moves`）。Rhai 侧保留了这一点：
//!
//! - `ctx.resolve_move_id("挑衅")` → 数字 id 或 `()`（解析不出来）
//! - `ctx.move_id_at(i)` → 第 i 个技能的 id（可能为 `()`）
//! - `boss.moves` → 中文名数组
//!
//! 这样「先按 id 匹配、id 拿不到就比字符串」的原版逻辑可以原样改写。

use rhai::{Dynamic, Map};
use serde::{Deserialize, Serialize};

use alpha_core::models::{BossData, ExtraLine};

/// 一条附加信息（`extra_lines` 的元素）。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ExtraLineView {
    pub zh: String,
    pub en: String,
}

impl From<&ExtraLine> for ExtraLineView {
    fn from(e: &ExtraLine) -> Self {
        Self {
            zh: e.zh.clone(),
            en: e.en.clone(),
        }
    }
}

/// 脚本看到的头目数据。
///
/// 字段全部 `pub`，因为这**就是** ABI —— 改名等于破坏兼容，
/// 应该有意识地对待，而不是顺手重构掉。
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct BossView {
    /// 中文名
    pub name: String,
    /// 英文名（可能为空）
    pub name_en: String,
    /// 特性（中文）
    pub ability: String,
    pub ability_id: Option<i64>,
    /// 技能中文名列表
    pub moves: Vec<String>,
    /// 与 `moves` 等长；解析不出来的位置是 `None`
    pub move_ids: Vec<Option<i64>>,
    /// 地点（中文）
    pub location: String,
    pub location_en: String,
    /// 时段文本，如 `14:30~15:45`
    pub period: String,
    /// 性别比例文本，如 `50.0%公`
    pub gender: String,
    pub egg_groups: Vec<String>,
    pub egg_groups_en: Vec<String>,
    /// 全国图鉴号（可能为空）
    pub pokedex_id: Option<i64>,
    /// 源给的附加信息
    pub extra_lines: Vec<ExtraLineView>,
    /// 来源适配器名
    pub source: String,
    /// 报点时间
    pub reported_at: String,
    /// 报点人（可能为空）
    pub reporter: String,
}

impl BossView {
    /// 从归一化数据构造视图。
    pub fn from_boss(b: &BossData) -> Self {
        Self {
            name: b.name.clone(),
            // 中文名即 `name`；英文名由图鉴翻译得到，脚本侧用 `ctx.pokemon_name_en` 取
            name_en: String::new(),
            ability: b.ability.clone(),
            ability_id: b.ability_id,
            moves: b.moves.clone(),
            move_ids: b.move_ids.clone(),
            location: b.location.clone(),
            location_en: b.location_en.clone(),
            period: b.period.clone(),
            gender: gender_text(&b.gender),
            egg_groups: b.egg_groups.clone(),
            egg_groups_en: b.egg_groups_en.clone(),
            pokedex_id: b.pokedex_id,
            extra_lines: b.extra_lines.iter().map(ExtraLineView::from).collect(),
            source: b.source.clone(),
            reported_at: b.reported_at.clone(),
            reporter: b.reporter.clone().unwrap_or_default(),
        }
    }
}

/// 性别比例文案，与原版 `f"{male_percent}{t.get('gender_suffix', '%公')}"` 对齐。
///
/// 原版是**直接拼接浮点数**，所以 50.0 会渲染成 `50.0%公`（保留一位小数）。
/// 这里必须复现这个细节，否则报文里的性别行会与原版不一致。
fn gender_text(g: &alpha_core::models::Gender) -> String {
    match g.male_percent {
        // Python 的 f"{50.0}" 渲染成 "50.0"，Rust 的 `{}` 会丢成 "50" ——
        // 必须 `:.1` 才能逐字对齐（性别过滤解析两者皆可，但报文细节要对）。
        Some(p) => format!("{p:.1}%公"),
        None => String::new(),
    }
}

/// 把 [`BossView`] 转成 Rhai 的 `Map`。
///
/// 手写而不是依赖 `serde` 自动转换，是为了**控制可选值的表示**：
/// `Option::None` 一律转成 Rhai 的 `()`（unit），脚本里用
/// `if id == () { ... }` 判断，与原版 Python 的 `if mid is None` 一一对应。
/// 若交给 serde，`None` 会变成「字段不存在」，脚本取字段会直接报错。
pub fn boss_to_rhai(b: &BossView) -> Map {
    let mut m = Map::new();
    m.insert("name".into(), Dynamic::from(b.name.clone()));
    m.insert("name_en".into(), Dynamic::from(b.name_en.clone()));
    m.insert("ability".into(), Dynamic::from(b.ability.clone()));
    m.insert("ability_id".into(), opt_i64(b.ability_id));
    m.insert(
        "moves".into(),
        Dynamic::from_iter(b.moves.iter().cloned()),
    );
    m.insert(
        "move_ids".into(),
        Dynamic::from_iter(b.move_ids.iter().map(|x| opt_i64(*x))),
    );
    m.insert("location".into(), Dynamic::from(b.location.clone()));
    m.insert("location_en".into(), Dynamic::from(b.location_en.clone()));
    m.insert("period".into(), Dynamic::from(b.period.clone()));
    m.insert("gender".into(), Dynamic::from(b.gender.clone()));
    m.insert(
        "egg_groups".into(),
        Dynamic::from_iter(b.egg_groups.iter().cloned()),
    );
    m.insert(
        "egg_groups_en".into(),
        Dynamic::from_iter(b.egg_groups_en.iter().cloned()),
    );
    m.insert("pokedex_id".into(), opt_i64(b.pokedex_id));
    m.insert(
        "extra_lines".into(),
        Dynamic::from_iter(b.extra_lines.iter().map(|e| {
            let mut em = Map::new();
            em.insert("zh".into(), Dynamic::from(e.zh.clone()));
            em.insert("en".into(), Dynamic::from(e.en.clone()));
            Dynamic::from_map(em)
        })),
    );
    m.insert("source".into(), Dynamic::from(b.source.clone()));
    m.insert("reported_at".into(), Dynamic::from(b.reported_at.clone()));
    m.insert("reporter".into(), Dynamic::from(b.reporter.clone()));
    m
}

/// `Option<i64>` → Rhai `Dynamic`（`None` 变 unit）。
fn opt_i64(v: Option<i64>) -> Dynamic {
    match v {
        Some(n) => Dynamic::from(n),
        None => Dynamic::UNIT,
    }
}

/// Rhai `Map` → [`BossView`]。
///
/// # 为什么需要反向转换
///
/// 内建决策器把活儿交回 Rust 引擎（`engine_report`），而引擎要的是
/// `BossData` 而不是 Rhai 的 Map。与其为「引擎调用」再注册一堆 getter
/// 让脚本自己拼参数，不如在这里转一次。
///
/// # 容错取向
///
/// 字段缺失或类型不符一律**取默认值**而不是报错。理由：这个函数是
/// 被沙箱内的脚本间接触发的，报错信息最终会显示成「插件运行失败」，
/// 而真正的原因（脚本传了个残缺的 map）用户很难自己定位。
/// 缺字段时退化成「信息不全的报告」至少还能看，比整条推送失败强。
pub fn boss_from_rhai(m: &Map) -> BossView {
    BossView {
        name: get_str(m, "name"),
        name_en: get_str(m, "name_en"),
        ability: get_str(m, "ability"),
        ability_id: get_opt_i64(m, "ability_id"),
        moves: get_str_vec(m, "moves"),
        move_ids: get_opt_i64_vec(m, "move_ids"),
        location: get_str(m, "location"),
        location_en: get_str(m, "location_en"),
        period: get_str(m, "period"),
        gender: get_str(m, "gender"),
        egg_groups: get_str_vec(m, "egg_groups"),
        egg_groups_en: get_str_vec(m, "egg_groups_en"),
        pokedex_id: get_opt_i64(m, "pokedex_id"),
        extra_lines: get_extra_lines(m, "extra_lines"),
        source: get_str(m, "source"),
        reported_at: get_str(m, "reported_at"),
        reporter: get_str(m, "reporter"),
    }
}

/// [`BossView`] → `BossData`（喂回 Rust 引擎用）。
pub fn boss_view_to_boss_data(v: &BossView) -> BossData {
    BossData {
        name: v.name.clone(),
        ability: v.ability.clone(),
        ability_id: v.ability_id,
        moves: v.moves.clone(),
        move_ids: v.move_ids.clone(),
        period: v.period.clone(),
        location: v.location.clone(),
        location_en: v.location_en.clone(),
        reporter: if v.reporter.is_empty() {
            None
        } else {
            Some(v.reporter.clone())
        },
        gender: alpha_core::models::Gender::from_male_percent(parse_male_percent(&v.gender)),
        egg_groups: v.egg_groups.clone(),
        egg_groups_en: v.egg_groups_en.clone(),
        extra_lines: v
            .extra_lines
            .iter()
            .map(|e| ExtraLine::new(e.zh.clone(), e.en.clone()))
            .collect(),
        pokedex_id: v.pokedex_id,
        source: v.source.clone(),
        reported_at: v.reported_at.clone(),
    }
}

/// 从 `"50.0%公"` 里抠出 `50.0`。
///
/// 原版这个值是浮点数，渲染成文案后才丢掉类型信息。往回解析是为了让
/// 「内建决策器交给引擎」这条路上，性别信息不至于在转换中丢失。
fn parse_male_percent(text: &str) -> Option<f64> {
    if text.is_empty() {
        return None;
    }
    let digits: String = text
        .chars()
        .take_while(|c| c.is_ascii_digit() || *c == '.')
        .collect();
    digits.parse().ok()
}

fn get_str(m: &Map, key: &str) -> String {
    m.get(key)
        .and_then(|v| v.clone().try_cast::<String>())
        .unwrap_or_default()
}

fn get_opt_i64(m: &Map, key: &str) -> Option<i64> {
    let v = m.get(key)?;
    if v.is_unit() {
        return None;
    }
    v.clone()
        .try_cast::<i64>()
        .or_else(|| v.clone().try_cast::<f64>().map(|f| f as i64))
}

fn get_str_vec(m: &Map, key: &str) -> Vec<String> {
    let Some(v) = m.get(key) else {
        return Vec::new();
    };
    if v.is_unit() {
        return Vec::new();
    }
    v.clone()
        .try_cast::<rhai::Array>()
        .map(|a| {
            a.iter()
                .filter_map(|x| x.clone().try_cast::<String>())
                .collect()
        })
        .unwrap_or_default()
}

fn get_opt_i64_vec(m: &Map, key: &str) -> Vec<Option<i64>> {
    let Some(v) = m.get(key) else {
        return Vec::new();
    };
    if v.is_unit() {
        return Vec::new();
    }
    v.clone()
        .try_cast::<rhai::Array>()
        .map(|a| {
            a.iter()
                .map(|x| {
                    if x.is_unit() {
                        None
                    } else {
                        x.clone()
                            .try_cast::<i64>()
                            .or_else(|| x.clone().try_cast::<f64>().map(|f| f as i64))
                    }
                })
                .collect()
        })
        .unwrap_or_default()
}

fn get_extra_lines(m: &Map, key: &str) -> Vec<ExtraLineView> {
    let Some(v) = m.get(key) else {
        return Vec::new();
    };
    let Some(arr) = v.clone().try_cast::<rhai::Array>() else {
        return Vec::new();
    };
    arr.iter()
        .filter_map(|x| x.clone().try_cast::<Map>())
        .map(|em| ExtraLineView {
            zh: get_str(&em, "zh"),
            en: get_str(&em, "en"),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn none_becomes_unit_not_missing_field() {
        let b = BossView {
            name: "测试".into(),
            pokedex_id: None,
            move_ids: vec![Some(1), None],
            ..Default::default()
        };
        let m = boss_to_rhai(&b);

        // 字段必须存在，值是 unit
        let pid = m.get("pokedex_id").expect("字段必须存在");
        assert!(pid.is_unit(), "None 应转成 unit，实际是 {pid:?}");

        let ids = m.get("move_ids").unwrap().clone().cast::<rhai::Array>();
        assert_eq!(ids.len(), 2);
        assert!(ids[1].is_unit(), "解析不出的技能 id 应是 unit");
    }

    #[test]
    fn all_documented_fields_are_present() {
        let m = boss_to_rhai(&BossView::default());
        // 这些是脚本 ABI，少一个都会让用户脚本报「字段不存在」
        for key in [
            "name", "name_en", "ability", "ability_id", "moves", "move_ids",
            "location", "location_en", "period", "gender", "egg_groups",
            "egg_groups_en", "pokedex_id", "extra_lines", "source",
            "reported_at", "reporter",
        ] {
            assert!(m.contains_key(key), "缺少 ABI 字段: {key}");
        }
    }

    #[test]
    fn extra_lines_carry_both_languages() {
        let b = BossView {
            extra_lines: vec![ExtraLineView {
                zh: "秘传技需求: 冲浪".into(),
                en: "HMs required: Surf".into(),
            }],
            ..Default::default()
        };
        let arr = boss_to_rhai(&b)
            .get("extra_lines")
            .unwrap()
            .clone()
            .cast::<rhai::Array>();
        let first = arr[0].clone().cast::<Map>();
        assert_eq!(first.get("zh").unwrap().clone().cast::<String>(), "秘传技需求: 冲浪");
        assert_eq!(
            first.get("en").unwrap().clone().cast::<String>(),
            "HMs required: Surf"
        );
    }

    /// 往返转换必须无损 —— 漏字段会让「内建决策器交给引擎」那条路
    /// 悄悄丢掉头目信息，而报文看起来只是「少了几行」，极难察觉。
    #[test]
    fn roundtrip_through_rhai_preserves_every_field() {
        let original = BossView {
            name: "沙奈朵".into(),
            name_en: "Gardevoir".into(),
            ability: "奇迹皮肤".into(),
            ability_id: Some(119),
            moves: vec!["特性互换".into(), "挑衅".into()],
            move_ids: vec![Some(285), Some(269)],
            location: "冠军之路".into(),
            location_en: "Victory Road".into(),
            period: "14:30~15:45".into(),
            gender: "50.0%公".into(),
            egg_groups: vec!["不定形".into()],
            egg_groups_en: vec!["Amorphous".into()],
            pokedex_id: Some(282),
            extra_lines: vec![ExtraLineView {
                zh: "秘传技需求: 冲浪".into(),
                en: "HMs required: Surf".into(),
            }],
            source: "lzpoke".into(),
            reported_at: "2026-09-22 14:30:00".into(),
            reporter: "hanyx".into(),
        };

        let back = boss_from_rhai(&boss_to_rhai(&original));
        assert_eq!(back, original, "往返转换有字段丢失或改变");
    }

    /// 缺字段时要降级成默认值而不是 panic。
    #[test]
    fn missing_fields_degrade_to_defaults() {
        let empty = Map::new();
        let v = boss_from_rhai(&empty);
        assert_eq!(v, BossView::default());

        // 类型不对也不能 panic（脚本可能传了个字符串当 id）
        let mut bad = Map::new();
        bad.insert("name".into(), Dynamic::from(123_i64));
        bad.insert("pokedex_id".into(), Dynamic::from("不是数字"));
        let v = boss_from_rhai(&bad);
        assert_eq!(v.name, "", "类型不符应取默认值");
        assert_eq!(v.pokedex_id, None);
    }

    /// 性别文案 `50.0%公` 必须能解析回浮点 —— 否则喂回引擎时性别会丢。
    #[test]
    fn gender_percent_parses_back() {
        assert_eq!(parse_male_percent("50.0%公"), Some(50.0));
        assert_eq!(parse_male_percent("12.5%公"), Some(12.5));
        assert_eq!(parse_male_percent("0%公"), Some(0.0));
        assert_eq!(parse_male_percent("100.0%公"), Some(100.0));
        assert_eq!(parse_male_percent(""), None);
        assert_eq!(parse_male_percent("无性别"), None);
    }

    /// 往返后性别要真的还原成引擎认识的形式。
    #[test]
    fn gender_survives_roundtrip_to_boss_data() {
        let v = BossView {
            name: "皮卡丘".into(),
            gender: "50.0%公".into(),
            ..Default::default()
        };
        let bd = boss_view_to_boss_data(&v);
        assert_eq!(bd.gender.male_percent, Some(50.0));
        assert!(bd.gender.is_dual());

        // 无性别时是 None，且 `is_dual` 为假
        let v = BossView {
            name: "未知".into(),
            ..Default::default()
        };
        let bd = boss_view_to_boss_data(&v);
        assert_eq!(bd.gender.male_percent, None);
        assert!(!bd.gender.is_dual());
    }

    #[test]
    fn extra_lines_survive_roundtrip_to_boss_data() {
        let v = BossView {
            extra_lines: vec![ExtraLineView {
                zh: "zh 文本".into(),
                en: "en text".into(),
            }],
            ..Default::default()
        };
        let bd = boss_view_to_boss_data(&v);
        assert_eq!(bd.extra_lines.len(), 1);
        assert_eq!(bd.extra_lines[0].text("zh"), "zh 文本");
        assert_eq!(bd.extra_lines[0].text("en"), "en text");
    }

    /// 空 reporter 要还原成 `None`，而不是 `Some("")` ——
    /// 后者会让引擎以为「有报点人但名字是空的」。
    #[test]
    fn empty_reporter_becomes_none() {
        let v = BossView {
            reporter: String::new(),
            ..Default::default()
        };
        assert_eq!(boss_view_to_boss_data(&v).reporter, None);

        let v = BossView {
            reporter: "hanyx".into(),
            ..Default::default()
        };
        assert_eq!(boss_view_to_boss_data(&v).reporter.as_deref(), Some("hanyx"));
    }

    /// 技能 id 数组里的 unit（源没解析出来）往返后仍是 None。
    #[test]
    fn unresolved_move_ids_survive_roundtrip() {
        let v = BossView {
            moves: vec!["挑衅".into(), "生僻技能".into()],
            move_ids: vec![Some(269), None],
            ..Default::default()
        };
        let back = boss_from_rhai(&boss_to_rhai(&v));
        assert_eq!(back.move_ids, vec![Some(269), None]);
        assert_eq!(back.moves, vec!["挑衅", "生僻技能"]);
    }
}
