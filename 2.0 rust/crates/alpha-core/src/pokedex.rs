//! 多语言图鉴。
//!
//! 核心思路：所有名字都先解析成数字 id，再从 id 取目标语言的名字。
//! ```text
//! 「任意语言 / 机翻 / 别名」 → id → 「官方中文 / 官方英文」
//! ```
//!
//! 这样：
//! - 上游 API 给英文（Alphapedia 的 movesEn / abilityEn）→ 直接查 id，绕开机翻
//! - 上游 API 给中文机翻 → 走别名表 → 查 id
//! - 输出想要什么语言，从 id 取就行
//!
//! 对应原版 `src/core/pokedex.py`。

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use once_cell::sync::OnceCell;
use regex::Regex;
use serde::Deserialize;

use crate::error::{CoreError, Result};

/// 蛋组中文 → 英文对照表。
///
/// 图鉴里只存了中文（build_pokedex 只导了中文 prose），
/// 英文播报要出英文蛋组就得有这张表 —— 源站给英文字段时优先用源的，
/// 没给（或源只给中文）时靠这张表兜底。
pub const EGG_GROUP_ZH_TO_EN: &[(&str, &str)] = &[
    ("怪兽", "Monster"),
    ("水中1", "Water 1"),
    ("水中2", "Water 2"),
    ("水中3", "Water 3"),
    ("水中３", "Water 3"), // 图鉴里有全角 ３ 的脏数据，一起认
    ("虫", "Bug"),
    ("飞行", "Flying"),
    ("陆上", "Field"),
    ("妖精", "Fairy"),
    ("植物", "Grass"),
    ("人型", "Human-Like"),
    ("人形", "Human-Like"), // 两种写法都收
    ("矿物", "Mineral"),
    ("不定形", "Amorphous"),
    ("百变怪", "Ditto"),
    ("龙", "Dragon"),
    ("未发现", "Undiscovered"),
];

/// 图鉴条目：中英文名。
#[derive(Debug, Clone, Deserialize, Default)]
pub struct NameEntry {
    #[serde(default)]
    pub zh: String,
    #[serde(default)]
    pub en: String,
}

/// 精灵条目：比通用条目多了性别比例、蛋组、隐藏特性。
#[derive(Debug, Clone, Deserialize, Default)]
pub struct PokemonEntry {
    #[serde(default)]
    pub zh: String,
    #[serde(default)]
    pub en: String,
    #[serde(default)]
    pub gender_rate: Option<f64>,
    #[serde(default)]
    pub egg_groups: Vec<String>,
    #[serde(default)]
    pub hidden_ability: Option<String>,
    #[serde(default)]
    pub hidden_ability_id: Option<i64>,
}

#[derive(Debug, Deserialize, Default)]
struct PokedexMeta {
    #[serde(default)]
    #[allow(dead_code)]
    generated_at: Option<String>,
}

/// 图鉴 JSON 的顶层结构。
#[derive(Debug, Deserialize, Default)]
struct PokedexFile {
    #[serde(default)]
    #[allow(dead_code)]
    meta: Option<PokedexMeta>,
    #[serde(default)]
    pokemon: HashMap<String, PokemonEntry>,
    #[serde(default)]
    abilities: HashMap<String, NameEntry>,
    #[serde(default)]
    moves: HashMap<String, NameEntry>,
}

/// 别名表。
#[derive(Debug, Deserialize, Default)]
struct AliasFile {
    #[serde(default)]
    moves: HashMap<String, String>,
    #[serde(default)]
    abilities: HashMap<String, String>,
    #[serde(default)]
    pokemon: HashMap<String, String>,
}

/// 自定义技能（rules.yaml 的 custom_moves）。
#[derive(Debug, Clone, Deserialize, Default)]
pub struct CustomMove {
    #[serde(default)]
    pub zh: String,
    #[serde(default)]
    pub en: String,
}

/// 归一化：小写、去掉所有非字母数字汉字的字符。
///
/// `"Zap-Cannon"` / `"Zap Cannon"` / `"zapcannon"` 都会变成 `"zapcannon"`。
pub fn norm(s: &str) -> String {
    static RE: OnceCell<Regex> = OnceCell::new();
    let re = RE.get_or_init(|| Regex::new(r"[^0-9a-z一-鿿]").unwrap());
    re.replace_all(&s.to_lowercase(), "").to_string()
}

