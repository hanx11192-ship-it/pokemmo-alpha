//! 测试辅助：构造 [`BossView`] 与调用内建插件。
//!
//! 公开出来（而非只放在 `#[cfg(test)]` 里）是为了让集成测试
//! （`tests/` 目录）也能复用，避免每个测试文件各写一份构造代码。

use crate::ast::BossView;

/// 用最少参数构造一个头目视图。
///
/// 图鉴 id 与技能 id 都会真的去查图鉴 —— 这样「按 id 匹配」的代码路径
/// 在测试里是被覆盖的，而不是永远走字符串兜底。
pub fn boss(name: &str, moves: &[&str]) -> BossView {
    boss_full(name, "", moves, None)
}

/// 完整构造：可指定特性与性别比例。
pub fn boss_full(name: &str, ability: &str, moves: &[&str], male: Option<f64>) -> BossView {
    let pk = alpha_core::pokedex::get_pokedex().ok();

    let pid = pk.and_then(|p| p.resolve_pokemon_id(name));
    let ability_id = if ability.is_empty() {
        None
    } else {
        pk.and_then(|p| p.resolve_ability_id(ability))
    };
    let move_ids: Vec<Option<i64>> = moves
        .iter()
        .map(|m| pk.and_then(|p| p.resolve_move_id(m)))
        .collect();

    BossView {
        name: name.to_string(),
        ability: ability.to_string(),
        ability_id,
        moves: moves.iter().map(|s| s.to_string()).collect(),
        move_ids,
        location: "测试地点".into(),
        period: "14:00~15:15".into(),
        gender: male.map(|p| format!("{p}%公")).unwrap_or_default(),
        pokedex_id: pid,
        source: "test".into(),
        reported_at: "2026-09-22 14:00:00".into(),
        ..Default::default()
    }
}
