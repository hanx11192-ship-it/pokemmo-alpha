//! Rhai 沙箱。
//!
//! # 安全边界（相对原版的核心改进）
//!
//! 原版用 `importlib` 直接 `exec_module()` 用户上传的 `.py`：
//! **上传一个文件就等于拿到了服务端任意代码执行权** —— 可以读
//! `panel.env` 里的 token、可以 `os.system("rm -rf /")`、可以开反向 shell。
//! 只要面板的「上传插件」入口对某些人开放，这就是一条完整的 RCE 链。
//!
//! Rhai 沙箱把能力收窄到了「算数 + 字符串 + 容器」：
//!
//! | 能力 | 处置 |
//! |---|---|
//! | 文件 / 网络 / 进程 / 环境变量 | **不注册任何 API**，脚本里根本调不到 |
//! | `eval` / 动态 import | 关闭 |
//! | 无限循环 | 指令数上限，超了直接中断 |
//! | 深递归 / 超大分配 | 调用深度与表达式深度上限 |
//! | 模块互相引用 | 不注册模块解析器，无法 import |
//!
//! # 超时为什么用指令数而不是墙钟时间
//!
//! 墙钟超时需要一个看门狗线程去打断另一个线程，而 Rhai 的执行是同步的、
//! 没法安全地从外部 kill。`max_operations` 是在解释器循环里检查的计数器，
//! 没有这个限制，一个 `while true {}` 就能让整个面板卡死。
//!
//! 代价是「指令数」和「真实耗时」不成严格正比（一条指令可能做很重的字符串
//! 操作），但作为「防失控」的兜底已经足够，而且天然可复现（同样的脚本
//! 同样的输入，一定跑到同一个数）—— 这点对回归测试很重要。

use std::path::{Path, PathBuf};
use std::sync::Arc;

use rhai::{Dynamic, Engine, Map, Scope, AST};

use crate::ast::BossView;
use crate::error::{PluginError, PluginResult};
use crate::eval::Evaluation;
use crate::manifest::Manifest;

/// 单个插件允许执行的指令数上限。
///
/// 取 200 万：正常的评估器（几十条规则、最多遍历 4 个技能）实测在
/// 1 万条以内，留两个数量级的余量，既够用又能挡住死循环。
/// 按本机（2 核）实测，这个上限对应大约 0.1–0.3 秒。
pub const DEFAULT_MAX_OPERATIONS: u64 = 2_000_000;

/// 表达式嵌套深度上限（防 `((((...))))` 撑爆栈）。
pub const DEFAULT_MAX_EXPR_DEPTH: usize = 64;

/// 函数调用深度上限（防无界递归）。
pub const DEFAULT_MAX_CALL_DEPTH: usize = 32;

/// 字符串长度上限（防分配爆炸）。
pub const DEFAULT_MAX_STRING_SIZE: usize = 64 * 1024;

/// 数组 / Map 元素数上限。
pub const DEFAULT_MAX_ARRAY_SIZE: usize = 4096;

/// 插件类别 —— 决定调用哪个入口函数。
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PluginKind {
    /// 决策器：`fn dispatch(boss, ctx) -> String`
    Dispatcher,
    /// 评估器：`fn evaluate(boss, ctx) -> Map | ()`
    Evaluator,
}

impl PluginKind {
    /// 入口函数名。
    pub fn entry(&self) -> &'static str {
        match self {
            Self::Dispatcher => "dispatch",
            Self::Evaluator => "evaluate",
        }
    }

    /// 面板里的人类可读名。
    pub fn label(&self) -> &'static str {
        match self {
            Self::Dispatcher => "决策器",
            Self::Evaluator => "评估器",
        }
    }

    /// 数据库里对应的 `filename` 后缀习惯（仅用于展示与猜测）。
    pub fn ext(&self) -> &'static str {
        "rhai"
    }
}

/// 已加载的插件。
///
/// `AST` 只编译一次，之后可以反复调用 —— 原版每次 `exec_module()` 都要
/// 重新解析整个文件，面板每 60 秒轮询就重编译一次，纯浪费。
pub struct Plugin {
    engine: Arc<Engine>,
    ast: AST,
    kind: PluginKind,
    /// 脚本头部注释里声明的元信息（没有就用文件里的默认值）
    manifest: Manifest,
    /// 源文件路径（内存内建插件为 None）
    path: Option<PathBuf>,
    /// 源码（面板「查看源码」要用；也用于把内建插件写盘）
    source: String,
}

impl std::fmt::Debug for Plugin {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Plugin")
            .field("kind", &self.kind)
            .field("name", &self.manifest.name)
            .field("path", &self.path)
            .finish_non_exhaustive()
    }
}

impl Plugin {
    /// 从源码编译一个插件。
    ///
    /// `kind` 决定入口函数名，也决定要不要注册 `engine_report`
    /// （只有决策器需要把活儿交回 Rust 引擎）。
    pub fn compile(
        source: &str,
        kind: PluginKind,
        path: Option<PathBuf>,
    ) -> PluginResult<Self> {
        let engine = Arc::new(build_engine(kind));

        let ast = engine
            .compile(source)
            .map_err(|e| PluginError::Compile(e.to_string()))?;

        // 编译通过 ≠ 能用：必须真的实现了入口函数。
        // 这里在加载期就拦住，而不是等第一次推送时才发现 —— 那时头目可能已经
        // 因为「插件不可用」而被跳过，用户还以为是数据源的问题。
        if !ast.iter_functions().any(|f| f.name == kind.entry()) {
            return Err(PluginError::MissingEntry(kind.entry()));
        }

        let manifest = Manifest::parse(source, &ast);

        Ok(Self {
            engine,
            ast,
            kind,
            manifest,
            path,
            source: source.to_string(),
        })
    }

    /// 从文件加载。
    pub fn load_file(path: impl AsRef<Path>, kind: PluginKind) -> PluginResult<Self> {
        let path = path.as_ref().to_path_buf();
        let source = std::fs::read_to_string(&path).map_err(|e| PluginError::Io {
            path: path.display().to_string(),
            source: e,
        })?;
        Self::compile(&source, kind, Some(path))
    }

    pub fn manifest(&self) -> &Manifest {
        &self.manifest
    }

    pub fn source(&self) -> &str {
        &self.source
    }

    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    pub fn kind(&self) -> PluginKind {
        self.kind
    }

