#!/usr/bin/env bash
# Fetch the streaming speech recognition model for the server into .tools/asr. With `whisper`, also fetch Whisper small.en, used only by the comparison benchmark; see crates/speech/readme.md.
set -euo pipefail
cd "$(dirname "$0")/.."
root="$PWD/.tools/asr"
mkdir -p "$root"
releases='https://github.com/k2-fsa/sherpa-onnx/releases/download/asr-models'

# The SHA-256 of a file, with GNU coreutils or the macOS tools.
digest() {
    local actual
    if command -v sha256sum >/dev/null; then actual=$(sha256sum "$1"); else actual=$(shasum -a 256 "$1"); fi
    echo "${actual%% *}"
}

# Download the archive `$1` to `$root`, check it against `$2` when given, and extract it.
fetch() {
    local name=$1 sha=${2:-} archive="$root/$1.tar.bz2"
    if [[ ! -f "$archive" ]] || [[ -n "$sha" && "$(digest "$archive")" != "$sha" ]]; then
        curl --fail --location --retry 3 "$releases/$name.tar.bz2" -o "$archive.part"
        if [[ -n "$sha" && "$(digest "$archive.part")" != "$sha" ]]; then
            echo "Checksum mismatch for $archive.part" >&2
            exit 1
        fi
        mv "$archive.part" "$archive"
    fi
    tar -xjf "$archive" -C "$root"
    rm "$archive"
}

# Check an extracted file `$1` against `$2`.
check() {
    if [[ "$(digest "$1")" != "$2" ]]; then
        echo "Checksum mismatch for $1" >&2
        exit 1
    fi
}

model=sherpa-onnx-nemo-streaming-fast-conformer-transducer-en-480ms-int8
if [[ ! -s "$root/$model/encoder.int8.onnx" || ! -s "$root/$model/decoder.int8.onnx" || ! -s "$root/$model/joiner.int8.onnx" || ! -s "$root/$model/tokens.txt" ]]; then
    fetch "$model" da93061cbf7b708b6b65976f70b29f519be29df750d8cdcabf98c65645930f13
fi
echo "Streaming recognition model ready in $root/$model"

if [[ "${1:-}" == whisper ]]; then
    whisper=sherpa-onnx-whisper-small.en
    if [[ ! -s "$root/$whisper/small.en-encoder.int8.onnx" || ! -s "$root/$whisper/small.en-decoder.int8.onnx" || ! -s "$root/$whisper/small.en-tokens.txt" ]]; then
        # The release publishes no digest for this archive, so the files used are checked after extraction.
        fetch "$whisper"
    fi
    check "$root/$whisper/small.en-encoder.int8.onnx" 8bdac288f369aa94ee2194059238c465ed82ea9d47ee8fa4a8c0a891873e462f
    check "$root/$whisper/small.en-decoder.int8.onnx" 710ccf890e10f3faa15f51ec346081a2723c9f3adb6e4da81c6573a5a6f877fb
    check "$root/$whisper/small.en-tokens.txt" 306cd27f03c1a714eca7108e03d66b7dc042abe8c258b44c199a7ed9838dd930
    echo "Whisper small.en ready in $root/$whisper"
fi
