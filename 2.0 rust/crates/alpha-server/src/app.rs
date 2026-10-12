//! 应用共享状态与路由器装配。
//!
//! # 为什么 `AppState` 是一个结构体而不是拆成多个 `State`
//!
//! axum 的 `State` 每个 handler 只能取一个，拆开的话 handler 签名会变成
//! `State<A>, State<B>, State<C>`，增删一个依赖要改所有 handler。
//! 收在一个 `AppState` 里，将来加东西不用动签名。
//!
//! 里面全是 `Arc` 或廉价可克隆的句柄，克隆成本可以忽略。

use std::sync::Arc;

use axum::extract::{Multipart, State};
use axum::routing::{get, get_service, post};
use axum::{Json, Router};

use crate::auth::layer::auth_middleware;
use crate::auth::session::SessionSigner;
use crate::config::Config;
use crate::error::ApiError;
use crate::routes;
use crate::routes::plugins::Side;

/// 全局共享状态。
#[derive(Clone)]
pub struct AppState {
    /// 启动配置（端口、路径、密钥校验结果）。
    pub config: Arc<Config>,
    /// 数据层。`Store` 内部是 `Arc<Mutex<Connection>>`，克隆是廉价的。
    pub store: alpha_store::Store,
    /// 会话签名器。
    pub signer: SessionSigner,
    /// 轮询调度器。
    ///
    /// `Option` 是为了让**测试**能构造一个不带调度器的 `AppState`
    /// （调度器要起后台任务、要真网络）。生产路径 `AppState::new`
    /// 永远会把它建出来。
    ///
    /// 用 `Arc<Scheduler>` 而不是直接持有：后台循环需要一份**独立的所有权**
    /// 才能移进 `tokio::spawn`，同时又得让 HTTP handler 拿到同一份
    /// （调 `start_debug` 要打到同一个 `DebugHandle` 上）。
    pub scheduler: Option<Arc<alpha_scheduler::Scheduler>>,

    /// 插件注册表（扫描 `data/plugins/` + 编译缓存）。
    ///
    /// 与调度器同样用 `Option`：测试可以不挂载它。
    /// 面板的插件页（上传 / 下载 / 模板 / 预览）都从它取文件。
    pub registry: Option<Arc<alpha_plugin::PluginRegistry>>,
}

impl std::fmt::Debug for AppState {
    /// 手写 `Debug`：`Config` 里有会话密钥，不能被打进日志。
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AppState")
            .field("config", &"<已隐藏（含密钥）>")
            .field("store", &"<Store>")
            .field("signer", &self.signer)
            .field("scheduler", &self.scheduler.is_some())
            .field("registry", &self.registry.is_some())
            .finish()
    }
}

impl AppState {
    pub fn new(config: Config, store: alpha_store::Store) -> Self {
        let signer = SessionSigner::new(config.secret.clone(), config.session_ttl);
        Self {
            config: Arc::new(config),
            store,
            signer,
            scheduler: None,
            registry: None,
        }
    }

    /// 挂上插件注册表。目录不存在会被创建。
    ///
    /// # 为什么返回 `Result` 而不是吞掉错误
    ///
    /// 之前这里是「失败就打条日志、`registry` 保持 `None`」。看着很稳，
    /// 实际是把一个**功能大面积不可用**的状态藏了起来：决策器、评估器、
    /// 调试台、头目报点四个功能会一起返回 500「插件系统未初始化」，
    /// 而启动日志里只有一行不太起眼的 error。
    ///
    /// 现在把错误交给调用方 —— `main` 会把它打成醒目的警告并说清
    /// 「哪些功能会不可用」。服务照样启动（面板的其它部分还能用来诊断），
    /// 但用户是**知道**的。
    pub fn attach_registry(&mut self) -> Result<(), alpha_plugin::PluginError> {
        let root = self.config.plugins_dir();
        let registry = alpha_plugin::PluginRegistry::new(&root)?;

        // 把内置插件（默认决策器 / 脚本队评估器）写成磁盘上的 .rhai 文件。
        //
        // 这里也漏过：物化只有测试在调，生产路径从来没有 —— 全新部署时
        // 数据库种子行指向 default_dispatcher.rhai，磁盘上却没有这个文件，
        // 调试台报「分发器文件不存在」。已存在则跳过（用户改过的版本
        // 不会被覆盖），所以每次启动都调是安全的。
        let written = registry.materialize_builtins()?;
        if !written.is_empty() {
            tracing::info!(
                files = ?written.iter().map(|p| p.display().to_string()).collect::<Vec<_>>(),
                "内置插件已物化"
            );
        }

        self.registry = Some(Arc::new(registry));
        Ok(())
    }

