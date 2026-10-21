#!/usr/bin/env bash
# 将 Rust release 二进制打包成标准 macOS .app bundle。
#
# 产出：target/release/Scripter.app/
# 用法：
#   ./scripts/build-macos-app.sh           # 编译 + 打包
#   ./scripts/build-macos-app.sh --sign    # 额外做 ad-hoc 签名（可自运行，绕过 Gatekeeper）
#   ./scripts/build-macos-app.sh --clean   # 先清理再编译
#
# 需要：cargo（nightly 或 stable 都行）。签名可选：需要 Xcode command line tools。
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
APP_NAME="Scripter"
BINARY_NAME="scripter"
BUNDLE_ID="com.scripter.app"
PLIST_SRC="$ROOT/packaging/macos/Info.plist"
ENTITLEMENTS_SRC="$ROOT/packaging/macos/entitlements.plist"

SIGN=0
CLEAN=0
for arg in "$@"; do
    case "$arg" in
        --sign)  SIGN=1 ;;
        --clean) CLEAN=1 ;;
        *) echo "未知参数：$arg"; exit 1 ;;
    esac
done

cd "$ROOT"

if [ "$CLEAN" = "1" ]; then
    echo "==> cargo clean"
    cargo clean
fi

echo "==> cargo build --release"
cargo build --release

# --- 组装 .app 目录结构 ---
APP_DIR="$ROOT/target/release/$APP_NAME.app"
CONTENTS="$APP_DIR/Contents"
MACOS_DIR="$CONTENTS/MacOS"
RESOURCES_DIR="$CONTENTS/Resources"

rm -rf "$APP_DIR"
mkdir -p "$MACOS_DIR" "$RESOURCES_DIR"

echo "==> 复制二进制"
cp "$ROOT/target/release/$BINARY_NAME" "$MACOS_DIR/$BINARY_NAME"
chmod +x "$MACOS_DIR/$BINARY_NAME"

echo "==> 复制 Info.plist"
cp "$PLIST_SRC" "$CONTENTS/Info.plist"
# 写入 PkgInfo（APPL + 四个空格，AppKit 应用的标配）
printf 'APPL????' > "$CONTENTS/PkgInfo"

# --- 可选：代码签名 ---
if [ "$SIGN" = "1" ]; then
    echo "==> ad-hoc 签名"
    # 用 - 做 ad-hoc 签名（self-signed，本地能跑，不能分发）
    codesign --force --deep \
        --sign - \
        --entitlements "$ENTITLEMENTS_SRC" \
        --options runtime \
        "$APP_DIR"
    echo "    签名完成。验证：codesign --verify --deep --strict $APP_DIR"
fi

echo ""
echo "==> 打包完成：$APP_DIR"
echo "    双击 Scripter.app 或 ./$APP_DIR/Contents/MacOS/$BINARY_NAME 运行。"
echo ""
echo "    首次运行需要系统设置授权："
echo "      - 辅助功能：系统设置 → 隐私与安全性 → 辅助功能"
echo "      - 屏幕录制：系统设置 → 隐私与安全性 → 屏幕录制"
echo "    （首次调用 typeStr / scanQr 时系统会自动弹窗请求）"
