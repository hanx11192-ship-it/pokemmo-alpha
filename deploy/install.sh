#!/usr/bin/env bash
#
# Alpha 面板部署脚本
# ============================================================================
#
# 一条脚本完成：构建（或使用预编译二进制）→ 安装到 /opt/pokemmo_alpha →
# 生成会话密钥 → 安装 systemd unit 并启动。
#
#   - 数据、配置「已存在就不动」：重复执行是安全的（幂等升级）
#   - 任何一步失败都立刻退出（set -euo pipefail），不会留下半截状态
#
# 用法：
#   ./deploy/install.sh --from-dir bin --enable   # 用仓库里预编译的二进制部署并启动
#   ./deploy/install.sh --enable                  # 从源码构建后部署并启动
#   ./deploy/install.sh                           # 只安装不启用（先手动验证）
#   ./deploy/install.sh --prefix /opt/xxx         # 换部署目录
#   ./deploy/install.sh --port 5704               # 换监听端口（默认 5703）
#   ./deploy/install.sh --from-dir /path          # 指定已有构建产物所在目录
#
set -euo pipefail

# ---------------------------------------------------------------- 默认参数
PREFIX="/opt/pokemmo_alpha"
SERVICE_NAME="pokemmo-alpha"
OLD_SERVICE_NAME="pokemmo-panel"
PORT="5703"
ENABLE=0
FROM_DIR=""

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

# ---------------------------------------------------------------- 输出工具
# 颜色只在终端里用；重定向到文件时关掉，免得日志里全是转义符
if [[ -t 1 ]]; then
  RED=$'\033[31m'; GREEN=$'\033[32m'; YELLOW=$'\033[33m'
  BLUE=$'\033[34m'; BOLD=$'\033[1m'; RESET=$'\033[0m'
else
  RED=""; GREEN=""; YELLOW=""; BLUE=""; BOLD=""; RESET=""
fi

info()  { printf '%s==>%s %s\n' "$BLUE" "$RESET" "$*"; }
ok()    { printf '%s  ✓%s %s\n' "$GREEN" "$RESET" "$*"; }
warn()  { printf '%s  !%s %s\n' "$YELLOW" "$RESET" "$*" >&2; }
die()   { printf '%s  ✗ %s%s\n' "$RED" "$*" "$RESET" >&2; exit 1; }

usage() {
  # 只截取顶部注释块（从第 3 行到 `set -euo pipefail` 之前），
  # 用结束标记定位而不是写死行号 —— 否则改注释时行号会漂。
  awk 'NR>2 && /^set -euo pipefail/ {exit} NR>2 {print}' "${BASH_SOURCE[0]}" \
    | sed 's/^# \{0,1\}//'
  exit 0
}

# ---------------------------------------------------------------- 解析参数
while [[ $# -gt 0 ]]; do
  case "$1" in
    --enable)   ENABLE=1; shift ;;
    --prefix)   PREFIX="${2:?--prefix 需要一个路径}"; shift 2 ;;
    --port)     PORT="${2:?--port 需要一个端口号}"; shift 2 ;;
    --from-dir) FROM_DIR="${2:?--from-dir 需要一个路径}"; shift 2 ;;
    -h|--help)  usage ;;
    *)          die "未知参数: $1（用 --help 看用法）" ;;
  esac
done

[[ "$PORT" =~ ^[0-9]+$ ]] && [[ "$PORT" -ge 1 ]] && [[ "$PORT" -le 65535 ]] \
  || die "端口不合法: $PORT"

[[ "$(id -u)" == "0" ]] || die "需要 root 权限（要写 $PREFIX 和 /etc/systemd/system）"

info "部署目录: ${BOLD}$PREFIX${RESET}"
info "仓库根目录: $REPO_ROOT"

# ============================================================================
# 第 1 步：构建
# ============================================================================
# 单独一个函数，是为了让 --from-dir 那条路径能跳过它 —— 在 2 核机器上
# 全量构建要十几分钟，而 CI 里通常已经构建好了。

