# <span lang="zh">Pokemmo Alpha 头目监控面板</span> <span lang="en">Pokemmo Alpha Boss Monitor Panel</span>

<p align="center">
  <strong><span lang="zh">并发轮询 · 命中即播报 · 冲突投票 · 跨源去重 · 决策 · 分发</span></strong>
  <br>
  <span lang="en">Multi-source Monitor · Vote · Cross-source Dedup · Decide · Dispatch</span>
</p>

<p align="center">
  <a href="https://anexample.top/alpha"><strong><span lang="zh">在线演示</span> <span lang="en">Live Demo</span></strong></a>
  <br>
  <span lang="zh">推送 QQ 群：<strong>857597325</strong></span>
  <br>
  <span lang="en">Push-notification QQ group: <strong>857597325</strong></span>
</p>

---

## <span lang="zh">简介</span> <span lang="en">Introduction</span>

**Alpha 分发决策面板** 是一个 Pokemmo 头目（Alpha）自动监控与播报系统。它会同时盯着多个数据源，
**每轮并发轮询、任意一个源命中即立即播报**，仅在多源命中且报了不同头目时才按指纹投票裁决；
跨源去重避免重复推送，再由打法引擎生成打法推荐与自动评分，经可插拔的决策器分发，
最终送达你配置的各个渠道（WxPusher、Webhook 等）。

核心特性：

- **多源并发监控** —— 同时轮询多个数据源，一个挂了不影响其他
- **命中即播报 + 冲突投票** —— 任一轮只要有源命中就立即播报（无需等齐多源）；
  多源命中且报了不同头目时，才按内容指纹计票、得票多者胜出
- **跨源全局去重** —— 同一头目无论被几个源报到，只推一次
- **可插拔决策器 / 评估器** —— Rhai 沙箱脚本，面板上传即用
- **中英双语** —— 面板、推送内容均支持中文 / 英文 / 双语切换
- **Web 管理面板** —— 后端 + 原生 JS SPA，含调试、插件、渠道、定时、日志、系统配置等分页

---

## <span lang="zh">仓库结构</span> <span lang="en">Repository Layout</span>

| 目录 | 实现 | 状态 | 说明 |
|---|---|---|---|
| [`1.0 python/`](1.0%20python/) | Python | 历史归档 | 最初的实现，功能完整，保留作参考 |
| [`2.0 rust/`](2.0%20rust/) | Rust / Axum | **当前在用（推荐）** | 单二进制、常驻内存约 6 MB、无 Python 依赖 |

> 两个版本**共用同一套配置格式与数据库结构** —— 2.0 可以直接接管 1.0 的 `panel.db`，
> **不需要任何数据迁移**。这条承诺由 `alpha-store` 里的现网兼容性测试守着。

各版本的详细文档在各自目录下：

- [`1.0 python/README.md`](1.0%20python/README.md)
- [`2.0 rust/README.md`](2.0%20rust/README.md) ← **推荐从这里开始**

---

## <span lang="zh">快速开始（2.0 Rust）</span> <span lang="en">Quick Start</span>

```bash
cd "2.0 rust"

# 方式一：用仓库里已有的预编译二进制直接部署
sudo ./deploy/install.sh --from-dir bin --enable

# 方式二：从源码构建（glibc 动态链接）
sudo ./deploy/install.sh --enable

# 方式三：Alpine 等 musl 系统 —— 先编译静态版再部署
./deploy/build-musl.sh --copy bin
sudo ./deploy/install.sh --from-dir bin --enable --init openrc
```

部署脚本会**自动识别初始化系统**（systemd / OpenRC），也可以用 `--init` 强制指定。
数据与配置遵循「已存在就不动」，因此重复执行是安全的（幂等升级）。

---

## <span lang="zh">本次更新</span> <span lang="en">What's New</span>

### 一、Alpine / musl 部署支持（新增）

之前的部署链路只覆盖 systemd + glibc。本次补齐了 **Alpine（musl libc + OpenRC）** 这条路：

| 文件 | 变化 | 说明 |
|---|---|---|
| `2.0 rust/deploy/build-musl.sh` | **新增** | 构建 `x86_64-unknown-linux-musl` 静态二进制。默认的 `cargo build --release` 产出的是动态链接 glibc 的，直接拿到 Alpine 上会报 `Error loading shared library ld-linux-x86-64.so.2` —— 而这个报错看起来像「文件不存在」，很不直观 |
| `2.0 rust/deploy/pokemmo-alpha.openrc` | **新增** | OpenRC 服务脚本（基于 `supervise-daemon`），支持开机自启与崩溃自动拉起 |
| `2.0 rust/deploy/pokemmo-alpha.confd` | **新增** | OpenRC 侧的配置项（端口、数据目录、运行用户等） |
| `2.0 rust/deploy/install.sh` | 改造 | **自动识别 systemd / OpenRC** 并走对应安装路径；可用 `--init` 强制指定；数据与配置「已存在就不动」，重复执行安全 |
| `2.0 rust/deploy/README.md` | 改造 | 补充 Alpine 部署章节与两种 init 的差异说明 |

### 二、目录结构调整

- `1.0python/` → **`1.0 python/`**（数字与中文之间补一个空格，与 `2.0 rust/` 命名对齐）
- Rust 版从**仓库根目录整体迁入 `2.0 rust/`**，两个实现现在是平级的同级目录
- 根目录只保留：`README.md`（本文件）、`LICENSE`、`.gitignore` 以及两个版本目录
- `.gitignore` 拆为三层：仓库根放通用规则，两个版本各自维护自己的忽略规则

### 三、其他修正

- `2.0 rust/crates/alpha-server/src/routes/data/about.json`：面板「关于」页的仓库地址更新为本仓库
- `2.0 rust/crates/alpha-store/tests/live_compat.rs`：文档注释里出现的真实服务器地址与 SSH 端口已改为占位符

---

## <span lang="zh">关于 `bin/alpha-server`</span>

`2.0 rust/bin/alpha-server` 是**上一次发布的预编译产物**（glibc 动态链接，x86-64），
方便不想装 Rust 工具链的机器直接部署。

如果你在 **Alpine / 容器 / 其他 musl 系统**上部署，请重新编译静态版：

```bash
cd "2.0 rust"
./deploy/build-musl.sh --copy bin      # 覆盖为 musl 静态版
```

---

## <span lang="zh">许可证</span> <span lang="en">License</span>

MIT，见 [LICENSE](LICENSE)。
