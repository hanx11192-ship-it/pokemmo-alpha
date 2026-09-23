//! 调度器与监控冷却的 7 个接口。
//!
//! 对应原版 `panel/app.py` 的：
//!
//! | 方法 | 路径 | 对应原版函数 |
//! |---|---|---|
//! | GET | `/api/scheduler` | `api_scheduler` |
//! | POST | `/api/scheduler/set` | `api_scheduler_set` |
//! | POST | `/api/scheduler/debug` | `api_scheduler_debug` |
//! | GET | `/api/scheduler/debug` | `api_scheduler_debug_state` |
//! | GET | `/api/monitor` | `api_monitor_state` |
//! | POST | `/api/monitor/check` | `api_monitor_check` |
//! | POST | `/api/monitor/auto-pause` | `api_monitor_auto_pause` |
//!
//! 全部需要登录 —— 原版这 7 个都挂了 `@auth.login_required`。
//! 本版把它们放进 `build_router` 的 `protected` 分支，
//! 由中间件统一拦，而不是逐个写装饰器。
//!
//! # 响应形状保持原样
//!
//! 所有 POST 都返回 `{"ok": true, ...当前状态}`。**多带一份完整状态**
//! 是原版刻意的设计：前端改完设置不用再发一次 GET 就能刷新界面。
//! 如果只回 `{"ok":true}`，前端得连着两个请求，慢且容易出现状态闪烁。
//!
//! # `bool(enabled) if enabled is not None else None` 这个三段式
//!
//! 原版 `api_monitor_auto_pause` 里：
//!
//! ```python
//! enabled = data.get("enabled")           # 可能是 None / True / False
//! scheduler_mod.set_monitor_auto_pause(
//!     enabled=bool(enabled) if enabled is not None else None, ...)
//! ```
//!
//! 这是一个**三态**字段：`None` = 「别动这项」，`True`/`False` = 「设成它」。
//! 用 Rust 的 `Option<bool>` 表达正合适。写成「缺省即 false」就会让
//! 「只改 mode 不动开关」变成「顺手把开关关了」——这是很容易犯的错。

use axum::extract::State;
use axum::Json;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use alpha_scheduler::pause::{self, MonitorState};
use alpha_scheduler::state as sched_state;
use alpha_scheduler::{DebugState, SchedulerState};

use crate::error::{ApiError, ApiResult};
use crate::AppState;

fn scheduler(st: &AppState) -> ApiResult<&std::sync::Arc<alpha_scheduler::Scheduler>> {
    st.scheduler
        .as_ref()
        .ok_or_else(|| ApiError::Internal("调度器未初始化".into()))
}

/// `GET /api/scheduler`
pub async fn get_scheduler(State(st): State<AppState>) -> ApiResult<Json<SchedulerState>> {
    Ok(Json(scheduler(&st)?.snapshot()))
}

/// `POST /api/scheduler/set` 的请求体。
#[derive(Debug, Deserialize)]
pub struct SchedulerSetRequest {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub interval: Option<i64>,
}

/// `POST /api/scheduler/set`
///
/// 开关与间隔一起提交。间隔为 `None` 表示「不改」——
/// 对应原版 `set_scheduler(enabled, int(interval) if interval else None)`。
///
/// 注意原版这里**不校验** interval：`0` 也会写进去，真正生效是在
/// `_loop` 里 `max(5, interval)`。本版保持一致，面板回显的是用户填的值，
/// 实际用的是夹紧后的值。
pub async fn set_scheduler(
    State(st): State<AppState>,
    Json(body): Json<SchedulerSetRequest>,
) -> ApiResult<Json<Value>> {
    let s = scheduler(&st)?;

    sched_state::set_enabled(&s.deps.store, body.enabled, body.interval)
        .map_err(store_err)?;

    // 原版的日志文本：`定时任务启用` / `定时任务停用`，带间隔时补一句
    let mut msg = format!("定时任务{}", if body.enabled { "启用" } else { "停用" });
    if let Some(i) = body.interval {
        msg.push_str(&format!("，间隔 {i}s"));
    }
    s.deps
        .store
        .log(alpha_store::LogLevel::Info, "scheduler", &msg, "panel");

    Ok(Json(ok_with(to_value(&s.snapshot()))))
}

