# -*- coding: utf-8 -*-
"""
回归测试：新引擎的输出必须和原 autodoalpha.py 逐字一致。

原脚本已稳定运行两个月，重构不能改变任何判定结果。
基线来自原 autodoalpha.py __main__ 里的测试用例，以及手工构造的几类头目。
"""

import os
import subprocess
import sys

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
sys.path.insert(0, ROOT)

from src.core.models import BossData, Gender          # noqa: E402
from src.core.pokedex import get_pokedex              # noqa: E402
from src.core.config import get_config                # noqa: E402
from src.strategy.rules import Rules                  # noqa: E402
from src.strategy.engine import generate_report       # noqa: E402

POKEDEX = get_pokedex()
CFG = get_config()
RULES = Rules(CFG.rules, POKEDEX)


def make_boss(name, ability, moves, gender=50.0, period="15:58~17:13",
              location="<丰缘>天空之柱<5层>", egg_groups=None, reporter=""):
    """按真实流程构造 BossData：先把中文名解析成 id。"""
    pid = POKEDEX.resolve_pokemon_id(name)
    aid = POKEDEX.resolve_ability_id(ability)
    mids = [POKEDEX.resolve_move_id(m) for m in moves]
    return BossData(
        name=name, ability=ability, moves=moves,
        period=period, location=location, reporter=reporter,
        gender=Gender.from_male_percent(gender),
        egg_groups=egg_groups or [],
        pokedex_id=pid, ability_id=aid, move_ids=mids,
    )


def render(boss, lang="zh"):
    return generate_report(boss, RULES, POKEDEX, lang)


# ---------------- 基线：原脚本的实际输出 ----------------

# 注意：性别百分比沿用原脚本的字面输出（50.0 而非 50），不做格式化，保证逐字一致
BASE_SALAMENCE = """15:58-17:13
暴飞龙(自信过度)-(龙组)-50.0%公
<丰缘>天空之柱<5层>
技能: 龙爪, 劈瓦, 羽栖, 铁壁
打法推荐：
沙奈朵：临别礼物
长耳兔：掉包, 治愈之愿
呆壳兽：哈欠, 中转
图图犬：回复封锁, 搏命
呆壳兽：哈欠, 中转"""


def run_original(name, ability, moves, gender, period, location, egg_groups):
    """调用原始 autodoalpha.py 生成基线输出（保证对比的是真原版，不是我重写的）。"""
    orig = os.path.join(ROOT, "..", "pokemmo_auto_alpha", "autodoalpha.py")
    if not os.path.exists(orig):
        return None
    code = f'''
import sys, json
sys.path.insert(0, r"{os.path.dirname(orig)}")
from autodoalpha import generate_strategy
r = generate_strategy({{
    "period": {period!r}, "official_name": {name!r}, "ability": {ability!r},
    "moves": {moves!r}, "location": {location!r},
    "gender_rate": {gender!r}, "egg_groups": {egg_groups!r},
}})
print(r["文本报告"], end="")
'''
    out = subprocess.run([sys.executable, "-c", code], capture_output=True, text=True)
    return out.stdout.strip() if out.returncode == 0 else None


CASES = [
    # (说明, boss, 期望基线)
    (
        "原脚本自带用例：暴飞龙（带回复技 → 插中转手）",
        make_boss("暴飞龙", "自信过度", ["龙爪", "劈瓦", "羽栖", "铁壁"], 50.0,
                  egg_groups=["龙组"]),
        BASE_SALAMENCE,
    ),
]

# 用户确认的行为变更：长耳兔的技能位不再按空列表计算
# （旧版读 team_skills["沙奈朵"]，恶作剧之心队伍里没有沙奈朵 → 算多一格 → 漏掉拍手）
EXPECTED_CHANGES = {
    "勾魂眼（恶作剧之心 + 需识破）",
}


