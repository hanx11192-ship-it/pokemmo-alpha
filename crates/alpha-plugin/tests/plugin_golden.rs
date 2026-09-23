//! 内建评估器的**逐字回归**：Rhai 版必须与现网 Python 版给出完全一样的结果。
//!
//! # 为什么期望值全部来自 Python
//!
//! 把 `user_jbdui.py` 的评分逻辑翻译成 Rhai，最容易出的错不是「跑不起来」，
//! 而是**算错一分**、**漏掉一个分支**、**factors 里的顺序不一样**。
//! 这类错误手写断言根本发现不了 —— 因为断言的期望值也是我手写的，
//! 我脑子里想的那套逻辑和代码里那套逻辑同时错，测试照样绿。
//!
//! 所以这里一个期望值都不手写：全部由
//! `tools/gen_plugin_golden.py` 跑**现网在用的** `user_jbdui.py` 生成，
//! 存进 `fixtures/plugin_golden.json`。测试只负责「跑 Rhai、逐字段比」。
//!
//! 重新生成（改了评估器逻辑时）：
//!
//! ```text
//! python3 tools/gen_plugin_golden.py \
//!     /workspace/pokemmo_alpha_orig \
//!     crates/alpha-plugin/tests/fixtures/plugin_golden.json
//! ```
//!
//! # 覆盖了哪些分支
//!
//! 34 个用例覆盖评分模型的 5 个阶段，以及几个**翻译时最容易翻错**的细节：
//!
//! | 阶段 / 细节 | 对应用例 |
//! |---|---|
//! | 小丑头（含顺序颠倒、超集、子集） | `clown_*` |
//! | 基础项 ×4 类 + 同类叠加 | `base_*` |
//! | 梦话 ×4 分支 | `talk_*` |
//! | 特判（巨牙鲨 / 斗笠菇，含部分命中） | `special_*` |
//! | clamp 上限 | `clamp_*` |
//! | 空技能 / 未知技能 / 未知精灵 / 性别比例 | `edge_*` |

use std::path::PathBuf;

use alpha_plugin::test_support::boss_full;
use alpha_plugin::{builtins, Context, Evaluation, Plugin};
use serde::Deserialize;

/// 一条 golden 用例。
///
/// 字段名与 Python 生成脚本写出的 JSON 一一对应。
#[derive(Debug, Deserialize)]
struct Case {
    /// 用例名（失败时直接指出是哪个场景）
    name: String,
    /// 精灵中文名
    pokemon: String,
    /// 特性（可能是空串）
    ability: String,
    /// 技能中文名
    moves: Vec<String>,
    /// 性别比例（`null` 表示原版没给）
    male_percent: Option<f64>,
    /// 图鉴号：Rust 侧解析出来必须一致，否则说明图鉴数据本身就有差异
    pokedex_id: Option<i64>,
    /// 技能 id 列表
    move_ids: Vec<Option<i64>>,
    /// 中文模式下的原版输出（`null` = 不评估）
    result_zh: Option<PyResult>,
    /// 英文模式下的原版输出
    result_en: Option<PyResult>,
}

/// 原版 `evaluate()` 返回的 dict（只保留可比较的键）。
#[derive(Debug, Deserialize)]
struct PyResult {
    score: Option<i64>,
    label: Option<String>,
    #[serde(default)]
    label_en: Option<String>,
    #[serde(default)]
    detail: Option<String>,
    #[serde(default)]
    factors: Vec<String>,
    #[serde(default)]
    line: Option<String>,
}

fn fixture_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/plugin_golden.json")
}

fn load_cases() -> Vec<Case> {
    let text = std::fs::read_to_string(fixture_path()).unwrap_or_else(|e| {
        panic!(
            "读不到 golden 用例 {}：{e}\n\
             请先跑 tools/gen_plugin_golden.py 生成（见本文件头部说明）",
            fixture_path().display()
        )
    });
    serde_json::from_str(&text).expect("golden 用例 JSON 解析失败")
}

