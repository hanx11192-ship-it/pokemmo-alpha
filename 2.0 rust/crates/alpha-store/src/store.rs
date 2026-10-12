//! 面板用的 SQLite 存储。
//!
//! # 为什么用 `Mutex<Connection>` 而不是连接池
//!
//! 面板的写操作很稀疏（几秒一次日志、偶尔改配置），瓶颈根本不在数据库。
//! 一个 `Connection` 加锁足够，而且避免了 WAL 模式下多连接的 busy 重试逻辑。
//! SQLite 本身是单写者模型，连接池在这里只是「看起来更专业」。
//!
//! # 时间格式
//!
//! 全部用 `YYYY-MM-DD HH:MM:SS`（北京时间、无时区后缀），与原版
//! `time.strftime("%Y-%m-%d %H:%M:%S")` 逐字对齐 —— 这样面板上按字符串
//! 比较时间就能正确排序，接管老库也不会有格式混杂。

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};

use chrono::{DateTime, Datelike, Duration, Timelike};
use rusqlite::{params, Connection, OptionalExtension, Row};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::{StoreError, StoreResult};
use crate::schema;

/// 北京时间格式化（与原版 `strftime("%Y-%m-%d %H:%M:%S")` 一致）。
///
/// 手写格式化而不用 `%Y-%m-%d %H:%M:%S`，是因为 chrono 的 `%Y` 在公元 0 年
/// 之前会带符号，而面板只关心字符串**定长 19 字符**这个不变量
/// （前端的 `slice(11, 16)` 取时分依赖它）。
fn fmt_stamp(t: DateTime<chrono_tz::Tz>) -> String {
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02}",
        t.year(),
        t.month(),
        t.day(),
        t.hour(),
        t.minute(),
        t.second()
    )
}

/// 当前北京时间戳。
pub fn now_stamp() -> String {
    fmt_stamp(alpha_core::time::now_beijing())
}

/// `now_stamp()` 在 `N` 天前的对应时刻。
pub fn stamp_days_ago(days: i64) -> String {
    fmt_stamp(alpha_core::time::now_beijing() - Duration::days(days))
}

/// 把内置条目的文件名从原版的 `.py` 改成 `.rhai`。
///
/// # 为什么必须有这一步
///
/// 这一版是**接管现网数据库**的（`/opt/pokemmo_alpha/panel/panel.db`），
/// 而那个库是原版 Python 面板建的，里面的内置条目长这样：
///
/// ```text
/// id=1  name=默认决策器    filename=default_dispatcher.py  is_builtin=1
/// ```
///
/// Rust 版的内置插件是 Rhai 脚本，物化出来的是 `.rhai` 文件。
/// 两边文件名对不上时，`is_builtin` 那行**指向一个永远不会存在的文件**，
/// 后果是接管之后决策器直接不可用（`/debug/run` 报「分发器文件不存在」）——
/// 而且这是**接管现网库必然发生**的事，不是边缘情况。
///
/// # 为什么只改内置条目
///
/// 判据是 `is_builtin=1` **而不是**「文件名以 .py 结尾」。
/// 用户自己上传的 Python 插件是真实存在的文件（原版真的能跑），
/// 我们不能替它决定命运；那些需要人工改写成 Rhai。
/// 只有内置条目是「我们知道自己换了实现」的。
///
/// # 幂等
///
/// 每次启动都跑一遍，条件写成「当前值还是老的才更新」，
/// 所以重复执行没有副作用，也不需要版本号或迁移表。
fn migrate_builtin_filenames(conn: &Connection) -> StoreResult<()> {
    // (表, 旧文件名, 新文件名)
    let pairs = [
        (
            "dispatchers",
            "default_dispatcher.py",
            schema::DEFAULT_DISPATCHER.1,
        ),
        ("evaluators", "script_team.py", schema::DEFAULT_EVALUATOR.1),
    ];

    for (table, old, new) in pairs {
        if old == new {
            continue; // 已经不是老名字了，没什么可做
        }
        // 表名来自上面的字面量，不是用户输入；文件名走参数绑定。
        let sql = format!(
            "UPDATE {table} SET filename = ?1 \
             WHERE is_builtin = 1 AND filename = ?2"
        );
        let changed = conn.execute(&sql, params![new, old])?;
        if changed > 0 {
            // 接管现网库时这条会打出来 —— 说明真的动了东西，值得记一笔
            tracing::info!(
                table,
                from = old,
                to = new,
                rows = changed,
                "已把内置插件条目从原版 .py 迁移到 Rhai 脚本"
            );
        }
    }
    Ok(())
}

