//! LZPoke 报点源（`tool.lzpoke.com`）。
//!
//! 接口：`GET https://tool.lzpoke.com/api/reports?type=alpha`
//!
//! # 本源特征（写适配器时踩过的点）
//!
//! 1. 只给英文精灵名 `nameEn`，不给中文名 → 优先用英文名查图鉴（英文名是标准名，绕开机翻）
//! 2. 只给 `monsterId`（全国图鉴号，**不用除 100**）→ 直接从图鉴取中文名/特性/蛋组
//! 3. 不给特性 → Pokemmo 头目一律隐藏特性，从图鉴 `hidden_ability` 补齐
//! 4. `moves` 是中文技能名 → 走中文名 + 图鉴解析（查不到就原样保留，不丢信息）
//! 5. 有「报点投票」机制（`voteScore` / `voteCount` / `threshold` / `confirmed`）
//!    → 主流程只取「最佳一条」；[`fetch_reports`] 返回全部，由调用方决定怎么用
//! 6. 不给「剩余消失时间」→ 正文首行时段用「命中时刻 + 75 分钟」推算

use std::collections::HashMap;

use alpha_core::config::{Config, SourceOptions};
use alpha_core::models::{BossData, ExtraLine, FetchResult, Gender};
use alpha_core::pokedex::Pokedex;
use alpha_core::time::{make_period, now_beijing, slot_name_of_hour, slot_name_en, ALPHA_LIFETIME_MIN};
use alpha_core::time::parse_iso;
use chrono::{Duration, Timelike};
use serde_json::Value;

use crate::base::{err_result, NameResolver};
use crate::base::HttpClient;
use crate::{DataSource, SourceError};

pub const NAME: &str = "lzpoke_reports";

/// 源站校验 UA / Referer（照抄浏览器与用户给的 curl）。
fn headers() -> HashMap<String, String> {
    let mut h = HashMap::new();
    h.insert(
        "User-Agent".into(),
        "Mozilla/5.0 (Linux; Android 16; TB710FU Build/BP2A.250605.031.A3) \
         AppleWebKit/537.36 (KHTML, like Gecko) Version/4.0 \
         Chrome/131.0.6778.200 Safari/537.36"
            .into(),
    );
    h.insert(
        "Referer".into(),
        "https://tool.lzpoke.com/encounter/alpha-spawn".into(),
    );
    h.insert("Accept-Language".into(), "zh-CN,zh;q=0.9".into());
    h
}

pub struct LzpokeReportsSource {
    http: HttpClient,
    target: String,
    options: SourceOptions,
    sample: Option<Value>,
}

impl LzpokeReportsSource {
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

    async fn load(&self) -> Result<Value, SourceError> {
        if let Some(s) = &self.sample {
            tracing::info!("[{NAME}] 使用注入的样本数据（不发网络请求）");
            return Ok(s.clone());
        }
        if self.target.is_empty() {
            return Err(SourceError::Config("未配置 target".into()));
        }
        let url = self.http.build_url(&self.target);
        self.http.get_json(&url, &headers()).await
    }

    /// 挑「最佳一条」：已确认优先 → 票分最高 → 报点最新。
    fn best(reports: &[Value]) -> Option<&Value> {
        reports.iter().max_by(|a, b| {
            let ka = (
                a.get("confirmed").and_then(|v| v.as_bool()).unwrap_or(false) as u8,
                a.get("voteScore").and_then(|v| v.as_f64()).unwrap_or(0.0),
                a.get("createdAt").and_then(|v| v.as_str()).unwrap_or(""),
            );
            let kb = (
                b.get("confirmed").and_then(|v| v.as_bool()).unwrap_or(false) as u8,
                b.get("voteScore").and_then(|v| v.as_f64()).unwrap_or(0.0),
                b.get("createdAt").and_then(|v| v.as_str()).unwrap_or(""),
            );
            ka.partial_cmp(&kb).unwrap_or(std::cmp::Ordering::Equal)
        })
    }

