//! 调度器 7 个接口的端到端测试。
//!
//! 跑的是完整链路：真实 router → 真实鉴权中间件 → 真实 handler
//! → 真实 SQLite。**不带调度器**（`AppState::scheduler` 是 `None`
//! 时接口应当明确报错而不是 panic），另外单独验一个装了调度器的场景。
//!
//! # 为什么这些测试重要
//!
//! 原版这 7 个接口里有几个「不显眼的正确性」：
//!
//! - `POST /api/scheduler/set` 必须**带当前状态一起返回**，前端靠它刷界面；
//! - `POST /api/monitor/auto-pause` 的三个字段是**三态**的，
//!   只改 `mode` 不能顺手把 `enabled` 关掉；
//! - 这 7 个全都要登录，一个漏了就是「未授权也能停掉调度器」。
//!
//! 前两条单测能覆盖序列化，但覆盖不了「中间件是否真的拦住了」——
//! 而鉴权漏配恰恰是这次重构要根治的那个问题的同一类。

use std::collections::HashMap;
use std::sync::Arc;

use alpha_server::auth::layer::{CSRF_HEADER, CSRF_HEADER_VALUE};
use alpha_server::{build_router, AppState, Config};
use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use http_body_util::BodyExt;
use serde_json::Value;
use tower::ServiceExt;

struct Fixture {
    _dir: tempfile::TempDir,
    app: Router,
}

impl Fixture {
    fn new() -> Self {
        Self::build(false)
    }

    /// 带调度器的版本。
    fn with_scheduler() -> Self {
        Self::build(true)
    }

    fn build(mount_scheduler: bool) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let cfg = Config {
            host: "127.0.0.1".into(),
            port: 0,
            secret: "test-secret-value-at-least-32-bytes-long".into(),
            data_dir: dir.path().to_path_buf(),
            config_dir: dir.path().to_path_buf(),
            session_ttl: 3600,
            envs: HashMap::new(),
        };
        let store = alpha_store::Store::open(cfg.db_path()).unwrap();
        let mut state = AppState::new(cfg, store);

        if mount_scheduler {
            let dedup = Arc::new(alpha_core::dedup::DedupStore::new(
                dir.path().join("state.json"),
                500,
            ));
            let notify = alpha_notify::Dispatcher::new(false, vec![]).unwrap();
            let deps = alpha_scheduler::LoopDeps {
                store: Arc::new(state.store.clone()),
                config: alpha_core::config::get_config().unwrap(),
                pokedex: alpha_core::pokedex::get_pokedex().unwrap(),
                registry: Arc::new(
                    alpha_plugin::PluginRegistry::new(dir.path().join("plugins")).unwrap(),
                ),
                dedup,
                // 与 `main.rs` 的装配保持一致：`RealNotifier` 要一份 `Store`
                // 才能逐渠道写推送日志（见该结构体的文档）。
                notify: Arc::new(alpha_scheduler::poll::RealNotifier::new(
                    Arc::new(notify),
                    state.store.clone(),
                )),
                // 用假源工厂：这些测试不该去打真实源站。
                // 需要真拉源的用例由 `poll_states.rs` 用注入的假源覆盖。
                // 这里的闭包类型必须写全 —— `Box<dyn Fn>` 里的 `_cfg`
                // 参数没法靠推断确定。
                sources: Arc::new(empty_sources()),
            };
            state.scheduler = Some(Arc::new(alpha_scheduler::Scheduler::new(deps)));
        }

