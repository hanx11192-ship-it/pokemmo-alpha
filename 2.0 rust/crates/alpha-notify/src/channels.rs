//! 推送渠道。
//!
//! # 设计要点
//!
//! - 三类渠道：[`ChannelKind::Wxpusher`] / [`ChannelKind::Webhook`] / [`ChannelKind::ServerChan`]
//! - **每个渠道单独成败**，一个渠道挂了不影响其它渠道
//! - **全部失败才算失败**：只要有任意一个渠道送达，就认为本轮推送成功、
//!   可以写去重标记；全挂则报错，让调用方下轮重试
//! - 渠道表为空时回退到老行为（读环境变量的 WxPusher），保证升级后推送不断
//!
//! # 为什么「失败必须报错」
//!
//! 原版早期版本在 `send()` 内部把所有异常吞掉，然后调用方照样写 `time.txt`，
//! 结果是「推送没发出去，但被标记为已处理」—— 这条头目就**永远丢了**。
//! 所以这里的铁律是：失败必须让调用方知道。

use std::collections::HashMap;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::{NotifyError, NotifyResult};

/// 渠道类型。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ChannelKind {
    Wxpusher,
    Webhook,
    Serverchan,
}

impl ChannelKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Wxpusher => "wxpusher",
            Self::Webhook => "webhook",
            Self::Serverchan => "serverchan",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s.to_lowercase().as_str() {
            "wxpusher" => Some(Self::Wxpusher),
            "webhook" => Some(Self::Webhook),
            "serverchan" => Some(Self::Serverchan),
            _ => None,
        }
    }

    /// 面板表单用的人类可读名。
    pub fn label(&self) -> &'static str {
        match self {
            Self::Wxpusher => "WxPusher 微信推送",
            Self::Webhook => "Webhook（通用 JSON）",
            Self::Serverchan => "Server 酱",
        }
    }

    pub fn desc(&self) -> &'static str {
        match self {
            Self::Wxpusher => "通过 WxPusher 公众号推送到微信",
            Self::Webhook => "向任意 URL POST 一段 JSON，可自定义字段结构",
            Self::Serverchan => "通过 Server 酱（sctapi）推送到微信服务号",
        }
    }

    pub fn all() -> &'static [ChannelKind] {
        &[Self::Wxpusher, Self::Webhook, Self::Serverchan]
    }

    /// 整张「渠道类型元信息」表，形状与原版 `channels.CHANNEL_TYPES` 一致。
    ///
    /// 即 `{ "<kind>": { "label": ..., "desc": ..., "fields": [...] } }`。
    /// 面板的 `GET /api/channels` 直接把它塞进响应里，
    /// 前端据此渲染新增表单与类型下拉框。
    ///
    /// # 为什么是 `Value` 而不是强类型
    ///
    /// 这个结构的唯一消费者是前端 JSON 解析器，后端自己不读它。
    /// 定义成结构体只会多一层「结构体 → JSON」的搬运，
    /// 而真正需要类型保护的部分（`fields` 的内容）已经在
    /// [`ChannelKind::fields`] 里了。
    pub fn types_table() -> serde_json::Value {
        let mut out = serde_json::Map::new();
        for k in Self::all() {
            out.insert(
                k.as_str().to_string(),
                serde_json::json!({
                    "label": k.label(),
                    "desc": k.desc(),
                    "fields": k.fields(),
                }),
            );
        }
        serde_json::Value::Object(out)
    }

    /// 表单字段元数据 —— 面板「分发渠道」页靠它渲染新增/编辑表单。
    ///
    /// 对应原版 `panel/channels.py` 的 `CHANNEL_TYPES[t]["fields"]`。
    /// `label` / `desc` / 三个 `placeholder` **逐字对齐**原版
    /// （含 `AT_xxx（留空则读环境变量 WXPUSHER_APP_TOKEN_alpha）` 这种长提示）。
    ///
    /// # 为什么放在这里而不是前端
    ///
    /// 放前端的话，后端新增一种渠道类型时前端会渲染出空表单；
    /// 放后端则「渠道类型」这个知识只有一份 —— 前后端都以它为准。
    pub fn fields(&self) -> &'static [ChannelField] {
        match self {
            Self::Wxpusher => &[
                ChannelField {
                    key: "app_token",
                    label: "App Token",
                    kind: FieldKind::Password,
                    placeholder: "AT_xxx（留空则读环境变量 WXPUSHER_APP_TOKEN_alpha）",
                },
                ChannelField {
                    key: "topic_ids",
                    label: "主题 ID",
                    kind: FieldKind::Text,
                    placeholder: "45385，多个用逗号分隔",
                },
            ],
            Self::Webhook => &[
                ChannelField {
                    key: "url",
                    label: "Webhook 地址",
                    kind: FieldKind::Text,
                    placeholder: "https://...",
                },
                ChannelField {
                    key: "template",
                    label: "Body 模板（JSON）",
                    kind: FieldKind::Textarea,
                    placeholder: r#"{"msgtype":"text","text":{"content":"{content}"}}"#,
                },
                ChannelField {
                    key: "headers",
                    label: "额外请求头（JSON，可选）",
                    kind: FieldKind::Textarea,
                    placeholder: r#"{"Authorization":"Bearer xxx"}"#,
                },
            ],
            Self::Serverchan => &[ChannelField {
                key: "send_key",
                label: "SendKey",
                kind: FieldKind::Password,
                placeholder: "SCTxxxx",
            }],
        }
    }
}