/// 把 Rust 侧的 `Evaluation` 与 Python 的 dict 逐字段比对。
///
/// 字段顺序刻意与 `Evaluation` 的声明顺序一致，方便肉眼核对。
///
/// # `detail` 为什么是规范化比较
///
/// 原版 `_resolve()` 返回 Python `set`，整数 set 按**哈希桶顺序**迭代，
/// 所以因子里的技能明细顺序既不等于名单顺序、也不等于 id 升序，还随
/// 「这组解析出几个 id」而变。Rhai 版固定用名单书写顺序（理由见
/// `plugins/script_team.rhai` 里的长注释）。
///
/// 这个差异**只影响这一个调试字段**：`score` / `label` 逐字一致，
/// 推送报文那一行（`format_line` 拼的）也完全不受影响。
/// 所以这里只把 `detail` 按分隔符切开排序后比，其余字段仍然严格逐字。
fn assert_same(case: &str, lang: &str, got: &Evaluation, want: &PyResult) {
    assert_eq!(got.score, want.score, "[{case}/{lang}] score 不一致");
    assert_eq!(got.label, want.label, "[{case}/{lang}] label 不一致");
    assert_eq!(
        got.label_en, want.label_en,
        "[{case}/{lang}] label_en 不一致"
    );
    assert_eq!(
        normalize_detail(got.detail.as_deref()),
        normalize_detail(want.detail.as_deref()),
        "[{case}/{lang}] detail 的**内容集合**不一致（顺序已规范化）\n\
         实际: {:?}\n期望: {:?}",
        got.detail,
        want.detail
    );
    // factors 就是 detail 的「未拼接版」，技能名顺序同样按上面的理由规范化。
    // 因子**之间**的顺序仍然是严格的 —— 那部分两边都由代码显式决定，
    // 不受 Python set 影响，没有放宽的必要。
    assert_eq!(
        got.factors.iter().map(|f| normalize_factor(f)).collect::<Vec<_>>(),
        want.factors.iter().map(|f| normalize_factor(f)).collect::<Vec<_>>(),
        "[{case}/{lang}] factors 的**内容集合**不一致（技能名顺序已规范化）\n\
         实际: {:?}\n期望: {:?}",
        got.factors,
        want.factors
    );
    assert_eq!(got.line, want.line, "[{case}/{lang}] line 不一致");
}

/// 把 `detail` 规范化成「排序后的一串内容」，用来忽略因子内部技能名的顺序。
///
/// # 为什么不能直接 `split('/')` 排序
///
/// 因子的形状是 `翻车技×3:电磁波/唱歌/催眠术` —— `/` 前面还挂着
/// **前缀** `翻车技×3:`。如果连前缀一起排序，`翻车技×3:` 会被当成一个
/// 「技能名」参与比较，两个仅顺序不同的字符串就排不到一起（第一版就是这么
/// 写错的，测试当场指出了）。所以必须**先按 `:` 切开前缀**，
/// 前缀原样保留、只对后面的技能名排序。
///
/// 只切开、排序、拼回 —— 不丢字符。所以「少了一个因子」「技能名写错」
/// 「前缀算错了」仍然会被测出来，被忽略的**只有技能名顺序**。
fn normalize_detail(detail: Option<&str>) -> String {
    let Some(detail) = detail else {
        return String::new();
    };

    // 外层：因子之间用 `；` 分隔（顺序也要忽略，所以最后整体排序）
    let mut factors: Vec<String> = detail.split('；').map(normalize_factor).collect();
    factors.sort_unstable();
    factors.join("；")
}