BIN=""
build() {
  if [[ -n "$FROM_DIR" ]]; then
    info "跳过构建，使用已有产物"
    BIN="$FROM_DIR/alpha-server"
    [[ -f "$BIN" ]] || die "$FROM_DIR/alpha-server 不存在"
    return
  fi

  command -v cargo >/dev/null 2>&1 \
    || die "找不到 cargo。装一个：curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh"

  info "构建 release 版本（2 核机器上大约 10-15 分钟）"
  ( cd "$REPO_ROOT" && cargo build --release -p alpha-server )

  BIN="$REPO_ROOT/target/release/alpha-server"
  [[ -f "$BIN" ]] || die "构建产物不存在: $BIN"
  ok "构建完成: $(du -h "$BIN" | cut -f1)"
}
build

# ============================================================================
# 第 2 步：建目录
# ============================================================================
# 注意 data/ 与 config/ 是**已存在就不动**的 —— 它们里面是现网的
# 数据库和配置，覆盖掉等于把用户数据删了。

info "准备目录"
mkdir -p "$PREFIX/bin" "$PREFIX/data" "$PREFIX/config" "$PREFIX/web/static"

# ============================================================================
# 第 3 步：装二进制与前端资源
# ============================================================================

info "安装二进制"
# 先装到临时名再 mv：如果目标正被运行中的进程占用，直接 cp 会得到
# 「Text file busy」；mv 是原子的，不会出现半个二进制。
install -m 0755 "$BIN" "$PREFIX/bin/.alpha-server.new"
mv -f "$PREFIX/bin/.alpha-server.new" "$PREFIX/bin/alpha-server"
ok "$PREFIX/bin/alpha-server"

