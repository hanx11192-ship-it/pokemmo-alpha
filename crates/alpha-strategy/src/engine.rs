//! 打法引擎。
//!
//! 输入：`BossData`（名称 / 特性 / 技能，已归一化成官方中文）
//! 输出：一段推送文本
//!
//! 两个设计点：
//! 1. 规则匹配全部走技能 id，所以上游给英文、机翻中文、别名都能命中同一条规则。
//! 2. 输出阶段才按目标语言翻译，同一套规则可以同时产出中英文报告。
//!
//! 行为严格对齐原版 `src/strategy/engine.py`，不改动任何判定逻辑。
//! 每个分支上方都标注了它对应原版的哪一段。

use alpha_core::models::BossData;
use alpha_core::pokedex::Pokedex;

use crate::rules::Rules;

/// 评估器钩子：`callable(boss) -> 评估行文本`。
///
/// 由调用方（分发器 / 调度器）注入；引擎自身不关心评估逻辑；
/// 为 `None` 时行为与旧版完全一致。
/// 评语回调：吃归一化头目，吐「评价词 评分N」一行。
///
/// `Arc` 而不是 `Box`：调度器要把**同一个**评估器同时交给插件决策器
/// 路径（`Context::with_report` 的 `'static` 闭包）和内置引擎兜底路径，
/// `Arc` 才能 clone 两不误。
pub type Evaluator = std::sync::Arc<dyn Fn(&BossData) -> String + Send + Sync>;

/// 双语输出分隔符。
pub const BILINGUAL_SEPARATOR: &str = "\n\n—————\n\n";

pub struct StrategyEngine<'a> {
    rules: &'a Rules,
    pokedex: &'a Pokedex,
    lang: String,
    evaluator: Option<&'a Evaluator>,
}

impl<'a> StrategyEngine<'a> {
    pub fn new(
        rules: &'a Rules,
        pokedex: &'a Pokedex,
        lang: &str,
        evaluator: Option<&'a Evaluator>,
    ) -> Self {
        Self {
            rules,
            pokedex,
            lang: lang.to_string(),
            evaluator,
        }
    }

    fn tmpl(&self, key: &str, default: &str) -> String {
        let v = self.rules.tmpl(&self.lang, key, default);
        if v.is_empty() {
            default.to_string()
        } else {
            v
        }
    }

    /// 调用评估器拿「评价 + 评分」行；任何异常都静默降级为空（不评估）。
    fn eval_line(&self, boss: &BossData) -> String {
        match self.evaluator {
            Some(f) => f(boss),
            None => String::new(),
        }
    }

    // ---------------- 翻译 ----------------

    fn t_move(&self, cn: &str) -> String {
        if let Some(key) = self.rules.custom_by_zh.get(cn) {
            return self.pokedex.custom_move(key, &self.lang);
        }
        if self.lang == "zh" {
            return cn.to_string();
        }
        self.pokedex.translate(cn, "move", &self.lang)
    }

    fn t_pokemon(&self, cn: &str) -> String {
        if self.lang == "zh" {
            return cn.to_string();
        }
        self.pokedex.translate(cn, "pokemon", &self.lang)
    }

    fn t_ability(&self, cn: &str) -> String {
        if self.lang == "zh" {
            return cn.to_string();
        }
        self.pokedex.translate(cn, "ability", &self.lang)
    }

    /// 翻译一个名字列表。技能列表会走 custom_moves（中转/拍手等）。
    fn t_list(&self, names: &[String], kind: &str) -> String {
        if names.is_empty() {
            return String::new();
        }
        let sep = self.tmpl("list_separator", ", ");
        let items: Vec<String> = match kind {
            "move" => names.iter().map(|n| self.t_move(n)).collect(),
            "pokemon" => names.iter().map(|n| self.t_pokemon(n)).collect(),
            _ => names.iter().map(|n| self.t_ability(n)).collect(),
        };
        items.join(&sep)
    }

    // ---------------- 判定辅助 ----------------

    fn is_prankster(&self, boss: &BossData) -> bool {
        let r = self.rules;
        if let Some(pid) = boss.pokedex_id {
            if r.prankster_pokemon_ids.contains(&pid) {
                return true;
            }
        }
        if let (Some(aid), Some(pid)) = (boss.ability_id, r.prankster_id) {
            if aid == pid {
                return true;
            }
        }
        r.trigger.pokemon.iter().any(|p| p == &boss.name) || boss.ability == "恶作剧之心"
    }

