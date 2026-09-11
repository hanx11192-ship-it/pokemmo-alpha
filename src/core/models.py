# -*- coding: utf-8 -*-
"""
领域模型。

这里定义的是「数据源」和「打法引擎」之间的契约：
不管上游 API 长什么样、返回什么语言，适配器都必须把数据捏成 BossData，
引擎只认这一种结构。
"""

from dataclasses import dataclass, field
from typing import List, Optional


@dataclass
class Gender:
    """性别比例。

    male_percent 为雄性百分比；None 表示无性别（不能走甜蜜球那套打法）。
    """
    male_percent: Optional[float] = None

    @property
    def is_dual(self) -> bool:
        """同进化链存在异性 —— 出招方案只对这种情况生效。"""
        return self.male_percent is not None and 0 < self.male_percent < 100

    @classmethod
    def from_male_percent(cls, v) -> "Gender":
        if v is None:
            return cls(None)
        try:
            return cls(float(v))
        except (TypeError, ValueError):
            return cls(None)


@dataclass
class ExtraLine:
    """附加信息行。

    不同 API 能提供的额外字段不一样（比如只有 Alphapedia 给秘传机信息），
    由适配器决定要不要给、给什么内容，主流程负责插到固定位置：
    「头目信息」和「打法推荐」之间。
    """
    zh: str
    en: str

    def text(self, lang: str) -> str:
        return self.en if lang == "en" else self.zh

    @classmethod
    def from_value(cls, zh: str, en: str) -> "ExtraLine":
        return cls(zh=zh, en=en)


@dataclass
class BossData:
    """归一化后的头目信息。

    三个必需字段是打法引擎真正依赖的全部内容：
        name    —— 头目名称（已归一化为官方中文）
        ability —— 特性（已归一化为官方中文）
        moves   —— 四个技能（已归一化为官方中文）

    其余都是可选项，缺了不影响出招，只是推送内容少几行。
    """
    # ---- 必需 ----
    name: str
    ability: str
    moves: List[str] = field(default_factory=list)

    # ---- 展示用可选信息 ----
    period: str = ""
    location: str = ""          # 中文地点
    location_en: str = ""       # 英文地点（源提供时填，否则英文推送里会夹中文）
    reporter: str = ""
    gender: Gender = field(default_factory=Gender)
    egg_groups: List[str] = field(default_factory=list)
    egg_groups_en: List[str] = field(default_factory=list)
    extra_lines: List[ExtraLine] = field(default_factory=list)

    # ---- 解析副产物：canonical id，引擎用来做规则匹配 ----
    pokedex_id: Optional[int] = None
    ability_id: Optional[int] = None
    move_ids: List[Optional[int]] = field(default_factory=list)

    # ---- 溯源 ----
    source: str = ""
    reported_at: str = ""      # 报点时间（ISO），用于多源投票裁决与跨源去重分桶

    def move_id_set(self) -> set:
        """引擎匹配用的技能 id 集合（解析失败的技能会被跳过）。"""
        return {i for i in self.move_ids if i is not None}

    def has_move_id(self, mid: int) -> bool:
        return mid in self.move_id_set()


@dataclass
class FetchResult:
    """数据源适配器返回值。

    status:
        hit    —— 拿到头目且有效，应该推送
        empty  —— 请求成功，但当前时段确实没刷 / 头目已过期
        error  —— 请求或解析失败（主流程会据此决定是否换下一个源）
    """
    status: str
    boss: Optional[BossData] = None

    # 下面三项都由适配器决定 —— 不是所有数据源都有「时段」这个概念。
    # 没有时段概念的源：填 dedup_key 就行，slot_name 留空（摘要走兜底）；
    # 连 dedup_key 都不填也没关系，基类会用头目内容算指纹兜底。
    dedup_key: str = ""         # 去重用标识（通常是报点时间）
    slot_name: str = ""         # 时段名（早头/午头/晚头/晨头），用作推送摘要
    slot_name_en: str = ""      # 时段名英文
    message: str = ""

    @property
    def is_hit(self) -> bool:
        return self.status == "hit" and self.boss is not None
