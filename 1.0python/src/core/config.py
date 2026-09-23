# -*- coding: utf-8 -*-
"""配置加载。所有路径相对项目根目录解析，不依赖运行时的 cwd。"""

import os
import yaml

ROOT = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))


def abspath(*parts) -> str:
    """把相对路径解析到项目根目录下。"""
    p = os.path.join(ROOT, *parts)
    return os.path.normpath(p)


def load_yaml(*parts) -> dict:
    path = abspath(*parts)
    with open(path, encoding="utf-8") as f:
        return yaml.safe_load(f) or {}


class Config:
    def __init__(self):
        self.settings = load_yaml("config", "settings.yaml")
        self.sources_doc = load_yaml("config", "sources.yaml")
        self.rules = load_yaml("config", "rules.yaml")

    # ---- 常用项 ----
    @property
    def timezone(self) -> str:
        return self.settings.get("timezone", "Asia/Shanghai")

    @property
    def language(self) -> str:
        """zh / en / both"""
        return self.settings.get("language", "zh")

    @property
    def languages(self) -> list:
        lang = self.language
        if lang == "both":
            return ["zh", "en"]
        return [lang]

    @property
    def http(self) -> dict:
        return self.settings.get("http", {}) or {}

    @property
    def notify(self) -> dict:
        return self.settings.get("notify", {}) or {}

    @property
    def dedup(self) -> dict:
        return self.settings.get("dedup", {}) or {}

    def state_path(self) -> str:
        return abspath(self.dedup.get("state_file", "data/state.json"))

    def enabled_sources(self) -> list:
        """按 priority 升序返回已启用的源配置。"""
        srcs = [s for s in (self.sources_doc.get("sources") or []) if s.get("enabled")]
        return sorted(srcs, key=lambda s: s.get("priority", 100))

    def templates(self, lang: str) -> dict:
        return (self.rules.get("templates") or {}).get(lang, {}) or {}


_cfg = None


def get_config() -> Config:
    global _cfg
    if _cfg is None:
        _cfg = Config()
    return _cfg