def main():
    print("=" * 60)
    print("回归测试：新引擎 vs 原 autodoalpha.py")
    print("=" * 60)

    ok = True

    # 1) 和原脚本基线逐字比对
    for desc, boss, expected in CASES:
        got = render(boss)
        same = got == expected
        ok &= same
        print(f"\n[{'PASS' if same else 'FAIL'}] {desc}")
        if not same:
            print("--- 期望 ---")
            print(expected)
            print("--- 实际 ---")
            print(got)

    # 2) 动态用例：直接调用原脚本生成基线，逐个比对
    dynamic = [
        ("双性别 + 强化技 + 电磁波（应出挑衅）", "暴飞龙", "自信过度",
         ["龙之舞", "电磁波", "龙爪", "劈瓦"], 50.0, "15:58~17:13", "地点A", ["龙组"]),
        ("单性别（纯公，不出打法）", "暴飞龙", "自信过度",
         ["龙爪", "劈瓦", "羽栖", "铁壁"], 100.0, "15:58~17:13", "地点A", ["龙组"]),
        ("无性别（不出打法）", "自爆磁怪", "分析",
         ["电磁炮", "加农光炮", "三重攻击", "锁定"], None, "17:44~18:59", "关都 无人发电厂", ["无性别"]),
        ("恶作剧之心特性（换队伍）", "暴飞龙", "恶作剧之心",
         ["龙爪", "劈瓦", "羽栖", "铁壁"], 50.0, "15:58~17:13", "地点A", ["龙组"]),
        ("勾魂眼（恶作剧之心 + 需识破）", "勾魂眼", "恶作剧之心",
         ["电磁波", "劈瓦", "羽栖", "铁壁"], 50.0, "15:58~17:13", "地点A", ["人形"]),
        ("带先制技（插中转手）", "暴飞龙", "自信过度",
         ["音速拳", "劈瓦", "岩崩", "地震"], 50.0, "15:58~17:13", "地点A", ["龙组"]),
        ("冰冻+麻痹技能（出神秘守护）", "暴飞龙", "自信过度",
         ["冰冻光束", "十万伏特", "岩崩", "地震"], 50.0, "15:58~17:13", "地点A", ["龙组"]),
        ("迟钝特性（出特性互换）", "暴飞龙", "迟钝",
         ["岩崩", "地震", "劈瓦", "龙爪"], 50.0, "15:58~17:13", "地点A", ["龙组"]),
        ("白名单：艾路雷朵（单性别也出打法）", "艾路雷朵", "正义之心",
         ["岩崩", "地震", "劈瓦", "剑舞"], 100.0, "15:58~17:13", "地点A", ["人形"]),
    ]

    print("\n" + "-" * 60)
    print("动态比对（基线由原脚本当场生成）")
    print("-" * 60)

    for desc, name, ability, moves, gender, period, loc, egg in dynamic:
        boss = make_boss(name, ability, moves, gender, period, loc, egg)
        mine = render(boss)
        base = run_original(name, ability, moves, gender, period, loc, egg)
        if base is None:
            print(f"\n[SKIP] {desc}（找不到原脚本）")
            continue
        same = mine == base
        if desc in EXPECTED_CHANGES:
            ok &= True
            print(f"\n[{'PASS(一致)' if same else 'CHANGED'}] {desc}  ← 用户确认的变更")
        else:
            ok &= same
            print(f"\n[{'PASS' if same else 'FAIL'}] {desc}")
        if not same:
            print("--- 原脚本 ---")
            print(base)
            print("--- 新引擎 ---")
            print(mine)

    # 3) 索罗亚克专项：挑衅逻辑与沙奈朵共用，但不带神秘守护 / 特性互换
    print("\n" + "-" * 60)
    print("索罗亚克专项")
    print("-" * 60)
    zoro_checks = [
        # (说明, 头目名, 特性, 技能, 不该出现的技能)
        ("冰冻技 → 仍不出神秘守护", "勾魂眼", "恶作剧之心",
         ["冰冻光束", "电磁波", "劈瓦", "铁壁"], ["神秘守护"]),
        ("麻痹技 → 仍不出神秘守护", "黑暗鸦", "恶作剧之心",
         ["十万伏特", "电磁波", "劈瓦", "铁壁"], ["神秘守护"]),
        ("需换特性时 → 仍不出特性互换", "勾魂眼", "恶作剧之心",
         ["电磁波", "劈瓦", "岩崩", "铁壁"], ["特性互换"]),
    ]
    for desc, name, ability, moves, forbidden in zoro_checks:
        out = render(make_boss(name, ability, moves, 50.0, location="地点A"))
        zoro_line = next((l for l in out.splitlines() if l.startswith("索罗亚克")), "")
        bad = [f for f in forbidden if f in zoro_line]
        okx = zoro_line.startswith("索罗亚克") and not bad
        ok &= okx
        print(f"[{'PASS' if okx else 'FAIL'}] {desc}  → {zoro_line.split('：')[-1]}")

    # 4) 长耳兔技能顺序：主攻手 3 招 → 拍手,掉包；2 招 → 掉包,拍手
    print("\n" + "-" * 60)
    print("长耳兔配招顺序")
    print("-" * 60)
    order_checks = [
        ("沙奈朵 3 招（挑衅+神秘守护+临别礼物）", "暴飞龙", "自信过度",
         ["冰冻光束", "电磁波", "劈瓦", "铁壁"], "拍手, 掉包"),
        ("沙奈朵 2 招（挑衅+临别礼物）", "暴飞龙", "自信过度",
         ["电磁波", "劈瓦", "岩崩", "铁壁"], "掉包, 拍手"),
        ("索罗亚克 2 招（挑衅+临别礼物）", "勾魂眼", "恶作剧之心",
         ["电磁波", "劈瓦", "岩崩", "铁壁"], "掉包, 拍手"),
    ]
    for desc, name, ability, moves, expect in order_checks:
        out = render(make_boss(name, ability, moves, 50.0, location="地点A"))
        lop = next((l for l in out.splitlines() if l.startswith("长耳兔")), "")
        okx = expect in lop
        ok &= okx
        print(f"[{'PASS' if okx else 'FAIL'}] {desc}  → 期望含「{expect}」→ {lop.split('：')[-1]}")

    # 5) 规则覆盖率自检
    print("\n" + "-" * 60)
    print("规则自检")
    print("-" * 60)
    un = RULES.report_unresolved()
    if un:
        print(f"【警告】{len(un)} 个名字解析不到技能 id：")
        for u in un:
            print("   ", u)
    else:
        print("所有规则名字都能解析到技能 id")

    print("\n" + "=" * 60)
    print("全部通过" if ok else "存在差异，见上面输出")
    print("=" * 60)
    return 0 if ok else 1


if __name__ == "__main__":
    sys.exit(main())
