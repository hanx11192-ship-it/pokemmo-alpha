//! 端到端验证：内建决策器通过 `engine_report` 真的能拿到官方引擎的报告。
//!
//! # 为什么这条测试必须存在
//!
//! `default_dispatcher.rhai` 的全部内容就是一行 `engine_report(boss, ctx)`。
//! 在写这条测试之前，它只被验证到「能编译」和「实现了 dispatch 函数」——
//! 但 `engine_report` 背后是一条**跨语言调用链**：
//!
//! ```text
//!   Rhai 脚本 fn dispatch
//!     → engine_report(boss, ctx)            （Rhai 侧注册的函数）
//!       → sandbox::call_report_hook         （把 Rhai Map 还原成 BossView）
//!         → thread_local REPORT_HOOK        （取当前线程挂着的闭包）
//!           → Context::with_report 注入的闭包
//!             → alpha_strategy::generate_report（真正的打法引擎）
//! ```
//!
//! 这条链上任何一环断了（字段名对不上、`lang` 传丢、
//! `boss_from_rhai` 丢字段），表现都是**静默的空字符串** ——
//! 面板不会报错，只是推送内容变成空的。这种故障在用户侧极难定位，
//! 所以要在测试里钉死。
//!
//! # 覆盖的三件事
//!
//! 1. 整条链路通：`engine_report` 返回**非空**且内容合理；
//! 2. 语言传对了：中文模式出中文、英文模式出英文；
//! 3. 头目数据没丢：报告里出现这只头目的名字。

use std::path::PathBuf;

use alpha_plugin::test_support::boss_full;
use alpha_plugin::{builtins, Context, Plugin};

/// 仓库根目录（`crates/alpha-plugin/` 往上两级）。
fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|p| p.parent())
        .expect("应有仓库根目录")
        .to_path_buf()
}

/// 确认 `config/rules.yaml` 在场 —— 不在的话下面所有断言都会因为
/// 「报告是空串」而失败，报错信息会指向错误的地方（看起来像引擎坏了）。
/// 单独先检查一遍，让失败信息直接说明真实原因。
fn assert_rules_available() {
    let path = repo_root().join("config/rules.yaml");
    assert!(
        path.exists(),
        "找不到规则文件 {} —— 端到端测试需要它才能生成报告",
        path.display()
    );
}

/// 编译内建决策器。
fn dispatcher() -> Plugin {
    builtins::default_dispatcher().expect("内建决策器应能编译")
}

/// **核心断言**：`engine_report` 整条链路通，产出非空报告。
#[test]
fn engine_report_produces_a_non_empty_report() {
    assert_rules_available();

    let p = dispatcher();
    let boss = boss_full("巨牙鲨", "粗糙皮肤", &["挑衅", "近身战", "水流喷射"], Some(50.0));

    let ctx = Context::zh().with_repo_rules(repo_root());
    let out = p
        .dispatch(&boss, &ctx)
        .expect("决策器执行失败（engine_report 链路断了）");

    assert!(
        !out.trim().is_empty(),
        "engine_report 返回了空报告 —— 链路某一环断了。\n\
         常见原因：config/rules.yaml 读不到、BossData 转换丢了字段、\
         REPORT_HOOK 没装上。"
    );

    // 报告必须提到这只头目，否则说明 boss 数据在转换过程中丢了
    assert!(
        out.contains("巨牙鲨"),
        "报告里没有出现头目名，说明头目数据没传进引擎。\n实际报告：\n{out}"
    );
}

/// 语言模式要真的生效 —— `ctx.lang` 必须沿着链路传到引擎。
#[test]
fn language_is_forwarded_through_the_whole_chain() {
    assert_rules_available();

    let p = dispatcher();
    let boss = boss_full("巨牙鲨", "粗糙皮肤", &["挑衅", "近身战"], Some(50.0));

    let zh = p
        .dispatch(&boss, &Context::zh().with_repo_rules(repo_root()))
        .expect("中文模式执行失败");
    let en = p
        .dispatch(&boss, &Context::en().with_repo_rules(repo_root()))
        .expect("英文模式执行失败");

    assert!(!zh.is_empty(), "中文报告为空");
    assert!(!en.is_empty(), "英文报告为空");
    assert_ne!(
        zh, en,
        "中英文报告完全一样 —— ctx.lang 没有传到引擎（链路里把语言丢了）"
    );

    // 中文报告应含中文名；英文报告应含英文名（图鉴里 巨牙鲨 = Sharpedo）
    assert!(zh.contains("巨牙鲨"), "中文报告里没有中文头目名：\n{zh}");
    assert!(
        en.contains("Sharpedo"),
        "英文报告里没有英文头目名（应为 Sharpedo）：\n{en}"
    );
}

/// 没注入报告生成器时，`engine_report` 返回空串而不是报错。
///
/// 这是 `build_engine` 里那个「注册成永远存在」的取舍的直接后果 ——
/// 把它写成测试，是因为这个行为**看起来像 bug**（静默空串），
/// 所以必须有一处地方说明「这是有意的，不是漏了」。
#[test]
fn engine_report_is_empty_when_no_hook_is_installed() {
    assert_rules_available();

    let p = dispatcher();
    let boss = boss_full("巨牙鲨", "粗糙皮肤", &["挑衅"], Some(50.0));

    // 刻意用不注入 report 的 Context
    let out = p
        .dispatch(&boss, &Context::zh())
        .expect("没注入钩子时不该抛异常");

    assert_eq!(
        out, "",
        "没注入生成器时应返回空串（有意如此，见 sandbox::build_engine 的注释）"
    );
}

/// `engine_report` 只是决策器可用的函数；评估器不该拿到它。
///
/// 与 `sandbox.rs` 里的单元测试同源，这里从内建插件这一侧再确认一次 ——
/// 因为「内建评估器误用了 engine_report」是实际会发生的写法错误。
#[test]
fn builtin_evaluator_does_not_use_engine_report() {
    let src = alpha_plugin::SCRIPT_TEAM_SRC;
    // 注释里提到它是可以的（文档会讲这个函数），代码里不该出现调用
    let code: String = src
        .lines()
        .filter(|l| !l.trim_start().starts_with("//"))
        .collect::<Vec<_>>()
        .join("\n");

    assert!(
        !code.contains("engine_report"),
        "内建评估器在代码里调用了 engine_report —— 评估器不产生报告"
    );
}

/// 报告在多语言（双语）模式下也应该正常 —— 调度器会用这个模式。
#[test]
fn bilingual_context_still_produces_a_report() {
    assert_rules_available();

    let p = dispatcher();
    let boss = boss_full("巨牙鲨", "粗糙皮肤", &["挑衅", "近身战"], Some(50.0));

    let out = p
        .dispatch(&boss, &Context::bilingual().with_repo_rules(repo_root()))
        .expect("双语模式决策器执行失败");

    assert!(!out.trim().is_empty(), "双语模式报告为空");
}
