#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
version=1.1.65
base="https://repo.maven.apache.org/maven2/org/khronos/openxr/openxr_loader_for_android/$version/openxr_loader_for_android-$version.aar"
mkdir -p target/downloads android/libs/arm64-v8a
archive="target/downloads/openxr-loader-$version.aar"
curl --fail --location --retry 3 "$base" -o "$archive"
curl --fail --location --retry 3 "$base.sha256" -o "$archive.sha256"
expected="$(tr -d '[:space:]' < "$archive.sha256")"
printf '%s  %s\n' "$expected" "$archive" > "$archive.check"
sha256sum -c "$archive.check"
unzip -p "$archive" jni/arm64-v8a/libopenxr_loader.so > android/libs/arm64-v8a/libopenxr_loader.so
echo "Installed Khronos OpenXR loader $version (arm64-v8a)."