// ---------------------------------------------------------------- 模型

/// 日志级别。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LogLevel {
    Debug,
    Info,
    Warning,
    Error,
}

impl LogLevel {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Debug => "debug",
            Self::Info => "info",
            Self::Warning => "warning",
            Self::Error => "error",
        }
    }

    pub fn parse(s: &str) -> Self {
        match s.to_lowercase().as_str() {
            "debug" => Self::Debug,
            "warning" | "warn" => Self::Warning,
            "error" => Self::Error,
            _ => Self::Info,
        }
    }
}

/// 一条日志。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogRow {
    pub id: i64,
    pub ts: String,
    pub level: String,
    pub kind: String,
    pub message: String,
    pub source: String,
}

/// 日志查询条件。
#[derive(Debug, Clone, Default)]
pub struct LogFilter {
    pub limit: usize,
    pub level: Option<String>,
    pub kind: Option<String>,
    pub source: Option<String>,
}

impl LogFilter {
    pub fn new(limit: usize) -> Self {
        Self {
            limit,
            ..Default::default()
        }
    }
}

/// 面板用户。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct User {
    pub id: i64,
    pub username: String,
    #[serde(skip_serializing)]
    pub password_hash: String,
    pub is_admin: bool,
    pub lang: String,
    pub created_at: String,
}

/// 面板用户的公开视图（不含密码哈希）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserPublic {
    pub id: i64,
    pub username: String,
    pub is_admin: bool,
    pub lang: String,
    pub created_at: String,
}

impl From<&User> for UserPublic {
    fn from(u: &User) -> Self {
        Self {
            id: u.id,
            username: u.username.clone(),
            is_admin: u.is_admin,
            lang: u.lang.clone(),
            created_at: u.created_at.clone(),
        }
    }
}

/// 插件型条目（dispatchers / evaluators 表结构完全相同）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginRow {
    pub id: i64,
    pub name: String,
    pub filename: String,
    pub enabled: bool,
    pub active: bool,
    pub priority: i64,
    pub description: String,
    pub is_builtin: bool,
    pub created_at: String,
}

/// 新建插件条目的入参。
#[derive(Debug, Clone)]
pub struct NewPlugin {
    pub name: String,
    pub filename: String,
    pub enabled: bool,
    pub active: bool,
    pub priority: i64,
    pub description: String,
    pub is_builtin: bool,
}

/// 会话。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session {
    pub token: String,
    pub username: String,
    pub created_at: String,
    pub expires_at: String,
}

/// 插件表种类 —— 决定操作哪张表。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PluginTable {
    Dispatchers,
    Evaluators,
}

impl PluginTable {
    fn table_name(&self) -> &'static str {
        match self {
            Self::Dispatchers => "dispatchers",
            Self::Evaluators => "evaluators",
        }
    }
}

// ---------------------------------------------------------------- 存储

/// 面板存储。
///
/// 内部 `Arc<Mutex<Connection>>`，clone 廉价，可自由跨线程 / 放进 axum 的 State。
#[derive(Clone)]
pub struct Store {
    conn: Arc<Mutex<Connection>>,
    path: PathBuf,
}

impl Store {
    /// 打开（或创建）指定路径的数据库，并初始化表结构。
    pub fn open(path: impl AsRef<Path>) -> StoreResult<Self> {
        let path = path.as_ref().to_path_buf();
        if let Some(dir) = path.parent() {
            if !dir.as_os_str().is_empty() {
                std::fs::create_dir_all(dir).map_err(|e| {
                    StoreError::Config(format!("无法创建数据库目录 {}: {e}", dir.display()))
                })?;
            }
        }

        let conn = Connection::open(&path)?;
        // WAL：读写不互相阻塞，面板查询不卡住写入
        conn.pragma_update(None, "journal_mode", "WAL")?;
        // 外键约束（本项目目前没用到，但开着没坏处）
        conn.pragma_update(None, "foreign_keys", "ON")?;
        // 等锁最多 3 秒，避免并发写直接报 busy
        conn.busy_timeout(std::time::Duration::from_secs(3))?;

        let store = Self {
            conn: Arc::new(Mutex::new(conn)),
            path,
        };
        store.init()?;
        Ok(store)
    }

