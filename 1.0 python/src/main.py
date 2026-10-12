# -*- coding: utf-8 -*-
"""
主流程。

设计要点（相对旧版 alpha.py）：
- 单进程函数调用，不再用 subprocess 靠退出码通信
- 时区统一按北京时间算，不依赖 VPS 本地时区
- 多数据源「并发」请求（而非按优先级顺序），谁先回来都算数，
  既保证及时性（不用等慢源），又保证可靠性（一个源挂了还有别的）
- 多源结论按「头目指纹」计票（源投票），得票最多者胜出；
  平票再按报点时间最新、优先级最高兜底
- 跨源全局去重：同一只头目在存活期内被多个源重复报，只推一次
- 只有推送成功才写去重标记，推送失败下轮重试
"""

import argparse
import concurrent.futures as cf
import json
import logging
import os
import sys
import time
from datetime import datetime

from .core.config import get_config, abspath
from .core.dedup import (
    already_processed_global,
    mark_processed_global,
    global_dedup_key,
    boss_fingerprint,
)
from .core.models import FetchResult
from .core.notify import NotifyError, send as notify_send
from .core.pokedex import get_pokedex
from .sources import create_source
from .strategy.engine import generate_bilingual, generate_report
from .strategy.rules import Rules

logger = logging.getLogger("alpha")


def setup_logging(cfg, verbose=False):
    log_cfg = cfg.settings.get("logging") or {}
    level = logging.DEBUG if verbose else getattr(
        logging, str(log_cfg.get("level", "INFO")).upper(), logging.INFO
    )
    logger.setLevel(level)
    fmt = logging.Formatter("%(asctime)s %(levelname)-7s %(message)s", "%m-%d %H:%M:%S")

    sh = logging.StreamHandler(sys.stdout)
    sh.setFormatter(fmt)
    logger.addHandler(sh)

    fpath = log_cfg.get("file")
    if fpath:
        full = abspath(fpath)
        os.makedirs(os.path.dirname(full), exist_ok=True)
        from logging.handlers import RotatingFileHandler

        fh = RotatingFileHandler(
            full,
            maxBytes=int(log_cfg.get("max_bytes", 2097152)),
            backupCount=int(log_cfg.get("backup_count", 3)),
            encoding="utf-8",
        )
        fh.setFormatter(fmt)
        logger.addHandler(fh)


def build_report(boss, rules, pokedex, langs) -> str:
    if len(langs) > 1:
        return generate_bilingual(boss, rules, pokedex, langs)
    return generate_report(boss, rules, pokedex, langs[0])


def _fetch_one(scfg, pokedex, logger, sample):
    """在独立线程里跑单个数据源，异常被吞掉并返回 error 结果，避免拖垮其它源。"""
    try:
        src = create_source(
            scfg["adapter"],
            options=scfg.get("options") or {},
            pokedex=pokedex,
            logger=logger,
            sample_data=sample,
        )
        return scfg, src.fetch()
    except Exception as e:
        return scfg, FetchResult("error", message=f"加载/执行异常: {e}")


def resolve_by_vote(hits, priority_map):
    """源投票裁决：按头目指纹计票，得票最多者胜；平票再用报点时间、优先级兜底。

    hits: list of (scfg, FetchResult)
    返回 (scfg, FetchResult)，即最终用于播报的那一条。
    """
    groups = {}
    for scfg, r in hits:
        fp = boss_fingerprint(r.boss)
        groups.setdefault(fp, []).append((scfg, r))

    if len(groups) == 1:
        fp = next(iter(groups))
    else:
        max_votes = max(len(v) for v in groups.values())
        winners = [fp for fp, v in groups.items() if len(v) == max_votes]
        if len(winners) == 1:
            fp = winners[0]
        else:
            # 平票：报点时间最新者优先，其次优先级数字最小者优先
            best = None
            for fp in winners:
                rep_dt = max(
                    (_parse_iso(r.boss.reported_at) or datetime.min)
                    for _, r in groups[fp]
                )
                prio = min(priority_map.get(s["name"], 999) for s, _ in groups[fp])
                # 时间越新、优先级数字越小 → 排序键越大
                key = (rep_dt, -prio)
                if best is None or key > best[0]:
                    best = (key, fp)
            fp = best[1]

    group = groups[fp]
    # 同指纹的一组里，挑优先级最高（数字最小）的那个源的完整结果来播报
    group.sort(key=lambda sr: priority_map.get(sr[0]["name"], 999))
    return group[0]


def _parse_iso(s):
    if not s:
        return None
    try:
        return datetime.fromisoformat(str(s).replace("Z", "+00:00"))
    except Exception:
        return None


