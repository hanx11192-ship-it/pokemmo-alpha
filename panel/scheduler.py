# -*- coding: utf-8 -*-
"""
定时轮询引擎（后台线程）。

复用核心轮询逻辑（src.main._fetch_one / resolve_by_vote），
并叠加：
  - 启停状态（存 db kv：scheduler_enabled / scheduler_interval）
  - 「当前时段已报点则剩余时间不再报」：靠 src.core.dedup 的全局去重
    （同一头目存活期内只推一次）实现；同时把最近一次探测到的头目
    写入 current_slot，供面板展示「本时段已报点」。
  - 每次命中/推送写入事件日志（时段、点、源、适配器）。
  - debug 触发：run_debug_once() 立即跑一次且**只生成报告不推送**，供面板临时测试。
"""
import json
import logging
import os
import sys
import threading
import time
from concurrent.futures import ThreadPoolExecutor, wait, FIRST_COMPLETED

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
if ROOT not in sys.path:
    sys.path.insert(0, ROOT)

from src.core.config import get_config
from src.core.pokedex import get_pokedex
from src.core.models import FetchResult
from src.core.dedup import (
    already_processed_global, mark_processed_global, global_dedup_key,
)
from src.main import _fetch_one, resolve_by_vote
from src.strategy.rules import Rules

import panel.db as db
import panel.dispatch as dispatch_mod
import panel.config_mgr as cfgmgr
import panel.channels as channels_mod

logger = logging.getLogger("scheduler")

_thread = None
_lock = threading.Lock()


def _langs() -> list:
    lang = cfgmgr.get_push_language()
    if lang == "both":
        return ["zh", "en"]
    if lang == "en":
        return ["en"]
    return ["zh"]


def _build_report(boss, rules, pokedex, langs):
    """用当前激活的分发器（默认决策器）生成报告。"""
    conn = db.get_db()
    row = conn.execute(
        "SELECT filename FROM dispatchers WHERE active=1 LIMIT 1"
    ).fetchone()
    if not row:
        row = conn.execute(
            "SELECT filename FROM dispatchers WHERE is_builtin=1 LIMIT 1"
        ).fetchone()
    conn.close()
    ctx = {"pokedex": pokedex, "rules": rules, "langs": langs}
    if row:
        try:
            return dispatch_mod.run_dispatcher(row["filename"], boss, ctx), row["filename"]
        except Exception as e:
            logger.warning("分发器执行失败，回退内置引擎: %s", e)
    from src.strategy.engine import generate_report
    return generate_report(boss, rules, pokedex, langs[0]), "builtin"


