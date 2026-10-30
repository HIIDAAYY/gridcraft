#!/usr/bin/env bash
# Build GridCraft for Android as an installable APK (arm64), with the Android SDK's own tools
# only: no Gradle, nothing downloaded at build time beyond Rust crates.
#
# Needs: Rust with the aarch64-linux-android target, a JDK (17+), and an Android SDK with
# build-tools, a platform (android-35 by default) and the NDK. Point ANDROID_HOME at the SDK;
# ANDROID_NDK_HOME defaults to the newest NDK inside it.
#
#   apps/gridcraft-android/build-apk.sh            # -> dist/android/GridCraft-<version>-arm64.apk
#   APP_ID=io.github.me.gridcraft apps/gridcraft-android/build-apk.sh
#   PROFILE=debug apps/gridcraft-android/build-apk.sh   # faster build, slower app
#
# The APK is signed with a local key, kept in apps/gridcraft-android/keys/ (gitignored) and
# created on the first build. Keep that file: Android installs an update only when it is signed
# with the same key as the version installed. Set KEYSTORE, KEYSTORE_PASS and KEY_ALIAS to sign
# with your own key instead.
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "$HERE/../.." && pwd)"

: "${ANDROID_HOME:=${ANDROID_SDK_ROOT:-}}"
if [[ -z "$ANDROID_HOME" || ! -d "$ANDROID_HOME" ]]; then
    echo "error: set ANDROID_HOME to your Android SDK" >&2
    exit 1
fi
if [[ -z "${ANDROID_NDK_HOME:-}" ]]; then
    ANDROID_NDK_HOME="$(ls -d "$ANDROID_HOME"/ndk/* 2>/dev/null | sort -V | tail -1 || true)"
fi
if [[ -z "$ANDROID_NDK_HOME" || ! -d "$ANDROID_NDK_HOME" ]]; then
    echo "error: no Android NDK found; install one (sdkmanager \"ndk;27.2.12479018\") or set ANDROID_NDK_HOME" >&2
    exit 1
fi
PLATFORM="${ANDROID_PLATFORM:-android-35}"
ANDROID_JAR="$ANDROID_HOME/platforms/$PLATFORM/android.jar"
BUILD_TOOLS="${BUILD_TOOLS:-$(ls -d "$ANDROID_HOME"/build-tools/* | sort -V | tail -1)}"
[[ -f "$ANDROID_JAR" ]] || { echo "error: $ANDROID_JAR missing (sdkmanager \"platforms;$PLATFORM\")" >&2; exit 1; }

MIN_SDK=26
TARGET_SDK="${PLATFORM#android-}"
APP_ID="${APP_ID:-gridcraft.android}"
PROFILE="${PROFILE:-release}"
TARGET=aarch64-linux-android
ABI=arm64-v8a
VERSION="$(sed -n 's/^version = "\(.*\)"/\1/p' "$ROOT/Cargo.toml" | head -1)"
# versionCode: 0.10.3 -> 1003, plus a build number for rebuilds of one version (CI sets it).
IFS=. read -r V_MAJOR V_MINOR V_PATCH <<<"$VERSION"
VERSION_CODE="${VERSION_CODE:-$(( (V_MAJOR * 10000 + V_MINOR * 100 + V_PATCH) * 100 + ${BUILD_NUMBER:-0} % 100 ))}"

HOST_TAG="$(ls "$ANDROID_NDK_HOME/toolchains/llvm/prebuilt" | head -1)"
LLVM="$ANDROID_NDK_HOME/toolchains/llvm/prebuilt/$HOST_TAG/bin"
export CARGO_TARGET_AARCH64_LINUX_ANDROID_LINKER="$LLVM/${TARGET}${MIN_SDK}-clang"
export CC_aarch64_linux_android="$LLVM/${TARGET}${MIN_SDK}-clang"
export CXX_aarch64_linux_android="$LLVM/${TARGET}${MIN_SDK}-clang++"
export AR_aarch64_linux_android="$LLVM/llvm-ar"
export RANLIB_aarch64_linux_android="$LLVM/llvm-ranlib"
# 16 KB pages (Android 15 devices) as well as 4 KB.
export CARGO_TARGET_AARCH64_LINUX_ANDROID_RUSTFLAGS="${CARGO_TARGET_AARCH64_LINUX_ANDROID_RUSTFLAGS:-} -C link-arg=-Wl,-z,max-page-size=16384"

OUT="$ROOT/dist/android"
WORK="$ROOT/target/android-apk"
rm -rf "$WORK"
mkdir -p "$WORK"/{classes,dex,res,apk/lib/$ABI} "$OUT"

