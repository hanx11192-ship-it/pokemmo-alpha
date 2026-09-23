# -*- coding: utf-8 -*-
"""
================================================================================
  LZPoke 报点源 / LZPoke Reports Source (tool.lzpoke.com)
================================================================================

接口：GET https://tool.lzpoke.com/api/reports?type=alpha
返回结构（与 Alphapedia 的 latest/slots 完全不同，是 reports 数组）：

{
  "reports": [
    {
      "type": "alpha",
      "windowStart": "2026-09-11T00:00:00.000Z",   # 报点窗口起（UTC）
      "windowEnd":   "2026-09-11T06:00:00.000Z",   # 报点窗口止（UTC）
      "monsterId": 342,                             # 全国图鉴号（直接，无需 /100）
      "level": 100,
      "moves": ["蟹钳锤","咬碎","近身战","龙之舞"],  # 中文技能名
      "nameEn": "Crawdaunt",                        # 英文精灵名（优先用它查图鉴）
      "location": "丰缘 · 弃船",                    # 中文地点
      "locationEn": "Abandoned Ship",
      "regionEn": "Hoenn", "region": "丰缘",
      "id": "ce99402e-...",                         # 报点 UUID
      "createdAt": "2026-09-11T04:50:53.351Z",     # 报点时间（UTC）
      "reporterName": "lmxm",
      "voteWeight": 1, "burstKey": "alpha|...|342|abandoned ship",
      "voteScore": 1, "voteCount": 1, "threshold": 5, "confirmed": false
    }
  ]
}

--------------------------------------------------------------------------------
 本源的特征（写适配器时踩过的点）
 Typical quirks (and how this adapter handles them)
--------------------------------------------------------------------------------
1. 只给英文精灵名 nameEn，不给中文名 → 优先用英文名查图鉴（英文名是标准名，绕开机翻）
   Chinese name absent; only nameEn given → resolve via English name (canonical).
2. 只给 monsterId（全国图鉴号，且不用除 100）→ 直接从图鉴取中文名/特性/蛋组
   Provides monsterId (National Dex no., no divisor) → look up dex for name/HA/eggs.
3. 不给特性 → Pokemmo 头目一律隐藏特性，从图鉴 hidden_ability 补齐
   No ability → alphas carry their Hidden Ability; fill from dex.
4. moves 是中文技能名 → 走中文名 + 图鉴解析（bm. 查不到就原样保留，不丢信息）
   moves are Chinese → resolve via zh names (unknown ones kept verbatim).
5. 有「报点投票」机制（voteScore / voteCount / threshold / confirmed）
   → 主流程只取「最佳一条」；API 则把全部报点都返回，由前端/调用方决定怎么用。
   Has a vote/confirm mechanism → fetch() picks the best; the boss API returns all.

--------------------------------------------------------------------------------
 联网：VPS 直连海外不通，走转发代理
 Networking: the VPS can't reach overseas directly, so use the transform proxy
--------------------------------------------------------------------------------
config/sources.yaml 里把 options.transform_url_env 指到 transfor_url 环境变量
（panel.env 已配好 https://trans.anexample.top/iptest.php + PROXY_KEY），
基类会自动把请求 POST 到转发服务（query 传 url + method=GET，并带 x-proxy-key）。
直连能通时（环境变量没配）基类会退回直连，两种都支持。
Set options.transform_url_env=transfor_url; the base class forwards automatically.
"""

from datetime import datetime, timezone, timedelta
from typing import List, Optional

from ..core.models import BossData, FetchResult, Gender, ExtraLine
from .base import BaseSource

CN_TZ = timezone(timedelta(hours=8))


def _slot_name_of(window_start_iso):
    """根据报点窗口起点(UTC)换算北京时间，返回 早/午/晚/凌晨头。"""
    if not window_start_iso:
        return ""
    try:
        dt = datetime.fromisoformat(str(window_start_iso).replace("Z", "+00:00"))
        dt = dt.astimezone(timezone(timedelta(hours=8)))
        h = dt.hour
    except Exception:
        return ""
    if 2 <= h < 8:
        return "凌晨头"
    if 8 <= h < 14:
        return "早头"
    if 14 <= h < 20:
        return "午头"
    return "晚头"


