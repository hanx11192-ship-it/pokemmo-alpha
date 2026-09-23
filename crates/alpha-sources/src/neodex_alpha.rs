//! NEODEX Alpha 源（`neodex.tools`，GraphQL）。
//!
//! # 数据来源（单步 GraphQL，公开，无需登录 Cookie）
//!
//! ```graphql
//! POST https://neodex.tools/graphql
//! query lastAlpha(language) -> 最近一次出现的 Alpha 头目记录
//! ```
//!
//! # 为什么需要「过期 + 时段」双验证（关键修复）
//!
//! `lastAlpha` 返回的是「**最近一次**出现的头目」，并不是「当前一定在线的头目」。
//! PokeMMO 的 Alpha 每天分 4 个时段（北京 02-08 / 08-14 / 14-20 / 20-次日02），
//! 每个时段最多 1 个头目，随机时刻刷新，仅持续约 75 分钟，其余时间是空档。
//! neodex.tools 在头目消失后**不会清空** `lastAlpha`，因此：
//!
//! > 空档期 / 跨时段时，`lastAlpha` 仍返回「上一个已过期的头目」。
//!
//! 仅靠「拿到 lastAlpha 就当当前头目报」会误报（例如 14:00 还在报 8-14 时段的旧头目）。
//! 本适配器据此加两道闸：
//!
//! 1. **过期闸**：`now(UTC) < despawnAt` 才算未过期；
//! 2. **时段闸**：当前北京时段必须落在头目的「出现时段 / 消失时段」之一，
//!    排除「上一时段 spillover 过来、但已不属于当前时段」的残留记录。
//!
//! 两闸任一不通过即返回 `empty`（面板不会误推送、也不会清不掉旧显示）。
//!
//! # 源站技能缺失的处理（重要约定）
//!
//! neodex.tools 的 `lastAlpha.moves` 经常为空（源站数据质量问题）。
//! 本适配器**不**回退到其它数据源（pokemmotools 有自己的适配器）。规则：
//!
//! > 双验证通过、但 `moves` 为空 -> 视为「无命中」，返回 `empty`，不推送。
//!
//! 即：宁可本时段不报，也不报一个缺技能的残空头目。

use std::collections::HashMap;

use alpha_core::config::{Config, SourceOptions};
use alpha_core::models::{BossData, ExtraLine, FetchResult, Gender};
use alpha_core::pokedex::Pokedex;
use alpha_core::time::{
    fmt_duration, now_beijing, parse_iso, slot_name_en, slot_name_of_hour, to_beijing,
};
use chrono::{Duration, Utc};
use serde_json::{json, Value};

use crate::base::{err_result, HttpClient};
use crate::pokemmotools_landing::{region_zh, translate_location};
use crate::{DataSource, SourceError};

pub const NAME: &str = "neodex_alpha";

const GRAPHQL_URL: &str = "https://neodex.tools/graphql";

const QUERY: &str = r#"query($language:String){
  lastAlpha(language:$language){
    species
    region
    location
    types
    kind
    pokedexId
    reportedAt
    despawnAt
    despawnExact
    moves{ name }
  }
}"#;

/// 18 属性：英文 -> 官方中文。查不到的属性原样保留（不丢信息）。
pub fn type_zh(t: &str) -> String {
    match t.to_uppercase().as_str() {
        "NORMAL" => "一般",
        "FIRE" => "火",
        "WATER" => "水",
        "ELECTRIC" => "电",
        "GRASS" => "草",
        "ICE" => "冰",
        "FIGHTING" => "格斗",
        "POISON" => "毒",
        "GROUND" => "地面",
        "FLYING" => "飞行",
        "PSYCHIC" => "超能力",
        "BUG" => "虫",
        "ROCK" => "岩石",
        "GHOST" => "幽灵",
        "DRAGON" => "龙",
        "DARK" => "恶",
        "STEEL" => "钢",
        "FAIRY" => "妖精",
        other => return other.to_string(),
    }
    .to_string()
}