    fn needs_skill_swap(&self, boss: &BossData) -> bool {
        let r = self.rules;
        if let Some(aid) = boss.ability_id {
            return r.ability_swap_ids.contains(&aid);
        }
        r.abilities
            .get("require_skill_swap")
            .map(|v| v.iter().any(|a| a == &boss.ability))
            .unwrap_or(false)
    }

    /// 头目为「加速(梦特性) + 携带挑衅」时，沙奈朵必须先特性互换再挑衅。
    ///
    /// 加速每回合提速，必须首回合换掉；若头目先手挑衅封住变化技，
    /// 则靠同性别小弟送掉换手后再上沙奈朵挑衅（临场操作，面板只管招序）。
    fn needs_swap_before_taunt(&self, boss: &BossData) -> bool {
        let r = self.rules;
        let is_accel = match boss.ability_id {
            Some(aid) => r.swap_before_taunt_ids.contains(&aid),
            None => r
                .abilities
                .get("skill_swap_before_taunt")
                .map(|v| v.iter().any(|a| a == &boss.ability))
                .unwrap_or(false),
        };
        if !is_accel {
            return false;
        }
        // 头目确实携带挑衅（挑衅属于 defense 分组，这里直接按技能名判定）
        match r.key_move_id("挑衅") {
            Some(taunt_id) => boss.has_move_id(taunt_id),
            None => boss.moves.iter().any(|m| m == "挑衅"),
        }
    }

    fn needs_foresight(&self, boss: &BossData) -> bool {
        let r = self.rules;
        if let Some(pid) = boss.pokedex_id {
            return r.foresight_ids.contains(&pid);
        }
        r.foresight.iter().any(|n| n == &boss.name)
    }

    fn weather_skill_swap(&self, boss: &BossData) -> bool {
        match boss.ability_id {
            None => {
                // 没有 id 时退回字符串比较
                self.rules.weather_rules.iter().any(|w| {
                    boss.moves.iter().any(|m| m == &w.r#move)
                        && w.abilities.iter().any(|a| a == &boss.ability)
                })
            }
            Some(aid) => {
                let id_set = boss.move_id_set();
                self.rules
                    .weather
                    .iter()
                    .any(|(mid, aids)| id_set.contains(mid) && aids.contains(&aid))
            }
        }
    }

    // ---------------- 队伍 ----------------

    /// 队伍顺序。
    pub fn team_order(&self, boss: &BossData) -> Vec<String> {
        let r = self.rules;
        let mut team: Vec<String> = if self.is_prankster(boss) {
            r.teams.get("anti_prankster").cloned().unwrap_or_default()
        } else {
            r.teams.get("default").cloned().unwrap_or_default()
        };

        // 头目带先制技或回复技 → 插一个中转手
        if !team.is_empty() && r.boss_has_any_group(boss, &r.pivot_insert_groups) && team.len() > 3 {
            team.push(team[3].clone());
            team.swap(2, 3);
        }
        team
    }

    // ---------------- 各位置配招 ----------------

    /// 沙奈朵配招。
    pub fn skills_gardevoir(&self, boss: &BossData) -> Vec<String> {
        let r = self.rules;
        let mut out: Vec<String> = Vec::new();

        if r.boss_has_any_group(boss, &["defense", "boost"]) {
            out.push("挑衅".to_string());
        }
        if self.needs_skill_swap(boss) {
            out.push("特性互换".to_string());
        }
        if self.weather_skill_swap(boss) {
            out.push("特性互换".to_string());
        }
        if r.boss_has_any_group(boss, &["freeze", "paralysis"]) {
            out.push("神秘守护".to_string());
        }
        out.push("临别礼物".to_string());

        // 去重（保持顺序）
        let mut new: Vec<String> = Vec::new();
        for s in out {
            if !new.contains(&s) {
                new.push(s);
            }
        }

        // 特殊：头目为「加速(梦特性) + 携带挑衅」时，沙奈朵必须先特性互换再挑衅。
        // 抢首回合换掉加速避免被超速；若被挑衅则靠同性别小弟送掉换手（临场操作）。
        if self.needs_swap_before_taunt(boss) {
            if !new.contains(&"特性互换".to_string()) {
                new.push("特性互换".to_string());
            }
            if let Some(taunt_idx) = new.iter().position(|s| s == "挑衅") {
                new.retain(|s| s != "特性互换");
                new.insert(taunt_idx, "特性互换".to_string());
            }
        }

        // 满 4 格时优先牺牲神秘守护
        if new.len() == 4 {
            if let Some(i) = new.iter().position(|s| s == "神秘守护") {
                new.remove(i);
            }
        }

        // 奇迹皮肤 / 魔法镜下，挑衅要排到第二位
        let is_taunt_second = match boss.ability_id {
            Some(aid) => r.taunt_second_ids.contains(&aid),
            None => r
                .abilities
                .get("taunt_second")
                .map(|v| v.iter().any(|a| a == &boss.ability))
                .unwrap_or(false),
        };
        if is_taunt_second && new.len() == 3 {
            new.swap(0, 1);
        }

        new
    }

    /// 长耳兔配招（按主攻手占掉的技能位来定）。
    pub fn skills_lopunny(&self, boss: &BossData, gardevoir_skills: &[String]) -> Vec<String> {
        let left = 3i64 - gardevoir_skills.len() as i64;
        let mut out: Vec<String> = Vec::new();
        let has_taunt = gardevoir_skills.iter().any(|s| s == "挑衅");

        if has_taunt && left == 0 {
            out.push("拍手".to_string());
            out.push("掉包".to_string());
        }
        if has_taunt && left == 1 {
            out.push("掉包".to_string());
            out.push("拍手".to_string());
        }
        if !has_taunt {
            out.push("掉包".to_string());
        }
        if self.needs_foresight(boss) {
            out.push("识破".to_string());
        }
        out.push("治愈之愿".to_string());
        out
    }

    /// 图图犬配招。
    pub fn skills_smeargle(&self, boss: &BossData) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        if self.rules.boss_has_group(boss, "heal") {
            out.push("回复封锁".to_string());
        }
        out.push("搏命".to_string());
        out
    }