echo "==> winit with Android modifier keys"
# winit 0.30 never reports Shift/Ctrl/Alt on Android, so a hardware keyboard's Ctrl+Z typed "z".
# winit-android-modifiers.patch fixes that; it is applied to a copy of the workspace's winit (the
# fork named in [patch.crates-io]) for this build only (Cargo.lock is put back afterwards, the
# desktop builds never see it).
(cd "$ROOT" && cargo fetch --target "$TARGET" -q)
WINIT_REV="$(sed -n 's|^winit = { git = "https://github.com/storytold/winit.git", rev = "\([0-9a-f]*\)" }|\1|p' "$ROOT/Cargo.toml" | head -1)"
WINIT_SRC="$(ls -d "${CARGO_HOME:-$HOME/.cargo}"/git/checkouts/winit-*/"${WINIT_REV:0:7}" | head -1)"
[[ -d "$WINIT_SRC" ]] || { echo "error: winit $WINIT_REV is not in the cargo cache" >&2; exit 1; }
WINIT_PATCHED="$ROOT/target/android-winit/winit-$WINIT_REV"
rm -rf "$WINIT_PATCHED"
mkdir -p "$(dirname "$WINIT_PATCHED")"
cp -R "$WINIT_SRC" "$WINIT_PATCHED"
patch -s -p1 -d "$WINIT_PATCHED" <"$HERE/winit-android-modifiers.patch"
cp "$ROOT/Cargo.lock" "$WORK/Cargo.lock.orig"
trap 'cp "$WORK/Cargo.lock.orig" "$ROOT/Cargo.lock"' EXIT

echo "==> Rust ($PROFILE, $TARGET)"
CARGO_FLAGS=(build -p gridcraft-android --lib --target "$TARGET" --config "patch.crates-io.winit.path=\"$WINIT_PATCHED\"")
[[ "$PROFILE" == release ]] && CARGO_FLAGS+=(--release)
(cd "$ROOT" && cargo "${CARGO_FLAGS[@]}")
SO="$ROOT/target/$TARGET/$PROFILE/libgridcraft_android.so"
cp "$SO" "$WORK/apk/lib/$ABI/"
if [[ "$PROFILE" == release ]]; then
    "$LLVM/llvm-strip" --strip-unneeded "$WORK/apk/lib/$ABI/libgridcraft_android.so"
fi
# A dependency built with C++ needs the C++ runtime beside it.
if "$LLVM/llvm-readelf" -d "$WORK/apk/lib/$ABI/libgridcraft_android.so" | grep -q 'libc++_shared.so'; then
    cp "$ANDROID_NDK_HOME/toolchains/llvm/prebuilt/$HOST_TAG/sysroot/usr/lib/$TARGET/libc++_shared.so" "$WORK/apk/lib/$ABI/"
fi

echo "==> Java"
find "$HERE/android/java" -name '*.java' >"$WORK/sources.txt"
javac --release 11 -nowarn -classpath "$ANDROID_JAR" -d "$WORK/classes" @"$WORK/sources.txt"
"$BUILD_TOOLS/d8" --release --min-api "$MIN_SDK" --lib "$ANDROID_JAR" --output "$WORK/dex" $(find "$WORK/classes" -name '*.class')

echo "==> Resources"
# The launcher icon is the app's own icon (assets/app-icon), copied at build time.
ICONS="$ROOT/assets/app-icon/hicolor"
for pair in mdpi:48 hdpi:64 xhdpi:128 xxhdpi:256 xxxhdpi:512; do
    density="${pair%%:*}"
    size="${pair##*:}"
    mkdir -p "$WORK/res/mipmap-$density"
    cp "$ICONS/${size}x${size}/apps/ai.storyteller.gridcraft.png" "$WORK/res/mipmap-$density/ic_launcher.png"
done
"$BUILD_TOOLS/aapt2" compile --dir "$WORK/res" -o "$WORK/res.zip"
"$BUILD_TOOLS/aapt2" link -o "$WORK/unsigned.apk" -I "$ANDROID_JAR" \
    --manifest "$HERE/android/AndroidManifest.xml" \
    --rename-manifest-package "$APP_ID" \
    --min-sdk-version "$MIN_SDK" --target-sdk-version "$TARGET_SDK" \
    --version-code "$VERSION_CODE" --version-name "$VERSION" \
    "$WORK/res.zip"

echo "==> Package"
cp "$WORK/dex/classes.dex" "$WORK/apk/"
(cd "$WORK/apk" && zip -q -r "$WORK/unsigned.apk" classes.dex lib)
"$BUILD_TOOLS/zipalign" -f -P 16 4 "$WORK/unsigned.apk" "$WORK/aligned.apk"

echo "==> Sign"
if [[ -z "${KEYSTORE:-}" ]]; then
    KEYSTORE="$HERE/keys/gridcraft-android.jks"
    KEYSTORE_PASS="${KEYSTORE_PASS:-gridcraft}"
    KEY_ALIAS="${KEY_ALIAS:-gridcraft}"
    if [[ ! -f "$KEYSTORE" ]]; then
        mkdir -p "$(dirname "$KEYSTORE")"
        keytool -genkeypair -keystore "$KEYSTORE" -storepass "$KEYSTORE_PASS" -keypass "$KEYSTORE_PASS" \
            -alias "$KEY_ALIAS" -keyalg RSA -keysize 4096 -validity 10000 \
            -dname "CN=GridCraft for Android (local build)" >/dev/null
        echo "    created signing key $KEYSTORE (keep it to install updates over this build)"
    fi
fi
APK="$OUT/GridCraft-$VERSION-arm64.apk"
"$BUILD_TOOLS/apksigner" sign --ks "$KEYSTORE" --ks-pass "pass:$KEYSTORE_PASS" --ks-key-alias "$KEY_ALIAS" \
    --min-sdk-version "$MIN_SDK" --out "$APK" "$WORK/aligned.apk"
"$BUILD_TOOLS/apksigner" verify "$APK"
echo "==> $APK ($(du -h "$APK" | cut -f1))"
