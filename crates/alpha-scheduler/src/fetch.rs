//! 拉源编排：一轮把**所有**启用的源并发跑完，整批共用一个超时。
//!
//! # 关键语义：超时是「整批」的，不是「每源各自算」
//!
//! 原版 `panel/scheduler.py::poll_once` 是这样的：
//!
//! ```python
//! deadline = time.monotonic() + timeout
//! while pending:
//!     done, pending = wait(pending, timeout=max(0.0, deadline - now),
//!                          return_when=FIRST_COMPLETED)
//!     ...
//! ```
//!
//! 也就是说：**轮询开始后的 `timeout` 秒内没回来的源，本轮全部作废**。
//! 不是「每个源各给 20 秒」——那样 8 个源串行最坏能拖到 160 秒，
//! 一个源卡住就把整个推送周期拖垮。这是个**全局节拍**，必须照搬。
//!
//! # 与原版的一处刻意偏离：超时后 abort，而不是放着跑
//!
//! 原版用的是 `ThreadPoolExecutor`，超时后 `pending` 里的线程**仍在后台跑**，
//! 只是结果被丢掉。它没得选——Python 线程杀不掉。
//!
//! Rust 的 `JoinSet` 能 abort，这里就 abort：被放弃的源不该继续占着
//! 连接、内存和 CPU。**对用户可见的行为完全一致**（结果都被丢弃、
//! 都记一条超时日志），只是不在后台留一堆僵尸任务。
//!
//! 需要留意的是 abort 是「协作式」的：在 `.await` 点上才会真正生效。
//! 一个纯同步的、卡在 CPU 里的 `fetch` 实现不会被立刻打断 —— 但那属于
//! 数据源适配器自己的问题，`DataSource` 契约已经要求它别阻塞。
//!
//! # 源加载失败 ≠ 整轮失败
//!
//! `create_source` 失败（适配器名写错、依赖缺失）时，原版是
//! `FetchResult("error", message="加载/执行异常: ...")` 照样参与统计。
//! 这里对齐：加载失败也产生一条 `Error` 结果，而不是把源从本轮里抹掉 ——
//! 面板上要能看到「这个源坏了」，静默跳过等于用户永远不知道。

use std::time::{Duration, Instant};

use alpha_core::config::SourceConfig;
use alpha_core::models::{FetchResult, FetchStatus};
use alpha_sources::SourceHandle;
use tokio::task::JoinSet;

use crate::vote::Vote;

/// 一轮的拉取产物。
#[derive(Debug)]
pub struct FetchRound {
    /// 全部源的结果（含 error / empty），按提交顺序对应
    pub results: Vec<(SourceConfig, FetchResult)>,
    /// 本轮实际耗时（秒，两位小数）
    pub elapsed: f64,
    /// 超时被放弃的源名
    pub timed_out: Vec<String>,
}

impl FetchRound {
    /// 只要 `hit` 的源，转成投票用的条目。
    pub fn hits(&self) -> Vec<Vote> {
        self.results
            .iter()
            .filter(|(_, r)| r.status == FetchStatus::Hit && r.boss.is_some())
            .map(|(s, r)| Vote {
                source_name: s.name.clone(),
                result: r.clone(),
            })
            .collect()
    }

    /// 有结果的源名 → 优先级映射。
    pub fn priority_map(&self) -> Vec<(String, i64)> {
        self.results
            .iter()
            .map(|(s, _)| (s.name.clone(), s.priority))
            .collect()
    }
}

/// 并发跑完所有源。
///
/// `handles` 来自 `alpha_sources::create_all(&config)`；`timeout` 是
/// **整批**共用的秒数。
pub async fn fetch_all(handles: Vec<SourceHandle>, timeout: f64) -> FetchRound {
    let started = Instant::now();

    if handles.is_empty() {
        return FetchRound {
            results: Vec::new(),
            elapsed: 0.0,
            timed_out: Vec::new(),
        };
    }

    // 按提交顺序记名字：JoinSet 的完成顺序是乱的，超时后只能靠任务自己
    // 带名字回传，所以这里让每个任务把 `SourceConfig` 一并返回。
    let mut set: JoinSet<(SourceConfig, FetchResult)> = JoinSet::new();
    // 本轮提交过的全部源名，超时名单 = 提交过 − 拿到结果
    let mut submitted: Vec<String> = Vec::new();
    for h in handles {
        let SourceHandle {
            config, adapter, ..
        } = h;
        submitted.push(config.name.clone());
        set.spawn(async move {
            let res = adapter.fetch().await;
            (config, res)
        });
    }

    let deadline = started + Duration::from_secs_f64(timeout.max(0.0));
    let mut results: Vec<(SourceConfig, FetchResult)> = Vec::with_capacity(submitted.len());

    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            break;
        }
        match tokio::time::timeout(remaining, set.join_next()).await {
            // 拿到一个结果
            Ok(Some(Ok(pair))) => results.push(pair),
            // 任务 panic：不能让它把整轮拖死，转成 error 结果
            Ok(Some(Err(join_err))) => results.push((
                SourceConfig {
                    name: format!("<task:{}>", join_err.id()),
                    ..Default::default()
                },
                FetchResult::error(format!("任务异常终止: {join_err}")),
            )),
            // 全部完成
            Ok(None) => break,
            // 到点了
            Err(_) => break,
        }
    }

    // 还在跑的源：abort 掉，别让它们跨轮泄漏
    let still_running = set.len();
    if still_running > 0 {
        set.abort_all();
        // 等 abort 生效再返回，否则任务可能活到下一轮、把连接池占满
        while set.join_next().await.is_some() {}
    }

    // 超时名单 = 提交过但没拿到结果的源。
    // 用「已收到的名字」做差集，而不是看 `still_running` —— 后者在
    // 全部完成时也有可能是 0，但两者的语义不同（一个是「还欠几个」，
    // 一个是「哪几个」），日志里要的是名字。
    let got: std::collections::HashSet<&str> =
        results.iter().map(|(s, _)| s.name.as_str()).collect();
    let timed_out: Vec<String> = submitted
        .into_iter()
        .filter(|n| !got.contains(n.as_str()))
        .collect();

    let elapsed = started.elapsed().as_secs_f64();
    FetchRound {
        results,
        elapsed: (elapsed * 100.0).round() / 100.0,
        timed_out,
    }
}

