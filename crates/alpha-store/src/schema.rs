//! SQLite 表结构。
//!
//! # 与原版的关系
//!
//! 表结构**完全沿用**原版 `panel/db.py` 的 `init_db()`，字段名与类型一字不改。
//! 目的是让 Rust 版可以直接接管现网的 `panel.db`，不用做数据迁移。
//!
//! # 建表语句为什么写成常量而不是迁移框架
//!
//! 表就 6 张、字段很少变动，原版用 `CREATE TABLE IF NOT EXISTS` 已经完全够用。
//! 引入迁移框架反而增加复杂度，还得为「老库补列」单独写一套逻辑。

/// 全部建表语句。全部是 `IF NOT EXISTS`，可重复执行。
pub const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS dispatchers (
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
CREATE TABLE IF NOT EXISTS evaluators (
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
CREATE TABLE IF NOT EXISTS logs (
    id      INTEGER PRIMARY KEY AUTOINCREMENT,
    ts      TEXT DEFAULT '',
    level   TEXT DEFAULT 'info',
    kind    TEXT DEFAULT '',
    message TEXT DEFAULT '',
    source  TEXT DEFAULT ''
);
CREATE TABLE IF NOT EXISTS users (
    id           INTEGER PRIMARY KEY AUTOINCREMENT,
    username     TEXT UNIQUE NOT NULL,
    password_hash TEXT NOT NULL,
    is_admin     INTEGER NOT NULL DEFAULT 0,
    lang         TEXT NOT NULL DEFAULT 'zh',
    created_at   TEXT DEFAULT ''
);
CREATE TABLE IF NOT EXISTS kv (
    k TEXT PRIMARY KEY,
    v TEXT DEFAULT ''
);
CREATE TABLE IF NOT EXISTS channels (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    name       TEXT NOT NULL,
    type       TEXT NOT NULL,
    enabled    INTEGER NOT NULL DEFAULT 1,
    config     TEXT DEFAULT '{}',
    created_at TEXT DEFAULT ''
);
-- 日志按「时间倒序分页」查询，这两个索引是热路径
CREATE INDEX IF NOT EXISTS idx_logs_ts   ON logs(ts DESC);
CREATE INDEX IF NOT EXISTS idx_logs_kind ON logs(kind);
-- sessions：面板登录态。原版用 Flask 的签名 cookie，Rust 版换成服务端会话表，
-- 好处是「改密码/登出全部设备」能立刻生效。
CREATE TABLE IF NOT EXISTS sessions (
    token      TEXT PRIMARY KEY,
    username   TEXT NOT NULL,
    created_at TEXT DEFAULT '',
    expires_at TEXT DEFAULT ''
);
CREATE INDEX IF NOT EXISTS idx_sessions_expires ON sessions(expires_at);
"#;

/// 内置默认分发器（首次启动时写入）。
///
/// # 文件名必须与 `alpha-plugin` 的 `BUILTINS` 逐字一致
///
/// 这里踩过一个坑：`filename` 一度还是原版的 `default_dispatcher.py`，
/// 而 `alpha-plugin` 实际物化出来的是 `default_dispatcher.rhai`。
/// 后果是**全新部署时决策器直接不可用** —— 数据库里那行指向一个
/// 永远不会存在的文件，`/debug/run` 报「分发器文件不存在」。
///
/// 两处清单的一致性现在由 `filename_matches_the_builtin_inventory`
/// 这条测试钉住（`alpha-server` 里），改一边不改另一边会立刻变红。
pub const DEFAULT_DISPATCHER: (&str, &str, &str) = (
    "默认决策器",
    "default_dispatcher.rhai",
    "内置：基于 alpha-strategy 的官方打法引擎",
);

/// 内置脚本队评估器（首次启动时写入）。
pub const DEFAULT_EVALUATOR: (&str, &str, &str) = (
    "脚本队评估器",
    "script_team.rhai",
    "内置：评估脚本队（呆壳兽核心）能否无随机性推进，0-18 评分",
);
