#!/usr/bin/env bash
# Builds Painter for Android (arm64) as an APK with cargo-apk:
# target/release/apk/Painter.apk. Needs the Android SDK and NDK
# (ANDROID_HOME, ANDROID_NDK_ROOT; the GitHub runners have both) and
# `cargo install cargo-apk`. Signed with your key or a throwaway one: install it with
# "install unknown apps" allowed, or `adb install`.
set -euo pipefail

cd "$(dirname "$0")/../.."
export ANDROID_NDK_ROOT="${ANDROID_NDK_ROOT:-${ANDROID_NDK_LATEST_HOME:-}}"
# Release builds must be signed. Without a key of your own
# (CARGO_APK_RELEASE_KEYSTORE / CARGO_APK_RELEASE_KEYSTORE_PASSWORD), a
# throwaway one is made: fine for trying the app, but each build then has a
# different key, so uninstall before installing a newer one.
if [ -z "${CARGO_APK_RELEASE_KEYSTORE:-}" ]; then
    KEY="$PWD/target/painter-test.keystore"
    if [ ! -f "$KEY" ]; then
        mkdir -p target
        keytool -genkeypair -keystore "$KEY" -storepass android -keypass android -alias androiddebugkey \
            -keyalg RSA -keysize 2048 -validity 10000 -dname "CN=Painter test build" -noprompt
    fi
    export CARGO_APK_RELEASE_KEYSTORE="$KEY"
    export CARGO_APK_RELEASE_KEYSTORE_PASSWORD=android
fi
cargo apk build -p painter-android --release
find target -name '*.apk' -path '*apk*' -exec ls -lh {} \;
