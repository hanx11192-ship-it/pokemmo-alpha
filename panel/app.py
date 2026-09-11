# -*- coding: utf-8 -*-
"""
Alpha 分发决策面板 —— Web 服务（单页应用 + 多分页）。

分页：仪表盘 / 调试 / 定时 / 头目源 / 分发器 / 日志 / 系统配置 / 关于
鉴权：首登即管理员；会话 cookie；除登录/me 外所有 /api 需登录。
"""
import os
import sys
import json
import time
import yaml

# 让 panel 能 import 到 src 包
ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
if ROOT not in sys.path:
    sys.path.insert(0, ROOT)

# systemd 用 EnvironmentFile 加载的环境变量文件；面板在这里写回
ENV_PATH = os.environ.get("PANEL_ENV_FILE", os.path.join(ROOT, "panel.env"))

from flask import (
    Flask, request, jsonify, render_template, send_file, abort, Response,
)
from werkzeug.utils import secure_filename

from src.core.pokedex import get_pokedex
from src.core.models import BossData, Gender
from src.strategy.rules import Rules

import panel.db as db
import panel.dispatch as dispatch_mod
import panel.auth as auth
import panel.config_mgr as cfgmgr
import panel.scheduler as scheduler_mod
import panel.channels as channel_mod

app = Flask(__name__)
app.secret_key = os.environ.get("PANEL_SECRET", "alpha-panel-dev-secret")

_pokedex = None
_rules = None


def pokedex():
    global _pokedex
    if _pokedex is None:
        _pokedex = get_pokedex()
    return _pokedex


def rules():
    global _rules
    if _rules is None:
        doc = yaml.safe_load(open(os.path.join(ROOT, "config", "rules.yaml"), encoding="utf-8"))
        _rules = Rules(doc, pokedex())
    return _rules


def pokedex_lists():
    pk = pokedex()
    pokemon = []
    for pid, e in pk.pokemon.items():
        pokemon.append({
            "id": int(pid), "zh": e.get("zh", ""), "en": e.get("en", ""),
            "g": e.get("gender_rate"), "eg": e.get("egg_groups", []),
            "ha": e.get("hidden_ability", ""),
        })
    abilities = [{"id": int(a), "zh": v.get("zh", ""), "en": v.get("en", "")}
                 for a, v in pk.abilities.items()]
    moves = [{"id": int(m), "zh": v.get("zh", ""), "en": v.get("en", "")}
             for m, v in pk.moves.items()]
    return {"pokemon": pokemon, "abilities": abilities, "moves": moves}


# ============================================================
# 登录 / 会话
# ============================================================

@app.route("/api/me")
def api_me():
    if auth.is_authenticated():
        u = db.get_user(auth.current_user())
        lang = u.get("lang", "zh") if u else "zh"
        return jsonify({"authenticated": True, "user": auth.current_user(),
                        "role": auth.current_role(), "lang": lang})
    return jsonify({"authenticated": False, "user": None, "role": None, "lang": "zh"})


@app.route("/api/login", methods=["POST"])
def api_login():
    data = request.get_json(silent=True) or {}
    res = auth.do_login(data.get("username", ""), data.get("password", ""))
    if res is None:
        return jsonify({"ok": False, "error": "用户名不存在或密码错误"}), 401
    return jsonify({"ok": True, **res})


@app.route("/api/logout", methods=["POST"])
def api_logout():
    auth.do_logout()
    return jsonify({"ok": True})


@app.route("/api/lang", methods=["POST"])
def api_set_lang():
    """设置当前用户面板语言偏好（zh/en）。"""
    if not auth.is_authenticated():
        return jsonify({"ok": False, "error": "未登录"}), 401
    data = request.get_json(silent=True) or {}
    lang = data.get("lang")
    if lang not in ("zh", "en"):
        return jsonify({"ok": False, "error": "语言不支持"}), 400
    db.set_user_lang(auth.current_user(), lang)
    return jsonify({"ok": True, "lang": lang})


# ============================================================
# 单页应用外壳
# ============================================================

@app.route("/")
def index():
    return render_template("index.html")


# ============================================================
# 仪表盘
# ============================================================

@app.route("/api/dashboard")
@auth.login_required
def api_dashboard():
    srcs = cfgmgr.list_sources()
    enabled = sum(1 for s in srcs if s.get("enabled"))
    logs = db.list_logs(limit=12)
    spawn_logs = db.list_logs(limit=200, kind="spawn")
    disp = db.get_db().execute(
        "SELECT name FROM dispatchers WHERE active=1 LIMIT 1").fetchone()
    conn = db.get_db()
    total_logs = conn.execute("SELECT COUNT(*) FROM logs").fetchone()[0]
    conn.close()
    sc = scheduler_mod.get_scheduler_state()
    pk = pokedex()
    return jsonify({
        "sources": {"total": len(srcs), "enabled": enabled},
        "recent_logs": logs,
        "spawn_count_200": len(spawn_logs),
        "total_logs": total_logs,
        "active_dispatcher": disp["name"] if disp else None,
        "scheduler": sc,
        "pokedex": {"pokemon": len(pk.pokemon), "abilities": len(pk.abilities), "moves": len(pk.moves)},
    })


