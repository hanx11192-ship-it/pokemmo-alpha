# -*- coding: utf-8 -*-
"""
================================================================================
  PokemmoTools Landing 源 / PokemmoTools Landing Source
  站点：alpha.pokemmotools.org
================================================================================

数据来源（两步，均公开，无需登录 Cookie）：

  1) GET /alpha-list                  -> 当前活跃头目名 / 地区 / 地点 / 剩余秒数
     - 公开页面（VPS 走隧道、无 Cookie 直接 HTTP 200）。
     - 有活跃头目时会渲染一块：
         <span data-i18n="Alpha currently active">Alpha currently active</span>:
         <a href="/alpha-list?pokemon=Luxray&region=Sinnoh&location=Route+205&...">
         ... data-i18n="Remains active for approximately ..." data-timedelta="171">
      即：当前活跃头目名 + 地区/地点（链接 query 里）+ 剩余秒数（data-timedelta）。
     - 用来「探活 + 定位（地区/地点）+ 剩余消失时间」，补 lzpoke 的盲区
       （lzpoke 只在有人 call 时才有 reports；本源主动感知当前谁在）。

  2) GET /pokedex/{图鉴号}             -> 该头目在各出现点的 4 携带技能 + 特性
     - 同样公开（无 Cookie 直接 200）。
     - 页面 #pokedex-alpha 面板下，每个 <article class="pokedex-spawn-entry">
       对应一个出现地点，内含若干 <div class="pokedex-spawn-moves">：
         · label="HMs Required"  -> 秘传技（如 Surf）
         · label="Moves"         -> 该地点阿尔法头目的 4 个携带技能
       （不同地点技能不同，例如 Luxray 在 Lake Verity 是 Thunderbolt/Ice Fang/
        Superpower/Charge Beam；在 Route 205 是 Thunderbolt/Superpower/Facade/
        Signal Beam）。
     - 特性：页面里的 pokedex-ability-link，其中带 pokedex-hidden-ability-icon
       图标的即隐藏特性（Luxray=Guts）。
     - 用第 1 步拿到的「地区/地点」匹配到正确的 spawn-entry，抽取该地点的 4 技能。

推送消息约定：
  - 地点：地区翻译为中文（神奥/关都…），详细地点保留英文（Route 205 等）。
    zh 用 boss.location（神奥 · Route 205），en 用 boss.location_en（Sinnoh · Route 205）。
  - 时段：由本源决定，命名为用户约定
        凌晨头(次日02:00-08:00) / 早头(08:00-14:00) /
        午头(14:00-20:00) / 晚头(20:00-次日02:00)，
        写入 boss.period 作为消息首行。

联网：VPS 直连海外不通，走本地隧道代理（sources.yaml 配 proxy: http://127.0.0.1:8899）。
"""

import re
import urllib.parse
from html import unescape
from datetime import datetime, timezone, timedelta
from typing import List, Optional, Tuple

from ..core.models import BossData, FetchResult, Gender, ExtraLine
from .base import BaseSource


# 北京时间（UTC+8）
CN_TZ = timezone(timedelta(hours=8))

BASE_SITE = "https://alpha.pokemmotools.org"

# 地区英文名 -> 中文名（详细地点如 Route 205 保留英文）
REGION_ZH = {
    "Kanto": "关都", "Johto": "城都", "Hoenn": "丰缘", "Sinnoh": "神奥",
    "Unova": "合众", "Kalos": "卡洛斯", "Alola": "阿罗拉", "Galar": "伽勒尔",
    "Hisui": "洗翠", "Paldea": "帕底亚",
}

# 时段定义（用户约定）。凌晨头 = 原「晨头」。
SLOTS: List[Tuple[str, str, "callable"]] = [
    ("凌晨头", "次日02:00-08:00", lambda h: 2 <= h < 8),
    ("早头",   "08:00-14:00",     lambda h: 8 <= h < 14),
    ("午头",   "14:00-20:00",     lambda h: 14 <= h < 20),
    ("晚头",   "20:00-次日02:00", lambda h: 20 <= h or h < 2),
]


