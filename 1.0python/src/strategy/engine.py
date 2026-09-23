# -*- coding: utf-8 -*-
"""
打法引擎。

输入：BossData（名称 / 特性 / 技能，已归一化成官方中文）
输出：一段推送文本

两个设计点：
1. 规则匹配全部走技能 id，所以上游给英文、机翻中文、别名都能命中同一条规则。
2. 输出阶段才按目标语言翻译，同一套规则可以同时产出中英文报告。

行为严格对齐原 autodoalpha.py（已稳定运行两个月），不改动任何判定逻辑。
"""

from typing import List, Optional

from ..core.models import BossData
from ..core.pokedex import Pokedex
from .rules import Rules


class StrategyEngine:
    def __init__(self, rules: Rules, pokedex: Pokedex, lang: str = "zh"):
        self.rules = rules
        self.pokedex = pokedex
        self.lang = lang
        self.tmpl = rules.templates.get(lang) or rules.templates.get("zh") or {}

    # ---------------- 翻译 ----------------

    def t_move(self, cn: str) -> str:
        key = self.rules.custom_by_zh.get(cn)
        if key:
            return self.pokedex.custom_move(key, self.lang)
        if self.lang == "zh":
            return cn
        return self.pokedex.translate(cn, "move", self.lang)

    def t_pokemon(self, cn: str) -> str:
        if self.lang == "zh":
            return cn
        return self.pokedex.translate(cn, "pokemon", self.lang)

    def t_ability(self, cn: str) -> str:
        if self.lang == "zh":
            return cn
        return self.pokedex.translate(cn, "ability", self.lang)

    def t_list(self, names: List[str], kind: str) -> str:
        """翻译一个名字列表。技能列表会走 custom_moves（中转/拍手等）。"""
        if not names:
            return ""
        sep = self.tmpl.get("list_separator", ", ")
        if kind == "move":
            items = [self.t_move(n) for n in names]
        elif kind == "pokemon":
            items = [self.t_pokemon(n) for n in names]
        else:
            items = [self.t_ability(n) for n in names]
        return sep.join(items)

    # ---------------- 判定辅助 ----------------

    def _eq(self, boss_value: str, boss_id: Optional[int], id_set: set, name_list: List[str]) -> bool:
        """优先按 id 匹配，id 不可用时退回字符串匹配。"""
        if boss_id is not None:
            return boss_id in id_set
        return boss_value in name_list

    def _is_prankster(self, boss: BossData) -> bool:
        r = self.rules
        if boss.pokedex_id is not None and boss.pokedex_id in r.prankster_pokemon_ids:
            return True
        if boss.ability_id is not None and r.prankster_id and boss.ability_id == r.prankster_id:
            return True
        return boss.name in (r.trigger.get("pokemon") or []) or boss.ability == "恶作剧之心"

    def _needs_skill_swap(self, boss: BossData) -> bool:
        r = self.rules
        if boss.ability_id is not None:
            return boss.ability_id in r.ability_swap_ids
        return boss.ability in (r.abilities.get("require_skill_swap") or [])

    def _needs_foresight(self, boss: BossData) -> bool:
        r = self.rules
        if boss.pokedex_id is not None:
            return boss.pokedex_id in r.foresight_ids
        return boss.name in r.foresight

    def _weather_skill_swap(self, boss: BossData) -> bool:
        if boss.ability_id is None:
            # 没有 id 时退回字符串比较
            for w in self.rules.weather_rules:
                if w.get("move") in boss.moves and boss.ability in (w.get("abilities") or []):
                    return True
            return False
        for mid, aids in self.rules.weather:
            if boss.has_move_id(mid) and boss.ability_id in aids:
                return True
        return False

    # ---------------- 队伍 ----------------

    def team_order(self, boss: BossData) -> List[str]:
        r = self.rules
        if self._is_prankster(boss):
            team = list(r.teams.get("anti_prankster") or [])
        else:
            team = list(r.teams.get("default") or [])

        # 头目带先制技或回复技 → 插一个中转手
        if team and r.boss_has_any_group(boss, r.pivot_insert_groups):
            team.append(team[3])
            team[2], team[3] = team[3], team[2]
        return team

    # ---------------- 各位置配招 ----------------

    def skills_gardevoir(self, boss: BossData) -> List[str]:
        r = self.rules
        out: List[str] = []
        if r.boss_has_any_group(boss, ["defense", "boost"]):
            out.append("挑衅")
        if self._needs_skill_swap(boss):
            out.append("特性互换")
        if self._weather_skill_swap(boss):
            out.append("特性互换")
        if r.boss_has_any_group(boss, ["freeze", "paralysis"]):
            out.append("神秘守护")
        out.append("临别礼物")

        # 去重（保持顺序）
        new: List[str] = []
        for s in out:
            if s not in new:
                new.append(s)

        # 满 4 格时优先牺牲神秘守护
        if len(new) == 4 and "神秘守护" in new:
            new.remove("神秘守护")

        # 奇迹皮肤 / 魔法镜下，挑衅要排到第二位
        if boss.ability_id is not None:
            is_taunt_second = boss.ability_id in r.taunt_second_ids
        else:
            is_taunt_second = boss.ability in (r.abilities.get("taunt_second") or [])
        if is_taunt_second and len(new) == 3:
            new[0], new[1] = new[1], new[0]

        return new

    def skills_lopunny(self, boss: BossData, gardevoir_skills: List[str]) -> List[str]:
        left = 3 - len(gardevoir_skills)
        out: List[str] = []
        if "挑衅" in gardevoir_skills and left == 0:
            out.append("拍手")
            out.append("掉包")
        if "挑衅" in gardevoir_skills and left == 1:
            out.append("掉包")
            out.append("拍手")
        if "挑衅" not in gardevoir_skills:
            out.append("掉包")
        if self._needs_foresight(boss):
            out.append("识破")
        out.append("治愈之愿")
        return out

    def skills_smeargle(self, boss: BossData) -> List[str]:
        out: List[str] = []
        if self.rules.boss_has_group(boss, "heal"):
            out.append("回复封锁")
        out.append("搏命")
        return out

    def skills_zoroark(self, boss: BossData) -> List[str]:
        """索罗亚克：头目是恶作剧之心时，用它顶替沙奈朵（同一个位置的两套人选）。

        挑衅的触发条件和沙奈朵共用同一份名单，
        但它不会神秘守护、也不会特性互换，所以只有这两招。
        """
        out: List[str] = []
        if self.rules.boss_has_any_group(boss, ["defense", "boost"]):
            out.append("挑衅")
        out.append("临别礼物")
        return out

    def skills_pivot(self, boss: BossData) -> List[str]:
        """中转手。

        具体是谁上场取决于队伍配置（月亮伊布接棒 / 呆壳兽瞬间移动），
        统一输出「哈欠 + 中转」，不写死具体技能。
        """
        return ["哈欠", "中转"]

    # ---------------- 主入口 ----------------

    def generate(self, boss: BossData) -> str:
        t = self.tmpl
        r = self.rules

        period = (boss.period or t.get("period_unknown", "未知时段")).replace("~", "-")
        name = self.t_pokemon(boss.name or t.get("name_unknown", "未知"))
        ability = self.t_ability(boss.ability or t.get("ability_none", "无特性"))
        location = (boss.location_en if self.lang == "en" and boss.location_en else boss.location) \
            or t.get("location_unknown", "未知地点")

        if boss.gender.male_percent is not None:
            rate_str = f"{boss.gender.male_percent}{t.get('gender_suffix', '%公')}"
        else:
            rate_str = t.get("no_gender", "无性别")

        moves_str = self.t_list(boss.moves, "move") or t.get("moves_none", "无")
        # 英文播报时蛋组也要是英文。优先级：源给的英文字段 > 图鉴对照表翻译 >
        # 原样输出。图鉴里只有中文蛋组，缺了这层翻译，英文播报会掺中文。
        if self.lang == "en" and boss.egg_groups_en:
            egg = boss.egg_groups_en
        elif self.lang == "en":
            egg = [self.pokedex.egg_group_name(g, "en") for g in boss.egg_groups]
        else:
            egg = boss.egg_groups
        egg_str = ", ".join(egg) if egg else ""

        second_line = f"{name}({ability})"
        if egg_str:
            second_line += f"-({egg_str})"
        second_line += f"-{rate_str}"

        header = [
            period,
            second_line,
            location,
            f"{t.get('moves_label', '技能: ')}{moves_str}",
        ]

        # 附加信息（秘传机、地点备注等）：插在头目信息和打法推荐之间
        extra = [e.text(self.lang) for e in boss.extra_lines if e.text(self.lang)]

        # 报点人：始终展示（无论单/双性别、是否出打法），向所有报点者致谢
        reporter_line = ""
        if boss.reporter:
            reporter_line = f"{t.get('reporter_label', '报点人: ')}{boss.reporter}"

        # 双性别（同进化链存在异性）或白名单才出打法
        is_dual = False
        if boss.pokedex_id is not None:
            if boss.pokedex_id in r.whitelist_ids:
                is_dual = True
        elif boss.name in r.whitelist:
            is_dual = True
        if boss.gender.is_dual:
            is_dual = True

        if not is_dual:
            # 单性别 / 无性别只推信息，不推打法，但报点人仍展示
            return "\n".join(x for x in header + extra + [reporter_line] if x)

        team = self.team_order(boss)

        # 主攻手（沙奈朵 / 索罗亚克二选一）先单独算出来：
        # 长耳兔要按它占掉的技能位来配招，不能依赖队伍里的先后顺序。
        # 原脚本这里写死读 team_skills["沙奈朵"]，恶作剧之心队伍里没有沙奈朵，
        # 读出来是空列表，长耳兔的技能位就算错了。
        # 注意两者配招规则不同：索罗亚克只有挑衅 + 临别礼物。
        main_fn = {"沙奈朵": self.skills_gardevoir, "索罗亚克": self.skills_zoroark}
        main_poke = next((p for p in team if p in main_fn), None)
        main_skills = main_fn[main_poke](boss) if main_poke else []

        team_skills = {}
        for poke in team:
            if poke == main_poke:
                team_skills[poke] = main_skills
            elif poke == "长耳兔":
                team_skills[poke] = self.skills_lopunny(boss, main_skills)
            elif poke == "图图犬":
                team_skills[poke] = self.skills_smeargle(boss)
            elif poke in ("呆壳兽", "月亮伊布"):
                team_skills[poke] = self.skills_pivot(boss)
            else:
                team_skills[poke] = []

        name_sep = t.get("name_separator", "：")
        lines = []
        for poke in team:
            sk = team_skills.get(poke, [])
            lines.append(
                f"{self.t_pokemon(poke)}{name_sep}"
                f"{self.t_list(sk, 'move') or t.get('moves_none', '无')}"
            )

        report = "\n".join(header + extra)
        report += f"\n{t.get('strategy_header', '打法推荐：')}\n"
        report += "\n".join(lines)
        if boss.reporter:
            report += f"\n{t.get('reporter_label', '报点人: ')}{boss.reporter}"
        return report


def generate_report(boss: BossData, rules: Rules, pokedex: Pokedex, lang: str = "zh") -> str:
    return StrategyEngine(rules, pokedex, lang).generate(boss)


def generate_bilingual(boss: BossData, rules: Rules, pokedex: Pokedex,
                       langs: List[str] = None, separator: str = "\n\n—————\n\n") -> str:
    """多语言合并输出（language: both 时用）。"""
    langs = langs or ["zh", "en"]
    parts = [generate_report(boss, rules, pokedex, l) for l in langs]
    return separator.join(parts)
