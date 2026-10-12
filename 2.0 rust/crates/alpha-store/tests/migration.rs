//! 内置插件文件名迁移（`.py` → `.rhai`）。
//!
//! # 场景
//!
//! Rust 版**接管现网数据库**时，库里内置条目的 `filename` 还是原版的
//! `default_dispatcher.py` / `script_team.py`。Rust 版物化的是 `.rhai`，
//! 两边对不上 → 决策器直接不可用（「分发器文件不存在」）。
//!
//! 这不是边缘情况：**接管现网库必然发生**。所以这里用程序构造一个
//! 「原版形状」的库，验证迁移把它修好。
//!
//! # 为什么不用真实库副本
//!
//! `live_compat.rs` 那套（`ALPHA_LIVE_DB`）依赖仓库外的真实库；
//! 迁移逻辑的形状用一个合成的旧库就能完整覆盖 —— 表结构照抄
//! 原版 `panel/db.py` 的建表语句，数据行照抄线上查到的实际内容
//! （1 内置决策器 + 2 评估器行）。覆盖的是**结构性**事实，
//! 不涉及任何业务期望值。

use alpha_store::{PluginTable, Store};

/// 原版 `panel/db.py` 的建表语句（只挑插件相关的两张 + logs 兜底）。
///
/// 与 Rust 版 `SCHEMA` 的差别只在「没有 IF NOT EXISTS」—— 原版
/// `init_db()` 也是这么写的。字段一字不差。
const ORIGINAL_SCHEMA: &str = r#"
CREATE TABLE dispatchers (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    name       TEXT UNIQUE NOT NULL,
    filename   TEXT NOT NULL,
    enabled    INTEGER NOT NULL DEFAULT 1,
    active     INTEGER NOT NULL DEFAULT 0,
    priority   INTEGER NOT NULL DEFAULT 10,
    description TEXT DEFAULT '',
    is_builtin INTEGER NOT NULL DEFAULT 0,
    created_at TEXT DEFAULT ''
);
CREATE TABLE evaluators (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    name       TEXT UNIQUE NOT NULL,
    filename   TEXT NOT NULL,
    enabled    INTEGER NOT NULL DEFAULT 1,
    active     INTEGER NOT NULL DEFAULT 0,
    priority   INTEGER NOT NULL DEFAULT 10,
    description TEXT DEFAULT '',
    is_builtin INTEGER NOT NULL DEFAULT 0,
    created_at TEXT DEFAULT ''
);
CREATE TABLE logs (
    id      INTEGER PRIMARY KEY AUTOINCREMENT,
    ts      TEXT DEFAULT '',
    level   TEXT DEFAULT 'info',
    kind    TEXT DEFAULT '',
    message TEXT DEFAULT '',
    source  TEXT DEFAULT ''
);
"#;

/// 构造一个「原版形状」的库：内置行用 `.py` 文件名，
/// 外加一条线上真实存在的用户评估器（jbdui）。
fn make_original_shaped_db(path: &std::path::Path) {
    let conn = rusqlite::Connection::open(path).unwrap();
    conn.execute_batch(ORIGINAL_SCHEMA).unwrap();
    conn.execute_batch(
        r#"
        INSERT INTO dispatchers (name, filename, enabled, active, priority, description, is_builtin, created_at)
        VALUES ('默认决策器', 'default_dispatcher.py', 1, 1, 10, '内置：基于 src/strategy/engine.py 的官方打法引擎', 1, '2026-09-10 22:38:00');

        INSERT INTO evaluators (name, filename, enabled, active, priority, description, is_builtin, created_at)
        VALUES ('脚本队评估器', 'script_team.py', 0, 0, 10, '内置：评估脚本队（呆壳兽核心）能否无随机性推进，0-18 评分', 1, '2026-09-10 22:38:00');

        -- 线上现役评估器：jbdui（用户上传，比内置优先级高）
        INSERT INTO evaluators (name, filename, enabled, active, priority, description, is_builtin, created_at)
        VALUES ('jbdui', 'user_jbdui.py', 1, 1, 11, '', 0, '2026-09-21 21:54:00');
        "#,
    )
    .unwrap();
}

fn plugin_files(store: &Store, table: PluginTable) -> Vec<(String, String, bool)> {
    let rows = match table {
        PluginTable::Dispatchers => store.list_dispatchers().unwrap(),
        PluginTable::Evaluators => store.list_evaluators().unwrap(),
    };
    rows.into_iter()
        .map(|r| (r.name, r.filename, r.is_builtin))
        .collect()
}

/// 接管原版库后，内置行的文件名必须指向 Rust 真实物化的 `.rhai`。
#[test]
fn takeover_rewrites_builtin_py_filenames() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("panel.db");
    make_original_shaped_db(&db);

    let store = Store::open(&db).unwrap();
    store.init().unwrap();

    let dispatchers = plugin_files(&store, PluginTable::Dispatchers);
    let evaluators = plugin_files(&store, PluginTable::Evaluators);

    assert_eq!(dispatchers.len(), 1, "行数不该变");
    assert_eq!(dispatchers[0].1, "default_dispatcher.rhai", "内置决策器该指向 Rhai 文件");
    assert!(dispatchers[0].2, "is_builtin 不该被动");

    let script_team = evaluators
        .iter()
        .find(|(name, _, _)| name == "脚本队评估器")
        .expect("内置评估器行还在");
    assert_eq!(script_team.1, "script_team.rhai", "内置评估器该指向 Rhai 文件");
}

/// **用户上传的 `.py` 行绝不能被动** —— 那是用户数据。
///
/// jbdui 那条记录的退役由部署脚本显式处理（并激活内置评估器），
/// 不是这里悄悄改。存储层只动「我们知道自己换了实现」的内置条目。
#[test]
fn user_uploaded_py_rows_are_left_alone() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("panel.db");
    make_original_shaped_db(&db);

    let store = Store::open(&db).unwrap();
    store.init().unwrap();

    let evaluators = plugin_files(&store, PluginTable::Evaluators);
    let jbdui = evaluators
        .iter()
        .find(|(name, _, _)| name == "jbdui")
        .expect("用户评估器行不该被删");

    assert_eq!(jbdui.1, "user_jbdui.py", "用户行文件名不能被改");
    assert!(!jbdui.2, "用户行的 is_builtin 不能被动");
}

/// 迁移必须幂等 —— 每次启动都会跑，不能有累积副作用。
#[test]
fn migration_is_idempotent() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("panel.db");
    make_original_shaped_db(&db);

    let store = Store::open(&db).unwrap();
    store.init().unwrap();
    let first = plugin_files(&store, PluginTable::Dispatchers);

    // 模拟第二次启动（同一个进程内再 init 一遍，以及重开连接）
    store.init().unwrap();
    drop(store);
    let store = Store::open(&db).unwrap();
    store.init().unwrap();

    let second = plugin_files(&store, PluginTable::Dispatchers);
    assert_eq!(first, second, "两次 init 的结果必须一致");
}

/// 全新库（空库）也要能正常 init —— 迁移在空表上不该出错。
#[test]
fn fresh_database_inits_cleanly() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("panel.db");

    let store = Store::open(&db).unwrap();
    store.init().unwrap();

    // 种子行该是 .rhai（这同时验证 schema 常量没再退回 .py）
    let dispatchers = plugin_files(&store, PluginTable::Dispatchers);
    assert_eq!(dispatchers.len(), 1);
    assert_eq!(
        dispatchers[0].1, "default_dispatcher.rhai",
        "全新库的种子行必须是 .rhai —— 之前这里错写成 .py，\
         导致全新部署的决策器指向一个永不存在的文件"
    );
}