# ============================================================
# 调试分页
# ============================================================

@app.route("/api/pokedex")
@auth.login_required
def api_pokedex():
    return jsonify(pokedex_lists())


@app.route("/debug/run", methods=["POST"])
@auth.login_required
def debug_run():
    data = request.get_json(silent=True) or {}
    pk = pokedex()

    pokemon_in = (data.get("pokemon") or "").strip()
    ability_in = (data.get("ability") or "").strip()
    moves_in = [m.strip() for m in (data.get("moves") or []) if m.strip()]
    gender = data.get("gender", "dual")

    if not pokemon_in:
        return jsonify({"ok": False, "error": "请选择或填写头目精灵"}), 400

    name_zh = pk.canonical_pokemon(pokemon_in)
    pid = pk.resolve_pokemon_id(pokemon_in)
    ability_zh = pk.canonical_ability(ability_in) if ability_in else "无特性"
    aid = pk.resolve_ability_id(ability_in) if ability_in else None
    moves_zh = [pk.canonical_move(m) for m in moves_in]
    move_ids = [pk.resolve_move_id(m) for m in moves_in]
    gender_pct = {"dual": 50.0, "male": 100.0, "female": 0.0, "none": None}.get(gender, 50.0)
    egg = data.get("egg_groups") or (pk.pokemon.get(str(pid), {}).get("egg_groups") if pid else [])

    boss = BossData(
        name=name_zh, ability=ability_zh, moves=moves_zh,
        gender=Gender.from_male_percent(gender_pct), egg_groups=egg,
        pokedex_id=pid, ability_id=aid, move_ids=move_ids,
        source="debug", reporter="debug",
    )

    disp_id = data.get("dispatcher_id")
    row = None
    if disp_id:
        row = db.get_db().execute(
            "SELECT * FROM dispatchers WHERE id=? AND enabled=1", (disp_id,)).fetchone()
    if not row:
        row = db.get_db().execute("SELECT * FROM dispatchers WHERE active=1 LIMIT 1").fetchone()
    if not row:
        row = db.get_db().execute("SELECT * FROM dispatchers WHERE is_builtin=1 LIMIT 1").fetchone()
    if not row:
        return jsonify({"ok": False, "error": "没有可用的分发器"}), 500

    lang = data.get("lang", "zh")
    langs = {"zh": ["zh"], "en": ["en"], "both": ["zh", "en"]}.get(lang, ["zh"])

    try:
        report = dispatch_mod.run_dispatcher(
            row["filename"], boss, {"pokedex": pk, "rules": rules(), "langs": langs})
    except Exception as e:
        return jsonify({"ok": False, "error": f"分发器执行失败: {e}"}), 500

    db.log_event("debug", "debug", f"调试运行：{name_zh}({ability_zh}) -> 分发器[{row['name']}]", source="debug")
    return jsonify({
        "ok": True, "dispatcher": row["name"],
        "resolved": {
            "name": name_zh, "pokemon_id": pid, "ability": ability_zh, "ability_id": aid,
            "moves": moves_zh, "move_ids": [m for m in move_ids if m is not None],
            "egg_groups": egg,
        },
        "report": report,
    })


# ============================================================
# 定时分页
# ============================================================

@app.route("/api/scheduler")
@auth.login_required
def api_scheduler():
    return jsonify(scheduler_mod.get_scheduler_state())


@app.route("/api/scheduler/set", methods=["POST"])
@auth.login_required
def api_scheduler_set():
    data = request.get_json(silent=True) or {}
    enabled = bool(data.get("enabled", False))
    interval = data.get("interval")
    scheduler_mod.set_scheduler(enabled, int(interval) if interval else None)
    db.log_event("info", "scheduler", f"定时任务{'启用' if enabled else '停用'}"
                 + (f"，间隔 {interval}s" if interval else ""), source="panel")
    return jsonify({"ok": True, **scheduler_mod.get_scheduler_state()})


@app.route("/api/scheduler/debug", methods=["POST"])
@auth.login_required
def api_scheduler_debug():
    """手动触发一次轮询（debug：只生成报告不推送）。

    异步执行：源卡住时不再让页面干等，立即返回，前端轮询 GET 拿结果。
    """
    return jsonify(scheduler_mod.start_debug_async())


@app.route("/api/scheduler/debug", methods=["GET"])
@auth.login_required
def api_scheduler_debug_state():
    """查询后台 debug 轮询的状态/结果。"""
    return jsonify(scheduler_mod.get_debug_state())


# ============================================================
# 头目源分页
# ============================================================

@app.route("/api/sources")
@auth.login_required
def api_sources():
    srcs = [cfgmgr.describe_source(s) for s in cfgmgr.list_sources()]
    adapters = cfgmgr.list_adapter_files()
    return jsonify({"sources": srcs, "adapters": adapters})


