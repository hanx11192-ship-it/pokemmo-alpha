//! PokemmoTools Landing 源（`alpha.pokemmotools.org`）。
//!
//! # 数据来源（两步，均公开，无需登录 Cookie）
//!
//! 1. `GET /alpha-list` —— 当前活跃头目名 / 地区 / 地点 / 剩余秒数
//!
//!    有活跃头目时会渲染一块：
//!    ```html
//!    <span data-i18n="Alpha currently active">Alpha currently active</span>:
//!    <a href="/alpha-list?pokemon=Luxray&region=Sinnoh&location=Route+205&...">
//!    ... data-i18n="Remains active for approximately ..." data-timedelta="171">
//!    ```
//!    即：头目名 + 地区/地点（链接 query 里）+ 剩余秒数（`data-timedelta`）。
//!    用来「探活 + 定位 + 拿剩余时间」，补 lzpoke 的盲区
//!    （lzpoke 只在有人 call 时才有 reports；本源主动感知当前谁在）。
//!
//! 2. `GET /pokedex/{图鉴号}` —— 该头目在各出现点的 4 携带技能 + 特性
//!
//!    页面 `#pokedex-alpha` 面板下，每个 `<article class="pokedex-spawn-entry">`
//!    对应一个出现地点，内含若干 `<div class="pokedex-spawn-moves">`：
//!    - `label="HMs Required"` -> 秘传技
//!    - `label="Moves"`        -> 该地点的 4 个携带技能
//!
//!    **不同地点技能不同**，所以必须用第 1 步拿到的地区/地点去匹配正确的 entry。
//!    特性藏在 `pokedex-ability-link` 里，带 `pokedex-hidden-ability-icon` 的即隐藏特性。
//!
//! # 归一化（关键）
//!
//! 图鉴页抓回的特性/技能都是**英文**，必须经 `canonical_*` / `resolve_*_id`
//! 归一化回官方中文并拿到数字 id，否则：
//! - 界面中文模式下仍显示英文；
//! - `boss.move_ids` 为空 -> 决策引擎按技能做的判定（龙之舞→挑衅等）全部落空，
//!   打法退化成固定甜蜜球模板。

use std::collections::HashMap;

use alpha_core::config::{Config, SourceOptions};
use alpha_core::models::{BossData, ExtraLine, FetchResult, Gender};
use alpha_core::pokedex::Pokedex;
use alpha_core::time::{now_beijing, slot_name_of_hour, slot_name_en, ALPHA_LIFETIME_MIN};
use chrono::{Duration, Timelike};
use once_cell::sync::Lazy;
use regex::Regex;


use crate::base::{err_result, HttpClient, NameKind, NameResolver};
use crate::{DataSource, SourceError};

pub const NAME: &str = "pokemmotools_landing";

const BASE_SITE: &str = "https://alpha.pokemmotools.org";

/// 额外信息里固定展示的数据来源（按需求统一标注为 Alphapedia）。
const SOURCE_LABEL: &str = "Alphapedia";

/// 地区英文名 -> 中文名。
pub fn region_zh(region: &str) -> &str {
    match region {
        "Kanto" => "关都",
        "Johto" => "城都",
        "Hoenn" => "丰缘",
        "Sinnoh" => "神奥",
        "Unova" => "合众",
        "Kalos" => "卡洛斯",
        "Alola" => "阿罗拉",
        "Galar" => "伽勒尔",
        "Hisui" => "洗翠",
        "Paldea" => "帕底亚",
        other => other,
    }
}

