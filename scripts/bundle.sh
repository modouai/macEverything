#!/usr/bin/env bash
# 组装 macEverything.app(Task 20;PoC-5 组包脚本 make-mini-app.sh 的正式版)。
#
# 结构:release/macEverything.app/Contents/
#   MacOS/macEverything-app   App 本体(CFBundleExecutable;LSUIElement 菜单栏应用)
#   MacOS/macEverything       引擎 CLI(随包;App 定位三档①「同目录」即命中它)
#   Resources/AppIcon.icns    应用图标(吉祥物);Info.plist 的 CFBundleIconFile 指向它
#   Resources/fonts/OFL.txt   内置字体(notocjk:Noto Sans SC/KR)的 OFL 1.1 许可文本
#   Resources/fonts/OFL-NotoSansSymbols.txt
#                             内置字体(notofonts:Symbols/Symbols 2)的 OFL 1.1
#                             许可文本(含版权头;OFL 要求随分发提供,缺失在
#                             自检段硬失败)
#   Info.plist                bundle 标识 = 自启条目身份键(PoC-5 坑②)
#
# 必须签名(PoC-5 硬门槛:未签名 arm64 进程直接 SIGKILL;签名无效时
# SMAppService.register 报 kSMErrorInvalidSignature)。先签内层 CLI 再签外层 bundle。
#
# 默认 ad-hoc 签名（本地开发）。设置 MACEEVERYTHING_SIGN_IDENTITY 时走 Developer ID
# + hardened runtime + timestamp（公证要求）；可选 MACEEVERYTHING_ENTITLEMENTS 指定
# entitlements 文件。
#
# 用法:
#   bash scripts/bundle.sh                         # ad-hoc 开发包
#   MACEEVERYTHING_SIGN_IDENTITY="Developer ID Application: ..." bash scripts/bundle.sh
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "$HERE/.." && pwd)"
cd "$ROOT"

APP="release/macEverything.app"
BUNDLE_ID="com.example.maceverything"    # 与 crates/app/src/autostart.rs::BUNDLE_ID 同值
EXE="macEverything-app"
CLI="macEverything"
SIGN_IDENTITY="${MACEEVERYTHING_SIGN_IDENTITY:--}"
ENTITLEMENTS="${MACEEVERYTHING_ENTITLEMENTS:-}"

# 1) 版本号:根 Cargo.toml 的 [workspace.package].version(单一真相)
VERSION="$(sed -n '/^\[workspace\.package\]/,/^\[/p' Cargo.toml \
  | sed -n 's/^version *= *"\(.*\)"/\1/p' | head -1)"
if [[ -z "$VERSION" ]]; then
  echo "错误:无法从 Cargo.toml 的 [workspace.package] 解析版本号" >&2
  exit 1
fi
echo "== 1/4 版本号: $VERSION([workspace.package]) =="

# 2) 构建两个二进制(仅 app 与 cli;engine/protocol/client 为其依赖自动带入)
echo "== 2/4 cargo build --release =="
cargo build --release -p "$EXE" -p "$CLI"

# 3) 组装 bundle
echo "== 3/4 组装 $APP =="
rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
cp "target/release/$EXE" "$APP/Contents/MacOS/$EXE"
cp "target/release/$CLI" "$APP/Contents/MacOS/$CLI"
chmod +x "$APP/Contents/MacOS/$EXE" "$APP/Contents/MacOS/$CLI"
cp "assets/AppIcon.icns" "$APP/Contents/Resources/AppIcon.icns"
# OFL 1.1 分发合规:许可文本必须随字体一起分发(内置覆盖字体全览见
# crates/app/src/ui/mod.rs BUNDLED_FONTS)。release.sh 的组装同样只经本脚本,
# ditto 打包时随 bundle 带走;缺失在文末自检段硬失败。
mkdir -p "$APP/Contents/Resources/fonts"
cp "assets/fonts/OFL.txt" "$APP/Contents/Resources/fonts/OFL.txt"
cp "assets/fonts/OFL-NotoSansSymbols.txt" "$APP/Contents/Resources/fonts/OFL-NotoSansSymbols.txt"
cat > "$APP/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>CFBundleExecutable</key><string>${EXE}</string>
	<key>CFBundleIdentifier</key><string>${BUNDLE_ID}</string>
	<key>CFBundleName</key><string>macEverything</string>
	<key>CFBundleDisplayName</key><string>macEverything</string>
	<key>CFBundlePackageType</key><string>APPL</string>
	<key>CFBundleIconFile</key><string>AppIcon.icns</string>
	<key>CFBundleShortVersionString</key><string>${VERSION}</string>
	<key>CFBundleVersion</key><string>${VERSION}</string>
	<key>LSMinimumSystemVersion</key><string>13.0</string>
	<key>LSUIElement</key><true/>
	<key>NSHighResolutionCapable</key><true/>
</dict>
</plist>
PLIST

# 4) 签名(先内层 CLI,再外层 bundle;--deep 已弃用,不用)
echo "== 4/4 签名: $SIGN_IDENTITY =="
if [[ "$SIGN_IDENTITY" == "-" ]]; then
  codesign --force -s - "$APP/Contents/MacOS/$CLI"
  codesign --force -s - "$APP"
else
  SIGN_ARGS=(--force --options runtime --timestamp --sign "$SIGN_IDENTITY")
  if [[ -n "$ENTITLEMENTS" ]]; then
    SIGN_ARGS+=(--entitlements "$ENTITLEMENTS")
  fi
  codesign "${SIGN_ARGS[@]}" "$APP/Contents/MacOS/$CLI"
  codesign "${SIGN_ARGS[@]}" "$APP"
fi

# —— 自检(产物实证,失败即退出非零)——
echo "-- plutil -lint Info.plist --"
plutil -lint "$APP/Contents/Info.plist"
echo "-- Resources/fonts/(OFL 分发合规) --"
diff -q "assets/fonts/OFL.txt" "$APP/Contents/Resources/fonts/OFL.txt"
diff -q "assets/fonts/OFL-NotoSansSymbols.txt" "$APP/Contents/Resources/fonts/OFL-NotoSansSymbols.txt"
echo "-- codesign --verify --strict(app) --"
codesign --verify --strict --verbose=2 "$APP"
echo "-- codesign --verify --strict(CLI) --"
codesign --verify --strict --verbose=2 "$APP/Contents/MacOS/$CLI"
echo "-- codesign -dv(app) --"
codesign -dv --verbose=4 "$APP" 2>&1 | grep -E "Identifier|Format|Signature|Authority|TeamIdentifier|Timestamp|flags=" || true
echo "-- 结构 --"
ls -l "$APP/Contents" "$APP/Contents/MacOS"
if [[ "$SIGN_IDENTITY" == "-" ]]; then
  echo "== 完成: $APP(ad-hoc 签名;版本 $VERSION) =="
else
  echo "== 完成: $APP(Developer ID 签名;版本 $VERSION) =="
fi
