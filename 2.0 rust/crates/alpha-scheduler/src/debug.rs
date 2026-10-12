//! 异步调试探测。
//!
//! 面板上点「调试探测」要立刻看到「正在探测…」，而不是卡住等网络。
//! 所以一轮探测跑在后台任务里，前端轮询取结果。
//!
//! # 不可重入
//!
//! 正在跑的时候再点一次，**不会**再起一个任务，而是返回 `running`
//! 加上已耗时。原版也是这个语义（`_debug_state["running"]`）。
//!
//! 这不只是省资源：两个并发探测会各自命中同一只头目，日志里出现两条
//! 一模一样的 `[DEBUG] 探测到…`，用户会以为系统在重复工作。
//!
//! # `result` 在跑完之前是 `None`
//!
//! 前端靠这个区分「还在跑」和「跑完了但没命中」——
//! 跑完没命中的 `result` 是一个带 `status: "empty"` 的对象，
//! **不是 `None`**。把两者混起来，面板会永远显示「探测中」。
//!
//! # 原版的一个小毛病：`started_at` 被重置
//!
//! 原版 `start_debug_async` 里是**整个替换** `_debug_state` 字典：
//!
//! ```python
//! _debug_state = {"running": True, "started_at": time.time(), "result": None}
//! ```
//!
//! 这没问题。但 `get_debug_state` 里有：
//!
//! ```python
//! st["elapsed"] = round(time.time() - st.get("started_at", time.time()), 1) if st["running"] else 0
//! ```
//!
//! 用 `.get(..., time.time())` 兜底意味着：**如果 `started_at` 缺失，
//! 耗时永远是 0**。这里用 `Option` 表达「还没开始」，缺失时按 0 处理，
//! 语义更清楚，行为一致。

use std::sync::{Arc, Mutex};
use std::time::Instant;

use serde::{Deserialize, Serialize};

use crate::poll::{poll_once, Deps};

/// 调试任务状态。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DebugState {
    pub running: bool,
    /// 已运行秒数（未运行时为 0）
    pub elapsed: f64,
    /// 探测结果；**跑完之前是 `None`**
    pub result: Option<serde_json::Value>,
}

impl Default for DebugState {
    fn default() -> Self {
        Self {
            running: false,
            elapsed: 0.0,
            result: None,
        }
    }
}

/// 共享的调试状态句柄。
#[derive(Debug, Clone, Default)]
pub struct DebugHandle {
    inner: Arc<Mutex<Inner>>,
}

#[derive(Debug, Default)]
struct Inner {
    running: bool,
    started: Option<Instant>,
    result: Option<serde_json::Value>,
}

impl DebugHandle {
    pub fn new() -> Self {
        Self::default()
    }

    /// 当前状态快照。
    pub fn snapshot(&self) -> DebugState {
        let g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        DebugState {
            running: g.running,
            elapsed: if g.running {
                g.started
                    .map(|s| {
                        let secs = s.elapsed().as_secs_f64();
                        (secs * 10.0).round() / 10.0
                    })
                    .unwrap_or(0.0)
            } else {
                0.0
            },
            result: g.result.clone(),
        }
    }

    /// 尝试开始一次调试。
    ///
    /// 返回 `(是否真的启动了, 当前状态)`。已经跑着的时候第一个值是 `false`。
    pub fn try_begin(&self) -> (bool, DebugState) {
        let mut g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        if g.running {
            let elapsed = g
                .started
                .map(|s| (s.elapsed().as_secs_f64() * 10.0).round() / 10.0)
                .unwrap_or(0.0);
            return (
                false,
                DebugState {
                    running: true,
                    elapsed,
                    result: g.result.clone(),
                },
            );
        }
        g.running = true;
        g.started = Some(Instant::now());
        // 新一轮开始，把上一轮结果清掉 —— 前端据此知道「这个结果是旧的」
        g.result = None;
        (
            true,
            DebugState {
                running: true,
                elapsed: 0.0,
                result: None,
            },
        )
    }

    /// 任务结束时写入结果。
    pub fn finish(&self, result: serde_json::Value) {
        let mut g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        g.running = false;
        g.started = None;
        g.result = Some(result);
    }
}

