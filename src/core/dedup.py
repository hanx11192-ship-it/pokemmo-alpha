# -*- coding: utf-8 -*-
"""
全局去重（跨源，按头目指纹）。

旧版按「源名」隔离（state[source] = [keys]），多源并发时同一只头目会被不同源各推一次。
现在改成全局按「头目内容指纹 + 报点时间分桶」去重：

- 同一只头目（id / 地点 / 特性 / 技能一致）在它存活的 75 分钟里，
  所有源报上来的指纹 + 桶都一样 → 只推一次；
- 头目过期后再次刷新（新的报点时间，落在另一个桶）→ 视为新事件，重新推；
- 不同源报同一只头目（哪怕报点时间差几分钟），分桶后会落到同一桶 → 不会双推。
"""

import hashlib
import json
import os
from datetime import datetime, timezone
from typing import Optional

from .config import get_config
from .models import BossData

# 最多保留多少条已处理记录
KEEP = 200

# 报点时间粗化粒度（分钟）：让同一刷新的多个源落在同一桶
BUCKET_MINUTES = 10


def _load(path: str) -> list:
    if not os.path.exists(path):
        return []
    try:
        data = json.load(open(path, encoding="utf-8"))
    except (json.JSONDecodeError, OSError):
        return []
    # 兼容旧版 {source: [keys]} 格式：直接丢弃，从头开始
    if isinstance(data, dict):
        return []
    return data


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
    key = boss.pokedex_id if boss.pokedex_id else (boss.name or "")
    parts = [
        str(key),
        boss.ability or "",
        boss.location or "",
        ",".join(sorted(boss.moves or [])),
    ]
    if not any(parts):
        return ""
    raw = "|".join(parts).encode("utf-8")
    return "fp:" + hashlib.md5(raw).hexdigest()[:16]


def _parse_iso(s) -> Optional[datetime]:
    if not s:
        return None
    try:
        return datetime.fromisoformat(str(s).replace("Z", "+00:00"))
    except Exception:
        return None


def spawn_bucket(reported_at: str) -> str:
    """把报点时间粗化到 BUCKET_MINUTES 分钟桶，让同一刷新的多个源落到同一桶。"""
    dt = _parse_iso(reported_at)
    if dt is None:
        return "na"
    dt = dt.astimezone(timezone.utc)
    bucketed = dt.replace(
        minute=(dt.minute // BUCKET_MINUTES) * BUCKET_MINUTES,
        second=0, microsecond=0,
    )
    return bucketed.strftime("%Y%m%d%H%M")


def global_dedup_key(boss: Optional[BossData]) -> str:
    """跨源去重键：指纹 + 报点时间分桶。"""
    return boss_fingerprint(boss) + "@" + spawn_bucket(boss.reported_at if boss else "")


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
