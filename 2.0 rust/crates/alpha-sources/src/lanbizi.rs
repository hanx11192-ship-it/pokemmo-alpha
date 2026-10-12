//! 致敬源（`pokemmo.lanbizi.com`）。
//!
//! # 这是什么
//!
//! 接口已 410 下线，代码保留作为「源 + 适配器」的**模板**。
//!
//! # 命名说明
//!
//! 面板上显示的源名是「致敬源」，但适配器名仍是 `lanbizi`，
//! `sources.yaml` 里用 `adapter: lanbizi` 指向本模块。
//! 改名只是为了纪念，不动代码结构。
//!
//! # 致敬
//!
//! 国内首个公开头目数据源。在还没有任何公开接口、也没有文档的年代，
//! 是它让「自动报点」这件事第一次跑通。后来的源都站在它肩上。
//!
//! # 本源的典型特征（写新适配器时常遇到的坑）
//!
//! 1. 只给中文，没有英文字段 → 走「中文名 + 别名表」解析
//! 2. 只给 `monster_id`，不给名字 → 从图鉴反查中文名
//! 3. 不给特性 → PokeMMO 头目一律隐藏特性，从图鉴 `hidden_ability` 补齐
//! 4. `monster_id` 需要除以 100 才是全国图鉴号
//!    （每个源算法不同，除数写在配置里，别硬编码）
//!
//! # 抄这个模板的正确姿势
//!
//! 1. 复制本模块，改适配器名
//! 2. 实现 [`DataSource::fetch`]，其余按需覆写
//! 3. 在 `config/sources.yaml` 里照抄一段配置，填上 `adapter` 名
//! 4. 在 [`crate::registry`] 里登记一行
//! 5. 主流程不用改 —— 这就是插件式设计的目的

use std::collections::HashMap;

use alpha_core::config::{Config, SourceOptions};
use alpha_core::models::{BossData, FetchResult, Gender};
use alpha_core::pokedex::Pokedex;
use serde_json::{json, Value};

use crate::base::{err_result, extract_reporter, HttpClient, NameKind, NameResolver};
use crate::{DataSource, SourceError};

pub const NAME: &str = "lanbizi";

pub struct LanbiziSource {
    http: HttpClient,
    target: String,
    options: SourceOptions,
    /// 注入的响应体（离线回归用）
    sample: Option<Value>,
}

impl LanbiziSource {
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

    fn headers() -> HashMap<String, String> {
        let mut h = HashMap::new();
        h.insert(
            "User-Agent".into(),
            "Mozilla/5.0 (Linux; Android 15; PKG110 Build/UKQ1.231108.001) \
             AppleWebKit/537.36"
                .into(),
        );
        h.insert("Content-Type".into(), "application/json".into());
        h.insert("Origin".into(), "https://pokemmo.lanbizi.com".into());
        h.insert("Sec-Fetch-Site".into(), "same-origin".into());
        h.insert("Sec-Fetch-Mode".into(), "cors".into());
        h.insert("Sec-Fetch-Dest".into(), "empty".into());
        h.insert("Referer".into(), "https://pokemmo.lanbizi.com/".into());
        h.insert("Accept-Language".into(), "zh-CN,zh;q=0.9".into());
        h
    }

    /// 把源给的原始字段翻成统一的 [`BossData`]。
    ///
    /// 这一层是适配器的核心：源再多，主流程只认 `BossData`。
    pub fn parse(&self, info: &Value, pokedex: &Pokedex) -> BossData {
        let resolver = NameResolver::new(pokedex);

        // 每个源算图鉴号的方式不一样，除数写在配置里
        let divisor = self.options.monster_id_divisor.unwrap_or(1).max(1);
        let pid = info
            .get("monster_id")
            .and_then(|v| v.as_i64())
            .map(|raw| raw / divisor);

        // 只有图鉴号，没有名字 → 从图鉴补
        let name = pokedex
            .pokemon_name(pid, "zh")
            .unwrap_or_else(|| match pid {
                Some(p) => format!("未知_{p}"),
                None => "未知头目".to_string(),
            });
        // 头目一律隐藏特性
        let (hidden, hidden_id) = pokedex.hidden_ability(pid);
        let ability = hidden.unwrap_or_else(|| "无特性".to_string());
        let male = pokedex.gender_of(pid);

        let mut moves = Vec::new();
        let mut move_ids = Vec::new();
        for i in 1..=4 {
            let key = format!("move{i}");
            if let Some(mv) = info.get(&key).and_then(|v| v.as_str()).filter(|s| !s.is_empty()) {
                let (cn, mid) = resolver.resolve_entry(Some(mv), None, NameKind::Move);
                moves.push(cn);
                move_ids.push(mid);
            }
        }

        let start = info
            .get("start_time_str")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let end = info.get("end_time_str").and_then(|v| v.as_str()).unwrap_or("");
        let period = if !start.is_empty() && !end.is_empty() {
            format!("{start}~{end}")
        } else {
            String::new()
        };

        let mut location = info
            .get("location_full_name")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let desc = info
            .get("description")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim();
        if !desc.is_empty() {
            location = format!("{location} - {desc}");
        }

        let reporter = extract_reporter(
            info.get("text").and_then(|v| v.as_str()).unwrap_or(""),
        );

        BossData {
            name,
            ability,
            moves,
            period,
            location,
            location_en: String::new(),
            reporter: if reporter.is_empty() {
                None
            } else {
                Some(reporter)
            },
            gender: Gender::from_male_percent(male),
            egg_groups: pokedex.egg_groups_of(pid),
            egg_groups_en: Vec::new(),
            extra_lines: Vec::new(),
            pokedex_id: pid,
            ability_id: hidden_id,
            move_ids,
            source: NAME.to_string(),
            reported_at: String::new(),
        }
    }
}

#[async_trait::async_trait]
impl DataSource for LanbiziSource {
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
                // 走转发时用中转要求的方法；本源历史上是带空 JSON body 的 POST
                let url = self.http.build_url(&self.target);
                match self
                    .http
                    .post_json(&url, &json!({}), &Self::headers())
                    .await
                {
                    Ok(d) => d,
                    Err(e) => return err_result(format!("请求失败: {e}")),
                }
            }
        };

        if data.get("code").and_then(|v| v.as_i64()).unwrap_or(0) != 0 {
            return FetchResult::empty(format!(
                "API 返回错误: {}",
                data.get("msg").and_then(|v| v.as_str()).unwrap_or("")
            ));
        }

        let list: Vec<Value> = data
            .get("data")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        if list.is_empty() {
            return FetchResult::empty("当前时段没有头目");
        }

        let boss = self.parse(&list[0], pokedex);
        // 本源没有时段概念，用起止时间当去重标识；
        // 如果哪天连 period 都没了，基类会退化成按头目内容算指纹
        let key = boss.period.clone();
        let name = boss.name.clone();
        FetchResult::hit(boss)
            .with_dedup_key(key)
            .with_message(format!("命中：{name}"))
    }
}
