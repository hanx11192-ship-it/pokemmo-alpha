# -*- coding: utf-8 -*-
"""
多语言图鉴。

核心思路：所有名字都先解析成数字 id，再从 id 取目标语言的名字。
    「任意语言 / 机翻 / 别名」 → id → 「官方中文 / 官方英文」

这样：
- 上游 API 给英文（Alphapedia 的 movesEn / abilityEn）→ 直接查 id，绕开机翻
- 上游 API 给中文机翻 → 走别名表 → 查 id
- 输出想要什么语言，从 id 取就行
"""

import json
import os
import re
from typing import Dict, Optional

import yaml

ROOT = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))


# 蛋组只有 15 个，且图鉴里只存了中文（build_pokedex 只导了中文 prose）。
# 英文播报要出英文蛋组，就得有这张对照表 —— 源站给英文字段时优先用源的，
# 没给（或源只给中文）时靠这张表兜底。
EGG_GROUP_ZH_TO_EN = {
    "怪兽": "Monster",
    "水中1": "Water 1",
    "水中2": "Water 2",
    "水中3": "Water 3",
    "水中３": "Water 3",   # 图鉴里有全角 ３ 的脏数据，一起认
    "虫": "Bug",
    "飞行": "Flying",
    "陆上": "Field",
    "妖精": "Fairy",
    "植物": "Grass",
    "人型": "Human-Like",
    "人形": "Human-Like",  # 两种写法都收
    "矿物": "Mineral",
    "不定形": "Amorphous",
    "百变怪": "Ditto",
    "龙": "Dragon",
    "未发现": "Undiscovered",
}


def _norm(s: str) -> str:
    """归一化：小写、去掉所有非字母数字汉字的字符。

    "Zap-Cannon" / "Zap Cannon" / "zapcannon" 都会变成 "zapcannon"
    """
    if not s:
        return ""
    return re.sub(r"[^0-9a-z一-鿿]", "", str(s).lower())


