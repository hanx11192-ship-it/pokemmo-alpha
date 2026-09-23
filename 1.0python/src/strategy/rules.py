# -*- coding: utf-8 -*-
"""
规则加载。

把 rules.yaml 里写的技能中文名解析成技能 id。
解析用 id 而不是字符串，好处是：上游 API 给英文、给机翻中文、给别名，
只要能落到同一个 id 就能正确匹配。

解析失败的名字（比如 Pokemmo 特有的叫法）不会被丢掉，
会留一份原名做字符串兜底匹配，保证老行为不退化。
"""

from dataclasses import dataclass, field
from typing import Dict, List, Set

from ..core.pokedex import Pokedex


@dataclass
class MoveGroup:
    ids: Set[int] = field(default_factory=set)
    unresolved: Set[str] = field(default_factory=set)  # 解析失败的原名，字符串兜底


class Rules:
    # 引擎直接引用的关键技能（挑衅/临别礼物/中转等）
    _KEY_MOVE_NAMES = [
        "挑衅", "特性互换", "神秘守护", "临别礼物", "掉包", "治愈之愿",
        "识破", "回复封锁", "搏命", "哈欠", "中转", "拍手",
    ]

    def __init__(self, doc: dict, pokedex: Pokedex):
        self.doc = doc
        self.pokedex = pokedex
        self.unresolved_keys: List[str] = []

        self.whitelist: List[str] = doc.get("whitelist") or []
        self.teams: Dict[str, List[str]] = doc.get("teams") or {}
        self.trigger = doc.get("anti_prankster_trigger") or {}
        self.pivot_insert_groups: List[str] = (doc.get("pivot_insert") or {}).get(
            "move_groups", []
        )
        self.abilities: Dict[str, List[str]] = doc.get("abilities") or {}
        self.weather_rules: List[dict] = doc.get("weather_rules") or []
        self.foresight: List[str] = doc.get("foresight_required") or []
        self.custom_moves: Dict[str, dict] = doc.get("custom_moves") or {}
        self.templates: Dict[str, dict] = doc.get("templates") or {}

        # 技能分组：中文名 → id 集合
        self.groups: Dict[str, MoveGroup] = {}
        for gname, names in (doc.get("move_groups") or {}).items():
            self.groups[gname] = self._build_group(names)

        # custom move 中文名 → key（输出时按语言取值）
        self.custom_by_zh: Dict[str, str] = {
            v.get("zh", k): k for k, v in self.custom_moves.items()
        }

        # 关键单技能：挑衅/特性互换/神秘守护/临别礼物/掉包/治愈之愿/识破/回复封锁/搏命/哈欠/中转/拍手
        self.key_moves: Dict[str, dict] = {}
        for cn in self._KEY_MOVE_NAMES:
            mid = pokedex.resolve_move_id(cn)
            self.key_moves[cn] = {"id": mid, "zh": cn}
            if mid is None:
                self.unresolved_keys.append(cn)

        # 特性触发条件
        self.ability_swap_ids: Set[int] = self._ability_ids(
            self.abilities.get("require_skill_swap", [])
        )
        self.taunt_second_ids: Set[int] = self._ability_ids(
            self.abilities.get("taunt_second", [])
        )
        self.prankster_id = pokedex.resolve_ability_id("恶作剧之心")

        # 天气规则：[(move_id, {ability_id...}), ...]
        self.weather: List[tuple] = []
        for r in self.weather_rules:
            mid = pokedex.resolve_move_id(r.get("move"))
            aids = self._ability_ids(r.get("abilities") or [])
            if mid is not None and aids:
                self.weather.append((mid, aids))

        # 队伍成员 / 白名单 / 识破名单 的 id，便于按 id 匹配
        self.prankster_pokemon_ids: Set[int] = {
            i for i in (pokedex.resolve_pokemon_id(n) for n in self.trigger.get("pokemon", []))
            if i is not None
        }
        self.whitelist_ids: Set[int] = {
            i for i in (pokedex.resolve_pokemon_id(n) for n in self.whitelist) if i is not None
        }
        self.foresight_ids: Set[int] = {
            i for i in (pokedex.resolve_pokemon_id(n) for n in self.foresight) if i is not None
        }

    def _build_group(self, names: List[str]) -> MoveGroup:
        g = MoveGroup()
        for n in names:
            # custom move（中转/拍手等）没有官方 id，走字符串
            if n in self.custom_moves:
                g.unresolved.add(n)
                continue
            mid = self.pokedex.resolve_move_id(n)
            if mid is None:
                g.unresolved.add(n)
            else:
                g.ids.add(mid)
        return g

    def _ability_ids(self, names: List[str]) -> Set[int]:
        out = set()
        for n in names:
            aid = self.pokedex.resolve_ability_id(n)
            if aid is not None:
                out.add(aid)
        return out

    # ---------------- 匹配 ----------------

    def boss_has_group(self, boss, gname: str) -> bool:
        """头目是否携带该分组里的任一技能。"""
        g = self.groups.get(gname)
        if not g:
            return False
        for mid in g.ids:
            if boss.has_move_id(mid):
                return True
        for name in g.unresolved:
            if name in boss.moves:
                return True
        return False

    def boss_has_any_group(self, boss, gnames: List[str]) -> bool:
        return any(self.boss_has_group(boss, g) for g in gnames)

    def move(self, cn_name: str) -> str:
        """取输出用的技能名（canonical 中文，输出时再翻译）。"""
        return cn_name

    # ---------------- 诊断 ----------------

    def report_unresolved(self) -> List[str]:
        """列出规则里解析不到 id 的名字，便于补齐 aliases.json。"""
        out = []
        for gname, g in self.groups.items():
            for n in sorted(g.unresolved):
                if n not in self.custom_moves:
                    out.append(f"move_groups.{gname}: {n}")
        for k in self.unresolved_keys:
            if k not in self.custom_moves:
                out.append(f"key_moves: {k}")
        return out
