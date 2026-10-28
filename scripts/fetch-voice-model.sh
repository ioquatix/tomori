#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
mkdir -p android/voice-assets target/downloads
model=ggml-base.en-q5_1.bin
sha=4baf70dd0d7c4247ba2b81fafd9c01005ac77c2f9ef064e00dcf195d0e2fdd2f
destination="android/voice-assets/$model"
if [[ -f "$destination" ]] && printf '%s  %s\n' "$sha" "$destination" | sha256sum --check --status; then
    exit 0
fi
curl --fail --location --retry 3 "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/$model" -o "target/downloads/$model"
printf '%s  %s\n' "$sha" "target/downloads/$model" | sha256sum --check
mv "target/downloads/$model" "$destination"
echo "Installed Whisper base.en-q5_1 (60 MB). No audio is uploaded."
