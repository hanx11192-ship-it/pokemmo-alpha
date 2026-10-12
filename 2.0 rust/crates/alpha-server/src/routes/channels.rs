//! 分发渠道 6 个接口。
//!
//! 对应原版 `panel/app.py` 的 `api_channels` / `api_channel_add` /
//! `api_channel_edit` / `api_channel_enable` / `api_channel_delete` /
//! `api_channel_test` / `api_channel_test_all`。
//!
//! # 本模块最要紧的一件事：脱敏
//!
//! `GET /api/channels` 回给前端的 config 里，`app_token` / `send_key`
//! **不能是明文** —— 面板是给多人用的，回显 token 等于把它广播出去。
//!
//! 原版的做法是「把每个 token 换成 `••••••` + 末四位」：
//!
//! ```python
//! if k in ("app_token", "send_key") and v:
//!     safe[k] = "••••••" + str(v)[-4:]
//! ```
//!
//! 但这就带来一个配套问题：**前端把表单原样提交回来时，
//! 那个 `••••••xxxx` 也会被当成新值写进库**，真 token 就被覆盖了。
//! 原版的对策是在 edit 里把这个前缀过滤掉：
//!
//! ```python
//! cfg = {k: v for k, v in cfg.items()
//!        if not (isinstance(v, str) and v.startswith("••••••"))}
//! ```
//!
//! 这套「发出去是掩码、收回来要认出来」的约定很脆 —— 它靠的是
//! 前端不会手打一个以 `••••••` 开头的真 token。本版**保留原版行为**
//! （改了会破坏前端），但把它集中到两个函数里，
//! 并且加了测试钉死不变量。
//!
//! # 与服务端推送路径的差异
//!
//! [`alpha_notify::Dispatcher::send_all`] 目前不写 store 日志（只
//! `tracing::warn!`），而原版 `channel_mod.send_all` 会**逐渠道**
//! 写一条 `info` / `error`。这里在 [`test_all`] 里补上了 ——
//! 那是唯一一处面板直接调 `send_all` 的地方。
//! 调度器那侧的日志另行处理。

use axum::extract::{Path as AxumPath, State};
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};

use alpha_notify::{ChannelConfig, ChannelKind};
use alpha_store::LogLevel;

use crate::error::{ApiError, ApiResult};
use crate::AppState;

/// 脱敏后回传的前缀。**必须**与原版逐字一致 ——
/// 前端在编辑表单里靠它判断「这个值不要动」。
const MASK_PREFIX: &str = "••••••";

/// 需要脱敏的配置键。
///
/// 与 [`alpha_notify::ChannelField`] 里 `type == password` 的那些字段
/// 是同一批（有个测试钉这个一致性）。这里之所以再列一遍，
/// 是因为脱敏要处理的是**已经存进库的 config**，
/// 而 `fields()` 描述的是**表单**，两者数据来源不同。
const SECRET_KEYS: &[&str] = &["app_token", "send_key"];

/// 把 config 里所有密钥换成 `••••••` + 末四位。
///
/// 空值原样保留 —— 原版的条件是 `k in (...) and v`，
/// 空串 / `None` 不该变成 `••••••`（那会让前端以为「有值」）。
fn mask_config(cfg: &Value) -> Value {
    let Some(map) = cfg.as_object() else {
        return cfg.clone();
    };
    let mut out = serde_json::Map::new();
    for (k, v) in map {
        let masked = if SECRET_KEYS.contains(&k.as_str()) && is_non_empty(v) {
            let s = match v {
                Value::String(s) => s.clone(),
                // 理论上密钥都是字符串，但 config 是 JSON 列 ——
                // 手工改过库的场景下可能是数字。`str(v)` 的等价物。
                other => other.to_string(),
            };
            // 末四位。Python 的 `v[-4:]` 对短于 4 的字符串返回整串，
            // Rust 的 `chars().rev().take(4)` 也是 —— 语义一致。
            //
            // 用 `chars()` 而不是字节切片：密钥可能是用户粘贴的
            // 任意文本，按字节切会在多字节字符中间断开、直接 panic。
            let tail: String = s.chars().rev().take(4).collect::<Vec<_>>()
                .into_iter()
                .rev()
                .collect();
            Value::String(format!("{MASK_PREFIX}{tail}"))
        } else {
            v.clone()
        };
        out.insert(k.clone(), masked);
    }
    Value::Object(out)
}