/// 多语言图鉴。
#[derive(Default)]
pub struct Pokedex {
    pub pokemon: HashMap<String, PokemonEntry>,
    pub abilities: HashMap<String, NameEntry>,
    pub moves: HashMap<String, NameEntry>,

    idx_pokemon: HashMap<String, String>,
    idx_abilities: HashMap<String, String>,
    idx_moves: HashMap<String, String>,

    alias_move: HashMap<String, String>,
    alias_ability: HashMap<String, String>,
    alias_pokemon: HashMap<String, String>,

    custom: HashMap<String, CustomMove>,
}

impl Pokedex {
    /// 从数据文件构造图鉴。
    pub fn load(
        data_path: Option<&Path>,
        aliases_path: Option<&Path>,
        rules_path: Option<&Path>,
    ) -> Result<Self> {
        let data_path = data_path
            .map(PathBuf::from)
            .unwrap_or_else(|| crate::config::data_dir().join("pokedex.json"));
        let aliases_path = aliases_path
            .map(PathBuf::from)
            .unwrap_or_else(|| crate::config::data_dir().join("aliases.json"));

        let raw = std::fs::read_to_string(&data_path).map_err(|e| {
            CoreError::io(format!("读取图鉴失败 {}: {e}", data_path.display()))
        })?;
        let file: PokedexFile = serde_json::from_str(&raw)
            .map_err(|e| CoreError::parse(format!("解析图鉴失败: {e}")))?;

        let aliases: AliasFile = match std::fs::read_to_string(&aliases_path) {
            Ok(t) => serde_json::from_str(&t).unwrap_or_default(),
            Err(_) => AliasFile::default(),
        };

        // custom_moves 里的自定义技能（中转/拍手等），也要能被解析
        let custom = Self::load_custom_moves(rules_path);

        let mut pk = Self {
            idx_pokemon: Self::build_index_pokemon(&file.pokemon),
            idx_abilities: Self::build_index_name(&file.abilities),
            idx_moves: Self::build_index_name(&file.moves),
            alias_move: aliases
                .moves
                .into_iter()
                .map(|(k, v)| (norm(&k), v))
                .collect(),
            alias_ability: aliases
                .abilities
                .into_iter()
                .map(|(k, v)| (norm(&k), v))
                .collect(),
            alias_pokemon: aliases
                .pokemon
                .into_iter()
                .map(|(k, v)| (norm(&k), v))
                .collect(),
            custom,
            pokemon: file.pokemon,
            abilities: file.abilities,
            moves: file.moves,
        };

        // 自定义技能索引：名字 → 官方中文键
        let customs = pk.custom.clone();
        for (key, vals) in customs.iter() {
            for v in [&vals.zh, &vals.en] {
                if !v.is_empty() {
                    pk.idx_moves.entry(norm(v)).or_insert_with(|| key.clone());
                }
            }
        }
        Ok(pk)
    }

    fn load_custom_moves(rules_path: Option<&Path>) -> HashMap<String, CustomMove> {
        let default = crate::config::config_dir().join("rules.yaml");
        let path = rules_path.map(PathBuf::from).unwrap_or(default);
        let text = match std::fs::read_to_string(&path) {
            Ok(t) => t,
            Err(_) => return HashMap::new(),
        };
        #[derive(Deserialize)]
        struct Doc {
            #[serde(default)]
            custom_moves: HashMap<String, CustomMove>,
        }
        serde_yaml::from_str::<Doc>(&text)
            .map(|d| d.custom_moves)
            .unwrap_or_default()
    }

    fn build_index_name(table: &HashMap<String, NameEntry>) -> HashMap<String, String> {
        let mut idx = HashMap::new();
        for (id, names) in table {
            for v in [&names.zh, &names.en] {
                if !v.is_empty() {
                    idx.entry(norm(v)).or_insert_with(|| id.clone());
                }
            }
        }
        idx
    }

    fn build_index_pokemon(table: &HashMap<String, PokemonEntry>) -> HashMap<String, String> {
        let mut idx = HashMap::new();
        for (id, names) in table {
            for v in [&names.zh, &names.en] {
                if !v.is_empty() {
                    idx.entry(norm(v)).or_insert_with(|| id.clone());
                }
            }
        }
        idx
    }

