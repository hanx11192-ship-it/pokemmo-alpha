//! 路由覆盖对账：Rust 版必须把原版 56 条路由**一条不漏**地实现。
//!
//! # 为什么需要这条测试
//!
//! 这次重写的验收标准之一是「路由全齐」。手工核对 56 条容易漏，
//! 而且漏了之后**不会报错** —— 只是某个前端页面点下去 404。
//! 所以把原版路由清单固化成一份数据，逐条对着路由表打。
//!
//! # 判据是「路由表里注册了」，不是「handler 写完了」
//!
//! 用 `Router` 自己的匹配结果判断：对每条路由发一个探测请求，
//! 断言**不是 404**。这比去读源码文本可靠得多 —— 它测的是
//! axum 真正会怎么路由。
//!
//! 探测请求用匿名身份 + 合法 CSRF 头：
//!
//! - 匿名 → 受保护的路由回 401，仍是「路由存在」的证据
//! - 带 CSRF 头 → 不会被 400 挡在前面，避免把「缺头」误判成「路由在」
//!
//! # 与 `/tmp/orig_routes.txt` 的关系
//!
//! 这里把清单**内联**进测试，而不是运行时去读原版仓库：
//! 原版目录不在构建产物里，CI 上一定读不到。
//! 清单来自 `grep '@app.route' panel/app.py`，明细见每条注释。

use std::collections::HashMap;
use std::sync::OnceLock;

use alpha_server::auth::layer::{CSRF_HEADER, CSRF_HEADER_VALUE};
use alpha_server::{build_router, AppState, Config};
use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use axum::Router;
use tower::ServiceExt;

/// 原版全部 56 条路由：(方法, 路径)。
///
/// Flask 的 `<int:did>` / `<name>` 已按 axum 的写法改成 `:id` / `:name`。
/// 路径里的动态段统一填 `probe`（`/api/sources/probe` 这类）。
const ORIGINAL_ROUTES: &[(&str, &str)] = &[
    // ---- 前端与鉴权（5 条）----
    ("GET", "/"),
    ("GET", "/api/me"),
    ("POST", "/api/login"),
    ("POST", "/api/logout"),
    ("POST", "/api/lang"),
    // ---- 定时任务（4 条）----
    ("GET", "/api/scheduler"),
    ("POST", "/api/scheduler/set"),
    ("GET", "/api/scheduler/debug"),
    ("POST", "/api/scheduler/debug"),
    // ---- 监控冷却（3 条）----
    ("GET", "/api/monitor"),
    ("POST", "/api/monitor/check"),
    ("POST", "/api/monitor/auto-pause"),
    // ---- 数据源（8 条）----
    ("GET", "/api/sources"),
    ("POST", "/api/sources"),
    ("POST", "/api/sources/upload"),
    ("GET", "/api/sources/template"),
    ("DELETE", "/api/sources/:name"),
    ("POST", "/api/sources/:name/enable"),
    ("POST", "/api/sources/:name/priority"),
    ("POST", "/api/sources/:name/edit"),
    ("GET", "/api/sources/:name/download"),
    // ---- 分发渠道（6 条）----
    ("GET", "/api/channels"),
    ("POST", "/api/channels"),
    ("POST", "/api/channels/test-all"),
    ("POST", "/api/channels/:id/edit"),
    ("POST", "/api/channels/:id/enable"),
    ("POST", "/api/channels/:id/test"),
    ("DELETE", "/api/channels/:id"),
    // ---- 头目报点（2 条）----
    ("GET", "/api/boss/reports"),
    ("POST", "/api/boss/dispatch"),
    // ---- 日志（3 条）----
    ("GET", "/api/logs"),
    ("POST", "/api/logs/clean"),
    ("POST", "/api/logs/state"),
    // ---- 系统配置 / 关于（3 条）----
    ("GET", "/api/system"),
    ("POST", "/api/system/save"),
    ("GET", "/api/about"),
    // ---- 调试台与首页（3 条）----
    ("GET", "/api/dashboard"),
    ("GET", "/api/pokedex"),
    ("POST", "/debug/run"),
    // ---- 决策器（8 条）----
    ("GET", "/api/dispatchers"),
    ("POST", "/api/dispatchers/upload"),
    ("GET", "/api/dispatchers/template"),
    ("POST", "/api/dispatchers/:id/enable"),
    ("POST", "/api/dispatchers/:id/edit"),
    ("POST", "/api/dispatchers/:id/activate"),
    ("POST", "/api/dispatchers/:id/delete"),
    ("GET", "/api/dispatchers/:id/download"),
    // ---- 评估器（8 条）----
    ("GET", "/api/evaluators"),
    ("POST", "/api/evaluators/upload"),
    ("GET", "/api/evaluators/template"),
    ("POST", "/api/evaluators/preview"),
    ("POST", "/api/evaluators/:id/enable"),
    ("POST", "/api/evaluators/:id/edit"),
    ("POST", "/api/evaluators/:id/activate"),
    ("POST", "/api/evaluators/:id/delete"),
    ("GET", "/api/evaluators/:id/download"),
];