/// 原版 `and v` 的等价物 —— Python 里空串、`None`、`0`、`{}` 都是假值。
fn is_non_empty(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::String(s) => !s.is_empty(),
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().is_some_and(|f| f != 0.0),
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
    }
}

/// 从「前端回传的 config」里摘掉脱敏占位值。
///
/// 返回的 `Value` 只含**真正要改**的键；调用方据此决定
/// 「整个 config 不动」还是「合并这几个键」。
fn strip_masked(cfg: &Value) -> Value {
    let Some(map) = cfg.as_object() else {
        return cfg.clone();
    };
    let mut out = serde_json::Map::new();
    for (k, v) in map {
        let is_mask = v
            .as_str()
            .is_some_and(|s| s.starts_with(MASK_PREFIX));
        if !is_mask {
            out.insert(k.clone(), v.clone());
        }
    }
    Value::Object(out)
}

// ---------------------------------------------------------------- 各接口

/// `GET /api/channels`
///
/// 回 `{channels: [...], types: {...}}`。`types` 就是原版的
/// `CHANNEL_TYPES`，前端据此渲染表单。
pub async fn list(
    State(state): State<AppState>) -> ApiResult<Json<Value>> {
    let channels = state.store.list_channels()?;

    let rows: Vec<Value> = channels
        .iter()
        .map(|c| {
            json!({
                "id": c.id,
                "name": c.name,
                "type": c.kind,
                "enabled": c.enabled,
                // config 先序列化成 JSON 再脱敏 —— 脱敏逻辑按「键名」
                // 判断，对 `ChannelConfig` 的结构一无所知。
                "config": mask_config(&serde_json::to_value(&c.config).unwrap_or(Value::Null)),
            })
        })
        .collect();

    Ok(Json(json!({
        "channels": rows,
        "types": ChannelKind::types_table(),
    })))
}

#[derive(Debug, Deserialize)]
pub struct AddRequest {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(rename = "type", default)]
    pub kind: Option<String>,
    #[serde(default)]
    pub config: Option<Value>,
    #[serde(default)]
    pub enabled: Option<bool>,
}

/// `POST /api/channels`
pub async fn add(
    State(state): State<AppState>, Json(body): Json<AddRequest>) -> ApiResult<Json<Value>> {
    let name = body.name.unwrap_or_default().trim().to_string();
    if name.is_empty() {
        return Err(ApiError::BadRequest("名称必填".into()));
    }

    let kind_str = body.kind.unwrap_or_default().trim().to_string();
    let Some(kind) = ChannelKind::parse(&kind_str) else {
        return Err(ApiError::BadRequest("未知渠道类型".into()));
    };

    let cfg = config_from_value(body.config.as_ref())?;
    // 原版 `bool(data.get("enabled", True))` —— 缺省是启用。
    let enabled = body.enabled.unwrap_or(true);

    let id = state.store.add_channel(&name, kind, &cfg, enabled)?;
    state.store.log(
        LogLevel::Info,
        "channel",
        &format!("新增分发渠道：{name}（{kind_str}）"),
        "panel",
    );

    Ok(Json(json!({ "ok": true, "id": id })))
}

#[derive(Debug, Deserialize)]
pub struct EditRequest {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub config: Option<Value>,
    #[serde(default)]
    pub enabled: Option<bool>,
}

