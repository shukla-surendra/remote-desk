#!/usr/bin/env bash
# Builds "Remote Desk.app" and a drag-to-Applications .dmg in dist/.
#
#   scripts/package-macos.sh              # universal (arm64 + x86_64) if both targets are installed
#   ARCH=native scripts/package-macos.sh  # host architecture only (faster)
#
# Signing: ad-hoc by default (runs locally; on other Macs Gatekeeper asks once — see README).
# For distribution set SIGN_IDENTITY="Developer ID Application: …" and optionally
# NOTARY_PROFILE=<xcrun notarytool keychain profile> to notarize + staple the dmg.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"
APP_NAME="Remote Desk"
VERSION="$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)"
DIST="$ROOT/dist"
APP="$DIST/$APP_NAME.app"
DMG="$DIST/RemoteDesk-$VERSION.dmg"
SIGN_IDENTITY="${SIGN_IDENTITY:--}"

[[ "$(uname)" == "Darwin" ]] || { echo "macOS only" >&2; exit 1; }

echo "==> Building web viewer"
(cd web && npm ci --no-audit --no-fund --silent && npm run build --silent)

echo "==> Building agent ($VERSION)"
targets=()
if [[ "${ARCH:-universal}" == "universal" ]] && rustup target list --installed | grep -q x86_64-apple-darwin \
   && rustup target list --installed | grep -q aarch64-apple-darwin; then
  targets=(aarch64-apple-darwin x86_64-apple-darwin)
else
  [[ "${ARCH:-universal}" == "universal" ]] && echo "    (x86_64/aarch64 target missing; building host arch only — 'rustup target add x86_64-apple-darwin aarch64-apple-darwin' for universal)"
fi
bins=()
if [[ ${#targets[@]} -gt 0 ]]; then
  for t in "${targets[@]}"; do
    MACOSX_DEPLOYMENT_TARGET=11.0 cargo build --release --locked -p remote-desk-agent --target "$t"
    bins+=("target/$t/release/rd-agent")
  done
else
  MACOSX_DEPLOYMENT_TARGET=11.0 cargo build --release --locked -p remote-desk-agent
  bins+=("target/release/rd-agent")
fi

echo "==> Assembling $APP_NAME.app"
rm -rf "$APP" "$DMG"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
if [[ ${#bins[@]} -gt 1 ]]; then
  lipo -create -output "$APP/Contents/MacOS/remote-desk" "${bins[@]}"
else
  cp "${bins[0]}" "$APP/Contents/MacOS/remote-desk"
fi
sed "s/__VERSION__/$VERSION/g" packaging/macos/Info.plist > "$APP/Contents/Info.plist"
cp packaging/macos/AppIcon.icns "$APP/Contents/Resources/"
cp -R web/dist "$APP/Contents/Resources/web"
plutil -lint "$APP/Contents/Info.plist" >/dev/null

echo "==> Signing (${SIGN_IDENTITY/#-/ad-hoc})"
sign_args=(--force --sign "$SIGN_IDENTITY")
[[ "$SIGN_IDENTITY" != "-" ]] && sign_args+=(--options runtime --timestamp)
codesign "${sign_args[@]}" "$APP"
codesign --verify --strict "$APP"

echo "==> Creating dmg"
STAGE="$(mktemp -d)"
trap 'rm -rf "$STAGE"' EXIT
cp -R "$APP" "$STAGE/"
ln -s /Applications "$STAGE/Applications"
hdiutil create -quiet -volname "$APP_NAME" -srcfolder "$STAGE" -fs HFS+ -format UDZO -ov "$DMG"
[[ "$SIGN_IDENTITY" != "-" ]] && codesign --force --sign "$SIGN_IDENTITY" --timestamp "$DMG"

if [[ -n "${NOTARY_PROFILE:-}" && "$SIGN_IDENTITY" != "-" ]]; then
  echo "==> Notarizing"
  xcrun notarytool submit "$DMG" --keychain-profile "$NOTARY_PROFILE" --wait
  xcrun stapler staple "$DMG"
fi

echo
echo "Built:"
echo "  $APP  ($(lipo -archs "$APP/Contents/MacOS/remote-desk"))"
echo "  $DMG  ($(du -h "$DMG" | cut -f1))"