    /// 调用 `dispatch(boss, ctx)`，返回推送文本。
    pub fn dispatch(&self, boss: &BossView, ctx: &Context) -> PluginResult<String> {
        assert_eq!(self.kind, PluginKind::Dispatcher, "决策器插件才能 dispatch");
        let (boss_m, ctx_m) = (crate::ast::boss_to_rhai(boss), ctx.to_rhai());
        let d = self.call_dynamic(&boss_m, &ctx_m, ctx.report.clone(), ctx.evaluator.clone())?;
        let ty = d.type_name().to_string();
        d.try_cast::<String>().ok_or_else(|| {
            PluginError::Runtime(format!("dispatch 必须返回字符串，实际返回了 {ty}"))
        })
    }

    /// 调用 `evaluate(boss, ctx)`，返回评估结果。
    ///
    /// 脚本返回 `()`（unit）表示「这只头目不评估」，与原版 `return None` 对应。
    pub fn evaluate(&self, boss: &BossView, ctx: &Context) -> PluginResult<Option<Evaluation>> {
        assert_eq!(self.kind, PluginKind::Evaluator, "评估器插件才能 evaluate");
        let (boss_m, ctx_m) = (crate::ast::boss_to_rhai(boss), ctx.to_rhai());

        // 入口可能返回 Map 也可能返回 unit，所以要按 Dynamic 收再判断
        let raw = self.call_dynamic(&boss_m, &ctx_m, None, None)?;
        if raw.is_unit() {
            return Ok(None);
        }
        let ty = raw.type_name().to_string();
        let m = raw.try_cast::<Map>().ok_or_else(|| {
            PluginError::Runtime(format!("evaluate 必须返回 map 或 ()，实际返回了 {ty}"))
        })?;
        Ok(Some(Evaluation::from_map(&m)))
    }

    /// 实际执行。
    ///
    /// 每次都用全新的 `Scope`，脚本之间不可能共享状态。
    ///
    /// `report` / `evaluator` 由 [`set_hooks`] 写进引擎可见的线程局部状态，
    /// 执行完立刻清掉 —— 避免一次调用的数据泄漏到下一次。
    fn call_dynamic(
        &self,
        boss: &Map,
        ctx: &Map,
        report: Option<ReportFn>,
        evaluator: Option<EvaluatorFn>,
    ) -> PluginResult<Dynamic> {
        let mut scope = Scope::new();

        // 装上钩子（报告给决策器、评估器给决策器与调试台）
        let guard = set_hooks(report, evaluator);
        let result = self.engine.call_fn(
            &mut scope,
            &self.ast,
            self.kind.entry(),
            (boss.clone(), ctx.clone()),
        );
        // 无论成败都要卸掉，否则并发调用会互相看到对方的钩子
        drop(guard);

        result.map_err(map_rhai_err)
    }
}

/// 报告生成器的**闭包**类型。
///
/// 抽成别名有两个原因：一是 `Context` 与 `thread_local` 都用它，
/// 写全了很长；二是它是「决策器插件能把活儿交回引擎」这条契约的类型载体，
/// 值得有个名字。
pub type ReportCallback = dyn Fn(&BossView, &str) -> String + Send + Sync;

/// 报告生成器（可跨线程共享、可克隆）。
pub type ReportFn = std::sync::Arc<ReportCallback>;

/// 评估器回调的**闭包**类型。
///
/// 入参是 `BossView` 与输出语言，返回「评价 + 评分」那一行的文本
/// （空串 = 这只头目不评估）。
pub type EvaluatorCallback = dyn Fn(&BossView, &str) -> String + Send + Sync;

/// 评估器回调（可跨线程共享、可克隆）。
///
/// # 为什么是 `Arc<dyn Fn>` 而不是 `Box<dyn Fn>`
///
/// 和 [`ReportFn`] 同一个理由，但这里还多一层：调试台一次请求里，
/// 「决策器 → 引擎 → 评估行」这条链上会用到同一份回调，
/// 而 `Arc` 让它可以同时挂在 `Context`（给脚本读 `ctx.evaluator`）
/// 和 `thread_local`（给脚本调 `evaluate_boss(boss)`）上，不必克隆底层插件。
pub type EvaluatorFn = std::sync::Arc<EvaluatorCallback>;

thread_local! {
    /// 当前调用生效的报告生成器。
    ///
    /// # 为什么用 thread_local 而不是全局可变状态
    ///
    /// 面板是异步的：多个请求可能同时在跑脚本，各自属于不同的
    /// 「当前语言」和「当前评估器」。全局共享会让 A 请求的英文报告
    /// 串到 B 请求的中文输出里 —— 而且只在并发时偶发，极难复现。
    ///
    /// Rhai 的 `register_fn` 只接受 `Send + Sync` 的普通闭包（它要能跨线程），
    /// 所以闭包本身不能捕获 `!Send` 的东西。thread_local 让「每次调用
    /// 各自的钩子」成立，而闭包只负责读当前线程的值。
    static REPORT_HOOK: std::cell::RefCell<Option<ReportFn>> =
        const { std::cell::RefCell::new(None) };

    /// 当前调用生效的评估器。
    ///
    /// 与 `REPORT_HOOK` 分开存，而不是塞进一个结构体里：两个钩子的
    /// 生命周期虽然都被 `call_dynamic` 管着，但装配路径不同 ——
    /// 报告钩子按 `PluginKind` 决定要不要注册，评估器则是「有就用」。
    static EVALUATOR_HOOK: std::cell::RefCell<Option<EvaluatorFn>> =
        const { std::cell::RefCell::new(None) };
}

/// 装上两个钩子，返回一个 RAII 守卫（drop 时自动卸除并恢复原值）。
fn set_hooks(report: Option<ReportFn>, evaluator: Option<EvaluatorFn>) -> HookGuard {
    // 先记「这次要不要动」再解构 —— `Option<Arc<dyn Fn>>` 不是 `Copy`，
    // 一旦按值取出内层就借不到 `report` 了
    let dropped_report = report.is_some();
    let dropped_eval = evaluator.is_some();

    let prev_report = report.map(|f| REPORT_HOOK.with(|h| h.borrow_mut().replace(f)));
    let prev_eval = evaluator.map(|f| EVALUATOR_HOOK.with(|h| h.borrow_mut().replace(f)));

    HookGuard {
        prev_report: prev_report.flatten(),
        prev_eval: prev_eval.flatten(),
        dropped_report,
        dropped_eval,
    }
}

/// 恢复上一层钩子。
///
/// 只还原**本次真的替换过**的那些：`None` 意味着「这次没动它」，
/// 若无条件写回 `None` 就会把外层（比如调试台里嵌的一层）的钩子抹掉。
struct HookGuard {
    prev_report: Option<ReportFn>,
    prev_eval: Option<EvaluatorFn>,
    dropped_report: bool,
    dropped_eval: bool,
}

