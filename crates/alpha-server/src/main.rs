//! `alpha-server` 二进制入口。
//!
//! # 启动顺序为什么是这样
//!
//! 1. 初始化日志 —— 后面每一步都可能失败，得先能看见原因
//! 2. 读配置并**校验密钥** —— 密钥不合格直接退出，不进入后面的步骤
//! 3. 开数据库、建目录
//! 4. 起 HTTP 服务
//!
//! 原版没有第 2 步（密钥有默认值），所以「配错了」这件事要到被人利用时
//! 才会发现。这里把它提到最前面 —— 配置错误是**启动期**错误，不是运行期错误。

use std::process::ExitCode;
use std::sync::Arc;

use alpha_server::{build_router, AppState, Config};

#[tokio::main]
async fn main() -> ExitCode {
    init_tracing();

    let config = match Config::from_env() {
        Ok(c) => c,
        Err(e) => {
            // 配置错误的报错要**完整、可读**，因为这是运维第一次部署时
            // 唯一会看到的东西。`{}` 而不是 `{:?}`：用户不需要看枚举结构。
            eprintln!("\n启动失败：配置有问题\n\n{e}\n");
            return ExitCode::FAILURE;
        }
    };

    if let Err(e) = config.ensure_dirs() {
        eprintln!("\n启动失败：无法创建数据目录\n\n{e}\n");
        return ExitCode::FAILURE;
    }

    let store = match alpha_store::Store::open(config.db_path()) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("\n启动失败：无法打开数据库 {}\n\n{e}\n", config.db_path().display());
            return ExitCode::FAILURE;
        }
    };

    // 建表 + 写入内置条目 + 迁移原版遗留的 `.py` 行。
    //
    // `Store::open` 只负责打开连接 —— 初始化是**显式的一步**，
    // 这样测试才能构造「未初始化」的库。之前这里漏了它：全新部署时
    // 数据库里一张表都没有，所有接口 500（no such table）；
    // 接管现网库时迁移也不会跑。两种情况都只有真机点开才会发现。
    //
    // 失败即退出：建表失败意味着磁盘或权限有问题，继续跑只会让
    // 每个请求都 500，报错反而更难懂。启动期错误就该在启动期暴露。
    if let Err(e) = store.init() {
        eprintln!("\n启动失败：数据库初始化失败\n\n{e}\n");
        return ExitCode::FAILURE;
    }

    let addr = format!("{}:{}", config.host, config.port);
    let listener = match tokio::net::TcpListener::bind(&addr).await {
        Ok(l) => l,
        Err(e) => {
            eprintln!("\n启动失败：无法监听 {addr}\n\n{e}\n");
            return ExitCode::FAILURE;
        }
    };

    let mut state = AppState::new(config, store);

    // 插件注册表：**无条件挂载**，与调度器解耦。
    //
    // 这里踩过一个真实的坑：这一版之前，registry 是在 `attach_scheduler`
    // 内部建的，然后只塞进调度器的依赖里 —— `AppState.registry` 永远是
    // `None`。后果是「决策器 / 评估器 / 调试台 / 头目报点」四个功能
    // 全部返回 500「插件系统未初始化」，而且**只有跑起来点进界面才会发现**
    // （单元测试自己会用 `with_registry` 建一个，所以一直是绿的）。
    //
    // 现在两者各自拿一份 registry（内部是 `Arc`，共享同一个扫描结果）。
    // 调度器装不上时，插件页照样能用。
    match state.attach_registry() {
        Ok(()) => tracing::info!(
            dir = %state.config.plugins_dir().display(),
            "插件注册表已挂载"
        ),
        Err(e) => eprintln!(
            "警告：插件目录不可用，决策器 / 评估器 / 调试台将不可用\n  {e}\n"
        ),
    }

    // 调度器：装不上的话**不退出**，只是面板少了自动轮询能力。
    //
    // 这条降级链是有意的：调度器起不来最常见的原因是配置文件坏了
    // （sources.yaml 语法错、rules.yaml 缺字段）。那种情况下最该能用的
    // 恰恰是面板 —— 用户要靠它看日志、改配置。直接退出等于把唯一
    // 的诊断入口也关掉了。
    match attach_scheduler(&state) {
        Ok(sched) => {
            let started = sched.spawn();
            tracing::info!(started, "调度器后台循环已启动");
            state.scheduler = Some(sched);
        }
        Err(e) => {
            eprintln!("警告：调度器初始化失败，将只提供面板功能（不会自动轮询）\n  {e}\n");
        }
    }

    let app = build_router(state);

    tracing::info!(%addr, "面板已启动");

    if let Err(e) = axum::serve(listener, app).await {
        tracing::error!(error = %e, "HTTP 服务异常退出");
        return ExitCode::FAILURE;
    }

    ExitCode::SUCCESS
}

