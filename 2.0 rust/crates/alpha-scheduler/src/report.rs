//! 报告生成：选决策器 → 跑决策器 → 失败则回落到内置引擎。
//!
//! # 原版的 `_build_report` 做了什么
//!
//! ```text
//! row = SELECT filename FROM dispatchers WHERE active=1 LIMIT 1
//!       or SELECT filename FROM dispatchers WHERE is_builtin=1 LIMIT 1
//! ctx = {"pokedex", "rules", "langs"}
//! ctx["make_evaluator"] = make_evaluator_callback   # 懒加载，失败只 warning
//! if row:
//!     try:  return run_dispatcher(row["filename"], boss, ctx), filename
//!     except:  warning(...)   # 注意：不 return，继续往下走
//! return generate_report(boss, rules, pokedex, langs[0], evaluator=...), "builtin"
//! ```
//!
//! 四个关键点：
//!
//! 1. **两级回落**：先找 `active=1`，没有才找 `is_builtin=1`；
//! 2. **插件炸了不中止**：异常只记 warning，然后**继续走到下面的内置引擎**——
//!    也就是说「决策器插件坏了」的最终表现是「用内置打法照常播报」，
//!    而不是「不播报」。这条降级链必须保留；
//! 3. **评估器是懒加载的**：读评估器失败只 warning，打法照常生成；
//! 4. 返回的第二个值进日志和 `slot_info["dispatcher"]`，内置时是字符串
//!    `"builtin"`（不是一个文件名）。
//!
//! # 评估器与决策器的关系
//!
//! 评估器**只影响报文里那一行评语**，不参与「打不打得过」的判断。
//! 所以结构是：先把评估器包成 `Fn(&BossData) -> String`，再喂给引擎
//! （`generate_report(..., evaluator)`）。引擎拿到 `Option<Evaluator>`，
//! `None` 表示「完全不评估」。
//!
//! # 为什么决策器的 ctx 里是「重新加载规则」而不是捕获 `Rules`
//!
//! `Context::with_report` 要求闭包 `'static + Send + Sync`，而 `Rules`
//! 既不是 `Clone` 也没有 `'static` 的实例可用（每次热重载都会换一份新的）。
//! `Pokedex` 倒是全局单例（`get_pokedex()` 返回 `&'static`），但 `Rules`
//! 依赖它构造，两者生命周期绑在一起。
//!
//! 所以决策器 ctx 里的报告闭包**自己重新加载一次规则**——和
//! `Context::with_repo_rules` 是同一套做法。代价是每次 `engine_report`
//! 调用多读一次 yaml；收益是决策器插件能拿到「当前真正生效的规则」，
//! 而不是调度器启动那一刻的旧快照。
//!
//! 加载失败时返回空串：决策器仍然会被调用，只是拿不到官方报告。
//! 这比「整个决策器不可用、直接回落」要好——插件里可能还有别的内容要加。

use std::sync::Arc;

use alpha_core::config::Config;
use alpha_core::models::BossData;
use alpha_core::pokedex::Pokedex;
use alpha_plugin::sandbox::{Context, PluginKind};
use alpha_plugin::PluginRegistry;
use alpha_strategy::engine::Evaluator;
use alpha_strategy::Rules;

/// 报告 + 实际使用的分发器名。
///
/// `dispatcher` 会写进日志与 `slot_info`。内置引擎时是 `"builtin"`。
#[derive(Debug, Clone)]
pub struct BuiltReport {
    pub report: String,
    /// 实际使用的分发器：插件文件名，或 `"builtin"`
    pub dispatcher: String,
}

