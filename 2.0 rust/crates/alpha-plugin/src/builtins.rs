//! 内建插件。
//!
//! # 内建 vs 上传
//!
//! | | 内建 | 上传 |
//! |---|---|---|
//! | 来源 | 编译进二进制的 `plugins/*.rhai` | 面板上传到 `data/plugins/` |
//! | 能否删除 | **不能**（`is_builtin=1`） | 能 |
//! | 首次启动 | 自动写入数据库并置为激活 | — |
//!
//! # 为什么内建插件也走 Rhai 而不是直接写 Rust
//!
//! 「默认决策器」和「脚本队评估器」的逻辑，用户**一定**会想改
//! （现网就已经有人改过评估器的评语）。写成 Rhai 有三个好处：
//!
//! 1. 面板「查看源码」能直接展示 —— 用户照着改就是自己的插件；
//! 2. 内建与上传走**完全相同**的代码路径，不存在「内建的能用、上传的不行」
//!    这种只在用户手里才暴露的差异；
//! 3. 改逻辑不用重新编译部署 —— 否则每次微调评语都得走一遍发版。
//!
//! # 与原版的兼容
//!
//! 原版内置的是 `default_dispatcher.py` + `script_team.py`。现网实际激活的
//! 是用户改过评语的 `user_jbdui.py`（经逐用例验证：评分逻辑完全一致，
//! 只有两句文案不同）。按用户要求**合并成一个内建评估器**，
//! 直接采用现网在用的那套评语。

use crate::error::PluginResult;
use crate::sandbox::{Plugin, PluginKind};

/// 内建「默认决策器」的源码。
///
/// 它什么额外内容都不加，直接转发官方引擎的输出 ——
/// 对应原版 `default_dispatcher.py`。
pub const DEFAULT_DISPATCHER_SRC: &str = include_str!("../plugins/default_dispatcher.rhai");

/// 内建「脚本队评估器」的源码。
pub const SCRIPT_TEAM_SRC: &str = include_str!("../plugins/script_team.rhai");

/// 内建插件描述。
pub struct Builtin {
    /// 数据库里 `name` 列的默认值
    pub name: &'static str,
    /// 数据库里 `filename` 列的默认值
    pub filename: &'static str,
    /// 数据库里 `description` 列的默认值
    pub description: &'static str,
    pub kind: PluginKind,
    pub source: &'static str,
}

/// 全部内建插件。
pub const BUILTINS: &[Builtin] = &[
    Builtin {
        name: "默认决策器",
        filename: "default_dispatcher.rhai",
        description: "内置：基于 alpha-strategy 的官方打法引擎",
        kind: PluginKind::Dispatcher,
        source: DEFAULT_DISPATCHER_SRC,
    },
    Builtin {
        name: "脚本队评估器",
        filename: "script_team.rhai",
        description: "内置：评估脚本队（呆壳兽核心）能否无随机性推进，0-18 评分",
        kind: PluginKind::Evaluator,
        source: SCRIPT_TEAM_SRC,
    },
];

/// 按类型取内建插件。
pub fn builtins_of(kind: PluginKind) -> Vec<&'static Builtin> {
    BUILTINS.iter().filter(|b| b.kind == kind).collect()
}

/// 编译内建「默认决策器」。
pub fn default_dispatcher() -> PluginResult<Plugin> {
    Plugin::compile_inline(DEFAULT_DISPATCHER_SRC, PluginKind::Dispatcher)
}

/// 编译内建「脚本队评估器」。
pub fn script_team() -> PluginResult<Plugin> {
    Plugin::compile_inline(SCRIPT_TEAM_SRC, PluginKind::Evaluator)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 内建源码必须全部能编译通过 —— 编译不过等于面板一启动就没插件可用。
    #[test]
    fn all_builtins_compile() {
        for b in BUILTINS {
            let p = Plugin::compile_inline(b.source, b.kind);
            assert!(
                p.is_ok(),
                "内建插件 {} 编译失败: {:?}",
                b.name,
                p.err().map(|e| e.to_string())
            );
            let p = p.unwrap();
            assert_eq!(
                p.manifest().name, b.name,
                "内建插件 {} 的 @name 与注册名不一致",
                b.name
            );
        }
    }

    /// 每种类型都必须至少有一个内建插件，否则面板首次启动会是空列表。
    #[test]
    fn each_kind_has_a_builtin() {
        assert!(!builtins_of(PluginKind::Dispatcher).is_empty());
        assert!(!builtins_of(PluginKind::Evaluator).is_empty());
    }

    /// 内建插件的 `filename` 必须唯一 —— 它们会写进同一个目录。
    #[test]
    fn builtin_filenames_are_unique() {
        let mut names: Vec<&str> = BUILTINS.iter().map(|b| b.filename).collect();
        names.sort_unstable();
        let before = names.len();
        names.dedup();
        assert_eq!(names.len(), before, "内建 filename 有重复");
    }
}