@app.route("/api/sources", methods=["POST"])
@auth.login_required
def api_source_add():
    data = request.get_json(silent=True) or {}
    name = (data.get("name") or "").strip()
    adapter = (data.get("adapter") or "").strip()
    if not name or not adapter:
        return jsonify({"ok": False, "error": "名称和适配器必填"}), 400
    if cfgmgr.get_source(name):
        return jsonify({"ok": False, "error": "同名源已存在"}), 400
    scfg = {
        "name": name, "adapter": adapter,
        "enabled": bool(data.get("enabled", True)),
        "priority": int(data.get("priority", 50)),
        "options": {"target": (data.get("target") or "").strip()},
    }
    cfgmgr.upsert_source(scfg)
    db.log_event("info", "source", f"新增源：{name} (adapter={adapter})", source="panel")
    return jsonify({"ok": True})


@app.route("/api/sources/<name>/enable", methods=["POST"])
@auth.login_required
def api_source_enable(name):
    body = request.get_json(silent=True) or {}
    cfgmgr.set_source_enabled(name, bool(body.get("enabled", True)))
    return jsonify({"ok": True})


@app.route("/api/sources/<name>/priority", methods=["POST"])
@auth.login_required
def api_source_priority(name):
    body = request.get_json(silent=True) or {}
    cfgmgr.set_source_priority(name, int(body.get("priority", 50)))
    return jsonify({"ok": True})


@app.route("/api/sources/<name>/edit", methods=["POST"])
@auth.login_required
def api_source_edit(name):
    """编辑一个源。支持改名（rename 会先删旧键再写新键，保持顺序位置）。"""
    body = request.get_json(silent=True) or {}
    s = cfgmgr.get_source(name)
    if not s:
        return jsonify({"ok": False, "error": "源不存在"}), 404

    if "enabled" in body:
        s["enabled"] = bool(body["enabled"])
    if "priority" in body:
        s["priority"] = int(body["priority"])
    if "adapter" in body and str(body["adapter"]).strip():
        s["adapter"] = str(body["adapter"]).strip()
    if "note" in body:
        s["note"] = str(body["note"]).strip()
    if "target" in body:
        s.setdefault("options", {})["target"] = str(body["target"]).strip()

    new_name = str(body.get("name") or "").strip()
    renamed = False
    if new_name and new_name != name:
        if cfgmgr.get_source(new_name):
            return jsonify({"ok": False, "error": "已存在同名源"}), 400
        s["name"] = new_name
        cfgmgr.replace_source(name, s)
        renamed = True
    else:
        cfgmgr.upsert_source(s)
    db.log_event("info", "source",
                 f"编辑源：{name}" + (f" -> {new_name}" if renamed else ""),
                 source="panel")
    return jsonify({"ok": True, "name": s["name"], "renamed": renamed})


@app.route("/api/sources/<name>", methods=["DELETE"])
@auth.login_required
def api_source_delete(name):
    if not cfgmgr.remove_source(name):
        return jsonify({"ok": False, "error": "源不存在"}), 404
    db.log_event("info", "source", f"删除源：{name}", source="panel")
    return jsonify({"ok": True})


@app.route("/api/sources/upload", methods=["POST"])
@auth.login_required
def api_source_upload():
    f = request.files.get("file")
    name = (request.form.get("name") or "").strip()
    if not f:
        return jsonify({"ok": False, "error": "未收到文件"}), 400
    raw = f.read().decode("utf-8", "ignore")
    if ("def fetch" not in raw) and ("BaseSource" not in raw):
        return jsonify({"ok": False, "error": "适配器需实现 fetch() 且继承 BaseSource"}), 400
    base = secure_filename((name or f.filename or "source")).rsplit(".", 1)[0]
    base = "".join(ch for ch in base if ch.isalnum() or ch in "_-")
    if not base:
        base = "source"
    filename = f"{base}.py"
    path = os.path.join(ROOT, "src", "sources", filename)
    if os.path.exists(path):
        return jsonify({"ok": False, "error": f"已存在同名适配器: {filename}"}), 400
    with open(path, "w", encoding="utf-8") as fp:
        fp.write(raw)
    db.log_event("info", "source", f"上传适配器：{filename}", source="panel")
    return jsonify({"ok": True, "filename": filename, "module": base})


@app.route("/api/sources/<name>/download")
@auth.login_required
def api_source_download(name):
    path = os.path.join(ROOT, "src", "sources", f"{name}.py")
    if not os.path.exists(path):
        abort(404)
    return send_file(path, as_attachment=True, download_name=f"{name}.py")


@app.route("/api/sources/template")
@auth.login_required
def api_source_template():
    tmpl = SOURCE_TEMPLATE
    return Response(tmpl, mimetype="text/plain",
                    headers={"Content-Disposition": "attachment; filename=source_template.py"})