/// 加载当前激活的评估器回调。
///
/// 没配评估器 / 读库失败 / 编译失败 → `None`（引擎跳过评估）。
/// **每一步失败都只降级，不向上传播** —— 评估器坏了不该让头目不播报。
pub fn make_evaluator(
    store: &alpha_store::Store,
    registry: &PluginRegistry,
    langs: &[String],
) -> Option<Evaluator> {
    let filename = match store.active_evaluator_filename() {
        Ok(Some(f)) => f,
        Ok(None) => return None,
        Err(e) => {
            tracing::warn!(error = %e, "读取激活评估器失败，跳过评估");
            return None;
        }
    };

    // 用 `load_by_filename`（按路径读盘）而不是 `get`（只查内存缓存）：
    // 调度器持有的 registry 实例从不做全量 scan，`get` 的缓存永远是空的，
    // 真机上就因此报过「评估器文件不存在」—— 文件明明好端端躺在 evaluators/ 里。
    let loaded = match registry.load_by_filename(PluginKind::Evaluator, &filename) {
        Ok(Some(l)) => l,
        Ok(None) => {
            tracing::warn!(plugin = %filename, "评估器文件不存在，跳过评估");
            return None;
        }
        Err(e) => {
            tracing::warn!(plugin = %filename, error = %e, "评估器读取失败，跳过评估");
            return None;
        }
    };
    let plugin = match loaded.get() {
        Ok(p) => Arc::clone(p),
        Err(e) => {
            tracing::warn!(plugin = %filename, error = %e, "评估器编译失败，跳过评估");
            return None;
        }
    };

    // 评估器只需要单语言 —— 原版 `make_evaluator_callback` 传的是
    // `langs=[langs[0]]`。注意 `Context::langs` 只影响脚本里 `ctx.langs`
    // 的可见值，`Option<Evaluation>` 的格式化用的是下面闭包捕获的 `lang`。
    let lang = langs.first().cloned().unwrap_or_else(|| "zh".to_string());
    let ctx = Context::new(lang.clone(), vec![lang.clone()]);

    Some(std::sync::Arc::new(move |boss: &BossData| {
        let view = alpha_plugin::BossView::from_boss(boss);
        match plugin.evaluate(&view, &ctx) {
            Ok(Some(ev)) => ev.format_line(&lang),
            // 脚本返回 unit = 「这只头目不评估」，与原版 `return None` 对应
            Ok(None) => String::new(),
            Err(e) => {
                tracing::warn!(plugin = %filename, error = %e, "评估器执行失败，跳过评估");
                String::new()
            }
        }
    }))
}

/// 给决策器插件用的 `Context`。
///
/// 规则在**每次调用时**重新解析，理由见模块文档。这里用的是
/// `Config::rules_raw`（已解析的 YAML）→ `RulesDoc` → `Rules::new`，
/// 不重新读文件 —— `get_config()` 本身也是进程级单例。
///
/// `evaluator` 传给引擎 —— **这是评语行唯一的注入点**。旧实现在这里
/// 刻意传 `None`，注释声称「评估器由调度器在最外层注入」，但外层
/// （[`try_dispatcher`]）拿到插件的返回文本后**什么都没做** —— 上线后
/// 第一条真实推送的报文里就没有评分行，调试台（自己挂了评估器）
/// 却一切正常，两条链路行为分裂。现在对齐原版：原版决策器插件从
/// `ctx["evaluator"]` 拿回调直接传给 `generate_report`，评语由引擎插入。
fn dispatcher_context(
    langs: &[String],
    evaluator: Option<Evaluator>,
) -> Context {
    let lang = langs.first().cloned().unwrap_or_else(|| "zh".to_string());
    Context::new(lang, langs.to_vec()).with_report(move |view, want_lang| {
        let Ok(pk) = alpha_core::pokedex::get_pokedex() else {
            tracing::warn!("决策器请求引擎报告时图鉴不可用，返回空");
            return String::new();
        };
        let Ok(config) = alpha_core::config::get_config() else {
            tracing::warn!("决策器请求引擎报告时配置不可用，返回空");
            return String::new();
        };
        let Ok(doc) =
            serde_yaml::from_value::<alpha_strategy::RulesDoc>(config.rules_raw.clone())
        else {
            tracing::warn!("决策器请求引擎报告时规则结构不合法，返回空");
            return String::new();
        };
        let rules = alpha_strategy::Rules::new(doc, pk);
        let bd = alpha_plugin::boss_view_to_boss_data(view);
        alpha_strategy::engine::generate_report(&bd, &rules, pk, want_lang, evaluator.as_ref())
    })
}