/// 秘传技 / 秘传招式：英文 -> 官方中文（网站给的是英文，如 Surf / Waterfall）。
/// 查不到的秘传技原样保留（不丢信息）。
pub fn hm_zh(hm: &str) -> String {
    match hm.trim().to_lowercase().as_str() {
        "surf" => "冲浪",
        "waterfall" => "攀瀑",
        "strength" => "怪力",
        "rock smash" => "碎岩",
        "cut" => "居合斩",
        "fly" => "飞翔",
        "flash" => "闪光",
        "dive" => "潜水",
        "defog" => "清除浓雾",
        "rock climb" => "攀岩",
        "headbutt" => "头锤",
        "whirlpool" => "潮旋",
        "teleport" => "瞬间移动",
        "dig" => "挖洞",
        "sweet scent" => "甜甜香气",
        "soft-boiled" => "生蛋",
        other => return other.to_string(),
    }
    .to_string()
}

/// 把英文详细地点译中。
///
/// - `Route N` -> `道路 N`（通用规则，覆盖绝大多数刷新点）
/// - 具名点位查 [`crate::location_map`]（先按地区，再全局兜底）
/// - 都查不到的保留英文原样（不丢信息）
pub fn translate_location(region: Option<&str>, location: Option<&str>) -> Option<String> {
    let raw = location?.trim();
    if raw.is_empty() {
        return None;
    }
    let low = raw.to_lowercase();

    if let Some(caps) = ROUTE_RE.captures(&low) {
        return Some(format!("道路 {}", &caps[1]));
    }

    if low.contains(' ') {
        // 有些源把空格重写成双空格（"mt  moon"），归一后再查
        let squeezed = low.split_whitespace().collect::<Vec<_>>().join(" ");
        if let Some(v) = lookup_location(region, &squeezed) {
            return Some(v.to_string());
        }
    }
    if let Some(v) = lookup_location(region, &low) {
        return Some(v.to_string());
    }
    Some(raw.to_string())
}

fn lookup_location(region: Option<&str>, key: &str) -> Option<&'static str> {
    let map: &crate::location_map::LocationMap = &crate::location_map::LOCATION_MAP;
    if let Some(r) = region {
        let r_lower = r.trim().to_lowercase();
        if let Some(zone) = map.get(r_lower.as_str()) {
            if let Some(v) = zone.get(key) {
                return Some(v);
            }
        }
    }
    // 地点名全局唯一，避免地区归类偏差
    // （如七之岛在 Pokemmo 归 Kanto，若某源把地区写成别的，全局兜底仍能译中）
    for zone in map.values() {
        if let Some(v) = zone.get(key) {
            return Some(v);
        }
    }
    None
}

static ROUTE_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"(?i)^route\s+(\S+)$").unwrap());
static ACTIVE_LINK_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r#"Alpha currently active</span>\s*:\s*<a[^>]*href="([^"]+)""#).unwrap()
});
static REMAINING_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r#"Remains active for approximately[^>]*data-timedelta="(\d+)""#).unwrap()
});
static ABILITY_LINK_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r#"pokedex-ability-link"[^>]*>(.*?)</a>"#).unwrap()
});
static DATA_VALUE_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r#"data-item_value="([^"]+)""#).unwrap());
static ALPHA_PANEL_HEAD: &str = r#"id="pokedex-alpha""#;
static NEXT_PANEL_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r#"<div id="pokedex-[a-z]"#).unwrap());
static SPAWN_ENTRY_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r#"<article class="pokedex-spawn-entry">(.*?)</article>"#).unwrap());
static ENTRY_REGION_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r#"data-i18n="__\(item_value::text::region\)__"\s*data-item_value="([^"]+)""#).unwrap()
});
static ENTRY_LOC_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r#"data-i18n="__\(item_value::text::locationPokeapi\)__"\s*data-item_value="([^"]+)""#)
        .unwrap()
});
static SPAWN_MOVES_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r#"pokedex-spawn-moves">(.*?)</div>"#).unwrap());
static LABEL_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r#"data-i18n="([^"]+)""#).unwrap());

/// `/alpha-list` 里「当前活跃头目」的解析结果。
#[derive(Debug, Clone)]
pub struct ActiveBoss {
    pub boss: BossData,
    /// 剩余秒数（源没给就是 None）
    pub remaining: Option<i64>,
    pub region: Option<String>,
    pub location: Option<String>,
    pub slot_name: &'static str,
    pub slot_range: &'static str,
}