/// 组装调度器依赖。**不修改** `state`，由调用方决定装不装得上。
fn attach_scheduler(
    state: &AppState,
) -> alpha_core::error::Result<Arc<alpha_scheduler::Scheduler>> {

    let plugins_dir = state.config.plugins_dir();
    let registry = Arc::new(
        alpha_plugin::PluginRegistry::new(&plugins_dir)
            .map_err(|e| alpha_core::error::CoreError::Config(format!("插件目录不可用: {e}")))?,
    );

    // 业务配置（源 / 规则 / 推送语言）与进程配置（端口 / 路径 / 密钥）分开：
    // 前者可能因配置文件出错而加载失败，后者已经在 `main` 开头校验过了
    let core_cfg = alpha_core::config::Config::load()?;

    // 去重状态文件：读 `config/settings.yaml` 的 `dedup.state_file`
    let dedup_path = {
        let p = std::path::PathBuf::from(&core_cfg.settings.dedup.state_file);
        if p.is_absolute() {
            p
        } else {
            alpha_core::config::project_root().join(p)
        }
    };
    let dedup = Arc::new(alpha_core::dedup::DedupStore::new(
        dedup_path,
        core_cfg.settings.dedup.keep,
    ));

    // 推送器：没有启用渠道时是否回退到环境变量 WxPusher。
    //
    // 原版是靠 `WXPUSHER_APP_TOKEN` 环境变量 + 配置里的 topic_ids 走一条
    // 独立的老路径。这里用 `wxpusher.enabled` 作为「允许兜底」的开关 ——
    // 它是用户能理解的说法（「没配渠道就用环境变量」），
    // 比再造一个 `legacy_fallback` 配置项清楚。
    let wx = &core_cfg.settings.notify.wxpusher;
    let notify = alpha_notify::Dispatcher::new(wx.enabled, wx.topic_ids.clone())
        .map_err(|e| alpha_core::error::CoreError::Config(format!("推送器初始化失败: {e}")))?;

    let deps = alpha_scheduler::LoopDeps {
        // `Store` 内部是 `Arc<Mutex<Connection>>`，克隆共享同一个连接
        store: Arc::new(state.store.clone()),
        config: Arc::new(core_cfg),
        pokedex: alpha_core::pokedex::get_pokedex()?,
        registry,
        dedup,
        // `RealNotifier` 拿一份 `Store` 的克隆（内部是 `Arc<Mutex<Connection>>`，
        // 与 `deps.store` 共享同一个连接）：它要在每个渠道推送完
        // 写一条日志，见该结构体的文档。
        notify: Arc::new(alpha_scheduler::poll::RealNotifier::new(
            Arc::new(notify),
            state.store.clone(),
        )),
        sources: Arc::new(alpha_scheduler::poll::real_sources()),
    };

    Ok(Arc::new(alpha_scheduler::Scheduler::new(deps)))
}

/// 日志：`RUST_LOG` 控制级别，默认 `info`。
///
/// 默认级别没有选 `debug`，因为那会把每个请求的会话解析都打出来，
/// 生产环境刷屏。排查时设 `RUST_LOG=alpha_server=debug` 即可。
fn init_tracing() {
    use tracing_subscriber::EnvFilter;

    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("alpha_server=info,tower_http=info,warn"));

    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(true)
        .init();
}
