# -*- coding: utf-8 -*-
"""
================================================================================
 致敬源 / Tribute Source (pokemmo.lanbizi.com)
================================================================================

【这是什么 / What is this】
接口已 410 下线，代码保留作为「源 + 适配器」的模板。
The API is retired (HTTP 410). This file is kept as the canonical
source + adapter template.

命名说明 / Naming：
    面板上显示的源名已改为「致敬源」，但适配器文件名/类名仍是 lanbizi，
    sources.yaml 里用 `adapter: lanbizi` 指向本文件。改名只是为了纪念，
    不动代码结构。
    The panel shows the source as 「致敬源」, but the adapter file/class stays
    `lanbizi`, referenced via `adapter: lanbizi` in sources.yaml. The rename is
    a tribute only; the code structure is untouched.

致敬 / In memory of：
    国内首个公开头目数据源。在还没有任何公开接口、也没有文档的年代，
    是它让「自动报点」这件事第一次跑通。后来的源都站在它肩上。
    The first public alpha-boss data source in China. Back when there was no
    public API and no documentation, it made automated alpha reporting possible
    for the very first time. Every source that came after stands on it.

--------------------------------------------------------------------------------
 本源的典型特征（写新适配器时常遇到的坑）
 Typical quirks of this source (the usual traps when writing a new adapter)
--------------------------------------------------------------------------------
1. 只给中文，没有英文字段 → 走「中文名 + 别名表」解析
   Chinese-only fields, no English → resolve via Chinese name + alias table
2. 只给 monster_id，不给名字 → 从 pokedex.json 反查中文名
   Provides only monster_id → look up the Chinese name in pokedex.json
3. 不给特性 → Pokemmo 头目一律隐藏特性，从图鉴 hidden_ability 补齐
   No ability given → alphas always have their Hidden Ability; fill it in
   from the dex's hidden_ability
4. monster_id 需要除以 100 才是全国图鉴号
   monster_id must be divided by 100 to get the National Dex number
   （每个源算法不同，除数写在配置里，别硬编码）
   (each source differs — put the divisor in config, never hardcode it)

--------------------------------------------------------------------------------
 抄这个模板的正确姿势 / How to copy this template
--------------------------------------------------------------------------------
1. 复制成 src/sources/<你的源>.py，改类名和 name
   Copy to src/sources/<yours>.py, rename the class and `name`
2. 实现 fetch() -> FetchResult，其余方法按需覆写
   Implement fetch() -> FetchResult; override the rest as needed
3. 在 config/sources.yaml 里照抄一段配置，填 adapter 名
   Add a block in config/sources.yaml pointing `adapter` at your file
4. 主流程不用改 —— 这就是插件式设计的目的
   No changes to the main flow — that's the whole point of the plugin design

另见 / See also：src/sources/base.py 里基类提供的能力（重试、超时、转发服务、
4xx 人话提示等），能不自己写就别自己写。
for what the base class already gives you (retries, timeouts, transform
proxy, human-readable 4xx hints). Don't reinvent them.
"""

import json
import re
from typing import List, Optional

from ..core.models import BossData, FetchResult, Gender
from .base import BaseSource