/// 尝试用指定的决策器插件生成报告。
///
/// 返回 `None` 表示「这个插件没能出结果」，调用方应继续往下回落。
fn try_dispatcher(
    registry: &PluginRegistry,
    filename: &str,
    boss: &BossData,
    langs: &[String],
    evaluator: Option<Evaluator>,
) -> Option<BuiltReport> {
    // 同 make_evaluator：按路径读盘，不依赖从未 scan 过的内存缓存
    let loaded = match registry.load_by_filename(PluginKind::Dispatcher, filename) {
        Ok(Some(l)) => l,
        Ok(None) => {
            tracing::warn!(plugin = %filename, "决策器文件不存在，回落到内置引擎");
            return None;
        }
        Err(e) => {
            tracing::warn!(plugin = %filename, error = %e, "决策器读取失败，回落到内置引擎");
            return None;
        }
    };
    let plugin = match loaded.get() {
        Ok(p) => p,
        Err(e) => {
            tracing::warn!(plugin = %filename, error = %e, "决策器编译失败，回落到内置引擎");
            return None;
        }
    };

    let view = alpha_plugin::BossView::from_boss(boss);
    match plugin.dispatch(&view, &dispatcher_context(langs, evaluator)) {
        Ok(text) => Some(BuiltReport {
            report: text,
            dispatcher: filename.to_string(),
        }),
        Err(e) => {
            tracing::warn!(plugin = %filename, error = %e, "决策器执行失败，回落到内置引擎");
            None
        }
    }
}

/// 生成报告，按「激活决策器 → 内置决策器 → 内置引擎」的顺序回落。
pub fn build_report(
    store: &alpha_store::Store,
    registry: &PluginRegistry,
    boss: &BossData,
    rules: &Rules,
    pokedex: &Pokedex,
    langs: &[String],
) -> BuiltReport {
    // 评估器提前构造。`Arc` 可克隆：插件决策器路径与内置引擎兜底
    // 路径共用同一个回调，谁先走到谁用，互不抢夺。
    let evaluator = make_evaluator(store, registry, langs);

    // 1) 激活的决策器（`active=1`，**不看 `enabled`** —— 对齐原版）
    if let Ok(Some(filename)) = store.active_dispatcher_filename() {
        if let Some(r) = try_dispatcher(registry, &filename, boss, langs, evaluator.clone()) {
            return r;
        }
    } else if let Ok(Some(filename)) = store.builtin_dispatcher_filename() {
        // 2) 内置决策器（数据库里有登记，但没被置为 active）
        if let Some(r) = try_dispatcher(registry, &filename, boss, langs, evaluator.clone()) {
            return r;
        }
    }

    // 3) 内置引擎兜底
    let report = if langs.len() > 1 {
        alpha_strategy::engine::generate_bilingual(boss, rules, pokedex, langs, evaluator.as_ref())
    } else {
        let lang = langs.first().map(|s| s.as_str()).unwrap_or("zh");
        alpha_strategy::engine::generate_report(boss, rules, pokedex, lang, evaluator.as_ref())
    };
    BuiltReport {
        report,
        dispatcher: "builtin".to_string(),
    }
}

/// 按推送语言配置算出语言列表。
///
/// 直接转调 `Config::languages()`（`both` → `["zh","en"]`，
/// `en` → `["en"]`，其余 → `["zh"]`）。
pub fn langs_from_config(config: &Config) -> Vec<String> {
    config.languages()
}

/// 决策器插件目录名。
pub fn dispatcher_dir() -> &'static str {
    alpha_plugin::dir_of(PluginKind::Dispatcher)
}