SOURCE_TEMPLATE = '''# -*- coding: utf-8 -*-
"""数据源适配器模板：实现 fetch() -> FetchResult 即可被主流程加载。

把本文件放到 src/sources/ 下（文件名即模块名），然后在「头目源」页
新增一个源，adapter 填本文件名（不含 .py）。

要点：
  - 能用英文/标准名字段就别用机翻中文，再用 pokedex 解析成官方译名
  - 头目还活着才返回 hit；时段空档期用 isActive / 报点时间判断
  - 去重标识优先用数据源给的时段唯一标识
"""
from ..core.models import BossData, FetchResult, Gender, ExtraLine
from .base import BaseSource, parse_male_ratio


class MySource(BaseSource):
    name = "my_source"

    def fetch(self) -> FetchResult:
        # 1. 拉数据（可用 self.request_json(url, headers)）
        # data = self.request_json(self.options.get("target"), {})
        # 2. 解析成 BossData（优先英文名查图鉴，避开机翻）
        # boss = BossData(name=..., ability=..., moves=[...], ...)
        # 3. 返回命中 / 空 / 错误
        return FetchResult("empty", message="未实现，请补全 fetch()")
'''


# ============================================================
# 分发分页（决策器）
# ============================================================

@app.route("/api/dispatchers")
@auth.login_required
def api_dispatchers():
    rows = db.get_db().execute(
        "SELECT id, name, filename, enabled, active, priority, description, is_builtin "
        "FROM dispatchers ORDER BY priority, id").fetchall()
    return jsonify([dict(r) for r in rows])


@app.route("/api/dispatchers/<int:did>/enable", methods=["POST"])
@auth.login_required
def api_disp_enable(did):
    body = request.get_json(silent=True) or {}
    db.get_db().execute("UPDATE dispatchers SET enabled=? WHERE id=?",
                        (1 if body.get("enabled", True) else 0, did))
    db.get_db().commit()
    return jsonify({"ok": True})


@app.route("/api/dispatchers/<int:did>/activate", methods=["POST"])
@auth.login_required
def api_disp_activate(did):
    conn = db.get_db()
    conn.execute("UPDATE dispatchers SET active=0")
    conn.execute("UPDATE dispatchers SET active=1 WHERE id=?", (did,))
    conn.commit()
    return jsonify({"ok": True})


@app.route("/api/dispatchers/<int:did>/edit", methods=["POST"])
@auth.login_required
def api_disp_edit(did):
    body = request.get_json(silent=True) or {}
    conn = db.get_db()
    row = conn.execute("SELECT * FROM dispatchers WHERE id=?", (did,)).fetchone()
    if not row:
        conn.close()
        return jsonify({"ok": False, "error": "不存在"}), 404
    name = str(body.get("name") or row["name"]).strip()
    if name != row["name"]:
        dup = conn.execute(
            "SELECT id FROM dispatchers WHERE name=? AND id<>?", (name, did)).fetchone()
        if dup:
            conn.close()
            return jsonify({"ok": False, "error": "已存在同名分发器"}), 400
    priority = int(body.get("priority", row["priority"]))
    desc = body.get("description")
    desc = row["description"] if desc is None else str(desc)
    conn.execute("UPDATE dispatchers SET name=?, priority=?, description=? WHERE id=?",
                 (name, priority, desc, did))
    conn.commit()
    conn.close()
    db.log_event("info", "dispatcher", f"编辑分发器：{row['name']} -> {name}", source="panel")
    return jsonify({"ok": True, "name": name})


@app.route("/api/dispatchers/<int:did>/delete", methods=["POST"])
@auth.login_required
def api_disp_delete(did):
    conn = db.get_db()
    row = conn.execute("SELECT * FROM dispatchers WHERE id=?", (did,)).fetchone()
    if not row:
        return jsonify({"ok": False, "error": "不存在"}), 404
    if row["is_builtin"]:
        return jsonify({"ok": False, "error": "内置分发器不可删除"}), 400
    path = os.path.join(dispatch_mod.DISP_DIR, row["filename"])
    if os.path.exists(path):
        os.remove(path)
    conn.execute("DELETE FROM dispatchers WHERE id=?", (did,))
    conn.commit()
    db.log_event("info", "dispatcher", f"删除分发器：{row['name']}", source="panel")
    return jsonify({"ok": True})


