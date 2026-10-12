//! 逐字回归测试：把 Rust 打法引擎的输出与原版 Python 引擎的输出逐字比对。
//!
//! 基准数据由 `tools/gen_golden_cases.py` 从原版 Python 实现生成，
//! 存放在 `tests/fixtures/golden_cases.json`。
//!
//! 这是本次重写的**验收核心**：任何一个用例输出不一致，都说明行为发生了偏移。

use std::path::{Path, PathBuf};

use alpha_core::models::{BossData, ExtraLine, Gender};
use alpha_core::pokedex::Pokedex;
use alpha_strategy::engine::generate_report;
use alpha_strategy::rules::Rules;
use serde::Deserialize;

#[derive(Deserialize)]
struct Case {
    tag: String,
    input: CaseInput,
    output: std::collections::HashMap<String, String>,
}

#[derive(Deserialize)]
struct CaseInput {
    name: String,
    ability: String,
    moves: Vec<String>,
    male: Option<f64>,
    #[serde(default, deserialize_with = "null_as_empty")]
    period: String,
    #[serde(default, deserialize_with = "null_as_empty")]
    location: String,
    #[serde(default, deserialize_with = "null_as_empty")]
    location_en: String,
    #[serde(default, deserialize_with = "null_as_empty_seq")]
    egg_groups: Vec<String>,
    #[serde(default, deserialize_with = "null_as_empty_seq")]
    egg_groups_en: Vec<String>,
    #[serde(default)]
    reporter: Option<String>,
    #[serde(default, deserialize_with = "null_as_empty_seq")]
    extra_lines: Vec<(String, String)>,
    #[serde(default, deserialize_with = "null_as_empty_seq")]
    #[allow(dead_code)]
    langs: Vec<String>,
    pokedex_id: Option<i64>,
    ability_id: Option<i64>,
    #[serde(default, deserialize_with = "null_as_empty_seq")]
    move_ids: Vec<Option<i64>>,
}

/// JSON 里的 `null` 视作空串（Python 侧可选字段会序列化成 null）。
fn null_as_empty<'de, D>(d: D) -> Result<String, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Ok(Option::<String>::deserialize(d)?.unwrap_or_default())
}

/// JSON 里的 `null` 视作空列表。
fn null_as_empty_seq<'de, D, T>(d: D) -> Result<Vec<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: serde::Deserialize<'de>,
{
    Ok(Option::<Vec<T>>::deserialize(d)?.unwrap_or_default())
}

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf()
}

fn fixtures_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("golden_cases.json")
}

/// 用**原版 Python 产出的 id 与规范名**构造 BossData，
/// 这样比对的是「引擎逻辑」而不是「图鉴解析」——两者分别有独立测试。
fn build_boss(c: &Case) -> BossData {
    let extra = c
        .input
        .extra_lines
        .clone()
        .into_iter()
        .map(|(zh, en)| ExtraLine::new(zh, en))
        .collect();
    BossData {
        name: c.input.name.clone(),
        ability: c.input.ability.clone(),
        moves: c.input.moves.clone(),
        period: c.input.period.clone(),
        location: c.input.location.clone(),
        location_en: c.input.location_en.clone(),
        reporter: c.input.reporter.clone(),
        gender: Gender::from_male_percent(c.input.male),
        egg_groups: c.input.egg_groups.clone(),
        egg_groups_en: c.input.egg_groups_en.clone(),
        extra_lines: extra,
        pokedex_id: c.input.pokedex_id,
        ability_id: c.input.ability_id,
        move_ids: c.input.move_ids.clone(),
        source: String::new(),
        reported_at: String::new(),
    }
}

#[test]
fn strategy_engine_matches_python_golden() {
    let root = workspace_root();
    let rules_text = std::fs::read_to_string(root.join("config/rules.yaml")).unwrap();
    let pokedex: &'static Pokedex = alpha_core::pokedex::get_pokedex().unwrap();
    let rules = Rules::from_yaml(&rules_text, pokedex).unwrap();

    let raw = std::fs::read_to_string(fixtures_path()).expect("缺少 golden_cases.json");
    let cases: Vec<Case> = serde_json::from_str(&raw).unwrap();
    assert!(!cases.is_empty(), "基准用例为空");

    let mut failures: Vec<String> = Vec::new();
    let mut checked = 0usize;

    for case in cases.iter() {
        let boss = build_boss(case);
        for (lang, expected) in case.output.iter() {
            let actual = generate_report(&boss, &rules, pokedex, lang, None);
            checked += 1;
            if actual != *expected {
                failures.push(format!(
                    "\n=== 用例[{}] 语言[{}] 输出不一致 ===\n--- 期望(Python) ---\n{}\n--- 实际(Rust) ---\n{}\n--- 差异 ---\n{}",
                    case.tag,
                    lang,
                    expected,
                    actual,
                    diff_lines(expected, &actual)
                ));
            }
        }
    }

    if !failures.is_empty() {
        panic!(
            "{} / {} 组输出不一致:\n{}",
            failures.len(),
            checked,
            failures.join("\n")
        );
    }
    eprintln!("✓ {} 组输出与原版逐字一致（{} 个用例）", checked, cases.len());
}

/// 逐行差异，方便定位偏移。
fn diff_lines(expected: &str, actual: &str) -> String {
    let e: Vec<&str> = expected.lines().collect();
    let a: Vec<&str> = actual.lines().collect();
    let mut out = Vec::new();
    let n = e.len().max(a.len());
    for i in 0..n {
        let le = e.get(i).copied().unwrap_or("<缺失>");
        let la = a.get(i).copied().unwrap_or("<缺失>");
        if le != la {
            out.push(format!("  行{}: 期望 {:?} / 实际 {:?}", i + 1, le, la));
        }
    }
    if out.is_empty() {
        out.push("  （行内容相同，差异在空白字符）".to_string());
    }
    out.join("\n")
}
