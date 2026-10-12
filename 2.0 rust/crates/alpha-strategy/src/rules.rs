//! 规则加载。
//!
//! 把 `rules.yaml` 里写的技能中文名解析成技能 id。
//! 解析用 id 而不是字符串，好处是：上游 API 给英文、给机翻中文、给别名，
//! 只要能落到同一个 id 就能正确匹配。
//!
//! 解析失败的名字（比如 Pokemmo 特有的叫法）不会被丢掉，
//! 会留一份原名做字符串兜底匹配，保证老行为不退化。
//!
//! 对应原版 `src/strategy/rules.py`。

use std::collections::{HashMap, HashSet};

use alpha_core::error::Result;
use alpha_core::models::BossData;
use alpha_core::pokedex::Pokedex;
use serde::Deserialize;

/// 一组技能的 id 集合 + 解析失败的原名（字符串兜底）。
#[derive(Debug, Clone, Default)]
pub struct MoveGroup {
    pub ids: HashSet<i64>,
    pub unresolved: HashSet<String>,
}

/// `rules.yaml` 的顶层结构。
#[derive(Debug, Clone, Deserialize, Default)]
pub struct RulesDoc {
    #[serde(default)]
    pub version: Option<i64>,
    #[serde(default)]
    pub whitelist: Vec<String>,
    #[serde(default)]
    pub teams: HashMap<String, Vec<String>>,
    #[serde(default)]
    pub anti_prankster_trigger: TriggerDoc,
    #[serde(default)]
    pub pivot_insert: PivotDoc,
    #[serde(default)]
    pub move_groups: HashMap<String, Vec<String>>,
    #[serde(default)]
    pub abilities: HashMap<String, Vec<String>>,
    #[serde(default)]
    pub weather_rules: Vec<WeatherRuleDoc>,
    #[serde(default)]
    pub foresight_required: Vec<String>,
    #[serde(default)]
    pub custom_moves: HashMap<String, CustomMoveDoc>,
    #[serde(default)]
    pub templates: HashMap<String, HashMap<String, String>>,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct TriggerDoc {
    #[serde(default)]
    pub pokemon: Vec<String>,
    #[serde(default)]
    pub abilities: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct PivotDoc {
    #[serde(default)]
    pub move_groups: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct WeatherRuleDoc {
    #[serde(default)]
    pub r#move: String,
    #[serde(default)]
    pub abilities: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct CustomMoveDoc {
    #[serde(default)]
    pub zh: String,
    #[serde(default)]
    pub en: String,
}

/// 引擎直接引用的关键技能（挑衅/临别礼物/中转等）。
pub const KEY_MOVE_NAMES: &[&str] = &[
    "挑衅",
    "特性互换",
    "神秘守护",
    "临别礼物",
    "掉包",
    "治愈之愿",
    "识破",
    "回复封锁",
    "搏命",
    "哈欠",
    "中转",
    "拍手",
];

/// 已加载的规则。
pub struct Rules {
    pub doc: RulesDoc,
    pub whitelist: Vec<String>,
    pub teams: HashMap<String, Vec<String>>,
    pub trigger: TriggerDoc,
    pub pivot_insert_groups: Vec<String>,
    pub abilities: HashMap<String, Vec<String>>,
    pub weather_rules: Vec<WeatherRuleDoc>,
    pub foresight: Vec<String>,
    pub custom_moves: HashMap<String, CustomMoveDoc>,
    pub templates: HashMap<String, HashMap<String, String>>,

    pub groups: HashMap<String, MoveGroup>,

    /// custom move 中文名 → key（输出时按语言取值）
    pub custom_by_zh: HashMap<String, String>,

    /// 关键单技能 → (id, 中文名)
    pub key_moves: HashMap<String, KeyMove>,

    pub ability_swap_ids: HashSet<i64>,
    pub taunt_second_ids: HashSet<i64>,
    pub swap_before_taunt_ids: HashSet<i64>,
    pub prankster_id: Option<i64>,

    /// 天气规则：[(move_id, {ability_id...}), ...]
    pub weather: Vec<(i64, HashSet<i64>)>,

    pub prankster_pokemon_ids: HashSet<i64>,
    pub whitelist_ids: HashSet<i64>,
    pub foresight_ids: HashSet<i64>,

    /// 启动诊断：解析不到 id 的名字
    pub unresolved_keys: Vec<String>,
}

#[derive(Debug, Clone, Default)]
pub struct KeyMove {
    pub id: Option<i64>,
    pub zh: String,
}

impl Rules {
    /// 从 YAML 文本 + 图鉴构造规则。
    pub fn from_yaml(text: &str, pokedex: &Pokedex) -> Result<Self> {
        let doc: RulesDoc = serde_yaml::from_str(text)?;
        Ok(Self::new(doc, pokedex))
    }

    /// 从已解析的文档构造。
    pub fn new(doc: RulesDoc, pokedex: &Pokedex) -> Self {
        let mut unresolved_keys: Vec<String> = Vec::new();

        // 技能分组：中文名 → id 集合
        let mut groups: HashMap<String, MoveGroup> = HashMap::new();
        for (gname, names) in doc.move_groups.iter() {
            groups.insert(gname.clone(), Self::build_group(names, &doc.custom_moves, pokedex));
        }

        // custom move 中文名 → key
        let custom_by_zh: HashMap<String, String> = doc
            .custom_moves
            .iter()
            .map(|(k, v)| {
                let zh = if v.zh.is_empty() { k.clone() } else { v.zh.clone() };
                (zh, k.clone())
            })
            .collect();

        // 关键单技能
        let mut key_moves: HashMap<String, KeyMove> = HashMap::new();
        for cn in KEY_MOVE_NAMES {
            let mid = pokedex.resolve_move_id(cn);
            if mid.is_none() {
                unresolved_keys.push(format!("key_moves: {cn}"));
            }
            key_moves.insert(
                (*cn).to_string(),
                KeyMove {
                    id: mid,
                    zh: (*cn).to_string(),
                },
            );
        }

        let ability_ids = |names: &[String]| -> HashSet<i64> {
            names
                .iter()
                .filter_map(|n| pokedex.resolve_ability_id(n))
                .collect()
        };

        let ability_swap_ids = ability_ids(
            doc.abilities
                .get("require_skill_swap")
                .map(|v| v.as_slice())
                .unwrap_or(&[]),
        );
        let taunt_second_ids = ability_ids(
            doc.abilities
                .get("taunt_second")
                .map(|v| v.as_slice())
                .unwrap_or(&[]),
        );
        let swap_before_taunt_ids = ability_ids(
            doc.abilities
                .get("skill_swap_before_taunt")
                .map(|v| v.as_slice())
                .unwrap_or(&[]),
        );
        let prankster_id = pokedex.resolve_ability_id("恶作剧之心");

        // 天气规则
        let mut weather: Vec<(i64, HashSet<i64>)> = Vec::new();
        for r in doc.weather_rules.iter() {
            if let Some(mid) = pokedex.resolve_move_id(&r.r#move) {
                let aids = ability_ids(&r.abilities);
                if !aids.is_empty() {
                    weather.push((mid, aids));
                }
            }
        }

        let pid_set = |names: &[String]| -> HashSet<i64> {
            names
                .iter()
                .filter_map(|n| pokedex.resolve_pokemon_id(n))
                .collect()
        };
        let prankster_pokemon_ids = pid_set(&doc.anti_prankster_trigger.pokemon);
        let whitelist_ids = pid_set(&doc.whitelist);
        let foresight_ids = pid_set(&doc.foresight_required);

        Self {
            whitelist: doc.whitelist.clone(),
            teams: doc.teams.clone(),
            trigger: doc.anti_prankster_trigger.clone(),
            pivot_insert_groups: doc.pivot_insert.move_groups.clone(),
            abilities: doc.abilities.clone(),
            weather_rules: doc.weather_rules.clone(),
            foresight: doc.foresight_required.clone(),
            custom_moves: doc.custom_moves.clone(),
            templates: doc.templates.clone(),
            doc,
            groups,
            custom_by_zh,
            key_moves,
            ability_swap_ids,
            taunt_second_ids,
            swap_before_taunt_ids,
            prankster_id,
            weather,
            prankster_pokemon_ids,
            whitelist_ids,
            foresight_ids,
            unresolved_keys,
        }
    }

    fn build_group(
        names: &[String],
        customs: &HashMap<String, CustomMoveDoc>,
        pokedex: &Pokedex,
    ) -> MoveGroup {
        let mut g = MoveGroup::default();
        for n in names {
            // custom move（中转/拍手等）没有官方 id，走字符串
            if customs.contains_key(n) {
                g.unresolved.insert(n.clone());
                continue;
            }
            match pokedex.resolve_move_id(n) {
                Some(mid) => {
                    g.ids.insert(mid);
                }
                None => {
                    g.unresolved.insert(n.clone());
                }
            }
        }
        g
    }

    // ---------------- 匹配 ----------------

    /// 头目是否携带该分组里的任一技能。
    pub fn boss_has_group(&self, boss: &BossData, gname: &str) -> bool {
        let Some(g) = self.groups.get(gname) else {
            return false;
        };
        let id_set = boss.move_id_set();
        for mid in g.ids.iter() {
            if id_set.contains(mid) {
                return true;
            }
        }
        for name in g.unresolved.iter() {
            if boss.moves.iter().any(|m| m == name) {
                return true;
            }
        }
        false
    }

    /// 头目是否携带这些分组里的任一技能。
    pub fn boss_has_any_group<S: AsRef<str>>(&self, boss: &BossData, gnames: &[S]) -> bool {
        gnames.iter().any(|g| self.boss_has_group(boss, g.as_ref()))
    }

    /// 取某个关键技能的 id。
    pub fn key_move_id(&self, cn: &str) -> Option<i64> {
        self.key_moves.get(cn).and_then(|k| k.id)
    }

    /// 取某个分组的定义。
    pub fn group(&self, name: &str) -> Option<&MoveGroup> {
        self.groups.get(name)
    }

    /// 取某个模板字段，缺省值由调用方给。
    pub fn tmpl(&self, lang: &str, key: &str, default: &str) -> String {
        self.templates
            .get(lang)
            .and_then(|t| t.get(key))
            .cloned()
            .unwrap_or_else(|| default.to_string())
    }

    // ---------------- 诊断 ----------------

    /// 列出规则里解析不到 id 的名字，便于补齐 aliases.json。
    pub fn report_unresolved(&self) -> Vec<String> {
        let mut out = Vec::new();
        let mut gnames: Vec<&String> = self.groups.keys().collect();
        gnames.sort();
        for gname in gnames {
            let g = &self.groups[gname];
            let mut names: Vec<&String> = g.unresolved.iter().collect();
            names.sort();
            for n in names {
                if !self.custom_moves.contains_key(n) {
                    out.push(format!("move_groups.{gname}: {n}"));
                }
            }
        }
        for k in self.unresolved_keys.iter() {
            if !self.custom_moves.contains_key(k.split(": ").last().unwrap_or(k)) {
                out.push(k.clone());
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn load() -> (Rules, &'static Pokedex) {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .parent()
            .unwrap();
        let text = std::fs::read_to_string(root.join("config/rules.yaml")).unwrap();
        let pk = alpha_core::pokedex::get_pokedex().unwrap();
        (Rules::from_yaml(&text, pk).unwrap(), pk)
    }

    #[test]
    fn rules_load_from_real_config() {
        let (r, _pk) = load();
        // 关键技能都能解析到 id
        assert!(r.key_move_id("挑衅").is_some(), "挑衅 应能解析");
        assert!(r.key_move_id("临别礼物").is_some());
        assert!(r.key_move_id("识破").is_some());
        // 队伍
        assert_eq!(
            r.teams.get("default").unwrap(),
            &vec!["沙奈朵", "长耳兔", "图图犬", "呆壳兽"]
        );
        assert_eq!(
            r.teams.get("anti_prankster").unwrap(),
            &vec!["索罗亚克", "长耳兔", "图图犬", "月亮伊布"]
        );
        // 分组
        assert!(!r.groups.get("defense").unwrap().ids.is_empty());
        assert!(!r.groups.get("boost").unwrap().ids.is_empty());
        assert!(!r.groups.get("priority").unwrap().ids.is_empty());
        // 恶作剧之心 id
        assert!(r.prankster_id.is_some());
        // 自定义技能
        assert_eq!(r.custom_by_zh.get("拍手").map(|s| s.as_str()), Some("拍手"));
        assert_eq!(r.custom_by_zh.get("中转").map(|s| s.as_str()), Some("中转"));
    }

    #[test]
    fn group_matching_by_id() {
        let (r, pk) = load();
        // 龙之舞 在 boost 组
        let dragon_dance = pk.resolve_move_id("龙之舞").unwrap();
        let boss = BossData {
            name: "测试".to_string(),
            moves: vec!["龙之舞".to_string()],
            move_ids: vec![Some(dragon_dance)],
            ..Default::default()
        };
        assert!(r.boss_has_group(&boss, "boost"));
        assert!(!r.boss_has_group(&boss, "defense"));

        // 英文名也能命中（走 id）
        let boss_en = BossData {
            name: "Test".to_string(),
            moves: vec!["Dragon Dance".to_string()],
            move_ids: vec![Some(dragon_dance)],
            ..Default::default()
        };
        assert!(r.boss_has_group(&boss_en, "boost"));
    }

    #[test]
    fn group_matching_string_fallback() {
        let (r, _pk) = load();
        // 「中转」是 custom move，没有官方 id → 走字符串兜底
        let boss = BossData {
            name: "测试".to_string(),
            moves: vec!["中转".to_string()],
            move_ids: vec![None],
            ..Default::default()
        };
        let g = r.group("defense").unwrap();
        // 中转 不在 defense 组，但确认 custom 走 unresolved 的机制可用
        assert!(!g.ids.contains(&0));
        // priority 组里应当没有「中转」
        assert!(!r.boss_has_group(&boss, "priority"));
    }

    #[test]
    fn abilities_resolve() {
        let (r, pk) = load();
        let accel = pk.resolve_ability_id("加速").unwrap();
        assert!(r.ability_swap_ids.contains(&accel));
        assert!(r.swap_before_taunt_ids.contains(&accel));
        let skin = pk.resolve_ability_id("奇迹皮肤").unwrap();
        assert!(r.taunt_second_ids.contains(&skin));
        assert!(r.ability_swap_ids.contains(&skin));
    }

    #[test]
    fn weather_rules_resolve() {
        let (r, pk) = load();
        assert_eq!(r.weather.len(), 4, "应有 4 条天气规则");
        let sunny = pk.resolve_move_id("大晴天").unwrap();
        let chloro = pk.resolve_ability_id("叶绿素").unwrap();
        let found = r.weather.iter().any(|(m, a)| *m == sunny && a.contains(&chloro));
        assert!(found, "大晴天 + 叶绿素 应命中");
    }

    #[test]
    fn whitelist_and_foresight() {
        let (r, pk) = load();
        assert!(r.whitelist_ids.contains(&pk.resolve_pokemon_id("艾路雷朵").unwrap()));
        assert!(r.foresight_ids.contains(&pk.resolve_pokemon_id("勾魂眼").unwrap()));
        assert!(r.prankster_pokemon_ids.contains(&pk.resolve_pokemon_id("利欧路").unwrap()));
        assert!(r.prankster_pokemon_ids.contains(&pk.resolve_pokemon_id("勾魂眼").unwrap()));
        // 恶作剧之心不在 pokemon 名单里，它是特性触发
        assert!(r.trigger.abilities.contains(&"恶作剧之心".to_string()));
    }

    #[test]
    fn template_lookup() {
        let (r, _pk) = load();
        assert_eq!(r.tmpl("zh", "strategy_header", "?"), "打法推荐：");
        assert_eq!(r.tmpl("en", "strategy_header", "?"), "Strategy:");
        assert_eq!(r.tmpl("zh", "moves_label", "?"), "技能: ");
        assert_eq!(r.tmpl("en", "moves_label", "?"), "Moves: ");
        // 缺失字段走默认
        assert_eq!(r.tmpl("zh", "no_such_key", "D"), "D");
    }
}