/// `POST /api/scheduler/debug`
///
/// 手动触发一次探测。**异步执行**：源卡住时页面不该干等。
/// 已有任务在跑时返回既有的 `elapsed`，不会起第二个。
pub async fn start_debug(State(st): State<AppState>) -> ApiResult<Json<DebugState>> {
    Ok(Json(scheduler(&st)?.start_debug()))
}

/// `GET /api/scheduler/debug`
///
/// 查询后台探测状态。跑完之前 `result` 是 `null`。
pub async fn debug_state(State(st): State<AppState>) -> ApiResult<Json<DebugState>> {
    Ok(Json(scheduler(&st)?.debug_state()))
}

/// `GET /api/monitor`
pub async fn get_monitor(State(st): State<AppState>) -> ApiResult<Json<MonitorState>> {
    Ok(Json(pause::monitor_state(&scheduler(&st)?.deps.store)))
}

/// `POST /api/monitor/check`
///
/// 手动立即检查：**先解除冷却**再跑一轮（强制）。
///
/// 清冷却这一步不能省：只绕过判断的话，手动检查完仍处于冷却中，
/// 下一轮自动轮询还是不动。
pub async fn monitor_check(State(st): State<AppState>) -> ApiResult<Json<Value>> {
    let s = scheduler(&st)?;
    let deps = s.deps.as_deps();

    let result = alpha_scheduler::poll::force_check_once(&deps)
        .await
        .map_err(|e| ApiError::Internal(format!("强制检查失败: {e}")))?;

    s.deps.store.log(
        alpha_store::LogLevel::Info,
        "scheduler",
        "手动立即检查（解除监控冷却）",
        "panel",
    );

    let mut body = to_value(&pause::monitor_state(&s.deps.store));
    body.insert("ok".into(), json!(true));
    body.insert("result".into(), poll_outcome_json(&result));
    Ok(Json(Value::Object(body)))
}

/// `POST /api/monitor/auto-pause` 的请求体。
///
/// 三个字段都是**可选**的，`None` = 不动这一项。
#[derive(Debug, Deserialize)]
pub struct AutoPauseRequest {
    #[serde(default)]
    pub enabled: Option<bool>,
    #[serde(default)]
    pub mode: Option<String>,
    #[serde(default)]
    pub pause_minutes: Option<i64>,
}

/// `POST /api/monitor/auto-pause`
pub async fn set_auto_pause(
    State(st): State<AppState>,
    Json(body): Json<AutoPauseRequest>,
) -> ApiResult<Json<Value>> {
    let s = scheduler(&st)?;

    // 模式只认 slot / fixed。原版是「非 fixed 一律当 slot」，
    // 那样用户填个 `slo` 会静默变成 slot，面板却回显他填的值。
    // 这里明确报错，让前端能提示。
    if let Some(m) = body.mode.as_deref() {
        if !matches!(m, "slot" | "fixed") {
            return Err(ApiError::BadRequest(
                "mode 只支持 slot / fixed".into(),
            ));
        }
    }

    pause::set_auto_pause(
        &s.deps.store,
        body.enabled,
        body.mode.as_deref(),
        body.pause_minutes,
    )
    .map_err(store_err)?;

    // 日志文本对齐原版
    s.deps.store.log(
        alpha_store::LogLevel::Info,
        "scheduler",
        &format!(
            "监控设置更新 auto_pause={} mode={} minutes={}",
            opt_str(body.enabled.map(|b| b.to_string())),
            opt_str(body.mode.clone()),
            opt_str(body.pause_minutes.map(|m| m.to_string())),
        ),
        "panel",
    );

    Ok(Json(ok_with(to_value(&pause::monitor_state(&s.deps.store)))))
}

/// `null` 的显示形式与原版 Python 的 `None` 一致（`None` 打印成 `None`，
/// 这里用 `null` 更贴近 JSON 语境，且不影响前端取值）。
fn opt_str(v: Option<String>) -> String {
    v.unwrap_or_else(|| "null".to_string())
}

