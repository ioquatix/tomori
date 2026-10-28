#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
root="$PWD/.tools/asr"
venv="$PWD/.tools/asr-venv"
model="$root/faster-whisper-small.en"
revision=d1d751a5f8271d482d14ca55d9e2deeebbae577f
sha=62b2a45b05ee59acb4a5341b33ee35e041395d378d418a18acfe4c9e768ee37a
mkdir -p "$model"
if [[ ! -x "$venv/bin/python" ]]; then python3 -m venv "$venv"; fi
if ! "$venv/bin/python" -c 'import faster_whisper, ctranslate2' 2>/dev/null ||
   ! "$venv/bin/pip" show nvidia-cublas-cu12 nvidia-cudnn-cu12 >/dev/null 2>&1; then
    "$venv/bin/pip" install --only-binary=:all: 'faster-whisper==1.2.1' 'huggingface-hub<2' 'nvidia-cublas-cu12' 'nvidia-cudnn-cu12'
fi
base="https://huggingface.co/Systran/faster-whisper-small.en/resolve/$revision"
for file in config.json tokenizer.json vocabulary.txt; do
    if [[ ! -s "$model/$file" ]]; then curl --fail --location --retry 3 "$base/$file" -o "$model/$file.part"; mv "$model/$file.part" "$model/$file"; fi
done
if [[ ! -s "$model/model.bin" ]] || ! printf '%s  %s\n' "$sha" "$model/model.bin" | sha256sum --check --status; then
    curl --fail --location --retry 3 "$base/model.bin" -o "$model/model.bin.part"
    printf '%s  %s\n' "$sha" "$model/model.bin.part" | sha256sum --check
    mv "$model/model.bin.part" "$model/model.bin"
fi
echo "PC Whisper small.en installed in $model"