/// `/pokedex/{id}` 页面抽出的技能 / 特性 / 秘传技。
#[derive(Debug, Clone, Default)]
pub struct PokedexPage {
    pub moves: Vec<String>,
    pub abilities: Vec<String>,
    pub hidden_ability: Option<String>,
    pub hms: Vec<String>,
}

pub struct PokemmotoolsLandingSource {
    http: HttpClient,
    target: String,
    /// 注入的 HTML（离线回归用）
    sample_html: Option<String>,
}

impl PokemmotoolsLandingSource {
    pub fn new(options: SourceOptions, cfg: &Config) -> Result<Self, SourceError> {
        let http = HttpClient::new(NAME, &options, cfg)?;
        Ok(Self {
            http,
            target: options.target.clone(),
            sample_html: None,
        })
    }

    /// 注入样本 HTML（离线回归用，不发网络请求）。
    pub fn with_sample_html(mut self, html: impl Into<String>) -> Self {
        self.sample_html = Some(html.into());
        self
    }

    fn headers() -> HashMap<String, String> {
        let mut h = HashMap::new();
        h.insert(
            "Accept".into(),
            "text/html,application/xhtml+xml".into(),
        );
        h.insert(
            "User-Agent".into(),
            "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 \
             (KHTML, like Gecko) Chrome/124.0 Safari/537.36"
                .into(),
        );
        h.insert("Referer".into(), "https://alpha.pokemmotools.org/".into());
        h
    }

    // ---------------- 解析：alpha-list ----------------

    /// 从公开 alpha-list 页面抽取「当前活跃头目」。无则返回 `None`。
    pub fn parse(&self, html: &str, pokedex: &Pokedex) -> Option<ActiveBoss> {
        if !html.contains("Alpha currently active") {
            return None;
        }

        let caps = ACTIVE_LINK_RE.captures(html)?;
        let href = html_unescape(&caps[1]);
        let query = href.split_once('?').map(|(_, q)| q).unwrap_or("");
        let params = parse_qs(query);

        let name_en = params.get("pokemon")?.first()?.clone();
        let region = params.get("region").and_then(|v| v.first()).cloned();
        let location = params.get("location").and_then(|v| v.first()).cloned();

        // 剩余秒数
        let remaining = REMAINING_RE
            .captures(html)
            .and_then(|c| c.get(1))
            .and_then(|m| m.as_str().parse::<i64>().ok());

        // 图鉴号 / 中文名
        let resolver = NameResolver::new(pokedex);
        let (cn_name, pid) = resolver.resolve_entry(None, Some(&name_en), NameKind::Pokemon);

        let slot_name = slot_name_of_hour(now_beijing().hour()).0;
        let slot_range = slot_name_of_hour(now_beijing().hour()).1;

        let cn_region = region.as_deref().map(region_zh);
        let loc_zh = translate_location(region.as_deref(), location.as_deref());
        let loc_zh_full = join_dot(&[cn_region, loc_zh.as_deref()]);
        let loc_en = join_dot(&[region.as_deref(), location.as_deref()]);

        // 正文首行时段：午头(约止于14:35)。
        // alpha-list 给了剩余秒数就用它算；拿不到则按「命中时刻 + 75 分钟」兜底。
        let expire = match remaining {
            Some(s) => now_beijing() + Duration::seconds(s.max(0)),
            None => now_beijing() + Duration::minutes(ALPHA_LIFETIME_MIN),
        };
        let period = alpha_core::time::make_period(slot_name, expire);

        let (hidden_ability, hidden_ability_id) = pokedex.hidden_ability(pid);

        let boss = BossData {
            name: if cn_name.is_empty() { name_en } else { cn_name },
            // 初始特性先用图鉴表的中文隐藏特性；若 pokedex 页抓到则以英文归一化覆盖
            ability: hidden_ability.unwrap_or_else(|| "无特性".to_string()),
            moves: Vec::new(), // 稍后由 pokedex 补全并归一化
            period,
            location: loc_zh_full,
            location_en: loc_en,
            reporter: None,
            gender: Gender::from_male_percent(pokedex.gender_of(pid)),
            egg_groups: pokedex.egg_groups_of(pid),
            egg_groups_en: Vec::new(),
            extra_lines: vec![ExtraLine::new(
                format!("数据来源: {SOURCE_LABEL}"),
                format!("Source: {SOURCE_LABEL}"),
            )],
            pokedex_id: pid,
            ability_id: hidden_ability_id,
            move_ids: Vec::new(), // 稍后由 pokedex 补全
            source: NAME.to_string(),
            // 源没给报点时间；用当前时刻，供源投票的「最新优先」兜底
            reported_at: alpha_core::time::now_str(),
        };

        Some(ActiveBoss {
            boss,
            remaining,
            region,
            location,
            slot_name,
            slot_range,
        })
    }