impl Drop for HookGuard {
    fn drop(&mut self) {
        if self.dropped_report {
            let prev = self.prev_report.take();
            REPORT_HOOK.with(|h| *h.borrow_mut() = prev);
        }
        if self.dropped_eval {
            let prev = self.prev_eval.take();
            EVALUATOR_HOOK.with(|h| *h.borrow_mut() = prev);
        }
    }
}

/// `engine_report(boss, ctx)` 的实现：转发给当前线程挂着的报告生成器。
fn call_report_hook(boss: &Map, ctx: &Map) -> String {
    let view = crate::ast::boss_from_rhai(boss);
    let lang = ctx
        .get("lang")
        .and_then(|v| v.clone().try_cast::<String>())
        .unwrap_or_else(|| "zh".to_string());
    REPORT_HOOK
        .with(|h| h.borrow().clone())
        .map(|f| f(&view, &lang))
        .unwrap_or_default()
}

/// `evaluate_boss(boss, lang?)` 的实现：转发给当前线程挂着的评估器。
///
/// 没装评估器 → 空串（「不评估」），**不报错**。这对齐原版
/// `_eval_line()` 的语义：评估器缺失或抛异常都只是「这一行不出现」，
/// 绝不能把播报主流程搞崩。
fn call_evaluator_hook(boss: &Map, lang: &str) -> String {
    let view = crate::ast::boss_from_rhai(boss);
    let lang = if lang.is_empty() { "zh" } else { lang };
    EVALUATOR_HOOK
        .with(|h| h.borrow().clone())
        .map(|f| f(&view, lang))
        .unwrap_or_default()
}


/// 把「当前激活的评估器」包成回调，供决策器插件调用。
///
/// 对应原版 `panel/evaluator_hook.py` 的 `make_evaluator_callback(lang)`。
///
/// # 为什么是这个签名
///
/// 闭包收 `(&BossView, &str)` 而不是只收 `&BossView`：一次请求里
/// 语言是固定的（`langs[0]`），但把它做成参数能让**同一个回调实例**
/// 在中英两条通路上复用 —— 原版这边做不到，双语模式下它会把中文
/// 回调再喂给英文通路（见 `evaluator_hook.make_evaluator_callback`），
/// 只不过因为 `format_line` 只影响 `评分`/`Score` 模板、而中英评估器
/// 通常是同一份脚本，这个差别一直没暴露出来。
///
/// # 失败即「不评估」
///
/// `evaluate` 返回 `()`（脚本自己说这只头目不评估）和返回错误
/// （脚本炸了）都降级为空串。原版 `evaluator_hook._cb` 里是
/// `logger.warning(...) ; return ""`，行为一致。
pub fn make_evaluator_callback(plugin: Arc<Plugin>) -> EvaluatorFn {
    Arc::new(move |boss: &BossView, lang: &str| {
        // 评估器的 ctx 只给单语言 —— 与原版
        // `make_evaluator_callback(langs[0])` 一致：评估器不需要
        // 自己知道「当前是不是双语模式」。
        let ctx = Context::new(lang.to_string(), vec![lang.to_string()]);
        match plugin.evaluate(boss, &ctx) {
            Ok(Some(ev)) => ev.format_line(lang),
            // 脚本返回 unit = 「这只头目不评估」，与原版 `return None` 对应
            Ok(None) => String::new(),
            Err(e) => {
                tracing::warn!(plugin = %plugin.manifest().name, error = %e, "评估器执行失败，跳过评估");
                String::new()
            }
        }
    })
}

/// 把 Rhai 的错误翻译成我们的错误类型。
///
/// 单独抽出来是因为「超时」这个分支值得特殊对待：它不是脚本写错了，
/// 而是脚本跑飞了，面板上应该给出完全不同的提示。
///
/// 入参是 `Box<EvalAltResult>`（Rhai 为了缩小 `Result` 体积做的装箱）。
///
/// 这个 `Box` 不能省 —— clippy 的 `boxed_local` 在这里是**误报**：
/// 它看的是「函数体里不需要 Box」，但实际约束来自调用方
/// `Engine::call_fn` 的返回类型 `Result<_, Box<EvalAltResult>>`，
/// 函数签名必须对得上（`map_err` 不会替我们解箱）。
#[allow(clippy::boxed_local)]
fn map_rhai_err(e: Box<rhai::EvalAltResult>) -> PluginError {
    match *e {
        // 指令数超限 / 栈溢出：都是「脚本失控」而非「脚本写错」
        rhai::EvalAltResult::ErrorTooManyOperations(_)
        | rhai::EvalAltResult::ErrorStackOverflow(_) => {
            PluginError::Timeout(DEFAULT_MAX_OPERATIONS)
        }
        other => PluginError::Runtime(other.to_string()),
    }
}

/// 调用脚本时的上下文（对应原版 `ctx` dict）。
///
/// # 与原版的差别
///
/// 原版 `ctx` 直接塞了 `Pokedex` 和 `Rules` 两个 Python 对象，
/// 脚本可以调它们**任何**方法。这里只暴露一组**限定查询函数**：
/// 脚本无法遍历整个图鉴、无法读取规则内部结构，只能按名字查。
/// 这既缩小了沙箱面，也让 ABI 稳定 —— 图鉴内部怎么改都不影响脚本。
///
/// # 内部可变性
///
/// `report` 用 `Arc<dyn Fn>` 而非泛型参数，是为了让 `Context` 保持
/// 一个具体类型（`Context<'a, F>` 会传染到 `Plugin`、到面板的 handler，
/// 最终每个调用点都要写一遍泛型）。这里的动态派发开销相对于
/// 「解释执行一个脚本」完全可以忽略。
#[derive(Clone)]
pub struct Context {
    /// 语言模式：`zh` / `en`
    pub lang: String,
    /// 输出语言列表（双语模式是 `["zh","en"]`）
    pub langs: Vec<String>,
    /// 官方引擎报告的生成闭包（决策器用；评估器不需要）
    report: Option<ReportFn>,
    /// 当前激活评估器的回调（决策器用；也用于 `engine_report` 里补评语行）
    ///
    /// 对应原版 `ctx["evaluator"]`。原版还有 `ctx["make_evaluator"]`
    /// （一个能按语言再造回调的工厂函数），本版**不暴露** ——
    /// 回调本身已经接受 `lang` 参数，工厂函数是 Python 闭包里
    /// 「语言只能捕获不能传参」的产物，在 Rust 这边没有对应需求。
    evaluator: Option<EvaluatorFn>,
}

