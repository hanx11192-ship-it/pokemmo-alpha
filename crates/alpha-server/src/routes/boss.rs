//! 头目报点 2 个接口。
//!
//! 对应原版 `panel/app.py` 的 `api_boss_reports` / `api_boss_dispatch`
//! 以及它们的两个 helper `_get_boss_source` / `_boss_to_dict`。
//!
//! # 这两个接口是「调试台」
//!
//! 面板上「头目报点」那一页做两件事：
//!
//! 1. 实时拉一次 LZPoke 报点，把**原始 JSON** 与**归一化后的头目**
//!    并排显示 —— 用户能一眼看出「源给的字段」与「我们解析成的样子」
//!    差在哪，这是排查解析问题最快的路径
//! 2. 挑一条报点、跑一遍决策器，看播报长什么样
//!
//! 所以 `reports` 里每条都要带 `raw`（原始报文），不能只回归一化结果。
//!
//! # 与调度器链路的区别
//!
//! 调度器是「多源 → 投票 → 去重 → 决策 → 分发」的完整链路；
//! 这里是**单源直连**：直接从 LZPoke 拉、直接跑决策器、不投票不去重。
//! 目的是让用户能对着某一条具体报点调决策器，不受其它源的干扰。
//!
//! # `_get_boss_source` 的等价物
//!
//! 原版优先从 `sources.yaml` 里找一个 `adapter == "lzpoke_reports"` 的源、
//! 用它的 options；找不到就回退到内置默认配置。
//! **这个回退很重要**：面板的新用户还没配任何源时，
//! 「头目报点」页仍然要能打开，而不是报「没有数据源」。

use axum::extract::State;
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};

use alpha_core::config_mgr;
use alpha_core::pokedex::get_pokedex;
use alpha_sources::lzpoke_reports::LzpokeReportsSource;
use alpha_core::config::SourceOptions;
use alpha_store::LogLevel;

use crate::error::{ApiError, ApiResult};
use crate::AppState;

/// 找不到已配置的源时用的兜底地址。
///
/// 与原版 `_get_boss_source` 里的字面量逐字一致。
const FALLBACK_TARGET: &str = "https://tool.lzpoke.com/api/reports?type=alpha";

/// 从 `sources.yaml` 里找 `lzpoke_reports` 源的 options；
/// 找不到就回退到内置默认。
///
/// 注意原版只挑了「第一个 adapter 匹配的源」，**不看 `enabled`** ——
/// 一个被停用的源照样会被用上。这是刻意的：调试台不该因为
/// 用户临时关掉了那个源就变得不可用。本版保留。
fn resolve_options() -> Value {
    // `load_sources` 给的是 `serde_yaml::Value`，转成 JSON 再取 ——
    // 两边都用 `Option` 链取值，避免 `as_str()` 在 YAML 的
    // `Value::String` 上返回 None（YAML 与 JSON 是两套类型）。
    let sources = config_mgr::load_sources().unwrap_or_default();
    for s in sources {
        let adapter = s.get("adapter").and_then(|v| v.as_str());
        if adapter == Some(alpha_sources::lzpoke_reports::NAME) {
            if let Some(opts) = s.get("options") {
                return serde_json::to_value(opts).unwrap_or_else(|_| json!({}));
            }
        }
    }
    json!({ "target": FALLBACK_TARGET })
}

/// 构造一个可用的 LZPoke 源实例。
///
/// `cfg` 是 `alpha_core` 那份**配置内容**（settings + sources + rules），
/// 不是 `alpha_server::Config`（监听地址、密钥那些）。
/// 别混：同名不同物，编译器会拦，但读代码时容易看错。
fn build_source(cfg: &alpha_core::config::Config) -> ApiResult<LzpokeReportsSource> {
    let options: SourceOptions =
        serde_json::from_value(resolve_options()).unwrap_or_default();
    LzpokeReportsSource::new(options, cfg)
        .map_err(|e| ApiError::BadGateway(format!("构造数据源失败: {e}")))
}

/// 取核心配置（`settings.yaml` + `sources.yaml` + `rules.yaml`）。
///
/// 走 `get_config()` 那份进程级缓存 —— 系统配置页保存时会
/// `refresh_config()` 让它失效，所以拿到的一定是最新的。
fn core_config() -> ApiResult<std::sync::Arc<alpha_core::config::Config>> {
    alpha_core::config::get_config()
        .map_err(|e| ApiError::Internal(format!("加载核心配置失败: {e}")))
}