/// 双验证的判定结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Liveness {
    /// 未过期且属于当前时段
    Active,
    /// `despawnAt` 已过
    Expired,
    /// 属于其它时段（跨时段残留）
    WrongSlot,
    /// 缺少消失时间字段，无法验证
    NoDespawn,
}

/// 双验证明细，供日志与测试使用。
#[derive(Debug, Clone)]
pub struct JudgeOutcome {
    pub liveness: Liveness,
    pub despawn_at: Option<chrono::DateTime<Utc>>,
    /// 剩余秒数（负数表示已过期）
    pub remaining_sec: i64,
    pub current_slot: &'static str,
    pub current_slot_range: &'static str,
    /// 头目可能属于的时段集合（出现时段 + 消失时段）
    pub candidate_slots: Vec<&'static str>,
}

/// 「过期 + 时段」双验证（纯函数，`now` 由调用方注入，方便测试）。
pub fn judge(reported_at: Option<&str>, despawn_at: Option<&str>, now: chrono::DateTime<Utc>) -> JudgeOutcome {
    let des = despawn_at.and_then(parse_iso);
    let rep = reported_at.and_then(parse_iso);
    let now_bj = to_beijing(now);
    let (cur_name, cur_rng) = slot_name_of_hour(chrono::Timelike::hour(&now_bj));

    let Some(des) = des else {
        // 无消失时间，无法验证活跃 -> 保守视为非当前
        return JudgeOutcome {
            liveness: Liveness::NoDespawn,
            despawn_at: None,
            remaining_sec: 0,
            current_slot: cur_name,
            current_slot_range: cur_rng,
            candidate_slots: Vec::new(),
        };
    };

    let remaining = (des - now).num_seconds();
    let mut cand = Vec::new();
    if let Some(r) = rep {
        cand.push(slot_name_of_hour(chrono::Timelike::hour(&to_beijing(r))).0);
    }
    cand.push(slot_name_of_hour(chrono::Timelike::hour(&to_beijing(des))).0);
    cand.sort_unstable();
    cand.dedup();

    let liveness = if remaining <= 0 {
        Liveness::Expired
    } else if cand.contains(&cur_name) {
        Liveness::Active
    } else {
        Liveness::WrongSlot
    };

    JudgeOutcome {
        liveness,
        despawn_at: Some(des),
        remaining_sec: remaining,
        current_slot: cur_name,
        current_slot_range: cur_rng,
        candidate_slots: cand,
    }
}

pub struct NeodexAlphaSource {
    http: HttpClient,
    options: SourceOptions,
    /// 注入的 `lastAlpha` 节点（离线回归用）
    sample_node: Option<Value>,
    /// 注入的「现在」（离线回归用，让时段闸可复现）
    sample_now: Option<chrono::DateTime<Utc>>,
}

impl NeodexAlphaSource {
    pub fn new(options: SourceOptions, cfg: &Config) -> Result<Self, SourceError> {
        let http = HttpClient::new(NAME, &options, cfg)?;
        Ok(Self {
            http,
            options,
            sample_node: None,
            sample_now: None,
        })
    }

    /// 注入 `lastAlpha` 样本节点（离线回归用，不发网络请求）。
    pub fn with_sample_node(mut self, node: Value) -> Self {
        self.sample_node = Some(node);
        self
    }

    /// 注入「现在」（离线回归用，让「过期闸/时段闸」结果可复现）。
    pub fn with_sample_now(mut self, now: chrono::DateTime<Utc>) -> Self {
        self.sample_now = Some(now);
        self
    }

    fn headers() -> HashMap<String, String> {
        let mut h = HashMap::new();
        h.insert("content-type".into(), "application/json".into());
        h.insert("accept".into(), "application/json".into());
        h.insert("user-agent".into(), "Mozilla/5.0 (neodex-alpha-adapter)".into());
        h
    }