/// 渠道表单的一个输入项。
///
/// 字段名对齐原版的 `{key, label, type, placeholder}`。
/// `type` 用 [`FieldKind`] 而不是裸字符串，这样「有哪些控件种类」
/// 是类型系统里可见的 —— 前端多一个没实现的控件种类时，
/// 后端改这里会被编译器提醒。
#[derive(Debug, Clone, Serialize)]
pub struct ChannelField {
    pub key: &'static str,
    pub label: &'static str,
    #[serde(rename = "type")]
    pub kind: FieldKind,
    pub placeholder: &'static str,
}

/// 表单控件种类。序列化出来的字符串与原版一致。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum FieldKind {
    Text,
    Textarea,
    Password,
}

/// 一个渠道实例。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Channel {
    /// 数据库主键（新建时可为 None）
    #[serde(default)]
    pub id: Option<i64>,
    /// 显示名
    #[serde(default)]
    pub name: String,
    #[serde(rename = "type")]
    pub kind: ChannelKind,
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// 类型相关的配置（app_token / url / template / send_key ...）
    #[serde(default)]
    pub config: ChannelConfig,
}

fn default_true() -> bool {
    true
}

/// 渠道配置。用逐字段可选而不是 `HashMap`，是为了让「面板表单」
/// 和「后端校验」共用同一份结构定义，不至于各写一遍。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ChannelConfig {
    /// WxPusher：App Token（留空则读环境变量）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub app_token: Option<String>,
    /// WxPusher：主题 ID，逗号分隔或数组
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub topic_ids: Option<Value>,
    /// Webhook：目标地址
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// Webhook：Body 模板（JSON），支持 `{content}` / `{summary}` 占位
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub template: Option<String>,
    /// Webhook：额外请求头（JSON 字符串或对象）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub headers: Option<Value>,
    /// Server 酱：SendKey
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub send_key: Option<String>,
}

