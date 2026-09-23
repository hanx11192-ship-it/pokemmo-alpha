//! # alpha-scheduler —— 轮询调度、投票仲裁、推送、监控冷却
//!
//! 把原版 `panel/scheduler.py`（505 行）的职责拆成几个明确的部分：
//!
//! | 模块 | 负责 |
//! |---|---|
//! | [`fetch`] | 并发拉源，**整批共用一个超时预算** |
//! | [`vote`] | 多源投票仲裁（票数 → 报点时间 → 优先级） |
//! | [`report`] | 决策器选择与回落、评估器注入 |
//! | [`pause`] | 抓到头目后的监控冷却（`slot` / `fixed`） |
//! | [`state`] | kv 状态读写与面板快照 |
//! | [`debug`] | 异步调试探测（不可重入） |
//! | [`poll`] | 一轮轮询的完整编排 —— **五种返回状态** |
//! | [`loop`] | 后台循环的节拍 |
//!
//! # 为什么单独一个 crate
//!
//! 调度器要同时依赖 store / sources / strategy / notify / plugin 五个 crate。
//! 放进 `alpha-server` 会让「HTTP 层」和「业务编排」搅在一起，
//! 而且**测试要起一个 HTTP 服务才能测一轮轮询** —— 太慢也太脆。
//! 拆出来之后 `poll::poll_once` 是个纯异步函数，测试里直接调用即可。
//!
//! # 与原版的行为对齐策略
//!
//! 这个 crate 的每一处判定都对着原版逐行核过，`docs/` 里另有差分验证脚本。
//! 尤其是这几处**容易被"顺手优化掉"但会改行为**的地方，代码里都留了注释：
//!
//! - 整批超时 vs 每源超时（[`fetch`]）
//! - 去重标记「推成功才写」（[`poll`]）
//! - `deduped` 分支**仍要暂停**（[`poll`]）
//! - 推送失败**不暂停**（[`poll`]）
//! - `slot` 模式顺带改写 `monitor_pause_minutes`（[`pause`]）
//! - 决策器选的是 `active=1` 而非 `active=1 AND enabled=1`，
//!   评估器却**要** `enabled=1`（[`report`] + `alpha-store`）

pub mod debug;
pub mod fetch;
pub mod pause;
pub mod poll;
pub mod report;
pub mod state;
pub mod r#vote;

pub use debug::{DebugHandle, DebugState};
pub use fetch::{fetch_all, FetchRound};
pub use pause::{
    clear_pause, is_paused, maybe_pause_after_detect, monitor_state, set_auto_pause, MonitorState,
    PauseMode,
};
pub use poll::{poll_once, Deps, PollOutcome};
pub use report::{build_report, BuiltReport};
pub use r#vote::{resolve_by_vote, Vote};
pub use state::{interval, snapshot, SchedulerState};

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use alpha_core::dedup::DedupStore;
use alpha_core::models::FetchStatus;
use alpha_plugin::PluginRegistry;
use alpha_store::Store;