    /// 挂上插件注册表（测试便捷写法）。
    ///
    /// 测试里插件目录是刚建的，失败说明测试环境有问题而不是环境不兼容，
    /// 所以这里保留「失败即忽略」的旧行为，只是加了个 `expect` 态度的注释。
    /// 生产路径用 [`AppState::attach_registry`]。
    pub fn with_registry(mut self, root: impl AsRef<std::path::Path>) -> Self {
        match alpha_plugin::PluginRegistry::new(root) {
            Ok(r) => self.registry = Some(Arc::new(r)),
            Err(e) => {
                // 不 panic：面板的其它功能（源管理、日志、调度）都还能用，
                // 只是插件页会报「插件系统未初始化」。这比整个服务起不来好。
                tracing::error!(error = %e, "插件注册表初始化失败，插件页将不可用");
            }
        }
        self
    }
}

/// 前端静态资源的 `tower` 服务。
///
/// # 为什么分成「有目录」和「没目录」两条路
///
/// `ServeDir` 指向一个不存在的目录时，`/static/*` 会全部返回
/// **404 且响应体为空**。那是个很难排查的状态：浏览器控制台只说
/// 「加载 app.js 失败」，看不出是目录没搬过来还是路径写错了。
///
/// 所以目录不存在时换成一个明确的 500 + 说明文字 —— 和
/// [`crate::spa::missing_assets_page`] 是一套思路：让故障自证。
///
/// 正常情况下永远走 `ServeDir` 那一支。
fn static_service() -> axum::routing::MethodRouter {
    match crate::spa::static_dir() {
        Some(dir) => {
            tracing::info!(dir = %dir.display(), "前端静态目录已挂载");
            get_service(tower_http::services::ServeDir::new(dir))
        }
        None => {
            tracing::error!(
                "找不到前端静态目录（web/static）—— 面板界面将无法加载；\
                 API 不受影响。用 ALPHA_WEB_ROOT 指定前端根目录可修复。"
            );
            get(|| async {
                ApiError::Internal(
                    "前端静态资源缺失：服务器上找不到 web/static 目录。\
                     请把仓库的 web/ 目录部署到二进制旁边，或设置 ALPHA_WEB_ROOT。"
                        .into(),
                )
            })
        }
    }
}