impl std::fmt::Debug for Context {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Context")
            .field("lang", &self.lang)
            .field("langs", &self.langs)
            .field("has_report", &self.report.is_some())
            .field("has_evaluator", &self.evaluator.is_some())
            .finish_non_exhaustive()
    }
}

impl Default for Context {
    fn default() -> Self {
        Self {
            lang: "zh".into(),
            langs: vec!["zh".into()],
            report: None,
            evaluator: None,
        }
    }
}

impl Context {
    pub fn new(lang: impl Into<String>, langs: Vec<String>) -> Self {
        Self {
            lang: lang.into(),
            langs,
            report: None,
            evaluator: None,
        }
    }

    /// 中文语言模式。
    pub fn zh() -> Self {
        Self::new("zh", vec!["zh".to_string()])
    }

    /// 英文语言模式。
    pub fn en() -> Self {
        Self::new("en", vec!["en".to_string()])
    }

    /// 双语模式（先中文后英文）。
    pub fn bilingual() -> Self {
        Self::new("zh", vec!["zh".to_string(), "en".to_string()])
    }

    /// 注入「官方引擎报告」的生成闭包。
    ///
    /// 内建决策器靠它把活儿交回 Rust 引擎 —— 决策器插件只负责
    /// 「要不要加工、怎么加工」，打法本身仍是引擎的事。
    ///
    /// 传闭包而不是直接持有 `Rules`，是为了让 `alpha-plugin` 不必知道
    /// 引擎的构造细节（更不必持有 `Rules` 的生命周期）。
    pub fn with_report<F>(mut self, f: F) -> Self
    where
        F: Fn(&BossView, &str) -> String + Send + Sync + 'static,
    {
        self.report = Some(std::sync::Arc::new(f));
        self
    }

    /// 注入「当前激活评估器」的回调。
    ///
    /// 决策器插件拿到它之后有两个用法：
    ///
    /// - `ctx.evaluator` —— 脚本自己调，决定怎么用这条评语；
    /// - `evaluate_boss(boss)` —— 沙箱注册好的便捷函数，
    ///   用当前语言调一次评估器。
    ///
    /// 传 `None`（或干脆不调这个 builder）等价于「没有激活评估器」，
    /// 行为与原版一致：不评估，报文里少那一行。
    pub fn with_evaluator(mut self, cb: Option<EvaluatorFn>) -> Self {
        self.evaluator = cb;
        self
    }

    /// 现网部署用的便捷构造：直接从仓库根目录的 `config/rules.yaml` 加载规则。
    ///
    /// 规则加载失败时降级为「报告生成器返回空串」——决策器仍然能被调用，
    /// 只是输出为空。这样面板至少不会因为一个配置文件读不到而整站不可用。
    pub fn with_repo_rules(self, root: impl AsRef<Path>) -> Self {
        let root = root.as_ref().to_path_buf();

        // 先把评估器取出来 —— 报告闭包里要把评语行接上引擎。
        //
        // ⚠️ 这里**必须**用 `self.evaluator` 而不是重新读一次。原因在
        // 调用顺序：调用方通常写成
        // `Context::new(..).with_repo_rules(..).with_evaluator(cb)`，
        // 评估器是在 `with_repo_rules` **之后**才装上的。
        // 所以不能在这里就把 `evaluator` 捕获进闭包（那时还是 `None`）。
        //
        // 解法是让闭包去读**当前线程挂着的**评估器钩子
        // （`call_evaluator_hook`）—— 钩子是每次调用时由 `call_dynamic`
        // 装上的，读的时候值一定已经就位。
        //
        // 这个坑踩过一次：第一版直接写死 `None`，表现为
        // 「调试台明明配了评估器，报告里就是不出评语行」，
        // 而且不报任何错 —— 静默少一行是最难查的那种。
        self.with_report(move |boss, lang| {
            let Ok(text) = std::fs::read_to_string(root.join("config/rules.yaml")) else {
                return String::new();
            };
            let Ok(pk) = alpha_core::pokedex::get_pokedex() else {
                return String::new();
            };
            let Ok(rules) = alpha_strategy::Rules::from_yaml(&text, pk) else {
                return String::new();
            };
            let bd = crate::ast::boss_view_to_boss_data(boss);

            // 引擎要的是 `Fn(&BossData) -> String`，与脚本侧的
            // `EvaluatorFn`（收 `BossView`）不是一回事，这里转一层。
            //
            // 语言用闭包捕获的 `lang`（引擎按语言分别调用这个闭包，
            // 中文下评语模板是「评分N」、英文下是「Score N」）。
            //
            // 为什么在这里读 `thread_local` 而不是让 `report` 闭包
            // 捕获一份评估器：见上面 ⚠️ —— 调用方的装配顺序是
            // `with_repo_rules` 在前、`with_evaluator` 在后。
            let evaluator: Option<alpha_strategy::Evaluator> =
                EVALUATOR_HOOK.with(|h| h.borrow().is_some()).then(|| {
                    let lang = lang.to_string();
                    std::sync::Arc::new(move |b: &alpha_core::models::BossData| {
                        let view = crate::ast::BossView::from_boss(b);
                        EVALUATOR_HOOK
                            .with(|h| h.borrow().clone())
                            .map(|f| f(&view, &lang))
                            .unwrap_or_default()
                    }) as alpha_strategy::Evaluator
                });

            // 没有评估器时传 `None`，行为与旧版完全一致（不评估）
            alpha_strategy::engine::generate_report(
                &bd,
                &rules,
                pk,
                lang,
                evaluator.as_ref(),
            )
        })
    }

    pub fn is_bilingual(&self) -> bool {
        self.langs.len() > 1
    }