/// `POST /api/channels/<cid>/edit`
///
/// # 空串与「没提交」的区别
///
/// 原版用 `data.get("name")`（缺失 → `None`）与 `UPDATE ... SET name=?`
/// 的 `COALESCE` 组合，把「没提交」与「提交了空串」当成同一件事 ——
/// 两者都不改名。本版沿用：`name` 为 `None` 或纯空白时保持原名。
pub async fn edit(
    State(state): State<AppState>,
    AxumPath(cid): AxumPath<i64>,
    Json(body): Json<EditRequest>,
) -> ApiResult<Json<Value>> {
    let Some(ch) = state.store.get_channel(cid)? else {
        return Err(ApiError::NotFound("渠道不存在".into()));
    };

    let name = body
        .name
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());

    // 只有前端提交了 config（且是对象）才动 config。
    let cfg = match &body.config {
        Some(v) if v.is_object() => {
            let stripped = strip_masked(v);
            // 全部都是占位值 → 等价于「没提交 config」，保持原值。
            // 原版这里会传一个空 dict 进去，`update_channel` 的浅合并
            // 对空 dict 是空操作，所以行为一致；这里显式跳过更清楚。
            if stripped.as_object().is_some_and(|m| m.is_empty()) {
                None
            } else {
                Some(config_from_value(Some(&stripped))?)
            }
        }
        _ => None,
    };

    state
        .store
        .update_channel(cid, name, None, cfg.as_ref(), body.enabled)?;

    state.store.log(
        LogLevel::Info,
        "channel",
        &format!("编辑分发渠道：{}", ch.name),
        "panel",
    );

    Ok(Json(json!({ "ok": true })))
}

/// `POST /api/channels/<cid>/enable`
///
/// 原版**不检查渠道是否存在**：`update_channel` 对不存在的 id 返回 0 行，
/// 但接口照样回 `{ok: true}`。本版保留这个行为。
pub async fn set_enabled(
    State(state): State<AppState>,
    AxumPath(cid): AxumPath<i64>,
    Json(body): Json<EnableRequest>,
) -> ApiResult<Json<Value>> {
    let enabled = body.enabled.unwrap_or(true);
    state.store.update_channel(cid, None, None, None, Some(enabled))?;
    Ok(Json(json!({ "ok": true })))
}

#[derive(Debug, Deserialize)]
pub struct EnableRequest {
    #[serde(default)]
    pub enabled: Option<bool>,
}

/// `DELETE /api/channels/<cid>`
pub async fn delete(
    State(state): State<AppState>, AxumPath(cid): AxumPath<i64>) -> ApiResult<Json<Value>> {
    // 先取名字 —— 删掉之后日志里还想写它叫什么。
    let ch = state.store.get_channel(cid)?;

    if !state.store.delete_channel(cid)? {
        return Err(ApiError::NotFound("渠道不存在".into()));
    }

    state.store.log(
        LogLevel::Info,
        "channel",
        &format!(
            "删除分发渠道：{}",
            ch.map(|c| c.name).unwrap_or_else(|| cid.to_string())
        ),
        "panel",
    );

    Ok(Json(json!({ "ok": true })))
}

// ---------------------------------------------------------------- 测试推送

/// `POST /api/channels/<cid>/test`
///
/// 给单个渠道发一条测试消息。**无论成败都回 200** ——
/// 结果在 body 的 `ok` 字段里。这与「接口本身失败了」是两回事：
/// 渠道配错了不是 HTTP 错误，面板要把 `error` 原文显示出来。
pub async fn test_one(
    State(state): State<AppState>,
    AxumPath(cid): AxumPath<i64>,
) -> ApiResult<Json<Value>> {
    let Some(ch) = state.store.get_channel(cid)? else {
        return Err(ApiError::NotFound("渠道不存在".into()));
    };

    // `ChannelSender` 没有公开构造器（它由 `Dispatcher` 内部持有）。
    // 测试推送直接用 `Dispatcher::send_one`，内容走 `send_one` 的
    // summary 参数 —— 与原版 `test_channel` 生成的正文一致即可。
    let stamp = alpha_core::time::now_str();
    let content = format!(
        "【Alpha 面板 · 测试推送】\n时间：{stamp}\n渠道：{}\n\n如果你收到这条消息，说明该渠道配置正确。",
        ch.name
    );
    let dispatcher = alpha_notify::Dispatcher::new(false, Vec::new())
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    let outcome = dispatcher.send_one(&ch, &content, "面板测试推送").await;

    let (ok, error) = match &outcome {
        Ok(()) => (true, None),
        Err(e) => (false, Some(e.to_string())),
    };

    // 日志的 `source` 用渠道类型而不是 "panel" ——
    // 这样按来源筛日志时能把「某个渠道一直发不出去」一眼看出来。
    let msg = format!(
        "测试推送：{} - {}",
        ch.name,
        if ok {
            "成功".to_string()
        } else {
            error.clone().unwrap_or_default()
        }
    );
    state.store.log(
        if ok { LogLevel::Info } else { LogLevel::Error },
        "channel",
        &msg,
        ch.kind.as_str(),
    );

    let mut body = json!({ "ok": ok });
    if let Some(e) = error {
        body["error"] = Value::String(e);
    }
    Ok(Json(body))
}