    // ---------------- 解析：名字 → id ----------------

    fn resolve(&self, name: &str, alias: &HashMap<String, String>, idx: &HashMap<String, String>) -> Option<String> {
        if name.is_empty() {
            return None;
        }
        let mut n = norm(name);
        // 1) 先查别名（机翻名 / 老叫法 → 官方中文名）
        if let Some(canon) = alias.get(&n) {
            n = norm(canon);
        }
        // 2) 查正向索引
        idx.get(&n).cloned()
    }

    fn as_int(v: Option<String>) -> Option<i64> {
        v.and_then(|s| s.parse::<i64>().ok())
    }

    pub fn resolve_move_id(&self, name: &str) -> Option<i64> {
        Self::as_int(self.resolve(name, &self.alias_move, &self.idx_moves))
    }

    pub fn resolve_ability_id(&self, name: &str) -> Option<i64> {
        Self::as_int(self.resolve(name, &self.alias_ability, &self.idx_abilities))
    }

    pub fn resolve_pokemon_id(&self, name: &str) -> Option<i64> {
        Self::as_int(self.resolve(name, &self.alias_pokemon, &self.idx_pokemon))
    }

    // ---------------- 归一化：名字 → 官方中文名 ----------------

    pub fn canonical_move(&self, name: &str) -> String {
        match self.resolve_move_id(name) {
            Some(id) => self
                .moves
                .get(&id.to_string())
                .map(|e| e.zh.clone())
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| name.to_string()),
            None => name.to_string(),
        }
    }

    pub fn canonical_ability(&self, name: &str) -> String {
        match self.resolve_ability_id(name) {
            Some(id) => self
                .abilities
                .get(&id.to_string())
                .map(|e| e.zh.clone())
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| name.to_string()),
            None => name.to_string(),
        }
    }

    pub fn canonical_pokemon(&self, name: &str) -> String {
        match self.resolve_pokemon_id(name) {
            Some(id) => self
                .pokemon
                .get(&id.to_string())
                .map(|e| e.zh.clone())
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| name.to_string()),
            None => name.to_string(),
        }
    }

    // ---------------- 本地化：id → 目标语言名 ----------------

    fn name_of(&self, table: &HashMap<String, NameEntry>, id: Option<i64>, lang: &str) -> Option<String> {
        let id = id?;
        let e = table.get(&id.to_string())?;
        let pick = if lang == "en" { &e.en } else { &e.zh };
        if !pick.is_empty() {
            return Some(pick.clone());
        }
        if !e.zh.is_empty() {
            return Some(e.zh.clone());
        }
        if !e.en.is_empty() {
            return Some(e.en.clone());
        }
        None
    }

    pub fn move_name(&self, mid: Option<i64>, lang: &str) -> Option<String> {
        self.name_of(&self.moves, mid, lang)
    }

    pub fn ability_name(&self, aid: Option<i64>, lang: &str) -> Option<String> {
        self.name_of(&self.abilities, aid, lang)
    }

    pub fn pokemon_name(&self, pid: Option<i64>, lang: &str) -> Option<String> {
        // 精灵表结构不同，单独处理
        let id = pid?;
        let e = self.pokemon.get(&id.to_string())?;
        let pick = if lang == "en" { &e.en } else { &e.zh };
        if !pick.is_empty() {
            return Some(pick.clone());
        }
        if !e.zh.is_empty() {
            return Some(e.zh.clone());
        }
        if !e.en.is_empty() {
            return Some(e.en.clone());
        }
        None
    }

    // ---------------- 便捷方法 ----------------

    /// 返回雄性百分比；`None` 表示无性别。
    pub fn gender_of(&self, pid: Option<i64>) -> Option<f64> {
        let id = pid?;
        self.pokemon.get(&id.to_string())?.gender_rate
    }

    /// 取精灵条目的隐藏特性（PokeMMO 头目一律隐藏特性）。
    pub fn hidden_ability(&self, pid: Option<i64>) -> (Option<String>, Option<i64>) {
        let Some(id) = pid else {
            return (None, None);
        };
        match self.pokemon.get(&id.to_string()) {
            Some(e) => (e.hidden_ability.clone(), e.hidden_ability_id),
            None => (None, None),
        }
    }

    /// 取精灵条目的蛋组。
    pub fn egg_groups_of(&self, pid: Option<i64>) -> Vec<String> {
        match pid {
            Some(id) => self
                .pokemon
                .get(&id.to_string())
                .map(|e| e.egg_groups.clone())
                .unwrap_or_default(),
            None => Vec::new(),
        }
    }

    /// 把官方中文名翻译成目标语言。`kind`: move / ability / pokemon
    pub fn translate(&self, canonical_zh: &str, kind: &str, lang: &str) -> String {
        if lang == "zh" || canonical_zh.is_empty() {
            return canonical_zh.to_string();
        }
        match kind {
            "move" => match self.resolve_move_id(canonical_zh) {
                Some(id) => self.move_name(Some(id), lang).unwrap_or_else(|| canonical_zh.to_string()),
                None => canonical_zh.to_string(),
            },
            "ability" => match self.resolve_ability_id(canonical_zh) {
                Some(id) => self
                    .ability_name(Some(id), lang)
                    .unwrap_or_else(|| canonical_zh.to_string()),
                None => canonical_zh.to_string(),
            },
            _ => match self.resolve_pokemon_id(canonical_zh) {
                Some(id) => self
                    .pokemon_name(Some(id), lang)
                    .unwrap_or_else(|| canonical_zh.to_string()),
                None => canonical_zh.to_string(),
            },
        }
    }

    /// 自定义技能（中转/拍手等）的指定语言写法。
    pub fn custom_move(&self, key: &str, lang: &str) -> String {
        match self.custom.get(key) {
            Some(v) => {
                let pick = if lang == "en" { &v.en } else { &v.zh };
                if !pick.is_empty() {
                    pick.clone()
                } else if !v.zh.is_empty() {
                    v.zh.clone()
                } else {
                    key.to_string()
                }
            }
            None => key.to_string(),
        }
    }

    /// 蛋组名的指定语言写法。图鉴只存了中文，英文靠内置对照表。
    pub fn egg_group_name(&self, zh: &str, lang: &str) -> String {
        if lang == "zh" || zh.is_empty() {
            return zh.to_string();
        }
        let t = zh.trim();
        for (k, v) in EGG_GROUP_ZH_TO_EN {
            if *k == t {
                return v.to_string();
            }
        }
        let n = norm(t);
        for (k, v) in EGG_GROUP_ZH_TO_EN {
            if norm(k) == n {
                return v.to_string();
            }
        }
        zh.to_string()
    }

    pub fn egg_groups(&self, groups: &[String], lang: &str) -> Vec<String> {
        groups.iter().map(|g| self.egg_group_name(g, lang)).collect()
    }

    /// 图鉴规模统计。
    pub fn stats(&self) -> PokedexStats {
        PokedexStats {
            pokemon: self.pokemon.len(),
            abilities: self.abilities.len(),
            moves: self.moves.len(),
            custom_moves: self.custom.len(),
        }
    }

    /// 三个平铺列表，供前端的下拉框 / 自动完成渲染。
    ///
    /// 对应原版 `panel/app.py` 的 `pokedex_lists()`。
    ///
    /// 精灵条目带上了 `g`（性别比例）、`eg`（蛋组）、`ha`（隐藏特性）——
    /// 调试页选了精灵之后要自动填这几个字段，
    /// 拉一次列表就能本地下拉，不用每选一只再问一次后端。
    ///
    /// # 顺序
    ///
    /// 按 id 升序。`HashMap` 的迭代顺序是随机的，不排的话
    /// 前端下拉框的顺序每次刷新都在变 —— 用户会以为选错了。
    pub fn lists(&self) -> serde_json::Value {
        fn sorted<T>(map: &HashMap<String, T>) -> Vec<(i64, &T)> {
            let mut v: Vec<(i64, &T)> = map
                .iter()
                .filter_map(|(k, val)| k.parse::<i64>().ok().map(|id| (id, val)))
                .collect();
            v.sort_by_key(|(id, _)| *id);
            v
        }

        let pokemon: Vec<serde_json::Value> = sorted(&self.pokemon)
            .into_iter()
            .map(|(id, e)| {
                serde_json::json!({
                    "id": id,
                    "zh": e.zh,
                    "en": e.en,
                    "g": e.gender_rate,
                    "eg": e.egg_groups,
                    "ha": e.hidden_ability.clone().unwrap_or_default(),
                })
            })
            .collect();

        let names = |map: &HashMap<String, NameEntry>| -> Vec<serde_json::Value> {
            sorted(map)
                .into_iter()
                .map(|(id, v)| serde_json::json!({ "id": id, "zh": v.zh, "en": v.en }))
                .collect()
        };

        serde_json::json!({
            "pokemon": pokemon,
            "abilities": names(&self.abilities),
            "moves": names(&self.moves),
        })
    }
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct PokedexStats {
    pub pokemon: usize,
    pub abilities: usize,
    pub moves: usize,
    pub custom_moves: usize,
}

