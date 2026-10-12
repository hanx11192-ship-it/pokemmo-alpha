# -*- coding: utf-8 -*-
"""
数据源注册表。

按 config/sources.yaml 里的 adapter 名动态加载 src/sources/<adapter>.py，
找到其中 BaseSource 的子类并实例化。加新源不用改这里的代码。
"""

import importlib
import inspect
from typing import Type

from .base import BaseSource


def load_source_class(adapter: str) -> Type[BaseSource]:
    mod = importlib.import_module(f".{adapter}", package=__package__)
    for _, obj in inspect.getmembers(mod, inspect.isclass):
        if issubclass(obj, BaseSource) and obj is not BaseSource and obj.__module__ == mod.__name__:
            return obj
    raise ImportError(f"模块 {adapter} 里没有找到 BaseSource 的子类")


def create_source(adapter: str, options: dict = None, pokedex=None, logger=None,
                  sample_data: dict = None) -> BaseSource:
    cls = load_source_class(adapter)
    return cls(options=options, pokedex=pokedex, logger=logger, sample_data=sample_data)


__all__ = ["BaseSource", "load_source_class", "create_source"]
