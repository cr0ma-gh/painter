#!/usr/bin/env bash
# Builds Painter for Linux and packs it as an AppImage
# (target/appimage/Painter-x86_64.AppImage). Needs the GTK 3, X11 and
# Wayland development packages (see .github/workflows/ci.yml) and downloads
# appimagetool unless APPIMAGETOOL points to one. The AppImage uses the
# system's GTK 3 (file dialogs) and graphics drivers (Vulkan or OpenGL).
set -euo pipefail

cd "$(dirname "$0")/../.."
cargo build --release -p painter-app

OUT=target/appimage
APPDIR="$OUT/Painter.AppDir"
rm -rf "$APPDIR"
mkdir -p "$APPDIR/usr/bin" "$APPDIR/usr/share/applications" "$APPDIR/usr/share/icons/hicolor/512x512/apps"
cp target/release/painter "$APPDIR/usr/bin/painter"
cp packaging/linux/painter.desktop "$APPDIR/painter.desktop"
cp packaging/linux/painter.desktop "$APPDIR/usr/share/applications/painter.desktop"
cp packaging/painter.png "$APPDIR/painter.png"
cp packaging/painter.png "$APPDIR/usr/share/icons/hicolor/512x512/apps/painter.png"
cat > "$APPDIR/AppRun" <<'RUN'
#!/bin/sh
HERE="$(dirname "$(readlink -f "$0")")"
exec "$HERE/usr/bin/painter" "$@"
RUN
chmod +x "$APPDIR/AppRun"

TOOL="${APPIMAGETOOL:-$OUT/appimagetool}"
if [ ! -x "$TOOL" ]; then
    curl -fsSL -o "$TOOL" https://github.com/AppImage/appimagetool/releases/download/continuous/appimagetool-x86_64.AppImage
    chmod +x "$TOOL"
fi
# Extract-and-run: works without FUSE (CI containers).
ARCH=x86_64 APPIMAGE_EXTRACT_AND_RUN=1 "$TOOL" --no-appstream "$APPDIR" "$OUT/Painter-x86_64.AppImage"
ls -lh "$OUT/Painter-x86_64.AppImage"