/// `GET /api/boss/reports`
///
/// 回 `{ok, source, type, reports, message}`。
///
/// 拉取失败回 **502**（原版如此）—— 它表达的是「上游坏了」，
/// 不是「我们的请求写错了」。前端据此把错误显示成「数据源不可用」
/// 而不是「参数错误」。
pub async fn reports(State(state): State<AppState>) -> ApiResult<Json<Value>> {
    let src = build_source(core_config()?.as_ref())?;
    let pokedex = get_pokedex()
        .map_err(|e| ApiError::Internal(format!("加载图鉴失败: {e}")))?;

    let (ok, list, message) = src.fetch_reports(pokedex).await;

    if !ok {
        // 原版把上游错误包成 502，消息里带「拉取失败: 」
        return Err(ApiError::BadGateway(format!("拉取失败: {message}")));
    }

    let count = list.len();
    state.store.log(
        LogLevel::Info,
        "boss",
        &format!("拉取头目报点 {count} 条"),
        "panel",
    );

    Ok(Json(json!({
        "ok": true,
        "source": alpha_sources::lzpoke_reports::NAME,
        "type": "alpha",
        "reports": list,
        "message": message,
    })))
}

#[derive(Debug, Deserialize)]
pub struct DispatchRequest {
    /// 按报点的原始 id 选。与 `index` 二选一。
    #[serde(default)]
    pub report_id: Option<String>,
    /// 按 `/api/boss/reports` 返回数组的下标选。
    #[serde(default)]
    pub index: Option<i64>,
}

/// `POST /api/boss/dispatch`
///
/// 对某一条报点跑当前生效的决策器，回播报文本。
pub async fn dispatch(
    State(state): State<AppState>,
    Json(body): Json<DispatchRequest>,
) -> ApiResult<Json<Value>> {
    let src = build_source(core_config()?.as_ref())?;
    let pokedex = get_pokedex()
        .map_err(|e| ApiError::Internal(format!("加载图鉴失败: {e}")))?;

    let (ok, list, message) = src.fetch_reports(pokedex).await;

    // 拉取失败时**不**报错，而是当成「没有报点」继续往下 ——
    // 原版就是 `reports = res.get("reports", []) if res.get("ok") else []`。
    // 结果是下面那个 404「当前没有可决策的报点」。
    // 这个行为有点糙（把上游故障说成「没有报点」），但保留：前端的
    // 处理路径依赖这个状态码。真要看原因得走 `/api/boss/reports`。
    let list = if ok { list } else { Vec::new() };

    if list.is_empty() {
        let _ = message;
        return Err(ApiError::NotFound("当前没有可决策的报点".into()));
    }

    // ---- 选哪一条 ----
    let chosen = pick(&list, body.report_id.as_deref(), body.index);

    let Some(chosen) = chosen else {
        // `index` 越界 / `report_id` 不存在时**回落到第一条**（原版行为）。
        // 注意 `None` 只会来自「list 非空但选不出来」，
        // 上面已经排除了 list 为空的情况。
        return Err(ApiError::NotFound("当前没有可决策的报点".into()));
    };

    // 报点必须能解析出头目 —— 解析失败的条目 `boss` 是 null
    let boss_value = chosen.get("boss").cloned().unwrap_or(Value::Null);
    if boss_value.is_null() {
        return Err(ApiError::Unprocessable("该报点无法解析为头目数据".into()));
    }
    let boss: alpha_core::models::BossData = serde_json::from_value(boss_value.clone())
        .map_err(|_| ApiError::Unprocessable("该报点无法解析为头目数据".into()))?;

    // ---- 找决策器 ----
    //
    // 原版的优先级：激活的 → 内建的。两处都是 `LIMIT 1`。
    // 这与「没有可用分发器」的 500 一起构成完整的回退链。
    let dispatcher = state
        .store
        .active_dispatcher_filename()?
        .or(state.store.builtin_dispatcher_filename()?);

    let Some(dispatcher) = dispatcher else {
        return Err(ApiError::Internal("没有可用分发器".into()));
    };

    // ---- 跑决策器 ----
    let registry = state
        .registry
        .as_ref()
        .ok_or_else(|| ApiError::Internal("插件系统未初始化".into()))?;

    let loaded = registry
        .load_by_filename(alpha_plugin::PluginKind::Dispatcher, &dispatcher)
        .map_err(|e| ApiError::Internal(format!("加载决策器失败: {e}")))?
        .ok_or_else(|| ApiError::Internal(format!("决策器文件不存在: {dispatcher}")))?;
    let plugin = loaded
        .get()
        .map_err(|e| ApiError::Internal(format!("决策器编译失败: {e}")))?;

    // 推送语言 → 传给决策器的 langs 列表。
    // 原版：`{"zh": ["zh"], "en": ["en"], "both": ["zh","en"]}`。
    let push_lang = config_mgr::get_push_language(state.store.get_kv("push_lang")?.as_deref());
    let langs: Vec<String> = match push_lang.as_str() {
        "en" => vec!["en".into()],
        "both" => vec!["zh".into(), "en".into()],
        _ => vec!["zh".into()],
    };

    // `lang` 传给决策器的是**单语言**取值：`both` 时决策器以中文为基准，
    // 英文内容由 `langs` 里的第二个元素驱动。这与原版
    // `dispatch_mod.run_dispatcher(..., {"langs": langs})` 的语义一致 ——
    // 决策器自己看 `ctx.is_bilingual()` 决定要不要出双语。
    let lang = if push_lang == "en" { "en" } else { "zh" }.to_string();
    let ctx = alpha_plugin::Context::new(lang, langs)
        .with_repo_rules(alpha_core::config::project_root());

    let report_text = plugin
        .dispatch(&alpha_plugin::BossView::from_boss(&boss), &ctx)
        .map_err(|e| ApiError::Internal(format!("决策器执行失败: {e}")))?;

    Ok(Json(json!({
        "ok": true,
        "dispatcher": dispatcher,
        "report": report_text,
        "boss": boss_value,
        "raw": chosen.get("raw").cloned().unwrap_or(Value::Null),
    })))
}