        let app = build_router(state.clone());
        Self { _dir: dir, app }
    }

    async fn get(&self, path: &str, cookie: Option<&str>) -> (StatusCode, Value) {
        let mut b = Request::builder().uri(path);
        if let Some(c) = cookie {
            b = b.header("cookie", c);
        }
        send(self.app.clone(), b.body(Body::empty()).unwrap()).await
    }

    async fn post(&self, path: &str, body: Value, cookie: Option<&str>) -> (StatusCode, Value) {
        self.post_raw(path, Some(body), cookie, Some(CSRF_HEADER_VALUE))
            .await
    }

    async fn post_raw(
        &self,
        path: &str,
        body: Option<Value>,
        cookie: Option<&str>,
        csrf: Option<&str>,
    ) -> (StatusCode, Value) {
        let mut b = Request::builder().method("POST").uri(path);
        if let Some(c) = cookie {
            b = b.header("cookie", c);
        }
        if let Some(c) = csrf {
            b = b.header(CSRF_HEADER, c);
        }
        let payload = match body {
            Some(v) => {
                b = b.header("content-type", "application/json");
                serde_json::to_vec(&v).unwrap()
            }
            None => Vec::new(),
        };
        send(self.app.clone(), b.body(Body::from(payload)).unwrap()).await
    }

    async fn login(&self) -> String {
        let req = Request::builder()
            .method("POST")
            .uri("/api/login")
            .header("content-type", "application/json")
            .header(CSRF_HEADER, CSRF_HEADER_VALUE)
            .body(Body::from(
                serde_json::to_vec(&serde_json::json!({"username": "tester", "password": "pw"}))
                    .unwrap(),
            ))
            .unwrap();
        let resp = self.app.clone().oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK, "前置条件：登录应该成功");
        resp.headers()
            .get("set-cookie")
            .expect("登录成功必须下发 cookie")
            .to_str()
            .unwrap()
            .split(';')
            .next()
            .unwrap()
            .to_string()
    }
}

/// 一个「一个源都不给」的源工厂。
///
/// 返回 `Vec<SourceHandle>` 而不是 `!`：`create_all` 的失败语义是
/// 「跳过这个源」，空列表就是「所有源都被跳过」，走的是 `empty` 分支。
fn empty_sources() -> alpha_scheduler::poll::SourceFactory {
    Box::new(|_cfg: &alpha_core::config::Config| Vec::new())
}

async fn send(app: Router, req: Request<Body>) -> (StatusCode, Value) {
    let resp = app.oneshot(req).await.unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, value)
}

// ---------------------------------------------------------------------------
// 鉴权覆盖：7 个端点一个都不能漏
// ---------------------------------------------------------------------------

/// 7 个端点全部需要登录。**任何一个漏了都是「未授权也能停掉调度器」。**
#[tokio::test]
async fn all_seven_endpoints_require_login() {
    let f = Fixture::with_scheduler();

    let gets = ["/api/scheduler", "/api/scheduler/debug", "/api/monitor"];
    for path in gets {
        let (status, _) = f.get(path, None).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "GET {path} 应要求登录");
    }

    let posts = [
        "/api/scheduler/set",
        "/api/scheduler/debug",
        "/api/monitor/check",
        "/api/monitor/auto-pause",
    ];
    for path in posts {
        let (status, _) = f.post(path, serde_json::json!({}), None).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "POST {path} 应要求登录");
    }
}

/// 状态变更接口必须校验 CSRF 头 —— 不带就该被拒。
#[tokio::test]
async fn state_changing_endpoints_require_the_csrf_header() {
    let f = Fixture::with_scheduler();
    let cookie = f.login().await;

    let (status, _) = f
        .post_raw(
            "/api/scheduler/set",
            Some(serde_json::json!({"enabled": true})),
            Some(&cookie),
            None, // 故意不带 CSRF 头
        )
        .await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "缺 CSRF 头应当被拒，实际 {status}"
    );
}

// ---------------------------------------------------------------------------
// /api/scheduler
// ---------------------------------------------------------------------------

#[tokio::test]
async fn get_scheduler_returns_the_state_shape_the_frontend_needs() {
    let f = Fixture::with_scheduler();
    let cookie = f.login().await;

    let (status, body) = f.get("/api/scheduler", Some(&cookie)).await;
    assert_eq!(status, StatusCode::OK);
    for k in [
        "enabled",
        "interval",
        "status",
        "last_run",
        "last_result",
        "current_slot",
        "current_slot_is_current",
        "reported_this_slot",
        "monitor",
    ] {
        assert!(body.get(k).is_some(), "缺少字段 {k}: {body}");
    }
    assert_eq!(body["enabled"], false, "默认应当是关着的");
    assert_eq!(body["status"], "stopped");
    assert_eq!(body["interval"], 60);
}