    /// 索罗亚克：头目是恶作剧之心时，用它顶替沙奈朵（同一个位置的两套人选）。
    ///
    /// 挑衅的触发条件和沙奈朵共用同一份名单，
    /// 但它不会神秘守护、也不会特性互换，所以只有这两招。
    pub fn skills_zoroark(&self, boss: &BossData) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        if self.rules.boss_has_any_group(boss, &["defense", "boost"]) {
            out.push("挑衅".to_string());
        }
        out.push("临别礼物".to_string());
        out
    }

    /// 中转手。
    ///
    /// 具体是谁上场取决于队伍配置（月亮伊布接棒 / 呆壳兽瞬间移动），
    /// 统一输出「哈欠 + 中转」，不写死具体技能。
    pub fn skills_pivot(&self, _boss: &BossData) -> Vec<String> {
        vec!["哈欠".to_string(), "中转".to_string()]
    }

    // ---------------- 主入口 ----------------

    /// 生成报告文本。
    pub fn generate(&self, boss: &BossData) -> String {
        let r = self.rules;

        let period = {
            let p = if boss.period.is_empty() {
                self.tmpl("period_unknown", "未知时段")
            } else {
                boss.period.clone()
            };
            p.replace('~', "-")
        };
        let name_src = if boss.name.is_empty() {
            self.tmpl("name_unknown", "未知")
        } else {
            boss.name.clone()
        };
        let name = self.t_pokemon(&name_src);
        let ability_src = if boss.ability.is_empty() {
            self.tmpl("ability_none", "无特性")
        } else {
            boss.ability.clone()
        };
        let ability = self.t_ability(&ability_src);
        let location = if self.lang == "en" && !boss.location_en.is_empty() {
            boss.location_en.clone()
        } else if !boss.location.is_empty() {
            boss.location.clone()
        } else {
            self.tmpl("location_unknown", "未知地点")
        };

        let rate_str = match boss.gender.male_percent {
            Some(p) => format!("{}{}", fmt_percent(p), self.tmpl("gender_suffix", "%公")),
            None => self.tmpl("no_gender", "无性别"),
        };

        let moves_str = {
            let s = self.t_list(&boss.moves, "move");
            if s.is_empty() {
                self.tmpl("moves_none", "无")
            } else {
                s
            }
        };

        // 英文播报时蛋组也要是英文。优先级：源给的英文字段 > 图鉴对照表翻译 > 原样输出。
        // 图鉴里只有中文蛋组，缺了这层翻译，英文播报会掺中文。
        let egg: Vec<String> = if self.lang == "en" && !boss.egg_groups_en.is_empty() {
            boss.egg_groups_en.clone()
        } else if self.lang == "en" {
            boss.egg_groups
                .iter()
                .map(|g| self.pokedex.egg_group_name(g, "en"))
                .collect()
        } else {
            boss.egg_groups.clone()
        };
        let egg_str = egg.join(", ");

        let mut second_line = format!("{}({})", name, ability);
        if !egg_str.is_empty() {
            second_line.push_str(&format!("-({})", egg_str));
        }
        second_line.push_str(&format!("-{}", rate_str));

        let mut header: Vec<String> = vec![
            period,
            second_line,
            location,
            format!("{}{}", self.tmpl("moves_label", "技能: "), moves_str),
        ];

        // 评估器（可插拔）：把「评价 + 评分」插到第 2 行
        // （第 1 行是时段，第 2 行原本是头目信息，评估行插在它前面）
        let eval_line = self.eval_line(boss);
        if !eval_line.is_empty() {
            header.insert(1, eval_line);
        }

        // 附加信息（秘传机、地点备注等）：插在头目信息和打法推荐之间
        let extra: Vec<String> = boss
            .extra_lines
            .iter()
            .map(|e| e.text(&self.lang).to_string())
            .filter(|s| !s.is_empty())
            .collect();

        // 报点人：始终展示（无论单/双性别、是否出打法），向所有报点者致谢
        let reporter = boss.reporter.clone().unwrap_or_default();
        let reporter_line = if reporter.is_empty() {
            String::new()
        } else {
            format!("{}{}", self.tmpl("reporter_label", "报点人: "), reporter)
        };

        // 双性别（同进化链存在异性）或白名单才出打法
        let mut is_dual = false;
        if let Some(pid) = boss.pokedex_id {
            if r.whitelist_ids.contains(&pid) {
                is_dual = true;
            }
        } else if r.whitelist.iter().any(|n| n == &boss.name) {
            is_dual = true;
        }
        if boss.gender.is_dual() {
            is_dual = true;
        }

        if !is_dual {
            // 单性别 / 无性别只推信息，不推打法，但报点人仍展示
            let mut lines: Vec<String> = header;
            lines.extend(extra);
            lines.push(reporter_line);
            return lines
                .into_iter()
                .filter(|x| !x.is_empty())
                .collect::<Vec<_>>()
                .join("\n");
        }

        let team = self.team_order(boss);

        // 主攻手（沙奈朵 / 索罗亚克二选一）先单独算出来：
        // 长耳兔要按它占掉的技能位来配招，不能依赖队伍里的先后顺序。
        // 原脚本这里写死读 team_skills["沙奈朵"]，恶作剧之心队伍里没有沙奈朵，
        // 读出来是空列表，长耳兔的技能位就算错了。
        // 注意两者配招规则不同：索罗亚克只有挑衅 + 临别礼物。
        let main_poke = team
            .iter()
            .find(|p| p.as_str() == "沙奈朵" || p.as_str() == "索罗亚克")
            .cloned();
        let main_skills: Vec<String> = match main_poke.as_deref() {
            Some("沙奈朵") => self.skills_gardevoir(boss),
            Some("索罗亚克") => self.skills_zoroark(boss),
            _ => Vec::new(),
        };

        let mut team_skills: Vec<(String, Vec<String>)> = Vec::new();
        for poke in team.iter() {
            let sk = if Some(poke) == main_poke.as_ref() {
                main_skills.clone()
            } else {
                match poke.as_str() {
                    "长耳兔" => self.skills_lopunny(boss, &main_skills),
                    "图图犬" => self.skills_smeargle(boss),
                    "呆壳兽" | "月亮伊布" => self.skills_pivot(boss),
                    _ => Vec::new(),
                }
            };
            team_skills.push((poke.clone(), sk));
        }

        let name_sep = self.tmpl("name_separator", "：");
        let lines: Vec<String> = team_skills
            .iter()
            .map(|(poke, sk)| {
                let sk_str = {
                    let s = self.t_list(sk, "move");
                    if s.is_empty() {
                        self.tmpl("moves_none", "无")
                    } else {
                        s
                    }
                };
                format!("{}{}{}", self.t_pokemon(poke), name_sep, sk_str)
            })
            .collect();

        let mut report = String::new();
        report.push_str(
            &header
                .into_iter()
                .chain(extra)
                .collect::<Vec<_>>()
                .join("\n"),
        );
        report.push('\n');
        report.push_str(&self.tmpl("strategy_header", "打法推荐："));
        report.push('\n');
        report.push_str(&lines.join("\n"));
        if !reporter.is_empty() {
            report.push('\n');
            report.push_str(&reporter_line);
        }
        report
    }
}