    /// 转成 Rhai Map。
    ///
    /// # `ctx.evaluator` 在这里是个**布尔值**，不是函数
    ///
    /// 原版 `ctx["evaluator"]` 是个 Python 可调用对象，脚本写
    /// `ctx["evaluator"](boss)` 就能拿到评语。Rhai **做不到这一点**：
    /// 它只有两种可调用值 —— 脚本里 `fn` 定义出来的 `FnPtr`，
    /// 或者 `Dynamic::from(Box<dyn Fn>)`；而后者一装箱 `clone_cast`
    /// 就挂了（Rhai 的 `FnPtr` 回调是 `Fn` 不是 `FnOnce`，要支持重复调用
    /// 就得克隆，闭包一旦被 `Box` 住就不可克隆）。
    ///
    /// 试过并放弃的替代方案：把回调注册成引擎函数——但 `Engine` 不可
    /// 变（`build_engine` 只在编译插件时跑一次），注册完就没法卸，
    /// 于是钩子只能全局共享，正是我们想避免的那种并发串味。
    ///
    /// 所以这里换个形状：`ctx.evaluator` 退化成「有没有评估器」的标志位，
    /// 真正取评语走沙箱注册的 **`evaluate_boss(boss)`** / **`evaluate_boss(boss, lang)`**。
    /// 契约仍然是「决策器把评估器接到引擎上」，只是接口从
    /// 「调用一个函数值」变成「调用一个内置函数」。
    ///
    /// 没有激活评估器时这个键为 `false` —— 脚本可以据此决定要不要多输出
    /// 一行标题之类的（原版里 `if ctx.get("evaluator")` 就是这个用法）。
    fn to_rhai(&self) -> Map {
        let mut m = Map::new();
        m.insert("lang".into(), Dynamic::from(self.lang.clone()));
        m.insert(
            "langs".into(),
            Dynamic::from_iter(self.langs.iter().cloned()),
        );
        m.insert("bilingual".into(), Dynamic::from(self.is_bilingual()));
        m.insert(
            "evaluator".into(),
            Dynamic::from(self.evaluator.is_some()),
        );
        m
    }
}

/// 构建沙箱引擎。
///
/// **这里注册了什么，脚本就能做什么** —— 每加一个函数都等于扩一次权限。
/// 当前只注册纯计算与只读查询，没有任何 IO。
fn build_engine(kind: PluginKind) -> Engine {
    let mut engine = Engine::new();

    // ---- 资源限制 ----
    engine.set_max_operations(DEFAULT_MAX_OPERATIONS);
    engine.set_max_expr_depths(DEFAULT_MAX_EXPR_DEPTH, DEFAULT_MAX_EXPR_DEPTH);
    engine.set_max_call_levels(DEFAULT_MAX_CALL_DEPTH);
    engine.set_max_string_size(DEFAULT_MAX_STRING_SIZE);
    engine.set_max_array_size(DEFAULT_MAX_ARRAY_SIZE);
    engine.set_max_map_size(DEFAULT_MAX_ARRAY_SIZE);

    // ---- 关掉危险开关 ----
    engine.disable_symbol("eval"); // 禁 eval，脚本无法自己造代码跑
    engine.set_fast_operators(false); // 保持运算语义与文档一致（不做代数化简）

    // ---- 通用小工具（纯计算，见 script_std 的说明）----
    //
    // Rhai 的标准包里没有 `Array::join()`，而 Python 的 `"/".join(names)`
    // 在评估器里用得很多。用 Rust 实现一次注册给所有脚本，
    // 好过让每个插件各写一遍字符串累加循环。
    crate::script_std::register(&mut engine);

    // ---- 只读查询：技能 / 精灵 / 特性 名字 ↔ id ----
    engine.register_fn("resolve_move_id", |name: &str| -> Dynamic {
        match pokedex().and_then(|p| p.resolve_move_id(name)) {
            Some(id) => Dynamic::from(id),
            None => Dynamic::UNIT,
        }
    });
    engine.register_fn("resolve_pokemon_id", |name: &str| -> Dynamic {
        match pokedex().and_then(|p| p.resolve_pokemon_id(name)) {
            Some(id) => Dynamic::from(id),
            None => Dynamic::UNIT,
        }
    });
    engine.register_fn("resolve_ability_id", |name: &str| -> Dynamic {
        match pokedex().and_then(|p| p.resolve_ability_id(name)) {
            Some(id) => Dynamic::from(id),
            None => Dynamic::UNIT,
        }
    });

    // ---- 只读查询：翻译 ----
    engine.register_fn("move_name", |id: i64, lang: &str| -> String {
        translate_by_id("move", id, lang)
    });
    engine.register_fn("pokemon_name", |id: i64, lang: &str| -> String {
        translate_by_id("pokemon", id, lang)
    });
    engine.register_fn("ability_name", |id: i64, lang: &str| -> String {
        translate_by_id("ability", id, lang)
    });

    // ---- 官方引擎报告（只有决策器需要）----
    //
    // 没在 `Context` 里注入生成器时，这个函数会返回空串。
    // 这是有意的取舍：注册成「永远存在」比「按调用动态注册」简单得多
    // （`Engine` 不可 Clone，而 Rhai 要求注册的闭包 `Send + Sync`），
    // 代价是脚本调用它但没注入时拿到空串而非报错。
    //
    // 实际影响很小：`engine_report` 只在决策器插件里用，
    // 而决策器路径**一定**会注入生成器（见 `alpha-server` 的调度器）。
    if kind == PluginKind::Dispatcher {
        engine.register_fn("engine_report", |boss: Map, ctx: Map| -> String {
            call_report_hook(&boss, &ctx)
        });

        // ---- 当前激活评估器（只有决策器需要）----
        //
        // 为什么决策器需要它：原版把 `ctx["evaluator"]` 注入给**决策器**，
        // 决策器再把它接到引擎上，报文里才会出现「评价 + 评分」那一行。
        // 换句话说评语是**决策器带进来的**，不是引擎自己知道要出。
        //
        // 本版把这个契约保留下来，但同时提供了一个更省事的入口
        // `evaluate_boss(boss)`：内建决策器如果只是想「按当前语言
        // 拿一行评语」，不必自己去 `ctx.evaluator` 里翻。
        //
        // 注意这里的 `evaluate_boss` 用的是**捕获的语言**（`call_evaluator_hook`
        // 的第二个参数缺省 "zh"）—— 决策器要按 `ctx.lang` 调就用
        // `evaluate_boss(boss, ctx.lang)` 这个两参形式。
        engine.register_fn("evaluate_boss", |boss: Map| -> String {
            call_evaluator_hook(&boss, "zh")
        });
        engine.register_fn("evaluate_boss", |boss: Map, lang: &str| -> String {
            call_evaluator_hook(&boss, lang)
        });
    }

    engine
}

/// 图鉴单例（拿不到时返回 None，脚本侧的查询一律降级为空）。
fn pokedex() -> Option<&'static alpha_core::pokedex::Pokedex> {
    alpha_core::pokedex::get_pokedex().ok()
}

/// 按 id 取指定语言的规范名。
fn translate_by_id(kind: &str, id: i64, lang: &str) -> String {
    let Some(p) = pokedex() else {
        return String::new();
    };
    let id = Some(id);
    match kind {
        "move" => p.move_name(id, lang).unwrap_or_default(),
        "pokemon" => p.pokemon_name(id, lang).unwrap_or_default(),
        "ability" => p.ability_name(id, lang).unwrap_or_default(),
        _ => String::new(),
    }
}