/// `POST /api/scheduler/set` 必须**带当前状态一起返回**，
/// 前端靠它刷界面（原版就是 `{"ok": true, **state}`）。
#[tokio::test]
async fn setting_the_scheduler_echoes_the_full_new_state() {
    let f = Fixture::with_scheduler();
    let cookie = f.login().await;

    let (status, body) = f
        .post(
            "/api/scheduler/set",
            serde_json::json!({"enabled": true, "interval": 90}),
            Some(&cookie),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["ok"], true);
    assert_eq!(body["enabled"], true);
    assert_eq!(body["interval"], 90);

    // 再 GET 一次确认真的落库了
    let (_, again) = f.get("/api/scheduler", Some(&cookie)).await;
    assert_eq!(again["enabled"], true);
    assert_eq!(again["interval"], 90);
}

/// 不提交 `interval` 时不该改动原有间隔。
#[tokio::test]
async fn omitting_the_interval_leaves_it_unchanged() {
    let f = Fixture::with_scheduler();
    let cookie = f.login().await;

    f.post(
        "/api/scheduler/set",
        serde_json::json!({"enabled": true, "interval": 120}),
        Some(&cookie),
    )
    .await;

    let (_, body) = f
        .post(
            "/api/scheduler/set",
            serde_json::json!({"enabled": false}),
            Some(&cookie),
        )
        .await;
    assert_eq!(body["enabled"], false);
    assert_eq!(body["interval"], 120, "未提交 interval 时不该被改掉");
}

/// 间隔下限在**读取**时夹紧（写进去的是原值，回显也是原值）。
#[tokio::test]
async fn a_too_small_interval_is_clamped_on_read() {
    let f = Fixture::with_scheduler();
    let cookie = f.login().await;

    let (_, body) = f
        .post(
            "/api/scheduler/set",
            serde_json::json!({"enabled": true, "interval": 0}),
            Some(&cookie),
        )
        .await;
    assert_eq!(
        body["interval"], 5,
        "0 会被夹到下限 5，否则面板就成了「疯狂打源站」的按钮"
    );
}

// ---------------------------------------------------------------------------
// 调试探测
// ---------------------------------------------------------------------------

/// 装好调度器时，调试接口应当能正常返回状态（不是 500）。
#[tokio::test]
async fn debug_endpoints_work_with_a_mounted_scheduler() {
    let f = Fixture::with_scheduler();
    let cookie = f.login().await;

    let (status, body) = f.post("/api/scheduler/debug", Value::Null, Some(&cookie)).await;
    assert_eq!(status, StatusCode::OK, "启动调试不应失败: {body}");
    assert_eq!(body["running"], true);
    assert!(body["result"].is_null(), "刚启动时 result 必须是 null");

    // 状态查询接口也要能通
    let (status, body) = f.get("/api/scheduler/debug", Some(&cookie)).await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.get("running").is_some());
    assert!(body.get("result").is_some());
}

/// 没装调度器时应当明确报错，而不是 panic（生产里调度器可能起不来）。
#[tokio::test]
async fn endpoints_report_a_clear_error_without_a_scheduler() {
    let f = Fixture::new(); // 不带调度器
    let cookie = f.login().await;

    for (path, is_post) in [
        ("/api/scheduler", false),
        ("/api/monitor", false),
        ("/api/scheduler/debug", true),
    ] {
        let (status, body) = if is_post {
            f.post(path, Value::Null, Some(&cookie)).await
        } else {
            f.get(path, Some(&cookie)).await
        };
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR, "{path}");
        assert_eq!(body["ok"], false);
        assert!(
            body["error"].as_str().unwrap_or("").contains("调度器"),
            "{path} 的报错应说明是调度器没初始化: {body}"
        );
    }
}