    /// 内存库（测试用）。
    pub fn open_in_memory() -> StoreResult<Self> {
        let conn = Connection::open_in_memory()?;
        let store = Self {
            conn: Arc::new(Mutex::new(conn)),
            path: PathBuf::from(":memory:"),
        };
        store.init()?;
        Ok(store)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// 拿到底层连接的锁。
    ///
    /// 公开是为了让上层（以及测试）能在需要时执行一条裸 SQL —— 比如面板的
    /// 「数据库维护」功能要 `VACUUM`，而给每一种维护操作都包一个方法并不划算。
    /// 代价是绕过类型检查，所以只用于「无业务逻辑的运维语句」。
    pub fn lock(&self) -> StoreResult<MutexGuard<'_, Connection>> {
        self.conn
            .lock()
            .map_err(|e| StoreError::Config(format!("数据库锁已中毒: {e}")))
    }

    /// 建表 + 写入内置默认条目 + 修正从原版继承来的行。
    pub fn init(&self) -> StoreResult<()> {
        let conn = self.lock()?;
        conn.execute_batch(schema::SCHEMA)?;

        // 首次启动确保内置默认分发器存在，并设为启用 + 当前激活
        let n: i64 = conn.query_row("SELECT COUNT(*) FROM dispatchers", [], |r| r.get(0))?;
        if n == 0 {
            let (name, filename, desc) = schema::DEFAULT_DISPATCHER;
            conn.execute(
                "INSERT INTO dispatchers \
                 (name, filename, enabled, active, priority, description, is_builtin, created_at) \
                 VALUES (?,?,?,?,?,?,?,?)",
                params![name, filename, 1, 1, 10, desc, 1, now_stamp()],
            )?;
        }

        // 首次启动确保内置「脚本队评估器」存在，并设为启用 + 当前激活
        let n: i64 = conn.query_row("SELECT COUNT(*) FROM evaluators", [], |r| r.get(0))?;
        if n == 0 {
            let (name, filename, desc) = schema::DEFAULT_EVALUATOR;
            conn.execute(
                "INSERT INTO evaluators \
                 (name, filename, enabled, active, priority, description, is_builtin, created_at) \
                 VALUES (?,?,?,?,?,?,?,?)",
                params![name, filename, 1, 1, 10, desc, 1, now_stamp()],
            )?;
        }

        // 接管原版数据库时，把内置条目的 `.py` 文件名改成 `.rhai`。
        migrate_builtin_filenames(&conn)?;
        Ok(())
    }

    // ------------------------------------------------------------ 日志

    /// 写一条日志。
    ///
    /// **永不因为日志失败而中断主流程** —— 原版这里也是 `try/except: pass`，
    /// 保留该行为：日志写不进去最多丢一条记录，不该让整轮推送崩掉。
    pub fn log(&self, level: LogLevel, kind: &str, message: &str, source: &str) {
        if let Err(e) = self.try_log(level, kind, message, source) {
            tracing::warn!("写日志失败（忽略）: {e}");
        }
    }

    fn try_log(
        &self,
        level: LogLevel,
        kind: &str,
        message: &str,
        source: &str,
    ) -> StoreResult<()> {
        let conn = self.lock()?;
        conn.execute(
            "INSERT INTO logs (ts, level, kind, message, source) VALUES (?,?,?,?,?)",
            params![now_stamp(), level.as_str(), kind, message, source],
        )?;
        Ok(())
    }

    /// 便捷：info 级。
    pub fn log_info(&self, kind: &str, message: &str, source: &str) {
        self.log(LogLevel::Info, kind, message, source);
    }

    /// 便捷：error 级。
    pub fn log_error(&self, kind: &str, message: &str, source: &str) {
        self.log(LogLevel::Error, kind, message, source);
    }

