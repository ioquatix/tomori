#!/usr/bin/env bash
# Fetch the streaming speech recognition model, and the model that punctuates its transcripts, for the server into .tools/asr. With `whisper`, also fetch Whisper small.en, used only by the comparison benchmark; see crates/speech/readme.md.
set -euo pipefail
cd "$(dirname "$0")/.."
root="$PWD/.tools/asr"
mkdir -p "$root"
releases='https://github.com/k2-fsa/sherpa-onnx/releases/download'

# The SHA-256 of a file, with GNU coreutils or the macOS tools.
digest() {
    local actual
    if command -v sha256sum >/dev/null; then actual=$(sha256sum "$1"); else actual=$(shasum -a 256 "$1"); fi
    echo "${actual%% *}"
}

# Download the archive `$2` from the release tagged `$1` to `$root`, check it against `$3` when given, and extract it.
fetch() {
    local tag=$1 name=$2 sha=${3:-} archive="$root/$2.tar.bz2"
    if [[ ! -f "$archive" ]] || [[ -n "$sha" && "$(digest "$archive")" != "$sha" ]]; then
        curl --fail --location --retry 3 "$releases/$tag/$name.tar.bz2" -o "$archive.part"
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
    fetch asr-models "$model" da93061cbf7b708b6b65976f70b29f519be29df750d8cdcabf98c65645930f13
fi
echo "Streaming recognition model ready in $root/$model"

# Restores capitals and punctuation in final transcripts; without it, transcripts are lowercase and unpunctuated. The digest is the one the release publishes in checksum.txt.
punctuation=sherpa-onnx-online-punct-en-2024-08-06
if [[ ! -s "$root/$punctuation/model.int8.onnx" || ! -s "$root/$punctuation/bpe.vocab" ]]; then
    fetch punctuation-models "$punctuation" 9f5e5a72c7d2829635bd074fce92b6bbd5b78da8a52e7ad8ed1be933f366b99d
fi
echo "Punctuation model ready in $root/$punctuation"

if [[ "${1:-}" == whisper ]]; then
    whisper=sherpa-onnx-whisper-small.en
    if [[ ! -s "$root/$whisper/small.en-encoder.int8.onnx" || ! -s "$root/$whisper/small.en-decoder.int8.onnx" || ! -s "$root/$whisper/small.en-tokens.txt" ]]; then
        # The release publishes no digest for this archive, so the files used are checked after extraction.
        fetch asr-models "$whisper"
    fi
    check "$root/$whisper/small.en-encoder.int8.onnx" 8bdac288f369aa94ee2194059238c465ed82ea9d47ee8fa4a8c0a891873e462f
    check "$root/$whisper/small.en-decoder.int8.onnx" 710ccf890e10f3faa15f51ec346081a2723c9f3adb6e4da81c6573a5a6f877fb
    check "$root/$whisper/small.en-tokens.txt" 306cd27f03c1a714eca7108e03d66b7dc042abe8c258b44c199a7ed9838dd930
    echo "Whisper small.en ready in $root/$whisper"
fi
