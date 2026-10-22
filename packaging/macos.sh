#!/usr/bin/env bash
# Package the Flutter macOS app as an (unsigned) .dmg.
# usage: packaging/macos.sh <version> [app-dir]
set -euo pipefail
VERSION="${1:?version}"
APP="${2:-apps/flutter/build/macos/Build/Products/Release/hokkabaz.app}"
OUT="${OUT:-dist}"
mkdir -p "$OUT"
STAGE="$(mktemp -d)"
cp -R "$APP" "$STAGE/Hokkabaz.app"
ln -s /Applications "$STAGE/Applications"
hdiutil create -volname "Hokkabaz $VERSION" -srcfolder "$STAGE" -ov -format UDZO "$OUT/hokkabaz-macos.dmg"
ls -l "$OUT"