    async fn request_graphql(&self, language: &str) -> Result<Value, SourceError> {
        let payload = json!({
            "query": QUERY,
            "variables": { "language": language },
        });
        let url = self.options.target.trim();
        let url = if url.is_empty() { GRAPHQL_URL } else { url };
        self.http
            .post_json(&self.http.build_url(url), &payload, &Self::headers())
            .await
    }

    /// 解析 + 归一化 + 双验证。
    pub fn handle(
        &self,
        node: Option<&Value>,
        pokedex: &Pokedex,
        now: chrono::DateTime<Utc>,
    ) -> FetchResult {
        let Some(node) = node else {
            return FetchResult::empty("当前没有头目数据（lastAlpha 为空）");
        };

        let species = match node.get("species").and_then(|v| v.as_str()) {
            Some(s) if !s.is_empty() => s.to_string(),
            _ => return FetchResult::empty("GraphQL 返回的头目缺少 species"),
        };

        // ===== 双验证：过期 + 时段 =====
        let outcome = judge(
            node.get("reportedAt").and_then(|v| v.as_str()),
            node.get("despawnAt").and_then(|v| v.as_str()),
            now,
        );

        match outcome.liveness {
            Liveness::Expired => {
                return FetchResult::empty(format!(
                    "头目已过期（despawnAt 已过，已等待新头目 约 {}）",
                    fmt_duration(-outcome.remaining_sec)
                ));
            }
            Liveness::WrongSlot => {
                return FetchResult::empty(format!(
                    "头目属于其他时段（跨时段残留），不属于当前时段 {}({})，忽略",
                    outcome.current_slot, outcome.current_slot_range
                ));
            }
            Liveness::NoDespawn => {
                return FetchResult::empty("头目缺少消失时间字段，无法验证活跃状态");
            }
            Liveness::Active => {}
        }

        // ===== 以下为「当前活跃头目」 =====
        // 源站技能缺失 -> 视为无命中（不回退其它源，避免报残空头目）
        let moves_en: Vec<String> = node
            .get("moves")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|m| m.get("name").and_then(|n| n.as_str()))
                    .filter(|s| !s.is_empty())
                    .map(|s| s.to_string())
                    .collect()
            })
            .unwrap_or_default();

        if moves_en.is_empty() {
            tracing::info!("[neodex] 源站未返回技能，视为无命中: {species}");
            return FetchResult::empty(
                "源站未返回技能，视为无命中（等待带技能数据的头目）",
            );
        }

        let pid = node.get("pokedexId").and_then(|v| v.as_i64());
        let region = node.get("region").and_then(|v| v.as_str());
        let location = node.get("location").and_then(|v| v.as_str());
        let types_en: Vec<String> = node
            .get("types")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|t| t.as_str())
                    .filter(|s| !s.is_empty())
                    .map(|s| s.to_string())
                    .collect()
            })
            .unwrap_or_default();

        // 中文名：优先用图鉴号取官方中文，回退英文名反查
        let cn_name = pokedex
            .pokemon_name(pid, "zh")
            .unwrap_or_else(|| pokedex.canonical_pokemon(&species));
        if cn_name.is_empty() {
            return FetchResult::empty("无法解析头目名称");
        }

        // 技能：英文 -> 官方中文 + move_ids（决策引擎依赖 move_ids 做规则匹配）
        let cn_moves: Vec<String> = moves_en.iter().map(|m| pokedex.canonical_move(m)).collect();
        let move_ids: Vec<Option<i64>> = moves_en
            .iter()
            .map(|m| pokedex.resolve_move_id(m))
            .collect();

        // 特性：neodex 不提供，退回图鉴表 hidden_ability（与 pokemmotools_landing 兜底一致）
        let (hidden, hidden_id) = pokedex.hidden_ability(pid);
        let ability = hidden.unwrap_or_else(|| "无特性".to_string());

        // 属性：英文 -> 中文
        let types_zh: Vec<String> = types_en.iter().map(|t| type_zh(t)).collect();

        // 地点：地区译中；详细地点尽量译中（复用 pokemmotools_landing 对照表）
        let cn_region = region.map(region_zh);
        let loc_zh = translate_location(region, location);
        let loc_zh_full = join_dot(&[cn_region, loc_zh.as_deref()]);
        let loc_en = join_dot(&[region, location]);

        let mut extra: Vec<ExtraLine> = Vec::new();
        if !types_en.is_empty() {
            extra.push(ExtraLine::new(
                format!("属性: {}", types_zh.join("/")),
                format!("Types: {}", types_en.join("/")),
            ));
        }
        // 说明：原「时段: xx · 剩余 xx」额外信息行已移出 ——
        //       时段与剩余时间已并入摘要「午头(约止于14:35)」，此处不再重复展示。
        extra.push(ExtraLine::new("数据来源: GraphQL", "Source: GraphQL"));

        // 首行时段用的消失时刻：本源给出 despawnAt（精确消失时间）
        // → 用「现在 + 剩余秒数」换算，比固定 75 分钟更准。
        let expire = now_beijing() + Duration::seconds(outcome.remaining_sec.max(0));
        let period = alpha_core::time::make_period(outcome.current_slot, expire);

        let boss = BossData {
            name: cn_name,
            ability,
            moves: cn_moves,
            period,
            location: loc_zh_full,
            location_en: loc_en,
            reporter: None,
            gender: Gender::from_male_percent(pokedex.gender_of(pid)),
            egg_groups: pokedex.egg_groups_of(pid),
            egg_groups_en: Vec::new(),
            extra_lines: extra,
            pokedex_id: pid,
            ability_id: hidden_id,
            move_ids,
            source: NAME.to_string(),
            reported_at: node
                .get("reportedAt")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
        };

        tracing::info!(
            "[neodex] 命中(活跃) {} / {} / 技能={:?} / 属性={:?} / 剩余={}",
            boss.name,
            boss.location,
            boss.moves,
            types_zh,
            fmt_duration(outcome.remaining_sec)
        );

        // 摘要（推送标题）与正文首行同源
        let summary = boss.period.clone();
        let name = boss.name.clone();
        // 去重：同一在线头目（物种+地区+地点）不重复推
        let dedup = format!("{species}|{}|{}", region.unwrap_or(""), location.unwrap_or(""));

        FetchResult::hit(boss)
            .with_dedup_key(dedup)
            .with_slot(summary, slot_name_en(outcome.current_slot).to_string())
            .with_message(format!("命中(活跃): {name}"))
    }
}

