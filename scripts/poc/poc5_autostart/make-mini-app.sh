#!/usr/bin/env bash
# PoC-5：编译 Swift 二进制 + 组最小 .app + ad-hoc 签名
# 用法: bash make-mini-app.sh [目标.app 路径]   （默认 /tmp/poc5.app，可用 POC5_APP 覆盖）
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
APP_DIR="${1:-${POC5_APP:-/tmp/poc5.app}}"
BUNDLE_ID="com.example.maceverything.poc5"
EXE_NAME="poc5-binary"

echo "== 1) swiftc 编译 $EXE_NAME =="
mkdir -p "$HERE/build"
swiftc "$HERE/poc5.swift" -o "$HERE/build/$EXE_NAME" -framework ServiceManagement
echo "编译产物: $HERE/build/$EXE_NAME"

echo "== 2) 组装最小 .app: $APP_DIR =="
rm -rf "$APP_DIR"
mkdir -p "$APP_DIR/Contents/MacOS"
cp "$HERE/build/$EXE_NAME" "$APP_DIR/Contents/MacOS/$EXE_NAME"
chmod +x "$APP_DIR/Contents/MacOS/$EXE_NAME"
cat > "$APP_DIR/Contents/Info.plist" <<'PLIST'
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>CFBundleExecutable</key><string>poc5-binary</string>
	<key>CFBundleIdentifier</key><string>com.example.maceverything.poc5</string>
	<key>CFBundleName</key><string>poc5</string>
	<key>CFBundleDisplayName</key><string>poc5</string>
	<key>CFBundlePackageType</key><string>APPL</string>
	<key>CFBundleShortVersionString</key><string>0.1.0</string>
	<key>CFBundleVersion</key><string>1</string>
	<key>LSMinimumSystemVersion</key><string>13.0</string>
	<key>LSUIElement</key><true/>
</dict>
</plist>
PLIST
echo "Info.plist 写好（CFBundleIdentifier=$BUNDLE_ID, LSUIElement=true）"

echo "== 3) ad-hoc 签名 =="
codesign --force --deep -s - "$APP_DIR"
echo "-- 验签 --"
codesign --verify --deep --verbose=2 "$APP_DIR"
echo "-- 签名详情 --"
codesign -dvvv "$APP_DIR" 2>&1 | grep -E "Identifier|Signature|CDHash|TeamIdentifier|Format" || true

echo "== 完成: $APP_DIR =="