// ---------------- 全局单例 ----------------

static GLOBAL: OnceCell<Pokedex> = OnceCell::new();

/// 取全局图鉴（首次调用时加载，进程内复用）。
pub fn get_pokedex() -> Result<&'static Pokedex> {
    if GLOBAL.get().is_none() {
        let pk = Pokedex::load(None, None, None)?;
        let _ = GLOBAL.set(pk);
    }
    Ok(GLOBAL.get().expect("pokedex initialized"))
}

/// 强制重载图鉴（配置变更 / 测试用）。
pub fn reload_pokedex() -> Result<&'static Pokedex> {
    // OnceCell 不支持重置，这里用一个可替换的全局锁实现热重载
    use std::sync::RwLock;
    static SWAPPABLE: OnceCell<RwLock<Pokedex>> = OnceCell::new();
    let pk = Pokedex::load(None, None, None)?;
    match SWAPPABLE.get() {
        Some(lock) => {
            *lock.write().unwrap() = pk;
        }
        None => {
            let _ = SWAPPABLE.set(RwLock::new(pk));
        }
    }
    Ok(GLOBAL.get_or_init(|| Pokedex::load(None, None, None).unwrap_or_default()))
}


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn norm_works() {
        assert_eq!(norm("Zap-Cannon"), "zapcannon");
        assert_eq!(norm("Zap Cannon"), "zapcannon");
        assert_eq!(norm("zapcannon"), "zapcannon");
        assert_eq!(norm("电磁波"), "电磁波");
        assert_eq!(norm(""), "");
    }

    #[test]
    fn egg_group_translation() {
        let pk = Pokedex::default();
        assert_eq!(pk.egg_group_name("矿物", "en"), "Mineral");
        assert_eq!(pk.egg_group_name("怪兽", "en"), "Monster");
        // 全角 ３ 的脏数据也要认
        assert_eq!(pk.egg_group_name("水中３", "en"), "Water 3");
        // 中文直接返回
        assert_eq!(pk.egg_group_name("矿物", "zh"), "矿物");
        // 未知原样返回
        assert_eq!(pk.egg_group_name("未知蛋组", "en"), "未知蛋组");
    }

    #[test]
    fn loads_real_pokedex() {
        // 依赖仓库内的 data/pokedex.json（测试时 cwd 为 crate 根）
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .parent()
            .unwrap();
        let data = root.join("data/pokedex.json");
        if !data.exists() {
            eprintln!("跳过：未找到 {}", data.display());
            return;
        }
        let pk = Pokedex::load(
            Some(&data),
            Some(&root.join("data/aliases.json")),
            Some(&root.join("config/rules.yaml")),
        )
        .unwrap();

        // 自爆磁怪 = 462
        assert_eq!(pk.resolve_pokemon_id("自爆磁怪"), Some(462));
        assert_eq!(pk.resolve_pokemon_id("Magnezone"), Some(462));
        // 别名：挥指功 → 挥指
        assert!(pk.resolve_move_id("挥指功").is_some());
        assert_eq!(pk.resolve_move_id("挥指功"), pk.resolve_move_id("挥指"));
        // 别名：呆河马 → 呆壳兽
        assert_eq!(pk.resolve_pokemon_id("呆河马"), pk.resolve_pokemon_id("呆壳兽"));
        // 自定义技能
        assert_eq!(pk.custom_move("拍手", "en"), "Encore");
        assert_eq!(pk.custom_move("中转", "en"), "Pivot");
        // 规范名
        assert_eq!(pk.canonical_pokemon("Magnezone"), "自爆磁怪");

        let s = pk.stats();
        assert!(s.pokemon > 1000, "pokemon={}", s.pokemon);
        assert!(s.moves > 900, "moves={}", s.moves);
    }
}
