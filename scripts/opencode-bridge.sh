#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
if [[ -f .android-env ]]; then source .android-env; fi
if [[ ! -d bridge/node_modules/@opencode/client || ! -d bridge/node_modules/sherpa-onnx-node ]]; then
    npm ci --prefix bridge --ignore-scripts
fi
if [[ ! -s .tools/tts/kokoro-multi-lang-v1_0/model.onnx ]]; then
    echo "Kokoro model not installed; chat still works. Run bash scripts/fetch-tts-model.sh for spoken replies." >&2
fi
if [[ ! -x .tools/asr-venv/bin/python || ! -s .tools/asr/faster-whisper-small.en/model.bin ]]; then
    echo "PC transcription not installed; run bash scripts/setup-pc-transcription.sh before recording." >&2
fi
export PATH="${ANDROID_HOME:-$HOME/Android/Sdk}/platform-tools:$PATH"
adb reverse tcp:8765 "tcp:${BASE_SWARM_BRIDGE_PORT:-8765}"
exec node bridge/server.mjs