/// 评估器插件目录名。
pub fn evaluator_dir() -> &'static str {
    alpha_plugin::dir_of(PluginKind::Evaluator)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dir_names_match_the_original_layout() {
        assert_eq!(dispatcher_dir(), "dispatchers");
        assert_eq!(evaluator_dir(), "evaluators");
    }
    /// 内置引擎兜底：库里没有任何决策器登记时，必须能出一条非空报告，
    /// 且 `dispatcher` 标记为 `"builtin"`。
    #[test]
    fn falls_back_to_builtin_engine_with_no_dispatcher_registered() {
        let store = alpha_store::Store::open_in_memory().unwrap();
        store.init().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let registry = PluginRegistry::new(dir.path()).unwrap();

        let boss = BossData {
            name: "呆壳兽".into(),
            ..Default::default()
        };
        let pk = alpha_core::pokedex::get_pokedex().expect("图鉴应可加载");
        let config = alpha_core::config::get_config().expect("配置应可加载");
        let doc: alpha_strategy::RulesDoc =
            serde_yaml::from_value(config.rules_raw.clone()).expect("规则结构应合法");
        let rules = Rules::new(doc, pk);
        let langs = vec!["zh".to_string()];
        let got = build_report(&store, &registry, &boss, &rules, pk, &langs);
        assert_eq!(got.dispatcher, "builtin");
        assert!(
            !got.report.is_empty(),
            "兜底路径必须产出非空报告，实际为空"
        );
    }
}

#[cfg(test)]
mod regression_tests {
    use super::*;
    use alpha_store::PluginTable;

    /// **插件决策器路径的报文必须带评语行。**
    ///
    /// 旧实现把评估器只接进内置引擎兜底路径，插件决策器返回的报文里
    /// 从来没有评分行 —— 上线后第一条真实推送（06:23 龙王蝎，50.0%公，
    /// 本该评「简单头 评分1」）暴露了这一点；而调试台自己挂了评估器，
    /// 一直正常，两条链路行为分裂很久都没人发现。
    #[test]
    fn plugin_dispatcher_path_includes_the_evaluation_line() {
        let store = alpha_store::Store::open_in_memory().unwrap();
        store.init().unwrap();

        // 激活内置决策器与评估器（种子默认 active=0）
        let dispatchers = store.list_dispatchers().unwrap();
        let evaluators = store.list_evaluators().unwrap();
        let d = dispatchers.iter().find(|r| r.is_builtin).unwrap();
        let e = evaluators.iter().find(|r| r.is_builtin).unwrap();
        assert!(store.activate_plugin(PluginTable::Dispatchers, d.id).unwrap());
        assert!(store.activate_plugin(PluginTable::Evaluators, e.id).unwrap());

        // registry 里要有内置插件文件
        let dir = tempfile::tempdir().unwrap();
        let registry = PluginRegistry::new(dir.path()).unwrap();
        registry.materialize_builtins().unwrap();

        let pk = alpha_core::pokedex::get_pokedex().expect("图鉴应可加载");

        // 50% 公的双性别头目 + 吹飞（离场技 → 简单头 评分1）
        // move_ids 与 moves **等长**是适配器契约，测试要遵守
        // （旧构造漏了 move_ids，脚本在防御分支前就越界，暴露的是另一个 bug）
        let boss = BossData {
            name: "皮卡丘".into(),
            moves: vec!["吹飞".into(), "十万伏特".into()],
            move_ids: vec![pk.resolve_move_id("吹飞"), pk.resolve_move_id("十万伏特")],
            gender: alpha_core::models::Gender::from_male_percent(Some(50.0)),
            ..Default::default()
        };
        let config = alpha_core::config::get_config().expect("配置应可加载");
        let doc: alpha_strategy::RulesDoc =
            serde_yaml::from_value(config.rules_raw.clone()).expect("规则结构应合法");
        let rules = Rules::new(doc, pk);
        let langs = vec!["zh".to_string()];

        let got = build_report(&store, &registry, &boss, &rules, pk, &langs);

        // 走的是插件决策器，不是内置引擎兜底
        assert_eq!(got.dispatcher, "default_dispatcher.rhai", "应命中插件决策器路径");
        assert!(
            got.report.contains("评分"),
            "插件决策器路径的报文必须带评语行（评分），实际报文：\n{}",
            got.report
        );
    }
}
