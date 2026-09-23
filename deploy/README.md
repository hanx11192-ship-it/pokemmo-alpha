# 部署

一条安装脚本从零到 systemd 运行：拷文件 → 生成会话密钥 → 装 unit → 启动。
数据与配置**已存在就不动**，所以重复执行是安全的（幂等升级）。

## 快速开始

```bash
# 1. 安装 + 启动（用仓库里预编译的二进制）
sudo bash deploy/install.sh --from-dir bin --port 5703 --enable

# 2. 验证（另开终端）
curl -s localhost:5703/api/me    # 应返回 {"authenticated":false,...}

# 3. 浏览器打开 http://<你的IP>:5703/
#    数据库为空时，登录页输入的用户名密码即创建管理员账号
```

不想让脚本直接接管 systemd，就去掉 `--enable`：它会装好一切并打印手动启动命令，
确认面板能打开、登录正常后，再补执行一次带 `--enable` 的命令即可。

## 安装脚本做了什么

| 步骤 | 行为 |
|---|---|
| 构建或取用二进制 | `--from-dir` 直接用现成产物；不带则 `cargo build --release` |
| 建目录 | `/opt/pokemmo_alpha/{bin,web,data,config}` |
| 装二进制 | 先写临时名再原子 `mv`，运行中的服务也能安全覆盖 |
| 装前端资源 | `web/` 全部静态文件 |
| 数据库 | `data/panel.db` 不存在则提示首次启动自动建表；**存在则不动** |
| 配置 | `config/*.yaml` 已存在不动；不存在则装默认配置 |
| 会话密钥 | `panel.env` 没有 `PANEL_SECRET` 就自动生成（48 字节随机）；太弱就重新生成；权限 600 |
| systemd | 装 unit（按 `--prefix`/`--port` 改写路径）→ `enable --now` → 打印状态 |

## 部署布局

```
/opt/pokemmo_alpha/
├── bin/alpha-server        # 二进制
├── web/                    # 前端资源（index.html + static/）
├── data/
│   ├── panel.db            # SQLite（首次启动自动建表）
│   ├── pokedex.json
│   └── aliases.json
├── config/                 # settings/sources/rules.yaml
├── panel.env               # 环境变量（含 PANEL_SECRET，权限 600）
└── panel.log               # 服务日志
```

`PORT`、`HOST`、`DATA_DIR`、`CONFIG_DIR`、`ENV_FILE`、`ALPHA_WEB_ROOT` 都可用
环境变量覆盖；systemd unit 里已显式注入，换部署目录用 `install.sh --prefix` 即可。

## 会话密钥：`PANEL_SECRET`

会话签名密钥是面板安全模型的核心，**没有默认值**：

- 缺失、短于 32 字符、或命中已知弱密钥黑名单 → 服务**拒绝启动**并给出明确报错
- `install.sh` 会自动生成一个 48 字节随机密钥写进 `panel.env`
- 换密钥会让所有已登录会话立刻失效（预期行为）

手工生成：

```bash
echo "PANEL_SECRET=$(head -c 48 /dev/urandom | base64 | tr -d '\n')" >> /opt/pokemmo_alpha/panel.env
```

## 插件系统（Rhai）

决策器与评估器都是 [Rhai](https://rhai.rs) 沙箱脚本（`.rhai`）：有操作数、调用深度、
字符串长度上限，**没有**文件/网络/进程访问。内置的默认决策器与脚本队评估器随包预置并激活。

- 上传：面板 → 决策器 / 评估器 → 上传 `.rhai`
- 契约：决策器 `dispatch(boss, ctx) -> String`；评估器 `evaluate(boss)` 返回
  `#{ score, label, detail }` 或 `()`（不评估）
- 脚本抛异常不会中断轮询：决策器回退到官方引擎输出，评估器跳过评分行，都记日志

详细示例与可用函数清单见仓库根 `README.md` 的「接入指南」。

## 排障

**起来就退**

```bash
journalctl -u pokemmo-alpha -n 50 --no-pager
```

最常见的两条：

- `PANEL_SECRET 没有设置` / `太短` / `是已知的弱密钥` → 按上面的命令生成一个
- `无法打开数据库` → 检查 `data/panel.db` 的属主与权限

**界面白屏**

静态资源没找到。服务会返回一个明确说明问题的页面（不是纯白屏），
上面写了探测过的路径。修法：

```bash
ALPHA_WEB_ROOT=/opt/pokemmo_alpha/web /opt/pokemmo_alpha/bin/alpha-server
```

**点按钮全部报 400 且提示 CSRF**

前端垫片没加载。检查 `web/static/csrf-shim.js` 是否存在，
以及 `web/index.html` 里它的位置**在 `app.js` 之前**
（`app.js` 是立即执行的 IIFE，加载瞬间就发请求）。

**点「下载」弹出 401，文件下不来**

日志里应有找不到有效会话的记录。浏览器的 cookie 按（名字, 路径, 域）匹配，
**不区分端口** —— 如果浏览器里残留了同站点的多个 `session` cookie
（比如换过端口部署），它们会共存于同一条请求里。服务端会逐个验签、
采用有效的那一个，所以正常情况下不需要处理；若仍然 401，说明残留的
cookie 全部无效 —— 浏览器 F12 → 应用 → Cookie，删掉该站点多余的
`session` 条目，重新登录即可。

**端口被占**

```bash
ss -ltnp | grep 5703       # 看谁占着端口
systemctl stop <占用的服务>  # 或者换端口：install.sh --port 5704
```

## 升级

用新的 `alpha-server` 覆盖 `/opt/pokemmo_alpha/bin/alpha-server`，然后
`systemctl restart pokemmo-alpha`。也可以直接重跑安装脚本（幂等，
不会动已有的数据库、配置与密钥）。

## 验证清单

部署完过一遍：

- [ ] 打开 `http://<ip>:5703/` 能看到登录页（样式正常，不是白底黑字）
- [ ] 登录成功；首次部署时输入的账号已成为管理员
- [ ] 仪表盘数字正常（数据源数、决策器、日志量）
- [ ] 调试台能跑一次决策，评语行出现
- [ ] 某个渠道「测试推送」能收到消息
- [ ] 日志页能翻页、能筛选
- [ ] 上传一个 Rhai 插件，能启用、能激活
- [ ] 定时任务状态是 `running`