/// 单次发送的结果。
#[derive(Debug, Clone, Serialize)]
pub struct SendOutcome {
    pub name: String,
    #[serde(rename = "type")]
    pub kind: ChannelKind,
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// 整轮推送的汇总。
#[derive(Debug, Clone, Serialize)]
pub struct SendReport {
    /// 成功送达的渠道数
    pub sent: usize,
    /// 失败的渠道数
    pub failed: usize,
    pub details: Vec<SendOutcome>,
    /// 是否走了「无渠道时回退环境变量」的老路径
    pub legacy: bool,
}

impl SendReport {
    /// 至少一个渠道送达。
    pub fn any_ok(&self) -> bool {
        self.sent > 0
    }
}

// ---------------------------------------------------------------- 发送器

/// WxPusher 发送。`app_token` 为空时读环境变量，保持与老配置兼容。
const WXPUSHER_ENDPOINT: &str = "https://wxpusher.zjiecode.com/api/send/message";
const DEFAULT_TOKEN_ENV: &str = "WXPUSHER_APP_TOKEN_alpha";

pub async fn send_wxpusher(
    http: &reqwest::Client,
    cfg: &ChannelConfig,
    content: &str,
    summary: &str,
) -> NotifyResult<Value> {
    let token = cfg
        .app_token
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .or_else(|| {
            std::env::var(DEFAULT_TOKEN_ENV)
                .ok()
                .filter(|s| !s.trim().is_empty())
        })
        .ok_or_else(|| {
            NotifyError::Config(format!(
                "缺少 App Token（未填写，环境变量 {DEFAULT_TOKEN_ENV} 也未设置）"
            ))
        })?;

    let payload = serde_json::json!({
        "appToken": token,
        "content": content,
        "summary": if summary.is_empty() { "头目" } else { summary },
        "contentType": 1,
        "topicIds": parse_topic_ids(cfg.topic_ids.as_ref()),
    });

    let resp = http
        .post(WXPUSHER_ENDPOINT)
        .json(&payload)
        .timeout(Duration::from_secs(20))
        .send()
        .await
        .map_err(|e| NotifyError::Network(format!("WxPusher 推送失败: {e}")))?;

    let status = resp.status();
    let text = resp
        .text()
        .await
        .map_err(|e| NotifyError::Network(format!("WxPusher 响应读取失败: {e}")))?;
    if !status.is_success() {
        return Err(NotifyError::Network(format!(
            "WxPusher HTTP {status}: {}",
            truncate(&text, 300)
        )));
    }

    let data: Value = serde_json::from_str(&text)
        .map_err(|e| NotifyError::Parse(format!("WxPusher 响应不是 JSON: {e}；原文 {}", truncate(&text, 300))))?;

    // WxPusher 用 HTTP 200 + body 里的 code 表示业务失败，必须一起判断
    if let Some(code) = data.get("code").and_then(|v| v.as_i64()) {
        if code != 1000 {
            return Err(NotifyError::Business(format!("WxPusher 业务错误: {data}")));
        }
    }
    Ok(data)
}

/// Server 酱发送。
pub async fn send_serverchan(
    http: &reqwest::Client,
    cfg: &ChannelConfig,
    content: &str,
    summary: &str,
) -> NotifyResult<Value> {
    let key = cfg
        .send_key
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| NotifyError::Config("未填写 SendKey".into()))?;

    let url = format!("https://sctapi.ftqq.com/{key}.send");
    let resp = http
        .post(&url)
        .form(&[
            ("title", if summary.is_empty() { "头目" } else { summary }),
            ("desp", content),
        ])
        .timeout(Duration::from_secs(20))
        .send()
        .await
        .map_err(|e| NotifyError::Network(format!("Server酱推送失败: {e}")))?;

    let status = resp.status();
    let text = resp
        .text()
        .await
        .map_err(|e| NotifyError::Network(format!("Server酱响应读取失败: {e}")))?;
    if !status.is_success() {
        return Err(NotifyError::Network(format!(
            "Server酱 HTTP {status}: {}",
            truncate(&text, 300)
        )));
    }

    let data: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
    if let Some(code) = data.get("code").and_then(|v| v.as_i64()) {
        if code != 0 {
            return Err(NotifyError::Business(format!("Server酱错误: {data}")));
        }
    }
    Ok(data)
}

