"""Private local Whisper worker, line-delimited JSON over stdin/stdout.

No audio is written to disk. A bounded rolling window keeps long prompts from
requiring an ever-growing audio buffer. One worker serves one active recording.
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
REFRESH = RATE * 2
WINDOW = RATE * 24
COMMIT_BEFORE = 20.0


class RollingTranscript:
    def __init__(self, model):
        self.model = model
        self.audio = np.empty(0, dtype=np.float32)
        self.committed = []
        self.pending = ""
        self.since_update = 0

    def feed(self, pcm):
        samples = np.frombuffer(pcm, dtype="<i2").astype(np.float32) / 32768.0
        self.audio = np.concatenate((self.audio, samples))
        self.since_update += len(samples)
        if self.since_update >= REFRESH or len(self.audio) >= WINDOW:
            self.decode(False)
        return self.text()

    def decode(self, final):
        self.since_update = 0
        if len(self.audio) < RATE // 3:
            return
        # Silence should never create an invented prompt.
        # Check short windows instead of the average of a whole 24-second
        # buffer: a quiet phrase followed by a long pause must not disappear.
        peak_rms = max(np.sqrt(np.mean(self.audio[i:i + 1600] ** 2))
                       for i in range(0, len(self.audio), 1600))
        if peak_rms < 0.003:
            if len(self.audio) >= WINDOW:
                self.audio = self.audio[-RATE * 4:]
            self.pending = ""
            return
        segments, _ = self.model.transcribe(
            self.audio, language="en", beam_size=1, word_timestamps=True,
            condition_on_previous_text=False, vad_filter=False,
            temperature=0, no_speech_threshold=0.6,
        )
        words = [word for segment in segments for word in (segment.words or [])]
        self.pending = "".join(word.word for word in words).strip()
        if not final and len(self.audio) >= WINDOW:
            stable = [word for word in words if word.end <= COMMIT_BEFORE]
            if stable:
                self.committed.append("".join(word.word for word in stable).strip())
                cut = max(1, min(len(self.audio), int(stable[-1].end * RATE)))
                self.audio = self.audio[cut:].copy()
                self.pending = "".join(word.word for word in words[len(stable):]).strip()
            else:
                # Never silently discard a potentially spoken section.
                raise ValueError("Could not find a safe transcription boundary; no message was sent")

    def text(self):
        return " ".join(part for part in [*self.committed, self.pending] if part).strip()

    def finish(self):
        self.decode(True)
        return self.text()


def respond(message):
    print(json.dumps(message, ensure_ascii=False), flush=True)


def main():
    model_path = os.environ.get("BASE_SWARM_ASR_MODEL", ".tools/asr/faster-whisper-small.en")
    try:
        has_cuda = ctranslate2.get_cuda_device_count() > 0
    except RuntimeError:
        has_cuda = False
    device = os.environ.get("BASE_SWARM_ASR_DEVICE", "cuda" if has_cuda else "cpu")
    compute = "float16" if device == "cuda" else "int8"
    model = WhisperModel(model_path, device=device, compute_type=compute, cpu_threads=4)
    # Warm up on the selected backend before reporting ready.
    list(model.transcribe(np.zeros(RATE, dtype=np.float32), language="en", beam_size=1))
    respond({"ready": True, "device": device, "model": "small.en"})
    stream = None
    stream_id = None
    for line in sys.stdin:
        message = None
        try:
            message = json.loads(line)
            operation = message["op"]
            if operation == "start":
                if stream is not None:
                    raise ValueError("A transcription is already active")
                stream_id = message["stream"]
                stream = RollingTranscript(model)
                response = {"text": ""}
            elif operation == "cancel":
                if stream_id == message["stream"]:
                    stream = None
                    stream_id = None
                response = {"cancelled": True}
            elif stream is None or stream_id != message["stream"]:
                raise ValueError("Transcription is no longer active")
            elif operation == "audio":
                pcm = base64.b64decode(message["audio"], validate=True)
                if not pcm or len(pcm) % 2 or len(pcm) > RATE * 2 * 2:
                    raise ValueError("Invalid PCM chunk")
                response = {"text": stream.feed(pcm)}
            elif operation == "finish":
                response = {"text": stream.finish()}
                stream = None
                stream_id = None
            else:
                raise ValueError("Unknown transcription operation")
            respond({"id": message["id"], **response})
        except Exception as error:
            print(f"ASR worker error: {error}", file=sys.stderr, flush=True)
            respond({"id": message.get("id") if message else None, "error": str(error)})


if __name__ == "__main__":
    try:
        main()
    except Exception:
        traceback.print_exc(file=sys.stderr)
        sys.exit(1)