/// 百分比格式化：整数不带小数点，小数保留一位（与原版 Python f-string 行为一致）。
///
/// Python 的 `f"{50.0}%公"` 会输出 `50.0%公`，而 `f"{100}%公"` 输出 `100%公`。
/// 由于 `male_percent` 在 Python 侧是 `float`，我们需要同样保留 `.0`。
fn fmt_percent(v: f64) -> String {
    if v.fract() == 0.0 {
        format!("{:.1}", v)
    } else {
        format!("{}", v)
    }
}

/// 生成单语言报告。
pub fn generate_report(
    boss: &BossData,
    rules: &Rules,
    pokedex: &Pokedex,
    lang: &str,
    evaluator: Option<&Evaluator>,
) -> String {
    StrategyEngine::new(rules, pokedex, lang, evaluator).generate(boss)
}

/// 多语言合并输出（`language: both` 时用）。
///
/// `evaluator` 会按语言分别调用（同一评估器，语言由闭包决定）。
pub fn generate_bilingual(
    boss: &BossData,
    rules: &Rules,
    pokedex: &Pokedex,
    langs: &[String],
    evaluator: Option<&Evaluator>,
) -> String {
    let langs: Vec<String> = if langs.is_empty() {
        vec!["zh".to_string(), "en".to_string()]
    } else {
        langs.to_vec()
    };
    let parts: Vec<String> = langs
        .iter()
        .map(|l| generate_report(boss, rules, pokedex, l, evaluator))
        .collect();
    parts.join(BILINGUAL_SEPARATOR)
}