class PokemmotoolsLandingSource(BaseSource):
    """PokemmoTools 适配器：alpha-list 探活 + pokedex 补技能/特性（均公开）。"""

    name = "pokemmotools_landing"

    HEADERS = {
        "Accept": "text/html,application/xhtml+xml",
        "User-Agent": (
            "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 "
            "(KHTML, like Gecko) Chrome/124.0 Safari/537.36"
        ),
        "Referer": "https://alpha.pokemmotools.org/",
    }

    def build_headers(self) -> dict:
        return dict(self.HEADERS)

    # ---------------- 时段 ----------------

    @staticmethod
    def _slot_of(hour: int) -> Tuple[str, str]:
        for name, rng, pred in SLOTS:
            if pred(hour):
                return name, rng
        return "晚头", "20:00-次日02:00"

    # ---------------- 主流程 ----------------

    def fetch(self) -> FetchResult:
        url = self.build_url()
        if not url:
            return FetchResult("error", message="未配置 target")

        # 离线样本注入（回归测试用，不发网络请求）
        if self.sample_data is not None:
            self.log("使用注入的样本数据（不发网络请求）")
            return self._handle_data(self.sample_data)

        try:
            resp = self.http_get(url, self.build_headers())
        except Exception as e:
            return FetchResult("error", message=f"请求失败: {e}")

        if resp.status_code >= 400:
            return FetchResult("error", message=f"HTTP {resp.status_code}")
        html = getattr(resp, "text", "") or ""
        return self._handle_data(html)

    def _handle_data(self, html: str) -> FetchResult:
        info = self.parse(html)
        if info is None:
            return FetchResult("empty", message="当前无活跃头目(alpha-list 未发现 currently active)")
        boss = info["boss"]

        # 第 2 步：去 pokedex 页拿 4 技能 + 特性（按地点匹配）
        pk = self._fetch_pokedex(boss.pokedex_id, info.get("region"), info.get("location"))
        if pk:
            if pk.get("moves"):
                boss.moves = pk["moves"]
            if pk.get("hidden_ability"):
                boss.ability = pk["hidden_ability"]
            if pk.get("hms"):
                boss.extra_lines.append(ExtraLine(
                    zh=f"秘传技需求: {', '.join(pk['hms'])}",
                    en=f"HMs required: {', '.join(pk['hms'])}",
                ))

        boss.source = self.name
        self.log(
            f"[alpha-list] 命中 {boss.name} / {boss.location} / 技能={boss.moves} / 剩余≈{info['remaining']}s",
            "info",
        )
        return FetchResult(
            "hit", boss=boss,
            slot_name=info["slot_name"],
            message=f"命中: {boss.name}",
        )

    # ---------------- alpha-list 解析（探活 + 定位） ----------------

    def parse(self, html: str):
        """从公开 alpha-list 页面抽取「当前活跃头目」。无则返回 None。"""
        if "Alpha currently active" not in html:
            return None

        # 1) 当前活跃头目的链接（含 pokemon/region/location query）
        m = re.search(r'Alpha currently active</span>\s*:\s*<a[^>]*href="([^"]+)"', html)
        if not m:
            return None
        href = m.group(1)
        # 原始 HTML 中属性值里的 & 是实体 &amp;，需先解码再做 URL 解析
        href = urllib.parse.unquote(unescape(href))
        q = urllib.parse.parse_qs(urllib.parse.urlparse(href).query)
        name_en = (q.get("pokemon") or [None])[0]
        region = (q.get("region") or [None])[0]
        location = (q.get("location") or [None])[0]
        if not name_en:
            return None

        # 2) 剩余秒数（Remains active for approximately ... data-timedelta="N"）
        remaining = None
        rm = re.search(r'Remains active for approximately[^>]*data-timedelta="(\d+)"', html)
        if rm:
            try:
                remaining = int(rm.group(1))
            except ValueError:
                remaining = None

        # 3) 图鉴号 / 中文名
        pid = None
        cn_name, _id = self.resolve_entry(en=name_en, kind="pokemon")
        if _id is not None:
            pid = _id
        entry = self.pokedex.pokemon.get(str(pid), {}) if pid is not None else {}

        # 4) 时段（本源决定）
        slot_name, slot_rng = self._slot_of(datetime.now(CN_TZ).hour)

        # 5) 地点：地区译中、详细地点保留英文
        cn_region = REGION_ZH.get(region, region) if region else None
        loc_zh = " · ".join([x for x in (cn_region, location) if x])
        loc_en = " · ".join([x for x in (region, location) if x])

        extra: List[ExtraLine] = []
        if remaining is not None:
            mins = remaining // 60
            dur = f"{mins} 分钟" if mins >= 1 else f"{remaining} 秒"
            extra.append(ExtraLine(
                zh=f"剩余消失: 约 {dur}",
                en=f"Despawns in ~{dur}",
            ))
        extra.append(ExtraLine(
            zh=f"数据来源: PokemmoTools 当前活跃头目(公开页) + 图鉴技能 · {slot_name}",
            en=f"Source: PokemmoTools currently-active (public) + pokedex moves · {slot_name}",
        ))

        boss = BossData(
            name=cn_name or name_en,
            ability=entry.get("hidden_ability") or "无特性",
            moves=[],  # 稍后由 pokedex 补全
            location=loc_zh,
            location_en=loc_en,
            reporter=None,
            gender=Gender.from_male_percent(
                self.pokedex.gender_of(pid) if pid is not None else None
            ),
            egg_groups=list(entry.get("egg_groups") or []),
            pokedex_id=pid,
            ability_id=entry.get("hidden_ability_id"),
            move_ids=[],
            extra_lines=extra,
            source=self.name,
            period=f"{slot_name}({slot_rng})",
        )
        return {
            "boss": boss,
            "remaining": remaining,
            "region": region,
            "location": location,
            "slot_name": slot_name,
        }

    # ---------------- pokedex 抓取 + 解析（技能/特性） ----------------

    def _fetch_pokedex(self, pid, region, location):
        """抓 /pokedex/{pid}，按 (region, location) 匹配 spawn-entry，抽 4 技能 + 隐藏特性。
        任何失败都返回空 dict（不阻断主流程）。"""
        if not pid:
            return {}
        url = f"{BASE_SITE}/pokedex/{pid}"
        try:
            resp = self.http_get(url, self.build_headers())
        except Exception as e:
            self.log(f"[pokedex] 请求失败: {e}", "warn")
            return {}
        if resp.status_code >= 400:
            self.log(f"[pokedex] HTTP {resp.status_code}", "warn")
            return {}
        html = getattr(resp, "text", "") or ""
        return self._parse_pokedex(html, region, location)

    @staticmethod
    def _parse_pokedex(html: str, region: Optional[str], location: Optional[str]) -> dict:
        out = {"moves": [], "abilities": [], "hidden_ability": None, "hms": []}

        # 1) 特性：pokedex-ability-link，带 pokedex-hidden-ability-icon 的是隐藏特性
        for m in re.finditer(r'pokedex-ability-link"[^>]*>(.*?)</a>', html, re.S):
            blk = m.group(1)
            nm = re.search(r'data-item_value="([^"]+)"', blk)
            if not nm:
                continue
            name = nm.group(1)
            out["abilities"].append(name)
            if "pokedex-hidden-ability-icon" in blk:
                out["hidden_ability"] = name

        # 2) alpha 面板下的 spawn-entry（按地点匹配）
        alpha = re.search(r'id="pokedex-alpha".*?(?=<div id="pokedex-[a-z]|</div>\s*</div>\s*</div>\s*</div>)', html, re.S)
        panel = alpha.group(0) if alpha else html
        entries = re.findall(r'<article class="pokedex-spawn-entry">(.*?)</article>', panel, re.S)
        target = None
        for e in entries:
            reg = re.search(r'data-i18n="__\(item_value::text::region\)__"\s*data-item_value="([^"]+)"', e)
            loc = re.search(r'data-i18n="__\(item_value::text::locationPokeapi\)__"\s*data-item_value="([^"]+)"', e)
            reg_v = (reg.group(1) if reg else "").lower()
            loc_v = (loc.group(1) if loc else "").lower()
            loc_ok = bool(location) and (location.lower() in loc_v)
            reg_ok = (not region) or (not reg_v) or (region.lower() in reg_v)
            if loc_ok and reg_ok:
                target = e
                break
        if target is None and entries:
            target = entries[0]
        if target:
            for blk in re.finditer(r'pokedex-spawn-moves">(.*?)</div>', target, re.S):
                b = blk.group(1)
                lbl = re.search(r'data-i18n="([^"]+)"', b)
                label = lbl.group(1) if lbl else ""
                vals = re.findall(r'data-item_value="([^"]+)"', b)
                if "HMs" in label:
                    out["hms"] = vals
                elif "Moves" in label:
                    out["moves"] = vals[:4]
        return out
