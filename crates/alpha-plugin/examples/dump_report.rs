//! 手工看一眼内建决策器产出的报告长什么样。
//!
//! ```text
//! cargo run -p alpha-plugin --example dump_report -- 巨牙鲨 挑衅 近身战
//! ```
use alpha_plugin::{builtins, test_support::boss_full, Context};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (name, moves) = args.split_first().expect("用法: dump_report <精灵名> [技能...]");
    let moves: Vec<&str> = moves.iter().map(String::as_str).collect();

    let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|p| p.parent())
        .unwrap()
        .to_path_buf();

    let p = builtins::default_dispatcher().unwrap();
    let boss = boss_full(name, "", &moves, Some(50.0));

    for (label, ctx) in [
        ("===== zh =====", Context::zh()),
        ("===== en =====", Context::en()),
    ] {
        println!("{label}");
        println!("{}", p.dispatch(&boss, &ctx.with_repo_rules(&root)).unwrap());
        println!();
    }
}