impl Plugin {
    /// 给测试与内建插件用：不带文件的编译。
    pub fn compile_inline(source: &str, kind: PluginKind) -> PluginResult<Self> {
        Self::compile(source, kind, None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::boss;

    const MINIMAL_DISPATCHER: &str = r#"
        fn dispatch(boss, ctx) { "ok:" + boss.name }
    "#;

    #[test]
    fn compiles_and_calls_a_minimal_dispatcher() {
        let p = Plugin::compile_inline(MINIMAL_DISPATCHER, PluginKind::Dispatcher).unwrap();
        let out = p.dispatch(&boss("皮卡丘", &["电光一闪"]), &Context::zh()).unwrap();
        assert_eq!(out, "ok:皮卡丘");
    }

    #[test]
    fn rejects_script_without_entry_function() {
        let err = Plugin::compile_inline("fn other() { 1 }", PluginKind::Dispatcher)
            .unwrap_err();
        assert!(matches!(err, PluginError::MissingEntry("dispatch")), "实际: {err:?}");
        assert!(err.is_user_fault(), "应被判定为用户脚本问题");
    }

    #[test]
    fn rejects_syntax_error() {
        let err = Plugin::compile_inline("fn dispatch(boss, ctx { ", PluginKind::Dispatcher)
            .unwrap_err();
        assert!(matches!(err, PluginError::Compile(_)), "实际: {err:?}");
    }

    /// 死循环必须被中断 —— 这是沙箱存在的意义。
    /// 没有这个限制，一个 `while true {}` 就能把整个面板卡死。
    #[test]
    fn infinite_loop_is_interrupted() {
        let src = r#"fn dispatch(boss, ctx) { let i = 0; while true { i += 1; } "never" }"#;
        let p = Plugin::compile_inline(src, PluginKind::Dispatcher).unwrap();
        let err = p.dispatch(&boss("x", &[]), &Context::zh()).unwrap_err();
        assert!(matches!(err, PluginError::Timeout(_)), "实际: {err:?}");
    }

    /// 无界递归同样要被拦住。
    #[test]
    fn infinite_recursion_is_interrupted() {
        let src = r#"fn rec(n) { rec(n + 1) } fn dispatch(boss, ctx) { rec(0) }"#;
        let p = Plugin::compile_inline(src, PluginKind::Dispatcher).unwrap();
        let err = p.dispatch(&boss("x", &[]), &Context::zh()).unwrap_err();
        // 栈溢出或指令超限都算被拦住了
        assert!(
            matches!(err, PluginError::Timeout(_) | PluginError::Runtime(_)),
            "实际: {err:?}"
        );
    }

    /// 沙箱里**不允许**有任何 IO 入口。
    /// 这条测试是对「相对原版 RCE 的改进」的直接断言。
    ///
    /// # 判据是「这一行跑不通」，不是「报错信息长什么样」
    ///
    /// 失败可能发生在两个阶段，都是可接受的：
    /// - **编译期**：`eval` 被 `disable_symbol` 拦下（它是保留字，直接不认）
    /// - **运行期**：`system()` 之类根本没注册，报「函数不存在」
    ///
    /// 关键是**不能有任何一条能成功执行** —— 那才是真的漏了。
    #[test]
    fn sandbox_exposes_no_io() {
        for forbidden in [
            "eval", "system", "read_file", "write_file", "open", "run", "import",
        ] {
            let src = format!(r#"fn dispatch(boss, ctx) {{ {forbidden}("x") }}"#);

            // 编译失败 = 已经在编译期挡住了，直接通过
            let Ok(plugin) = Plugin::compile_inline(&src, PluginKind::Dispatcher) else {
                continue;
            };

            // 编译过的话，运行期必须失败
            let err = plugin.dispatch(&boss("x", &[]), &Context::zh());
            assert!(
                err.is_err(),
                "`{forbidden}` 竟然可以被调用并成功返回 —— 沙箱漏了"
            );
        }
    }

    /// 补一个正向断言：确认沙箱确实**只有**我们注册的那些函数可用。
    /// 光测「known bad 被挡住」不够，还得证明「没有意外的名字能跑」。
    #[test]
    fn only_registered_functions_are_callable() {
        // 任何一个不存在的函数名都该报错
        for name in ["magic_io", "http_get", "exec_python", "load_so"] {
            let src = format!(r#"fn dispatch(boss, ctx) {{ {name}() }}"#);
            let Ok(p) = Plugin::compile_inline(&src, PluginKind::Dispatcher) else {
                continue;
            };
            assert!(
                p.dispatch(&boss("x", &[]), &Context::zh()).is_err(),
                "未注册的 `{name}` 竟然可调用"
            );
        }

        // 而注册过的查询函数必须能用（每个函数配一个它认得的名字）
        for (func, probe) in [
            ("resolve_move_id", "挑衅"),
            ("resolve_pokemon_id", "皮卡丘"),
            ("resolve_ability_id", "静电"),
        ] {
            let src = format!(
                r#"fn dispatch(boss, ctx) {{ if {func}("{probe}") == () {{ "unit" }} else {{ "id" }} }}"#
            );
            let p = Plugin::compile_inline(&src, PluginKind::Dispatcher)
                .unwrap_or_else(|e| panic!("`{func}` 应可用: {e}"));
            let out = p
                .dispatch(&boss("x", &[]), &Context::zh())
                .unwrap_or_else(|e| panic!("`{func}` 调用失败: {e}"));
            assert_eq!(out, "id", "`{func}(\"{probe}\")` 应解析出 id");
        }
    }

    /// 评估器插件**不该**拿到 `engine_report` —— 评估器不产生报告，
    /// 给了它只会诱导用户写出「评估器里再生成一份报告」这种混乱结构。
    #[test]
    fn evaluator_cannot_call_engine_report() {
        let src = r#"fn evaluate(boss, ctx) { engine_report(boss, ctx); () }"#;
        let Ok(p) = Plugin::compile_inline(src, PluginKind::Evaluator) else {
            return; // 编译期就挡住了，也算正确
        };
        assert!(
            p.evaluate(&boss("x", &[]), &Context::zh()).is_err(),
            "评估器不该能调用 engine_report"
        );
    }

    /// 评估器插件同样不该看到「评估器钩子」—— 评估器调评估器是自我递归。
    #[test]
    fn evaluator_cannot_call_evaluate_boss() {
        let src = r#"fn evaluate(boss, ctx) { evaluate_boss(boss); () }"#;
        let Ok(p) = Plugin::compile_inline(src, PluginKind::Evaluator) else {
            return;
        };
        assert!(
            p.evaluate(&boss("x", &[]), &Context::zh()).is_err(),
            "评估器不该能调用 evaluate_boss"
        );
    }

    // ---------------- 评估器钩子 ----------------

    /// 一个固定的假评估器：不管什么头目都回同一行。
    fn stub_evaluator(line: &'static str) -> EvaluatorFn {
        std::sync::Arc::new(move |_b: &BossView, _lang: &str| line.to_string())
    }

    /// `evaluate_boss(boss)` 单参形式：语言缺省中文。
    #[test]
    fn evaluate_boss_defaults_to_chinese() {
        let src = r#"fn dispatch(boss, ctx) { evaluate_boss(boss) }"#;
        let p = Plugin::compile_inline(src, PluginKind::Dispatcher).unwrap();
        let ctx = Context::zh().with_evaluator(Some(stub_evaluator("简单头 评分3")));
        assert_eq!(p.dispatch(&boss("x", &[]), &ctx).unwrap(), "简单头 评分3");
    }

    /// 双参形式会把语言透传给回调 —— 这是 `ctx.evaluator` 做不到的事。
    #[test]
    fn evaluate_boss_forwards_the_language_argument() {
        let src = r#"fn dispatch(boss, ctx) { evaluate_boss(boss, ctx.lang) }"#;
        let p = Plugin::compile_inline(src, PluginKind::Dispatcher).unwrap();

        // 回调按语言分支，用来证明语言真的传到了
        let cb: EvaluatorFn = std::sync::Arc::new(|_b: &BossView, lang: &str| {
            if lang == "en" {
                "RNG Alpha Score 5".to_string()
            } else {
                "看脸头 评分5".to_string()
            }
        });

        let zh = Context::zh().with_evaluator(Some(std::sync::Arc::clone(&cb)));
        assert_eq!(p.dispatch(&boss("x", &[]), &zh).unwrap(), "看脸头 评分5");

        let en = Context::en().with_evaluator(Some(cb));
        assert_eq!(p.dispatch(&boss("x", &[]), &en).unwrap(), "RNG Alpha Score 5");
    }

    /// 没注入评估器 → 空串，**且不报错**。
    ///
    /// 这条对应原版 `_eval_line()` 的「任何异常都静默降级为空」：
    /// 评估器缺失绝不能让决策器失败。
    #[test]
    fn missing_evaluator_yields_empty_line_without_error() {
        let src = r#"fn dispatch(boss, ctx) { evaluate_boss(boss) }"#;
        let p = Plugin::compile_inline(src, PluginKind::Dispatcher).unwrap();
        let got = p.dispatch(&boss("x", &[]), &Context::zh());
        assert_eq!(got.unwrap(), "", "没有评估器时应返回空串");
    }

    /// `ctx.evaluator` 是「有没有评估器」的布尔标志。
    ///
    /// Rhai 没法把 Rust 回调变成脚本可调用的函数值（见 `Context::to_rhai`
    /// 的说明），所以这个键退化成了标志位。把形状钉死在这里，
    /// 免得以后有人以为它能当函数用。
    #[test]
    fn ctx_evaluator_is_a_boolean_flag() {
        let src = r#"fn dispatch(boss, ctx) {
            if ctx.evaluator { "yes" } else { "no" }
        }"#;
        let p = Plugin::compile_inline(src, PluginKind::Dispatcher).unwrap();

        let without = p.dispatch(&boss("x", &[]), &Context::zh()).unwrap();
        assert_eq!(without, "no");

        let with = Context::zh().with_evaluator(Some(stub_evaluator("x")));
        assert_eq!(p.dispatch(&boss("x", &[]), &with).unwrap(), "yes");
    }

    /// 钩子在调用结束后必须卸干净 —— 否则第二次调用会看到上一次的。
    ///
    /// 这是 thread_local 钩子最典型的泄漏方式：第一次注入、
    /// 第二次不注入，第二次却仍然拿到了评语。
    #[test]
    fn evaluator_hook_is_cleared_after_each_call() {
        let src = r#"fn dispatch(boss, ctx) { evaluate_boss(boss) }"#;
        let p = Plugin::compile_inline(src, PluginKind::Dispatcher).unwrap();

        let with = Context::zh().with_evaluator(Some(stub_evaluator("残留")));
        assert_eq!(p.dispatch(&boss("x", &[]), &with).unwrap(), "残留");

        // 换一个不带评估器的 ctx，必须什么都不剩
        assert_eq!(
            p.dispatch(&boss("x", &[]), &Context::zh()).unwrap(),
            "",
            "上一次调用的评估器钩子泄漏到了下一次"
        );
    }

    /// 决策器抛错时钩子也必须卸掉（RAII 守卫要覆盖 panic/Err 路径）。
    #[test]
    fn evaluator_hook_is_cleared_even_when_the_script_fails() {
        let src = r#"fn dispatch(boss, ctx) { throw "boom" }"#;
        let p = Plugin::compile_inline(src, PluginKind::Dispatcher).unwrap();

        let with = Context::zh().with_evaluator(Some(stub_evaluator("残留")));
        assert!(p.dispatch(&boss("x", &[]), &with).is_err());

        // 换一个「真的会用评估器」的脚本，确认钩子没留下
        let probe = Plugin::compile_inline(
            r#"fn dispatch(boss, ctx) { evaluate_boss(boss) }"#,
            PluginKind::Dispatcher,
        )
        .unwrap();
        assert_eq!(
            probe.dispatch(&boss("x", &[]), &Context::zh()).unwrap(),
            "",
            "脚本失败后评估器钩子没被卸掉"
        );
    }

    /// `make_evaluator_callback`：把评估器插件包成回调，跑通整条链。
    #[test]
    fn make_evaluator_callback_wraps_a_real_plugin() {
        let src = r#"fn evaluate(boss, ctx) {
            if boss.name == "巨牙鲨" { #{ score: 5, label: "看脸头" } } else { () }
        }"#;
        let plugin = std::sync::Arc::new(
            Plugin::compile_inline(src, PluginKind::Evaluator).unwrap(),
        );
        let cb = make_evaluator_callback(std::sync::Arc::clone(&plugin));

        let hit = crate::test_support::boss_full("巨牙鲨", "粗糙皮肤", &[], Some(50.0));
        assert_eq!(cb(&hit, "zh"), "看脸头 评分5");
        assert_eq!(cb(&hit, "en"), "看脸头 Score 5", "英文模板应是 Score");

        let miss = crate::test_support::boss_full("皮卡丘", "静电", &[], Some(50.0));
        assert_eq!(cb(&miss, "zh"), "", "脚本返回 () 时该是空串");
    }

    /// **回归**：`with_repo_rules` 的报告闭包必须把评估器接给引擎。
    ///
    /// 这条测试的来由是一个真实的静默 bug：第一版的 `with_repo_rules`
    /// 里硬编码了 `generate_report(..., None)`，于是不管调用方有没有
    /// 装评估器，`engine_report` 出来的报告里**永远没有评语行** ——
    /// 不报错、不告警，就是少一行。
    ///
    /// 更麻烦的是装配顺序：调用方写的是
    /// `Context::new(..).with_repo_rules(..).with_evaluator(cb)`，
    /// 评估器在 `with_repo_rules` **之后**才装上，所以闭包不能
    /// 按值捕获它，只能到执行时去读线程局部的钩子。
    ///
    /// 这条测试按真实调用顺序构造 Context，判据就是「评语出现在报告里」。
    #[test]
    fn repo_rules_report_includes_the_evaluator_line() {
        let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(|p| p.parent())
            .expect("应有仓库根目录")
            .to_path_buf();
        let rules = root.join("config/rules.yaml");
        assert!(rules.exists(), "端到端测试需要 {}", rules.display());

        // 双性别 + 命中队伍白名单，才会走完整报告路径（含 header 里的评估行）
        let b = crate::test_support::boss_full("巨牙鲨", "粗糙皮肤", &["挑衅"], Some(50.0));
        let src = r#"fn dispatch(boss, ctx) { engine_report(boss, ctx) }"#;
        let p = Plugin::compile_inline(src, PluginKind::Dispatcher).unwrap();

        let without = p
            .dispatch(&b, &Context::zh().with_repo_rules(&root))
            .expect("无评估器时该能出报告");
        assert!(
            !without.contains("探针评语"),
            "没装评估器时不该有评语行：\n{without}"
        );

        // 按真实调用顺序：先 with_repo_rules，再 with_evaluator
        let ctx = Context::zh()
            .with_repo_rules(&root)
            .with_evaluator(Some(std::sync::Arc::new(|_b: &BossView, _l: &str| {
                "探针评语".to_string()
            })));
        let with = p.dispatch(&b, &ctx).expect("装了评估器也该能出报告");

        assert!(
            with.contains("探针评语"),
            "装了评估器后报告里必须有评语行 —— \
             `with_repo_rules` 的报告闭包没把评估器接给引擎（静默少一行）：\n{with}"
        );

        // 位置：第 2 行
        let lines: Vec<&str> = with.lines().collect();
        assert_eq!(lines.get(1).copied(), Some("探针评语"), "\n{with}");
    }

    /// 评估器插件炸了 → 回调返回空串而不是把异常抛给决策器。
    #[test]
    fn make_evaluator_callback_swallows_runtime_errors() {
        let plugin = std::sync::Arc::new(
            Plugin::compile_inline(
                r#"fn evaluate(boss, ctx) { throw "evaluator blew up" }"#,
                PluginKind::Evaluator,
            )
            .unwrap(),
        );
        let cb = make_evaluator_callback(plugin);
        let b = crate::test_support::boss_full("巨牙鲨", "粗糙皮肤", &[], Some(50.0));
        assert_eq!(cb(&b, "zh"), "", "评估器报错该降级为空串");
    }

    #[test]
    fn query_functions_work_from_script() {
        let src = r#"
            fn dispatch(boss, ctx) {
                let id = resolve_move_id("挑衅");
                if id == () { return "解析失败"; }
                move_name(id, "zh")
            }
        "#;
        let p = Plugin::compile_inline(src, PluginKind::Dispatcher).unwrap();
        assert_eq!(p.dispatch(&boss("x", &[]), &Context::zh()).unwrap(), "挑衅");
    }

    #[test]
    fn unknown_name_resolves_to_unit() {
        let src = r#"
            fn dispatch(boss, ctx) {
                if resolve_move_id("这个技能不存在") == () { "unit" } else { "got" }
            }
        "#;
        let p = Plugin::compile_inline(src, PluginKind::Dispatcher).unwrap();
        assert_eq!(p.dispatch(&boss("x", &[]), &Context::zh()).unwrap(), "unit");
    }

    #[test]
    fn evaluator_returning_unit_means_no_evaluation() {
        let p = Plugin::compile_inline("fn evaluate(boss, ctx) { () }", PluginKind::Evaluator)
            .unwrap();
        assert!(p.evaluate(&boss("x", &[]), &Context::zh()).unwrap().is_none());
    }

    #[test]
    fn evaluator_returning_map_is_parsed() {
        let src = r#"
            fn evaluate(boss, ctx) {
                #{ score: 3, label: "看脸头", detail: "测试", factors: ["a", "b"] }
            }
        "#;
        let p = Plugin::compile_inline(src, PluginKind::Evaluator).unwrap();
        let ev = p.evaluate(&boss("x", &[]), &Context::zh()).unwrap().unwrap();
        assert_eq!(ev.score, Some(3));
        assert_eq!(ev.label.as_deref(), Some("看脸头"));
        assert_eq!(ev.factors, vec!["a", "b"]);
    }

    /// 返回了非 Map 非 unit 的东西，要给出清楚的报错，而不是静默丢弃。
    #[test]
    fn evaluator_returning_wrong_type_reports_clearly() {
        let p = Plugin::compile_inline(r#"fn evaluate(boss, ctx) { 42 }"#, PluginKind::Evaluator)
            .unwrap();
        let err = p.evaluate(&boss("x", &[]), &Context::zh()).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("map"), "报错应说明需要 map: {msg}");
    }

    /// 每次调用都要用全新的 Scope —— 脚本不能靠全局状态在两次调用间传递信息，
    /// 否则轮询时会因为上一次的残留而给出不同结果（极难排查）。
    #[test]
    fn calls_do_not_share_state() {
        let src = r#"
            fn dispatch(boss, ctx) {
                // 局部变量每次都从零开始
                let n = 0;
                n += 1;
                n.to_string()
            }
        "#;
        let p = Plugin::compile_inline(src, PluginKind::Dispatcher).unwrap();
        for _ in 0..3 {
            assert_eq!(p.dispatch(&boss("x", &[]), &Context::zh()).unwrap(), "1");
        }
    }

    /// 编译一次、多次调用 —— AST 复用是性能改进点，顺手钉住。
    #[test]
    fn ast_is_reused_across_calls() {
        let p = Plugin::compile_inline(MINIMAL_DISPATCHER, PluginKind::Dispatcher).unwrap();
        for i in 0..50 {
            let b = boss(&format!("头目{i}"), &[]);
            assert_eq!(p.dispatch(&b, &Context::zh()).unwrap(), format!("ok:头目{i}"));
        }
    }
}