/// `POST /api/channels/test-all`
///
/// 给所有**启用**的渠道各发一条。
///
/// # 与单渠道测试的差异
///
/// 单渠道测试回 `{ok, error}`；这个回 `{ok: true, sent, failed, details}`，
/// 外层 `ok` 恒为 `true` —— 它表达的是「请求被受理了」，
/// 而不是「都发出去了」。原版如此，前端也是这么读的。
pub async fn test_all(
    State(state): State<AppState>, Json(body): Json<TestAllRequest>) -> ApiResult<Json<Value>> {
    let text = body
        .content
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| {
            "【Alpha 面板 · 测试推送】\n如果你收到这条消息，说明渠道配置正确。".to_string()
        });

    let channels: Vec<_> = state
        .store
        .list_channels()?
        .into_iter()
        .filter(|c| c.enabled)
        .collect();

    // 原版的 `send_all` 逐渠道写日志；Rust 版的 `Dispatcher::send_all`
    // 只 `tracing::warn!`。这里补上 store 日志 —— 面板的「日志」页
    // 是用户唯一的排查入口，缺了它就只能去翻 stderr。
    let dispatcher = alpha_notify::Dispatcher::new(false, Vec::new())
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    let report = dispatcher.send_all(&channels, &text, "面板测试推送").await;

    let report = match report {
        Ok(r) => r,
        Err(e) => {
            // 全部失败时 `send_all` 会返回 `Err(AllFailed)` ——
            // 但每个渠道的失败原因在它内部已经写进日志/丢弃了。
            // 这里不能再拿到 per-channel 细节，只能报一个总失败。
            state.store.log(
                LogLevel::Error,
                "channel",
                &format!("测试推送（全部）：{e}"),
                "panel",
            );
            return Ok(Json(json!({
                "ok": true,
                "sent": 0,
                "failed": channels.len(),
                "details": [],
                "error": e.to_string(),
                "legacy": false,
            })));
        }
    };

    for d in &report.details {
        state.store.log(
            if d.ok { LogLevel::Info } else { LogLevel::Error },
            "channel",
            &format!(
                "测试推送：{} - {}",
                d.name,
                if d.ok {
                    "成功".to_string()
                } else {
                    d.error.clone().unwrap_or_default()
                }
            ),
            d.kind.as_str(),
        );
    }

    Ok(Json(json!({
        "ok": true,
        "sent": report.sent,
        "failed": report.failed,
        "details": report.details,
        "legacy": report.legacy,
    })))
}

#[derive(Debug, Deserialize)]
pub struct TestAllRequest {
    #[serde(default)]
    pub content: Option<String>,
}

/// 把前端提交的 config 对象转成 [`ChannelConfig`]。
///
/// 用 `serde_json::from_value` 而不是手工逐字段搬 ——
/// 多余键会被 `serde` 忽略（`ChannelConfig` 没有 `deny_unknown_fields`），
/// 少键用 `#[serde(default)]` 补上，与 Python 的宽容行为一致。
fn config_from_value(v: Option<&Value>) -> ApiResult<ChannelConfig> {
    match v {
        None | Some(Value::Null) => Ok(ChannelConfig::default()),
        Some(v) => serde_json::from_value(v.clone())
            .map_err(|e| ApiError::BadRequest(format!("渠道配置格式错误: {e}"))),
    }
}

/// 渠道配置里「哪些键是密钥」—— 供测试与前端文档用。
pub fn secret_keys() -> &'static [&'static str] {
    SECRET_KEYS
}

