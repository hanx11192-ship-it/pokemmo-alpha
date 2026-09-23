# -*- coding: utf-8 -*-
"""
================================================================================
  你的爬虫源 / Mycrawler Source
================================================================================

场景：你已写好一个爬虫，能拿到
  - 头目必要信息（名称 / 特性 / 4 技能）
  - 一些其他信息（票数 votes、剩余时间 remaining_seconds）

本适配器负责把爬虫输出翻成统一的 BossData，主流程只认 BossData。

落位约定：
  - 名称/特性/技能     -> BossData 必要字段 + canonical id（给引擎做打法决策）
  - 地区/地点          -> BossData.location（中）、location_en（英）
  - 票数 / 剩余时间    -> BossData.extra_lines（可选追加消息，纯展示）
  - 时段窗口           -> BossData.period（正文首行）+ FetchResult.slot_name（摘要）
================================================================================
"""

import re
from datetime import datetime, timezone, timedelta
from typing import List, Optional

from ..core.models import BossData, FetchResult, Gender, ExtraLine
from .base import BaseSource


# 北京时间（UTC+8）—— 用来算时段窗口
CN_TZ = timezone(timedelta(hours=8))

# 时段定义（按你项目里约定的命名）
SLOTS = [
    ("凌晨头", "次日02:00-08:00", lambda h: 2 <= h < 8),
    ("早头",   "08:00-14:00",     lambda h: 8 <= h < 14),
    ("午头",   "14:00-20:00",     lambda h: 14 <= h < 20),
    ("晚头",   "20:00-次日02:00", lambda h: 20 <= h or h < 2),
]


class MycrawlerSource(BaseSource):
    """爬虫源适配器：把爬虫输出翻成 BossData。"""

    # 必须和 sources.yaml 里的 adapter 字段对上
    name = "mycrawler"

    HEADERS = {
        "User-Agent": "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 "
                      "(KHTML, like Gecko) Chrome/124.0 Safari/537.36",
    }

    # ---------------- 时段 ----------------
    @staticmethod
    def _slot_of(hour: int):
        for name, rng, pred in SLOTS:
            if pred(hour):
                return name, rng
        return "晚头", "20:00-次日02:00"

    # ---------------- 主流程 ----------------
    def fetch(self) -> FetchResult:
        # 离线样本注入（不改网络逻辑就能测）：把 sample_data 配成爬虫 dict 即可
        if self.sample_data is not None:
            self.log("使用注入的样本数据（不发网络请求）")
            raw = self.sample_data
        else:
            try:
                raw = self._crawl()          # 你的爬虫逻辑，返回下面的 dict 结构
            except Exception as e:
                return FetchResult("error", message=f"爬虫失败: {e}")

        if not raw:
            # 查了，但当前没头目 —— 这是 empty，不是 error
            return FetchResult("empty", message="爬虫未返回头目")

        try:
            boss = self._to_bossdata(raw)
        except Exception as e:
            return FetchResult("error", message=f"解析失败: {e}")

        boss.source = self.name
        slot_name = boss.period.split("(")[0]   # 取 "凌晨头"，不含时段范围
        self.log(f"命中 {boss.name} / {boss.location} / 票数={raw.get('votes')}", "info")
        return FetchResult(
            "hit", boss=boss,
            slot_name=slot_name,
            message=f"命中: {boss.name}",
        )

    # ---------------- 你的爬虫逻辑（替换这里） ----------------
    def _crawl(self) -> Optional[dict]:
        """返回结构示例：
        {
            "pokemon": "Luxray",                 # 英文名优先（便于查 canonical id）
            "ability": "Guts",
            "moves": ["Thunderbolt","Superpower","Facade","Signal Beam"],
            "region": "Sinnoh", "location": "Route 205",
            "votes": 12,                         # 额外信息
            "remaining_seconds": 171,            # 额外信息
            "reporter": "@someone",
            "gender_rate": 50.0,                 # 雄性百分比，可省
        }
        返回 None 表示当前无头目。
        """
        # 例：直接复用基类带重试/超时的请求（海外站记得在 yaml 配 proxy）
        # url = self.options.get("target")
        # resp = self.http_get(url, self.HEADERS)
        # return resp.json()
        raise NotImplementedError("在这里接你的爬虫")

    # ---------------- 翻成 BossData ----------------
    def _to_bossdata(self, raw: dict) -> BossData:
        # 1) 名称 + 图鉴号（英文优先查 canonical id，绕开机翻）
        cn_name, pid = self.resolve_entry(en=raw.get("pokemon"), kind="pokemon")
        entry = self.pokedex.pokemon.get(str(pid), {}) if pid is not None else {}

        # 2) 特性（源没给时，头目一律隐藏特性，从图鉴补）
        if raw.get("ability"):
            ability, ability_id = self.resolve_entry(en=raw["ability"], kind="ability")
        else:
            ability = entry.get("hidden_ability") or "无特性"
            ability_id = entry.get("hidden_ability_id")

        # 3) 4 技能 -> 中文名 + id
        moves: List[str] = []
        move_ids: List[Optional[int]] = []
        for mv in (raw.get("moves") or [])[:4]:
            cn, mid = self.resolve_entry(en=mv, kind="move")
            moves.append(cn)
            move_ids.append(mid)

        # 4) 地点（地区译中、详细地点保留英文）
        region = raw.get("region")
        location = raw.get("location")
        REGION_ZH = {  # 按你项目里的映射表补
            "Kanto": "关都", "Johto": "城都", "Hoenn": "丰缘", "Sinnoh": "神奥",
            "Unova": "合众", "Kalos": "卡洛斯", "Alola": "阿罗拉", "Galar": "伽勒尔",
            "Hisui": "洗翠", "Paldea": "帕底亚",
        }
        loc_zh = " · ".join([x for x in (REGION_ZH.get(region, region), location) if x])
        loc_en = " · ".join([x for x in (region, location) if x])

        # 5) 时段窗口（本源按北京时间算，与 pokemmotools 源一致）
        slot_name, slot_rng = self._slot_of(datetime.now(CN_TZ).hour)
        period = f"{slot_name}({slot_rng})"

        # 6) 额外信息 -> 附加行（票数 / 剩余时间，纯展示，不参与打法决策）
        extra: List[ExtraLine] = []
        if raw.get("votes") is not None:
            extra.append(ExtraLine(zh=f"票数: {raw['votes']}", en=f"Votes: {raw['votes']}"))
        if raw.get("remaining_seconds") is not None:
            rem = int(raw["remaining_seconds"])
            dur = f"{rem // 60} 分钟" if rem >= 60 else f"{rem} 秒"
            extra.append(ExtraLine(zh=f"剩余消失: 约 {dur}", en=f"Despawns in ~{dur}"))

        # 7) 性别 / 蛋组（可省）
        gender = Gender.from_male_percent(raw.get("gender_rate"))
        egg = list(entry.get("egg_groups") or [])

        return BossData(
            name=cn_name or raw.get("pokemon") or "未知",
            ability=ability,
            moves=moves,
            period=period,
            location=loc_zh,
            location_en=loc_en,
            reporter=raw.get("reporter") or "",
            gender=gender,
            egg_groups=egg,
            extra_lines=extra,
            pokedex_id=pid,
            ability_id=ability_id,
            move_ids=move_ids,
            source=self.name,
        )