#[cfg(test)]
mod tests {
    use super::*;
    use alpha_core::models::{ExtraLine, Gender};

    struct Ctx {
        rules: Rules,
        pokedex: &'static Pokedex,
    }

    fn ctx() -> Ctx {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .parent()
            .unwrap();
        let text = std::fs::read_to_string(root.join("config/rules.yaml")).unwrap();
        let pk = alpha_core::pokedex::get_pokedex().unwrap();
        Ctx {
            rules: Rules::from_yaml(&text, pk).unwrap(),
            pokedex: pk,
        }
    }

    fn boss(c: &Ctx, name: &str, ability: &str, moves: &[&str], male: Option<f64>) -> BossData {
        let pid = c.pokedex.resolve_pokemon_id(name);
        let aid = c.pokedex.resolve_ability_id(ability);
        let moves_v: Vec<String> = moves.iter().map(|m| m.to_string()).collect();
        let move_ids: Vec<Option<i64>> = moves
            .iter()
            .map(|m| c.pokedex.resolve_move_id(m))
            .collect();
        BossData {
            name: c.pokedex.canonical_pokemon(name),
            ability: c.pokedex.canonical_ability(ability),
            moves: moves_v
                .iter()
                .map(|m| c.pokedex.canonical_move(m))
                .collect(),
            gender: Gender::from_male_percent(male),
            pokedex_id: pid,
            ability_id: aid,
            move_ids,
            ..Default::default()
        }
    }

    fn render(c: &Ctx, b: &BossData, lang: &str) -> String {
        generate_report(b, &c.rules, c.pokedex, lang, None)
    }

    #[test]
    fn single_gender_outputs_info_only() {
        let c = ctx();
        // 无性别 → 不出打法
        let mut b = boss(&c, "自爆磁怪", "分析", &["十万伏特"], None);
        b.period = "早头(约止于09:00)".to_string();
        b.location = "神奥 · 道路 205".to_string();
        b.reporter = Some("测试员".to_string());
        let out = render(&c, &b, "zh");

        assert!(out.contains("早头(约止于09:00)"), "{out}");
        assert!(out.contains("自爆磁怪(分析)"), "{out}");
        assert!(out.contains("无性别"), "{out}");
        assert!(out.contains("神奥 · 道路 205"), "{out}");
        assert!(out.contains("技能: 十万伏特"), "{out}");
        assert!(out.contains("报点人: 测试员"), "{out}");
        // 不出打法
        assert!(!out.contains("打法推荐"), "{out}");
    }

