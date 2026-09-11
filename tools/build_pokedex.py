#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""
构建多语言宝可梦图鉴 pokedex.json

数据来源：PokeAPI 全量 CSV（https://github.com/PokeAPI/pokeapi/tree/master/data/v2/csv）
一次下载、离线组装，不需要逐个请求 API。

用法：
    python3 tools/build_pokedex.py                # 用缓存，没有则下载
    python3 tools/build_pokedex.py --refresh      # 强制重新下载 CSV
    python3 tools/build_pokedex.py --proxy http://127.0.0.1:7890

输出结构：
    {
      "meta": {...},
      "pokemon":   {"462": {"zh": "自爆磁怪", "en": "Magnezone", "gender_rate": null}},
      "abilities": {"1":   {"zh": "恶臭",     "en": "Stench"}},
      "moves":     {"1":   {"zh": "拍击",     "en": "Pound"}}
    }

为什么用全国图鉴 id 做键：
    不同 API 返回的名称语言不同（有中文、有机翻中文、有英文），
    只有数字 id 是稳定锚点。解析流程统一为「任意语言名 → id → 目标语言名」。
"""

import argparse
import csv
import json
import os
import sys
import urllib.request
from datetime import datetime, timezone, timedelta

BASE = "https://raw.githubusercontent.com/PokeAPI/pokeapi/master/data/v2/csv"
NEEDED = [
    "pokemon_species.csv",
    "pokemon_species_names.csv",
    "abilities.csv",
    "ability_names.csv",
    "moves.csv",
    "move_names.csv",
    "languages.csv",
    "egg_groups.csv",
    "egg_group_prose.csv",
    "pokemon_abilities.csv",
    "pokemon_egg_groups.csv",
]

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(HERE)
CACHE = os.path.join(ROOT, ".cache")
OUT = os.path.join(ROOT, "data", "pokedex.json")

# PokeAPI 语言 id：9=en, 12=zh-Hans
LANG = {"en": "9", "zh": "12"}


def log(msg):
    print(msg, flush=True)


def fetch(name, refresh=False, proxy=None):
    os.makedirs(CACHE, exist_ok=True)
    path = os.path.join(CACHE, name)
    if os.path.exists(path) and not refresh:
        return path
    url = f"{BASE}/{name}"
    log(f"  下载 {name} ...")
    opener = urllib.request.build_opener()
    if proxy:
        opener = urllib.request.build_opener(
            urllib.request.ProxyHandler({"http": proxy, "https": proxy})
        )
    req = urllib.request.Request(url, headers={"User-Agent": "pokedex-builder/1.0"})
    with opener.open(req, timeout=120) as r, open(path, "wb") as f:
        f.write(r.read())
    return path


def read_csv(name):
    with open(os.path.join(CACHE, name), encoding="utf-8") as f:
        return list(csv.DictReader(f))


def build_lang_map(rows, key_field, name_field="name"):
    """行列表 -> {id: {zh:..., en:...}}"""
    out = {}
    for r in rows:
        lid = r["local_language_id"]
        if lid not in LANG.values():
            continue
        out.setdefault(r[key_field], {})
        if lid == LANG["zh"]:
            out[r[key_field]]["zh"] = r[name_field]
        elif lid == LANG["en"]:
            out[r[key_field]]["en"] = r[name_field]
    return out


def male_percent(gender_rate):
    """PokeAPI gender_rate 是「雌性比例」，单位为 1/8；-1 表示无性别。
    转成雄性百分比，与项目原有 data.json 的语义保持一致。"""
    try:
        g = int(gender_rate)
    except (TypeError, ValueError):
        return None
    if g == -1:
        return None
    return round((8 - g) * 100 / 8, 1)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--refresh", action="store_true", help="强制重新下载 CSV")
    ap.add_argument("--proxy", default=os.environ.get("HTTPS_PROXY"), help="下载代理")
    args = ap.parse_args()

    log("准备 CSV ...")
    for n in NEEDED:
        try:
            fetch(n, args.refresh, args.proxy)
        except Exception as e:
            log(f"[失败] {n}: {e}")
            sys.exit(1)

    # ---------- 精灵 ----------
    species = {r["id"]: r for r in read_csv("pokemon_species.csv")}
    sp_names = build_lang_map(read_csv("pokemon_species_names.csv"), "pokemon_species_id")

    egg_zh = {}
    for r in read_csv("egg_group_prose.csv"):
        if r["local_language_id"] == LANG["zh"]:
            egg_zh[r["egg_group_id"]] = r["name"]

    # species_id -> 第一个形态的 pokemon_id（用于查特性/蛋组）
    sp_to_mon = {}
    for r in read_csv("pokemon.csv"):
        sp_to_mon.setdefault(r["species_id"], r["id"])

    # 隐藏特性：Pokemmo 的头目一律是隐藏特性，
    # 所以只给图鉴号、不给特性的数据源可以直接从图鉴补出来。
    ability_names = build_lang_map(read_csv("ability_names.csv"), "ability_id")
    hidden_by_mon, normal_by_mon = {}, {}
    for r in read_csv("pokemon_abilities.csv"):
        pid = r["pokemon_id"]
        if r["is_hidden"] == "1":
            hidden_by_mon.setdefault(pid, r["ability_id"])
        else:
            normal_by_mon.setdefault(pid, []).append(r["ability_id"])

    # 蛋组：species_id -> [中文组名]
    egg_by_species = {}
    for r in read_csv("pokemon_egg_groups.csv"):
        sid = r["species_id"]
        egg_by_species.setdefault(sid, []).append(egg_zh.get(r["egg_group_id"], r["egg_group_id"]))

    pokemon = {}
    missing_zh = 0
    for sid, sp in species.items():
        nm = sp_names.get(sid, {})
        zh = nm.get("zh")
        en = nm.get("en") or sp["identifier"]
        if not zh:
            zh = en  # 兜底：没有中文就用英文
            missing_zh += 1

        mon_id = sp_to_mon.get(sid)
        # 隐藏特性优先；没有隐藏特性且只有一个普通特性时，用它
        hid = hidden_by_mon.get(mon_id)
        if hid is None:
            normals = normal_by_mon.get(mon_id) or []
            if len(normals) == 1:
                hid = normals[0]
        hid_zh = None
        if hid is not None:
            hid_zh = (ability_names.get(hid) or {}).get("zh")

        pokemon[sid] = {
            "zh": zh,
            "en": en,
            "gender_rate": male_percent(sp.get("gender_rate")),
            "egg_groups": egg_by_species.get(sid, []),
            "hidden_ability_id": int(hid) if hid is not None else None,
            "hidden_ability": hid_zh,
        }

    # ---------- 特性 ----------
    abilities = {}
    for r in read_csv("abilities.csv"):
        aid = r["id"]
        abilities[aid] = {"zh": None, "en": r["identifier"]}
    for aid, nm in build_lang_map(read_csv("ability_names.csv"), "ability_id").items():
        if aid in abilities:
            abilities[aid]["zh"] = nm.get("zh")
            abilities[aid]["en"] = nm.get("en") or abilities[aid]["en"]
    ab_missing = 0
    for aid, v in abilities.items():
        if not v["zh"]:
            v["zh"] = v["en"]
            ab_missing += 1

    # ---------- 技能 ----------
    moves = {}
    for r in read_csv("moves.csv"):
        moves[r["id"]] = {"zh": None, "en": r["identifier"]}
    for mid, nm in build_lang_map(read_csv("move_names.csv"), "move_id").items():
        if mid in moves:
            moves[mid]["zh"] = nm.get("zh")
            moves[mid]["en"] = nm.get("en") or moves[mid]["en"]
    mv_missing = 0
    for mid, v in moves.items():
        if not v["zh"]:
            v["zh"] = v["en"]
            mv_missing += 1

    payload = {
        "meta": {
            "version": "1.0",
            "generated_at": datetime.now(timezone(timedelta(hours=8))).isoformat(
                timespec="seconds"
            ),
            "source": "PokeAPI CSV (v2)",
            "languages": ["zh", "en"],
            "counts": {
                "pokemon": len(pokemon),
                "abilities": len(abilities),
                "moves": len(moves),
            },
            "notes": {
                "missing_zh_pokemon": missing_zh,
                "missing_zh_ability": ab_missing,
                "missing_zh_move": mv_missing,
                "gender_rate": "雄性百分比；null 表示无性别",
            },
        },
        "pokemon": pokemon,
        "abilities": abilities,
        "moves": moves,
    }

    os.makedirs(os.path.dirname(OUT), exist_ok=True)
    with open(OUT, "w", encoding="utf-8") as f:
        json.dump(payload, f, ensure_ascii=False, indent=1)

    size_kb = os.path.getsize(OUT) / 1024
    log("")
    log(f"完成 -> {OUT}  ({size_kb:.0f} KB)")
    log(f"  精灵 {len(pokemon)}  特性 {len(abilities)}  技能 {len(moves)}")
    log(f"  缺中文兜底为英文：精灵 {missing_zh}  特性 {ab_missing}  技能 {mv_missing}")


if __name__ == "__main__":
    main()