/// 通用 Webhook 发送。
///
/// 模板里的 `{content}` / `{summary}` 会被替换成**合法的 JSON 字符串片段**
/// （即 `json.dumps` 后去掉首尾引号）。这样即使正文里有英文双引号、反斜杠、
/// 换行，替换进模板后整体仍是合法 JSON —— 下游（如 qq-bridge）不会报 bad json。
pub async fn send_webhook(
    http: &reqwest::Client,
    cfg: &ChannelConfig,
    content: &str,
    summary: &str,
) -> NotifyResult<Value> {
    let url = cfg
        .url
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| NotifyError::Config("未填写 Webhook 地址".into()))?;

    let tpl = cfg
        .template
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or(r#"{"content": "{content}", "summary": "{summary}"}"#);
    let body_text = render_template(tpl, content, summary);

    let mut headers: HashMap<String, String> = HashMap::new();
    headers.insert("Content-Type".into(), "application/json".into());
    if let Some(extra) = &cfg.headers {
        let map = match extra {
            Value::String(s) => serde_json::from_str::<HashMap<String, Value>>(s)
                .ok()
                .map(|m| {
                    m.into_iter()
                        .map(|(k, v)| (k, value_to_string(&v)))
                        .collect::<HashMap<_, _>>()
                }),
            Value::Object(m) => Some(
                m.iter()
                    .map(|(k, v)| (k.clone(), value_to_string(v)))
                    .collect(),
            ),
            _ => None,
        };
        if let Some(map) = map {
            for (k, v) in map {
                headers.insert(k, v);
            }
        }
    }

    // 模板不是合法 JSON 时退化为纯文本提交（保持与原版一致的行为）
    let parsed: Option<Value> = serde_json::from_str(&body_text).ok();

    let mut req = http.post(url).timeout(Duration::from_secs(20));
    for (k, v) in &headers {
        req = req.header(k.as_str(), v.as_str());
    }
    let resp = match parsed {
        Some(body) => req.json(&body).send().await,
        None => {
            req = req.header("Content-Type", "text/plain; charset=utf-8");
            req.body(body_text.clone()).send().await
        }
    }
    .map_err(|e| NotifyError::Network(format!("Webhook 推送失败: {e}")))?;

    let status = resp.status();
    let text = resp.text().await.unwrap_or_default();
    if !status.is_success() {
        return Err(NotifyError::Network(format!(
            "Webhook HTTP {status}: {}",
            truncate(&text, 300)
        )));
    }
    Ok(serde_json::json!({
        "status_code": status.as_u16(),
        "body": truncate(&text, 300),
    }))
}

// ---------------------------------------------------------------- 分发

/// 只持有 HTTP 客户端的轻量发送器。
///
/// 从 [`Dispatcher`] 里拆出来，是为了能自由移进异步块（不借用 `self`）。
/// `reqwest::Client` 内部是 `Arc`，clone 很便宜，连接池共享。
#[derive(Clone)]
pub struct ChannelSender {
    http: reqwest::Client,
}

impl ChannelSender {
    /// 按渠道类型分发到对应发送器。
    pub async fn send_one(
        &self,
        ch: &Channel,
        content: &str,
        summary: &str,
    ) -> NotifyResult<()> {
        match ch.kind {
            ChannelKind::Wxpusher => {
                send_wxpusher(&self.http, &ch.config, content, summary).await?;
            }
            ChannelKind::Webhook => {
                send_webhook(&self.http, &ch.config, content, summary).await?;
            }
            ChannelKind::Serverchan => {
                send_serverchan(&self.http, &ch.config, content, summary).await?;
            }
        }
        Ok(())
    }

    /// 底层 client（供调试 / 复用）。
    pub fn client(&self) -> &reqwest::Client {
        &self.http
    }
}

/// 渠道分发器。
///
/// 持有单个复用的 `reqwest::Client`（连接池复用），
/// 以及可选的「渠道来源」（面板模式下从数据库读，CLI 模式下用静态列表）。
pub struct Dispatcher {
    http: reqwest::Client,
    /// 无渠道时的兜底：老行为（读环境变量的 WxPusher）
    legacy_fallback: bool,
    /// 兜底的 WxPusher topic（老配置里写死的）
    legacy_topic_ids: Vec<i64>,
}