/// 从候选里选一条报点。
///
/// `report_id` 优先于 `index`（原版是 `if report_id: ... elif index is not None:`）。
/// 两者都没命中时返回第一条。
fn pick(list: &[Value], report_id: Option<&str>, index: Option<i64>) -> Option<Value> {
    if let Some(id) = report_id.filter(|s| !s.is_empty()) {
        if let Some(r) = list
            .iter()
            .find(|r| r.get("raw").and_then(|raw| raw.get("id")).and_then(|v| v.as_str()) == Some(id))
        {
            return Some(r.clone());
        }
    } else if let Some(i) = index {
        // 负数下标在 Python 里是「从后往前数」，Rust 的 `get` 对 usize 会返回 None。
        // 原版负数会被 `report[-1]` 成功取到 —— 但那是**意外**而非设计
        // （前端永远传非负下标），所以这里用 `i < 0 → None`（回落到第一条），
        // 比复刻一个没人依赖的负索引语义更清楚。
        if let Some(r) = usize::try_from(i).ok().and_then(|i| list.get(i)) {
            return Some(r.clone());
        }
    }
    list.first().cloned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_reports() -> Vec<Value> {
        vec![
            json!({"raw": {"id": "r1", "monsterId": 6}, "boss": {"name": "喷火龙"}}),
            json!({"raw": {"id": "r2", "monsterId": 9}, "boss": {"name": "水箭龟"}}),
            json!({"raw": {"id": "r3", "monsterId": 3}, "boss": null}),
        ]
    }

    #[test]
    fn pick_by_report_id() {
        let r = pick(&sample_reports(), Some("r2"), None).unwrap();
        assert_eq!(r["raw"]["id"], "r2");
    }

    #[test]
    fn pick_by_index() {
        let r = pick(&sample_reports(), None, Some(1)).unwrap();
        assert_eq!(r["raw"]["id"], "r2");
    }

    #[test]
    fn pick_report_id_wins_over_index() {
        // 原版是 `if report_id: ... elif index is not None:` —— id 优先
        let r = pick(&sample_reports(), Some("r3"), Some(0)).unwrap();
        assert_eq!(r["raw"]["id"], "r3");
    }

    #[test]
    fn pick_falls_back_to_first_on_unknown_id() {
        let r = pick(&sample_reports(), Some("nope"), None).unwrap();
        assert_eq!(r["raw"]["id"], "r1", "找不到该 id 时该回落到第一条");
    }

    #[test]
    fn pick_falls_back_to_first_on_out_of_range_index() {
        for bad in [99, -1] {
            let r = pick(&sample_reports(), None, Some(bad)).unwrap();
            assert_eq!(r["raw"]["id"], "r1", "index={bad} 该回落到第一条");
        }
    }

    #[test]
    fn pick_with_no_selector_takes_the_first() {
        let r = pick(&sample_reports(), None, None).unwrap();
        assert_eq!(r["raw"]["id"], "r1");
    }

    #[test]
    fn pick_empty_list_is_none() {
        assert!(pick(&[], None, None).is_none());
        assert!(pick(&[], Some("r1"), None).is_none());
    }

    #[test]
    fn empty_report_id_string_is_ignored() {
        // 前端可能提交 `{"report_id": ""}`（没选中的下拉框）
        let r = pick(&sample_reports(), Some(""), Some(2)).unwrap();
        assert_eq!(
            r["raw"]["id"], "r3",
            "空串 report_id 该被跳过、走 index 分支"
        );
    }

    #[test]
    fn fallback_target_matches_the_original() {
        assert_eq!(
            FALLBACK_TARGET,
            "https://tool.lzpoke.com/api/reports?type=alpha"
        );
    }
}