    // ---------------- 解析：pokedex 页 ----------------

    /// 从 `/pokedex/{pid}` 页面按 `(region, location)` 匹配 spawn-entry，
    /// 抽 4 技能 + 隐藏特性 + 秘传技。
    pub fn parse_pokedex(
        html: &str,
        region: Option<&str>,
        location: Option<&str>,
    ) -> PokedexPage {
        let mut out = PokedexPage::default();

        // 1) 特性：pokedex-ability-link，带 pokedex-hidden-ability-icon 的是隐藏特性
        for caps in ABILITY_LINK_RE.captures_iter(html) {
            let blk = &caps[1];
            let Some(nm) = DATA_VALUE_RE.captures(blk) else {
                continue;
            };
            let name = nm[1].to_string();
            if blk.contains("pokedex-hidden-ability-icon") {
                out.hidden_ability = Some(name.clone());
            }
            out.abilities.push(name);
        }

        // 2) alpha 面板下的 spawn-entry（按地点匹配）
        let panel = extract_alpha_panel(html);

        let entries: Vec<&str> = SPAWN_ENTRY_RE
            .captures_iter(panel)
            .map(|c| c.get(1).unwrap().as_str())
            .collect();

        let mut target: Option<&str> = None;
        for e in &entries {
            let reg_v = ENTRY_REGION_RE
                .captures(e)
                .map(|c| c[1].to_lowercase())
                .unwrap_or_default();
            let loc_v = ENTRY_LOC_RE
                .captures(e)
                .map(|c| c[1].to_lowercase())
                .unwrap_or_default();

            let loc_ok = location
                .map(|l| !l.is_empty() && loc_v.contains(&l.to_lowercase()))
                .unwrap_or(false);
            let reg_ok = region
                .map(|r| r.is_empty() || reg_v.is_empty() || reg_v.contains(&r.to_lowercase()))
                .unwrap_or(true);

            if loc_ok && reg_ok {
                target = Some(e);
                break;
            }
        }
        // 匹配不到就退回第一条（源站偶尔地区字段缺失）
        if target.is_none() {
            target = entries.first().copied();
        }

        if let Some(entry) = target {
            for caps in SPAWN_MOVES_RE.captures_iter(entry) {
                let b = &caps[1];
                let label = LABEL_RE.captures(b).map(|c| c[1].to_string()).unwrap_or_default();
                let vals: Vec<String> = DATA_VALUE_RE
                    .captures_iter(b)
                    .map(|c| c[1].to_string())
                    .collect();
                if label.contains("HMs") {
                    out.hms = vals;
                } else if label.contains("Moves") {
                    out.moves = vals.into_iter().take(4).collect();
                }
            }
        }
        out
    }

    async fn fetch_pokedex(
        &self,
        pid: Option<i64>,
        region: Option<&str>,
        location: Option<&str>,
    ) -> PokedexPage {
        let Some(pid) = pid else {
            return PokedexPage::default();
        };
        let url = format!("{BASE_SITE}/pokedex/{pid}");
        match self.http.get_text(&url, &Self::headers()).await {
            Ok(text) => Self::parse_pokedex(&text, region, location),
            Err(e) => {
                tracing::warn!("[{NAME}] 图鉴页抓取失败（不阻断主流程）: {e}");
                PokedexPage::default()
            }
        }
    }