impl Dispatcher {
    pub fn new(legacy_fallback: bool, legacy_topic_ids: Vec<i64>) -> NotifyResult<Self> {
        let http = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(5))
            .timeout(Duration::from_secs(20))
            .user_agent("pokemmo-alpha/2.0 (+rust)")
            .build()
            .map_err(|e| NotifyError::Config(format!("构造 HTTP 客户端失败: {e}")))?;
        Ok(Self {
            http,
            legacy_fallback,
            legacy_topic_ids,
        })
    }

    /// 向单个渠道发送。返回 `Ok(())` 或带人话原因的错误。
    pub async fn send_one(
        &self,
        ch: &Channel,
        content: &str,
        summary: &str,
    ) -> NotifyResult<()> {
        self.sender().send_one(ch, content, summary).await
    }

    /// 取一个只持有 HTTP 客户端的轻量发送器（可自由跨 `await` / 移进异步块）。
    fn sender(&self) -> ChannelSender {
        ChannelSender {
            http: self.http.clone(),
        }
    }

    /// 向所有启用渠道**并发**发送。
    ///
    /// - 每个渠道独立成败，互不影响
    /// - 只要有一个送达就返回 `sent > 0`
    /// - 全部失败时返回 [`NotifyError::AllFailed`]，调用方**不得**写去重标记
    /// - `channels` 为空且开了兜底 -> 走老的单 WxPusher 路径
    pub async fn send_all(
        &self,
        channels: &[Channel],
        content: &str,
        summary: &str,
    ) -> NotifyResult<SendReport> {
        let enabled: Vec<&Channel> = channels.iter().filter(|c| c.enabled).collect();

        if enabled.is_empty() {
            if !self.legacy_fallback {
                return Err(NotifyError::NoChannel(
                    "没有启用的推送渠道，且未开启环境变量兜底".into(),
                ));
            }
            // 回退到老行为：读环境变量的 WxPusher
            let cfg = ChannelConfig {
                app_token: None,
                topic_ids: Some(serde_json::json!(self.legacy_topic_ids)),
                ..Default::default()
            };
            send_wxpusher(&self.http, &cfg, content, summary).await?;
            return Ok(SendReport {
                sent: 1,
                failed: 0,
                details: vec![SendOutcome {
                    name: "WxPusher(环境变量)".into(),
                    kind: ChannelKind::Wxpusher,
                    ok: true,
                    error: None,
                }],
                legacy: true,
            });
        }

        // 并发发送。渠道数量一般是个位数，`join_all` 就够了 ——
        // 加信号量限流反而让总耗时退化回串行，没必要。
        // `ChannelSender` 只持有 client（内部是 Arc），可自由移进异步块。
        let mut tasks = Vec::with_capacity(enabled.len());
        for ch in &enabled {
            let ch = (*ch).clone();
            let content = content.to_string();
            let summary = summary.to_string();
            let sender = self.sender();
            tasks.push(async move {
                match sender.send_one(&ch, &content, &summary).await {
                    Ok(()) => SendOutcome {
                        name: ch.name.clone(),
                        kind: ch.kind,
                        ok: true,
                        error: None,
                    },
                    Err(e) => SendOutcome {
                        name: ch.name.clone(),
                        kind: ch.kind,
                        ok: false,
                        error: Some(e.to_string()),
                    },
                }
            });
        }
        let details: Vec<SendOutcome> = futures::future::join_all(tasks).await;

        let sent = details.iter().filter(|d| d.ok).count();
        let failed = details.len() - sent;

        for d in details.iter().filter(|d| !d.ok) {
            tracing::warn!(
                "渠道 {} 推送失败: {}",
                d.name,
                d.error.as_deref().unwrap_or("未知错误")
            );
        }

        if sent == 0 {
            return Err(NotifyError::AllFailed(
                details
                    .iter()
                    .map(|d| format!("{}: {}", d.name, d.error.as_deref().unwrap_or("未知错误")))
                    .collect::<Vec<_>>()
                    .join("；"),
            ));
        }

        Ok(SendReport {
            sent,
            failed,
            details,
            legacy: false,
        })
    }

    /// 发一条测试消息。
    pub async fn test_channel(&self, ch: &Channel) -> NotifyResult<()> {
        let stamp = alpha_core::time::now_str();
        let content = format!(
            "【Alpha 面板 · 测试推送】\n时间：{stamp}\n渠道：{}\n\n如果你收到这条消息，说明该渠道配置正确。",
            ch.name
        );
        self.send_one(ch, &content, "面板测试推送").await
    }
}

// ---------------------------------------------------------------- 工具

/// 把 `content` / `summary` 转义为「合法的 JSON 字符串片段」（去掉首尾引号）。
fn render_template(tpl: &str, content: &str, summary: &str) -> String {
    let safe_content = json_string_inner(content);
    let safe_summary = json_string_inner(summary);
    tpl.replace("{content}", &safe_content)
        .replace("{summary}", &safe_summary)
}