/// 把 `StoreError` 转成 500。
fn store_err(e: alpha_store::StoreError) -> ApiError {
    tracing::error!(error = %e, "调度器状态写入失败");
    ApiError::Internal(format!("状态保存失败: {e}"))
}

/// `{"ok": true, ...状态}` —— 原版所有 POST 都用这个形状。
///
/// 多带一份完整状态是刻意的：前端改完设置不用再发一次 GET 就能刷新界面。
/// 只回 `{"ok":true}` 的话前端得连发两个请求，慢且容易出现状态闪烁。
fn ok_with(mut state: serde_json::Map<String, Value>) -> Value {
    // `ok` 放最前：`serde_json::Map` 默认是 BTreeMap，键有序；
    // 这里显式插进去只是为了不依赖那个顺序，语义上「ok 表示这次调用成功」。
    state.insert("ok".to_string(), json!(true));
    Value::Object(state)
}

/// 结构化序列化（`SchedulerState` / `MonitorState` 都是 `Serialize`）。
fn to_value<T: Serialize>(v: &T) -> serde_json::Map<String, Value> {
    match serde_json::to_value(v) {
        Ok(Value::Object(m)) => m,
        _ => serde_json::Map::new(),
    }
}

/// 单轮结果 → JSON（`/api/monitor/check` 的 `result` 字段）。
fn poll_outcome_json(o: &alpha_scheduler::poll::PollOutcome) -> Value {
    json!({
        "status": o.status,
        "message": o.message,
        "elapsed": o.elapsed,
        "report": o.report,
        "slot": o.slot,
        "channels": o.channels,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 三个字段都是可选 —— 空 body 不应 400。
    #[test]
    fn auto_pause_request_tolerates_missing_fields() {
        let r: AutoPauseRequest = serde_json::from_str("{}").unwrap();
        assert_eq!(r.enabled, None);
        assert_eq!(r.mode, None);
        assert_eq!(r.pause_minutes, None);
    }

    /// **只改 mode 不该顺手把开关关掉** —— 这是三态字段最容易犯的错。
    #[test]
    fn updating_only_the_mode_leaves_enabled_untouched() {
        let r: AutoPauseRequest =
            serde_json::from_str(r#"{"mode":"fixed"}"#).unwrap();
        assert_eq!(
            r.enabled, None,
            "未提交 enabled 时必须是 None（表示「别动」），而不是 Some(false)"
        );
        assert_eq!(r.mode.as_deref(), Some("fixed"));
    }

    /// 显式提交 `false` 与「没提交」是两回事。
    #[test]
    fn explicit_false_is_distinguishable_from_absent() {
        let off: AutoPauseRequest = serde_json::from_str(r#"{"enabled":false}"#).unwrap();
        assert_eq!(off.enabled, Some(false));

        let absent: AutoPauseRequest = serde_json::from_str("{}").unwrap();
        assert_eq!(absent.enabled, None);
        assert_ne!(off.enabled, absent.enabled);
    }

    #[test]
    fn scheduler_set_request_defaults_to_disabled_without_interval() {
        let r: SchedulerSetRequest = serde_json::from_str("{}").unwrap();
        assert!(!r.enabled);
        assert_eq!(r.interval, None);

        let r: SchedulerSetRequest =
            serde_json::from_str(r#"{"enabled":true,"interval":90}"#).unwrap();
        assert!(r.enabled);
        assert_eq!(r.interval, Some(90));
    }

    #[test]
    fn poll_outcome_json_has_the_keys_the_frontend_reads() {
        let o = alpha_scheduler::poll::PollOutcome {
            status: "pushed",
            message: "已推送".into(),
            report: Some("报文".into()),
            slot: None,
            channels: Some(json!({"sent": 1, "failed": 0})),
            resolved: None,
            elapsed: 0.5,
        };
        let v = poll_outcome_json(&o);
        for k in ["status", "message", "elapsed", "report", "slot", "channels"] {
            assert!(v.get(k).is_some(), "缺少字段 {k}: {v}");
        }
        assert_eq!(v["status"], "pushed");
    }
}