info "安装前端资源"
# 前端 9 个静态文件 + CSRF 垫片 + 外壳页。这里**必须**逐个列出来抄，
# 不能用 rsync --delete 之类的：PREFIX 下还可能有用户自己放的东西。
[[ -f "$REPO_ROOT/web/index.html" ]] || die "仓库里缺少 web/index.html"
cp -f "$REPO_ROOT/web/index.html" "$PREFIX/web/index.html"
for f in "$REPO_ROOT"/web/static/*; do
  cp -f "$f" "$PREFIX/web/static/"
done
ok "$PREFIX/web/（$(ls -1 "$PREFIX/web/static" | wc -l) 个静态资源）"

# ============================================================================
# 第 4 步：数据与配置
# ============================================================================

# -------------------------------------------------------------- 数据库
if [[ -f "$PREFIX/panel/panel.db" && ! -f "$PREFIX/data/panel.db" ]]; then
  # 更早版本把库放在 panel/panel.db；本版按 DATA_DIR 找 data/panel.db。
  # 表结构一致，检测到就直接搬过来，用户、渠道、日志全保留。
  info "从旧位置搬运数据库"
  cp -a "$PREFIX/panel/panel.db" "$PREFIX/data/panel.db"
  ok "panel.db（$(sqlite3 "$PREFIX/data/panel.db" \
        'SELECT COUNT(*) FROM users' 2>/dev/null || echo '?') 个用户）"
elif [[ -f "$PREFIX/data/panel.db" ]]; then
  ok "数据库已存在，跳过（$PREFIX/data/panel.db）"
else
  warn "没找到现成的 panel.db —— 服务首次启动会自动建一个空库，登录页输入的账号即成为管理员"
fi

# -------------------------------------------------------------- 配置
for f in settings.yaml sources.yaml rules.yaml; do
  if [[ -f "$PREFIX/config/$f" ]]; then
    ok "配置已存在: config/$f（未改动）"
  elif [[ -f "$REPO_ROOT/config/$f" ]]; then
    cp -f "$REPO_ROOT/config/$f" "$PREFIX/config/$f"
    ok "安装默认配置: config/$f"
  fi
done

# -------------------------------------------------------------- 图鉴数据
for f in pokedex.json aliases.json; do
  if [[ -f "$PREFIX/data/$f" ]]; then
    continue
  fi
  if [[ -f "$REPO_ROOT/data/$f" ]]; then
    cp -f "$REPO_ROOT/data/$f" "$PREFIX/data/$f"
    ok "安装数据文件: data/$f"
  fi
done

# ============================================================================
# 第 5 步：会话密钥
# ============================================================================
#
# 会话密钥是本面板安全模型的核心：密钥缺失、太短或命中已知弱密钥黑名单
# 时，服务会**拒绝启动**（绝不回退到某个弱默认值）。
#
# 所以这里：**没有就生成一个**，绝不留空。

ENV_FILE="$PREFIX/panel.env"
touch "$ENV_FILE"

if grep -qE '^[[:space:]]*PANEL_SECRET[[:space:]]*=' "$ENV_FILE" 2>/dev/null; then
  EXISTING="$(grep -E '^[[:space:]]*PANEL_SECRET[[:space:]]*=' "$ENV_FILE" \
              | head -1 | cut -d= -f2- | tr -d '[:space:]')"
  if [[ ${#EXISTING} -ge 32 && "$EXISTING" != "alpha-panel-dev-secret" ]]; then
    ok "PANEL_SECRET 已配置（${#EXISTING} 字符）"
  else
    warn "PANEL_SECRET 不合格（太短或是已知的弱密钥）—— 重新生成"
    NEW_SECRET="$(head -c 48 /dev/urandom | base64 | tr -d '\n')"
    # 用 # 当分隔符避免密钥里的 / 破坏 sed 表达式
    sed -i "s#^[[:space:]]*PANEL_SECRET[[:space:]]*=.*#PANEL_SECRET=${NEW_SECRET}#" "$ENV_FILE"
    ok "已重新生成 PANEL_SECRET（${#NEW_SECRET} 字符）"
  fi
else
  NEW_SECRET="$(head -c 48 /dev/urandom | base64 | tr -d '\n')"
  {
    echo ""
    echo "# 会话签名密钥 —— Rust 版必需，没有默认值。"
    echo "# 由 deploy/install.sh 于 $(date '+%Y-%m-%d %H:%M:%S') 自动生成。"
    echo "# 换掉它会让所有已登录的会话立刻失效（这是预期行为）。"
    echo "PANEL_SECRET=${NEW_SECRET}"
  } >> "$ENV_FILE"
  ok "已生成 PANEL_SECRET（${#NEW_SECRET} 字符）"
fi

# panel.env 的权限收紧：里面有会话密钥，不该是 0644
chmod 600 "$ENV_FILE"
ok "panel.env 权限设为 600"

# 顺带说明：旧配置里的 `PROXY_KEY` 是废弃键（对应早已下线的 PHP 中转）。
# 本版没有任何代码读它 —— 不搬运，留着也无副作用。

# ============================================================================
# 第 5.5 步：清理旧数据库里的遗留评估器行
# ============================================================================
#
# 更早版本数据库里可能残留指向已不存在文件的评估器登记行。这里：
#
#   1. 删掉这类登记行（磁盘上的脚本文件不删，只是不再指向它们）
#   2. 如果删掉后没有任何「激活中」的评估器，把内置评估器设为启用+激活
#      —— 推送报告的评语行为保持不变
#
# 幂等：行不存在时两条 SQL 都是空操作。
# 服务启动时的迁移会把内置行文件名 .py → .rhai，所以这里
# 两种文件名都匹配，先后顺序无关。

if [[ -f "$PREFIX/data/panel.db" ]] && command -v sqlite3 >/dev/null 2>&1; then
  info "清理遗留评估器登记行"
  sqlite3 "$PREFIX/data/panel.db" <<'SQL'
DELETE FROM evaluators WHERE filename = 'user_jbdui.py' AND is_builtin = 0;
UPDATE evaluators SET enabled = 1, active = 1
  WHERE is_builtin = 1
    AND filename IN ('script_team.rhai', 'script_team.py')
    AND NOT EXISTS (SELECT 1 FROM evaluators WHERE active = 1);
SQL
  ok "遗留登记行已清理；无激活评估器时由内置评估器接管"
elif [[ ! -f "$PREFIX/data/panel.db" ]]; then
  : # 全新库没有遗留行，无事可做
else
  warn "没有 sqlite3 命令，跳过遗留行清理 —— 指向不存在文件的行可在插件页手动停用"
fi

# ============================================================================
# 第 6 步：systemd
# ============================================================================

if [[ "$ENABLE" != "1" ]]; then
  echo
  ok "安装完成（未启用服务）"
  cat <<EOF

  先手动验证一下：

      cd $PREFIX
      ALPHA_WEB_ROOT=$PREFIX/web \\
      DATA_DIR=$PREFIX/data CONFIG_DIR=$PREFIX/config \\
      ENV_FILE=$PREFIX/panel.env PORT=$PORT \\
      $PREFIX/bin/alpha-server

  另开一个终端：

      curl -s localhost:$PORT/api/me

  确认没问题后，再装 unit 并启动：

      $0 --enable --prefix $PREFIX --port $PORT

  若机器上还在跑别的同端口服务，切换时会提示处理（端口冲突，两个不能同时跑）。

EOF
  exit 0
fi

info "安装 systemd unit"
UNIT_SRC="$REPO_ROOT/deploy/${SERVICE_NAME}.service"
[[ -f "$UNIT_SRC" ]] || die "找不到 unit 模板: $UNIT_SRC"

# unit 里的路径是写死的 /opt/pokemmo_alpha、端口写死 5703。换 prefix / port 时
# 同步替换，否则服务启动了但读的是另一个目录（或占了别人的端口）——
# 那会是个很难看出来的错。
sed -e "s#/opt/pokemmo_alpha#${PREFIX}#g" \
    -e "s/^Environment=PORT=5703$/Environment=PORT=${PORT}/" \
    "$UNIT_SRC" > "/etc/systemd/system/${SERVICE_NAME}.service"
ok "/etc/systemd/system/${SERVICE_NAME}.service（端口 $PORT）"

systemctl daemon-reload

# 旧的 pokemmo-panel 服务还在跑时停掉它：端口冲突，两个不能同时跑
if systemctl is-active --quiet "$OLD_SERVICE_NAME" 2>/dev/null; then
  info "停止旧服务（$OLD_SERVICE_NAME）"
  systemctl stop "$OLD_SERVICE_NAME"
  systemctl disable "$OLD_SERVICE_NAME" >/dev/null 2>&1 || true
  ok "旧服务已停止（unit 文件保留，想切回就 systemctl start $OLD_SERVICE_NAME）"
fi

systemctl enable --now "$SERVICE_NAME"

# systemd 启动是异步的，给它两秒再检查状态
sleep 2
if systemctl is-active --quiet "$SERVICE_NAME"; then
  ok "服务已启动"
  echo
  systemctl --no-pager status "$SERVICE_NAME" | head -12
  echo
  printf '%s常用命令%s\n' "$BOLD" "$RESET"
  cat <<EOF
  看状态    systemctl status $SERVICE_NAME
  看日志    journalctl -u $SERVICE_NAME -f     （或 tail -f $PREFIX/panel.log）
  重启      systemctl restart $SERVICE_NAME
  切回旧服务 systemctl stop $SERVICE_NAME && systemctl start $OLD_SERVICE_NAME
EOF
else
  echo
  warn "服务启动失败。看日志："
  echo
  journalctl -u "$SERVICE_NAME" --no-pager -n 40
  echo
  die "启动失败 —— 上面的日志里应该有具体原因（最常见的是 PANEL_SECRET 不合格）"
fi