/// 规范化单个因子：`前缀:技能/技能` → `前缀:技能/技能`（技能名排序）。
///
/// 没有 `:` 的因子（如 `梦话+翻车技:+2`）原样返回 —— 那个 `:+2` 里的
/// `:` 是加号分隔的一部分，不该被当作「前缀分界」乱切。
/// 判据用「`:` 后面是否含 `/`」来区分：只有带技能列表的因子才需要排序。
fn normalize_factor(factor: &str) -> String {
    let Some((prefix, names)) = factor.split_once(':') else {
        return factor.to_string();
    };
    if !names.contains('/') {
        return factor.to_string();
    }
    let mut inner: Vec<&str> = names.split('/').collect();
    inner.sort_unstable();
    format!("{prefix}:{}", inner.join("/"))
}

/// 这条测试把「detail 顺序差异」的意义钉住：**只差顺序，不差内容**。
///
/// 如果哪天真要做成逐字一致（复刻 CPython set），把它删掉即可 ——
/// 但那时应该同时确认推送报文确实没受影响。
#[test]
fn detail_ordering_is_normalized() {
    // 顺序不同、内容相同 → 规范化后相等
    assert_eq!(
        normalize_detail(Some("翻车技×3:电磁波/唱歌/催眠术")),
        normalize_detail(Some("翻车技×3:唱歌/电磁波/催眠术"))
    );

    // 内容不同 → 仍然测得出来（这是规范化不能掩盖的）
    assert_ne!(
        normalize_detail(Some("翻车技×3:电磁波/唱歌/催眠术")),
        normalize_detail(Some("翻车技×3:电磁波/唱歌"))
    );

    // 多因子时整体顺序也被忽略
    assert_eq!(
        normalize_detail(Some("翻车技×1:电磁波；先手技×1:音速拳")),
        normalize_detail(Some("先手技×1:音速拳；翻车技×1:电磁波"))
    );

    assert_eq!(normalize_detail(None), "");
    assert_eq!(normalize_detail(Some("")), "");
}

/// 主回归：34 个用例、中英双模式，全部逐字段对齐。
///
/// 一个测试函数跑完所有用例（而不是 34 个测试函数），是为了让失败时
/// `assert` 里的用例名把场景说清楚 —— 34 个几乎同名的测试函数
/// 反而不好读。
#[test]
fn rhai_evaluator_matches_python_on_every_case() {
    let cases = load_cases();
    assert!(!cases.is_empty(), "golden 用例为空");

    let plugin: Plugin = builtins::script_team().expect("内建评估器应能编译");

    let mut checked = 0usize;
    for c in &cases {
        let boss = boss_full(
            &c.pokemon,
            &c.ability,
            &c.moves.iter().map(String::as_str).collect::<Vec<_>>(),
            c.male_percent,
        );

        // 先确认输入侧就是同一个头目 —— 图鉴解析若不一致，
        // 下面的评分差异就无从归因了，会浪费大量排查时间。
        assert_eq!(
            boss.pokedex_id, c.pokedex_id,
            "[{}] 图鉴号解析不一致（输入侧就分叉了）",
            c.name
        );
        assert_eq!(
            boss.move_ids, c.move_ids,
            "[{}] 技能 id 解析不一致（输入侧就分叉了）",
            c.name
        );

        for (lang, want) in [("zh", &c.result_zh), ("en", &c.result_en)] {
            let ctx = if lang == "en" {
                Context::en()
            } else {
                Context::zh()
            };
            let got = plugin
                .evaluate(&boss, &ctx)
                .unwrap_or_else(|e| panic!("[{}] {lang} 模式执行失败: {e}", c.name));

            match want {
                None => assert!(
                    got.is_none(),
                    "[{}] {lang}: 原版返回 None（不评估），Rhai 返回了评估结果 {got:?}",
                    c.name
                ),
                Some(w) => {
                    let got = got.unwrap_or_else(|| {
                        panic!("[{}] {lang}: 原版有结果，Rhai 返回了 ()（不评估）", c.name)
                    });
                    assert_same(&c.name, lang, &got, w);
                    checked += 1;
                }
            }
        }
    }

    assert!(checked >= 30, "实际比对的评估结果只有 {checked} 条，覆盖不足");
    eprintln!("逐字回归通过：{} 个用例 / {checked} 条评估结果", cases.len());
}

