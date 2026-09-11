# -*- coding: utf-8 -*-
"""面板用 SQLite 存储：分发器注册表 + 事件日志 + 用户 + 调度状态 + 设置。"""
import os
import json
import sqlite3
import time

DB_PATH = os.path.join(os.path.dirname(os.path.abspath(__file__)), "panel.db")


def get_db():
    conn = sqlite3.connect(DB_PATH)
    conn.row_factory = sqlite3.Row
    return conn


def init_db():
    conn = get_db()
    c = conn.cursor()
    c.executescript(
        """
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
        """
    )
    # 首次启动确保内置默认分发器存在，并设为启用 + 当前激活
    if c.execute("SELECT COUNT(*) FROM dispatchers").fetchone()[0] == 0:
        c.execute(
            "INSERT INTO dispatchers "
            "(name, filename, enabled, active, priority, description, is_builtin, created_at) "
            "VALUES (?,?,?,?,?,?,?,?)",
            ("默认决策器", "default_dispatcher.py", 1, 1, 10,
             "内置：基于 src/strategy/engine.py 的官方打法引擎", 1,
             time.strftime("%Y-%m-%d %H:%M:%S")),
        )
    conn.commit()
    conn.close()


# ---------------- 日志 ----------------

def log_event(level: str, kind: str, message: str, source: str = ""):
    try:
        conn = get_db()
        conn.execute(
            "INSERT INTO logs (ts, level, kind, message, source) VALUES (?,?,?,?,?)",
            (time.strftime("%Y-%m-%d %H:%M:%S"), level, kind, message, source),
        )
        conn.commit()
        conn.close()
    except Exception:
        pass


def list_logs(limit: int = 200, kind: str = None, source: str = None):
    conn = get_db()
    sql = "SELECT * FROM logs WHERE 1=1"
    args = []
    if kind:
        sql += " AND kind=?"
        args.append(kind)
    if source:
        sql += " AND source=?"
        args.append(source)
    sql += " ORDER BY id DESC LIMIT ?"
    args.append(limit)
    rows = conn.execute(sql, args).fetchall()
    conn.close()
    return [dict(r) for r in rows]


def list_log_sources():
    """日志里出现过的来源（用于面板筛选下拉）。"""
    conn = get_db()
    rows = conn.execute(
        "SELECT DISTINCT source FROM logs WHERE source<>'' ORDER BY source"
    ).fetchall()
    conn.close()
    return [r[0] for r in rows]


def cleanup_logs(days: int = 3):
    try:
        cutoff = time.strftime(
            "%Y-%m-%d %H:%M:%S", time.localtime(time.time() - days * 86400)
        )
        conn = get_db()
        n = conn.execute("DELETE FROM logs WHERE ts < ?", (cutoff,)).rowcount
        conn.commit()
        conn.close()
        return n
    except Exception:
        return 0


# ---------------- 用户 ----------------

def count_users():
    conn = get_db()
    n = conn.execute("SELECT COUNT(*) FROM users").fetchone()[0]
    conn.close()
    return n


def add_user(username, password_hash, is_admin=False, lang="zh"):
    conn = get_db()
    conn.execute(
        "INSERT INTO users (username, password_hash, is_admin, lang, created_at) "
        "VALUES (?,?,?,?,?)",
        (username, password_hash, 1 if is_admin else 0, lang,
         time.strftime("%Y-%m-%d %H:%M:%S")),
    )
    conn.commit()
    conn.close()


def get_user(username):
    conn = get_db()
    row = conn.execute("SELECT * FROM users WHERE username=?", (username,)).fetchone()
    conn.close()
    return dict(row) if row else None


def set_user_lang(username, lang):
    conn = get_db()
    conn.execute("UPDATE users SET lang=? WHERE username=?", (lang, username))
    conn.commit()
    conn.close()


def list_users():
    conn = get_db()
    rows = conn.execute(
        "SELECT id, username, is_admin, lang, created_at FROM users ORDER BY id"
    ).fetchall()
    conn.close()
    return [dict(r) for r in rows]


# ---------------- 分发渠道 ----------------

def list_channels():
    conn = get_db()
    rows = conn.execute(
        "SELECT id, name, type, enabled, config, created_at FROM channels ORDER BY id"
    ).fetchall()
    conn.close()
    out = []
    for r in rows:
        d = dict(r)
        try:
            d["config"] = json.loads(d["config"] or "{}")
        except Exception:
            d["config"] = {}
        out.append(d)
    return out


def get_channel(cid):
    conn = get_db()
    row = conn.execute("SELECT * FROM channels WHERE id=?", (cid,)).fetchone()
    conn.close()
    if not row:
        return None
    d = dict(row)
    try:
        d["config"] = json.loads(d["config"] or "{}")
    except Exception:
        d["config"] = {}
    return d


def add_channel(name, ctype, config=None, enabled=True):
    conn = get_db()
    cur = conn.execute(
        "INSERT INTO channels (name, type, enabled, config, created_at) VALUES (?,?,?,?,?)",
        (name, ctype, 1 if enabled else 0,
         json.dumps(config or {}, ensure_ascii=False),
         time.strftime("%Y-%m-%d %H:%M:%S")),
    )
    conn.commit()
    cid = cur.lastrowid
    conn.close()
    return cid


def update_channel(cid, name=None, config=None, enabled=None):
    conn = get_db()
    row = conn.execute("SELECT * FROM channels WHERE id=?", (cid,)).fetchone()
    if not row:
        conn.close()
        return False
    new_name = name if name is not None else row["name"]
    new_cfg = row["config"]
    if config is not None:
        try:
            old = json.loads(row["config"] or "{}")
        except Exception:
            old = {}
        old.update(config)
        new_cfg = json.dumps(old, ensure_ascii=False)
    new_enabled = row["enabled"] if enabled is None else (1 if enabled else 0)
    conn.execute(
        "UPDATE channels SET name=?, config=?, enabled=? WHERE id=?",
        (new_name, new_cfg, new_enabled, cid),
    )
    conn.commit()
    conn.close()
    return True


def delete_channel(cid):
    conn = get_db()
    n = conn.execute("DELETE FROM channels WHERE id=?", (cid,)).rowcount
    conn.commit()
    conn.close()
    return n > 0


# ---------------- 键值（调度状态 / 设置）----------------

def get_kv(k, default=None):
    conn = get_db()
    row = conn.execute("SELECT v FROM kv WHERE k=?", (k,)).fetchone()
    conn.close()
    if row is None:
        return default
    return row[0]


def set_kv(k, v):
    conn = get_db()
    conn.execute(
        "INSERT INTO kv (k, v) VALUES (?, ?) ON CONFLICT(k) DO UPDATE SET v=excluded.v",
        (k, v),
    )
    conn.commit()
    conn.close()