    /// 把图鉴页抓到的英文技能/特性归一化，并补齐秘传技附加行。
    fn apply_pokedex_page(boss: &mut BossData, page: &PokedexPage, pokedex: &Pokedex) {
        if !page.moves.is_empty() {
            boss.moves = page
                .moves
                .iter()
                .map(|m| pokedex.canonical_move(m))
                .collect();
            boss.move_ids = page
                .moves
                .iter()
                .map(|m| pokedex.resolve_move_id(m))
                .collect();
        }
        if let Some(en_ability) = &page.hidden_ability {
            boss.ability = pokedex.canonical_ability(en_ability);
            if let Some(aid) = pokedex.resolve_ability_id(en_ability) {
                boss.ability_id = Some(aid);
            }
        }
        if !page.hms.is_empty() {
            let hms_zh: Vec<String> = page.hms.iter().map(|h| hm_zh(h)).collect();
            boss.extra_lines.push(ExtraLine::new(
                format!("秘传技需求: {}", hms_zh.join(", ")),
                format!("HMs required: {}", page.hms.join(", ")),
            ));
        }
    }
}

/// 截出 `id="pokedex-alpha"` 面板的 HTML 片段。
///
/// # 为什么不用正则
///
/// 原版 Python 用的是
/// `r'id="pokedex-alpha".*?(?=<div id="pokedex-[a-z]|</div>\s*</div>\s*</div>\s*</div>)'`，
/// 靠**前瞻断言**在「下一个面板开始」或「连续 4 个闭合 div」处停下。
/// Rust 的 `regex` crate 出于性能考虑**不支持前瞻 / 后顾**，照搬会 panic，
/// 而单靠贪婪匹配 + 手工截断又表达不了「两个终止条件取更近者」。
/// 所以这里手写一个小扫描器，把两个候选位置都算出来再取 `min`。
///
/// # 终止条件（两个都是**不含**结束标记）
///
/// 1. 下一个 `<div id="pokedex-xxx">` —— 兄弟面板开始
/// 2. 连续 4 个 `</div>` 的**起始位置** —— 面板自身的嵌套层级闭合
///
/// 第 2 条要特别注意：前瞻断言 `(?=...)` **不消费**它匹配的内容，所以
/// Python 的 `panel` 是**不含**那 4 个 `</div>` 的。手写时如果把 `i` 推进
/// 到 `</div>` 之后，就会多截一段 —— 见 `alpha_panel_extract` 回归测试。
///
/// 找不到面板头时返回整篇 HTML，与原版 `panel = html` 的兜底行为一致。
///
/// 公开是为了让回归测试能直接对着 Python 前瞻断言的 oracle 比对 ——
/// 这个函数纯字符串进出，没有副作用，测起来成本极低。
pub fn extract_alpha_panel(html: &str) -> &str {
    let Some(head_pos) = html.find(ALPHA_PANEL_HEAD) else {
        return html;
    };
    let scan_from = head_pos + ALPHA_PANEL_HEAD.len();

    // 条件 1：下一个 pokedex-* 面板
    let next_panel = NEXT_PANEL_RE
        .find(&html[scan_from..])
        .map(|m| scan_from + m.start());

    // 条件 2：连续 4 个 </div>（`\s*` 允许任意空白 / 换行）
    //
    // `run_start` 记录本轮连续段的起点 —— 到第 4 个 `</div>` 时，
    // 前瞻的「边界」就是这个起点，而不是第 4 个 `</div>` 的结束位置。
    let mut i = scan_from;
    let mut run = 0usize;
    let mut run_start = scan_from;
    let mut close_end: Option<usize> = None;
    while i < html.len() {
        if html[i..].starts_with("</div>") {
            if run == 0 {
                run_start = i;
            }
            run += 1;
            if run >= 4 {
                close_end = Some(run_start);
                break;
            }
            i += "</div>".len();
        } else if html.as_bytes()[i].is_ascii_whitespace() {
            // 空白不打断「连续」计数，对应原版 `\s*`
            i += 1;
        } else {
            run = 0;
            // 按 UTF-8 字符边界前进，避免在多字节字符中间切片
            i += html[i..].chars().next().map_or(1, |c| c.len_utf8());
        }
    }

    let end = match (next_panel, close_end) {
        (Some(a), Some(b)) => a.min(b),
        (Some(a), None) => a,
        (None, Some(b)) => b,
        (None, None) => html.len(),
    };
    &html[head_pos..end]
}