@app.route("/api/dispatchers/upload", methods=["POST"])
@auth.login_required
def api_disp_upload():
    f = request.files.get("file")
    name = (request.form.get("name") or "").strip()
    if not f:
        return jsonify({"ok": False, "error": "未收到文件"}), 400
    raw = f.read().decode("utf-8", "ignore")
    if "def dispatch" not in raw:
        return jsonify({"ok": False, "error": "分发器必须实现 dispatch(boss, ctx) 函数"}), 400
    base = secure_filename((name or f.filename or "dispatcher")).rsplit(".", 1)[0]
    base = "".join(ch for ch in base if ch.isalnum() or ch in "_-")
    if not base:
        base = "dispatcher"
    filename = f"user_{base}.py"
    path = os.path.join(dispatch_mod.DISP_DIR, filename)
    if os.path.exists(path):
        return jsonify({"ok": False, "error": f"已存在同名分发器文件: {filename}"}), 400
    with open(path, "w", encoding="utf-8") as fp:
        fp.write(raw)
    mod = dispatch_mod.load_module(filename)
    if mod is None:
        os.remove(path)
        return jsonify({"ok": False, "error": "加载失败：缺少 dispatch 函数或语法错误"}), 400
    desc = getattr(mod, "DESCRIPTION", "") or getattr(mod, "NAME", "") or "用户上传的分发器"
    conn = db.get_db()
    maxp = conn.execute("SELECT MAX(priority) FROM dispatchers").fetchone()[0] or 10
    conn.execute(
        "INSERT INTO dispatchers (name, filename, enabled, active, priority, description, is_builtin, created_at) "
        "VALUES (?,?,?,?,?,?,?,?)",
        (name or base, filename, 1, 0, (maxp + 1), desc, 0, time.strftime("%Y-%m-%d %H:%M:%S")))
    conn.commit()
    db.log_event("info", "dispatcher", f"上传分发器：{name or base}", source="panel")
    return jsonify({"ok": True, "filename": filename})


@app.route("/api/dispatchers/<int:did>/download")
@auth.login_required
def api_disp_download(did):
    row = db.get_db().execute("SELECT * FROM dispatchers WHERE id=?", (did,)).fetchone()
    if not row:
        abort(404)
    path = os.path.join(dispatch_mod.DISP_DIR, row["filename"])
    if not os.path.exists(path):
        abort(404)
    return send_file(path, as_attachment=True, download_name=row["filename"])


@app.route("/api/dispatchers/template")
@auth.login_required
def api_disp_template():
    tmpl = '''# -*- coding: utf-8 -*-
"""分发器模板：实现 dispatch(boss, ctx) 即可被面板加载。

  boss : src.core.models.BossData
  ctx  : {"pokedex": Pokedex, "rules": Rules, "langs": List[str]}
返回：一段可直接推送 / 展示的文本
"""
from src.strategy.engine import generate_report, generate_bilingual

NAME = "我的分发器"
DESCRIPTION = "一句话描述这个分发器的打法思路"


def dispatch(boss, ctx: dict) -> str:
    rules = ctx["rules"]
    pokedex = ctx["pokedex"]
    langs = ctx.get("langs") or ["zh"]
    if "zh" in langs and "en" in langs:
        return generate_bilingual(boss, rules, pokedex, langs)
    return generate_report(boss, rules, pokedex, langs[0])
'''
    return Response(tmpl, mimetype="text/plain",
                    headers={"Content-Disposition": "attachment; filename=dispatcher_template.py"})


# ============================================================
# 分发渠道分页
# ============================================================

@app.route("/api/channels")
@auth.login_required
def api_channels():
    chs = db.list_channels()
    # 脱敏：密码类字段不回传明文，只回传是否有值
    for c in chs:
        safe = {}
        for k, v in (c["config"] or {}).items():
            if k in ("app_token", "send_key") and v:
                safe[k] = "••••••" + str(v)[-4:]
            else:
                safe[k] = v
        c["config"] = safe
    return jsonify({"channels": chs, "types": channel_mod.CHANNEL_TYPES})


@app.route("/api/channels", methods=["POST"])
@auth.login_required
def api_channel_add():
    data = request.get_json(silent=True) or {}
    name = (data.get("name") or "").strip()
    ctype = (data.get("type") or "").strip()
    if not name:
        return jsonify({"ok": False, "error": "名称必填"}), 400
    if ctype not in channel_mod.CHANNEL_TYPES:
        return jsonify({"ok": False, "error": "未知渠道类型"}), 400
    cid = db.add_channel(name, ctype, data.get("config") or {}, bool(data.get("enabled", True)))
    db.log_event("info", "channel", f"新增分发渠道：{name}（{ctype}）", source="panel")
    return jsonify({"ok": True, "id": cid})


@app.route("/api/channels/<int:cid>/edit", methods=["POST"])
@auth.login_required
def api_channel_edit(cid):
    data = request.get_json(silent=True) or {}
    ch = db.get_channel(cid)
    if not ch:
        return jsonify({"ok": False, "error": "渠道不存在"}), 404
    cfg = data.get("config")
    if isinstance(cfg, dict):
        # 前端回传的脱敏占位值不覆盖原值
        cfg = {k: v for k, v in cfg.items() if not (isinstance(v, str) and v.startswith("••••••"))}
    db.update_channel(cid, name=data.get("name"), config=cfg, enabled=data.get("enabled"))
    db.log_event("info", "channel", f"编辑分发渠道：{ch['name']}", source="panel")
    return jsonify({"ok": True})


