# -*- coding: utf-8 -*-
"""并发 + 源投票 + 跨源去重的回归测试。"""

import os
import sys
import tempfile

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))

from src.core.dedup import (
    boss_fingerprint, spawn_bucket, global_dedup_key,
    already_processed_global, mark_processed_global, reset_global,
)
from src.core.models import BossData, FetchResult, Gender
from src.main import resolve_by_vote


def boss(name, ability="自信过度", moves=None, loc="地点A", pid=373, reported=None):
    b = BossData(
        name=name, ability=ability, moves=moves or ["龙爪", "劈瓦", "羽栖", "铁壁"],
        location=loc, pokedex_id=pid,
        gender=Gender.from_male_percent(50.0),
    )
    b.reported_at = reported or ""
    return b


def scfg(name, priority=100):
    return {"name": name, "priority": priority, "adapter": "alphapedia", "options": {}}


def hit(sc, b):
    return sc, FetchResult("hit", boss=b, slot_name="午头")


def test_fingerprint_cross_source():
    """同一只头目，不同源（技能顺序/缺字段）应算出同一个指纹。"""
    b1 = boss("暴飞龙", "自信过度", ["龙爪", "劈瓦", "羽栖", "铁壁"])
    b2 = boss("暴飞龙", "自信过度", ["羽栖", "铁壁", "龙爪", "劈瓦"])  # 顺序不同
    assert boss_fingerprint(b1) == boss_fingerprint(b2), "技能顺序不应影响指纹"
    # 名称和技能都一致、但无 id 的两个源 → 都退化为按名称，也应一致
    # （period 不在指纹内，所以时段不同不影响）
    b3 = boss("暴飞龙", "自信过度", ["龙爪", "劈瓦", "羽栖", "铁壁"], pid=None)
    b4 = boss("暴飞龙", "自信过度", ["龙爪", "劈瓦", "羽栖", "铁壁"], pid=None)
    b4.period = "99:99~99:99"
    assert boss_fingerprint(b3) == boss_fingerprint(b4), "都无 id 时按名称应一致（且忽略时段）"
    print("[PASS] 跨源指纹一致（忽略技能顺序；同有无 id 时一致）")


def test_spawn_bucket():
    """同一刷新（10 分钟内）落到同一桶；换刷新（75 分钟后）落到不同桶。"""
    near = spawn_bucket("2026-09-10T12:03:00.000Z")
    near2 = spawn_bucket("2026-09-10T12:09:30.000Z")  # 仍在 12:00~12:09 桶
    late = spawn_bucket("2026-09-10T13:30:00.000Z")    # 新刷新
    assert near == near2, "同一次刷新应同桶"
    assert near != late, "不同刷新应不同桶"
    print("[PASS] 报点时间分桶：同刷新同桶，换刷新异桶")


def test_vote_unanimous():
    """两源一致 → 该头目胜出。"""
    s_a, s_b = scfg("alphapedia", 10), scfg("pokemmotools", 20)
    b = boss("暴飞龙")
    chosen_sc, chosen = resolve_by_vote([hit(s_a, b), hit(s_b, b)],
                                        {"alphapedia": 10, "pokemmotools": 20})
    assert chosen.boss.name == "暴飞龙"
    print("[PASS] 两源一致 → 暴飞龙胜出")


def test_vote_majority():
    """2:1 → 多数方胜出。"""
    s_a, s_b = scfg("alphapedia", 10), scfg("pokemmotools", 20)
    b_x = boss("暴飞龙", pid=373)
    b_y = boss("勾魂眼", pid=198)  # 不同图鉴 id，确保是指纹不同的另一只
    # alphapedia 报 X，pokemmotools 报 X 和 Y（X 两票）
    hits = [hit(s_a, b_x), hit(s_b, b_x), hit(s_b, b_y)]
    _, chosen = resolve_by_vote(hits, {"alphapedia": 10, "pokemmotools": 20})
    assert chosen.boss.name == "暴飞龙", "多数票应胜出"
    print("[PASS] 2:1 多数票 → 暴飞龙胜出")


def test_vote_tie_by_time():
    """1:1 平票 → 报点时间更新者胜。"""
    s_a, s_b = scfg("alphapedia", 10), scfg("pokemmotools", 20)
    old = boss("暴飞龙", pid=373, reported="2026-09-10T12:00:00.000Z")
    new = boss("勾魂眼", pid=198, reported="2026-09-10T12:05:00.000Z")
    _, chosen = resolve_by_vote([hit(s_a, old), hit(s_b, new)],
                                {"alphapedia": 10, "pokemmotools": 20})
    assert chosen.boss.name == "勾魂眼", "平票应按报点时间最新者"
    print("[PASS] 1:1 平票 → 报点时间更新者（勾魂眼）胜出")


def test_global_dedup():
    """跨源全局去重：同一只头目两源各报一次，只标记一次。"""
    # 重定向 state 文件到临时目录
    import src.core.dedup as d
    tmp = tempfile.mkdtemp(prefix="alpha_dedup_")
    orig = d.get_config
    try:
        class FakeCfg:
            def state_path(self):
                return os.path.join(tmp, "state.json")
        d.get_config = lambda: FakeCfg()

        reset_global()
        b = boss("暴飞龙", reported="2026-09-10T12:03:00.000Z")
        key = global_dedup_key(b)
        assert not already_processed_global(key)
        mark_processed_global(key)
        assert already_processed_global(key), "标记后应判定为已处理"

        # 另一个源报同一只（different source name，但指纹+桶一致）→ 应判已处理
        b2 = boss("暴飞龙", reported="2026-09-10T12:08:00.000Z")  # 同桶
        assert already_processed_global(global_dedup_key(b2)), "跨源应去重"
        print("[PASS] 跨源全局去重：两源报同一只只推一次")
    finally:
        d.get_config = orig


if __name__ == "__main__":
    test_fingerprint_cross_source()
    test_spawn_bucket()
    test_vote_unanimous()
    test_vote_majority()
    test_vote_tie_by_time()
    test_global_dedup()
    print("\n全部通过")
