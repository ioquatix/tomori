# Speech Workers

The speech crate runs its engines in supervised Python worker processes that exchange line-delimited JSON over stdin and stdout. A worker is started on first use, or ahead of time with `start()`, and is restarted after any crash, timeout or malformed output. A failure is reported to the utterance or segment that saw it. Nothing is retried silently.

| Worker | Engine | Rust type |
|---|---|---|
| `asr_worker.py` | faster-whisper `small.en` (CTranslate2) | `stt::WhisperWorker` |
| `tts_worker.py` | Kokoro multi-language v1.0 (sherpa-onnx) | `tts::KokoroWorker` |

## Setup

From the repository root on the PC:

```bash
bash scripts/setup-pc-transcription.sh             # .tools/asr-venv and .tools/asr/faster-whisper-small.en
bash scripts/fetch-tts-model.sh                    # .tools/tts/kokoro-multi-lang-v1_0
.tools/asr-venv/bin/pip install 'sherpa-onnx==1.13.8'
```

Both workers share the `.tools/asr-venv` environment. `WhisperConfig::from_root` and `KokoroConfig::from_root` expect this layout. The Whisper config also puts the environment's CUDA libraries on `LD_LIBRARY_PATH`. Whisper runs on CUDA when it is available; set `WhisperConfig::device` to force `cpu`. The CUDA libraries are installed only on Linux; on macOS Whisper runs on the CPU. Kokoro runs on the CPU with two threads.

To check the real engines:

```bash
cargo test -p speech -- --ignored
```

## Protocol

Every worker first prints `{"ready": true, ...}`, describing what it loaded, or `{"error": "..."}` if it cannot start. Each request carries an `id`, and every response line repeats it. A response containing `error` fails only that request; the worker keeps running.

- Whisper: `{"id", "audio": base64 16 kHz s16le, "prompt"}` returns `{"id", "words": [{"text", "start", "end"}]}`.
- Kokoro: `{"id", "text", "voice", "speed"}` streams `{"id", "audio": base64 s16le, "sample_rate"}` for each sentence, then `{"id", "done": true}`.

No audio is written to disk.
