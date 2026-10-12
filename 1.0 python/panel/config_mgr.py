# -*- coding: utf-8 -*-
"""配置管理：读写 config/sources.yaml 与 config/settings.yaml。

sources.yaml 采用整体 load/dump 管理（保留固定头注释）；
settings.yaml 采用顶层字段行级编辑，保留其余注释。
"""
import io
import os
import re
import yaml

try:
    from ruamel.yaml import YAML as _RuamelYAML
except ImportError:      # 没装就回退到普通 yaml（会丢注释，但功能不瘫）
    _RuamelYAML = None

PANEL_DIR = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(PANEL_DIR)
SOURCES_PATH = os.path.join(ROOT, "config", "sources.yaml")
SETTINGS_PATH = os.path.join(ROOT, "config", "settings.yaml")

SOURCES_HEADER = """\
# ============================================================
# 数据源注册表
# 新增 / 删除 / 启停数据源均可在面板「头目源」页操作。
# 停用数据源：enabled 改为 false（面板里点开关即可）。
# 调整优先级：改 priority，数字越小越优先。
# 不同源的语言、字段、秘传机差异，由各适配器内部自行处理。
# ============================================================

"""


# ---------------- sources.yaml ----------------
#
# 这个文件里有很多人工注释（例如「转发服务地址从环境变量读」这类关键说明），
# 用普通 yaml.safe_dump 重写会把注释全部吃掉，所以优先用 ruamel.yaml 的
# round-trip 模式：只改该改的值，注释、缩进、顺序原样保留。

def _ruamel():
    if _RuamelYAML is None:
        return None
    y = _RuamelYAML()
    y.preserve_quotes = True
    y.indent(mapping=2, sequence=4, offset=2)
    return y


def load_sources_doc() -> dict:
    """读 sources.yaml。ruamel 可用时返回带注释的往返对象。"""
    if not os.path.exists(SOURCES_PATH):
        return {"sources": []}
    y = _ruamel()
    if y is not None:
        try:
            with open(SOURCES_PATH, encoding="utf-8") as f:
                doc = y.load(f)
        except Exception:
            doc = None
        if doc is None:
            doc = {"sources": []}
        if "sources" not in doc:
            doc["sources"] = []
        return doc
    with open(SOURCES_PATH, encoding="utf-8") as f:
        doc = yaml.safe_load(f) or {}
    if "sources" not in doc:
        doc["sources"] = []
    return doc


def save_sources_doc(doc: dict):
    y = _ruamel()
    if y is not None:
        try:
            buf = io.StringIO()
            y.dump(doc, buf)
            text = buf.getvalue()
            # ruamel 会丢掉文件顶部的独立注释块，补回来（避免重复）
            if not text.lstrip().startswith("#"):
                text = SOURCES_HEADER + text
            with open(SOURCES_PATH, "w", encoding="utf-8") as f:
                f.write(text)
            return
        except Exception:
            pass  # 落回普通写法，宁可丢注释也不能写坏文件
    with open(SOURCES_PATH, "w", encoding="utf-8") as f:
        f.write(SOURCES_HEADER)
        yaml.safe_dump(doc, f, allow_unicode=True, sort_keys=False, default_flow_style=False)


def list_sources() -> list:
    return load_sources_doc().get("sources") or []


def get_source(name: str):
    for s in list_sources():
        if s.get("name") == name:
            return s
    return None


def _merge_map(dst, src):
    """把 src 合并进 dst（原地），保留 dst 已有 key 上的注释。"""
    if not isinstance(dst, dict) or not isinstance(src, dict):
        return src
    for k in list(dst.keys()):
        if k not in src:
            try:
                del dst[k]
            except Exception:
                pass
    for k, v in src.items():
        if isinstance(v, dict) and isinstance(dst.get(k), dict):
            _merge_map(dst[k], v)
        else:
            try:
                dst[k] = v
            except Exception:
                pass
    return dst


def upsert_source(scfg: dict):
    """按 name 新增或更新一个源。已存在时原地合并，尽量保留注释。"""
    doc = load_sources_doc()
    srcs = doc.get("sources") or []
    for i, s in enumerate(srcs):
        if s.get("name") == scfg.get("name"):
            _merge_map(s, scfg)
            break
    else:
        srcs.append(scfg)
    doc["sources"] = srcs
    save_sources_doc(doc)


def replace_source(old_name: str, scfg: dict) -> bool:
    """原地替换一个源（可用于改名），保持位置与注释不变。"""
    doc = load_sources_doc()
    srcs = doc.get("sources") or []
    for i, s in enumerate(srcs):
        if s.get("name") == old_name:
            _merge_map(s, scfg)
            doc["sources"] = srcs
            save_sources_doc(doc)
            return True
    return False


def remove_source(name: str) -> bool:
    doc = load_sources_doc()
    srcs = doc.get("sources") or []
    new = [s for s in srcs if s.get("name") != name]
    if len(new) == len(srcs):
        return False
    doc["sources"] = new
    save_sources_doc(doc)
    return True