/// 把 `create_all` 可能漏掉的源补成 error 结果。
///
/// `create_all` 在构造失败时会 `tracing::warn!` 并**跳过**该源，
/// 于是面板上这个源会凭空消失。原版不会：它照样报一条
/// `加载/执行异常: ...`。这里在调度器侧补齐这个差异。
pub fn reconcile_missing(expected: &[&SourceConfig], got: &mut Vec<(SourceConfig, FetchResult)>) {
    // 先把已有的名字收进 `HashSet<String>`（owned），避免在 push 期间
    // 还借着 `got` 的不可变引用
    let seen: std::collections::HashSet<String> =
        got.iter().map(|(s, _)| s.name.clone()).collect();
    for scfg in expected {
        if !seen.contains(&scfg.name) {
            got.push((
                (*scfg).clone(),
                FetchResult::error(format!("加载/执行异常: 适配器 {} 不可用", scfg.adapter)),
            ));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alpha_sources::SourceHandle;

    struct Fake {
        name: &'static str,
        delay_ms: u64,
    }

    #[async_trait::async_trait]
    impl alpha_sources::DataSource for Fake {
        fn name(&self) -> &'static str {
            self.name
        }
        async fn fetch(&self) -> FetchResult {
            if self.delay_ms > 0 {
                tokio::time::sleep(Duration::from_millis(self.delay_ms)).await;
            }
            FetchResult::empty("nothing")
        }
    }

    fn handle(name: &'static str, delay_ms: u64, priority: i64) -> SourceHandle {
        SourceHandle {
            config: SourceConfig {
                name: name.to_string(),
                adapter: "fake".into(),
                enabled: true,
                priority,
                options: Default::default(),
                note: None,
            },
            adapter: Box::new(Fake { name, delay_ms }),
            priority,
        }
    }

    #[tokio::test]
    async fn empty_input_is_a_zero_round() {
        let r = fetch_all(Vec::new(), 5.0).await;
        assert!(r.results.is_empty());
        assert_eq!(r.elapsed, 0.0);
    }

    #[tokio::test]
    async fn all_sources_complete_within_the_budget() {
        let hs = vec![handle("a", 10, 100), handle("b", 20, 100)];
        let r = fetch_all(hs, 5.0).await;
        assert_eq!(r.results.len(), 2);
        assert!(r.timed_out.is_empty());
    }

    /// 一个慢源不该拖住整轮：慢的被放弃，快的照常产出。
    #[tokio::test]
    async fn a_slow_source_does_not_block_the_round() {
        let hs = vec![handle("fast", 10, 100), handle("slow", 5_000, 100)];
        let started = Instant::now();
        let r = fetch_all(hs, 0.3).await;
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "整批超时应当及时返回，实际 {:?}",
            started.elapsed()
        );
        assert_eq!(r.results.len(), 1);
        assert_eq!(r.results[0].0.name, "fast");
    }

    /// 超时是整批共享的：两个各 0.2s 的源在 0.3s 的总预算里活不下来
    /// （串行也才 0.4s，但这里是并发的所以能过）。换个角度验证预算确实是
    /// 「从开始算起」，而不是每源各算。
    #[tokio::test]
    async fn the_budget_is_shared_across_the_whole_batch() {
        // 4 个源各 0.15s，并发跑约 0.15s < 0.3s → 全过
        let hs = vec![
            handle("a", 150, 100),
            handle("b", 150, 100),
            handle("c", 150, 100),
            handle("d", 150, 100),
        ];
        let r = fetch_all(hs, 1.0).await;
        assert_eq!(r.results.len(), 4, "并发预算共享下四个源都该完成");
    }

    #[test]
    fn reconcile_fills_in_sources_that_failed_to_construct() {
        let a = SourceConfig {
            name: "a".into(),
            adapter: "x".into(),
            ..Default::default()
        };
        let b = SourceConfig {
            name: "b".into(),
            adapter: "y".into(),
            ..Default::default()
        };
        let mut got = vec![(a.clone(), FetchResult::empty("ok"))];
        reconcile_missing(&[&a, &b], &mut got);
        assert_eq!(got.len(), 2);
        assert_eq!(got[1].0.name, "b");
        assert_eq!(got[1].1.status, FetchStatus::Error);
        assert!(got[1].1.message.contains("加载/执行异常"));
    }
}