def poll_once(debug: bool = False) -> dict:
    """执行一轮轮询。debug=True 时只生成报告不推送。返回结果 dict。"""
    cfg = get_config()
    pokedex = get_pokedex()
    rules = Rules(cfg.rules, pokedex)
    sources = cfg.enabled_sources()
    logger.info("轮询开始，启用源 %d 个", len(sources))

    conc = cfg.settings.get("concurrency") or {}
    timeout = float(conc.get("timeout", 20))
    max_workers = int(conc.get("max_workers", max(len(sources), 1)))

    t0 = time.monotonic()
    results = []
    with ThreadPoolExecutor(max_workers=max_workers) as ex:
        futs = {
            ex.submit(_fetch_one, scfg, pokedex, logger, None): scfg
            for scfg in sources
        }
        deadline = time.monotonic() + timeout
        pending = set(futs)
        while pending and time.monotonic() < deadline:
            done, pending = wait(pending, timeout=max(0.0, deadline - time.monotonic()),
                                 return_when=FIRST_COMPLETED)
            for f in done:
                results.append(f.result())
        for f in pending:
            scfg = futs[f]
            logger.warning("数据源 %s 超时，本轮跳过", scfg["name"])

    elapsed = round(time.monotonic() - t0, 2)
    hits = []
    query_rows = []
    for scfg, res in results:
        logger.info("%s -> %s (%s)", scfg["name"], res.status, res.message)
        query_rows.append((scfg, res))
        if res.status == "error":
            continue
        if res.is_hit:
            hits.append((scfg, res))

    # 每个源的查询结果落库，供面板「日志」页按源查看
    _log_queries(query_rows, elapsed, timed_out=[
        futs[f]["name"] for f in pending
    ] if sources else [])

    db.set_kv("last_run", time.strftime("%Y-%m-%d %H:%M:%S"))

    if not hits:
        db.set_kv("last_result", json.dumps({
            "status": "empty", "time": time.strftime("%Y-%m-%d %H:%M:%S"),
            "elapsed": elapsed,
        }, ensure_ascii=False))
        return {"status": "empty", "message": "所有数据源都没有有效头目",
                "elapsed": elapsed}

    priority_map = {s["name"]: s.get("priority", 100) for s in sources}
    chosen_scfg, chosen = resolve_by_vote(hits, priority_map)
    boss = chosen.boss
    langs = _langs()
    report, disp = _build_report(boss, rules, pokedex, langs)
    summary = chosen.slot_name_en if langs[0] == "en" else (chosen.slot_name or "头目")

    slot_info = {
        "slot": chosen.slot_name or "",
        "slot_en": chosen.slot_name_en or "",
        "boss": boss.name,
        "source": chosen_scfg["name"],
        "dispatcher": disp,
        "time": time.strftime("%Y-%m-%d %H:%M:%S"),
        "reported": False,
    }

    if debug:
        # debug：只生成报告，不推送、不写去重标记
        db.log_event("debug", "scheduler",
                     f"[DEBUG] 探测到 {boss.name}({chosen.slot_name}) 源={chosen_scfg['name']} 分发器={disp}",
                     source="scheduler")
        return {
            "status": "hit", "debug": True,
            "report": report,
            "resolved": {
                "name": boss.name, "ability": boss.ability,
                "moves": boss.moves, "slot": chosen.slot_name,
                "source": chosen_scfg["name"], "dispatcher": disp,
            },
        }

    # 实际推送：先去重
    dedup_key = global_dedup_key(boss)
    if dedup_key and already_processed_global(dedup_key):
        logger.info("已推送过（%s），跳过", dedup_key)
        slot_info["reported"] = True
        db.set_kv("current_slot", json.dumps(slot_info, ensure_ascii=False))
        db.log_event("info", "scheduler",
                     f"本时段已报点，跳过重复推送：{boss.name}({chosen.slot_name}) 源={chosen_scfg['name']}",
                     source="scheduler")
        db.set_kv("last_result", json.dumps({
            "status": "deduped", "boss": boss.name, "slot": chosen.slot_name,
            "source": chosen_scfg["name"], "time": slot_info["time"],
        }, ensure_ascii=False))
        return {"status": "deduped", "message": "本时段已报点，已跳过", "slot": slot_info}

    try:
        send_res = channels_mod.send_all(report, summary)
    except Exception as e:
        logger.error("推送失败，本轮不写去重标记，下轮重试: %s", e)
        db.log_event("error", "scheduler", f"推送失败: {e}", source="scheduler")
        return {"status": "notify_error", "message": f"推送失败: {e}"}

    mark_processed_global(dedup_key)
    slot_info["reported"] = True
    db.set_kv("current_slot", json.dumps(slot_info, ensure_ascii=False))
    excerpt = report[:200].replace("\n", " ")
    db.log_event("spawn", "scheduler",
                 f"{slot_info['slot']} 爆点：{boss.name} · 源={chosen_scfg['name']} · 分发器={disp} · {excerpt}",
                 source=chosen_scfg["name"])
    db.set_kv("last_result", json.dumps({
        "status": "pushed", "boss": boss.name, "slot": chosen.slot_name,
        "source": chosen_scfg["name"], "dispatcher": disp, "time": slot_info["time"],
        "channels": {"sent": send_res.get("sent", 0), "failed": send_res.get("failed", 0)},
    }, ensure_ascii=False))
    return {"status": "pushed", "message": "已推送", "report": report,
            "slot": slot_info, "channels": send_res}


def _log_queries(rows, elapsed, timed_out=None):
    """把每个源的查询结果写入日志（kind=query），供面板按源排查。

    受 kv 开关 query_log 控制（默认开启）：每轮每源一条，
    开着能完整追溯「哪一轮哪个源返回了什么」，关掉则只留命中/报错。
    """
    verbose = db.get_kv("query_log", "1") == "1"
    timed_out = timed_out or []
    for scfg, res in rows:
        name = scfg.get("name", "?")
        st = getattr(res, "status", "?")
        if not verbose and st in ("empty",):
            continue          # 简洁模式下不记「没头目」这种噪音
        msg = getattr(res, "message", "") or ""
        boss = getattr(getattr(res, "boss", None), "name", None)
        line = f"源[{name}] 返回 {st}"
        if boss:
            line += f" · 头目={boss}"
        if msg:
            line += f" · {msg}"
        lvl = "error" if st == "error" else ("info" if st in ("hit", "empty") else "warning")
        db.log_event(lvl, "query", line, source=name)
    for name in timed_out:
        db.log_event("error", "query",
                     f"源[{name}] 本轮超时（整轮耗时 {elapsed}s）", source=name)


