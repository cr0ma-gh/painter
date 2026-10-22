#!/usr/bin/env bash
# Builds Painter for macOS (Apple Silicon and Intel in one binary) as
# target/macos/Painter.app and a disk image target/macos/Painter.dmg.
# Runs on macOS with Xcode's command-line tools. The app is signed ad hoc
# only: the first time, open it with right-click → Open (or allow it in
# System Settings → Privacy & Security).
set -euo pipefail

cd "$(dirname "$0")/../.."
export MACOSX_DEPLOYMENT_TARGET=11.0
for target in aarch64-apple-darwin x86_64-apple-darwin; do
    cargo build --release -p painter-app --target "$target"
done

OUT=target/macos
APP="$OUT/Painter.app"
rm -rf "$OUT"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
lipo -create -output "$APP/Contents/MacOS/painter" \
    target/aarch64-apple-darwin/release/painter target/x86_64-apple-darwin/release/painter
cp packaging/macos/Info.plist "$APP/Contents/Info.plist"
plutil -lint "$APP/Contents/Info.plist"

# The icon, at every size macOS asks for.
ICONSET="$OUT/painter.iconset"
mkdir -p "$ICONSET"
for size in 16 32 128 256 512; do
    sips -z "$size" "$size" packaging/painter-1024.png --out "$ICONSET/icon_${size}x${size}.png" > /dev/null
    double=$((size * 2))
    if [ "$double" -le 1024 ]; then
        sips -z "$double" "$double" packaging/painter-1024.png --out "$ICONSET/icon_${size}x${size}@2x.png" > /dev/null
    fi
done
iconutil -c icns "$ICONSET" -o "$APP/Contents/Resources/painter.icns"
rm -rf "$ICONSET"

codesign --force --deep --sign - "$APP"
hdiutil create -volname Painter -srcfolder "$APP" -ov -format UDZO "$OUT/Painter.dmg"
ls -lh "$OUT/Painter.dmg"