/// 调度器错误。
#[derive(Debug, thiserror::Error)]
pub enum SchedulerError {
    #[error("存储错误: {0}")]
    Store(#[from] alpha_store::StoreError),

    #[error("配置错误: {0}")]
    Config(String),

    #[error("通知错误: {0}")]
    Notify(#[from] alpha_notify::NotifyError),

    #[error("IO 错误: {0}")]
    Io(#[from] std::io::Error),
}

pub type SchedulerResult<T> = Result<T, SchedulerError>;

/// 后台循环的节拍控制。
///
/// 原版 `_loop` 的核心是「**间隔从本轮开始算**」：
///
/// ```text
/// cycle_start = now()
/// poll_once()
/// spent = now() - cycle_start
/// wait = interval - spent
/// if wait > 0: sleep(wait)
/// else: log("本轮耗时超过间隔，立即进入下一轮")
/// ```
///
/// 不是「跑完再等 interval」—— 那样实际周期是 `round + interval`，
/// 设 60 秒的间隔在慢轮次下会变成 80 秒。这个细节决定了
/// 面板上配的间隔到底是不是真实周期。
#[derive(Debug)]
pub struct LoopClock {
    interval: i64,
    cycle_start: Instant,
}

impl LoopClock {
    pub fn new(interval: i64) -> Self {
        Self {
            interval: interval.max(state::MIN_INTERVAL),
            cycle_start: Instant::now(),
        }
    }

    /// 本轮开始计时。
    pub fn begin_cycle(&mut self) {
        self.cycle_start = Instant::now();
    }

    /// 本轮跑完后该等多久。0 表示「已经超时，立即下一轮」。
    pub fn wait_after_cycle(&self) -> Duration {
        let spent = self.cycle_start.elapsed();
        let budget = Duration::from_secs(self.interval.max(0) as u64);
        budget.saturating_sub(spent)
    }

    /// 本轮耗时（秒）。
    pub fn spent(&self) -> f64 {
        self.cycle_start.elapsed().as_secs_f64()
    }

    pub fn interval(&self) -> i64 {
        self.interval
    }
}

/// 后台循环所需的全部依赖（带所有权，能跨任务移动）。
#[derive(Clone)]
pub struct LoopDeps {
    pub store: Arc<Store>,
    pub config: Arc<alpha_core::config::Config>,
    pub pokedex: &'static alpha_core::pokedex::Pokedex,
    pub registry: Arc<PluginRegistry>,
    pub dedup: Arc<DedupStore>,
    pub notify: Arc<dyn poll::Notifier>,
    /// 源工厂；生产环境用 [`poll::real_sources`]，测试可注入假源
    pub sources: Arc<poll::SourceFactory>,
}

impl LoopDeps {
    /// 借出一份 `Deps` 给单轮轮询用。
    pub fn as_deps(&self) -> poll::Deps<'_> {
        poll::Deps {
            store: &self.store,
            config: Arc::clone(&self.config),
            pokedex: self.pokedex,
            registry: &self.registry,
            dedup: &self.dedup,
            notify: self.notify.as_ref(),
            sources: &self.sources,
        }
    }

    /// 转成调试任务用的所有权版本。
    pub fn as_owned(&self) -> Arc<debug::DepsOwned> {
        Arc::new(debug::DepsOwned {
            store: Arc::clone(&self.store),
            config: Arc::clone(&self.config),
            pokedex: self.pokedex,
            registry: Arc::clone(&self.registry),
            dedup: Arc::clone(&self.dedup),
            notify: Arc::clone(&self.notify),
            sources: Arc::clone(&self.sources),
        })
    }
}

/// 调度器句柄：后台循环 + 调试状态。
pub struct Scheduler {
    pub deps: LoopDeps,
    pub debug: DebugHandle,
    /// 当前后台循环的 `JoinHandle`（重启用）
    handle: Mutex<Option<tokio::task::JoinHandle<()>>>,
}

impl Scheduler {
    pub fn new(deps: LoopDeps) -> Self {
        Self {
            deps,
            debug: DebugHandle::new(),
            handle: Mutex::new(None),
        }
    }

    /// 启动后台循环。重复调用不会起第二个循环。
    pub fn spawn(self: &Arc<Self>) -> bool {
        let mut slot = self.handle.lock().unwrap_or_else(|e| e.into_inner());
        if slot.as_ref().map(|h| !h.is_finished()).unwrap_or(false) {
            tracing::debug!("调度循环已在运行，跳过重复启动");
            return false;
        }
        let me = Arc::clone(self);
        *slot = Some(tokio::spawn(async move { me.run_loop().await }));
        true
    }

    /// 后台循环本体。
    ///
    /// 对应原版 `_loop`。两个退出条件都没有 —— 循环只在进程结束时停止，
    /// 靠 `scheduler_enabled` 开关控制「跑不跑轮询」而不是「退不退出」。
    /// 这样面板开开关能立即生效，不用重启服务。
    pub async fn run_loop(&self) {
        loop {
            // 每轮都重读开关与间隔 —— 面板改完立刻生效
            let enabled = state::enabled(&self.deps.store);
            let interval = state::interval(&self.deps.store);
            let mut clock = LoopClock::new(interval);
            clock.begin_cycle();

            if !enabled {
                // 停用时把状态标成 stopped 并短暂休眠。
                // 只 `continue` 不 sleep 会把这个循环变成 100% CPU 的忙等。
                self.deps
                    .store
                    .set_kv(state::KV_STATUS, "stopped")
                    .ok();
                tokio::time::sleep(Duration::from_secs(2)).await;
                continue;
            }

            let deps = self.deps.as_deps();
            let out = poll_once(&deps, false, false).await;
            self.deps.store.set_kv(state::KV_STATUS, "running").ok();
            tracing::debug!(status = out.status, elapsed = out.elapsed, "本轮完成");

            let wait = clock.wait_after_cycle();
            if wait.is_zero() {
                tracing::warn!(
                    "本轮耗时 {:.1}s 超过间隔 {}s，立即进入下一轮",
                    clock.spent(),
                    clock.interval()
                );
            } else {
                tokio::time::sleep(wait).await;
            }
        }
    }

    /// 启动一次调试探测。
    pub fn start_debug(&self) -> DebugState {
        debug::start_debug(&self.debug, self.deps.as_owned())
    }

    /// 调试状态。
    pub fn debug_state(&self) -> DebugState {
        self.debug.snapshot()
    }

    /// 面板状态快照。
    pub fn snapshot(&self) -> SchedulerState {
        state::snapshot(&self.deps.store)
    }
}

/// 统计一轮结果里各状态的条数（日志/诊断用）。
pub fn count_statuses(round: &FetchRound) -> (usize, usize, usize) {
    let mut hit = 0;
    let mut empty = 0;
    let mut error = 0;
    for (_, r) in &round.results {
        match r.status {
            FetchStatus::Hit => hit += 1,
            FetchStatus::Empty => empty += 1,
            FetchStatus::Error => error += 1,
        }
    }
    (hit, empty, error)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loop_clock_clamps_the_interval() {
        assert_eq!(LoopClock::new(0).interval(), state::MIN_INTERVAL);
        assert_eq!(LoopClock::new(1).interval(), state::MIN_INTERVAL);
        assert_eq!(LoopClock::new(60).interval(), 60);
    }

    /// 间隔是「本轮开始算起」的总时长，不是「跑完再等」。
    #[test]
    fn wait_is_budget_minus_time_spent() {
        let mut c = LoopClock::new(10);
        c.begin_cycle();
        let w = c.wait_after_cycle();
        assert!(
            w <= Duration::from_secs(10) && w > Duration::from_secs(9),
            "刚开始的轮次应几乎能等满 10 秒，实际 {w:?}"
        );
    }

    #[test]
    fn an_overrun_budget_yields_zero_wait() {
        let mut c = LoopClock::new(state::MIN_INTERVAL);
        // 造一个「已经过去很久」的时钟
        c.cycle_start = Instant::now() - Duration::from_secs(60);
        assert!(c.wait_after_cycle().is_zero());
        assert!(c.spent() >= 60.0);
    }
}