def run(args) -> int:
    cfg = get_config()
    setup_logging(cfg, args.verbose)

    pokedex = get_pokedex()
    rules = Rules(cfg.rules, pokedex)

    unresolved = rules.report_unresolved()
    if unresolved:
        logger.warning("规则里有 %d 个名字解析不到技能 id（将走字符串兜底，建议补进 aliases.json）：",
                       len(unresolved))
        for u in unresolved[:20]:
            logger.warning("  - %s", u)

    if args.lang:
        langs = ["zh", "en"] if args.lang == "both" else [args.lang]
    else:
        langs = cfg.languages
    logger.info("输出语言: %s", "+".join(langs))

    sources = cfg.enabled_sources()
    if args.source:
        sources = [s for s in sources if s["name"] == args.source]
        if not sources:
            logger.error("没有找到名为 %s 的已启用数据源", args.source)
            return 2

    sample = None
    if args.sample:
        with open(args.sample, encoding="utf-8") as f:
            sample = json.load(f)
        logger.info("使用样本文件: %s", args.sample)

    # ---------------- 并发拉取所有源 ----------------
    conc = cfg.settings.get("concurrency") or {}
    timeout = float(conc.get("timeout", 20))
    max_workers = int(conc.get("max_workers", max(len(sources), 1)))

    results = []  # (scfg, FetchResult)
    with cf.ThreadPoolExecutor(max_workers=max_workers) as ex:
        futs = {
            ex.submit(_fetch_one, scfg, pokedex, logger, sample): scfg
            for scfg in sources
        }
        deadline = time.monotonic() + timeout
        pending = set(futs)
        while pending and time.monotonic() < deadline:
            done, pending = cf.wait(
                pending,
                timeout=max(0.0, deadline - time.monotonic()),
                return_when=cf.FIRST_COMPLETED,
            )
            for f in done:
                results.append(f.result())
        for f in pending:
            scfg = futs[f]
            logger.warning("数据源 %s 在 %.0fs 内未返回，本轮跳过", scfg["name"], timeout)

    # ---------------- 汇总命中 ----------------
    hits = []
    for scfg, res in results:
        logger.info("%s -> %s (%s)", scfg["name"], res.status, res.message)
        if res.status == "error":
            continue
        if res.is_hit:
            hits.append((scfg, res))

    if not hits:
        logger.info("所有数据源都没拿到有效头目")
        return 0

    # ---------------- 源投票裁决 ----------------
    priority_map = {s["name"]: s.get("priority", 100) for s in sources}
    chosen_scfg, chosen = resolve_by_vote(hits, priority_map)
    logger.info("投票结果：%s 胜出（共 %d 个源命中）", chosen.boss.name, len(hits))

    # ---------------- 跨源全局去重 ----------------
    dedup_key = global_dedup_key(chosen.boss)
    if dedup_key and not args.force and already_processed_global(dedup_key):
        logger.info("已推送过（%s），跳过", dedup_key)
        return 0

    report = build_report(chosen.boss, rules, pokedex, langs)
    # 摘要由胜出源提供（没有时段概念的源会留空，走兜底）
    summary = chosen.slot_name_en if langs[0] == "en" else (chosen.slot_name or "头目")

    print("\n" + "=" * 40)
    print(report)
    print("=" * 40 + "\n")

    if args.dry_run:
        logger.info("dry-run 模式：不推送、不写去重标记")
        return 0

    try:
        notify_send(report, summary)
    except NotifyError as e:
        logger.error("推送失败，本轮不写去重标记，下轮重试: %s", e)
        return 1

    mark_processed_global(dedup_key)
    logger.info("推送成功，已标记 %s", dedup_key or "（该源未设置去重标识）")
    return 0


def main():
    ap = argparse.ArgumentParser(description="Pokemmo 头目监控与打法推送")
    ap.add_argument("--dry-run", action="store_true", help="只生成报告，不推送不写标记")
    ap.add_argument("--lang", choices=["zh", "en", "both"], help="覆盖配置里的输出语言")
    ap.add_argument("--source", help="只用指定的数据源")
    ap.add_argument("--sample", help="用本地 JSON 样本代替网络请求（离线测试）")
    ap.add_argument("--force", action="store_true", help="忽略去重，强制生成")
    ap.add_argument("--verbose", "-v", action="store_true", help="调试日志")
    args = ap.parse_args()

    try:
        sys.exit(run(args))
    except KeyboardInterrupt:
        sys.exit(130)


if __name__ == "__main__":
    main()
