#!/usr/bin/env bash
#
# 把 release 二进制打包成 macOS 的 .app 应用包，并装到 ~/Applications。
# Spotlight（Cmd+空格）只索引 .app，裸二进制搜不到，所以要走这一步。
#
# 用法：
#   scripts/bundle_macos.sh            # 装到 ~/Applications
#   scripts/bundle_macos.sh /Applications   # 或指定别的目录（可能要 sudo）
#
# 重新编译改动后，再跑一次即可原地覆盖更新。

set -euo pipefail

APP_NAME="Word Atlas"
BUNDLE_ID="com.sloan.wordatlas"
BIN_NAME="word_displayer"

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
DEST="${1:-$HOME/Applications}"
APP_DIR="$DEST/$APP_NAME.app"

echo "==> cargo build --release"
( cd "$ROOT" && cargo build --release )

echo "==> 组装 $APP_DIR"
rm -rf "$APP_DIR"
mkdir -p "$APP_DIR/Contents/MacOS" "$APP_DIR/Contents/Resources"
cp "$ROOT/target/release/$BIN_NAME" "$APP_DIR/Contents/MacOS/$BIN_NAME"

cat > "$APP_DIR/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleName</key><string>$APP_NAME</string>
    <key>CFBundleDisplayName</key><string>$APP_NAME</string>
    <key>CFBundleIdentifier</key><string>$BUNDLE_ID</string>
    <key>CFBundleExecutable</key><string>$BIN_NAME</string>
    <key>CFBundlePackageType</key><string>APPL</string>
    <key>CFBundleVersion</key><string>0.1.0</string>
    <key>CFBundleShortVersionString</key><string>0.1.0</string>
    <key>LSMinimumSystemVersion</key><string>10.15</string>
    <key>NSHighResolutionCapable</key><true/>
    <key>NSPrincipalClass</key><string>NSApplication</string>
</dict>
</plist>
PLIST

# ad-hoc 签名：本地自用足够，能少几次 Gatekeeper 拦截。未公证，首次仍可能
# 需要在「系统设置 > 隐私与安全性」里点一次「仍要打开」。
codesign --force --deep --sign - "$APP_DIR" >/dev/null 2>&1 || true

# 让 Spotlight 立刻把它索引进去，不用干等
mdimport "$APP_DIR" >/dev/null 2>&1 || true

echo "==> 完成：$APP_DIR"
echo "    Cmd+空格 搜 \"$APP_NAME\" 即可启动"