@app.route("/api/channels/<int:cid>/enable", methods=["POST"])
@auth.login_required
def api_channel_enable(cid):
    body = request.get_json(silent=True) or {}
    db.update_channel(cid, enabled=bool(body.get("enabled", True)))
    return jsonify({"ok": True})


@app.route("/api/channels/<int:cid>", methods=["DELETE"])
@auth.login_required
def api_channel_delete(cid):
    ch = db.get_channel(cid)
    if not db.delete_channel(cid):
        return jsonify({"ok": False, "error": "渠道不存在"}), 404
    db.log_event("info", "channel", f"删除分发渠道：{ch['name'] if ch else cid}", source="panel")
    return jsonify({"ok": True})


@app.route("/api/channels/<int:cid>/test", methods=["POST"])
@auth.login_required
def api_channel_test(cid):
    ch = db.get_channel(cid)
    if not ch:
        return jsonify({"ok": False, "error": "渠道不存在"}), 404
    r = channel_mod.test_channel(ch)
    db.log_event("info" if r["ok"] else "error", "channel",
                 f"测试推送：{ch['name']} - {'成功' if r['ok'] else r.get('error')}",
                 source=ch["type"])
    return jsonify(r)


@app.route("/api/channels/test-all", methods=["POST"])
@auth.login_required
def api_channel_test_all():
    """给所有启用渠道发一条测试消息。"""
    data = request.get_json(silent=True) or {}
    text = data.get("content") or "【Alpha 面板 · 测试推送】\n如果你收到这条消息，说明渠道配置正确。"
    res = channel_mod.send_all(text, "面板测试推送")
    return jsonify({"ok": True, **res})


# ============================================================
# 日志分页
# ============================================================

@app.route("/api/logs")
@auth.login_required
def api_logs():
    kind = request.args.get("kind")
    source = request.args.get("source")
    limit = int(request.args.get("limit", 200))
    rows = db.list_logs(limit=limit, kind=kind, source=source)
    return jsonify({"logs": rows, "sources": db.list_log_sources(),
                    "query_log": db.get_kv("query_log", "1") == "1"})


@app.route("/api/logs/clean", methods=["POST"])
@auth.login_required
def api_logs_clean():
    data = request.get_json(silent=True) or {}
    days = int(data.get("days", 3))
    n = db.cleanup_logs(days)
    db.log_event("info", "log", f"清理 {days} 天前日志，删除 {n} 条", source="panel")
    return jsonify({"ok": True, "deleted": n})


@app.route("/api/logs/state", methods=["POST"])
@auth.login_required
def api_logs_state():
    """切换「实时日志」开关。开启后定时轮询把每个源的查询结果也写进日志表。"""
    data = request.get_json(silent=True) or {}
    on = bool(data.get("enabled", False))
    db.set_kv("query_log", "1" if on else "0")
    db.log_event("info", "log", f"源查询日志{'开启' if on else '关闭'}", source="panel")
    return jsonify({"ok": True, "enabled": on})


# ============================================================
# 系统配置 / 关于
# ============================================================

@app.route("/api/system")
@auth.login_required
def api_system():
    s = cfgmgr.load_settings()
    push_lang = db.get_kv("push_lang") or s.get("language") or "zh"
    panel_lang = auth.current_user() and (db.get_user(auth.current_user()) or {}).get("lang", "zh") or "zh"
    # 环境变量：只回传是否已设置，不回传明文
    env_list = [
        {"key": "transfor_url", "label": "转发服务地址 (transfor_url)",
         "desc": "VPS 直连不通时用中转服务，源配置里通过 transform_url_env 引用"},
        {"key": "WXPUSHER_APP_TOKEN_alpha", "label": "WxPusher Token",
         "desc": "渠道页未单独填 Token 时的兜底"},
    ]
    for e in env_list:
        v = os.environ.get(e["key"], "")
        e["set"] = bool(v)
        e["preview"] = ("••••" + v[-4:]) if v else ""
    return jsonify({
        "timezone": s.get("timezone", "Asia/Shanghai"),
        "push_lang": push_lang,
        "panel_lang": panel_lang,
        "envs": env_list,
        "env_file": ENV_PATH,
        "timezones": [
            "Asia/Shanghai", "Asia/Tokyo", "Asia/Hong_Kong", "Asia/Taipei",
            "Asia/Singapore", "UTC", "Europe/London", "America/New_York",
            "America/Los_Angeles",
        ],
    })