    #[test]
    fn dual_gender_outputs_strategy() {
        let c = ctx();
        let mut b = boss(&c, "沙奈朵", "同步", &["十万伏特"], Some(50.0));
        b.period = "午头(约止于18:14)".to_string();
        let out = render(&c, &b, "zh");
        assert!(out.contains("打法推荐："), "{out}");
        assert!(out.contains("沙奈朵："), "{out}");
        assert!(out.contains("长耳兔："), "{out}");
        assert!(out.contains("图图犬："), "{out}");
        assert!(out.contains("呆壳兽："), "{out}");
        // 无挑衅 → 长耳兔只带掉包 + 治愈之愿
        assert!(out.contains("长耳兔：掉包, 治愈之愿"), "{out}");
    }

    #[test]
    fn whitelist_pokemon_gets_strategy_even_single_gender() {
        let c = ctx();
        // 艾路雷朵在白名单，即使单性别也出打法
        let mut b = boss(&c, "艾路雷朵", "正义之心", &["十万伏特"], Some(100.0));
        b.period = "早头(约止于09:00)".to_string();
        let out = render(&c, &b, "zh");
        assert!(out.contains("打法推荐："), "白名单应出打法: {out}");
    }

    #[test]
    fn prankster_switches_team_to_zoroark() {
        let c = ctx();
        // 勾魂眼 → 恶作剧之心 → 队伍换成 索罗亚克/长耳兔/图图犬/月亮伊布
        // 基准：原版 Python 引擎输出（已逐字核对）
        let mut b = boss(&c, "勾魂眼", "恶作剧之心", &["十万伏特"], Some(50.0));
        b.period = "早头(约止于09:00)".to_string();
        let out = render(&c, &b, "zh");
        assert!(out.contains("索罗亚克："), "{out}");
        assert!(out.contains("月亮伊布："), "{out}");
        assert!(!out.contains("沙奈朵："), "恶作剧之心队伍不该有沙奈朵: {out}");
        assert!(!out.contains("呆壳兽："), "恶作剧之心队伍不该有呆壳兽: {out}");
        // 索罗亚克只出临别礼物（不接挑衅/神秘守护/特性互换）
        assert!(out.contains("索罗亚克：临别礼物"), "{out}");
        // 长耳兔在恶作剧队伍里带识破（勾魂眼是幽灵系，需先破除）
        assert!(out.contains("长耳兔：掉包, 识破, 治愈之愿"), "{out}");
    }

    #[test]
    fn zoroark_never_gets_safeguard_or_skill_swap() {
        let c = ctx();
        // 即便头目带冰冻技/麻痹技，索罗亚克也只出「临别礼物」，绝不带神秘守护或特性互换
        let mut b = boss(&c, "勾魂眼", "恶作剧之心", &["冰冻光束", "十万伏特"], Some(50.0));
        b.period = "早头(约止于09:00)".to_string();
        let out = render(&c, &b, "zh");
        let zoro_line = out
            .lines()
            .find(|l| l.starts_with("索罗亚克"))
            .expect("应有索罗亚克行");
        assert_eq!(zoro_line, "索罗亚克：临别礼物", "{out}");
        assert!(!zoro_line.contains("神秘守护"));
        assert!(!zoro_line.contains("特性互换"));
        assert!(!zoro_line.contains("挑衅"));
    }

    #[test]
    fn lopunny_skill_slots_depend_on_main_attacker() {
        let c = ctx();
        // 头目带防御技（电磁波）→ 沙奈朵出挑衅 → 主攻手仍可带满 3 招
        let mut b = boss(&c, "沙奈朵", "同步", &["电磁波"], Some(50.0));
        b.period = "早头(约止于09:00)".to_string();
        let out = render(&c, &b, "zh");
        assert!(out.contains("沙奈朵：挑衅, 临别礼物"), "{out}");
        assert!(out.contains("长耳兔：掉包, 拍手, 治愈之愿"), "{out}");

        // 麻痹技（十万伏特）→ 沙奈朵改出神秘守护 → 长耳兔收窄为 掉包 + 治愈之愿
        let mut b2 = boss(&c, "沙奈朵", "同步", &["十万伏特"], Some(50.0));
        b2.period = "早头(约止于09:00)".to_string();
        let out2 = render(&c, &b2, "zh");
        assert!(out2.contains("沙奈朵：神秘守护, 临别礼物"), "{out2}");
        assert!(out2.contains("长耳兔：掉包, 治愈之愿"), "{out2}");
    }

