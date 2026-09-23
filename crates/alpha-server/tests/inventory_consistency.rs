//! 内置插件「数据库种子」与「磁盘物化」两份清单的一致性。
//!
//! # 为什么要有这条测试
//!
//! 内置插件的名字存在于**两个**地方：
//!
//! 1. `alpha-store::schema::DEFAULT_DISPATCHER` —— 首次启动写进数据库的行
//! 2. `alpha-plugin::builtins::BUILTINS` —— 启动时物化到 `data/plugins/` 的文件
//!
//! 两边对不上的后果：数据库那行指向一个**永远不会被物化的文件**，
//! `/debug/run` 报「分发器文件不存在」。这真的发生过 —— `filename`
//! 一度还是 `default_dispatcher.py`，而物化出来的是 `.rhai`，
//! 全新部署的决策器直接不可用，且只有真机点开调试台才暴露。
//!
//! 单元测试当时全绿，因为每套测试都只看自己那一半。
//! 这条测试存在的意义就是**把两半钉在一起**。

use alpha_plugin::builtins::BUILTINS;
use alpha_store::schema::{DEFAULT_DISPATCHER, DEFAULT_EVALUATOR};

/// 数据库种子的文件名必须能在物化清单里找到同名项。
#[test]
fn seeded_rows_match_the_materialized_builtins() {
    for (what, seeded) in [
        ("默认决策器", (DEFAULT_DISPATCHER.0, DEFAULT_DISPATCHER.1)),
        ("脚本队评估器", (DEFAULT_EVALUATOR.0, DEFAULT_EVALUATOR.1)),
    ] {
        let hit = BUILTINS.iter().find(|b| b.filename == seeded.1);

        let hit = hit.unwrap_or_else(|| {
            panic!(
                "数据库种子 {what:?} 的 filename={:?} 在 alpha-plugin::BUILTINS 里找不到 —— \
                 首次启动后这行会指向一个永不存在的文件（决策器/评估器直接不可用）",
                seeded.1
            )
        });

        assert_eq!(
            seeded.0, hit.name,
            "种子行 name 与物化清单 name 不一致（面板上会显示成两个东西）"
        );
        // Rhai 是唯一的插件形态 —— 出现 .py 就是把原版清单抄回来了
        assert!(
            seeded.1.ends_with(".rhai"),
            "种子 filename 必须是 .rhai（原版的 .py 在 Rust 版里永远不会存在）: {seeded:?}"
        );
    }
}

/// 物化清单里每个条目都要有种子 —— 反方向的覆盖也要对齐。
///
/// 不强求一一对应（将来可能出现「物化了但不当默认启用」的内置插件），
/// 但**当前**清单里的两个内置插件必须都有种子行，否则首次启动后
/// 它们在插件页的 is_builtin 状态和实际不符。
#[test]
fn every_current_builtin_has_a_seeded_row() {
    assert_eq!(BUILTINS.len(), 2, "新增内置插件时，请同步更新本测试与 schema 常量");

    for b in BUILTINS {
        let seeded_name = match b.filename {
            "default_dispatcher.rhai" => DEFAULT_DISPATCHER.0,
            "script_team.rhai" => DEFAULT_EVALUATOR.0,
            other => panic!("未知的内置插件 filename: {other:?} —— 请在 schema.rs 加对应的种子常量"),
        };
        assert_eq!(seeded_name, b.name, "{:?} 的名字两边不一致", b.filename);
    }
}