@app.route("/api/system/save", methods=["POST"])
@auth.login_required
def api_system_save():
    data = request.get_json(silent=True) or {}
    if "timezone" in data:
        cfgmgr.set_setting_timezone(data["timezone"])
    if "push_lang" in data and data["push_lang"] in ("zh", "en", "both"):
        db.set_kv("push_lang", data["push_lang"])
        cfgmgr.set_setting_language(data["push_lang"])
    if "panel_lang" in data and data["panel_lang"] in ("zh", "en"):
        db.set_user_lang(auth.current_user(), data["panel_lang"])

    # 环境变量：写进 panel.env 并热更新到当前进程（无需重启服务）
    envs = data.get("envs")
    restarted = False
    if isinstance(envs, dict):
        changed = cfgmgr.update_env_file(envs)
        for k, v in envs.items():
            if v is None:
                continue
            v = str(v).strip()
            if v == "":
                os.environ.pop(k, None)
            else:
                os.environ[k] = v
        if changed:
            restarted = cfgmgr.reload_core_config()
            db.log_event("info", "system",
                         f"更新环境变量：{', '.join(envs.keys())}"
                         + ("（已重载配置）" if restarted else ""), source="panel")
    db.log_event("info", "system", "保存系统配置", source="panel")
    return jsonify({"ok": True, "restarted": restarted})