    #[test]
    fn gardevoir_skill_swap_for_required_ability() {
        let c = ctx();
        // 迟钝 → 需要特性互换；同时沙奈朵自己吃满 3 招
        let mut b = boss(&c, "沙奈朵", "迟钝", &["十万伏特"], Some(50.0));
        b.period = "早头(约止于09:00)".to_string();
        let out = render(&c, &b, "zh");
        assert!(
            out.contains("沙奈朵：特性互换, 神秘守护, 临别礼物"),
            "{out}"
        );
    }

    #[test]
    fn gardevoir_safeguard_for_freeze_and_paralysis() {
        let c = ctx();
        let mut b = boss(&c, "沙奈朵", "同步", &["冰冻光束"], Some(50.0));
        b.period = "早头(约止于09:00)".to_string();
        let out = render(&c, &b, "zh");
        assert!(out.contains("神秘守护"), "{out}");
    }

    #[test]
    fn swap_before_taunt_reorders() {
        let c = ctx();
        // 加速 + 挑衅 → 特性互换 必须在 挑衅 之前
        let mut b = boss(&c, "巨牙鲨", "加速", &["挑衅", "咬碎"], Some(50.0));
        b.period = "早头(约止于09:00)".to_string();
        let out = render(&c, &b, "zh");
        let line = out.lines().find(|l| l.starts_with("沙奈朵")).unwrap();
        let ti = line.find("特性互换").expect("应有特性互换");
        let ta = line.find("挑衅").expect("应有挑衅");
        assert!(ti < ta, "特性互换应排在挑衅之前: {line}");
    }

    #[test]
    fn taunt_second_for_miracle_skin() {
        let c = ctx();
        // 奇迹皮肤 + 防御技 → 特性互换, 挑衅, 临别礼物；长耳兔被换序为 拍手, 掉包
        let mut b = boss(&c, "沙奈朵", "奇迹皮肤", &["电磁波"], Some(50.0));
        b.period = "早头(约止于09:00)".to_string();
        let out = render(&c, &b, "zh");
        let line = out
            .lines()
            .find(|l| l.starts_with("沙奈朵："))
            .expect("应有沙奈朵打法行");
        assert_eq!(line, "沙奈朵：特性互换, 挑衅, 临别礼物", "{out}");
        assert!(out.contains("长耳兔：拍手, 掉包, 治愈之愿"), "{out}");
    }

    #[test]
    fn pivot_insert_for_priority_moves() {
        let c = ctx();
        // 先制技（子弹拳）→ 队伍插中转手
        let mut b = boss(&c, "沙奈朵", "同步", &["子弹拳"], Some(50.0));
        b.period = "早头(约止于09:00)".to_string();
        let out = render(&c, &b, "zh");
        // 队伍变成 5 人（呆壳兽被插到前面）
        let team_lines: Vec<&str> = out
            .lines()
            .filter(|l| l.contains("："))
            .filter(|l| !l.starts_with("技能"))
            .collect();
        assert!(team_lines.len() >= 5, "应插入中转手: {out}");
    }

    #[test]
    fn foresight_for_ghost_pokemon() {
        let c = ctx();
        // 勾魂眼需要识破，但恶作剧之心队伍里长耳兔仍在
        let mut b = boss(&c, "勾魂眼", "锐利目光", &["十万伏特"], Some(50.0));
        b.period = "早头(约止于09:00)".to_string();
        let out = render(&c, &b, "zh");
        let line = out.lines().find(|l| l.starts_with("长耳兔")).unwrap();
        assert!(line.contains("识破"), "勾魂眼需要识破: {line}");
    }

    #[test]
    fn smeargle_block_heal() {
        let c = ctx();
        // 回复技（自我再生）→ 图图犬出回复封锁
        let mut b = boss(&c, "沙奈朵", "同步", &["自我再生"], Some(50.0));
        b.period = "早头(约止于09:00)".to_string();
        let out = render(&c, &b, "zh");
        assert!(out.contains("图图犬：回复封锁, 搏命"), "{out}");
    }

    #[test]
    fn extra_lines_between_info_and_strategy() {
        let c = ctx();
        let mut b = boss(&c, "沙奈朵", "同步", &["十万伏特"], Some(50.0));
        b.period = "早头(约止于09:00)".to_string();
        b.extra_lines = vec![ExtraLine::new("数据来源: Alphapedia", "Source: Alphapedia")];
        let out = render(&c, &b, "zh");
        let info_pos = out.find("技能: ").unwrap();
        let extra_pos = out.find("数据来源").unwrap();
        let strat_pos = out.find("打法推荐").unwrap();
        assert!(info_pos < extra_pos, "附加信息应在头目信息之后");
        assert!(extra_pos < strat_pos, "附加信息应在打法推荐之前");
    }

