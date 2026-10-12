//! 数据源适配器基类。
//!
//! 新增一个数据源只需要三步：
//! 1. 实现 [`DataSource`]（通常直接嵌一个 [`HttpClient`]）
//! 2. 在 `config/sources.yaml` 里加一段配置，`adapter` 填它的标识
//! 3. 在 [`registry`] 里登记
//!
//! 主流程不需要任何改动 —— 它只认 [`FetchResult`]。

use std::collections::HashMap;
use std::time::Duration;

use alpha_core::config::Config;
use alpha_core::models::{BossData, ExtraLine, FetchResult, FetchStatus};
use alpha_core::pokedex::Pokedex;
use serde_json::Value;

use crate::error::{SourceError, SourceResult};

/// 源适配器统一接口。
///
/// 用 `async_trait` 而不是原生 async trait，是为了保留 `dyn` 能力 ——
/// 注册表需要把不同适配器塞进同一个 `Vec<Box<dyn DataSource>>`。
#[async_trait::async_trait]
pub trait DataSource: Send + Sync {
    /// 适配器标识，必须与 `sources.yaml` 的 `adapter` 字段一致。
    fn name(&self) -> &'static str;

    /// 拉取并归一化为一条 [`FetchResult`]。
    ///
    /// 约定：**不要把异常往上抛**。源不可达是常态，
    /// 一律转成 `FetchStatus::Error` 并带上人话 message。
    async fn fetch(&self) -> FetchResult;
}

/// 已构造好的源实例（适配器 + 它的配置）。
pub struct SourceHandle {
    pub config: alpha_core::config::SourceConfig,
    pub adapter: Box<dyn DataSource>,
}

// ---------------------------------------------------------------- HTTP

/// 带重试 / 超时 / 转发服务的 HTTP 客户端。
///
/// 所有适配器共用这一层，别自己写 `reqwest`。
pub struct HttpClient {
    client: reqwest::Client,
    /// 源级代理（只影响本源请求）
    proxy: Option<String>,
    verify_ssl: bool,
    /// 连接超时：连不上时快速失败，不要白等整个 timeout
    connect_timeout: Duration,
    /// 读超时
    read_timeout: Duration,
    /// 重试前的等待序列
    retry_delays: Vec<f64>,
    /// 转发服务配置（源站在海外、面板在国内时用）
    transform: Option<TransformPlan>,
    /// 适配器名，仅用于日志
    tag: String,
}

/// 转发服务配置。
///
/// 源站在海外、面板跑在国内机器上时直连不通，需要一层中转。
/// 协议：POST 到中转地址，用 query 传目标地址：
/// `?url=<目标URL>&method=GET` —— method 必须放 query，放 header 不生效；
/// `x-proxy-key: <key>` 是中转自身的鉴权（可选）。
#[derive(Debug, Clone)]
pub struct TransformPlan {
    pub base: String,
    pub method: String,
    pub headers: HashMap<String, String>,
}

impl HttpClient {
    /// 按源配置 + 全局 HTTP 设置构造。
    pub fn new(tag: &str, options: &alpha_core::config::SourceOptions, cfg: &Config) -> SourceResult<Self> {
        let http = &cfg.settings.http;

        let proxy = options
            .proxy
            .clone()
            .filter(|s| !s.trim().is_empty())
            .or_else(|| http.proxy.clone().filter(|s| !s.trim().is_empty()))
            .or_else(|| std::env::var("HTTPS_PROXY").ok().filter(|s| !s.is_empty()))
            .or_else(|| std::env::var("https_proxy").ok().filter(|s| !s.is_empty()));

        let read_timeout = Duration::from_secs(http.timeout.max(1));
        let connect_timeout =
            Duration::from_secs(http.connect_timeout.max(1).min(http.timeout.max(1)));

        let mut builder = reqwest::Client::builder()
            .connect_timeout(connect_timeout)
            .timeout(read_timeout)
            .danger_accept_invalid_certs(!http.verify_ssl)
            .user_agent("pokemmo-alpha/2.0 (+rust)");

        if let Some(p) = proxy.as_deref() {
            builder = builder.proxy(
                reqwest::Proxy::all(p)
                    .map_err(|e| SourceError::Config(format!("代理地址无效 {p}: {e}")))?,
            );
        } else {
            // 显式禁用系统代理探测，行为才可预期
            builder = builder.no_proxy();
        }

        let client = builder
            .build()
            .map_err(|e| SourceError::Config(format!("构造 HTTP 客户端失败: {e}")))?;

        Ok(Self {
            client,
            proxy,
            verify_ssl: http.verify_ssl,
            connect_timeout,
            read_timeout,
            retry_delays: http.retry_delays.clone(),
            transform: Self::build_transform(options),
            tag: tag.to_string(),
        })
    }

