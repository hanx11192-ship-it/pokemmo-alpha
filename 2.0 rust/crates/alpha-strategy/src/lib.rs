//! `alpha-strategy`：规则加载 + 打法引擎。
//!
//! 纯函数层，零网络/磁盘副作用（规则与图鉴由调用方注入），
//! 便于做逐字回归比对。

pub mod engine;
pub mod rules;

pub use engine::{
    generate_bilingual, generate_report, Evaluator, StrategyEngine, BILINGUAL_SEPARATOR,
};
pub use rules::{Rules, RulesDoc};