@app.route("/api/about")
@auth.login_required
def api_about():
    """关于页内容。随面板语言切换，所以每个字段都给中英两份。

    分三块：project（面板介绍）/ thanks（致谢）/ examples（接入示例）。
    Three sections: project / thanks / examples.
    """
    return jsonify({
        "title": {"zh": "Alpha 分发决策面板", "en": "Alpha Dispatch Decision Panel"},
        "version": "2.0",
        "desc": {
            "zh": "Pokemmo 头目自动监控面板：多源监控 → 投票裁决 → 跨源去重 → 决策 → 分发。",
            "en": "Automated Pokemmo alpha-boss monitor: monitor → vote → dedupe → "
                  "decide → dispatch.",
        },
        # ---- 此项目：面板介绍 + 完整流程 ----
        "project": {
            "zh": {
                "intro": "这是一个 Pokemmo 头目（Alpha）的自动监控与播报面板。"
                         "它会同时盯着多个数据源，自己判断哪个头目是真的、值不值得报，"
                         "生成播报内容，再发到你配置的各个渠道。",
                "flow": [
                    {"name": "多源监控",
                     "note": "并发轮询所有已启用的数据源。某个源超时或报错不影响其他源，"
                             "本轮直接跳过它，不等它。"},
                    {"name": "投票裁决",
                     "note": "多个源给出不同结果时，按头目内容算指纹投票，"
                             "得票最多的那个胜出。单源报错不会污染结论。"},
                    {"name": "跨源去重",
                     "note": "同一个头目在不同源里可能重复出现，按全局去重键去重，"
                             "同一个时段只报一次。"},
                    {"name": "决策",
                     "note": "由「决策器」决定播报长什么样：用哪套打法、给哪些信息、"
                             "中英还是双语。决策器可插拔，可上传自己的。"},
                    {"name": "分发",
                     "note": "由「分发渠道」决定发到哪里：WxPusher、自定义 Webhook、"
                             "Server 酱等，多渠道同时发，任一成功即算推送成功。"},
                ],
            },
            "en": {
                "intro": "An automated monitor and broadcast panel for Pokemmo alpha "
                         "bosses. It watches several data sources at once, decides on "
                         "its own which alpha is real and worth reporting, renders the "
                         "broadcast, then pushes it to every channel you configured.",
                "flow": [
                    {"name": "Monitor",
                     "note": "Polls every enabled source concurrently. A source that "
                             "times out or errors doesn't hold up the others — it's "
                             "just skipped for this round."},
                    {"name": "Vote",
                     "note": "When sources disagree, it fingerprints the boss content "
                             "and votes; the majority wins. One broken source can't "
                             "poison the result."},
                    {"name": "Dedupe",
                     "note": "The same alpha may surface in several sources. A global "
                             "dedup key ensures each time slot is reported once."},
                    {"name": "Decide",
                     "note": "The decider shapes the report: which strategy to use, "
                             "what to include, Chinese / English / bilingual. "
                             "Pluggable — upload your own."},
                    {"name": "Dispatch",
                     "note": "Channels decide where it goes: WxPusher, custom webhook, "
                             "ServerChan… all at once; any one success counts as sent."},
                ],
            },
        },
        # ---- 示例：怎么接入新东西 ----
        "examples": {
            "zh": {
                "source": {
                    "title": "接入新的数据源与适配器",
                    "steps": [
                        "在 src/sources/ 下新建 <你的源>.py，继承 BaseSource，"
                        "实现 fetch() -> FetchResult。",
                        "fetch() 返回三种结果：hit（有头目）/ empty（查了没有）/ "
                        "error（没查成），三者要区分开。",
                        "在 config/sources.yaml 里加一段配置，填 name、adapter、"
                        "priority、options.target。",
                        "面板 → 头目源 → 新增源，或在页面上直接启用/改名/调优先级。",
                        "想抄现成的：src/sources/lanbizi.py 是中英双语的完整模板。",
                    ],
                    "code": "class MySource(BaseSource):\n"
                            "    name = \"mysource\"\n\n"
                            "    def fetch(self) -> FetchResult:\n"
                            "        data = self.request_json(self.build_url())\n"
                            "        boss = self.parse(data)\n"
                            "        return FetchResult(\"hit\", boss=boss,\n"
                            "                           dedup_key=..., message=\"命中\")",
                },
                "dispatcher": {
                    "title": "接入新的决策器 / 分发器",
                    "steps": [
                        "写一个 .py，实现 dispatch(boss, ctx) -> str，"
                        "返回一段可直接推送的文本。",
                        "boss 是归一化后的头目（名字/特性/技能/地点/时段/蛋组…），"
                        "ctx 里有 pokedex、rules、langs。",
                        "面板 → 决策器 → 上传决策器，上传后启用并设为当前。",
                        "抛异常不会炸：面板会回退到内置决策器并记一条 error 日志。",
                        "想抄现成的：panel/dispatchers/example_dispatcher.py 是带注释的示例。",
                    ],
                    "code": "def dispatch(boss, ctx: dict) -> str:\n"
                            "    rules = ctx[\"rules\"]; pokedex = ctx[\"pokedex\"]\n"
                            "    langs = ctx.get(\"langs\") or [\"zh\"]\n"
                            "    if \"zh\" in langs and \"en\" in langs:\n"
                            "        return generate_bilingual(boss, rules, pokedex, langs)\n"
                            "    return generate_report(boss, rules, pokedex, langs[0])",
                },
            },
            "en": {
                "source": {
                    "title": "Adding a new source & adapter",
                    "steps": [
                        "Create src/sources/<yours>.py, subclass BaseSource, "
                        "implement fetch() -> FetchResult.",
                        "fetch() returns three outcomes: hit (alpha found) / empty "
                        "(checked, nothing) / error (failed to check). Keep them distinct.",
                        "Add a block in config/sources.yaml with name, adapter, "
                        "priority and options.target.",
                        "Panel → Sources → Add, or enable / rename / reprioritize "
                        "right on the page.",
                        "Need a reference? src/sources/lanbizi.py is a full "
                        "bilingual template.",
                    ],
                    "code": "class MySource(BaseSource):\n"
                            "    name = \"mysource\"\n\n"
                            "    def fetch(self) -> FetchResult:\n"
                            "        data = self.request_json(self.build_url())\n"
                            "        boss = self.parse(data)\n"
                            "        return FetchResult(\"hit\", boss=boss,\n"
                            "                           dedup_key=..., message=\"hit\")",
                },
                "dispatcher": {
                    "title": "Adding a new decider / dispatcher",
                    "steps": [
                        "Write a .py implementing dispatch(boss, ctx) -> str, "
                        "returning text ready to push.",
                        "boss is the normalized alpha (name / ability / moves / "
                        "location / period / egg groups…); ctx carries pokedex, "
                        "rules and langs.",
                        "Panel → Deciders → Upload, then enable it and set as active.",
                        "Raising is safe: the panel falls back to the built-in decider "
                        "and logs an error.",
                        "Need a reference? panel/dispatchers/example_dispatcher.py "
                        "is a commented example.",
                    ],
                    "code": "def dispatch(boss, ctx: dict) -> str:\n"
                            "    rules = ctx[\"rules\"]; pokedex = ctx[\"pokedex\"]\n"
                            "    langs = ctx.get(\"langs\") or [\"zh\"]\n"
                            "    if \"zh\" in langs and \"en\" in langs:\n"
                            "        return generate_bilingual(boss, rules, pokedex, langs)\n"
                            "    return generate_report(boss, rules, pokedex, langs[0])",
                },
            },
        },
        "thanks": {
            "zh": [
                {"name": "蓝鼻子", "note": "国内首个公开头目 API 源"},
                {"name": "PokeAPI", "note": "公开免费的宝可梦数据 API"},
                {"name": "自愿报点的玩家", "note": "一切信息的源头"},
                {"name": "默认决策器提供者", "note": "我自己 🤗"},
            ],
            "en": [
                {"name": "Lanbizi", "note": "The first public alpha API source in China"},
                {"name": "PokeAPI", "note": "Free and open Pokémon data API"},
                {"name": "Players who report spawns", "note": "The origin of all information"},
                {"name": "Default decider provider", "note": "Myself 🤗"},
            ],
        },
        "git": "https://github.com/hanx11192-ship-it",
        "built_with": {
            "zh": "Python · Flask · SQLite · 原生 JS 单页应用",
            "en": "Python · Flask · SQLite · Vanilla JS SPA",
        },
        "credits": {
            "zh": "由 WorkBuddy 协助构建 · 面板风格参考 ZhuQue API Gateway",
            "en": "Built with WorkBuddy · Panel style inspired by ZhuQue API Gateway",
        },
    })


# ============================================================
# 启动
# ============================================================

def main():
    db.init_db()
    db.cleanup_logs(3)
    scheduler_mod.start_scheduler_thread()
    port = int(os.environ.get("PORT", 5000))
    app.run(host="0.0.0.0", port=port, debug=False)


if __name__ == "__main__":
    main()