class Pokedex:
    def __init__(self, data_path: str = None, aliases_path: str = None, rules_path: str = None):
        self.data_path = data_path or os.path.join(ROOT, "data", "pokedex.json")
        self.aliases_path = aliases_path or os.path.join(ROOT, "data", "aliases.json")
        self.rules_path = rules_path or os.path.join(ROOT, "config", "rules.yaml")

        with open(self.data_path, encoding="utf-8") as f:
            self._data = json.load(f)

        self.pokemon: Dict[str, dict] = self._data["pokemon"]
        self.abilities: Dict[str, dict] = self._data["abilities"]
        self.moves: Dict[str, dict] = self._data["moves"]

        with open(self.aliases_path, encoding="utf-8") as f:
            self._aliases = json.load(f)

        # custom_moves 里的自定义技能（中转/拍手等），也要能被解析
        self._custom: Dict[str, dict] = {}
        try:
            with open(self.rules_path, encoding="utf-8") as f:
                rules = yaml.safe_load(f) or {}
            self._custom = rules.get("custom_moves", {}) or {}
        except Exception:
            self._custom = {}

        self._idx_pokemon = self._build_index(self.pokemon)
        self._idx_abilities = self._build_index(self.abilities)
        self._idx_moves = self._build_index(self.moves)

        # 自定义技能索引：名字 → 官方中文键
        for key, vals in self._custom.items():
            for v in vals.values():
                if v:
                    self._idx_moves.setdefault(_norm(v), key)

        # 别名索引（别名 → 官方中文名）
        self._alias_move = {_norm(k): v for k, v in (self._aliases.get("moves") or {}).items()}
        self._alias_ability = {_norm(k): v for k, v in (self._aliases.get("abilities") or {}).items()}
        self._alias_pokemon = {_norm(k): v for k, v in (self._aliases.get("pokemon") or {}).items()}

    def _build_index(self, table: Dict[str, dict]) -> Dict[str, str]:
        """{归一化名: id}"""
        idx: Dict[str, str] = {}
        for _id, names in table.items():
            for lang in ("zh", "en"):
                v = names.get(lang)
                if v:
                    idx.setdefault(_norm(v), _id)
        return idx

    # ---------------- 解析：名字 → id ----------------

    def resolve_move_id(self, name: str) -> Optional[int]:
        return self._as_int(self._resolve(name, self._alias_move, self._idx_moves))

    def resolve_ability_id(self, name: str) -> Optional[int]:
        return self._as_int(self._resolve(name, self._alias_ability, self._idx_abilities))

    def resolve_pokemon_id(self, name: str) -> Optional[int]:
        return self._as_int(self._resolve(name, self._alias_pokemon, self._idx_pokemon))

    @staticmethod
    def _as_int(v):
        if v is None:
            return None
        try:
            return int(v)
        except (TypeError, ValueError):
            return None

    def _resolve(self, name: str, alias: Dict[str, str], idx: Dict[str, str]) -> Optional[str]:
        if not name:
            return None
        n = _norm(name)
        # 1) 先查别名（机翻名 / 老叫法 → 官方中文名）
        if n in alias:
            n = _norm(alias[n])
        # 2) 查正向索引
        if n in idx:
            return idx[n]
        return None

    # ---------------- 归一化：名字 → 官方中文名 ----------------

    def canonical_move(self, name: str) -> str:
        mid = self.resolve_move_id(name)
        if mid is None:
            return name  # 查不到就原样保留，不丢信息
        return self.moves.get(str(mid), {}).get("zh") or name

    def canonical_ability(self, name: str) -> str:
        aid = self.resolve_ability_id(name)
        if aid is None:
            return name
        return self.abilities.get(str(aid), {}).get("zh") or name

    def canonical_pokemon(self, name: str) -> str:
        pid = self.resolve_pokemon_id(name)
        if pid is None:
            return name
        return self.pokemon.get(str(pid), {}).get("zh") or name

    # ---------------- 本地化：id → 目标语言名 ----------------

    def move_name(self, mid, lang: str = "zh") -> str:
        return self._name(self.moves, mid, lang)

    def ability_name(self, aid, lang: str = "zh") -> str:
        return self._name(self.abilities, aid, lang)

    def pokemon_name(self, pid, lang: str = "zh") -> str:
        return self._name(self.pokemon, pid, lang)

    def _name(self, table: Dict[str, dict], _id, lang: str) -> Optional[str]:
        if _id is None:
            return None
        entry = table.get(str(_id))
        if not entry:
            return None
        return entry.get(lang) or entry.get("zh") or entry.get("en")

    # ---------------- 便捷方法 ----------------

    def gender_of(self, pid) -> Optional[float]:
        """返回雄性百分比；None 表示无性别。"""
        entry = self.pokemon.get(str(pid))
        if not entry:
            return None
        return entry.get("gender_rate")

    def translate(self, canonical_zh: str, kind: str, lang: str) -> str:
        """把官方中文名翻译成目标语言。kind: move / ability / pokemon"""
        if lang == "zh" or not canonical_zh:
            return canonical_zh
        table, resolver = {
            "move": (self.moves, self.resolve_move_id),
            "ability": (self.abilities, self.resolve_ability_id),
            "pokemon": (self.pokemon, self.resolve_pokemon_id),
        }[kind]
        _id = resolver(canonical_zh)
        if _id is None:
            return canonical_zh
        return self._name(table, _id, lang) or canonical_zh

    def custom_move(self, key: str, lang: str) -> str:
        """自定义技能（中转/拍手等）的指定语言写法。"""
        vals = self._custom.get(key) or {}
        return vals.get(lang) or vals.get("zh") or key

    def egg_group_name(self, zh: str, lang: str = "zh") -> str:
        """蛋组名的指定语言写法。图鉴只存了中文，英文靠内置对照表。"""
        if lang == "zh" or not zh:
            return zh
        return EGG_GROUP_ZH_TO_EN.get(zh.strip()) or EGG_GROUP_ZH_TO_EN.get(
            _norm(zh)) or zh

    def egg_groups(self, groups, lang: str = "zh") -> list:
        return [self.egg_group_name(g, lang) for g in (groups or [])]

    def stats(self) -> dict:
        return {
            "pokemon": len(self.pokemon),
            "abilities": len(self.abilities),
            "moves": len(self.moves),
            "custom_moves": len(self._custom),
        }


_pokedex: Optional[Pokedex] = None


def get_pokedex() -> Pokedex:
    global _pokedex
    if _pokedex is None:
        _pokedex = Pokedex()
    return _pokedex