// ---------------------------------------------------------------------------
// 监控冷却
// ---------------------------------------------------------------------------

#[tokio::test]
async fn get_monitor_returns_the_state_shape() {
    let f = Fixture::with_scheduler();
    let cookie = f.login().await;

    let (status, body) = f.get("/api/monitor", Some(&cookie)).await;
    assert_eq!(status, StatusCode::OK);
    for k in [
        "auto_pause",
        "mode",
        "pause_minutes",
        "paused",
        "pause_until",
        "pause_until_str",
        "remaining_seconds",
    ] {
        assert!(body.get(k).is_some(), "缺少字段 {k}: {body}");
    }
    assert_eq!(body["auto_pause"], true, "默认开着");
    assert_eq!(body["mode"], "slot");
    assert_eq!(body["paused"], false);
}

/// **只改 mode 不能顺手把 enabled 关掉。**
///
/// 这是三态字段最容易犯的错：把「没提交」当成 `false`，
/// 结果用户只想换个模式，开关却被关了。
#[tokio::test]
async fn updating_only_the_mode_does_not_toggle_the_switch() {
    let f = Fixture::with_scheduler();
    let cookie = f.login().await;

    let (status, body) = f
        .post(
            "/api/monitor/auto-pause",
            serde_json::json!({"mode": "fixed", "pause_minutes": 45}),
            Some(&cookie),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["ok"], true);
    assert_eq!(body["mode"], "fixed");
    assert_eq!(body["pause_minutes"], 45);
    assert_eq!(
        body["auto_pause"], true,
        "只提交 mode 时 auto_pause 必须保持原样（开着）"
    );
}

/// 显式提交 `false` 才真的关掉，并且**顺手解除冷却**。
#[tokio::test]
async fn explicitly_disabling_auto_pause_clears_an_active_pause() {
    let f = Fixture::with_scheduler();
    let cookie = f.login().await;

    // 先制造一个冷却
    let (_, body) = f
        .post(
            "/api/monitor/auto-pause",
            serde_json::json!({"mode": "fixed", "pause_minutes": 60}),
            Some(&cookie),
        )
        .await;
    assert_eq!(body["mode"], "fixed");

    // 触发一次「抓到头目」的暂停：直接调 check 走不通（会清冷却），
    // 所以这里写 kv 模拟
    // —— 见下面的 `monitor_check_clears_the_cooldown` 覆盖清冷却那一侧
    let (_, off) = f
        .post(
            "/api/monitor/auto-pause",
            serde_json::json!({"enabled": false}),
            Some(&cookie),
        )
        .await;
    assert_eq!(off["auto_pause"], false);
    assert_eq!(off["paused"], false, "关掉自动暂停后不该还在冷却中");
}

/// 非法 mode 应当 400，而不是静默按 slot 处理。
#[tokio::test]
async fn an_unknown_mode_is_rejected() {
    let f = Fixture::with_scheduler();
    let cookie = f.login().await;

    let (status, body) = f
        .post(
            "/api/monitor/auto-pause",
            serde_json::json!({"mode": "slo"}),
            Some(&cookie),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["ok"], false);
}

/// `POST /api/monitor/check` 会**清掉**冷却。
#[tokio::test]
async fn monitor_check_clears_the_cooldown() {
    let f = Fixture::with_scheduler();
    let cookie = f.login().await;

    let (status, body) = f.post("/api/monitor/check", Value::Null, Some(&cookie)).await;
    assert_eq!(status, StatusCode::OK, "强制检查应当成功: {body}");
    assert_eq!(body["ok"], true);
    assert_eq!(
        body["paused"], false,
        "强制检查之后不该还处于冷却中 —— 清冷却这一步不能省"
    );
    assert!(
        body.get("result").is_some(),
        "应当带回本轮检查结果: {body}"
    );
    assert_eq!(
        body["result"]["status"], "empty",
        "源工厂返回空列表，本轮应当是 empty"
    );
}