/// 启动一次异步调试探测。
///
/// `deps` 必须能活到任务结束 —— 它借用 `Store` / `Config` 等，
/// 所以调用方要保证这些对象比任务活得久（面板里它们都是 `'static` 的
/// `Arc` / 全局单例）。
///
/// 返回当前状态：已启动时 `running=true, elapsed=0`；
/// 已在跑时返回既有的 `elapsed`。
pub fn start_debug(
    handle: &DebugHandle,
    deps: Arc<DepsOwned>,
) -> DebugState {
    let (started, state) = handle.try_begin();
    if !started {
        return state;
    }

    let h = handle.clone();
    tokio::spawn(async move {
        let res = run_debug(&deps).await;
        h.finish(res);
    });

    DebugState {
        running: true,
        elapsed: 0.0,
        result: None,
    }
}

async fn run_debug(deps: &DepsOwned) -> serde_json::Value {
    let borrow = deps.as_deps();
    let out = poll_once(&borrow, true, true).await;
    serde_json::json!({
        "status": out.status,
        "message": out.message,
        "elapsed": out.elapsed,
        "report": out.report,
        "resolved": out.resolved,
    })
}

/// 让调试任务能持有依赖的「所有权版本」。
///
/// `Deps<'a>` 全是借用，跨 `tokio::spawn` 的 `'static` 边界活不过去。
/// 这里的做法是把借用换成 `Arc` / `'static` 引用 —— 面板里
/// `Store` 在 `AppState` 里是 `Arc`、`config`/`pokedex` 是全局单例，
/// 都能直接变成 `'static`，不需要真的克隆数据。
pub struct DepsOwned {
    pub store: Arc<alpha_store::Store>,
    pub config: Arc<alpha_core::config::Config>,
    pub pokedex: &'static alpha_core::pokedex::Pokedex,
    pub registry: Arc<alpha_plugin::PluginRegistry>,
    pub dedup: Arc<alpha_core::dedup::DedupStore>,
    pub notify: Arc<dyn crate::poll::Notifier>,
    pub sources: Arc<crate::poll::SourceFactory>,
}

impl DepsOwned {
    fn as_deps(&self) -> Deps<'_> {
        Deps {
            store: &self.store,
            config: Arc::clone(&self.config),
            pokedex: self.pokedex,
            registry: &self.registry,
            dedup: &self.dedup,
            notify: self.notify.as_ref(),
            sources: &self.sources,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn initial_state_is_idle_with_no_result() {
        let h = DebugHandle::new();
        let s = h.snapshot();
        assert!(!s.running);
        assert_eq!(s.elapsed, 0.0);
        assert!(s.result.is_none(), "未跑过时 result 必须是 None");
    }

    #[test]
    fn second_begin_is_rejected_while_running() {
        let h = DebugHandle::new();
        let (first, _) = h.try_begin();
        assert!(first);
        let (second, state) = h.try_begin();
        assert!(!second, "正在跑时不应重复启动");
        assert!(state.running);
        assert!(state.result.is_none());
    }

    /// 跑完之后 `result` 必须有值 —— 哪怕是「没命中」的空结果。
    /// 前端靠 `result.is_none()` 区分「还在跑」和「跑完了」。
    #[test]
    fn finishing_sets_a_result_even_when_empty() {
        let h = DebugHandle::new();
        h.try_begin();
        h.finish(serde_json::json!({"status": "empty"}));
        let s = h.snapshot();
        assert!(!s.running);
        assert_eq!(s.result.unwrap()["status"], "empty");
    }

    #[test]
    fn a_new_run_clears_the_previous_result() {
        let h = DebugHandle::new();
        h.try_begin();
        h.finish(serde_json::json!({"status": "pushed"}));
        assert!(h.snapshot().result.is_some());

        h.try_begin();
        let s = h.snapshot();
        assert!(s.running);
        assert!(
            s.result.is_none(),
            "新一轮开始时旧结果必须清掉，否则前端会显示上一轮的结果"
        );
    }

    #[test]
    fn elapsed_is_zero_when_not_running() {
        let h = DebugHandle::new();
        h.try_begin();
        h.finish(serde_json::json!({}));
        assert_eq!(h.snapshot().elapsed, 0.0);
    }
}
