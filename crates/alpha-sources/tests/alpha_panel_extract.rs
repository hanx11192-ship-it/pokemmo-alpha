//! `extract_alpha_panel` 的一致性回归。
//!
//! # 这个测试在防什么
//!
//! 原版 Python 用**前瞻断言**截取 `#pokedex-alpha` 面板：
//!
//! ```python
//! re.search(r'id="pokedex-alpha".*?(?=<div id="pokedex-[a-z]|</div>\s*</div>\s*</div>\s*</div>)', html, re.S)
//! ```
//!
//! Rust 的 `regex` crate **不支持前瞻**（照搬会 panic），所以 Rust 版手写了
//! 一个扫描器。手写代码最容易在边界上跑偏，而这个函数一旦多截或少截，
//! 后面按 `(region, location)` 匹配 spawn-entry 就会拿到错误的候选集 ——
//! 表现为「技能列表莫名少了几个」这种极难定位的线上问题。
//!
//! 所以这里用 Python 当 oracle，把两个终止条件的各种组合都钉死。
//!
//! 重新生成期望值：
//! ```bash
//! python3 tools/gen_alpha_panel_cases.py crates/alpha-sources/tests/fixtures/alpha_panel_cases.json
//! ```

use serde::Deserialize;

#[derive(Debug, Deserialize)]
struct Case {
    name: String,
    html: String,
    panel: String,
}

fn cases() -> Vec<Case> {
    let raw = include_str!("fixtures/alpha_panel_cases.json");
    serde_json::from_str(raw).expect("解析 oracle 用例")
}

/// 全部用例都必须与原版 Python 的 Python 前瞻断言逐字一致。
#[test]
fn extract_alpha_panel_matches_python() {
    let all = cases();
    assert!(!all.is_empty(), "用例集不能为空");

    let mut checked = 0;
    for c in &all {
        let got = alpha_sources::pokemmotools_landing::extract_alpha_panel(&c.html);
        assert_eq!(
            got, c.panel,
            "用例 `{}` 与原版不一致\n  期望: {:?}\n  实际: {:?}",
            c.name, c.panel, got
        );
        checked += 1;
    }
    println!("✓ {checked} 个用例与原版 Python 前瞻断言逐字一致");
}

/// 两个终止条件同时存在时，必须取**更近**的那个。
///
/// 这是扫描器最容易错的地方：只看一个条件（比如只找兄弟面板）在顺手的
/// 用例上能过，一旦另一条件更近就多截一段。
#[test]
fn takes_the_nearer_terminator() {
    let case = |name: &str| {
        cases()
            .into_iter()
            .find(|c| c.name == name)
            .unwrap_or_else(|| panic!("缺少用例 {name}"))
    };

    // 兄弟面板更近：截到它之前
    let c = case("both_terminators_panel_wins");
    let got = alpha_sources::pokemmotools_landing::extract_alpha_panel(&c.html);
    assert!(
        !got.contains("pokedex-moves"),
        "兄弟面板更近时应在此之前截断: {got:?}"
    );
    assert!(got.contains("<article>x</article>"), "面板内容应保留: {got:?}");

    // 4 个 div 更近：同样在 `</div>` 段**之前**截断（前瞻不消费结束标记）
    let c = case("both_terminators_divs_win");
    let got = alpha_sources::pokemmotools_landing::extract_alpha_panel(&c.html);
    assert!(
        !got.contains("</div>"),
        "4 个 div 处截断时不应把结束标记包进来: {got:?}"
    );
    assert!(got.contains("<article>y</article>"), "面板内容应保留: {got:?}");
    assert!(!got.contains("pokedex-moves"), "更远的兄弟面板不该被包含");

    // 两条路径截出来的长度应当一致 —— 终点都是「面板内容之后」
    assert_eq!(
        case("both_terminators_panel_wins").panel.len(),
        case("both_terminators_divs_win").panel.len()
    );
}

/// 找不到面板头时必须回退成整篇 HTML —— 原版是 `panel = html if not alpha else ...`，
/// 回退成空串会让本源彻底解析不出技能，且没有任何报错。
#[test]
fn falls_back_to_whole_html() {
    let c = cases()
        .into_iter()
        .find(|c| c.name == "no_alpha_panel")
        .unwrap();
    let got = alpha_sources::pokemmotools_landing::extract_alpha_panel(&c.html);
    assert_eq!(got, c.html);
    assert!(!got.is_empty());
}

/// 空输入不能 panic（面板首次部署、上游返回空页时会出现）。
#[test]
fn handles_empty_input() {
    assert_eq!(
        alpha_sources::pokemmotools_landing::extract_alpha_panel(""),
        ""
    );
}

/// 面板含中文等多字节字符时，扫描必须按字符边界走 ——
/// 否则 `html[i..]` 会在 UTF-8 中间切片而 panic。
#[test]
fn handles_multibyte_content_without_panicking() {
    let c = cases()
        .into_iter()
        .find(|c| c.name == "multibyte_content")
        .unwrap();
    let got = alpha_sources::pokemmotools_landing::extract_alpha_panel(&c.html);
    assert_eq!(got, c.panel);
    assert!(got.contains("雷电兽·湖泊"), "内容应完整保留");
}

/// 4 个 `</div>` 之间允许空白（原版 `\s*`），且空白不打断连续计数。
#[test]
fn whitespace_does_not_break_the_run() {
    let c = cases()
        .into_iter()
        .find(|c| c.name == "whitespace_between_divs")
        .unwrap();
    let got = alpha_sources::pokemmotools_landing::extract_alpha_panel(&c.html);
    assert_eq!(got, c.panel);
}

/// 只有 3 个连续 `</div>` 时**不该**截断 —— 一定要 4 个。
#[test]
fn three_divs_is_not_enough() {
    let html = r#"<div id="pokedex-alpha"><article>x</article></div></div></div><p>SHOULD_STAY</p>"#;
    let got = alpha_sources::pokemmotools_landing::extract_alpha_panel(html);
    assert!(
        got.contains("SHOULD_STAY"),
        "3 个 div 不足以判定面板结束: {got:?}"
    );
}

/// 面板头是搜索的起点：之前的内容必须被丢掉。
#[test]
fn drops_everything_before_the_panel() {
    let html = r#"<div id="pokedex-base">BEFORE_MARKER</div><div id="pokedex-alpha">KEEP</div></div></div></div></div>"#;
    let got = alpha_sources::pokemmotools_landing::extract_alpha_panel(html);
    assert!(!got.contains("BEFORE_MARKER"), "应丢弃面板之前的内容");
    assert!(got.contains("KEEP"));
}