    fn build_transform(options: &alpha_core::config::SourceOptions) -> Option<TransformPlan> {
        let env_name = options.transform_url_env.as_ref()?;
        let raw = std::env::var(env_name).unwrap_or_default();
        let base = raw.trim();
        if base.is_empty() {
            return None;
        }

        let mut headers = yaml_headers_to_map(options.transform_headers.as_ref());
        let key_env = options
            .proxy_key_env
            .clone()
            .unwrap_or_else(|| "PROXY_KEY".to_string());
        if let Ok(key) = std::env::var(&key_env) {
            let key = key.trim();
            if !key.is_empty() {
                headers.insert("x-proxy-key".to_string(), key.to_string());
            }
        }

        Some(TransformPlan {
            base: base.trim_end_matches(['?', '&']).to_string(),
            method: options
                .transform_method
                .clone()
                .unwrap_or_else(|| "POST".to_string())
                .to_uppercase(),
            headers,
        })
    }

    /// 当前是否配了转发服务。
    pub fn has_transform(&self) -> bool {
        self.transform.is_some()
    }

    pub fn proxy(&self) -> Option<&str> {
        self.proxy.as_deref()
    }

    pub fn verify_ssl(&self) -> bool {
        self.verify_ssl
    }

    pub fn connect_timeout(&self) -> Duration {
        self.connect_timeout
    }

    pub fn read_timeout(&self) -> Duration {
        self.read_timeout
    }

    /// 拼接最终请求地址：配了转发就走转发，否则直连。
    pub fn build_url(&self, target: &str) -> String {
        match &self.transform {
            None => target.to_string(),
            Some(tp) => {
                let encoded = percent_encode(target);
                format!("{}?url={}&method=GET", tp.base, encoded)
            }
        }
    }

    /// 实际使用的 HTTP 方法（走转发时用中转要求的方法，一般是 POST）。
    pub fn effective_method(&self) -> &str {
        match &self.transform {
            None => "GET",
            Some(tp) => tp.method.as_str(),
        }
    }

    fn apply_transform_headers(&self, headers: &mut HashMap<String, String>) {
        if let Some(tp) = &self.transform {
            for (k, v) in &tp.headers {
                headers.insert(k.clone(), v.clone());
            }
        }
    }

    /// 取文本（带重试）。4xx 直接抛 [`SourceError::Http`]，不重试。
    pub async fn get_text(
        &self,
        url: &str,
        headers: &HashMap<String, String>,
    ) -> SourceResult<String> {
        let method = self.effective_method().to_string();
        let mut hdrs = headers.clone();
        self.apply_transform_headers(&mut hdrs);

        let mut last_err: Option<String> = None;
        let attempts = if self.retry_delays.is_empty() {
            1
        } else {
            self.retry_delays.len()
        };

        for i in 0..attempts {
            if let Some(d) = self.retry_delays.get(i) {
                if *d > 0.0 {
                    tokio::time::sleep(Duration::from_secs_f64(*d)).await;
                }
            }

            let mut req = self
                .client
                .request(
                    reqwest::Method::from_bytes(method.as_bytes())
                        .unwrap_or(reqwest::Method::GET),
                    url,
                )
                .timeout(self.read_timeout);
            for (k, v) in &hdrs {
                req = req.header(k.as_str(), v.as_str());
            }

            match req.send().await {
                Ok(resp) => {
                    let code = resp.status().as_u16();
                    // 4xx 是「配置/接口层面」的问题，重试没有意义
                    if (400..500).contains(&code) {
                        return Err(SourceError::Http {
                            code,
                            url: url.to_string(),
                            hint: http_hint(code),
                        });
                    }
                    match resp.error_for_status() {
                        Ok(ok) => match ok.text().await {
                            Ok(t) => return Ok(t),
                            Err(e) => last_err = Some(format!("读取响应失败: {e}")),
                        },
                        Err(e) => last_err = Some(format!("HTTP {e}")),
                    }
                }
                Err(e) => last_err = Some(describe_reqwest(&e)),
            }

            tracing::warn!(
                "[{}] 第 {}/{} 次请求失败: {}",
                self.tag,
                i + 1,
                attempts,
                last_err.as_deref().unwrap_or("未知错误")
            );
        }

        Err(SourceError::Request(format!(
            "请求失败（已重试 {attempts} 次）: {}",
            last_err.unwrap_or_else(|| "未知错误".to_string())
        )))
    }

