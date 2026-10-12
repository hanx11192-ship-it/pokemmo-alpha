# -*- coding: utf-8 -*-
"""
默认分发器（内置）。

接口约定（所有分发器都必须实现）：
    def dispatch(boss: BossData, ctx: dict) -> str
        boss : src.core.models.BossData  归一化后的头目
        ctx  : { "pokedex": Pokedex, "rules": Rules, "langs": List[str] }
        返回 : 一段可直接推送 / 展示的文本

本分发器直接包装 src/strategy/engine.py 的官方打法引擎，
即为「我的这个决策器」的默认实现。后续上传的分发器只要实现同样的
dispatch(boss, ctx) 即可被面板加载、启用、切换。
"""

from src.strategy.engine import generate_report, generate_bilingual

# 面板展示用元信息
NAME = "默认决策器"
DESCRIPTION = "内置：基于 src/strategy/engine.py 的官方打法引擎"


def dispatch(boss, ctx: dict) -> str:
    rules = ctx["rules"]
    pokedex = ctx["pokedex"]
    langs = ctx.get("langs") or ["zh"]

    if "zh" in langs and "en" in langs:
        return generate_bilingual(boss, rules, pokedex, langs)
    return generate_report(boss, rules, pokedex, langs[0])