class LanbiziSource(BaseSource):
    """致敬源适配器 / Tribute Source adapter.

    保留原始实现作为参考。接口虽已下线，但里面处理「字段缺失」的套路
    （图鉴反查、隐藏特性兜底、报点人正则）依然是最常复用的部分。
    Kept as the original reference. The API is gone, but its handling of
    missing fields (dex lookup, hidden-ability fallback, reporter regex)
    is still the most reusable part.
    """

    # 适配器名：sources.yaml 的 adapter 字段要和这里对上
    # Adapter name: must match the `adapter` field in sources.yaml
    name = "lanbizi"

    # 请求头：源站多半会校验 UA / Referer，照抄浏览器最省事
    # Headers: most sites check UA / Referer; copying a browser is easiest
    HEADERS = {
        "User-Agent": ("Mozilla/5.0 (Linux; Android 15; PKG110 Build/UKQ1.231108.001) "
                       "AppleWebKit/537.36"),
        "Content-Type": "application/json",
        "Origin": "https://pokemmo.lanbizi.com",
        "Sec-Fetch-Site": "same-origin",
        "Sec-Fetch-Mode": "cors",
        "Sec-Fetch-Dest": "empty",
        "Referer": "https://pokemmo.lanbizi.com/",
        "Accept-Language": "zh-CN,zh;q=0.9",
    }

    def build_url(self) -> str:
        """拼最终请求地址 / Build the final request URL.

        走转发服务的情况基类已经处理了（query 传 url + method），
        这里一般直接返回配置里的 target 就行。
        The base class already handles the transform-proxy case (url + method
        in the query string), so just returning `target` is usually enough.
        """
        return self.options.get("target", "")

    def fetch(self) -> FetchResult:
        """拉数据并判定本轮结果 / Fetch data and decide this round's result.

        返回值三种 / Three possible outcomes:
            hit   —— 有有效头目 / a valid alpha was found
            empty —— 没头目（正常情况）/ no alpha (perfectly normal)
            error —— 请求或解析失败 / request or parsing failed

        注意区分 empty 和 error：empty 是「查了，没有」，
        error 是「没查成」。混在一起会让日志全是噪音。
        Keep `empty` and `error` distinct: empty means "checked, nothing
        there"; error means "failed to check". Mixing them floods the log.
        """
        url = self.build_url()
        if not url:
            return FetchResult("error", message="未配置 target")

        import time
        delays = self.config.http.get("retry_delays", [0, 0.5, 1])
        data = None
        last_err = None
        for i, d in enumerate(delays, 1):
            if d > 0:
                time.sleep(d)
            try:
                import requests

                http = self.config.http
                resp = requests.post(
                    url, data=json.dumps({}), headers=self.HEADERS,
                    timeout=http.get("timeout", 20),
                    verify=http.get("verify_ssl", False),
                )
                resp.raise_for_status()
                data = resp.json()
                break
            except Exception as e:
                last_err = e
                self.log(f"第 {i} 次请求失败: {e}", "warning")
        if data is None:
            return FetchResult("error", message=f"请求失败: {last_err}")

        if data.get("code") != 0:
            return FetchResult("empty", message=f"API 返回错误: {data.get('msg')}")

        boss_list = data.get("data") or []
        if not boss_list:
            return FetchResult("empty", message="当前时段没有头目")

        try:
            boss = self.parse(boss_list[0])
        except Exception as e:
            return FetchResult("error", message=f"解析失败: {e}")

        boss.source = self.name
        # 本源没有时段概念，用起止时间当去重标识；
        # 如果哪天连 period 都没了，基类会退化成按头目内容算指纹
        # This source has no time-slot concept, so use the start/end time as
        # the dedup key. If even `period` disappears one day, the base class
        # falls back to fingerprinting on boss content.
        return FetchResult("hit", boss=boss, dedup_key=f"{boss.period}",
                           message=f"命中：{boss.name}")

    def parse(self, info: dict) -> BossData:
        """把源给的原始字段翻成统一的 BossData。
        Translate the source's raw fields into a normalized BossData.

        这一层是适配器的核心：源再多，主流程只认 BossData。
        This is the heart of an adapter: however many sources there are,
        the main flow only ever sees BossData.
        """
        # 每个源算图鉴号的方式不一样，除数写在配置里
        # Every source computes the dex number differently — divisor goes in config
        divisor = int(self.options.get("monster_id_divisor", 1) or 1)
        raw = info.get("monster_id")
        pid = int(raw // divisor) if raw is not None and divisor else None

        # 只有图鉴号，没有名字 → 从图鉴补
        # Only a dex number, no name → fill it in from the dex
        entry = self.pokedex.pokemon.get(str(pid), {}) if pid is not None else {}
        name = entry.get("zh") or f"未知_{pid}"
        # 头目一律隐藏特性 / Alphas always carry their Hidden Ability
        ability = entry.get("hidden_ability") or "无特性"
        ability_id = entry.get("hidden_ability_id")
        male = entry.get("gender_rate")

        moves: List[str] = []
        move_ids: List[Optional[int]] = []
        for i in range(1, 5):
            mv = info.get(f"move{i}")
            if mv:
                cn, mid = self.resolve_entry(zh=mv, kind="move")
                moves.append(cn)
                move_ids.append(mid)

        start = info.get("start_time_str") or ""
        end = info.get("end_time_str") or ""
        period = f"{start}~{end}" if start and end else ""

        location = info.get("location_full_name") or ""
        desc = (info.get("description") or "").strip()
        if desc:
            location = f"{location} - {desc}"

        reporter = self._reporter(info.get("text") or "")

        return BossData(
            name=name,
            ability=ability,
            moves=moves,
            period=period,
            location=location,
            reporter=reporter,
            gender=Gender.from_male_percent(male),
            egg_groups=list(entry.get("egg_groups") or []),
            pokedex_id=pid,
            ability_id=ability_id,
            move_ids=move_ids,
        )

    @staticmethod
    def _reporter(text: str) -> str:
        """从自由文本里抠报点人 / Extract the reporter from free text.

        源站没给独立字段，报点人埋在一段描述文字里，只能正则。
        （正式做法是让源站给字段；这里是现实妥协。）
        The site gives no dedicated field, so the reporter hides inside a
        description blob and we regex it out. (The proper fix is asking the
        site for a field; this is the pragmatic compromise.)
        """
        m = re.search(r"报点人[:：](@?[\w]+)", text)
        return m.group(1).strip() if m else ""