/// `format_line` 也要对齐 —— 这行字是用户在微信里直接看到的。
///
/// 单独测是因为它**不是**评估器算出来的：`Evaluation::format_line`
/// 是 Rust 侧新写的拼接代码，Python 的对应实现是 `panel/evaluate.py` 里
/// 的 `format_line()`。两边都得独立正确。
#[test]
fn format_line_matches_python_templates() {
    let cases = load_cases();
    let plugin = builtins::script_team().expect("内建评估器应能编译");

    for c in &cases {
        let boss = boss_full(
            &c.pokemon,
            &c.ability,
            &c.moves.iter().map(String::as_str).collect::<Vec<_>>(),
            c.male_percent,
        );

        for (lang, want) in [("zh", &c.result_zh), ("en", &c.result_en)] {
            let Some(want) = want else { continue };
            let ctx = if lang == "en" {
                Context::en()
            } else {
                Context::zh()
            };
            let got = plugin.evaluate(&boss, &ctx).unwrap().unwrap();
            let line = got.format_line(lang);

            // 原版 format_line 的模板（含 score 缺失时只输出 label 的分支）
            let label = want.label.as_deref().unwrap_or("");
            let expected = match (lang, want.score) {
                ("en", Some(s)) => format!("{label} Score {s}"),
                ("en", None) => label.to_string(),
                (_, Some(s)) => format!("{label} 评分{s}"),
                (_, None) => label.to_string(),
            };

            assert_eq!(
                line, expected,
                "[{}/{}] 报文行不一致",
                c.name, lang
            );
        }
    }
}

/// 原版「不评估」的语义是返回 `None`，不是返回空 dict。
///
/// 这个区别在面板上是可见的：`None` → 不显示这一行；
/// 空 dict → 显示一个空行。现网评估器没有这个分支，
/// 所以用一个临时插件钉住契约，防止以后有人「优化」成返回 `#{}`。
#[test]
fn empty_map_is_not_the_same_as_no_evaluation() {
    let p = Plugin::compile_inline(
        r#"fn evaluate(boss, ctx) { #{} }"#,
        alpha_plugin::PluginKind::Evaluator,
    )
    .unwrap();

    let got = p
        .evaluate(&alpha_plugin::test_support::boss("皮卡丘", &[]), &Context::zh())
        .unwrap();

    let got = got.expect("空 map 应解析成一个（空的）评估结果，而不是 None");
    assert!(got.is_empty(), "空 map 的各字段都该是 None/空");
    assert_eq!(got.format_line("zh"), "", "空评估结果格式化后应是空串");
}

/// 同一个插件实例被反复调用（面板每 60 秒轮询就是这么用的），
/// 结果必须稳定 —— 脚本不能用全局状态影响下一次调用。
#[test]
fn repeated_evaluation_is_stable() {
    let cases = load_cases();
    let plugin = builtins::script_team().expect("内建评估器应能编译");

    // 挑一个 factors 最多、逻辑分支最复杂的用例
    let c = cases
        .iter()
        .max_by_key(|c| c.result_zh.as_ref().map_or(0, |r| r.factors.len()))
        .expect("至少有一个用例");

    let boss = boss_full(
        &c.pokemon,
        &c.ability,
        &c.moves.iter().map(String::as_str).collect::<Vec<_>>(),
        c.male_percent,
    );

    let first = plugin.evaluate(&boss, &Context::zh()).unwrap();
    for i in 1..20 {
        let again = plugin.evaluate(&boss, &Context::zh()).unwrap();
        assert_eq!(first, again, "[{}] 第 {i} 次调用结果与首次不同", c.name);
    }
}

