#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
root="$PWD/.tools/tts"
mkdir -p "$root"
archive="$root/kokoro-multi-lang-v1_0.tar.bz2"
# Whether the file has the expected SHA-256, with GNU coreutils or the macOS tools.
verify() {
    local actual
    if command -v sha256sum >/dev/null; then actual=$(sha256sum "$1"); else actual=$(shasum -a 256 "$1"); fi
    [[ "${actual%% *}" == "$sha" ]]
}
sha=60703707bc05ccd48d60e1eb723f38b07d6e474795a0b6a588d7220400e0f7a4
if [[ ! -f "$archive" ]] || ! verify "$archive"; then
    curl --fail --location --retry 3 'https://github.com/k2-fsa/sherpa-onnx/releases/download/tts-models/kokoro-multi-lang-v1_0.tar.bz2' -o "$archive.part"
    verify "$archive.part" || { echo "Checksum mismatch for $archive.part" >&2; exit 1; }
    mv "$archive.part" "$archive"
fi
if [[ ! -s "$root/kokoro-multi-lang-v1_0/model.onnx" || ! -s "$root/kokoro-multi-lang-v1_0/voices.bin" || ! -s "$root/kokoro-multi-lang-v1_0/tokens.txt" || ! -s "$root/kokoro-multi-lang-v1_0/lexicon-us-en.txt" || ! -d "$root/kokoro-multi-lang-v1_0/espeak-ng-data" ]]; then
    tar -xjf "$archive" -C "$root"
fi
echo "Kokoro v1.0 voices ready in $root/kokoro-multi-lang-v1_0"
