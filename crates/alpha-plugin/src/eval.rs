//! 评估结果，以及「压成报文一行」的格式化。
//!
//! # 格式化必须逐字对齐原版
//!
//! 原版 `panel/evaluate.py` 的 `format_line()` 决定报文里那一行长什么样，
//! 用户在微信里看到的就是这个字符串。改一个空格都是可见的回归。
//!
//! 原版逻辑（三条优先级）：
//!
//! 1. 脚本给了 `line` → 原样使用（脚本完全掌控措辞）
//! 2. 没有 `line` 但有 `label` → 中文拼 `{label} 评分{score}`；
//!    英文拼 `{label} Score {score}`；`score` 缺失时只输出 `label`
//! 3. 既无 `line` 又无 `label` → 空串（调用方据此跳过这一行）
//!
//! # ⚠️ `label_en` 在报文里是**死字段**
//!
//! 原版 `format_line()` 只读 `label`，**从不读 `label_en`**。也就是说评估器
//! 就算精心写了英文评语，报文里出现的仍然是中文 `label`，只是模板换成了
//! `Score -1`：
//!
//! ```text
//! zh → "小丑头 评分-1"
//! en → "小丑头 Score -1"      ← 注意这里仍是「小丑头」，不是 "Clown Alpha"
//! ```
//!
//! （已用原版代码实测确认，不是推断。）
//!
//! 这里**刻意复刻**这个行为，没有「顺手修正」成英文模式下用 `label_en`：
//! 那会让 Rust 版和原版在同一只头目上输出不同的报文，属于用户可见的静默
//! 行为变更 —— 而这次重构的前提是「行为对齐」。
//!
//! 如果将来确实要让 `label_en` 生效，那是一次**明确的产品决策**，
//! 应该同时改原版、并在面板上说明，而不是藏在重构里。

use rhai::Map;

/// 一次评估的结果。
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Evaluation {
    /// 分值 / 系数，含义由评估器自定
    pub score: Option<i64>,
    /// 评价词，如「白给头 / 简单头 / 看脸头」
    pub label: Option<String>,
    /// 一句话说明
    pub detail: Option<String>,
    /// 触发的加分项明细（供日志排查）
    pub factors: Vec<String>,
    /// 直接指定报文那一行的完整文本（给了就不走默认拼接）
    pub line: Option<String>,
    /// 英文评价词（英文模式下用）
    pub label_en: Option<String>,
}

impl Evaluation {
    /// 从脚本返回的 Map 构造。
    ///
    /// 所有字段都是可选的 —— 脚本只返回 `#{score: 3}` 也是合法的，
    /// 与原版 Python 的 `result.get(...)` 行为一致。
    pub fn from_map(m: &Map) -> Self {
        Self {
            score: get_i64(m, "score"),
            label: get_string(m, "label"),
            detail: get_string(m, "detail"),
            factors: get_string_array(m, "factors"),
            line: get_string(m, "line"),
            label_en: get_string(m, "label_en"),
        }
    }

    /// 压成报文那一行。见模块头部的优先级说明。
    ///
    /// `lang` 只影响模板（`评分` / `Score`），**不影响用哪个 label** ——
    /// 原版无论什么语言都用 `label`，`label_en` 不参与（见模块头部说明）。
    pub fn format_line(&self, lang: &str) -> String {
        // 1) 脚本自定义整行（最高优先级）
        //    原版判的是 `if custom:`，空串算「没给」，这里用 filter 对齐
        if let Some(custom) = self.line.as_deref().filter(|s| !s.is_empty()) {
            return custom.to_string();
        }

        // 2) 默认格式：label + score（不看 label_en，与原版一致）
        //    原版判的是 `if not label:`，空串同样算「没给」
        let Some(label) = self.label.as_deref().filter(|s| !s.is_empty()) else {
            return String::new(); // 3) 既无 line 又无 label
        };

        match (lang, self.score) {
            ("en", Some(s)) => format!("{label} Score {s}"),
            ("en", None) => label.to_string(),
            (_, Some(s)) => format!("{label} 评分{s}"),
            (_, None) => label.to_string(),
        }
    }

    /// 是否「有内容」—— 全空的评估结果等价于「不评估」。
    pub fn is_empty(&self) -> bool {
        self.score.is_none()
            && self.label.is_none()
            && self.line.is_none()
            && self.detail.is_none()
            && self.factors.is_empty()
    }
}

fn get_i64(m: &Map, key: &str) -> Option<i64> {
    let v = m.get(key)?;
    if v.is_unit() {
        return None;
    }
    v.clone().try_cast::<i64>()
        // 脚本可能写成浮点（1.0），宽松接受
        .or_else(|| v.clone().try_cast::<f64>().map(|f| f as i64))
}

fn get_string(m: &Map, key: &str) -> Option<String> {
    let v = m.get(key)?;
    if v.is_unit() {
        return None;
    }
    v.clone().try_cast::<String>()
}