    /// 取 JSON（带重试）。走转发时发 POST，直连时发 GET。
    pub async fn get_json(
        &self,
        url: &str,
        headers: &HashMap<String, String>,
    ) -> SourceResult<Value> {
        let text = self.get_text(url, headers).await?;
        serde_json::from_str(&text)
            .map_err(|e| SourceError::Parse(format!("JSON 解析失败: {e}（前 200 字符: {}）", preview(&text))))
    }

    /// 发一个 JSON body 的 POST（GraphQL 等）。
    pub async fn post_json(
        &self,
        url: &str,
        body: &Value,
        headers: &HashMap<String, String>,
    ) -> SourceResult<Value> {
        let mut hdrs = headers.clone();
        hdrs.entry("content-type".into())
            .or_insert_with(|| "application/json".into());

        let mut last_err: Option<String> = None;
        let attempts = if self.retry_delays.is_empty() {
            1
        } else {
            self.retry_delays.len()
        };

        for i in 0..attempts {
            if let Some(d) = self.retry_delays.get(i) {
                if *d > 0.0 {
                    tokio::time::sleep(Duration::from_secs_f64(*d)).await;
                }
            }

            let mut req = self.client.post(url).timeout(self.read_timeout);
            for (k, v) in &hdrs {
                req = req.header(k.as_str(), v.as_str());
            }
            req = req.body(body.to_string());

            match req.send().await {
                Ok(resp) => {
                    let code = resp.status().as_u16();
                    if (400..500).contains(&code) {
                        return Err(SourceError::Http {
                            code,
                            url: url.to_string(),
                            hint: http_hint(code),
                        });
                    }
                    match resp.error_for_status() {
                        Ok(ok) => match ok.json::<Value>().await {
                            Ok(v) => return Ok(v),
                            Err(e) => last_err = Some(format!("JSON 解析失败: {e}")),
                        },
                        Err(e) => last_err = Some(format!("HTTP {e}")),
                    }
                }
                Err(e) => last_err = Some(describe_reqwest(&e)),
            }

            tracing::warn!(
                "[{}] 第 {}/{} 次 POST 失败: {}",
                self.tag,
                i + 1,
                attempts,
                last_err.as_deref().unwrap_or("未知错误")
            );
        }

        Err(SourceError::Request(format!(
            "请求失败（已重试 {attempts} 次）: {}",
            last_err.unwrap_or_else(|| "未知错误".to_string())
        )))
    }
}

/// 把 YAML 里写的 `transform_headers` 拍平成 `HashMap<String, String>`。
///
/// 配置里写的是 `{key: value}` 映射；非字符串值统一 `to_string()`，
/// 避免因为顺手写了个数字就得改配置。
fn yaml_headers_to_map(v: Option<&serde_yaml::Value>) -> HashMap<String, String> {
    let mut out = HashMap::new();
    let Some(serde_yaml::Value::Mapping(m)) = v else {
        return out;
    };
    for (k, val) in m {
        let key = match k {
            serde_yaml::Value::String(s) => s.clone(),
            other => serde_yaml::to_string(other)
                .unwrap_or_default()
                .trim()
                .to_string(),
        };
        let value = match val {
            serde_yaml::Value::String(s) => s.clone(),
            serde_yaml::Value::Null => String::new(),
            other => serde_yaml::to_string(other)
                .unwrap_or_default()
                .trim()
                .to_string(),
        };
        if !key.is_empty() {
            out.insert(key, value);
        }
    }
    out
}

