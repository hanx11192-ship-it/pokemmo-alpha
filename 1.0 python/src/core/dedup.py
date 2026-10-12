# -*- coding: utf-8 -*-
"""
全局去重（跨源，按「头目指纹 + 时段」）。

游戏机制（用户给定）：
    一天分 4 个时段，每时段必定刷一只固定存在 75 分钟的头目：
        早头 08:00–14:00
        午头 14:00–20:00
        晚头 20:00–次日 02:00
        凌晨头 02:00–08:00
    头目存活 75 分钟（< 一个时段的 6 小时）；同一只头目两次刷新至少间隔一个时段（6h）。

去重策略：
    去重键 = 头目内容指纹（图鉴号+特性+地点+技能）+ 时段标识（北京时间 6h 边界切成 4 段）。
    - 同一时段、同一只头目（同指纹）→ 键相同 → 只推一次
      （解决「存活 75 分钟内被多次轮询 → 重复推送」）。
    - 跨时段（下一时段、甚至相邻几分钟，如 13:58 早头末 vs 14:05 午头）→ 键不同 → 必重推
      （契合「每时段必报一只」）。
    - 不依赖任何源特定的时间字段名：lzpoke 的 windowStart / alphapedia 的 reportedAt /
      lanbizi 的 period 只要能解析成时刻、转北京时间，都能正确分到时段；源没给时间时回退
      用轮询当下（北京时间）也算时段。6h 粒度对各源时间字段的误差极宽容，依然通用。
    跨源一致：不同源报同一只头目，指纹相同、时段相同 → 键相同 → 只推一次。
"""

import hashlib
import json
import os
from datetime import datetime, timezone, timedelta
from typing import Optional

from .config import get_config
from .models import BossData

# 最多保留多少条已处理记录（防 state 无限增长；旧键随日期/时段自然失效）
KEEP = 500

# 北京时间（UTC+8）
CN_TZ = timezone(timedelta(hours=8))


def _load(path: str) -> list:
    if not os.path.exists(path):
        return []
    try:
        data = json.load(open(path, encoding="utf-8"))
    except (json.JSONDecodeError, OSError):
        return []
    # 兼容旧版 {source: [keys]} 或 list[dict] 格式：直接丢弃，从头开始
    if isinstance(data, dict):
        return []
    return [x for x in data if isinstance(x, str)]


def _save(path: str, data: list) -> None:
    os.makedirs(os.path.dirname(path), exist_ok=True)
    tmp = path + ".tmp"
    with open(tmp, "w", encoding="utf-8") as f:
        json.dump(data, f, ensure_ascii=False, indent=1)
    os.replace(tmp, path)  # 原子替换，避免写一半崩掉留下坏文件


def boss_fingerprint(boss: Optional[BossData]) -> str:
    """头目内容指纹：跨源一致。图鉴 id 优先于名称（更稳）。"""
    if boss is None:
        return ""
    # 仅以「图鉴号(或名称)」为指纹主体；时段由 slot_of 叠加。
    # 游戏机制保证每时段只有一只头目、固定存活 75 分钟且不跨时段，
    # 因此「图鉴号 + 时段」即可唯一锁定一只头目，无需纳入 特性/地点/技能
    # （这些字段跨源格式不一，纳入会导致同一只头目被多源当成不同指纹而重复推送）。
    key = boss.pokedex_id if boss.pokedex_id else (boss.name or "")
    if not key:
        return ""
    return "fp:" + hashlib.md5(str(key).encode("utf-8")).hexdigest()[:16]


def _parse_iso(s) -> Optional[datetime]:
    if not s:
        return None
    try:
        return datetime.fromisoformat(str(s).replace("Z", "+00:00"))
    except Exception:
        return None


def slot_of(boss: Optional[BossData]) -> str:
    """返回头目所属时段的稳定标识（北京时间 6h 边界）：形如 20260911-08 / -14 / -20 / -02。

    晚头 20:00–次日 02:00 是跨午夜的同一时段：
        当天 20:xx 与 次日 01:xx 都归到「起始日 20:00」，即同 key。
    """
    dt = _parse_iso(boss.reported_at) if (boss and boss.reported_at) else None
    if dt is None:
        dt = datetime.now(timezone.utc)  # 源没给时间时，用轮询当下时刻兜底
    dt = dt.astimezone(CN_TZ)
    h = dt.hour
    if 8 <= h < 14:
        base, sh = dt.date(), 8
    elif 14 <= h < 20:
        base, sh = dt.date(), 14
    elif 20 <= h < 24:
        base, sh = dt.date(), 20
    else:  # 0<=h<2 属「前一天 20:00 起的晚头」；2<=h<8 属「当天 02:00 起的晨头」
        if 0 <= h < 2:
            base, sh = (dt - timedelta(days=1)).date(), 20
        else:
            base, sh = dt.date(), 2
    return f"{base.strftime('%Y%m%d')}-{sh:02d}"


def global_dedup_key(boss: Optional[BossData]) -> str:
    """跨源去重键：头目指纹 + 时段标识（不含更细的时间粒度，避免裂桶）。"""
    return boss_fingerprint(boss) + "@" + slot_of(boss)


def already_processed_global(key: str) -> bool:
    if not key:
        return False
    cfg = get_config()
    return key in set(_load(cfg.state_path()))


def mark_processed_global(key: str) -> None:
    """标记已处理。**只应在推送成功之后调用**。"""
    if not key:
        return
    cfg = get_config()
    path = cfg.state_path()
    state = _load(path)
    if key not in state:
        state.append(key)
    if len(state) > KEEP:
        state = state[-KEEP:]
    _save(path, state)


def reset_global() -> None:
    """清空去重记录（调试用）。"""
    cfg = get_config()
    path = cfg.state_path()
    if os.path.exists(path):
        os.remove(path)
