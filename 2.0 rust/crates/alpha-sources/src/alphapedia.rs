//! Alphapedia 数据源（`tool.lzpoke.com` 的 `latest`/`slots` 接口）。
//!
//! 这个源同时提供中英文字段（`nameEn` / `abilityEn` / `movesEn`），
//! 英文是标准名，所以优先拿英文查图鉴 —— 机翻中文的问题直接绕过。
//!
//! 它独有的字段（秘传机 `hms`、地点备注 `locationNotes`、星级 `tier`）
//! 由本适配器自己决定要不要放进推送，主流程只负责插到固定位置。
//!
//! # 判定的唯一依据是「头目还活着」
//!
//! 不能只看 `hasSpawn`/`isCurrent`：时段之间有空档（比如午头 18:45 结束、
//! 晚头 20:00 才开始），空档期 API 不给 `isCurrent`，
//! 但 18:40 刷出来的头目能活到 19:55，这时候照样该推。

use std::collections::HashMap;

use alpha_core::config::{Config, SourceOptions};
use alpha_core::models::{BossData, ExtraLine, FetchResult, Gender};
use alpha_core::pokedex::Pokedex;
use alpha_core::time::{parse_iso, slot_name_en, to_beijing};
use chrono::Timelike;
use serde_json::Value;

use crate::base::{
    err_result, parse_male_ratio, HttpClient, NameKind, NameResolver,
};
use crate::{DataSource, SourceError};

pub const NAME: &str = "alphapedia";

pub struct AlphapediaSource {
    http: HttpClient,
    target: String,
    options: SourceOptions,
    sample: Option<Value>,
}

impl AlphapediaSource {
    pub fn new(options: SourceOptions, cfg: &Config) -> Result<Self, SourceError> {
        let http = HttpClient::new(NAME, &options, cfg)?;
        Ok(Self {
            http,
            target: options.target.clone(),
            options,
            sample: None,
        })
    }

    /// 注入样本数据（离线回归用，不发网络请求）。
    pub fn with_sample(mut self, sample: Value) -> Self {
        self.sample = Some(sample);
        self
    }

    fn headers(&self) -> HashMap<String, String> {
        let mut h = HashMap::new();
        h.insert(
            "User-Agent".into(),
            "Mozilla/5.0 (Linux; Android 15; PKG110 Build/UKQ1.231108.001) \
             AppleWebKit/537.36"
                .into(),
        );
        h.insert("sec-fetch-site".into(), "same-origin".into());
        h.insert("sec-fetch-mode".into(), "cors".into());
        h.insert("sec-fetch-dest".into(), "empty".into());
        h.insert(
            "referer".into(),
            "https://tool.lzpoke.com/encounter/alpha-spawn".into(),
        );
        h.insert(
            "accept-language".into(),
            "zh-CN,zh;q=0.9,en-US;q=0.8,en;q=0.7".into(),
        );
        h.insert("Accept-Encoding".into(), "identity".into());

        // 源站新接口要 Bearer 鉴权，key 从环境变量读（不进配置文件）
        let env_name = self
            .options
            .api_key_env
            .clone()
            .unwrap_or_else(|| "ALPHAPEDIA_API_KEY".to_string());
        if let Ok(key) = std::env::var(&env_name) {
            let key = key.trim();
            if !key.is_empty() {
                h.insert("Authorization".into(), format!("Bearer {key}"));
            }
        }
        h
    }