/// 把常见的 4xx 翻译成人话，别让人对着状态码猜。
pub fn http_hint(code: u16) -> &'static str {
    match code {
        401 => "需要 API Key/登录态（把 key 配到环境变量后重启）",
        403 => "被拒绝（key 无效或权限不足）",
        404 => "接口不存在（源站可能改了路径）",
        410 => "接口已永久下线（源站换了新接口，需要更新 target）",
        429 => "触发限频（降低轮询频率）",
        _ => "请求被拒绝",
    }
}

fn describe_reqwest(e: &reqwest::Error) -> String {
    if e.is_timeout() {
        format!("超时: {e}")
    } else if e.is_connect() {
        format!("连接失败（源站不可达 / 需要代理）: {e}")
    } else if e.is_decode() {
        format!("响应解码失败: {e}")
    } else {
        format!("{e}")
    }
}

fn preview(s: &str) -> String {
    let t: String = s.chars().take(200).collect();
    t.replace('\n', " ")
}

/// 极简 percent-encoding（只编 URL 里必须编的字符，避免引入额外依赖）。
fn percent_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len() * 2);
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

// ---------------------------------------------------- 名称解析 / 附加信息

/// 名称解析 + 附加信息构造，所有适配器共用。
pub struct NameResolver<'a> {
    pub pokedex: &'a Pokedex,
}

impl<'a> NameResolver<'a> {
    pub fn new(pokedex: &'a Pokedex) -> Self {
        Self { pokedex }
    }

    /// 把任意语言的名字解析成 `(canonical 中文名, id)`。
    ///
    /// **优先用英文名查**（英文名是标准名，能绕开机翻）；
    /// 英文名缺失或查不到时，退回中文名 + 别名表。
    pub fn resolve_entry(
        &self,
        zh: Option<&str>,
        en: Option<&str>,
        kind: NameKind,
    ) -> (String, Option<i64>) {
        let pd = self.pokedex;

        let (resolve, canonical): (ResolverFn, CanonicalFn) = match kind {
            NameKind::Move => (|p, s| p.resolve_move_id(s), |p, s| p.canonical_move(s)),
            NameKind::Ability => (|p, s| p.resolve_ability_id(s), |p, s| p.canonical_ability(s)),
            NameKind::Pokemon => (|p, s| p.resolve_pokemon_id(s), |p, s| p.canonical_pokemon(s)),
        };

        if let Some(en) = en.filter(|s| !s.trim().is_empty()) {
            if let Some(id) = resolve(pd, en) {
                return (canonical(pd, en), Some(id));
            }
        }
        if let Some(zh) = zh.filter(|s| !s.trim().is_empty()) {
            if let Some(id) = resolve(pd, zh) {
                return (canonical(pd, zh), Some(id));
            }
            // 查不到就原样保留，不丢信息
            return (zh.to_string(), None);
        }
        (en.unwrap_or("").to_string(), None)
    }

    /// 把一组中文值渲染成附加信息行，英文值自动翻译（技能类查图鉴）。
    pub fn make_extra(
        &self,
        label_zh: &str,
        label_en: &str,
        values_zh: &[String],
    ) -> Option<ExtraLine> {
        if values_zh.is_empty() {
            return None;
        }
        let en_vals: Vec<String> = values_zh
            .iter()
            .map(|v| self.pokedex.translate(v, "move", "en"))
            .collect();
        Some(ExtraLine::new(
            format!("{label_zh}: {}", values_zh.join(", ")),
            format!("{label_en}: {}", en_vals.join(", ")),
        ))
    }
}

/// 「名字 → 数字 id」的解析函数指针（按 [`NameKind`] 选一个）。
pub type ResolverFn = fn(&Pokedex, &str) -> Option<i64>;

/// 「名字 → 官方规范名」的函数指针（按 [`NameKind`] 选一个）。
pub type CanonicalFn = fn(&Pokedex, &str) -> String;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NameKind {
    Move,
    Ability,
    Pokemon,
}

// ------------------------------------------------------------ 去重兜底

/// 兜底指纹：用头目内容算（名称+特性+技能+地点+时段）。
///
/// 每个源「什么算同一条头目」标准不一样，交给适配器自己在
/// [`FetchResult::dedup_key`] 里决定；什么都不填才走这里。
pub fn content_fingerprint(boss: Option<&BossData>) -> String {
    alpha_core::dedup::content_fingerprint(boss)
}

