# -*- coding: utf-8 -*-
"""
Alphapedia 数据源（tool.lzpoke.com）。

这个源同时提供中英文字段（nameEn / abilityEn / movesEn），
英文是标准名，所以优先拿英文查图鉴 —— 机翻中文的问题直接绕过。

它独有的字段（秘传机 hms、地点备注 locationNotes、星级 tier）
由本适配器自己决定要不要放进推送，主流程只负责插到固定位置。
"""

import os
from datetime import datetime, timezone, timedelta
from typing import List, Optional

from ..core.models import BossData, FetchResult, Gender, ExtraLine
from .base import BaseSource, parse_male_ratio

CN_TZ = timezone(timedelta(hours=8))

# 本源时段的英文写法（中文由 API 直接给）
SLOT_NAME_EN = {
    "凌晨头": "Dawn Alpha",
    "晨头": "Dawn Alpha",
    "早头": "Morning Alpha",
    "午头": "Noon Alpha",
    "晚头": "Night Alpha",
}


class AlphapediaSource(BaseSource):
    name = "alphapedia"

    HEADERS = {
        "User-Agent": ("Mozilla/5.0 (Linux; Android 15; PKG110 Build/UKQ1.231108.001) "
                       "AppleWebKit/537.36"),
        "sec-fetch-site": "same-origin",
        "sec-fetch-mode": "cors",
        "sec-fetch-dest": "empty",
        "referer": "https://tool.lzpoke.com/encounter/alpha-spawn",
        "accept-language": "zh-CN,zh;q=0.9,en-US;q=0.8,en;q=0.7",
        "Accept-Encoding": "identity",
    }

    def build_headers(self) -> dict:
        """源站新接口要 Bearer 鉴权，key 从环境变量读（不进配置文件）。"""
        h = dict(self.HEADERS)
        env_name = self.options.get("api_key_env") or "ALPHAPEDIA_API_KEY"
        key = (os.environ.get(env_name) or "").strip()
        if key:
            h["Authorization"] = f"Bearer {key}"
        return h

    # ---------------- 主流程 ----------------

    def fetch(self) -> FetchResult:
        url = self.build_url()
        if not url:
            return FetchResult("error", message="未配置 target")
        if not self.options.get("target"):
            return FetchResult("error", message="未配置 target")

        try:
            data = self.request_json(url, self.build_headers())
        except Exception as e:
            return FetchResult("error", message=f"请求失败: {e}")

        latest = data.get("latest") or {}
        slots = data.get("slots") or []

        # 判定的唯一依据是「头目还活着」。
        # 不能只看 hasSpawn/isCurrent：时段之间有空档（比如午头 18:45 结束、
        # 晚头 20:00 才开始），空档期 API 不给 isCurrent，
        # 但 18:40 刷出来的头目能活到 19:55，这时候照样该推。
        if not latest.get("isActive"):
            return FetchResult(
                "empty",
                message="最新头目已过期（头目只存在 75 分钟）",
            )

        # 时段：优先当前 slot，空档期则按报点时间反查所属时段
        reported = latest.get("reportedAt")
        slot = next((s for s in slots if s.get("isCurrent")), None) \
            or self._slot_for_time(slots, reported)
        slot_key = reported or (slot or {}).get("startIso") or ""
        slot_name = (slot or {}).get("name") or ""

        try:
            boss = self.parse(latest)
        except Exception as e:
            return FetchResult("error", dedup_key=slot_key, slot_name=slot_name,
                               message=f"解析失败: {e}")

        boss.source = self.name
        # 去重标识用报点时间：同一条报点只推一次，
        # 换时段、换头目自然就是新的 key
        return FetchResult("hit", boss=boss, dedup_key=slot_key, slot_name=slot_name,
                           slot_name_en=self.slot_name_en(slot_name),
                           message=f"{slot_name} 命中：{boss.name}")

    def slot_name_en(self, cn: str) -> str:
        return SLOT_NAME_EN.get(cn, cn or "Alpha")

    @staticmethod
    def _slot_for_time(slots, iso_str):
        """按报点时间找到它属于哪个时段（时段定义直接从 API 读，不硬编码）。"""
        if not iso_str:
            return None
        try:
            t = datetime.fromisoformat(str(iso_str).replace("Z", "+00:00"))
        except Exception:
            return None
        best = None
        for s in slots:
            try:
                a = datetime.fromisoformat(str(s["startIso"]).replace("Z", "+00:00"))
                b = datetime.fromisoformat(str(s["endIso"]).replace("Z", "+00:00"))
            except Exception:
                continue
            if a <= t <= b:
                return s
            # 窗口末尾刷出来的头目，报点时间会落在空档里，
            # 这种归到「结束时间早于报点时间」的最近一个时段
            if b <= t and (best is None or b > best[0]):
                best = (b, s)
        return best[1] if best else None

    # ---------------- 解析 ----------------

    def parse(self, latest: dict) -> BossData:
        catalog = (latest.get("catalog") or [{}])[0]

        # 名称：英文名优先
        name, pid = self.resolve_entry(latest.get("name"), latest.get("nameEn"), "pokemon")
        ability, aid = self.resolve_entry(catalog.get("ability"), catalog.get("abilityEn"), "ability")

        # 技能：逐个用英文名解析，中英文数组按位置对齐
        moves_zh = catalog.get("moves") or []
        moves_en = catalog.get("movesEn") or []
        moves: List[str] = []
        move_ids: List[Optional[int]] = []
        for i, mz in enumerate(moves_zh):
            me = moves_en[i] if i < len(moves_en) else None
            cn, mid = self.resolve_entry(mz, me, "move")
            moves.append(cn)
            move_ids.append(mid)

        # 性别：优先用图鉴（覆盖全、格式稳定），API 的 maleRatio 作兜底
        male = self.pokedex.gender_of(pid) if pid is not None else None
        if male is None:
            male = parse_male_ratio(catalog.get("maleRatio"))

        # 时段：报点时间 ~ 失效时间（北京时间）
        period = self._period(latest.get("reportedAt"), latest.get("activeUntil"))

        region = latest.get("region") or ""
        place = latest.get("location") or ""
        region_en = latest.get("regionEn") or ""
        place_en = latest.get("locationEn") or ""

        boss = BossData(
            name=name,
            ability=ability,
            moves=moves,
            period=period,
            location=f"{region} {place}".strip(),
            location_en=f"{region_en} {place_en}".strip(),
            reporter=latest.get("reporter") or "",
            gender=Gender.from_male_percent(male),
            egg_groups=list(catalog.get("eggGroups") or []),
            egg_groups_en=list(catalog.get("eggGroupsEn") or []),
            pokedex_id=pid,
            ability_id=aid,
            move_ids=move_ids,
            reported_at=latest.get("reportedAt") or "",
        )

        boss.extra_lines = self.build_extra_lines(catalog, latest)
        return boss

    def _period(self, reported_at, active_until) -> str:
        s = self._to_hhmm(reported_at)
        e = self._to_hhmm(active_until)
        return f"{s}~{e}" if s and e else ""

    @staticmethod
    def _to_hhmm(iso: str) -> str:
        if not iso:
            return ""
        try:
            dt = datetime.fromisoformat(str(iso).replace("Z", "+00:00"))
            return dt.astimezone(CN_TZ).strftime("%H:%M")
        except Exception:
            return ""

    # ---------------- 附加信息 ----------------
    # 由本源自己决定给什么：秘传机、地点备注、星级、备注
    # 主流程会把它们插在「头目信息」和「打法推荐」之间

    def build_extra_lines(self, catalog: dict, latest: dict) -> List[ExtraLine]:
        opts = self.options.get("extra_lines") or {}
        out: List[ExtraLine] = []

        if opts.get("hms"):
            line = self.make_extra("需要秘传机", "HM required", catalog.get("hms") or [])
            if line:
                out.append(line)

        if opts.get("location_notes"):
            note = (catalog.get("locationNotes") or "").strip()
            if note:
                out.append(ExtraLine(zh=f"地点备注: {note}", en=f"Location note: {note}"))

        if opts.get("notes"):
            notes = catalog.get("notes") or []
            if notes:
                out.append(ExtraLine(
                    zh=f"备注: {', '.join(notes)}",
                    en=f"Notes: {', '.join(notes)}",
                ))

        if opts.get("tier"):
            tier = catalog.get("tier")
            if tier is not None:
                out.append(ExtraLine(zh=f"星级: {tier}", en=f"Tier: {tier}"))

        return out
