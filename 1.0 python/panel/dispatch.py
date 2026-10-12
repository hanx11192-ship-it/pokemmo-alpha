# -*- coding: utf-8 -*-
"""分发器动态加载器：从 panel/dispatchers/ 目录按文件名加载 .py 模块。"""
import importlib.util
import os

DISP_DIR = os.path.join(os.path.dirname(os.path.abspath(__file__)), "dispatchers")


def load_module(filename: str):
    """按文件名加载分发器模块，必须存在 dispatch(boss, ctx) 函数。"""
    path = os.path.join(DISP_DIR, filename)
    if not os.path.exists(path):
        return None
    spec = importlib.util.spec_from_file_location("disp_" + filename[:-3], path)
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    if not hasattr(mod, "dispatch") or not callable(mod.dispatch):
        return None
    return mod


def run_dispatcher(filename: str, boss, ctx: dict) -> str:
    mod = load_module(filename)
    if mod is None:
        raise RuntimeError(f"分发器不可用: {filename}")
    return mod.dispatch(boss, ctx)
