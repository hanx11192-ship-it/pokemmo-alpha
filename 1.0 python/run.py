#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""
朱雀 / 青龙 等「python3 路径/脚本.py」式定时面板入口。

项目用包结构组织（src/ 是包，内部大量相对导入 from .core / from .sources），
直接 `python3 src/main.py` 会报
    attempted relative import with no known parent package
而 `python3 -m src.main` 又要求先 cd 到项目根目录。

把本文件放在【项目根目录】，面板以 `python3 路径/run.py` 调用时：
    1. run.py 所在目录（项目根）自动进入 sys.path[0]
    2. `from src.main import main` 以包方式加载 src.main
    3. src/main.py 里的相对导入因此能正常解析
本地开发仍可用 `python3 -m src.main`，两种入口互不冲突。
"""
from src.main import main

if __name__ == "__main__":
    main()
