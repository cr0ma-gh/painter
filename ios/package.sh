#!/usr/bin/env bash
# Builds Painter for iPad and packs an unsigned .ipa (ios/build/Painter-unsigned.ipa).
# Runs on macOS with Xcode (the GitHub Actions macOS runners have it). Sign
# and install the .ipa with a tool such as Sideloadly or AltStore, or sign it
# in CI for TestFlight.
set -euo pipefail

cd "$(dirname "$0")/.."
TARGET=aarch64-apple-ios
export IPHONEOS_DEPLOYMENT_TARGET=15.0

cargo build --release -p painter-app --target "$TARGET"

BUILD=ios/build
APP="$BUILD/Payload/Painter.app"
rm -rf "$BUILD"
mkdir -p "$APP"
cp "target/$TARGET/release/painter" "$APP/Painter"
cp ios/Info.plist "$APP/Info.plist"
# Home screen icons (named in Info.plist: CFBundleIcons~ipad).
for size in 120 152 167 180; do
    cp "ios/AppIcon-$size.png" "$APP/AppIcon-$size.png"
done
plutil -lint "$APP/Info.plist"

(cd "$BUILD" && zip -qry Painter-unsigned.ipa Payload)
ls -lh "$BUILD/Painter-unsigned.ipa"