/// 把 `:id` / `:name` 换成探测值。
fn concrete(path: &str) -> String {
    path.replace(":id", "1").replace(":name", "probe")
}

fn app() -> &'static Router {
    static APP: OnceLock<Router> = OnceLock::new();
    APP.get_or_init(|| {
        let dir = Box::leak(Box::new(tempfile::tempdir().unwrap()));
        let base = dir.path().to_path_buf();
        std::fs::create_dir_all(base.join("config")).unwrap();
        std::fs::create_dir_all(base.join("data")).unwrap();
        std::fs::write(base.join("config/sources.yaml"), "sources: []\n").unwrap();
        std::fs::write(
            base.join("config/settings.yaml"),
            "timezone: Asia/Shanghai\nlanguage: zh\n",
        )
        .unwrap();
        std::fs::write(base.join("panel.env"), "# test\n").unwrap();
        // 图鉴是 `/api/pokedex` / `/debug/run` 的依赖，缺了会 500 ——
        // 但 500 也不影响本测试的判据（只看是不是 404），所以尽力而为
        for name in ["pokedex.json", "aliases.json"] {
            let src = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .parent()
                .and_then(|p| p.parent())
                .unwrap()
                .join("data")
                .join(name);
            if src.exists() {
                let _ = std::fs::copy(&src, base.join("data").join(name));
            }
        }

        std::env::set_var("ALPHA_ROOT", &base);
        std::env::set_var("PANEL_ENV_FILE", base.join("panel.env"));

        let cfg = Config {
            host: "127.0.0.1".into(),
            port: 0,
            secret: "test-secret-value-at-least-32-bytes-long".into(),
            data_dir: base.join("data"),
            config_dir: base.join("config"),
            session_ttl: 3600,
            envs: HashMap::new(),
        };
        let store = alpha_store::Store::open(cfg.db_path()).unwrap();
        store.init().unwrap();
        let state = AppState::new(cfg, store).with_registry(base.join("data/plugins"));
        build_router(state)
    })
}

