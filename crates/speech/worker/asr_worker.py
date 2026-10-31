# Released under the MIT License.
# Copyright, 2026, by Samuel Williams.

"""Whisper recognition worker: line-delimited JSON over stdin/stdout.

Each request decodes one window of audio and returns its words with
timestamps. The worker keeps no state between requests; the Rust speech crate
owns the audio, the agreement policy and the committed transcript. No audio is
written to disk.

Request:  {"id": 1, "audio": "<base64 16 kHz mono s16le>", "prompt": "..." | null}
Response: {"id": 1, "words": [{"text": " Hello", "start": 0.0, "end": 0.4}, ...]}
Failure:  {"id": 1, "error": "message"}
"""
import base64
import json
import os
import sys
import traceback

import numpy as np
import ctranslate2
from faster_whisper import WhisperModel

RATE = 16000
# Whisper decodes at most 30 seconds; the Rust policy keeps windows shorter.
MAXIMUM_SAMPLES = RATE * 30


def respond(message):
    print(json.dumps(message, ensure_ascii=False), flush=True)


def load():
    model_path = os.environ.get("TOMORI_ASR_MODEL", ".tools/asr/faster-whisper-small.en")
    try:
        has_cuda = ctranslate2.get_cuda_device_count() > 0
    except RuntimeError:
        has_cuda = False
    device = os.environ.get("TOMORI_ASR_DEVICE", "cuda" if has_cuda else "cpu")
    compute = "float16" if device == "cuda" else "int8"
    model = WhisperModel(model_path, device=device, compute_type=compute, cpu_threads=4)
    # Warm up on the selected backend before reporting ready.
    list(model.transcribe(np.zeros(RATE, dtype=np.float32), language="en", beam_size=1))
    return model, device, os.path.basename(os.path.normpath(model_path))


def decode(model, language, message):
    pcm = base64.b64decode(message["audio"], validate=True)
    if not pcm or len(pcm) % 2 or len(pcm) > MAXIMUM_SAMPLES * 2:
        raise ValueError("Invalid PCM window")
    audio = np.frombuffer(pcm, dtype="<i2").astype(np.float32) / 32768.0
    prompt = message.get("prompt") or None
    segments, _ = model.transcribe(
        audio, language=language, beam_size=1, word_timestamps=True,
        condition_on_previous_text=False, initial_prompt=prompt, vad_filter=False,
        temperature=0, no_speech_threshold=0.6,
    )
    return [
        {"text": word.word, "start": round(float(word.start), 3), "end": round(float(word.end), 3)}
        for segment in segments for word in (segment.words or [])
    ]


def main():
    model, device, name = load()
    language = os.environ.get("TOMORI_ASR_LANGUAGE", "en")
    respond({"ready": True, "device": device, "model": name})
    for line in sys.stdin:
        message = None
        try:
            message = json.loads(line)
            respond({"id": message["id"], "words": decode(model, language, message)})
        except Exception as error:
            print(f"ASR worker error: {error}", file=sys.stderr, flush=True)
            respond({"id": message.get("id") if isinstance(message, dict) else None, "error": str(error)})


if __name__ == "__main__":
    try:
        main()
    except Exception:
        traceback.print_exc(file=sys.stderr)
        sys.exit(1)
