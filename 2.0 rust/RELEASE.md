# Pokemmo Alpha 面板 · 发布包（v2.0.0）

本包 = 完整源码 + 已编译的二进制 `bin/alpha-server`，二进制可直接部署，无需重新编译。

## 二进制信息

- 文件：`bin/alpha-server`（约 14.2 MB，已 strip）
- 架构：x86-64，动态链接 glibc —— 在 Ubuntu 22.04 / 24.04、Debian 12 上直接可跑
- 构建环境：Rust stable，release profile
- 完整性校验：`sha256sum bin/alpha-server`

## 目录结构

```
├── bin/alpha-server      ← 编译好的二进制（部署用它）
├── crates/               ← Rust 源码（workspace：alpha-server / scheduler / sources /
│                            strategy / plugin / notify / store / core，含全量回归测试）
├── web/                  ← 前端（原生 JS SPA，由二进制直接托管）
├── config/               ← rules.yaml / settings.yaml / sources.yaml
├── data/                 ← 静态资源（pokedex.json / aliases.json）
├── deploy/               ← install.sh + systemd unit + 详细部署文档（deploy/README.md）
├── docs/screenshots/     ← 面板截图
├── fixtures/             ← 回归测试基准数据
├── Cargo.toml / .lock    ← 源码编译入口（改了代码重新编译用：cargo build --release）
└── 1.0python/            ← 1.0 版（Python/Flask 初版）完整归档，仅供查阅，不再维护
```

## 快速部署（三步）

详细说明（手工部署、环境变量、数据持久化、排障清单）见 `deploy/README.md` 与仓库根 `README.md`。

```bash
# 1) 进入项目目录
cd pokemmo-alpha

# 2) 跑安装脚本（自动：拷二进制与前端资源、装默认配置、
#    生成 PANEL_SECRET、装 systemd unit）
sudo bash deploy/install.sh --from-dir bin --port 5703 --enable

# 3) 脚本会自动启动服务并打印状态；浏览器打开
#    http://<你的IP>:5703/ ，登录页输入用户名密码即创建管理员
```

手工部署（不用脚本）时的最小要求：

- 二进制放 `/opt/pokemmo_alpha/bin/alpha-server`（chmod +x）
- `web/`、`config/`、`data/` 拷到 `/opt/pokemmo_alpha/` 对应位置
- `panel.env` 里必须有一行 `PANEL_SECRET=<openssl rand -base64 48 生成>`，权限 600
- systemd unit 见 `deploy/pokemmo-alpha.service`（按需改路径/端口）

## 首次初始化

- 服务首次启动会自动在 `data/` 下建好 SQLite 数据库（用户、渠道、插件、日志表）
- 数据库为空时，登录页输入的**第一个账号自动成为管理员**
- 登录后依次配置：「分发渠道」→「头目源」→「定时任务」启动轮询
- 内置决策器与评估器（Rhai 脚本）已预置并激活，无需额外配置即可产出带评分的报文

## 从源码构建

```bash
cargo build --release          # 产物 target/release/alpha-server
cargo test --workspace         # 全量回归（约 400 条）
```
