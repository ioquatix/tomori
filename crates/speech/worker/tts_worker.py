# Released under the MIT License.
# Copyright, 2026, by Samuel Williams.

"""Kokoro synthesis worker: line-delimited JSON over stdin/stdout.

Each request synthesizes one segment of text and streams its audio as it is
generated, one sentence at a time. The worker keeps no state between requests;
ordering and queueing belong to the Rust speech crate. No audio is written to
disk.

Ready:    {"ready": true, "sample_rate": 24000, "num_speakers": 54, ...}
Request:  {"id": 1, "text": "Hello.", "voice": 3, "speed": 1.0}
Chunk:    {"id": 1, "audio": "<base64 mono s16le>", "sample_rate": 24000}
Done:     {"id": 1, "done": true}
Failure:  {"id": 1, "error": "message"}
"""
import base64
import json
import os
import sys
import traceback

import numpy as np

FILES = ["model.onnx", "voices.bin", "tokens.txt", "lexicon-us-en.txt", "espeak-ng-data"]
MAXIMUM_TEXT = 2000


def respond(message):
    print(json.dumps(message, ensure_ascii=False), flush=True)


def encode(samples):
    samples = np.asarray(samples, dtype=np.float32)
    pcm = (np.clip(samples, -1.0, 1.0) * 32767.0).round().astype("<i2")
    return base64.b64encode(pcm.tobytes()).decode("ascii")


def load():
    import sherpa_onnx

    root = os.environ.get("TOMORI_TTS_MODEL", ".tools/tts/kokoro-multi-lang-v1_0")
    missing = [name for name in FILES if not os.path.exists(os.path.join(root, name))]
    if missing:
        raise RuntimeError(f"Kokoro model missing {', '.join(missing)} in {root}; run bash scripts/fetch-tts-model.sh")
    threads = int(os.environ.get("TOMORI_TTS_THREADS", "2"))
    config = sherpa_onnx.OfflineTtsConfig(
        model=sherpa_onnx.OfflineTtsModelConfig(
            kokoro=sherpa_onnx.OfflineTtsKokoroModelConfig(
                model=os.path.join(root, "model.onnx"),
                voices=os.path.join(root, "voices.bin"),
                tokens=os.path.join(root, "tokens.txt"),
                data_dir=os.path.join(root, "espeak-ng-data"),
                lexicon=os.path.join(root, "lexicon-us-en.txt"),
            ),
            num_threads=threads,
            provider="cpu",
            debug=False,
        ),
        max_num_sentences=1,
    )
    if not config.validate():
        raise RuntimeError(f"invalid Kokoro configuration for {root}")
    return root, sherpa_onnx.OfflineTts(config)


def synthesize(tts, request):
    identifier = request["id"]
    text = request.get("text")
    if not isinstance(text, str) or not text.strip() or len(text) > MAXIMUM_TEXT:
        raise ValueError(f"speech requires 1 to {MAXIMUM_TEXT} characters")
    voice = int(request.get("voice", 3))
    if not 0 <= voice < tts.num_speakers:
        raise ValueError(f"voice {voice} is unavailable; the model has {tts.num_speakers} voices")
    speed = float(request.get("speed", 1.0))
    streamed = 0

    def chunk(samples, progress):
        nonlocal streamed
        if len(samples):
            respond({"id": identifier, "audio": encode(samples), "sample_rate": tts.sample_rate})
            streamed += 1
        return 1

    audio = tts.generate(text.strip(), sid=voice, speed=speed, callback=chunk)
    if not streamed:
        # Some builds do not call back; send the whole segment instead.
        if not len(audio.samples):
            raise RuntimeError("Kokoro returned no audio")
        respond({"id": identifier, "audio": encode(audio.samples), "sample_rate": audio.sample_rate})
    respond({"id": identifier, "done": True})


def main():
    try:
        root, tts = load()
    except Exception as error:
        respond({"error": f"Kokoro unavailable: {error}"})
        return 1
    respond({"ready": True, "model": root, "sample_rate": tts.sample_rate, "num_speakers": tts.num_speakers})
    for line in sys.stdin:
        line = line.strip()
        if not line:
            continue
        request = json.loads(line)
        try:
            synthesize(tts, request)
        except Exception as error:
            traceback.print_exc(file=sys.stderr)
            respond({"id": request.get("id"), "error": str(error)})
    return 0


if __name__ == "__main__":
    sys.exit(main())