    /// 按条件查日志（时间倒序）。
    pub fn list_logs(&self, f: &LogFilter) -> StoreResult<Vec<LogRow>> {
        let conn = self.lock()?;
        let mut sql = String::from("SELECT id, ts, level, kind, message, source FROM logs WHERE 1=1");
        let mut args: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();

        if let Some(v) = &f.level {
            sql.push_str(" AND level=?");
            args.push(Box::new(v.clone()));
        }
        if let Some(v) = &f.kind {
            sql.push_str(" AND kind=?");
            args.push(Box::new(v.clone()));
        }
        if let Some(v) = &f.source {
            sql.push_str(" AND source=?");
            args.push(Box::new(v.clone()));
        }
        sql.push_str(" ORDER BY id DESC LIMIT ?");
        args.push(Box::new(f.limit.clamp(1, 10_000) as i64));

        let params: Vec<&dyn rusqlite::ToSql> = args.iter().map(|b| b.as_ref()).collect();
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt
            .query_map(params.as_slice(), |r| {
                Ok(LogRow {
                    id: r.get(0)?,
                    ts: r.get(1)?,
                    level: r.get(2)?,
                    kind: r.get(3)?,
                    message: r.get(4)?,
                    source: r.get(5)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// 日志里出现过的来源（用于面板筛选下拉）。
    pub fn list_log_sources(&self) -> StoreResult<Vec<String>> {
        let conn = self.lock()?;
        let mut stmt =
            conn.prepare("SELECT DISTINCT source FROM logs WHERE source<>'' ORDER BY source")?;
        let rows = stmt
            .query_map([], |r| r.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// 清理 `days` 天前的日志，返回删除条数。
    /// 日志总数（不分页、不筛选）。
    ///
    /// 仪表盘上那个「总日志数」用它。与 `list_logs(...).len()` 的区别是
    /// 这里走 `COUNT(*)`，不用把行取出来再数。
    pub fn count_logs(&self) -> StoreResult<usize> {
        let conn = self.lock()?;
        let n: i64 = conn.query_row("SELECT COUNT(*) FROM logs", [], |r| r.get(0))?;
        Ok(n as usize)
    }

    pub fn cleanup_logs(&self, days: i64) -> StoreResult<usize> {
        let conn = self.lock()?;
        let n = conn.execute("DELETE FROM logs WHERE ts < ?", params![stamp_days_ago(days)])?;
        Ok(n)
    }

    // ------------------------------------------------------------ 用户

    pub fn count_users(&self) -> StoreResult<i64> {
        let conn = self.lock()?;
        Ok(conn.query_row("SELECT COUNT(*) FROM users", [], |r| r.get(0))?)
    }

    /// 新建用户。用户名重复返回 [`StoreError::Conflict`]。
    pub fn add_user(
        &self,
        username: &str,
        password_hash: &str,
        is_admin: bool,
        lang: &str,
    ) -> StoreResult<i64> {
        let conn = self.lock()?;
        conn.execute(
            "INSERT INTO users (username, password_hash, is_admin, lang, created_at) \
             VALUES (?,?,?,?,?)",
            params![username, password_hash, is_admin as i64, lang, now_stamp()],
        )
        .map_err(|e| {
            if is_unique_violation(&e) {
                StoreError::Conflict(format!("用户名 {username} 已存在"))
            } else {
                StoreError::Sqlite(e)
            }
        })?;
        Ok(conn.last_insert_rowid())
    }

    pub fn get_user(&self, username: &str) -> StoreResult<Option<User>> {
        let conn = self.lock()?;
        let row = conn
            .query_row(
                "SELECT id, username, password_hash, is_admin, lang, created_at \
                 FROM users WHERE username=?",
                params![username],
                map_user,
            )
            .optional()?;
        Ok(row)
    }

    pub fn list_users(&self) -> StoreResult<Vec<UserPublic>> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare(
            "SELECT id, username, password_hash, is_admin, lang, created_at \
             FROM users ORDER BY id",
        )?;
        let rows = stmt
            .query_map([], map_user)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows.iter().map(UserPublic::from).collect())
    }

    pub fn set_user_lang(&self, username: &str, lang: &str) -> StoreResult<()> {
        let conn = self.lock()?;
        conn.execute(
            "UPDATE users SET lang=? WHERE username=?",
            params![lang, username],
        )?;
        Ok(())
    }

    pub fn set_user_password(&self, username: &str, password_hash: &str) -> StoreResult<()> {
        let conn = self.lock()?;
        conn.execute(
            "UPDATE users SET password_hash=? WHERE username=?",
            params![password_hash, username],
        )?;
        Ok(())
    }

    pub fn delete_user(&self, username: &str) -> StoreResult<bool> {
        let conn = self.lock()?;
        let n = conn.execute("DELETE FROM users WHERE username=?", params![username])?;
        Ok(n > 0)
    }

    // -------------------------------------------------------- 会话

    pub fn create_session(
        &self,
        token: &str,
        username: &str,
        ttl_hours: i64,
    ) -> StoreResult<()> {
        let conn = self.lock()?;
        let expires = fmt_stamp(alpha_core::time::now_beijing() + Duration::hours(ttl_hours));
        conn.execute(
            "INSERT OR REPLACE INTO sessions (token, username, created_at, expires_at) \
             VALUES (?,?,?,?)",
            params![token, username, now_stamp(), expires],
        )?;
        Ok(())
    }

    /// 取一个**未过期**的会话。
    pub fn get_session(&self, token: &str) -> StoreResult<Option<Session>> {
        let conn = self.lock()?;
        let row = conn
            .query_row(
                "SELECT token, username, created_at, expires_at FROM sessions \
                 WHERE token=? AND expires_at > ?",
                params![token, now_stamp()],
                |r| {
                    Ok(Session {
                        token: r.get(0)?,
                        username: r.get(1)?,
                        created_at: r.get(2)?,
                        expires_at: r.get(3)?,
                    })
                },
            )
            .optional()?;
        Ok(row)
    }

    pub fn delete_session(&self, token: &str) -> StoreResult<()> {
        let conn = self.lock()?;
        conn.execute("DELETE FROM sessions WHERE token=?", params![token])?;
        Ok(())
    }

    /// 删除某个用户的**全部**会话（改密码 / 踢下线用）。
    pub fn delete_user_sessions(&self, username: &str) -> StoreResult<usize> {
        let conn = self.lock()?;
        let n = conn.execute("DELETE FROM sessions WHERE username=?", params![username])?;
        Ok(n)
    }

    /// 清理过期会话，返回删除条数。
    pub fn cleanup_sessions(&self) -> StoreResult<usize> {
        let conn = self.lock()?;
        let n = conn.execute("DELETE FROM sessions WHERE expires_at <= ?", params![now_stamp()])?;
        Ok(n)
    }

    // -------------------------------------------------------- 渠道

    pub fn list_channels(&self) -> StoreResult<Vec<alpha_notify::Channel>> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare(
            "SELECT id, name, type, enabled, config FROM channels ORDER BY id",
        )?;
        let rows = stmt
            .query_map([], map_channel)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    pub fn get_channel(&self, id: i64) -> StoreResult<Option<alpha_notify::Channel>> {
        let conn = self.lock()?;
        let row = conn
            .query_row(
                "SELECT id, name, type, enabled, config FROM channels WHERE id=?",
                params![id],
                map_channel,
            )
            .optional()?;
        Ok(row)
    }

    /// 只取启用的渠道（推送热路径）。
    pub fn list_enabled_channels(&self) -> StoreResult<Vec<alpha_notify::Channel>> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare(
            "SELECT id, name, type, enabled, config FROM channels \
             WHERE enabled=1 ORDER BY id",
        )?;
        let rows = stmt
            .query_map([], map_channel)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    pub fn add_channel(
        &self,
        name: &str,
        kind: alpha_notify::ChannelKind,
        config: &alpha_notify::ChannelConfig,
        enabled: bool,
    ) -> StoreResult<i64> {
        let conn = self.lock()?;
        let cfg = serde_json::to_string(config)?;
        conn.execute(
            "INSERT INTO channels (name, type, enabled, config, created_at) VALUES (?,?,?,?,?)",
            params![name, kind.as_str(), enabled as i64, cfg, now_stamp()],
        )?;
        Ok(conn.last_insert_rowid())
    }

    /// 局部更新：只改传入的字段，`config` 是**浅合并**（保留未提到的键）。
    pub fn update_channel(
        &self,
        id: i64,
        name: Option<&str>,
        kind: Option<alpha_notify::ChannelKind>,
        config: Option<&alpha_notify::ChannelConfig>,
        enabled: Option<bool>,
    ) -> StoreResult<bool> {
        let conn = self.lock()?;
        let existing = conn
            .query_row(
                "SELECT name, type, enabled, config FROM channels WHERE id=?",
                params![id],
                |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, i64>(2)?,
                        r.get::<_, String>(3)?,
                    ))
                },
            )
            .optional()?;
        let Some((old_name, old_type, old_enabled, old_cfg)) = existing else {
            return Ok(false);
        };

        let new_name = name.unwrap_or(&old_name).to_string();
        let new_type = kind.map(|k| k.as_str().to_string()).unwrap_or(old_type);
        let new_enabled = enabled.map(|b| b as i64).unwrap_or(old_enabled);

        // config 做浅合并：面板每次只提交改动的字段，直接整体覆盖会丢键
        let new_cfg = match config {
            None => old_cfg,
            Some(partial) => {
                let mut merged: serde_json::Map<String, Value> =
                    serde_json::from_str(&old_cfg).unwrap_or_default();
                let patch = serde_json::to_value(partial)?;
                if let Value::Object(m) = patch {
                    for (k, v) in m {
                        merged.insert(k, v);
                    }
                }
                Value::Object(merged).to_string()
            }
        };

        conn.execute(
            "UPDATE channels SET name=?, type=?, config=?, enabled=? WHERE id=?",
            params![new_name, new_type, new_cfg, new_enabled, id],
        )?;
        Ok(true)
    }

    pub fn delete_channel(&self, id: i64) -> StoreResult<bool> {
        let conn = self.lock()?;
        let n = conn.execute("DELETE FROM channels WHERE id=?", params![id])?;
        Ok(n > 0)
    }

    /// 全量替换渠道配置（用于「导入配置」场景）。
    pub fn replace_channel_config(
        &self,
        id: i64,
        config: &alpha_notify::ChannelConfig,
    ) -> StoreResult<bool> {
        let conn = self.lock()?;
        let cfg = serde_json::to_string(config)?;
        let n = conn.execute(
            "UPDATE channels SET config=? WHERE id=?",
            params![cfg, id],
        )?;
        Ok(n > 0)
    }

    // ------------------------------------------------- 插件（分发器/评估器）

    fn list_plugins(&self, t: PluginTable) -> StoreResult<Vec<PluginRow>> {
        let conn = self.lock()?;
        let sql = format!(
            "SELECT id, name, filename, enabled, active, priority, description, \
             is_builtin, created_at FROM {} ORDER BY priority, id",
            t.table_name()
        );
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt
            .query_map([], map_plugin)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    pub fn list_dispatchers(&self) -> StoreResult<Vec<PluginRow>> {
        self.list_plugins(PluginTable::Dispatchers)
    }

    pub fn list_evaluators(&self) -> StoreResult<Vec<PluginRow>> {
        self.list_plugins(PluginTable::Evaluators)
    }

    /// 按表种类取列表。
    ///
    /// 面板的决策器页与评估器页是**同一套逻辑**（两张表结构完全相同），
    /// 所以路由层需要一个能接受运行期 `PluginTable` 的入口。
    /// 原来那两个具名方法保留 —— 它们在调用点就已经确定了表，
    /// 比传一个变量更不容易写错。
    pub fn list_dispatchers_or(&self, t: PluginTable) -> StoreResult<Vec<PluginRow>> {
        self.list_plugins(t)
    }

    pub fn get_plugin(&self, t: PluginTable, id: i64) -> StoreResult<Option<PluginRow>> {
        let conn = self.lock()?;
        let sql = format!(
            "SELECT id, name, filename, enabled, active, priority, description, \
             is_builtin, created_at FROM {} WHERE id=?",
            t.table_name()
        );
        let row = conn.query_row(&sql, params![id], map_plugin).optional()?;
        Ok(row)
    }

    /// 取当前激活的插件（同表内理论上只有一个 `active=1`）。
    pub fn active_plugin(&self, t: PluginTable) -> StoreResult<Option<PluginRow>> {
        let conn = self.lock()?;
        let sql = format!(
            "SELECT id, name, filename, enabled, active, priority, description, \
             is_builtin, created_at FROM {} WHERE active=1 ORDER BY priority, id LIMIT 1",
            t.table_name()
        );
        let row = conn.query_row(&sql, [], map_plugin).optional()?;
        Ok(row)
    }

    /// 调度器选分发器用：只看 `active=1`，**不看 `enabled`**。
    ///
    /// 这是照抄原版 `panel/scheduler.py::_build_report` 的
    /// `SELECT filename FROM dispatchers WHERE active=1 LIMIT 1`。
    /// 「active 是选中，enabled 是可用」两列在原版里是分开的语义，
    /// 调度器只认前者 —— 别自作主张加上 `enabled=1`。
    ///
    /// 原版没写 `ORDER BY`，同表多行 `active=1` 时取哪行是不确定的；
    /// 这里补上 `ORDER BY id`，把不确定性收敛成「取最早建的」。
    pub fn active_dispatcher_filename(&self) -> StoreResult<Option<String>> {
        self.plugin_filename_where(PluginTable::Dispatchers, "active=1")
    }

    /// 没有任何 `active=1` 的分发器时，回落到内置分发器。
    /// 对应原版的 `SELECT filename FROM dispatchers WHERE is_builtin=1 LIMIT 1`。
    pub fn builtin_dispatcher_filename(&self) -> StoreResult<Option<String>> {
        self.plugin_filename_where(PluginTable::Dispatchers, "is_builtin=1")
    }

    /// 同上的整行版本 —— 调用方要用到 `name` / `filename` 而不只是文件名。
    pub fn builtin_dispatcher(&self) -> StoreResult<Option<PluginRow>> {
        self.first_plugin_where(PluginTable::Dispatchers, "is_builtin=1")
    }

    /// 调度器加载评估器用：这里**要** `enabled=1`。
    ///
    /// 对应原版 `panel/evaluator_hook.py` 的
    /// `... WHERE active=1 AND enabled=1 LIMIT 1`。
    /// 现网 `evaluators` 表里 `script_team.py` 是 `active=0/enabled=0`，
    /// `user_jbdui.py` 是 `active=1/enabled=1` —— 少了 `enabled=1`
    /// 会把被停用的内置评估器选出来，这条条件是实打实吃重的。
    pub fn active_evaluator_filename(&self) -> StoreResult<Option<String>> {
        self.plugin_filename_where(PluginTable::Evaluators, "active=1 AND enabled=1")
    }

    fn plugin_filename_where(&self, t: PluginTable, cond: &str) -> StoreResult<Option<String>> {
        let conn = self.lock()?;
        let sql = format!(
            "SELECT filename FROM {} WHERE {} ORDER BY id LIMIT 1",
            t.table_name(),
            cond
        );
        let row = conn
            .query_row(&sql, [], |r| r.get::<_, String>(0))
            .optional()?;
        Ok(row)
    }

    /// 按条件取一行（整行）。
    ///
    /// `cond` 是**拼进 SQL 的字符串**，不是参数 —— 它只能来自代码里的
    /// 字面量（`"active=1"` / `"is_builtin=1"`），绝不允许来自请求。
    /// 写成 `&str` 而不是 `enum` 是为了让「这两种筛选」看起来一样；
    /// 真需要参数化筛选时该另加方法，不要往这里塞用户输入。
    fn first_plugin_where(&self, t: PluginTable, cond: &str) -> StoreResult<Option<PluginRow>> {
        let conn = self.lock()?;
        let sql = format!(
            "SELECT id, name, filename, enabled, active, priority, description, \
             is_builtin, created_at FROM {} WHERE {} ORDER BY id LIMIT 1",
            t.table_name(),
            cond
        );
        let row = conn.query_row(&sql, [], map_plugin).optional()?;
        Ok(row)
    }

    pub fn add_plugin(&self, t: PluginTable, p: &NewPlugin) -> StoreResult<i64> {
        let conn = self.lock()?;
        let sql = format!(
            "INSERT INTO {} (name, filename, enabled, active, priority, description, \
             is_builtin, created_at) VALUES (?,?,?,?,?,?,?,?)",
            t.table_name()
        );
        conn.execute(
            &sql,
            params![
                p.name,
                p.filename,
                p.enabled as i64,
                p.active as i64,
                p.priority,
                p.description,
                p.is_builtin as i64,
                now_stamp()
            ],
        )
        .map_err(|e| {
            if is_unique_violation(&e) {
                StoreError::Conflict(format!("名称 {} 已存在", p.name))
            } else {
                StoreError::Sqlite(e)
            }
        })?;
        Ok(conn.last_insert_rowid())
    }

    pub fn update_plugin(
        &self,
        t: PluginTable,
        id: i64,
        name: Option<&str>,
        description: Option<&str>,
        priority: Option<i64>,
        enabled: Option<bool>,
    ) -> StoreResult<bool> {
        let conn = self.lock()?;
        let sql = format!(
            "UPDATE {} SET name=COALESCE(?, name), description=COALESCE(?, description), \
             priority=COALESCE(?, priority), enabled=COALESCE(?, enabled) WHERE id=?",
            t.table_name()
        );
        let n = conn.execute(
            &sql,
            params![
                name,
                description,
                priority,
                enabled.map(|b| b as i64),
                id
            ],
        )?;
        Ok(n > 0)
    }

    /// 设为当前激活项（同表内先全部取消，再置一）。
    ///
    /// 用事务保证「不会出现 0 个或 2 个 active」——面板的调度器依赖这个不变量。
    pub fn activate_plugin(&self, t: PluginTable, id: i64) -> StoreResult<bool> {
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let table = t.table_name();
        let exists: i64 = tx.query_row(
            &format!("SELECT COUNT(*) FROM {table} WHERE id=?"),
            params![id],
            |r| r.get(0),
        )?;
        if exists == 0 {
            return Ok(false);
        }
        tx.execute(&format!("UPDATE {table} SET active=0"), [])?;
        tx.execute(
            &format!("UPDATE {table} SET active=1, enabled=1 WHERE id=?"),
            params![id],
        )?;
        tx.commit()?;
        Ok(true)
    }

    pub fn delete_plugin(&self, t: PluginTable, id: i64) -> StoreResult<bool> {
        let conn = self.lock()?;
        let sql = format!("DELETE FROM {} WHERE id=? AND is_builtin=0", t.table_name());
        let n = conn.execute(&sql, params![id])?;
        Ok(n > 0)
    }

    // -------------------------------------------------------- kv

    pub fn get_kv(&self, key: &str) -> StoreResult<Option<String>> {
        let conn = self.lock()?;
        let v = conn
            .query_row("SELECT v FROM kv WHERE k=?", params![key], |r| {
                r.get::<_, Option<String>>(0)
            })
            .optional()?
            .flatten();
        Ok(v)
    }

    pub fn get_kv_or(&self, key: &str, default: &str) -> String {
        self.get_kv(key).ok().flatten().unwrap_or_else(|| default.to_string())
    }

    pub fn set_kv(&self, key: &str, value: &str) -> StoreResult<()> {
        let conn = self.lock()?;
        conn.execute(
            "INSERT INTO kv (k, v) VALUES (?, ?) \
             ON CONFLICT(k) DO UPDATE SET v=excluded.v",
            params![key, value],
        )?;
        Ok(())
    }

    /// 取 kv 里的 JSON 值。
    pub fn get_kv_json(&self, key: &str) -> StoreResult<Option<Value>> {
        let Some(raw) = self.get_kv(key)? else {
            return Ok(None);
        };
        if raw.trim().is_empty() {
            return Ok(None);
        }
        Ok(serde_json::from_str(&raw).ok())
    }

    pub fn set_kv_json<V: Serialize>(&self, key: &str, value: &V) -> StoreResult<()> {
        self.set_kv(key, &serde_json::to_string(value)?)
    }

    /// 所有 kv（面板「设置」页要一次性读出来）。
    pub fn all_kv(&self) -> StoreResult<Vec<(String, String)>> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare("SELECT k, v FROM kv ORDER BY k")?;
        let rows = stmt
            .query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, Option<String>>(1)?.unwrap_or_default(),
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    pub fn delete_kv(&self, key: &str) -> StoreResult<()> {
        let conn = self.lock()?;
        conn.execute("DELETE FROM kv WHERE k=?", params![key])?;
        Ok(())
    }
}

// ---------------------------------------------------------------- 行映射

fn map_user(r: &Row<'_>) -> rusqlite::Result<User> {
    Ok(User {
        id: r.get(0)?,
        username: r.get(1)?,
        password_hash: r.get(2)?,
        is_admin: r.get::<_, i64>(3)? != 0,
        lang: r.get(4)?,
        created_at: r.get::<_, Option<String>>(5)?.unwrap_or_default(),
    })
}

fn map_plugin(r: &Row<'_>) -> rusqlite::Result<PluginRow> {
    Ok(PluginRow {
        id: r.get(0)?,
        name: r.get(1)?,
        filename: r.get(2)?,
        enabled: r.get::<_, i64>(3)? != 0,
        active: r.get::<_, i64>(4)? != 0,
        priority: r.get(5)?,
        description: r.get::<_, Option<String>>(6)?.unwrap_or_default(),
        is_builtin: r.get::<_, i64>(7)? != 0,
        created_at: r.get::<_, Option<String>>(8)?.unwrap_or_default(),
    })
}

/// 行 -> 渠道。`type` 列存的是小写字符串，未知类型会构造失败 ——
/// 这是有意为之：宁可日志里报错，也不静默产出一个「永远不可能发送成功」的渠道。
fn map_channel(r: &Row<'_>) -> rusqlite::Result<alpha_notify::Channel> {
    let type_str: String = r.get(2)?;
    let kind = alpha_notify::ChannelKind::parse(&type_str).ok_or_else(|| {
        rusqlite::Error::FromSqlConversionFailure(
            2,
            rusqlite::types::Type::Text,
            Box::new(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("未知渠道类型: {type_str}"),
            )),
        )
    })?;
    let cfg_raw: String = r.get::<_, Option<String>>(4)?.unwrap_or_default();
    let config = serde_json::from_str(&cfg_raw).unwrap_or_default();

    Ok(alpha_notify::Channel {
        id: Some(r.get(0)?),
        name: r.get(1)?,
        kind,
        enabled: r.get::<_, i64>(3)? != 0,
        config,
    })
}

fn is_unique_violation(e: &rusqlite::Error) -> bool {
    matches!(
        e,
        rusqlite::Error::SqliteFailure(err, _)
            if err.code == rusqlite::ErrorCode::ConstraintViolation
    )
}
