#!/usr/bin/env bash
# Download and verify the prebuilt sherpa-onnx libraries that the speech crate links, into .tools/sherpa-onnx.
#
# `bash scripts/fetch-sherpa-onnx.sh` fetches the static CPU libraries for this machine. `.cargo/config.toml` points
# SHERPA_ONNX_ARCHIVE_DIR at .tools/sherpa-onnx, so the build uses this verified archive and never downloads one itself.
#
# `bash scripts/fetch-sherpa-onnx.sh cuda` fetches and unpacks the Linux x64 CUDA build for the speech-shared feature,
# and prints the SHERPA_ONNX_LIB_DIR to build with. See crates/speech/readme.md.
set -euo pipefail
cd "$(dirname "$0")/.."
# The version of the sherpa-onnx crate pinned in crates/speech/Cargo.toml.
version=1.13.8
root="$PWD/.tools/sherpa-onnx"
mkdir -p "$root"

# Whether the file has the expected SHA-256, with GNU coreutils or the macOS tools.
verify() {
    local actual
    if command -v sha256sum >/dev/null; then actual=$(sha256sum "$1"); else actual=$(shasum -a 256 "$1"); fi
    [[ "${actual%% *}" == "$sha" ]]
}

variant="${1:-static}"
case "$variant/$(uname -s)/$(uname -m)" in
    static/Darwin/arm64)
        archive="sherpa-onnx-v$version-osx-arm64-static-lib.tar.bz2"
        sha=9091bf160dc7fdacedbc906b212badf53c2993f4e5277a0e03998e96c31d60da ;;
    static/Darwin/x86_64)
        archive="sherpa-onnx-v$version-osx-x64-static-lib.tar.bz2"
        sha=a3f88da3e54c850a12d61431e73f8affcd1f13738b75b768847dd79541835b4b ;;
    static/Linux/x86_64)
        archive="sherpa-onnx-v$version-linux-x64-static-lib.tar.bz2"
        sha=e1fdc5b67530e15741ef897fa5ffff297056f3bf0c6d829a27af9225a4c4b5a6 ;;
    static/Linux/aarch64)
        archive="sherpa-onnx-v$version-linux-aarch64-static-lib.tar.bz2"
        sha=77983e3cf29aa60f2e531d249dbd01d15596530550c8db2e9e02fc6a655da6bb ;;
    cuda/Linux/x86_64)
        archive="sherpa-onnx-v$version-cuda-12.x-cudnn-9.x-onnxruntime1.28.2-linux-x64-gpu.tar.bz2"
        sha=2132af1ae8c84c84f86ac091e4311839287851d2bf5e660332ebce3898a1e0b2 ;;
    *)
        echo "No pinned sherpa-onnx $version libraries for $variant on $(uname -s) $(uname -m); see crates/speech/readme.md." >&2
        exit 1 ;;
esac

path="$root/$archive"
if [[ ! -f "$path" ]] || ! verify "$path"; then
    curl --fail --location --retry 3 "https://github.com/k2-fsa/sherpa-onnx/releases/download/v$version/$archive" -o "$path.part"
    verify "$path.part" || { echo "Checksum mismatch for $path.part" >&2; exit 1; }
    mv "$path.part" "$path"
fi

if [[ "$variant" == cuda ]]; then
    stem="${archive%.tar.bz2}"
    if [[ ! -d "$root/$stem/lib" ]]; then
        tar -xjf "$path" -C "$root"
    fi
    echo "sherpa-onnx $version CUDA libraries ready; build with SHERPA_ONNX_LIB_DIR=$root/$stem/lib"
    exit 0
fi

# sherpa-onnx-sys links whatever it already unpacked into the target directory without checking it, so remove a copy
# that does not match; the next build unpacks the verified archive instead.
cache="${CARGO_TARGET_DIR:-$PWD/target}/sherpa-onnx-prebuilt"
if [[ -f "$cache/$archive" ]] && ! verify "$cache/$archive"; then
    echo "Removing an unverified sherpa-onnx copy from $cache"
    rm -rf "${cache:?}/$archive" "${cache:?}/${archive%.tar.bz2}"
fi
echo "sherpa-onnx $version libraries ready in $root"
