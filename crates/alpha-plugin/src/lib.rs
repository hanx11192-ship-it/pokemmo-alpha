//! `alpha-plugin`：Rhai 沙箱插件系统，替代原版的「上传 `.py` 直接 exec」。
//!
//! # 为什么必须换掉原版的插件机制
//!
//! 原版 `panel/dispatch.py` 与 `panel/evaluate.py` 都是这么加载插件的：
//!
//! ```python
//! spec = importlib.util.spec_from_file_location("disp_" + filename[:-3], path)
//! mod = importlib.util.module_from_spec(spec)
//! spec.loader.exec_module(mod)      # ← 用户上传什么就执行什么
//! ```
//!
//! 这意味着**面板的「上传插件」入口等价于一个 RCE 后门**：
//! 上传一个 `.py` 就能读 `panel.env` 里的推送 token、能
//! `os.system("rm -rf /")`、能开反向 shell 连出去。
//! 原版对此没有任何限制 —— 而它是个要暴露在公网上的 Web 面板。
//!
//! Rhai 沙箱把能力收窄到「算数 + 字符串 + 容器」：没有文件、网络、
//! 进程、环境变量 API，另外还有指令数上限挡住死循环。
//!
//! # 模块地图
//!
//! | 模块 | 职责 |
//! |---|---|
//! | [`ast`] | 脚本可见的数据视图（ABI 的定义处） |
//! | [`sandbox`] | Rhai 引擎构建、资源限制、插件编译与调用 |
//! | [`manifest`] | 从脚本头部注释里读元信息 |
//! | [`eval`] | 评估结果与「压成报文一行」的格式化 |
//! | [`builtins`] | 编译进二进制的内建插件 |
//! | [`registry`] | 插件目录扫描、加载与热更新 |
//! | [`script_std`] | 给脚本用的通用小工具（补 Rhai 标准库的缺口）|
//!
//! # 与原版插件的一一对应
//!
//! | 原版 Python | Rust / Rhai |
//! |---|---|
//! | `def dispatch(boss, ctx) -> str` | `fn dispatch(boss, ctx)` |
//! | `def evaluate(boss, ctx) -> dict \| None` | `fn evaluate(boss, ctx)` 返回 map 或 `()` |
//! | `ctx["pokedex"].resolve_move_id(n)` | `resolve_move_id(n)` |
//! | `ctx["rules"]` | 不暴露（引擎通过 `engine_report` 回调提供） |
//! | `boss.has_move_id(mid)` | 遍历 `boss.move_ids` 比对（见内建评估器） |
//! | 模块级 `NAME` / `DESCRIPTION` | 头部注释 `// @name` / `// @description` |
//!
//! # 示例：写一个自己的评估器
//!
//! ```text
//! // @name 我的评估器
//! // @description 头目带挑衅就加 2 分
//!
//! fn evaluate(boss, ctx) {
//!     if boss.moves.contains("挑衅") {
//!         return #{ score: 2, label: "有点烦" };
//!     }
//!     ()          // 返回 () 表示这只头目不评估
//! }
//! ```

pub mod ast;
pub mod builtins;
pub mod error;
pub mod eval;
pub mod manifest;
pub mod registry;
pub mod sandbox;
pub mod script_std;

pub use ast::{boss_from_rhai, boss_to_rhai, boss_view_to_boss_data, BossView, ExtraLineView};
pub use builtins::{builtins_of, Builtin, BUILTINS, DEFAULT_DISPATCHER_SRC, SCRIPT_TEAM_SRC};
pub use error::{PluginError, PluginResult};
pub use eval::Evaluation;
pub use manifest::Manifest;
pub use registry::{dir_of, PluginFile, PluginRegistry, UPLOAD_EXTENSIONS};
pub use sandbox::{
    make_evaluator_callback, Context, EvaluatorCallback, EvaluatorFn, Plugin, PluginKind,
    ReportCallback, ReportFn, DEFAULT_MAX_ARRAY_SIZE, DEFAULT_MAX_CALL_DEPTH,
    DEFAULT_MAX_EXPR_DEPTH, DEFAULT_MAX_OPERATIONS, DEFAULT_MAX_STRING_SIZE,
};

/// 测试辅助（构造头目、加载内建插件）。
///
/// 公开而非 `#[cfg(test)]`：集成测试在独立 crate 里编译，
/// 拿不到 `cfg(test)` 的东西。
pub mod test_support;
