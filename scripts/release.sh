#!/usr/bin/env bash
# Developer ID 签名 + Apple 公证 + 可分发 DMG。
#
# 前置：
#   1) 钥匙串里有 Developer ID Application 证书。
#   2) notarytool 凭据已存为 keychain profile（默认 maceverything-notary）：
#      xcrun notarytool store-credentials maceverything-notary \
#        --apple-id <APPLE_ID> --team-id <TEAM_ID> --password <APP_SPECIFIC_PASSWORD>
#
# 用法：
#   bash scripts/release.sh
#   MACEEVERYTHING_NOTARY_PROFILE=my-profile bash scripts/release.sh
#
# 输出：
#   release/macEverything-<version>.zip   （公证用中间产物，保留便于排查）
#   release/macEverything-<version>.dmg   （分发产物，已签名/公证/staple）
#   release/macEverything-<version>.dmg.sha256
#   site/version.json                    （版本清单，部署站点时随 site/ 一起发布）
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "$HERE/.." && pwd)"
cd "$ROOT"

APP="release/macEverything.app"
# 本地/发布配置(.env.local 不入库):MCE_NOTARY_PROFILE 等
if [[ -f "$ROOT/.env.local" ]]; then
  set -a
  # shellcheck disable=SC1091
  source "$ROOT/.env.local"
  set +a
fi
PROFILE="${MACEEVERYTHING_NOTARY_PROFILE:-${MCE_NOTARY_PROFILE:-maceverything-notary}}"

VERSION="$(sed -n '/^\[workspace\.package\]/,/^\[/p' Cargo.toml \
  | sed -n 's/^version *= *"\(.*\)"/\1/p' | head -1)"
if [[ -z "$VERSION" ]]; then
  echo "错误:无法从 Cargo.toml 的 [workspace.package] 解析版本号" >&2
  exit 1
fi
ZIP="release/macEverything-${VERSION}.zip"
DMG="release/macEverything-${VERSION}.dmg"

IDENTITY="${MACEEVERYTHING_SIGN_IDENTITY:-}"
if [[ -z "$IDENTITY" ]]; then
  IDENTITY="$(security find-identity -v -p codesigning \
    | sed -n 's/.*"\(Developer ID Application: [^"]*\)".*/\1/p' | head -1)"
fi
if [[ -z "$IDENTITY" ]]; then
  echo "错误:未找到 Developer ID Application 证书（security find-identity -v -p codesigning）" >&2
  exit 1
fi
echo "== 发布 macEverything $VERSION =="
echo "签名身份: $IDENTITY"

# 0) 公证凭据：优先 keychain profile；钥匙串锁定时回落到环境变量
if xcrun notarytool history --keychain-profile "$PROFILE" >/dev/null 2>&1; then
  NOTARY_ARGS=(--keychain-profile "$PROFILE")
  echo "公证凭据: keychain profile $PROFILE"
elif [[ -n "${APPLE_ID:-}" && -n "${APPLE_TEAM_ID:-}" && -n "${APPLE_APP_SPECIFIC_PASSWORD:-}" ]]; then
  NOTARY_ARGS=(--apple-id "$APPLE_ID" --team-id "$APPLE_TEAM_ID" --password "$APPLE_APP_SPECIFIC_PASSWORD")
  echo "公证凭据: 环境变量（钥匙串不可用）"
else
  echo "错误:未找到可用公证凭据（keychain profile 或 APPLE_ID/APPLE_TEAM_ID/APPLE_APP_SPECIFIC_PASSWORD）" >&2
  exit 1
fi

# 1) 构建 + Developer ID 签名（hardened runtime + timestamp）
MACEEVERYTHING_SIGN_IDENTITY="$IDENTITY" bash scripts/bundle.sh
codesign --verify --strict --verbose=2 "$APP"
codesign --verify --strict --verbose=2 "$APP/Contents/MacOS/macEverything"
SIGN_INFO="$(codesign -dv --verbose=4 "$APP" 2>&1)"
if ! grep -q "flags=0x10000(runtime)" <<<"$SIGN_INFO"; then
  echo "错误:App 未启用 hardened runtime" >&2
  exit 1
fi

# 2) ZIP 公证 + staple App（保证离线首启）
rm -f "$ZIP"
ditto -c -k --keepParent "$APP" "$ZIP"
submit_and_require_accepted() {
  local file="$1"
  local out
  out="$(xcrun notarytool submit "$file" "${NOTARY_ARGS[@]}" --wait 2>&1)"
  printf '%s\n' "$out"
  if ! printf '%s\n' "$out" | grep -q "status: Accepted"; then
    echo "错误:公证未通过: $file" >&2
    exit 1
  fi
}
submit_and_require_accepted "$ZIP"
xcrun stapler staple "$APP"
xcrun stapler validate "$APP"
spctl --assess --type execute --verbose=4 "$APP"

# 3) 组装 DMG（App + /Applications 链接）
STAGE="$(mktemp -d "${TMPDIR:-/tmp}/mce-dmg.XXXXXX")"
trap 'rm -rf "$STAGE"' EXIT
# 用 ditto 而不是 cp -R：APFS clone 下 cp -R 可能让 hdiutil 在快照时久挂；
# ditto 完整拷贝并保留签名/扩展属性。
ditto "$APP" "$STAGE/macEverything.app"
ln -s /Applications "$STAGE/Applications"
sync
rm -f "$DMG"
hdiutil create -volname "macEverything" -srcfolder "$STAGE" -ov -format UDZO "$DMG"
codesign --force --timestamp --sign "$IDENTITY" "$DMG"

# 4) DMG 公证 + staple（挂在 DMG 上的分发路径也通过 Gatekeeper）
submit_and_require_accepted "$DMG"
xcrun stapler staple "$DMG"
xcrun stapler validate "$DMG"
spctl --assess --type open --context context:primary-signature --verbose=4 "$DMG"

# 5) 摘要
shasum -a 256 "$DMG" | tee "$DMG.sha256"

# 6) 生成官网版本清单（与 Cargo / 页面 / DMG 同一版本号）
VERSION_JSON="site/version.json"
mkdir -p site
cat > "$VERSION_JSON.tmp" <<JSON
{
  "version": "${VERSION}"
}
JSON
mv -f "$VERSION_JSON.tmp" "$VERSION_JSON"
echo "版本清单: $(pwd)/$VERSION_JSON"

echo "== 完成 =="
echo "DMG: $(cd "$(dirname "$DMG")" && pwd)/$(basename "$DMG")"
echo "SHA256: $DMG.sha256"