/// 供 `alpha-notify` 侧一致性测试引用（见 `channels.rs`）。
pub fn mask_prefix() -> &'static str {
    MASK_PREFIX
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mask_replaces_secrets_with_tail_four() {
        let cfg = json!({"app_token": "AT_abcdefgh1234", "topic_ids": "45385"});
        let m = mask_config(&cfg);
        assert_eq!(m["app_token"], "••••••1234");
        // 非密钥字段原样保留
        assert_eq!(m["topic_ids"], "45385");
    }

    #[test]
    fn mask_handles_send_key_too() {
        let m = mask_config(&json!({"send_key": "SCTxxxxYYYY"}));
        assert_eq!(m["send_key"], "••••••YYYY");
    }

    #[test]
    fn mask_leaves_empty_secret_alone() {
        // 空值不能被脱敏成 `••••••` —— 那会让前端以为「这里有值」，
        // 用户看到一串点以为是已配置好的，实际根本没有。
        // 判据是「掩码前缀不在结果里」而不是「值等于什么」：
        // `""` 与 `null` 都该原样返回，各保持各的类型。
        for v in [json!(""), json!(null)] {
            let m = mask_config(&json!({"app_token": v.clone()}));
            assert_eq!(m["app_token"], v, "空值该原样返回（含类型）: {m}");
        }
    }

    #[test]
    fn mask_does_not_panic_on_short_or_multibyte_values() {
        // 短于四个字符
        assert_eq!(
            mask_config(&json!({"app_token": "ab"}))["app_token"],
            "••••••ab"
        );
        // 多字节字符 —— 按字节切会 panic，所以必须按 char 切
        assert_eq!(
            mask_config(&json!({"app_token": "密钥一二三四"}))["app_token"],
            "••••••一二三四",
            "「密钥一二三四」有 6 个字符，取末四位是「一二三四」"
        );
        assert_eq!(
            mask_config(&json!({"app_token": "abc密钥"}))["app_token"],
            "••••••bc密钥",
            "「abc密钥」是 5 个字符，末四位是 b、c、密、钥"
        );
    }

    #[test]
    fn strip_masked_drops_placeholder_values() {
        let incoming = json!({
            "app_token": "••••••1234",
            "url": "https://new.test/hook",
        });
        let s = strip_masked(&incoming);
        assert!(
            s.get("app_token").is_none(),
            "占位值该被丢掉，否则真 token 会被覆盖"
        );
        assert_eq!(s["url"], "https://new.test/hook");
    }

    #[test]
    fn strip_masked_keeps_real_values() {
        let incoming = json!({"app_token": "AT_brand_new_token"});
        assert_eq!(
            strip_masked(&incoming)["app_token"],
            "AT_brand_new_token",
            "真 token 必须留下"
        );
    }

    /// 脱敏 → 摘除 的往返：把脱敏结果原样提交回来，不该改变任何东西。
    /// 这是前端编辑表单最常见的路径（用户只改了别的字段就保存）。
    #[test]
    fn mask_then_strip_is_a_no_op_for_secrets() {
        let original = json!({"app_token": "AT_real_token_9999", "topic_ids": "1"});
        let round_tripped = strip_masked(&mask_config(&original));
        assert!(
            round_tripped.get("app_token").is_none(),
            "回传的掩码值不该被当成新值: {round_tripped}"
        );
        assert_eq!(round_tripped["topic_ids"], "1");
    }

    #[test]
    fn secret_keys_match_the_password_form_fields() {
        // 这个不变量很容易被破坏：给某种渠道加一个 password 字段，
        // 却忘了把它加进 SECRET_KEYS，于是新密钥会以明文回传。
        let form_secrets: Vec<&str> = ChannelKind::all()
            .iter()
            .flat_map(|k| k.fields())
            .filter(|f| serde_json::to_value(f).unwrap()["type"] == "password")
            .map(|f| f.key)
            .collect();
        for key in &form_secrets {
            assert!(
                SECRET_KEYS.contains(key),
                "表单里的密码字段 {key} 没被列入脱敏名单"
            );
        }
        for key in SECRET_KEYS {
            assert!(
                form_secrets.contains(key),
                "脱敏名单里的 {key} 在表单里不是密码字段"
            );
        }
    }

    #[test]
    fn mask_prefix_matches_the_original() {
        // 前端硬编码了这六个点做判断，改一个字符前端就失效
        assert_eq!(MASK_PREFIX, "••••••");
        assert_eq!(MASK_PREFIX.chars().count(), 6, "是六个 U+2022，不是五个点");
    }

    #[test]
    fn masking_a_non_object_is_a_no_op() {
        for v in [json!(null), json!("str"), json!([1, 2])] {
            assert_eq!(mask_config(&v), v);
            assert_eq!(strip_masked(&v), v);
        }
    }
}