    /// 按报点时间找到它属于哪个时段（时段定义直接从 API 读，不硬编码）。
    fn slot_for_time<'a>(slots: &'a [Value], iso_str: Option<&str>) -> Option<&'a Value> {
        let t = parse_iso(iso_str?)?;
        let mut best: Option<(chrono::DateTime<chrono::Utc>, &Value)> = None;
        for s in slots {
            let a = s.get("startIso").and_then(|v| v.as_str()).and_then(parse_iso);
            let b = s.get("endIso").and_then(|v| v.as_str()).and_then(parse_iso);
            let (Some(a), Some(b)) = (a, b) else { continue };
            if a <= t && t <= b {
                return Some(s);
            }
            // 窗口末尾刷出来的头目，报点时间会落在空档里，
            // 这种归到「结束时间早于报点时间」的最近一个时段
            if b <= t && best.map(|(bb, _)| b > bb).unwrap_or(true) {
                best = Some((b, s));
            }
        }
        best.map(|(_, s)| s)
    }

    /// 解析 `latest` 节点为 [`BossData`]。
    pub fn parse(&self, latest: &Value, pokedex: &Pokedex) -> BossData {
        let resolver = NameResolver::new(pokedex);
        let catalog = latest
            .get("catalog")
            .and_then(|v| v.as_array())
            .and_then(|a| a.first())
            .cloned()
            .unwrap_or_else(|| Value::Object(Default::default()));

        // 名称：英文名优先
        let (name, pid) = resolver.resolve_entry(
            latest.get("name").and_then(|v| v.as_str()),
            latest.get("nameEn").and_then(|v| v.as_str()),
            NameKind::Pokemon,
        );
        let (ability, aid) = resolver.resolve_entry(
            catalog.get("ability").and_then(|v| v.as_str()),
            catalog.get("abilityEn").and_then(|v| v.as_str()),
            NameKind::Ability,
        );

        // 技能：逐个用英文名解析，中英文数组按位置对齐
        let moves_zh: Vec<String> = str_array(&catalog, "moves");
        let moves_en: Vec<String> = str_array(&catalog, "movesEn");
        let mut moves = Vec::new();
        let mut move_ids = Vec::new();
        for (i, mz) in moves_zh.iter().enumerate() {
            let me = moves_en.get(i).map(|s| s.as_str());
            let (cn, mid) = resolver.resolve_entry(Some(mz), me, NameKind::Move);
            moves.push(cn);
            move_ids.push(mid);
        }

        // 性别：优先用图鉴（覆盖全、格式稳定），API 的 maleRatio 作兜底
        let mut male = pokedex.gender_of(pid);
        if male.is_none() {
            male = parse_male_ratio(catalog.get("maleRatio"));
        }

        // 时段：报点时间 ~ 失效时间（北京时间）
        let period = self.period(
            latest.get("reportedAt").and_then(|v| v.as_str()),
            latest.get("activeUntil").and_then(|v| v.as_str()),
        );

        let region = str_of(latest, "region");
        let place = str_of(latest, "location");
        let region_en = str_of(latest, "regionEn");
        let place_en = str_of(latest, "locationEn");

        let mut boss = BossData {
            name,
            ability,
            moves,
            period,
            location: format!("{region} {place}").trim().to_string(),
            location_en: format!("{region_en} {place_en}").trim().to_string(),
            reporter: latest
                .get("reporter")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
                .map(|s| s.to_string()),
            gender: Gender::from_male_percent(male),
            egg_groups: str_array(&catalog, "eggGroups"),
            egg_groups_en: str_array(&catalog, "eggGroupsEn"),
            extra_lines: Vec::new(),
            pokedex_id: pid,
            ability_id: aid,
            move_ids,
            source: NAME.to_string(),
            reported_at: str_of(latest, "reportedAt"),
        };
        boss.extra_lines = self.build_extra_lines(&catalog, latest);
        boss
    }

    fn period(&self, reported_at: Option<&str>, active_until: Option<&str>) -> String {
        let s = to_hhmm(reported_at);
        let e = to_hhmm(active_until);
        if !s.is_empty() && !e.is_empty() {
            format!("{s}~{e}")
        } else {
            String::new()
        }
    }

    /// 附加信息：由本源自己决定给什么，主流程只负责插到固定位置
    /// （「头目信息」和「打法推荐」之间）。
    pub fn build_extra_lines(&self, catalog: &Value, _latest: &Value) -> Vec<ExtraLine> {
        let opts = &self.options.extra_lines;
        let mut out = Vec::new();

        if opts.hms {
            let hms = str_array(catalog, "hms");
            if !hms.is_empty() {
                let resolver = NameResolver::new(
                    alpha_core::pokedex::get_pokedex().expect("图鉴已加载"),
                );
                if let Some(line) = resolver.make_extra("需要秘传机", "HM required", &hms) {
                    out.push(line);
                }
            }
        }

        if opts.location_notes {
            let note = str_of(catalog, "locationNotes");
            if !note.is_empty() {
                out.push(ExtraLine::new(
                    format!("地点备注: {note}"),
                    format!("Location note: {note}"),
                ));
            }
        }

        if opts.notes {
            let notes = str_array(catalog, "notes");
            if !notes.is_empty() {
                out.push(ExtraLine::new(
                    format!("备注: {}", notes.join(", ")),
                    format!("Notes: {}", notes.join(", ")),
                ));
            }
        }

        if opts.tier {
            if let Some(tier) = catalog.get("tier").filter(|v| !v.is_null()) {
                let t = match tier {
                    Value::String(s) => s.clone(),
                    other => other.to_string(),
                };
                out.push(ExtraLine::new(format!("星级: {t}"), format!("Tier: {t}")));
            }
        }

        out
    }

    /// 时段中文名 -> 英文名（`slot_name_en` 对未知名字返回 "Alpha"）。
    fn slot_en(&self, cn: &str) -> String {
        if cn.is_empty() {
            return "Alpha".to_string();
        }
        slot_name_en(cn).to_string()
    }
}