fn json_string_inner(s: &str) -> String {
    let encoded = serde_json::to_string(s).unwrap_or_else(|_| "\"\"".to_string());
    // 去掉首尾引号，得到可直接嵌进 JSON 字符串字面量的片段
    encoded[1..encoded.len().saturating_sub(1)].to_string()
}

fn parse_topic_ids(v: Option<&Value>) -> Vec<i64> {
    let Some(v) = v else {
        return Vec::new();
    };
    match v {
        Value::Array(arr) => arr.iter().filter_map(as_i64_loose).collect(),
        Value::Number(n) => n.as_i64().into_iter().collect(),
        Value::String(s) => s
            .replace('，', ",")
            .split(',')
            .filter_map(|p| p.trim().parse::<i64>().ok())
            .collect(),
        _ => Vec::new(),
    }
}

fn as_i64_loose(v: &Value) -> Option<i64> {
    match v {
        Value::Number(n) => n.as_i64(),
        Value::String(s) => s.trim().parse::<i64>().ok(),
        _ => None,
    }
}

fn value_to_string(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

fn truncate(s: &str, n: usize) -> String {
    let t: String = s.chars().take(n).collect();
    if s.chars().count() > n {
        format!("{t}…")
    } else {
        t
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- 渠道表单字段元数据（面板「分发渠道」页靠它渲染） ----

    /// 三种类型都要有表单字段 —— 少一种，前端就渲染出一个空表单。
    #[test]
    fn every_channel_kind_has_form_fields() {
        for k in ChannelKind::all() {
            assert!(
                !k.fields().is_empty(),
                "{} 没有表单字段定义",
                k.as_str()
            );
        }
    }

    /// 字段 `key` 必须落在 [`ChannelConfig`] 真正有的字段上。
    ///
    /// 这是防「元数据和结构各改一半」的闸：前端会把表单值按 `key`
    /// 拼成 config 提交，而 `ChannelConfig` 是 `serde` 逐字段定义的，
    /// 键对不上就会被静默丢掉 —— 用户看到「保存成功」但配置没生效。
    #[test]
    fn field_keys_are_real_channel_config_fields() {
        const KNOWN: &[&str] = &[
            "app_token",
            "topic_ids",
            "url",
            "template",
            "headers",
            "send_key",
        ];
        for k in ChannelKind::all() {
            for f in k.fields() {
                assert!(
                    KNOWN.contains(&f.key),
                    "{} 的字段 {} 不在 ChannelConfig 里",
                    k.as_str(),
                    f.key
                );
            }
        }
    }

    #[test]
    fn password_fields_are_the_secret_ones() {
        // 面板靠这个判定哪些字段要脱敏（回传 `••••••xxxx`）
        let secret: Vec<&str> = ChannelKind::all()
            .iter()
            .flat_map(|k| k.fields())
            .filter(|f| f.kind == FieldKind::Password)
            .map(|f| f.key)
            .collect();
        assert!(secret.contains(&"app_token"));
        assert!(secret.contains(&"send_key"));
    }

    /// 字段元数据要能序列化成前端直接吃的形状（`type` 而不是 `kind`）。
    #[test]
    fn field_serializes_with_type_key() {
        let v = serde_json::to_value(&ChannelKind::Webhook.fields()[0]).unwrap();
        assert_eq!(v["key"], "url");
        assert_eq!(v["label"], "Webhook 地址");
        assert_eq!(v["type"], "text");
        assert!(v.get("kind").is_none(), "对外必须是 type 而不是 kind: {v}");
    }

    #[test]
    fn channel_kind_serializes_as_lowercase() {
        assert_eq!(
            serde_json::to_value(ChannelKind::Serverchan).unwrap(),
            Value::String("serverchan".into())
        );
    }

    /// `types_table()` 的形状必须与原版 `CHANNEL_TYPES` 对得上 ——
    /// 前端就是照着原版那个形状解析的，键名差一个字母表单就渲染不出来。
    #[test]
    fn types_table_matches_the_original_shape() {
        let t = ChannelKind::types_table();

        // 顶层是三个键，分别对应三种渠道
        assert_eq!(
            t.as_object().unwrap().len(),
            3,
            "该正好三种渠道类型: {t}"
        );
        for k in ChannelKind::all() {
            let entry = &t[k.as_str()];
            assert!(entry.is_object(), "{} 该有元信息: {t}", k.as_str());
            assert_eq!(entry["label"], k.label());
            assert_eq!(entry["desc"], k.desc());
            assert!(
                entry["fields"].is_array(),
                "{} 的 fields 该是数组: {entry}",
                k.as_str()
            );
        }

        // 逐字核对一个字段，防止「结构对但内容串了」
        let f = &t["serverchan"]["fields"][0];
        assert_eq!(f["key"], "send_key");
        assert_eq!(f["label"], "SendKey");
        assert_eq!(f["type"], "password");
        assert_eq!(f["placeholder"], "SCTxxxx");

        // 键必须是渠道类型字符串本身，不是下标
        assert!(t.get("0").is_none(), "顶层该是对象不是数组: {t}");
    }

    #[test]
    fn template_escapes_quotes_and_newlines() {
        // 正文里有英文双引号、反斜杠、换行 —— 替换后整体仍须是合法 JSON
        let content = "技能: 十万伏特\n报点人: \"lmxm\" \\ 备注";
        let tpl = r#"{"msgtype":"text","text":{"content":"{content}"}}"#;
        let rendered = render_template(tpl, content, "早头");
        let parsed: Value = serde_json::from_str(&rendered).expect("渲染结果应为合法 JSON");
        assert_eq!(
            parsed["text"]["content"].as_str().unwrap(),
            content,
            "转义后应能还原出原文"
        );
    }

    #[test]
    fn template_replaces_both_placeholders() {
        let rendered = render_template("{summary}|{content}", "正文", "午头");
        assert_eq!(rendered, "午头|正文");
    }

    #[test]
    fn topic_ids_accepts_list_string_and_number() {
        assert_eq!(parse_topic_ids(Some(&serde_json::json!([45385, 123]))), vec![45385, 123]);
        assert_eq!(parse_topic_ids(Some(&serde_json::json!("45385, 123"))), vec![45385, 123]);
        assert_eq!(parse_topic_ids(Some(&serde_json::json!("45385，123"))), vec![45385, 123]);
        assert_eq!(parse_topic_ids(Some(&serde_json::json!(45385))), vec![45385]);
        assert_eq!(parse_topic_ids(None), Vec::<i64>::new());
        // 脏数据不应 panic，直接跳过
        assert_eq!(parse_topic_ids(Some(&serde_json::json!("abc, 12"))), vec![12]);
    }

    #[test]
    fn channel_kind_roundtrip() {
        for k in ChannelKind::all() {
            assert_eq!(ChannelKind::parse(k.as_str()), Some(*k));
        }
        assert_eq!(ChannelKind::parse("nope"), None);
    }

    #[test]
    fn channel_deserializes_from_panel_json() {
        // 面板 API 传来的形状（与 SQLite 里存的 config JSON 一致）
        let raw = r#"{
            "id": 1,
            "name": "QQ 群 857597325",
            "type": "webhook",
            "enabled": true,
            "config": {
                "url": "http://127.0.0.1:18080/notify",
                "template": "{\"content\":\"{content}\"}",
                "headers": {"x-token": "abc"}
            }
        }"#;
        let ch: Channel = serde_json::from_str(raw).unwrap();
        assert_eq!(ch.kind, ChannelKind::Webhook);
        assert!(ch.enabled);
        assert_eq!(ch.config.url.as_deref(), Some("http://127.0.0.1:18080/notify"));
    }

    #[test]
    fn json_string_inner_matches_python_dumps() {
        // 原版是 json.dumps(x)[1:-1]，这里逐字对齐几个关键输入
        assert_eq!(json_string_inner("abc"), "abc");
        assert_eq!(json_string_inner("a\"b"), "a\\\"b");
        assert_eq!(json_string_inner("a\nb"), "a\\nb");
        assert_eq!(json_string_inner("a\\b"), "a\\\\b");
        // 中文不转义（serde_json 默认与 Python ensure_ascii=True 不同，
        // 但两者都是合法 JSON，下游能正常解析 —— 这里记录该差异）
        assert_eq!(json_string_inner("早头"), "早头");
    }
}