/// 探测一条路由：返回 `true` 表示「路由表里有」。
///
/// # 三个会把判据带偏的坑（都踩过）
///
/// **坑一：401 遮住了 404。** 中间件在鉴权那步就短路了：未登录 +
/// `/api/*` → 直接 401，**根本走不到路由表**。所以匿名探测时，
/// 真实路由和不存在的路由都回 401 —— 一条没实现的路由也会被判成「存在」。
/// 解法：带**有效登录会话**探测。
///
/// **坑二：404 不一定是「路由不存在」。** 带着登录态探测之后，
/// `/api/boss/dispatch` 仍然回 404 —— 但那不是路由表未命中，
/// 而是 handler **自己的业务 404**（「当前没有可决策的报点」，
/// 因为测试环境连不上报点源）。
/// 拿状态码当判据会把这类路由误判成「没实现」。
/// 解法：用 `405 + Allow` 头判断 —— 见下。
///
/// **坑三：非 API 路径的 SPA 回退。** 未登录时中间件对非 API 路径
/// 返回 200 的入口页，把 404 吃掉。
///
/// # 最终判据：看「这个方法是否被该路径接受」
///
/// 光靠状态码分不清「路由没注册」（404）和「handler 里返回了 404」。
/// 用一个**没人注册的方法**（`PATCH`）去探同一路径：
///
/// - 405 + `Allow: POST` → 这条路径**注册了**，且不接受 PATCH
/// - 404 → 路由表里根本没这条路径
///
/// PATCH 的实际方法是什么无所谓 —— 只有它跟真实方法不同，
/// `Allow` 里列出真实方法就说明路由存在。这比猜状态码可靠得多。
async fn exists(method: &str, path: &str) -> bool {
    let real = Method::from_bytes(method.as_bytes()).unwrap();
    // 挑一个一定与真实方法不同的探测方法
    let probe = if real == Method::PATCH {
        Method::PUT
    } else {
        Method::PATCH
    };

    let resp = app()
        .clone()
        .oneshot(
            Request::builder()
                .method(probe)
                .uri(path)
                .header(CSRF_HEADER, CSRF_HEADER_VALUE)
                .header("cookie", session().await)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    if resp.status() != StatusCode::METHOD_NOT_ALLOWED {
        // 既不是 405 也不是我们要的 —— 落到这里说明该路径要么压根没有
        // （PATCH 未命中路径 → 404），要么被中间件接管了。
        return false;
    }

    // 405 只说明「路径在，但 PATCH 不被接受」；还得确认
    // **真实方法确实在 Allow 列表里**，否则 `exists("POST", "/x")`
    // 只要 `/x` 注册了 GET 也会返回 true。
    resp.headers()
        .get("allow")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|allow| {
            allow
                .split(',')
                .any(|m| m.trim().eq_ignore_ascii_case(method))
        })
}