#[async_trait::async_trait]
impl DataSource for PokemmotoolsLandingSource {
    fn name(&self) -> &'static str {
        NAME
    }

    async fn fetch(&self) -> FetchResult {
        let pokedex = match alpha_core::pokedex::get_pokedex() {
            Ok(p) => p,
            Err(e) => return err_result(format!("图鉴加载失败: {e}")),
        };

        let html = match &self.sample_html {
            Some(h) => {
                tracing::info!("[{NAME}] 使用注入的样本数据（不发网络请求）");
                h.clone()
            }
            None => {
                if self.target.is_empty() {
                    return err_result("未配置 target");
                }
                let url = self.http.build_url(&self.target);
                match self.http.get_text(&url, &Self::headers()).await {
                    Ok(t) => t,
                    Err(e) => return err_result(format!("请求失败: {e}")),
                }
            }
        };

        let Some(mut info) = self.parse(&html, pokedex) else {
            return FetchResult::empty(
                "当前无活跃头目(alpha-list 未发现 currently active)",
            );
        };

        // 第 2 步：去 pokedex 页拿 4 技能 + 特性（按地点匹配）
        // 离线注入时没有图鉴页 html，跳过（不阻断）。
        if self.sample_html.is_none() {
            let page = self
                .fetch_pokedex(
                    info.boss.pokedex_id,
                    info.region.as_deref(),
                    info.location.as_deref(),
                )
                .await;
            Self::apply_pokedex_page(&mut info.boss, &page, pokedex);
        }

        let boss = info.boss;
        tracing::info!(
            "[{NAME}] 命中 {} / {} / 技能={:?} / 剩余≈{:?}s",
            boss.name,
            boss.location,
            boss.moves,
            info.remaining
        );

        // 摘要（推送标题）与正文首行同源，都用 boss.period
        let summary = boss.period.clone();
        let name = boss.name.clone();
        FetchResult::hit(boss)
            .with_slot(summary, slot_name_en(info.slot_name).to_string())
            .with_message(format!("命中: {name}"))
    }
}

// ------------------------------------------------------------- 小工具

fn join_dot(parts: &[Option<&str>]) -> String {
    parts
        .iter()
        .flatten()
        .filter(|s| !s.is_empty())
        .copied()
        .collect::<Vec<_>>()
        .join(" · ")
}

/// 极简 query string 解析（够用，不引额外依赖）。
fn parse_qs(query: &str) -> HashMap<String, Vec<String>> {
    let mut out: HashMap<String, Vec<String>> = HashMap::new();
    for pair in query.split('&') {
        if pair.is_empty() {
            continue;
        }
        let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
        out.entry(percent_decode(k))
            .or_default()
            .push(percent_decode(v));
    }
    out
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b'%' if i + 2 < bytes.len() => {
                let hex = &s[i + 1..i + 3];
                match u8::from_str_radix(hex, 16) {
                    Ok(b) => {
                        out.push(b);
                        i += 3;
                    }
                    Err(_) => {
                        out.push(bytes[i]);
                        i += 1;
                    }
                }
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// HTML 实体解码（只需处理属性值里常见的几个）。
fn html_unescape(s: &str) -> String {
    s.replace("&amp;", "&")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
}
