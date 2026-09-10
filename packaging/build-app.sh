#!/bin/bash
# Build NotificationForwarder.app (PRD Phase 5, unsigned/ad-hoc signed).
#
# Usage: ./packaging/build-app.sh [--debug]
# Output: ./dist/NotificationForwarder.app
#
# Notes:
# - LSUIElement=true (see packaging/Info.plist): menu-bar agent, no Dock icon.
# - Ad-hoc signature (`codesign -s -`) is enough to run locally. Distributing
#   to other machines needs a Developer ID + notarization (Phase 5).
# - The `gpui-ui` feature additionally requires full Xcode (`xcrun metal`).
set -euo pipefail
cd "$(dirname "$0")/.."

PROFILE=release
TARGET_SUBDIR=release
if [[ "${1:-}" == "--debug" ]]; then
  PROFILE=dev
  TARGET_SUBDIR=debug
fi

echo "==> cargo build --profile $PROFILE"
cargo build --profile "$PROFILE" -p macos-notification-forwarder

APP="dist/NotificationForwarder.app"
# Respect CARGO_TARGET_DIR when the environment overrides it (e.g. sandboxes).
BIN="${CARGO_TARGET_DIR:-target}/$TARGET_SUBDIR/notification-forwarder"

echo "==> assembling $APP"
rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
cp "$BIN" "$APP/Contents/MacOS/notification-forwarder"
chmod +x "$APP/Contents/MacOS/notification-forwarder"
cp packaging/Info.plist "$APP/Contents/Info.plist"
printf 'APPL????' > "$APP/Contents/PkgInfo"
if [[ -f assets/icon.icns ]]; then
  cp assets/icon.icns "$APP/Contents/Resources/icon.icns"
fi

echo "==> ad-hoc codesign with stable requirement"
codesign --force --deep --sign - -r='designated => identifier "com.qya.notification-forwarder"' "$APP"

echo "==> verify"
codesign --verify --verbose "$APP"
"$APP/Contents/MacOS/notification-forwarder" --help | head -n 3
echo "OK: $APP"