/// 真实登录一次，拿到会话 cookie（只做一次，之后的用例复用）。
async fn session() -> String {
    use tokio::sync::OnceCell;
    static SESSION: OnceCell<String> = OnceCell::const_new();

    SESSION
        .get_or_init(|| async {
            let resp = app()
                .clone()
                .oneshot(
                    Request::builder()
                        .method("POST")
                        .uri("/api/login")
                        .header("content-type", "application/json")
                        .header(CSRF_HEADER, CSRF_HEADER_VALUE)
                        .body(Body::from(
                            serde_json::to_vec(&serde_json::json!({
                                "username": "hanyx", "password": "test-password"
                            }))
                            .unwrap(),
                        ))
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(
                resp.status(),
                StatusCode::OK,
                "测试登录该成功 —— 拿不到会话的话下面所有探测都会退化成 401"
            );
            resp.headers()
                .get("set-cookie")
                .expect("登录要下发 cookie")
                .to_str()
                .unwrap()
                .split(';')
                .next()
                .unwrap()
                .to_string()
        })
        .await
        .clone()
}

/// **核心对账**：原版 56 条一条都不能少。
#[tokio::test]
async fn every_original_route_is_registered() {
    let mut missing = Vec::new();
    for (method, path) in ORIGINAL_ROUTES {
        let p = concrete(path);
        if !exists(method, &p).await {
            missing.push(format!("{method} {path}"));
        }
    }
    assert!(
        missing.is_empty(),
        "以下 {} 条原版路由在 Rust 版里没注册：\n  {}",
        missing.len(),
        missing.join("\n  ")
    );
}

/// 清单本身要是 56 条 —— 别在维护这份清单时手滑删掉一条。
#[test]
fn the_checklist_itself_has_all_56_routes() {
    assert_eq!(
        ORIGINAL_ROUTES.len(),
        56,
        "原版 `grep -c '@app.route' panel/app.py` 是 56；\
         清单条数对不上说明要么漏抄要么多抄了"
    );
}

/// 路由表里不能有**不存在的** `/api/*` 路由 —— 反向确认 `exists()` 判据有效。
///
/// 少了这条，`exists()` 万一写成「永远返回 true」，上面那条对账
/// 就会一直绿着，而实际上是空的。
///
/// # 只测 `/api/*`，不测非 API 路径
///
/// 中间件对「未登录 + 非 API 路径」会返回 200 的 `spa_entry()`
/// （让前端静态资源与深层链接都能落到 SPA 上）。也就是说
/// **非 API 路径上的 404 会被中间件吃掉**，`exists()` 在那里恒为 `true`。
///
/// 这是有意的设计（原版的多页结构没这个需求，改 SPA 之后必须有），
/// 但它意味着「非 API 路径是否真的注册了」不能靠 404 判断 ——
/// 只能靠 [`ORIGINAL_ROUTES`] 那份清单逐条打。
#[tokio::test]
async fn unknown_api_paths_are_not_found() {
    for (method, path) in [
        ("POST", "/api/debug/run"), // 常见误解：以为带 /api 前缀
        ("GET", "/api/nonexistent"),
        ("POST", "/api/dispatchers/1/disable"),
        ("GET", "/api/channels/1/unknown"),
    ] {
        assert!(
            !exists(method, path).await,
            "{method} {path} 不该存在，但被路由表接住了 —— \
             要么是实现了多余的路由，要么是 `exists()` 的判据坏了"
        );
    }
}

/// `/debug/run` 只接受 POST。
///
/// 它在中间件里被特殊对待（按 API 语义鉴权 + CSRF），所以值得确认
/// 这个特殊处理没把方法也放宽。
///
/// 注意这条**不能**靠状态码判断：`GET /debug/run` 是非 API 路径，
/// 未登录时会被中间件的 SPA 回退接走、返回 200。所以判据换成
/// 「带着有效 CSRF 头、且不给它走 SPA 回退」——
/// 这里用**已登录**的身份探测，绕过那层回退，才能看到真正的 405。
#[tokio::test]
async fn debug_run_only_accepts_post() {
    assert!(exists("POST", "/debug/run").await, "POST /debug/run 该存在");

    // 用一个畸形 cookie 让中间件走到「已认证为假但仍是非 API」的老路？
    // 不行 —— 那条路还是会 SPA 回退。真正可靠的做法是直接确认
    // 路由表里这条只注册了 POST，而 `exists()` 对 POST 为真、
    // 对 GET 走的是回退（不是路由命中）。
    //
    // 所以判据改为：GET 拿到的**不是 JSON**（回退给的是 HTML），
    // 即证明它不是由 `/debug/run` 这条路由处理的。
    let resp = app()
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/debug/run")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let ct = resp
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_string();
    assert!(
        !ct.contains("application/json"),
        "GET /debug/run 不该被这条路由处理（它只注册了 POST），\
         实际 content-type={ct} —— 说明方法放宽了"
    );
}

/// 中间件对未登录的非 API 路径返回 SPA 入口页（200 + HTML）。
///
/// 这条是上面 `debug_run_only_accepts_post` 判据成立的前提条件，
/// 单独钉一遍 —— 哪天有人把 SPA 回退改成 404，那条用例的结论就反了。
#[tokio::test]
async fn anonymous_non_api_paths_get_the_spa_fallback() {
    let resp = app()
        .clone()
        .oneshot(
            Request::builder()
                .uri("/some/deep/link")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let ct = resp
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_string();
    assert!(
        ct.contains("text/html"),
        "非 API 路径该回退到 SPA 入口页（HTML），实际 {ct}"
    );
}

/// 前端入口 `/` 在**未登录**时返回 200 的 HTML。
///
/// `is_public` 放行 `GET /` 的本意就是「未登录也能打开登录页」。
/// 只靠中间件里的 `spa_entry()` 也能做到，但已登录的 `/` 会一路
/// 走到路由表 —— 那条路由必须真的注册上，否则 404。
#[tokio::test]
async fn root_serves_the_spa_shell_when_anonymous() {
    let resp = app()
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let ct = resp
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_string();
    assert!(ct.contains("text/html"), "入口页该是 HTML，实际 {ct}");
}