    /// 把一条报点翻成统一的 [`BossData`]。
    ///
    /// 只给 `monsterId` + `nameEn` + 中文 `moves`，特性/蛋组从图鉴补。
    pub fn parse(&self, report: &Value, pokedex: &Pokedex) -> BossData {
        let resolver = NameResolver::new(pokedex);

        // monsterId 即全国图鉴号（不除 100）；也允许配置 divisor 兜底
        let divisor = self.options.monster_id_divisor.unwrap_or(1).max(1);
        let raw = report.get("monsterId").and_then(|v| v.as_i64());
        let mut pid = raw.map(|v| v / divisor);

        // 名称：英文名优先（标准名，绕开机翻），查不到回退中文/原名
        let (name, pid2) = resolver.resolve_entry(
            report.get("name").and_then(|v| v.as_str()),
            report.get("nameEn").and_then(|v| v.as_str()),
            crate::base::NameKind::Pokemon,
        );
        if pid.is_none() {
            pid = pid2;
        }

        // 特性：源不提供 → 头目一律隐藏特性，从图鉴补
        let (ability, ability_id) = pokedex.hidden_ability(pid);
        let ability = ability.unwrap_or_else(|| "无特性".to_string());

        // 技能：中文名逐条解析（查不到的保留原样，不丢信息）
        let mut moves = Vec::new();
        let mut move_ids = Vec::new();
        if let Some(arr) = report.get("moves").and_then(|v| v.as_array()) {
            for m in arr {
                let Some(mz) = m.as_str() else { continue };
                let (cn, mid) = resolver.resolve_entry(
                    Some(mz),
                    None,
                    crate::base::NameKind::Move,
                );
                moves.push(cn);
                move_ids.push(mid);
            }
        }

        // 性别：优先图鉴（覆盖全、格式稳），源没给性别字段
        let male = pokedex.gender_of(pid);

        // 首行时段：午头(约止于18:14)。
        // 本源不给剩余时间 → 用「命中时刻 + 75 分钟」推算消失时刻。
        let slot_name = report
            .get("windowStart")
            .and_then(|v| v.as_str())
            .and_then(parse_iso)
            .map(|dt| slot_name_of_hour(alpha_core::time::to_beijing(dt).hour()).0.to_string())
            .unwrap_or_else(|| slot_name_of_hour(now_beijing().hour()).0.to_string());
        let expire_dt = now_beijing() + Duration::minutes(ALPHA_LIFETIME_MIN);
        let period = make_period(&slot_name, expire_dt);

        // 源已把「地区 · 地点」拼进 location/locationEn，直接用，避免重复前缀
        let location = report
            .get("location")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let location_en = report
            .get("locationEn")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        let mut boss = BossData {
            name,
            ability,
            moves,
            period,
            location,
            location_en,
            reporter: report
                .get("reporterName")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string()),
            gender: Gender::from_male_percent(male),
            egg_groups: pokedex.egg_groups_of(pid),
            egg_groups_en: Vec::new(),
            extra_lines: Vec::new(),
            pokedex_id: pid,
            ability_id,
            move_ids,
            source: NAME.to_string(),
            reported_at: report
                .get("createdAt")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
        };
        boss.extra_lines = self.build_extra_lines(report);
        boss
    }

    /// 附加信息（投票进度）。
    ///
    /// 等级(`level`) 已按需求从可选附加信息中移出，不再渲染；
    /// 本源也没有「剩余消失时间」字段，故没有该附加行。
    pub fn build_extra_lines(&self, report: &Value) -> Vec<ExtraLine> {
        let opts = &self.options.extra_lines;
        let mut out = Vec::new();

        if opts.vote {
            let get_i = |k: &str| report.get(k).and_then(|v| v.as_i64()).unwrap_or(0);
            let confirmed = report
                .get("confirmed")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            if confirmed {
                out.push(ExtraLine::new("状态: 已确认", "Status: Confirmed"));
            } else {
                let (vs, vc, th) = (get_i("voteScore"), get_i("voteCount"), get_i("threshold"));
                out.push(ExtraLine::new(
                    format!("确认进度: {vs}/{th}（{vc} 票）"),
                    format!("Confirmation: {vs}/{th} ({vc} votes)"),
                ));
            }
        }
        out
    }

    /// 返回**全部**报点（归一化 + 原始），供面板「头目 API」调用。
    pub async fn fetch_reports(
        &self,
        pokedex: &Pokedex,
    ) -> (bool, Vec<Value>, String) {
        let data = match self.load().await {
            Ok(d) => d,
            Err(e) => return (false, Vec::new(), format!("请求失败: {e}")),
        };

        let reports = data
            .get("reports")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();

        let out: Vec<Value> = reports
            .iter()
            .map(|r| {
                let boss = self.parse(r, pokedex);
                serde_json::json!({
                    "raw": r,
                    "boss": boss,
                    "parsed": true,
                    "confirmed": r.get("confirmed").and_then(|v| v.as_bool()).unwrap_or(false),
                })
            })
            .collect();

        let n = out.len();
        (true, out, format!("共 {n} 条报点"))
    }
}

#[async_trait::async_trait]
impl DataSource for LzpokeReportsSource {
    fn name(&self) -> &'static str {
        NAME
    }

    async fn fetch(&self) -> FetchResult {
        let pokedex = match alpha_core::pokedex::get_pokedex() {
            Ok(p) => p,
            Err(e) => return err_result(format!("图鉴加载失败: {e}")),
        };

        let data = match self.load().await {
            Ok(d) => d,
            Err(e) => return err_result(format!("请求失败: {e}")),
        };

        let reports = data
            .get("reports")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        if reports.is_empty() {
            return FetchResult::empty("当前没有头目报点");
        }

        let best = match Self::best(&reports) {
            Some(b) => b.clone(),
            None => return FetchResult::empty("当前没有头目报点"),
        };

        let boss = self.parse(&best, pokedex);
        // 摘要（推送标题）与正文首行同源：都用 boss.period
        let summary = boss.period.clone();
        let slot = best
            .get("windowStart")
            .and_then(|v| v.as_str())
            .and_then(parse_iso)
            .map(|dt| slot_name_of_hour(alpha_core::time::to_beijing(dt).hour()).0.to_string())
            .unwrap_or_else(|| slot_name_of_hour(now_beijing().hour()).0.to_string());

        // 去重标识用 burstKey（按 窗口+图鉴号+地点 归组，稳定标识同一次刷怪）
        let key = best
            .get("burstKey")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .or_else(|| best.get("id").and_then(|v| v.as_str()))
            .unwrap_or("")
            .to_string();

        let name = boss.name.clone();
        FetchResult::hit(boss)
            .with_dedup_key(key)
            .with_slot(summary, slot_name_en(&slot).to_string())
            .with_message(format!("命中：{name}"))
    }
}
