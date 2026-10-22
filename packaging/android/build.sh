#!/usr/bin/env bash
# Builds Painter for Android (arm64) as an APK with cargo-apk:
# target/release/apk/Painter.apk. Needs the Android SDK and NDK
# (ANDROID_HOME, ANDROID_NDK_ROOT; the GitHub runners have both) and
# `cargo install cargo-apk`. Signed with the debug key: install it with
# "install unknown apps" allowed, or `adb install`.
set -euo pipefail

cd "$(dirname "$0")/../.."
export ANDROID_NDK_ROOT="${ANDROID_NDK_ROOT:-${ANDROID_NDK_LATEST_HOME:-}}"
cargo apk build -p painter-android --release
find target -name '*.apk' -path '*apk*' -exec ls -lh {} \;
