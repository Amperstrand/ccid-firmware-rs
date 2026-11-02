#!/usr/bin/env bash
# Manual (gradle-free) APK build for the bench HCE relay app (#529).
# Toolchain: /usr/lib/android-sdk (build-tools 34.0.0 d8 + platform 34).
set -euo pipefail
cd "$(dirname "$0")"

SDK="${ANDROID_HOME:-/usr/lib/android-sdk}"
BT="$SDK/build-tools/34.0.0"
PLATFORM="$SDK/platforms/android-34/android.jar"
OUT=build
KEY=bench-relay.keystore  # stable across builds: adb install -r needs it

rm -rf "$OUT"; mkdir -p "$OUT/classes" "$OUT/gen"

# 1. resources + R.java + base APK skeleton
"$BT/aapt2" compile --dir res -o "$OUT/res.zip"
"$BT/aapt2" link -o "$OUT/base.apk" -I "$PLATFORM" \
    --manifest AndroidManifest.xml -R "$OUT/res.zip" \
    --java "$OUT/gen" --auto-add-overlay

# 2. java -> class (R.java + the service)
find "$OUT/gen" -name '*.java' > "$OUT/sources.txt"
find src -name '*.java' >> "$OUT/sources.txt"
javac --release 11 -classpath "$PLATFORM" -d "$OUT/classes" @"$OUT/sources.txt"

# 3. dex
"$BT/d8" --release --lib "$PLATFORM" --output "$OUT" \
    $(find "$OUT/classes" -name '*.class')

# 4. pack + align + sign (self-signed bench key, kept stable for adb install -r)
cd "$OUT"
zip -q base.apk classes.dex
"$BT/zipalign" -f 4 base.apk bench-relay.apk
[ -f "$KEY" ] || keytool -genkeypair -keystore "$KEY" \
    -storepass benchrelay -keypass benchrelay -alias bench \
    -dname "CN=bench-hce-relay" -keyalg RSA -validity 10950 >/dev/null 2>&1
"$BT/apksigner" sign --ks "$KEY" --ks-pass pass:benchrelay \
    bench-relay.apk
echo "OK: $PWD/bench-relay.apk"