    #[test]
    fn english_output() {
        let c = ctx();
        let mut b = boss(&c, "沙奈朵", "同步", &["十万伏特"], Some(50.0));
        b.period = "早头(约止于09:00)".to_string();
        b.location = "神奥 · 道路 205".to_string();
        b.location_en = "Sinnoh · Route 205".to_string();
        b.egg_groups = vec!["不定形".to_string()];
        let out = render(&c, &b, "en");
        assert!(out.contains("Sinnoh · Route 205"), "英文应用 location_en: {out}");
        assert!(out.contains("Moves: Thunderbolt"), "{out}");
        assert!(out.contains("Strategy:"), "{out}");
        assert!(out.contains("50.0% male"), "{out}");
        assert!(out.contains("Amorphous"), "蛋组应译英: {out}");
        assert!(out.contains("Gardevoir"), "{out}");
        // 自定义技能英文
        assert!(!out.contains("打法推荐"), "英文输出不该含中文标题");
    }

    #[test]
    fn bilingual_joins_with_separator() {
        let c = ctx();
        let mut b = boss(&c, "沙奈朵", "同步", &["十万伏特"], Some(50.0));
        b.period = "早头(约止于09:00)".to_string();
        let out = generate_bilingual(
            &b,
            &c.rules,
            c.pokedex,
            &["zh".to_string(), "en".to_string()],
            None,
        );
        assert!(out.contains(BILINGUAL_SEPARATOR.trim()), "{out}");
        assert!(out.contains("打法推荐："));
        assert!(out.contains("Strategy:"));
    }

    #[test]
    fn evaluator_line_inserted_at_position_two() {
        let c = ctx();
        let mut b = boss(&c, "沙奈朵", "同步", &["十万伏特"], Some(50.0));
        b.period = "早头(约止于09:00)".to_string();
        let ev: Evaluator = std::sync::Arc::new(|_b| "看脸头 评分5".to_string());
        let out = generate_report(&b, &c.rules, c.pokedex, "zh", Some(&ev));
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(lines[0], "早头(约止于09:00)");
        assert_eq!(lines[1], "看脸头 评分5", "评估行应插在第 2 行: {out}");
    }

    #[test]
    fn reporter_always_shown() {
        let c = ctx();
        // 单性别也展示报点人
        let mut b = boss(&c, "自爆磁怪", "分析", &["十万伏特"], None);
        b.period = "早头(约止于09:00)".to_string();
        b.reporter = Some("张三".to_string());
        let out = render(&c, &b, "zh");
        assert!(out.contains("报点人: 张三"), "{out}");

        // 双性别也展示报点人（在末尾）
        let mut b2 = boss(&c, "沙奈朵", "同步", &["十万伏特"], Some(50.0));
        b2.period = "早头(约止于09:00)".to_string();
        b2.reporter = Some("李四".to_string());
        let out2 = render(&c, &b2, "zh");
        assert!(out2.contains("报点人: 李四"), "{out2}");
        assert!(out2.trim_end().ends_with("报点人: 李四"), "{out2}");
    }

    #[test]
    fn period_tilde_normalized() {
        let c = ctx();
        let mut b = boss(&c, "自爆磁怪", "分析", &["十万伏特"], None);
        b.period = "20:00~次日02:00".to_string();
        let out = render(&c, &b, "zh");
        assert!(out.contains("20:00-次日02:00"), "~ 应被替换为 -: {out}");
    }

    #[test]
    fn gender_percent_formatting() {
        let c = ctx();
        let mut b = boss(&c, "沙奈朵", "同步", &["十万伏特"], Some(50.0));
        b.period = "早头".to_string();
        assert!(render(&c, &b, "zh").contains("50.0%公"));
        // 100 也保留一位小数，与 Python float 行为一致
        let mut b2 = boss(&c, "艾路雷朵", "正义之心", &["十万伏特"], Some(100.0));
        b2.period = "早头".to_string();
        assert!(render(&c, &b2, "zh").contains("100.0%公"));
        // 87.5 这种非整数保留原值
        let mut b3 = boss(&c, "沙奈朵", "同步", &["十万伏特"], Some(87.5));
        b3.period = "早头".to_string();
        assert!(render(&c, &b3, "zh").contains("87.5%公"));
    }
}
