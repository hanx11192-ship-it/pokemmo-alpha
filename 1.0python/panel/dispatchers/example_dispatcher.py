# -*- coding: utf-8 -*-
"""
================================================================================
 示例分发器 / Example Dispatcher
================================================================================

【这是什么 / What is this】
一个可以直接上传使用的分发器示例。它演示了三件最常用的事：
A sample dispatcher you can upload as-is. It demonstrates the three things
people most often want to do:

  1. 拿到归一化后的头目数据后，读取你想要的字段
     Read the fields you want from the normalized boss data
  2. 在官方打法引擎生成的报告之外，追加你自己的内容
     Append your own content on top of the official engine's report
  3. 正确处理中 / 英 / 中英双语三种语言模式
     Handle zh / en / bilingual correctly

它和 default_dispatcher.py 的区别：
Difference from default_dispatcher.py:
    default  —— 原样输出引擎报告，不加任何东西
                emits the engine report verbatim
    本示例   —— 在报告末尾追加一行自定义队伍提示
                appends one custom team-hint line at the end

--------------------------------------------------------------------------------
 接口约定（所有分发器必须遵守）
 The contract (every dispatcher must follow it)
--------------------------------------------------------------------------------
    def dispatch(boss: BossData, ctx: dict) -> str

    boss : src.core.models.BossData —— 归一化后的头目，常用字段：
           Normalized alpha. Commonly used fields:
             boss.name         中文名 / Chinese name
             boss.name_en      英文名 / English name（可能为空 may be empty）
             boss.ability      特性 / ability
             boss.moves        技能列表 / move list
             boss.location     地点 / location
             boss.location_en  英文地点 / English location
             boss.period       时段，如 "14:30~15:45" / time window
             boss.gender       性别比例 / gender ratio
             boss.egg_groups   蛋组 / egg groups
             boss.pokedex_id   全国图鉴号 / National Dex number
             boss.extra_lines  源给的附加信息（秘传机、地点备注等）
                               extra info from the source (HMs, location notes…)

    ctx  : {
             "pokedex": Pokedex,     # 图鉴，可用 pokedex.translate(...) 翻译
             "rules":   Rules,       # 打法规则
             "langs":   List[str],   # ["zh"] / ["en"] / ["zh","en"]
           }

    返回 : 一段可直接推送 / 展示的文本
           A piece of text ready to push or display

--------------------------------------------------------------------------------
 怎么用 / How to use
--------------------------------------------------------------------------------
1. 面板 → 决策器 → 上传决策器，选这个文件
   Panel → Deciders → Upload, pick this file
2. 上传后在列表里启用它，再点「设为当前」
   Enable it in the list, then set it as active
3. 调试页填一个头目，选这个分发器，看输出效果
   Go to Debug, fill in a boss, pick this dispatcher, check the output

小提示：改坏了不会炸。分发器抛异常时面板会回退到内置决策器，
       并在日志里记一条 error，不会中断轮询。
Tip: breaking things is safe. If a dispatcher raises, the panel falls back to
     the built-in decider and logs an error — polling never stops.
"""

from src.strategy.engine import generate_report, generate_bilingual

# 面板展示用元信息 / Metadata shown in the panel
NAME = "示例分发器（带队伍提示）"
DESCRIPTION = "示例：在官方报告末尾追加一行自定义队伍提示，演示分发器写法"

# 自定义队伍提示：按图鉴号给一句提醒
# Custom team hints keyed by National Dex number.
# 想加自己的配置，往这个字典里塞就行。
# Add your own by extending this dict.
TEAM_HINTS = {
    282: {  # 沙奈朵 / Gardevoir
        "zh": "队伍提示：沙奈朵怕物理强攻，建议先手催眠再刀背。",
        "en": "Team hint: Gardevoir is frail physically — sleep it, then False Swipe.",
    },
    571: {  # 索罗亚克 / Zoroark
        "zh": "队伍提示：索罗亚克会变身，别被假属性骗了。",
        "en": "Team hint: Zoroark uses Illusion — don't trust the typing you see.",
    },
}


def dispatch(boss, ctx: dict) -> str:
    """生成最终播报文本 / Produce the final broadcast text."""
    rules = ctx["rules"]
    pokedex = ctx["pokedex"]
    langs = ctx.get("langs") or ["zh"]

    # 1) 先让官方引擎生成标准报告
    #    Let the official engine produce the standard report first
    if "zh" in langs and "en" in langs:
        text = generate_bilingual(boss, rules, pokedex, langs)
    else:
        text = generate_report(boss, rules, pokedex, langs[0])

    # 2) 再按图鉴号查自定义提示，有就追加
    #    Then look up a custom hint by dex number and append it
    hint = TEAM_HINTS.get(boss.pokedex_id)
    if hint:
        # 双语模式下把中英提示都带上；单语模式只给对应语言
        # In bilingual mode include both; otherwise only the matching language
        if "zh" in langs and "en" in langs:
            extra = f"{hint['zh']}\n{hint['en']}"
        elif "en" in langs:
            extra = hint["en"]
        else:
            extra = hint["zh"]
        text = f"{text}\n{extra}"

    return text
