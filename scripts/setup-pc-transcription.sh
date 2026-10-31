#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
root="$PWD/.tools/asr"
venv="$PWD/.tools/asr-venv"
model="$root/faster-whisper-small.en"
revision=d1d751a5f8271d482d14ca55d9e2deeebbae577f
sha=62b2a45b05ee59acb4a5341b33ee35e041395d378d418a18acfe4c9e768ee37a
mkdir -p "$model"
# The CUDA libraries are only published for Linux; elsewhere, such as on macOS, Whisper runs on the CPU.
cuda=()
if [[ "$(uname -s)" == Linux ]]; then cuda=(nvidia-cublas-cu12 nvidia-cudnn-cu12); fi
# Whether the file has the expected SHA-256, with GNU coreutils or the macOS tools.
verify() {
    local actual
    if command -v sha256sum >/dev/null; then actual=$(sha256sum "$1"); else actual=$(shasum -a 256 "$1"); fi
    [[ "${actual%% *}" == "$sha" ]]
}
if [[ ! -x "$venv/bin/python" ]]; then python3 -m venv "$venv"; fi
if ! "$venv/bin/python" -c 'import faster_whisper, ctranslate2' 2>/dev/null ||
   { (( ${#cuda[@]} )) && ! "$venv/bin/pip" show "${cuda[@]}" >/dev/null 2>&1; }; then
    "$venv/bin/pip" install --only-binary=:all: 'faster-whisper==1.2.1' 'huggingface-hub<2' ${cuda[@]+"${cuda[@]}"}
fi
base="https://huggingface.co/Systran/faster-whisper-small.en/resolve/$revision"
for file in config.json tokenizer.json vocabulary.txt; do
    if [[ ! -s "$model/$file" ]]; then curl --fail --location --retry 3 "$base/$file" -o "$model/$file.part"; mv "$model/$file.part" "$model/$file"; fi
done
if [[ ! -s "$model/model.bin" ]] || ! verify "$model/model.bin"; then
    curl --fail --location --retry 3 "$base/model.bin" -o "$model/model.bin.part"
    verify "$model/model.bin.part" || { echo "Checksum mismatch for $model/model.bin.part" >&2; exit 1; }
    mv "$model/model.bin.part" "$model/model.bin"
fi
echo "PC Whisper small.en installed in $model"