#[async_trait::async_trait]
impl DataSource for NeodexAlphaSource {
    fn name(&self) -> &'static str {
        NAME
    }

    async fn fetch(&self) -> FetchResult {
        let pokedex = match alpha_core::pokedex::get_pokedex() {
            Ok(p) => p,
            Err(e) => return err_result(format!("图鉴加载失败: {e}")),
        };

        let now = self.sample_now.unwrap_or_else(Utc::now);

        // 离线样本注入（回归测试，不发网络请求）
        if let Some(node) = &self.sample_node {
            tracing::info!("[{NAME}] 使用注入的样本数据（不发网络请求）");
            return self.handle(Some(node), pokedex, now);
        }

        let data = match self.request_graphql("en").await {
            Ok(d) => d,
            Err(e) => return err_result(format!("请求失败: {e}")),
        };

        if let Some(errors) = data.get("errors") {
            if !errors.is_null() {
                return err_result(format!("GraphQL 错误: {errors}"));
            }
        }

        self.handle(
            data.get("data").and_then(|d| d.get("lastAlpha")),
            pokedex,
            now,
        )
    }
}

fn join_dot(parts: &[Option<&str>]) -> String {
    parts
        .iter()
        .flatten()
        .filter(|s| !s.is_empty())
        .copied()
        .collect::<Vec<_>>()
        .join(" · ")
}