class LzpokeReportsSource(BaseSource):
    """LZPoke 报点适配器 / LZPoke reports adapter.

    适配器名必须和 sources.yaml 的 adapter 字段对上（lzpoke_reports）。
    The adapter name must match the `adapter` field in sources.yaml.
    """

    name = "lzpoke_reports"

    # 源站校验 UA / Referer（照抄浏览器与用户给的 curl）
    HEADERS = {
        "User-Agent": (
            "Mozilla/5.0 (Linux; Android 16; TB710FU Build/BP2A.250605.031.A3) "
            "AppleWebKit/537.36 (KHTML, like Gecko) Version/4.0 "
            "Chrome/131.0.6778.200 Safari/537.36"
        ),
        "Referer": "https://tool.lzpoke.com/encounter/alpha-spawn",
        "Accept-Language": "zh-CN,zh;q=0.9",
    }

    def build_headers(self) -> dict:
        return dict(self.HEADERS)

    # ---------------- 主流程：取「最佳一条」供投票/去重/推送 ----------------
    # 主流程期望每个源只返回 1 个头目。这里挑「已确认优先 → 票分最高 → 报点最新」
    # 的那条，作为本轮代表。完整列表由头目 API 提供。
    # The main flow expects one boss per source; we return the best single report.

    def fetch(self) -> FetchResult:
        url = self.build_url()
        if not url:
            return FetchResult("error", message="未配置 target")

        try:
            data = self.request_json(url, self.build_headers())
        except Exception as e:
            return FetchResult("error", message=f"请求失败: {e}")

        reports = data.get("reports") or [] if isinstance(data, dict) else []
        if not reports:
            return FetchResult("empty", message="当前没有头目报点")

        best = self._best(reports)
        try:
            boss = self.parse(best)
        except Exception as e:
            return FetchResult("error", message=f"解析失败: {e}")

        boss.source = self.name
        # 时段名（早/午/晚/晨头）：按 windowStart(UTC) 换算北京时间后判定
        slot_name = _slot_name_of(best.get("windowStart"))
        # 去重标识用 burstKey（按 窗口+图鉴号+地点 归组，稳定标识同一次刷怪）
        return FetchResult(
            "hit", boss=boss,
            dedup_key=best.get("burstKey") or best.get("id") or "",
            slot_name=slot_name,
            message=f"命中：{boss.name}",
        )

    # ---------------- 头目 API：返回全部报点（归一化 + 原始）----------------
    # 供面板「头目 API」调用：实时拉取整份报点，逐条归一化为 BossData。
    # Used by the boss API to surface every report, not just the best one.

    def fetch_reports(self) -> dict:
        """返回 {'ok', 'reports', 'message'}。
        每条 report: {'raw': 原始 dict, 'boss': BossData|None, 'parsed': bool,
                      'confirmed': bool}
        """
        try:
            data = self.request_json(self.build_url(), self.build_headers())
        except Exception as e:
            return {"ok": False, "reports": [], "message": f"请求失败: {e}"}

        reports = data.get("reports") or [] if isinstance(data, dict) else []
        out = []
        for r in reports:
            try:
                boss = self.parse(r)
            except Exception as e:
                boss = None
                self.log(f"解析报点失败: {e}", "warning")
            out.append({
                "raw": r,
                "boss": boss,
                "parsed": boss is not None,
                "confirmed": bool(r.get("confirmed")),
            })
        return {"ok": True, "reports": out,
                "message": f"共 {len(out)} 条报点"}

    # ---------------- 选最佳一条 ----------------

    @staticmethod
    def _best(reports: list) -> dict:
        def score(r: dict):
            confirmed = 1 if r.get("confirmed") else 0
            vote = float(r.get("voteScore") or 0)
            created = r.get("createdAt") or ""
            # 已确认 > 票分高 > 报点新
            return (confirmed, vote, created)
        return max(reports, key=score)

    # ---------------- 解析 ----------------

    def parse(self, report: dict) -> BossData:
        """把一条报点翻成统一的 BossData。

        只给 monsterId + nameEn + 中文 moves，特性/蛋组从图鉴补。
        """
        # monsterId 即全国图鉴号（不除 100）；也允许配置 divisor 兜底
        divisor = int(self.options.get("monster_id_divisor", 1) or 1)
        raw = report.get("monsterId")
        pid = int(raw) // divisor if raw is not None else None

        # 名称：英文名优先（标准名，绕开机翻），查不到回退中文/原名
        name, pid2 = self.resolve_entry(report.get("name"), report.get("nameEn"), "pokemon")
        if pid is None and pid2 is not None:
            pid = pid2

        entry = self.pokedex.pokemon.get(str(pid), {}) if pid is not None else {}
        # 特性：源不提供 → 头目一律隐藏特性，从图鉴补
        ability = entry.get("hidden_ability") or "无特性"
        ability_id = entry.get("hidden_ability_id")

        # 技能：中文名逐条解析（查不到的保留原样，不丢信息）
        moves_zh = report.get("moves") or []
        moves: List[str] = []
        move_ids: List[Optional[int]] = []
        for mz in moves_zh:
            cn, mid = self.resolve_entry(zh=str(mz), kind="move")
            moves.append(cn)
            move_ids.append(mid)

        # 性别：优先图鉴（覆盖全、格式稳），源没给性别字段
        male = self.pokedex.gender_of(pid) if pid is not None else None

        # 时段：窗口起 ~ 窗口止（北京时间 HH:MM）
        period = self._period(report.get("windowStart"), report.get("windowEnd"))

        # 源已把「地区 · 地点」拼进 location/locationEn，直接用，避免重复前缀
        location = report.get("location") or ""
        location_en = report.get("locationEn") or ""

        boss = BossData(
            name=name,
            ability=ability,
            moves=moves,
            period=period,
            location=location,
            location_en=location_en,
            reporter=report.get("reporterName") or "",
            gender=Gender.from_male_percent(male),
            egg_groups=list(entry.get("egg_groups") or []),
            pokedex_id=pid,
            ability_id=ability_id,
            move_ids=move_ids,
            reported_at=report.get("createdAt") or "",
            source=self.name,
        )
        # 避免同一只存活头目因报点时间落在不同 10 分钟桶而被误判为新事件、重复推送。
        boss.extra_lines = self.build_extra_lines(report)
        return boss

    # ---------------- 附加信息（等级 / 投票进度）----------------

    def build_extra_lines(self, report: dict) -> List[ExtraLine]:
        opts = self.options.get("extra_lines") or {}
        out: List[ExtraLine] = []

        if opts.get("level"):
            lvl = report.get("level")
            if lvl is not None:
                out.append(ExtraLine(zh=f"等级: {lvl}", en=f"Level: {lvl}"))

        if opts.get("vote"):
            vs = report.get("voteScore") or 0
            vc = report.get("voteCount") or 0
            th = report.get("threshold") or 0
            if bool(report.get("confirmed")):
                out.append(ExtraLine(zh="状态: 已确认", en="Status: Confirmed"))
            else:
                out.append(ExtraLine(
                    zh=f"确认进度: {vs}/{th}（{vc} 票）",
                    en=f"Confirmation: {vs}/{th} ({vc} votes)"))

        return out

    # ---------------- 时段（UTC → 北京时间）----------------

    @staticmethod
    def _to_hhmm(iso: str) -> str:
        if not iso:
            return ""
        try:
            dt = datetime.fromisoformat(str(iso).replace("Z", "+00:00"))
            return dt.astimezone(CN_TZ).strftime("%H:%M")
        except Exception:
            return ""

    def _period(self, a, b) -> str:
        s, e = self._to_hhmm(a), self._to_hhmm(b)
        return f"{s}~{e}" if s and e else ""