/// 装配完整的应用路由器。
///
/// 中间件在这里挂 —— 这是**唯一**一处挂载点。原版那种「51 个装饰器
/// 分散在各处」的结构没法一眼看出鉴权是否完整覆盖。
pub fn build_router(state: AppState) -> Router {
    // 需要登录的接口
    let protected = Router::new()
        // 调度器（原版 `panel/app.py` 的 api_scheduler*）
        .route("/api/scheduler", get(routes::scheduler::get_scheduler))
        .route("/api/scheduler/set", post(routes::scheduler::set_scheduler))
        .route(
            "/api/scheduler/debug",
            get(routes::scheduler::debug_state).post(routes::scheduler::start_debug),
        )
        // 监控冷却
        .route("/api/monitor", get(routes::scheduler::get_monitor))
        .route(
            "/api/monitor/check",
            post(routes::scheduler::monitor_check),
        )
        .route(
            "/api/monitor/auto-pause",
            post(routes::scheduler::set_auto_pause),
        )
        // 头目源（原版 `panel/app.py` 的 api_source*）
        //
        // 注册顺序不影响正确性（axum 是静态路径优先于动态匹配），
        // 但把 `upload` / `template` 写在 `<name>` 之前更符合直觉 ——
        // 否则读代码的人得先确认一遍「upload 会不会被当成一个源名」。
        .route(
            "/api/sources",
            get(routes::sources::list_sources).post(routes::sources::add_source),
        )
        .route(
            "/api/sources/upload",
            post(routes::sources::upload_source),
        )
        .route("/api/sources/template", get(routes::sources::source_template))
        .route(
            "/api/sources/:name",
            axum::routing::delete(routes::sources::delete_source),
        )
        .route(
            "/api/sources/:name/enable",
            post(routes::sources::set_enable),
        )
        .route(
            "/api/sources/:name/priority",
            post(routes::sources::set_priority),
        )
        .route("/api/sources/:name/edit", post(routes::sources::edit_source))
        .route(
            "/api/sources/:name/download",
            get(routes::sources::download_source),
        )
        // 分发渠道（原版 `api_channels*`）
        //
        // `test-all` 必须排在 `:id` 之前吗？axum 里静态段优先，不用。
        // 但它是 `POST /api/channels/test-all`，而 `GET /api/channels`
        // 是另一条完全不同的方法，两者不冲突。
        .route(
            "/api/channels",
            get(routes::channels::list).post(routes::channels::add),
        )
        .route(
            "/api/channels/test-all",
            post(routes::channels::test_all),
        )
        .route(
            "/api/channels/:cid/edit",
            post(routes::channels::edit),
        )
        .route(
            "/api/channels/:cid/enable",
            post(routes::channels::set_enabled),
        )
        .route(
            "/api/channels/:cid/test",
            post(routes::channels::test_one),
        )
        .route(
            "/api/channels/:cid",
            axum::routing::delete(routes::channels::delete),
        )
        // 头目报点（原版 `api_boss_*`）
        .route("/api/boss/reports", get(routes::boss::reports))
        .route("/api/boss/dispatch", post(routes::boss::dispatch))
        // 调试台与首页（原版 `api_dashboard` / `api_pokedex` / `debug_run`）
        //
        // `/debug/run` **没有** `/api` 前缀 —— 原版就是挂在根上的，
        // 前端也是照这个路径调的。别「顺手统一」掉。
        .route("/api/dashboard", get(routes::debug::dashboard))
        .route("/api/pokedex", get(routes::debug::pokedex))
        .route("/debug/run", post(routes::debug::debug_run))
        // 日志（原版 `api_logs*`）
        .route("/api/logs", get(routes::logs::list))
        .route("/api/logs/clean", post(routes::logs::clean))
        .route("/api/logs/state", post(routes::logs::set_state))
        // 系统配置 / 关于（原版 `api_system*` / `api_about`）
        .route("/api/system", get(routes::system::get_system))
        .route("/api/system/save", post(routes::system::save_system))
        .route("/api/about", get(routes::system::about))
        // 决策器 / 评估器（原版 `api_dispatchers*` / `api_evaluators*`）
        //
        // 两张表结构完全相同、handler 也共用一套实现（见 routes::plugins），
        // 靠 `Side` 区分那四处差异。这里用闭包把 `Side` 绑进 handler ——
        // axum 的 handler 不能有额外的非提取器参数，闭包是标准做法。
        //
        // `template` / `upload` 写在 `:id` 之前：axum 里静态段本来就优先，
        // 但显式排序让读的人不用去查这个规则。
        .route(
            "/api/dispatchers",
            get(|st: State<AppState>| routes::plugins::list(st.0, Side::Dispatcher)),
        )
        .route(
            "/api/dispatchers/template",
            get(|| routes::plugins::template(Side::Dispatcher)),
        )
        .route(
            "/api/dispatchers/upload",
            post(
                |st: State<AppState>, mp: Multipart| {
                    routes::plugins::upload(st.0, Side::Dispatcher, mp)
                },
            ),
        )
        .route(
            "/api/dispatchers/:id/enable",
            post(
                |st: State<AppState>, axum::extract::Path(id): axum::extract::Path<i64>, b: Json<routes::plugins::EnableRequest>| {
                    routes::plugins::set_enabled(st.0, Side::Dispatcher, id, b.0)
                },
            ),
        )
        .route(
            "/api/dispatchers/:id/activate",
            post(
                |st: State<AppState>, axum::extract::Path(id): axum::extract::Path<i64>| {
                    routes::plugins::activate(st.0, Side::Dispatcher, id)
                },
            ),
        )
        .route(
            "/api/dispatchers/:id/edit",
            post(
                |st: State<AppState>, axum::extract::Path(id): axum::extract::Path<i64>, b: Json<routes::plugins::EditRequest>| {
                    routes::plugins::edit(st.0, Side::Dispatcher, id, b.0)
                },
            ),
        )
        .route(
            "/api/dispatchers/:id/delete",
            post(
                |st: State<AppState>, axum::extract::Path(id): axum::extract::Path<i64>| {
                    routes::plugins::delete(st.0, Side::Dispatcher, id)
                },
            ),
        )
        .route(
            "/api/dispatchers/:id/download",
            get(
                |st: State<AppState>, axum::extract::Path(id): axum::extract::Path<i64>| {
                    routes::plugins::download(st.0, Side::Dispatcher, id)
                },
            ),
        )
        // ---- 评估器（同上，只多一条 preview） ----
        .route(
            "/api/evaluators",
            get(|st: State<AppState>| routes::plugins::list(st.0, Side::Evaluator)),
        )
        .route(
            "/api/evaluators/template",
            get(|| routes::plugins::template(Side::Evaluator)),
        )
        .route(
            "/api/evaluators/upload",
            post(
                |st: State<AppState>, mp: Multipart| {
                    routes::plugins::upload(st.0, Side::Evaluator, mp)
                },
            ),
        )
        .route(
            "/api/evaluators/preview",
            post(routes::plugins::preview),
        )
        .route(
            "/api/evaluators/:id/enable",
            post(
                |st: State<AppState>, axum::extract::Path(id): axum::extract::Path<i64>, b: Json<routes::plugins::EnableRequest>| {
                    routes::plugins::set_enabled(st.0, Side::Evaluator, id, b.0)
                },
            ),
        )
        .route(
            "/api/evaluators/:id/activate",
            post(
                |st: State<AppState>, axum::extract::Path(id): axum::extract::Path<i64>| {
                    routes::plugins::activate(st.0, Side::Evaluator, id)
                },
            ),
        )
        .route(
            "/api/evaluators/:id/edit",
            post(
                |st: State<AppState>, axum::extract::Path(id): axum::extract::Path<i64>, b: Json<routes::plugins::EditRequest>| {
                    routes::plugins::edit(st.0, Side::Evaluator, id, b.0)
                },
            ),
        )
        .route(
            "/api/evaluators/:id/delete",
            post(
                |st: State<AppState>, axum::extract::Path(id): axum::extract::Path<i64>| {
                    routes::plugins::delete(st.0, Side::Evaluator, id)
                },
            ),
        )
        .route(
            "/api/evaluators/:id/download",
            get(
                |st: State<AppState>, axum::extract::Path(id): axum::extract::Path<i64>| {
                    routes::plugins::download(st.0, Side::Evaluator, id)
                },
            ),
        );

    // 放行名单（与 `auth::layer::is_public` 必须一一对应）
    let public = Router::new()
        // 前端入口（原版 `@app.route("/")`）。
        //
        // 它必须真的**注册**一条路由，而不是只靠中间件里的 `spa_entry()`：
        // 中间件对未登录的非 API 路径确实会返回入口页，但已登录的 `/`
        // 会一路走到路由表 —— 没有这条就是 404。而且 `is_public` 里
        // 把 `GET /` 列为放行，本意就是「未登录也能打开」，所以
        // 让它在两条路径下都返回同一份 HTML 才是对的。
        //
        // 包一层闭包是因为 `spa_entry()` 直接返回 `Response` ——
        // 它是给中间件调的，签名不符合 axum 的 `Handler`
        // （handler 要么返回 `impl IntoResponse`，要么接提取器参数；
        // 一个零参、返回 `Response` 的普通函数两者都不是）。
        .route("/", get(|| async { crate::auth::layer::spa_entry() }))
        // 前端静态资源（`web/static/`）。
        //
        // # 为什么它必须挂在 `public` 这一侧
        //
        // 登录页自己要用 `style.css` / `app.js` —— 如果静态资源也要求登录，
        // 未登录用户拿到的是一坨 HTML（`spa_entry` 的降级行为），
        // 然后浏览器把 `text/html` 当 JS 解析，登录页永远渲染不出来。
        // 这正好是「死锁式」故障：**要登录才能拿到登录所需的东西**。
        //
        // # 为什么用 `nest_service` 而不是 `.route("/static/*", ...)`
        //
        // `ServeDir` 是 `tower` 的 `Service`，不是 axum 的 `Handler`。
        // 它自己会处理路径前缀剥离、目录穿越防护、Range 请求、ETag、
        // `Content-Type` 推断 —— 这些自己写都容易漏（尤其是目录穿越）。
        .nest_service("/static", static_service())
        .route("/api/me", get(routes::auth::me))
        .route("/api/login", post(routes::auth::login))
        .route("/api/logout", post(routes::auth::logout))
        .route("/api/lang", post(routes::auth::set_lang));

    let signer = state.signer.clone();

    public
        .merge(protected)
        .layer(axum::middleware::from_fn_with_state(
            signer,
            auth_middleware,
        ))
        .with_state(state)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    /// 构造一份测试用配置。密码相关字段走真实的校验路径之外，
    /// 是为了让测试不必去动进程级环境变量（那在多线程测试里是竞态源）。
    pub fn test_config(dir: &std::path::Path) -> Config {
        Config {
            host: "127.0.0.1".into(),
            port: 0,
            secret: "test-secret-value-at-least-32-bytes-long".into(),
            data_dir: dir.to_path_buf(),
            config_dir: dir.to_path_buf(),
            session_ttl: 3600,
            envs: HashMap::new(),
        }
    }

    fn make_state(dir: &std::path::Path) -> (Config, AppState) {
        let cfg = test_config(dir);
        let store = alpha_store::Store::open(cfg.db_path()).unwrap();
        let state = AppState::new(cfg.clone(), store);
        (cfg, state)
    }

    #[test]
    fn app_state_debug_hides_the_secret() {
        let dir = tempfile::tempdir().unwrap();
        let (_cfg, state) = make_state(dir.path());

        let text = format!("{state:?}");
        assert!(
            !text.contains("test-secret-value-at-least-32-bytes-long"),
            "密钥泄漏到 Debug 输出里了: {text}"
        );
        assert!(text.contains("已隐藏"));
    }

    #[test]
    fn router_assembles_without_panicking() {
        let dir = tempfile::tempdir().unwrap();
        let (_cfg, state) = make_state(dir.path());
        let _router = build_router(state);
    }

    // ---- 插件注册表挂载 ----

    /// `attach_registry` 必须真的把注册表挂上。
    ///
    /// **这条是实测发现的生产缺陷的回归测试。** 之前 registry 是在
    /// `main` 的 `attach_scheduler` 里建的，只塞进调度器依赖，
    /// `AppState.registry` 永远是 `None` —— 生产环境里
    /// 「决策器 / 评估器 / 调试台 / 头目报点」四个功能全部 500。
    ///
    /// 之所以一直没被测出来：**所有端到端测试都自己调 `with_registry`**，
    /// 于是测试路径有注册表、生产路径没有，两边行为不同。
    /// 这条用例专门盯着「生产路径用的那个方法」。
    #[test]
    fn attach_registry_makes_the_plugin_routes_usable() {
        let dir = tempfile::tempdir().unwrap();
        let (_cfg, mut state) = make_state(dir.path());

        assert!(
            state.registry.is_none(),
            "新构造的 AppState 不该自带注册表（要由 main 显式挂）"
        );

        state
            .attach_registry()
            .expect("插件目录不该建不出来");

        assert!(
            state.registry.is_some(),
            "attach_registry 之后 registry 还是 None —— \
             生产环境里决策器/评估器/调试台会全部 500"
        );
    }

    /// 插件目录不可用时要返回错误，**不能**静默通过。
    ///
    /// 静默的话调用方无从知道「刚才那步没成功」，只能等用户点进界面
    /// 看到 500。错误必须能被拿到。
    #[test]
    fn attach_registry_reports_failure_instead_of_hiding_it() {
        let dir = tempfile::tempdir().unwrap();
        let (_cfg, mut state) = make_state(dir.path());

        // 把「插件目录」指到一个文件上：`create_dir_all` 会失败
        let blocker = dir.path().join("not-a-dir");
        std::fs::write(&blocker, b"x").unwrap();
        let mut cfg = test_config(dir.path());
        cfg.data_dir = blocker.clone();
        state.config = Arc::new(cfg);

        assert!(
            state.attach_registry().is_err(),
            "插件目录不可用时必须返回错误，让 main 能打出警告"
        );
        assert!(state.registry.is_none(), "失败了就不该挂上半成品");
    }
}
