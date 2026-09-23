//! 日志 3 个接口。
//!
//! 对应原版 `panel/app.py` 的 `api_logs` / `api_logs_clean` / `api_logs_state`。
//!
//! # 「实时日志」这个开关到底是什么
//!
//! `POST /api/logs/state` 写一个叫 `query_log` 的 KV（`"1"` / `"0"`），
//! 只有调度器会读它 —— 开启后每一轮轮询会把**每个源的查询结果**
//! 也写进日志表（见 `alpha-scheduler`），而默认只记「有头目」的结果。
//!
//! 所以这个接口本身不产生任何日志行为，它只是翻一个开关。
//! `GET /api/logs` 顺带回传这个开关的当前值，
//! 前端据此渲染勾选框的状态。

use axum::extract::{Query, State};
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};

use alpha_store::{LogFilter, LogLevel};

use crate::error::ApiResult;
use crate::AppState;

/// `query_log` 这个 KV 的键名。别处（调度器）也读它。
pub const QUERY_LOG_KEY: &str = "query_log";

/// 原版的 `limit` 缺省值。
const DEFAULT_LIMIT: usize = 200;

#[derive(Debug, Deserialize)]
pub struct LogsQuery {
    #[serde(default)]
    pub kind: Option<String>,
    #[serde(default)]
    pub source: Option<String>,
    /// 用 `Option<String>` 而不是 `Option<usize>`：
    ///
    /// 原版是 `int(request.args.get("limit", 200))`，前端如果传了
    /// `limit=` （空串）或 `limit=abc`，那行会抛 `ValueError` 变成 500。
    /// 本版把解析失败当成「用缺省值」—— 分页参数写坏不该让整页崩掉。
    #[serde(default)]
    pub limit: Option<String>,
}

/// 把 `limit` 字符串解析成数字，坏值回落到缺省。
fn parse_limit(raw: Option<&str>) -> usize {
    raw.and_then(|s| s.trim().parse::<usize>().ok())
        .filter(|n| *n > 0)
        .unwrap_or(DEFAULT_LIMIT)
}

/// `GET /api/logs`
///
/// 回 `{logs, sources, query_log}`。
/// - `sources` 是日志库里出现过的所有来源（前端做筛选下拉框）
/// - `query_log` 是「实时日志」开关的当前状态，**布尔**（不是 `"1"`/`"0"`）
pub async fn list(
    State(state): State<AppState>,
    Query(q): Query<LogsQuery>,
) -> ApiResult<Json<Value>> {
    let mut filter = LogFilter::new(parse_limit(q.limit.as_deref()));
    // 空串当成「不筛」—— 前端的下拉框默认项会提交空串，
    // 直接拿去 `WHERE kind=''` 会一条都查不到。
    filter.kind = q.kind.filter(|s| !s.is_empty());
    filter.source = q.source.filter(|s| !s.is_empty());

    let logs = state.store.list_logs(&filter)?;
    let sources = state.store.list_log_sources()?;

    // 原版 `db.get_kv("query_log", "1") == "1"` —— **缺省是开**。
    // 这点容易看漏：`get_kv_or(key, "1")` 里的 "1" 是缺省值，
    // 不是「关」。首次部署时 `kv` 表里没有这个键，此刻实时日志是**开着**的。
    let query_log = state.store.get_kv_or(QUERY_LOG_KEY, "1") == "1";

    Ok(Json(json!({
        "logs": logs,
        "sources": sources,
        "query_log": query_log,
    })))
}

#[derive(Debug, Deserialize)]
pub struct CleanRequest {
    #[serde(default)]
    pub days: Option<i64>,
}

/// `POST /api/logs/clean`
///
/// 删掉 `days` 天前的日志。原版缺省 3 天。
///
/// 删完自己写一条日志 —— 所以「清理」这个动作本身也留痕，
/// 但这条日志的 `ts` 是现在，不会被下一次清理带走。
pub async fn clean(
    State(state): State<AppState>,
    Json(body): Json<CleanRequest>,
) -> ApiResult<Json<Value>> {
    let days = body.days.unwrap_or(3);
    // 负数会让 `stamp_days_ago` 算出一个**未来**的时间点，
    // 于是 `WHERE ts < 未来` 会把所有日志删光。
    // 原版没挡这个（`int(data.get("days", 3))`），本版挡掉。
    let days = days.max(0);

    let n = state.store.cleanup_logs(days)?;

    state.store.log(
        LogLevel::Info,
        "log",
        &format!("清理 {days} 天前日志，删除 {n} 条"),
        "panel",
    );

    Ok(Json(json!({ "ok": true, "deleted": n })))
}

#[derive(Debug, Deserialize)]
pub struct StateRequest {
    #[serde(default)]
    pub enabled: Option<bool>,
}

/// `POST /api/logs/state`
///
/// 切换「实时日志」。原版缺省是 **`false`**（`bool(data.get("enabled", False))`）
/// —— 与 `api_logs` 读出来的缺省值 `"1"`（开）方向相反。
///
/// 这两个缺省值不一致是原版的事实行为，本版保留：
/// 它只影响「不带字段的请求」这种边角情形，前端总是显式传值。
pub async fn set_state(
    State(state): State<AppState>,
    Json(body): Json<StateRequest>,
) -> ApiResult<Json<Value>> {
    let on = body.enabled.unwrap_or(false);

    state
        .store
        .set_kv(QUERY_LOG_KEY, if on { "1" } else { "0" })?;

    state.store.log(
        LogLevel::Info,
        "log",
        &format!("源查询日志{}", if on { "开启" } else { "关闭" }),
        "panel",
    );

    Ok(Json(json!({ "ok": true, "enabled": on })))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn limit_defaults_and_falls_back_on_garbage() {
        assert_eq!(parse_limit(None), 200);
        assert_eq!(parse_limit(Some("50")), 50);
        assert_eq!(parse_limit(Some(" 50 ")), 50, "该容忍前后空白");
        // 坏值不该让接口 500 —— 原版这里会抛 ValueError
        assert_eq!(parse_limit(Some("abc")), 200);
        assert_eq!(parse_limit(Some("")), 200);
        assert_eq!(parse_limit(Some("-5")), 200, "负数该回落到缺省");
        assert_eq!(parse_limit(Some("0")), 200, "0 会让前端拿不到任何数据");
    }

    #[test]
    fn query_log_key_matches_the_original() {
        // 调度器读的是同一个键，改这里必须两边一起改
        assert_eq!(QUERY_LOG_KEY, "query_log");
    }
}