def run_debug_once() -> dict:
    try:
        return poll_once(debug=True)
    except Exception as e:
        logger.exception("debug 轮询异常")
        return {"status": "error", "message": f"debug 轮询异常: {e}"}


# ---- 异步 debug：面板点一下立即返回，后台跑完再回来看结果 ----
_debug_lock = threading.Lock()
_debug_state = {"running": False, "started_at": 0, "result": None}


def start_debug_async() -> dict:
    """后台起一次 debug 轮询，立刻返回，不阻塞页面。"""
    global _debug_state
    with _debug_lock:
        if _debug_state["running"]:
            return {"status": "running", "elapsed": round(time.time() - _debug_state["started_at"], 1)}
        _debug_state = {"running": True, "started_at": time.time(), "result": None}

    def _job():
        global _debug_state
        try:
            res = run_debug_once()
        except Exception as e:  # 兜底，绝不让线程静默死掉
            res = {"status": "error", "message": str(e)}
        with _debug_lock:
            _debug_state["running"] = False
            _debug_state["result"] = res

    threading.Thread(target=_job, daemon=True, name="debug-once").start()
    return {"status": "running", "elapsed": 0}


def get_debug_state() -> dict:
    with _debug_lock:
        st = dict(_debug_state)
    st["elapsed"] = round(time.time() - st.get("started_at", time.time()), 1) if st["running"] else 0
    return st


def _loop():
    """固定节奏轮询：以上一次开始时间为基准对齐，不叠加轮询耗时。

    之前是「跑完再 sleep(interval)」，源卡 50 秒 + 睡 60 秒 = 实际 110 秒一轮，
    会整点跳过。现在按开始时间对齐，间隔就是 interval。
    """
    while True:
        try:
            enabled = db.get_kv("scheduler_enabled", "0") == "1"
            interval = int(db.get_kv("scheduler_interval", "60") or 60)
            interval = max(5, interval)
            if not enabled:
                db.set_kv("scheduler_status", "stopped")
                time.sleep(2)
                continue

            cycle_start = time.monotonic()
            try:
                poll_once(debug=False)
                db.set_kv("scheduler_status", "running")
            except Exception as e:
                logger.exception("轮询异常: %s", e)
                db.log_event("error", "scheduler", f"轮询异常: {e}", source="scheduler")

            # 对齐到固定节奏：下一次开始 = cycle_start + interval
            spent = time.monotonic() - cycle_start
            wait_s = interval - spent
            if wait_s > 0:
                time.sleep(wait_s)
            else:
                logger.warning("本轮耗时 %.1fs 超过间隔 %ds，立即进入下一轮", spent, interval)
        except Exception:
            logger.exception("调度循环异常")
            time.sleep(5)


def start_scheduler_thread():
    global _thread
    with _lock:
        if _thread and _thread.is_alive():
            return
        _thread = threading.Thread(target=_loop, daemon=True)
        _thread.start()
    logger.info("定时轮询线程已启动")


def get_scheduler_state() -> dict:
    enabled = db.get_kv("scheduler_enabled", "0") == "1"
    interval = int(db.get_kv("scheduler_interval", "60") or 60)
    status = db.get_kv("scheduler_status", "stopped")
    last_run = db.get_kv("last_run", "")
    last_result = db.get_kv("last_result", "")
    current_slot = db.get_kv("current_slot", "")
    try:
        last_result = json.loads(last_result) if last_result else {}
    except Exception:
        last_result = {}
    try:
        current_slot = json.loads(current_slot) if current_slot else {}
    except Exception:
        current_slot = {}
    # 判断当前时段是否已报点：current_slot.reported 为 True
    return {
        "enabled": enabled,
        "interval": interval,
        "status": status,
        "last_run": last_run,
        "last_result": last_result,
        "current_slot": current_slot,
        "reported_this_slot": bool(current_slot.get("reported")),
    }


def set_scheduler(enabled: bool, interval: int = None):
    db.set_kv("scheduler_enabled", "1" if enabled else "0")
    if interval is not None:
        db.set_kv("scheduler_interval", str(int(interval)))