fn get_string_array(m: &Map, key: &str) -> Vec<String> {
    let Some(v) = m.get(key) else {
        return Vec::new();
    };
    if v.is_unit() {
        return Vec::new();
    }
    v.clone()
        .try_cast::<rhai::Array>()
        .map(|a| {
            a.iter()
                .filter_map(|x| x.clone().try_cast::<String>())
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn eval(score: Option<i64>, label: Option<&str>) -> Evaluation {
        Evaluation {
            score,
            label: label.map(|s| s.to_string()),
            ..Default::default()
        }
    }

    /// 中文默认格式：`{label} 评分{score}` —— 空格与「评分」二字都是契约的一部分。
    #[test]
    fn chinese_default_format() {
        assert_eq!(eval(Some(5), Some("看脸头")).format_line("zh"), "看脸头 评分5");
        assert_eq!(eval(Some(0), Some("白给头")).format_line("zh"), "白给头 评分0");
        assert_eq!(eval(Some(-1), Some("小丑头")).format_line("zh"), "小丑头 评分-1");
    }

    /// 英文默认格式：`{label} Score {score}`。
    #[test]
    fn english_default_format() {
        assert_eq!(eval(Some(5), Some("RNG Alpha")).format_line("en"), "RNG Alpha Score 5");
    }

    /// 给了 `line` 就用它，忽略 label/score。
    #[test]
    fn custom_line_wins() {
        let e = Evaluation {
            score: Some(3),
            label: Some("看脸头".into()),
            line: Some("好打 难度系数3".into()),
            ..Default::default()
        };
        assert_eq!(e.format_line("zh"), "好打 难度系数3");
        assert_eq!(e.format_line("en"), "好打 难度系数3", "自定义行不受语言影响");
    }

    /// 没给 score 时只输出 label（不能出现「评分」后面跟着 `None`）。
    #[test]
    fn label_only_when_score_missing() {
        assert_eq!(eval(None, Some("白给头")).format_line("zh"), "白给头");
        assert_eq!(eval(None, Some("Easy")).format_line("en"), "Easy");
    }

    /// 既无 line 又无 label → 空串，调用方据此跳过该行。
    #[test]
    fn empty_when_nothing_to_say() {
        assert_eq!(eval(Some(3), None).format_line("zh"), "");
        assert_eq!(Evaluation::default().format_line("zh"), "");
        assert_eq!(eval(None, Some("")).format_line("zh"), "");
    }

    /// **`label_en` 不参与报文行** —— 这是原版的行为，不是遗漏。
    ///
    /// 原版 `format_line()` 只读 `label`，所以就算脚本写了英文评语，
    /// 英文模式的报文里出现的仍是中文 label，只是模板变成 `Score n`。
    /// 这条测试是对「行为对齐」的直接断言：改坏了会被立刻发现。
    ///
    /// 详见模块头部「`label_en` 在报文里是死字段」。
    #[test]
    fn label_en_is_not_used_in_the_line() {
        let e = Evaluation {
            score: Some(3),
            label: Some("看脸头".into()),
            label_en: Some("RNG Alpha".into()),
            ..Default::default()
        };
        assert_eq!(e.format_line("en"), "看脸头 Score 3", "原版不会用 label_en");
        assert_eq!(e.format_line("zh"), "看脸头 评分3");
    }

    /// 英文模式同样用中文 label（就是上一条的一般化）。
    #[test]
    fn english_uses_the_same_label_as_chinese() {
        let e = eval(Some(2), Some("简单头"));
        assert_eq!(e.format_line("en"), "简单头 Score 2");
    }

    /// 只有 `label_en`、没有 `label` 时输出空串 —— 原版 `if not label` 直接返回。
    ///
    /// 换个角度说：脚本想控制英文措辞，**唯一**的办法是给 `line`。
    #[test]
    fn label_en_alone_produces_nothing() {
        let e = Evaluation {
            score: Some(3),
            label_en: Some("RNG Alpha".into()),
            ..Default::default()
        };
        assert_eq!(e.format_line("en"), "");
        assert_eq!(e.format_line("zh"), "");
    }

    #[test]
    fn parses_map_with_partial_fields() {
        let mut m = Map::new();
        m.insert("score".into(), rhai::Dynamic::from(3_i64));
        let e = Evaluation::from_map(&m);
        assert_eq!(e.score, Some(3));
        assert!(e.label.is_none());
        assert!(e.factors.is_empty());
    }

    /// 脚本里 `factors: []` 与不写 `factors` 等价。
    #[test]
    fn empty_factors_are_fine() {
        let mut m = Map::new();
        m.insert("score".into(), rhai::Dynamic::from(1_i64));
        m.insert("factors".into(), rhai::Dynamic::from(rhai::Array::new()));
        let e = Evaluation::from_map(&m);
        assert!(e.factors.is_empty());
        assert!(e.label.is_none());
    }

    /// 浮点分数要能接受（脚本里写 `3.0` 很自然）。
    #[test]
    fn accepts_float_score() {
        let mut m = Map::new();
        m.insert("score".into(), rhai::Dynamic::from(3.0_f64));
        assert_eq!(Evaluation::from_map(&m).score, Some(3));
    }

    /// unit 值（脚本里写了字段但值是 `()`）应视为「没给」。
    #[test]
    fn unit_values_are_treated_as_absent() {
        let mut m = Map::new();
        m.insert("score".into(), rhai::Dynamic::UNIT);
        m.insert("label".into(), rhai::Dynamic::UNIT);
        let e = Evaluation::from_map(&m);
        assert!(e.score.is_none());
        assert!(e.label.is_none());
    }

    #[test]
    fn is_empty_detects_blank_results() {
        assert!(Evaluation::default().is_empty());
        assert!(!eval(Some(3), None).is_empty());
        assert!(!eval(None, Some("x")).is_empty());
    }
}
