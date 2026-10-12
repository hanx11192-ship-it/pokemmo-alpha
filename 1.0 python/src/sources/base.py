# -*- coding: utf-8 -*-
"""
数据源适配器基类。

新增一个数据源只需要：
1. 继承 BaseSource，实现 fetch() -> FetchResult
2. 在 config/sources.yaml 里加一段配置

主流程不需要任何改动。
"""

import os
import re
from typing import Optional, List

import requests
import urllib3
urllib3.disable_warnings(urllib3.exceptions.InsecureRequestWarning)

from ..core.config import get_config
from ..core.models import FetchResult, ExtraLine
from ..core.pokedex import Pokedex


class SourceError(RuntimeError):
    pass


class BaseSource:
    """所有数据源的基类。子类只需实现 fetch()。"""

    name = "base"

    def __init__(self, options: dict = None, pokedex: Pokedex = None, logger=None,
                 sample_data: dict = None):
        self.options = options or {}
        self.pokedex = pokedex or Pokedex()
        self.logger = logger
        self.config = get_config()
        # 注入的样本数据：有值时不再发网络请求，方便离线测试和回归
        self.sample_data = sample_data

    # ---------------- 工具方法 ----------------

    def log(self, msg, level="info"):
        if self.logger:
            getattr(self.logger, level, self.logger.info)(f"[{self.name}] {msg}")

    # ---------------- 转发服务 ----------------
    # 源站在海外、面板跑在国内机器上时直连不通，需要一层中转。
    # 中转一般在国内，走得通；源站只认中转的出口 IP。
    # 协议：POST 到中转地址，用 query 传目标地址，
    #   ?url=<目标URL>&method=GET    —— method 必须放 query，放 header 不生效
    #   x-proxy-key: <key>            —— 中转自身的鉴权（可选，取决于中转实现）

    def transform_plan(self):
        """返回中转配置 {base, method, headers}，没配中转则返回 None。"""
        env_name = self.options.get("transform_url_env")
        tf = (os.environ.get(env_name) or "").strip() if env_name else ""
        if not tf:
            return None
        return {
            "base": tf.rstrip("?&"),
            "method": (self.options.get("transform_method") or "POST").upper(),
            "headers": self.transform_headers(),
        }

    def transform_headers(self) -> dict:
        h = dict(self.options.get("transform_headers") or {})
        env_name = self.options.get("proxy_key_env") or "PROXY_KEY"
        key = (os.environ.get(env_name) or "").strip()
        if key:
            h["x-proxy-key"] = key
        return h

    def http_request(self, method: str, url: str, headers: dict = None) -> requests.Response:
        http = self.config.http
        proxies = None
        # 源级代理优先(只影响本源请求),其次全局配置或环境变量
        proxy = (self.options.get("proxy")
                 or http.get("proxy")
                 or os.environ.get("HTTPS_PROXY")
                 or os.environ.get("https_proxy"))
        if proxy:
            no_proxy = os.environ.get("NO_PROXY") or os.environ.get("no_proxy") or ""
            proxies = {"http": proxy, "https": proxy}
            if no_proxy:
                proxies["no_proxy"] = no_proxy
        # 分别给「连接」和「读取」设超时：连不上时快速失败，不要白等整个 timeout。
        # 源不可达属于典型情况（海外直连不通），这一项直接决定面板手感。
        total = http.get("timeout", 20)
        connect = min(http.get("connect_timeout", 5) or 5, total)
        return requests.request(
            method,
            url,
            headers=headers,
            timeout=(connect, total),
            verify=http.get("verify_ssl", False),
            proxies=proxies,
        )

    def http_get(self, url: str, headers: dict = None) -> requests.Response:
        return self.http_request("GET", url, headers)

    def request_json(self, url: str, headers: dict = None) -> dict:
        """带重试的 JSON 请求，全部失败抛异常。"""
        if self.sample_data is not None:
            self.log("使用注入的样本数据（不发网络请求）")
            return self.sample_data

        delays = self.config.http.get("retry_delays", [0, 0.5, 1])
        last_err = None
        import time

        # 走中转时用中转要求的方法（一般是 POST），直连时才是 GET
        tp = self.transform_plan()
        method = tp["method"] if tp else "GET"
        if tp and tp["headers"]:
            merged = dict(headers or {})
            merged.update(tp["headers"])
            headers = merged

        for i, d in enumerate(delays, 1):
            if d > 0:
                time.sleep(d)
            try:
                resp = self.http_request(method, url, headers)
                # 4xx 是「配置/接口层面」的问题，重试没有意义，直接抛明确原因
                if 400 <= resp.status_code < 500:
                    raise SourceError(self._http_hint(resp.status_code, url))
                resp.raise_for_status()
                return resp.json()
            except SourceError:
                raise
            except Exception as e:
                last_err = e
                self.log(f"第 {i} 次请求失败: {e}", "warning")
        raise SourceError(f"请求失败（已重试 {len(delays)} 次）: {last_err}")

    @staticmethod
    def _http_hint(code: int, url: str) -> str:
        """把常见的 4xx 翻译成人话，别让人对着状态码猜。"""
        hints = {
            401: "需要 API Key/登录态（把 key 配到环境变量后重启）",
            403: "被拒绝（key 无效或权限不足）",
            404: "接口不存在（源站可能改了路径）",
            410: "接口已永久下线（源站换了新接口，需要更新 target）",
            429: "触发限频（降低轮询频率）",
        }
        return f"HTTP {code} {hints.get(code, '请求被拒绝')}: {url}"

    def build_url(self) -> str:
        """拼接最终请求地址。配了中转就走中转，否则直连。"""
        target = self.options.get("target", "") or ""
        tp = self.transform_plan()
        if not tp:
            return target
        from urllib.parse import quote
        return f"{tp['base']}?url={quote(target, safe='')}&method=GET"

    # ---------------- 名称解析 ----------------

    def resolve_entry(self, zh=None, en=None, kind="move"):
        """把任意语言的名字解析成 (canonical中文名, id)。

        优先用英文名查（英文名是标准名，能绕开机翻）；
        英文名缺失或查不到时，退回中文名 + 别名表。
        """
        table = {
            "move": (self.pokedex.resolve_move_id, self.pokedex.canonical_move),
            "ability": (self.pokedex.resolve_ability_id, self.pokedex.canonical_ability),
            "pokemon": (self.pokedex.resolve_pokemon_id, self.pokedex.canonical_pokemon),
        }[kind]
        resolver, canonical = table

        # 英文优先
        if en:
            _id = resolver(en)
            if _id is not None:
                return canonical(en), _id
        if zh:
            _id = resolver(zh)
            if _id is not None:
                return canonical(zh), _id
            return zh, None  # 查不到就原样保留
        return (en or ""), None

    def make_extra(self, label_zh: str, label_en: str, values_zh: List[str]) -> Optional[ExtraLine]:
        """把一组中文值渲染成附加信息行，英文值自动翻译（技能类查图鉴）。"""
        if not values_zh:
            return None
        zh = f"{label_zh}: {', '.join(values_zh)}"
        en_vals = [self.pokedex.translate(v, "move", "en") for v in values_zh]
        en = f"{label_en}: {', '.join(en_vals)}"
        return ExtraLine(zh=zh, en=en)

    # ---------------- 去重 ----------------
    # 每个源「什么算同一条头目」标准不一样，交给适配器决定：
    # 有时段概念的源用时段/报点时间，没有的源用别的字段，
    # 什么都不填则由基类按头目内容算指纹兜底。

    def dedup_key(self, result: FetchResult) -> str:
        """去重用标识。返回空串表示这个源不去重（每次命中都推）。"""
        if result.dedup_key:
            return f"{result.dedup_key}"
        return self._content_fingerprint(result.boss)

    @staticmethod
    def _content_fingerprint(boss) -> str:
        """兜底：用头目内容算指纹（名称+特性+技能+地点+时段）。"""
        if boss is None:
            return ""
        raw = "|".join([
            boss.name or "",
            boss.ability or "",
            ",".join(boss.moves or []),
            boss.location or "",
            boss.period or "",
        ])
        if not raw.strip("|"):
            return ""
        import hashlib

        return "fp:" + hashlib.md5(raw.encode("utf-8")).hexdigest()[:16]

    # ---------------- 实现点 ----------------

    def fetch(self) -> FetchResult:
        raise NotImplementedError


# ---------------- 通用解析工具 ----------------

_GENDERLESS = {"n/a", "na", "none", "", "-", "无", "无性别", "genderless", "unknown"}


def parse_male_ratio(value) -> Optional[float]:
    """解析雄性百分比。

    各数据源给的性别格式五花八门，这里做防御式解析：
        "50%"   -> 50.0
        "50"    -> 50.0
        "0.5"   -> 50.0   （比例写法）
        "N/A"   -> None   （无性别）
        "公1母7" -> 12.5
    """
    if value is None:
        return None
    s = str(value).strip()
    if s.lower() in _GENDERLESS:
        return None

    # "公1母7" / "♂1♀7" 这类写法
    m = re.search(r"公\s*(\d+(?:\.\d+)?)\s*母\s*(\d+(?:\.\d+)?)", s)
    if m:
        male, female = float(m.group(1)), float(m.group(2))
        total = male + female
        return round(male * 100 / total, 1) if total else None

    m = re.search(r"(\d+(?:\.\d+)?)\s*%?", s)
    if not m:
        return None
    raw = m.group(1)
    val = float(raw)
    # 0.x 这种比例写法（且不是 "0"）转成百分比
    if "." in raw and 0 < val <= 1:
        val *= 100
    return val
