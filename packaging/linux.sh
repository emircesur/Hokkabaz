#!/usr/bin/env bash
# Package a Flutter Linux release bundle as .deb, .tar.gz and (if linuxdeploy is on PATH) AppImage.
# usage: packaging/linux.sh <version> [bundle-dir]
set -euo pipefail
VERSION="${1:?version}"
BUNDLE="${2:-apps/flutter/build/linux/x64/release/bundle}"
OUT="${OUT:-dist}"
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
mkdir -p "$OUT"
cd "$ROOT"

# --- tar.gz
tar -C "$BUNDLE" -czf "$OUT/hokkabaz-linux-x64.tar.gz" .

# --- deb: /opt/hokkabaz + /usr/bin symlink + desktop entry
PKG="$(mktemp -d)/hokkabaz_${VERSION}_amd64"
mkdir -p "$PKG/DEBIAN" "$PKG/opt/hokkabaz" "$PKG/usr/bin" "$PKG/usr/share/applications" "$PKG/usr/share/icons/hicolor/256x256/apps"
cp -a "$BUNDLE/." "$PKG/opt/hokkabaz/"
ln -s /opt/hokkabaz/hokkabaz "$PKG/usr/bin/hokkabaz"
install -m644 packaging/hokkabaz.desktop "$PKG/usr/share/applications/hokkabaz.desktop"
install -m644 apps/flutter/macos/Runner/Assets.xcassets/AppIcon.appiconset/app_icon_256.png "$PKG/usr/share/icons/hicolor/256x256/apps/hokkabaz.png" 2>/dev/null \
  || install -m644 apps/flutter/macos/Runner/Assets.xcassets/AppIcon.appiconset/app_icon_128.png "$PKG/usr/share/icons/hicolor/256x256/apps/hokkabaz.png"
SIZE=$(du -sk "$PKG" | cut -f1)
cat > "$PKG/DEBIAN/control" <<CTL
Package: hokkabaz
Version: ${VERSION}
Section: utils
Priority: optional
Architecture: amd64
Installed-Size: ${SIZE}
Depends: libgtk-3-0, libmpv2 | libmpv1, libstdc++6, libgomp1
Recommends: ffmpeg, python3
Maintainer: Hokkabaz <noreply@users.noreply.github.com>
Homepage: https://github.com/emircesur/Hokkabaz
Description: Multi-provider AI agent harness with n8n-compatible automation and a video studio
 Hokkabaz runs AI agents and n8n-compatible flows on your machine.
CTL
dpkg-deb --root-owner-group --build "$PKG" "$OUT/hokkabaz_${VERSION}_amd64.deb"

# --- AppImage
if command -v linuxdeploy >/dev/null 2>&1; then
  APPDIR="$(mktemp -d)/AppDir"
  mkdir -p "$APPDIR/usr/bin" "$APPDIR/usr/share/applications" "$APPDIR/usr/share/icons/hicolor/256x256/apps"
  cp -a "$BUNDLE/." "$APPDIR/usr/bin/"
  cp packaging/hokkabaz.desktop "$APPDIR/usr/share/applications/"
  cp "$PKG/usr/share/icons/hicolor/256x256/apps/hokkabaz.png" "$APPDIR/usr/share/icons/hicolor/256x256/apps/"
  # The Flutter bundle finds its libs relative to the executable (rpath $ORIGIN/lib).
  ( cd "$OUT" && VERSION="$VERSION" linuxdeploy --appdir "$APPDIR" --desktop-file "$APPDIR/usr/share/applications/hokkabaz.desktop" \
      --icon-file "$APPDIR/usr/share/icons/hicolor/256x256/apps/hokkabaz.png" --output appimage ) || echo "AppImage step failed; deb and tar.gz are still produced" >&2
fi
ls -l "$OUT"