fn str_of(v: &Value, key: &str) -> String {
    v.get(key)
        .and_then(|x| x.as_str())
        .unwrap_or("")
        .to_string()
}

fn str_array(v: &Value, key: &str) -> Vec<String> {
    v.get(key)
        .and_then(|x| x.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|i| i.as_str())
                .map(|s| s.to_string())
                .collect()
        })
        .unwrap_or_default()
}

fn to_hhmm(iso: Option<&str>) -> String {
    let Some(iso) = iso else {
        return String::new();
    };
    match parse_iso(iso) {
        Some(dt) => to_beijing(dt).format("%H:%M").to_string(),
        None => String::new(),
    }
}

/// 兜底推断：本源拿不到剩余时间时（其实有 activeUntil），
/// 用它算「当前时段」。
fn current_slot_name() -> &'static str {
    alpha_core::time::slot_name_of_hour(alpha_core::time::now_beijing().hour()).0
}

#[async_trait::async_trait]
impl DataSource for AlphapediaSource {
    fn name(&self) -> &'static str {
        NAME
    }

    async fn fetch(&self) -> FetchResult {
        let pokedex = match alpha_core::pokedex::get_pokedex() {
            Ok(p) => p,
            Err(e) => return err_result(format!("图鉴加载失败: {e}")),
        };

        if self.target.is_empty() {
            return err_result("未配置 target");
        }

        let data = match &self.sample {
            Some(s) => s.clone(),
            None => {
                let url = self.http.build_url(&self.target);
                match self.http.get_json(&url, &self.headers()).await {
                    Ok(d) => d,
                    Err(e) => return err_result(format!("请求失败: {e}")),
                }
            }
        };

        let latest = data
            .get("latest")
            .cloned()
            .unwrap_or_else(|| Value::Object(Default::default()));
        let slots: Vec<Value> = data
            .get("slots")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();

        // 判定的唯一依据是「头目还活着」。
        // 不能只看 hasSpawn/isCurrent：时段之间有空档（比如午头 18:45 结束、
        // 晚头 20:00 才开始），空档期 API 不给 isCurrent，
        // 但 18:40 刷出来的头目能活到 19:55，这时候照样该推。
        if !latest
            .get("isActive")
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
        {
            return FetchResult::empty("最新头目已过期（头目只存在 75 分钟）");
        }

        // 时段：优先当前 slot，空档期则按报点时间反查所属时段
        let reported = latest.get("reportedAt").and_then(|v| v.as_str());
        let slot = slots
            .iter()
            .find(|s| s.get("isCurrent").and_then(|v| v.as_bool()).unwrap_or(false))
            .or_else(|| Self::slot_for_time(&slots, reported));

        let slot_key = reported
            .map(|s| s.to_string())
            .or_else(|| {
                slot.and_then(|s| s.get("startIso").and_then(|v| v.as_str()))
                    .map(|s| s.to_string())
            })
            .unwrap_or_default();
        let slot_name = slot
            .and_then(|s| s.get("name").and_then(|v| v.as_str()))
            .unwrap_or("")
            .to_string();

        let boss = self.parse(&latest, pokedex);
        if boss.name.is_empty() {
            return FetchResult::empty("无法解析头目名称");
        }

        // 摘要（推送标题）与正文首行同源；本源 API 直接给时段名
        let summary = if slot_name.is_empty() {
            boss.period.clone()
        } else {
            slot_name.clone()
        };
        let _ = current_slot_name();
        let name = boss.name.clone();

        FetchResult::hit(boss)
            // 去重标识用报点时间：同一条报点只推一次，
            // 换时段、换头目自然就是新的 key
            .with_dedup_key(slot_key)
            .with_slot(summary, self.slot_en(&slot_name))
            .with_message(format!("{slot_name} 命中：{name}"))
    }
}
