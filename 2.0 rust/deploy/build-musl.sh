#!/usr/bin/env bash
#
# 构建 musl 静态二进制（供 Alpine 等 musl 系统使用）
# ============================================================================
#
# # 为什么需要单独的脚本
#
# 默认 `cargo build --release` 产出的是**动态链接 glibc** 的二进制，
# 拿到 Alpine（musl libc）上跑会直接报「not found」或
# 「Error loading shared library ld-linux-x86-64.so.2」——
# 而且这个报错很不直观，看起来像文件不存在。
#
# 本脚本构建的是 `x86_64-unknown-linux-musl` 目标的**静态**二进制：
# 不依赖宿主的 libc，放到任何 x86-64 Linux 上都能直接跑，
# 包括 Alpine、以及各种精简容器。
#
# # 前置条件
#
#   Debian/Ubuntu:  sudo apt install musl-tools
#   Alpine:         apk add musl-dev          # 本身就是 musl，无需交叉工具
#   Rust:           rustup target add x86_64-unknown-linux-musl
#
# # 用法
#
#   ./deploy/build-musl.sh                 # 构建到 target/<target>/release/
#   ./deploy/build-musl.sh --copy bin      # 顺手把产物复制到 bin/alpha-server
#
set -euo pipefail

TARGET="${TARGET:-x86_64-unknown-linux-musl}"
COPY_TO=""

while [[ $# -gt 0 ]]; do
  case "$1" in
    --copy)   COPY_TO="${2:?--copy 需要一个目录}"; shift 2 ;;
    --target) TARGET="${2:?--target 需要一个 rust target}"; shift 2 ;;
    -h|--help)
      sed -n '2,40p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'
      exit 0 ;;
    *) echo "未知参数: $1" >&2; exit 1 ;;
  esac
done

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO_ROOT"

command -v cargo >/dev/null 2>&1 \
  || { echo "✗ 找不到 cargo。先装 Rust：curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh" >&2; exit 1; }

# musl 目标的 C 编译（rusqlite 的 bundled sqlite3、ring 的汇编都要编 C）
if [[ "$TARGET" == *musl* ]]; then
  if ! command -v musl-gcc >/dev/null 2>&1 && [[ "$(uname -s)" != "Linux" || ! -f /etc/alpine-release ]]; then
    echo "✗ 找不到 musl-gcc。Debian/Ubuntu 上：sudo apt install musl-tools" >&2
    echo "  （Alpine 自带 musl-dev，可忽略此提示）" >&2
    exit 1
  fi
  # 明确告知 cc-rs 用哪个编译器 —— 交叉编译时它的自动探测不可靠，
  # 探测错了会得到一个「链接到了 glibc」的假静态二进制
  export CC_x86_64_unknown_linux_musl="${CC_x86_64_unknown_linux_musl:-musl-gcc}"
  export AR_x86_64_unknown_linux_musl="${AR_x86_64_unknown_linux_musl:-ar}"
fi

echo "==> 目标: $TARGET"
rustup target add "$TARGET" 2>/dev/null || true

echo "==> 构建 release（2 核机器约 10-15 分钟）"
cargo build --release --target "$TARGET" -p alpha-server

BIN="$REPO_ROOT/target/$TARGET/release/alpha-server"
[[ -f "$BIN" ]] || { echo "✗ 构建产物不存在: $BIN" >&2; exit 1; }

echo
echo "  ✓ 构建完成: $BIN  ($(du -h "$BIN" | cut -f1))"
file "$BIN" 2>/dev/null || true

# 验证「真静态」：ldd 对静态二进制会输出 "not a dynamic executable"
if command -v ldd >/dev/null 2>&1; then
  echo -n "  链接方式: "
  ldd "$BIN" 2>&1 | head -1 || true
fi

if [[ -n "$COPY_TO" ]]; then
  mkdir -p "$COPY_TO"
  install -m 0755 "$BIN" "$COPY_TO/alpha-server"
  echo "  ✓ 已复制到 $COPY_TO/alpha-server"
fi
