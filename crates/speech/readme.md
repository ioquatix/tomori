# Speech

Speech recognition and synthesis for the Tomori server, run in the server process through [sherpa-onnx](https://k2-fsa.github.io/sherpa/onnx/). Each engine runs on threads owned by this crate. Engines start when the server starts, and a failure is reported to the utterance or sentence that saw it. Nothing is retried silently. See `design/speech.md`.

| Engine | Model | Rust type |
|---|---|---|
| Streaming recognition (default) | NeMo FastConformer streaming transducer, English, 480 ms chunks, int8 | `stt::TransducerRecognizer` |
| Whole-clip recognition (comparison only) | Whisper `small.en`, int8 | `stt::WhisperRecognizer` |
| Synthesis | Kokoro multi-language v1.0, voice 3 (`af_heart`) | `tts::KokoroSynthesizer` |

The streaming recognizer decodes each utterance once, as the audio arrives, so partial words appear while the participant is still speaking and the final transcript is ready shortly after they let go. Whisper is not used by the server: the Whisper models that sherpa-onnx publishes report no word times, so local-agreement streaming cannot drop the audio it has committed. It is kept for the benchmark below.

Kokoro reports each sentence as soon as it is generated.

## Setup

From the repository root:

```bash
bash scripts/fetch-asr-model.sh   # .tools/asr/sherpa-onnx-nemo-streaming-fast-conformer-transducer-en-480ms-int8 (130 MB)
bash scripts/fetch-tts-model.sh   # .tools/tts/kokoro-multi-lang-v1_0 (380 MB)
cargo run -p server --bin tomori-server -- --speech-root .
```

`TransducerConfig::from_root`, `KokoroConfig::from_root` and `WhisperConfig::from_root` expect this layout. `bash scripts/fetch-asr-model.sh whisper` also fetches Whisper `small.en` (640 MB download) for the benchmark.

## Building and linking

The `sherpa-onnx` crate's build script downloads prebuilt sherpa-onnx and ONNX Runtime libraries for the target from the sherpa-onnx GitHub release that matches the crate version. It caches them in `target/sherpa-onnx-prebuilt` (about 20 MB to download and 90 MB unpacked). The download is not checked against a checksum. To build offline, put the archive in a directory and set `SHERPA_ONNX_ARCHIVE_DIR` to it.

By default the libraries are linked statically and run on the CPU, so `tomori-server` needs no extra shared libraries at run time:

- macOS (arm64 or x86_64): links `libc++` and the Foundation framework from the system.
- Linux (x86_64 or aarch64): links `libstdc++`, `libm`, `libpthread` and `libdl`.

The `sherpa-onnx` dependency is pinned to 1.13.8. In sherpa-onnx 2.0, Kokoro skips any word missing from its lexicon, such as "Tomori", instead of pronouncing it with espeak-ng. Check this before upgrading.

### CUDA on Linux

The GPU build of sherpa-onnx is published only as shared libraries. To use it:

1. Download and unpack `sherpa-onnx-v1.13.8-cuda-12.x-cudnn-9.x-onnxruntime1.28.2-linux-x64-gpu.tar.bz2` from the sherpa-onnx v1.13.8 release. It needs CUDA 12 and cuDNN 9 installed.
2. Build with the shared libraries:

   ```bash
   SHERPA_ONNX_LIB_DIR=/path/to/unpacked/lib cargo build --release -p server --features speech-shared
   ```

   The build script sets an rpath to that directory, so keep the libraries there, or put them on `LD_LIBRARY_PATH`.
3. Run `tomori-server --speech-root . --speech-provider cuda`.

The CUDA path has not been tested yet. Kokoro always runs on the CPU.

## Tests

```bash
cargo test -p speech
cargo test -p speech -- --ignored   # the real engines, with the models installed
```

The ignored tests use the models under the repository root. To use models installed elsewhere, set `TOMORI_SPEECH_ROOT` to the directory holding their `.tools/`.

## Benchmark

`examples/recognition_benchmark.rs` compares recognizers on English clips:

- the LibriSpeech samples that come with the streaming model;
- sentences spoken by four Kokoro voices: American and British, female and male.

Streaming models are fed in real time, in 100 ms frames, as the server receives them.

```bash
cargo run --release -p speech --example recognition_benchmark -- --root . \
	--model .tools/asr/sherpa-onnx-nemo-streaming-fast-conformer-transducer-en-480ms-int8 --whisper
```

Results on an Apple M4 Pro (12 cores), 11 clips totalling 55.5 seconds, measured on 1 November 2026. Other work was keeping the machine busy (load average 80–200), so the timings are pessimistic.

| Model | Word error rate | First partial | Final after release | Real-time factor | CPU seconds per audio second | Threads |
|---|---|---|---|---|---|---|
| FastConformer transducer, 480 ms (default) | 1.2% (2/163) | 0.99 s | 0.11 s (max 0.28) | 0.39 | 0.22 | 2 |
| Kroko streaming Zipformer | 7.4% (12/163) | 1.13 s | 0.01 s | 0.04 | 0.06 | 2 |
| Whisper `small.en`, whole clip | 0.6% (1/163) | about 2.6 s | 3.6 s (max 9.7) | 0.71 | 1.22 | 4 |

- **First partial:** the time from the start of the audio to the first words shown.
- **Final after release:** the time from the end of the audio to the final transcript.
- **Real-time factor:** the time to decode the audio when it is fed as fast as possible, divided by its length.
- Whisper has no streaming mode. Its first partial is estimated as one second of audio plus the time to decode it, and its final is the time to decode the whole clip.

Notes on the results:

- The FastConformer's only mistake was "week end" for "weekend". Its transcripts are lowercase and unpunctuated.
- Kroko dropped the last word or two of several clips.
- NVIDIA's larger Nemotron 0.6B streaming model punctuates, but needed about 3 CPU seconds per audio second here, which is too slow for real time on this CPU.