def set_source_enabled(name: str, enabled: bool):
    s = get_source(name)
    if s is None:
        return False
    s["enabled"] = bool(enabled)
    upsert_source(s)
    return True


def set_source_priority(name: str, priority: int):
    s = get_source(name)
    if s is None:
        return False
    s["priority"] = int(priority)
    upsert_source(s)
    return True


def list_adapter_files() -> list:
    """src/sources/ 下可用的适配器 .py 文件（排除基类与 __init__）。"""
    d = os.path.join(ROOT, "src", "sources")
    if not os.path.isdir(d):
        return []
    out = []
    for fn in sorted(os.listdir(d)):
        if not fn.endswith(".py"):
            continue
        if fn in ("base.py", "__init__.py"):
            continue
        out.append(fn[:-3])
    return out


def describe_source(scfg: dict) -> dict:
    opts = scfg.get("options") or {}
    notes = []
    if opts.get("target"):
        notes.append(opts["target"])
    extra = opts.get("extra_lines") or {}
    on = [k for k, v in extra.items() if v]
    if on:
        notes.append("附加:" + "/".join(on))
    return {
        "name": scfg.get("name"),
        "adapter": scfg.get("adapter"),
        "enabled": bool(scfg.get("enabled", False)),
        "priority": int(scfg.get("priority", 100)),
        "note": " · ".join(notes),
        "options": opts,
    }


# ---------------- settings.yaml ----------------

def load_settings() -> dict:
    if not os.path.exists(SETTINGS_PATH):
        return {}
    with open(SETTINGS_PATH, encoding="utf-8") as f:
        return yaml.safe_load(f) or {}


def _set_top_field(path, field, new_val):
    with open(path, encoding="utf-8") as f:
        lines = f.read().splitlines(keepends=True)
    out, changed = [], False
    for line in lines:
        m = re.match(rf"^{re.escape(field)}:\s*(.*)$", line)
        if m and not changed:
            scalar = new_val if isinstance(new_val, (int, float, bool)) else str(new_val)
            out.append(f"{field}: {scalar}\n")
            changed = True
            continue
        out.append(line)
    if changed:
        with open(path, "w", encoding="utf-8") as f:
            f.write("".join(out))
    return changed


def set_setting_timezone(tz: str) -> bool:
    return _set_top_field(SETTINGS_PATH, "timezone", tz)


def set_setting_language(lang: str) -> bool:
    return _set_top_field(SETTINGS_PATH, "language", lang)


def get_push_language() -> str:
    """推送语言：优先 db 设置，其次 yaml。返回 zh/en/both。"""
    import panel.db as db
    v = db.get_kv("push_lang")
    if v in ("zh", "en", "both"):
        return v
    return load_settings().get("language") or "zh"


# ---------------- panel.env（环境变量） ----------------

def _env_path() -> str:
    return os.environ.get("PANEL_ENV_FILE", os.path.join(ROOT, "panel.env"))


def read_env_file() -> dict:
    """读 panel.env 成 dict（不解析 export 之类复杂语法）。"""
    p = _env_path()
    out = {}
    if not os.path.exists(p):
        return out
    with open(p, encoding="utf-8") as f:
        for line in f:
            line = line.strip()
            if not line or line.startswith("#") or "=" not in line:
                continue
            k, v = line.split("=", 1)
            out[k.strip()] = v.strip()
    return out


def update_env_file(updates: dict) -> bool:
    """更新 panel.env。保留既有注释与其他行，值变了才写回。"""
    p = _env_path()
    cur = read_env_file()
    changed = False
    for k, v in (updates or {}).items():
        if v is None:
            continue
        v = str(v).strip()
        if cur.get(k, "") != v:
            changed = True
    if not changed:
        return False

    lines = []
    if os.path.exists(p):
        with open(p, encoding="utf-8") as f:
            lines = f.read().splitlines()

    seen = set()
    out = []
    for line in lines:
        s = line.strip()
        if s and not s.startswith("#") and "=" in s:
            k = s.split("=", 1)[0].strip()
            if k in updates and updates[k] is not None:
                out.append(f"{k}={str(updates[k]).strip()}")
                seen.add(k)
                continue
        out.append(line)
    for k, v in (updates or {}).items():
        if v is None or k in seen:
            continue
        out.append(f"{k}={str(v).strip()}")

    if not any(l.strip() == "# Alpha 面板环境变量" for l in lines):
        out.insert(0, "# Alpha 面板环境变量（可在面板「系统配置」页修改）")
    with open(p, "w", encoding="utf-8") as f:
        f.write("\n".join(out).rstrip() + "\n")
    return True


def reload_core_config() -> bool:
    """清掉 src.core.config 的缓存，让新环境变量立即生效。"""
    try:
        import src.core.config as ccfg
        if hasattr(ccfg, "get_config"):
            fn = getattr(ccfg.get_config, "cache_clear", None)
            if fn:
                fn()
                return True
        for name in ("_CONFIG", "_config", "_cfg"):
            if hasattr(ccfg, name):
                setattr(ccfg, name, None)
                return True
    except Exception:
        pass
    return False