/// 源结果的去重标识：优先用适配器给的，否则按内容算指纹。
pub fn dedup_key(r: &FetchResult) -> String {
    let k = r.dedup_key.trim();
    if !k.is_empty() {
        return k.to_string();
    }
    content_fingerprint(r.boss.as_ref())
}

// -------------------------------------------------------- 通用解析工具

/// 无性别的各种写法。
const GENDERLESS: &[&str] = &[
    "n/a", "na", "none", "", "-", "无", "无性别", "genderless", "unknown",
];

/// 解析雄性百分比。
///
/// 各数据源给的性别格式五花八门，这里做防御式解析：
/// - `"50%"`   -> `50.0`
/// - `"50"`    -> `50.0`
/// - `"0.5"`   -> `50.0`（比例写法）
/// - `"N/A"`   -> `None`（无性别）
/// - `"公1母7"` -> `12.5`
pub fn parse_male_ratio(value: Option<&Value>) -> Option<f64> {
    let v = value?;
    match v {
        Value::Null => None,
        Value::Number(n) => n.as_f64(),
        Value::Bool(_) => None,
        Value::String(s) => parse_male_ratio_str(s),
        _ => None,
    }
}

/// 字符串版的雄性百分比解析。
pub fn parse_male_ratio_str(s: &str) -> Option<f64> {
    let s = s.trim();
    if GENDERLESS.contains(&s.to_lowercase().as_str()) || s.is_empty() {
        return None;
    }

    // "公1母7" / "♂1♀7" 这类写法
    if let Some(caps) = MALE_FEMALE_RE.captures(s) {
        let male: f64 = caps[1].parse().ok()?;
        let female: f64 = caps[2].parse().ok()?;
        let total = male + female;
        if total <= 0.0 {
            return None;
        }
        return Some((male * 100.0 / total * 10.0).round() / 10.0);
    }

    let caps = NUM_RE.captures(s)?;
    let raw = caps.get(1)?.as_str();
    let mut val: f64 = raw.parse().ok()?;
    // 0.x 这种比例写法（且不是 "0"）转成百分比
    if raw.contains('.') && val > 0.0 && val <= 1.0 {
        val *= 100.0;
    }
    Some(val)
}

static MALE_FEMALE_RE: once_cell::sync::Lazy<regex::Regex> = once_cell::sync::Lazy::new(|| {
    regex::Regex::new(r"公\s*(\d+(?:\.\d+)?)\s*母\s*(\d+(?:\.\d+)?)").unwrap()
});
static NUM_RE: once_cell::sync::Lazy<regex::Regex> =
    once_cell::sync::Lazy::new(|| regex::Regex::new(r"(\d+(?:\.\d+)?)\s*%?").unwrap());

static EXTRACT_REPORTER_RE: once_cell::sync::Lazy<regex::Regex> =
    once_cell::sync::Lazy::new(|| regex::Regex::new(r"报点人[:：](@?[\w]+)").unwrap());

/// 从自由文本里抠报点人（源站没给独立字段时的现实妥协）。
pub fn extract_reporter(text: &str) -> String {
    EXTRACT_REPORTER_RE
        .captures(text)
        .and_then(|c| c.get(1))
        .map(|m| m.as_str().trim().to_string())
        .unwrap_or_default()
}

/// 便捷构造：`FetchStatus::Error` + 人话消息。
pub fn err_result(msg: impl Into<String>) -> FetchResult {
    FetchResult::error(msg)
}

/// 便捷构造：`FetchStatus::Empty` + 说明。
pub fn empty_result(msg: impl Into<String>) -> FetchResult {
    FetchResult::empty(msg)
}

/// 断言式便捷：把 `Result` 折成 `FetchResult`。
pub fn map_err<E: std::fmt::Display>(e: E, ctx: &str) -> FetchResult {
    FetchResult::error(format!("{ctx}: {e}"))
}

/// `FetchStatus` 的人话名（日志用）。
pub fn status_label(s: FetchStatus) -> &'static str {
    match s {
        FetchStatus::Hit => "hit",
        FetchStatus::Empty => "empty",
        FetchStatus::Error => "error",
    }
}