// ---------------------------------------------------------------------------
// 性别过滤（新需求，原版 Python 没有此逻辑 —— golden 对齐不含它）
// ---------------------------------------------------------------------------
//
// 脚本队的推进依赖异性配队机制：单性别 / 无性别的头目打不了，评了也白评。
// 规则：只公 / 只母 / 无性别 → 不评估（evaluate 返回 ()）；
// 白名单（艾路雷朵/Gallade）豁免；gender 文本解析失败时宁可误评不错杀。

/// 双性别照常评估 —— 过滤不能误伤正常头目。
#[test]
fn dual_gender_is_still_evaluated() {
    let plugin = builtins::script_team().unwrap();
    let boss = boss_full("皮卡丘", "静电", &["电磁波"], Some(50.0));
    let got = plugin.evaluate(&boss, &Context::zh()).unwrap();
    assert!(got.is_some(), "双性别头目不该被过滤");
}

/// 只公（100%）不评估。
#[test]
fn male_only_is_not_evaluated() {
    let plugin = builtins::script_team().unwrap();
    let boss = boss_full("皮卡丘", "静电", &["电磁波"], Some(100.0));
    let got = plugin.evaluate(&boss, &Context::zh()).unwrap();
    assert!(got.is_none(), "只公头目应返回 ()（不评估），实际 {got:?}");
}

/// 只母（0%）不评估。
#[test]
fn female_only_is_not_evaluated() {
    let plugin = builtins::script_team().unwrap();
    let boss = boss_full("皮卡丘", "静电", &["电磁波"], Some(0.0));
    let got = plugin.evaluate(&boss, &Context::zh()).unwrap();
    assert!(got.is_none(), "只母头目应返回 ()（不评估），实际 {got:?}");
}

/// 无性别（数据源没给性别比例）不评估。
#[test]
fn genderless_is_not_evaluated() {
    let plugin = builtins::script_team().unwrap();
    let boss = boss_full("百变怪", "柔软", &["电磁波"], None);
    let got = plugin.evaluate(&boss, &Context::zh()).unwrap();
    assert!(got.is_none(), "无性别头目应返回 ()（不评估），实际 {got:?}");
}

/// 白名单豁免：艾路雷朵只有公的（100%），但脚本队有对应打法，照常评估。
#[test]
fn whitelisted_gallade_is_still_evaluated() {
    let plugin = builtins::script_team().unwrap();
    // 先确认图鉴里真有艾路雷朵 —— 若名字解析不出，
    // 白名单就是靠名字兜底命中的，得知道走的哪条路。
    let boss = boss_full("艾路雷朵", "不屈之心", &["近身战", "精神剑"], Some(100.0));
    assert_eq!(boss.pokedex_id, Some(475), "艾路雷朵应解析出图鉴号 475");
    let got = plugin.evaluate(&boss, &Context::zh()).unwrap();
    assert!(got.is_some(), "白名单精灵不该被过滤");
}

/// 白名单豁免只救白名单上的精灵：同为只公的别家照样被过滤。
#[test]
fn male_only_other_than_whitelist_is_not_evaluated() {
    let plugin = builtins::script_team().unwrap();
    let boss = boss_full("皮卡丘", "静电", &["电磁波"], Some(100.0));
    assert_eq!(boss.pokedex_id, Some(25), "皮卡丘应解析出图鉴号 25");
    let got = plugin.evaluate(&boss, &Context::zh()).unwrap();
    assert!(got.is_none(), "非白名单的只公头目应被过滤，实际 {got:?}");
}

/// gender 文本解析失败时**不拦**：宁可误评，也不错杀真头目。
#[test]
fn unparseable_gender_text_falls_through_to_evaluation() {
    let plugin = builtins::script_team().unwrap();
    let mut boss = boss_full("皮卡丘", "静电", &["电磁波"], Some(50.0));
    boss.gender = "性别未知".into(); // 没有 '%'，解析不出
    let got = plugin.evaluate(&boss, &Context::zh()).unwrap();
    assert!(got.is_some(), "解析不出的性别文本不该触发过滤");
}
