# Speech

Speech recognition and synthesis for the Tomori server, run in the server process through [sherpa-onnx](https://k2-fsa.github.io/sherpa/onnx/). Each engine runs on threads owned by this crate. Engines start when the server starts, and a failure is reported to the utterance or sentence that saw it. Nothing is retried silently. See `design/speech.md`.

- A native abort in sherpa-onnx or ONNX Runtime ends the server process.
- A Rust panic on an engine thread is caught and disables that engine: it is logged once at error level, and every open and later request fails with `Error::Stopped`. The server tells participants that recognition or synthesis is unavailable until it restarts.
- Utterances share one recognition thread and are decoded in turn. An utterance whose audio waits more than 5 seconds (`stt::BACKLOG`) to be decoded fails with `Error::Behind`, so a recognizer that cannot keep up fails visibly instead of queueing audio without bound.
- Each engine thread has a `Stopper`, which the server uses at shutdown to stop the thread and wait for it to unload its model and exit; see `design/server.md`.

| Engine | Model | Rust type |
|---|---|---|
| Streaming recognition (default) | NeMo FastConformer streaming transducer, English, 480 ms chunks, int8 | `stt::TransducerRecognizer` |
| Transcript punctuation | English CNN-BiLSTM punctuation and casing, int8 | `stt::CnnBilstmPunctuator`, applied by `stt::Punctuated` |
| Whole-clip recognition (comparison only) | Whisper `small.en`, int8 | `stt::WhisperRecognizer` |
| Synthesis | Kokoro multi-language v1.0, voice 3 (`af_heart`) | `tts::KokoroSynthesizer` |

The streaming recognizer decodes each utterance once, as the audio arrives, so partial words appear while the participant is still speaking and the final transcript is ready shortly after they let go. Its vocabulary has no capitals or punctuation, so `stt::Punctuated` passes each final transcript through the punctuation model on the recognition thread, and ends it with a full stop, question mark or exclamation mark. Partial transcripts stay lowercase, because punctuating them would change words already shown. Punctuation never fails an utterance: if the model fails or changes the words, the transcript is delivered as recognized, and a panic disables punctuation only, logged once. Without the punctuation model, transcripts are lowercase and unpunctuated, and the server says so when it starts. Whisper cannot stream, so the server does not use it; it is kept for the benchmark below.

Kokoro reports each sentence as soon as it is generated.

## Speakable text

`Speakable` turns one assistant message's streaming Markdown into the document-level blocks that have closed, using base-markdown's `IncrementalParser` with GitHub Flavored Markdown. `push` and `finish` return each `Block` once, in order, with:

- its `kind` (paragraph, heading, list, block quote, table, code and so on) and its `span` in `Speakable::source`;
- its `events`, from its start to its end, including nested blocks, so that a table, code block or list can be shown or run as a unit;
- its `sentences`, the speakable text split at sentence boundaries, and long sentences at a clause or word break, so that synthesis of the first starts at once.

A block is returned only once no later text can change it:

- A paragraph or block quote followed by a blank line is returned at the blank line, and a heading or fenced code block after a blank line at the end of its last line.
- A block that follows the previous one without a blank line, such as a list, fence or heading directly after a paragraph, or a heading directly after a list, does not yet prove that the previous block ended, so both wait until the next block after them starts. A reply that opens with "Here's how:" and goes straight into a list is therefore spoken from the start only once the list has ended.
- A list may continue after a blank line, so it waits for the first line of the next block, and is returned whole.
- A table continues with every following plain line as a row, until a blank line or another kind of block.
- The last block of a message is returned when the message finishes.

`Block::span` may include trailing line endings, depending on the kind of block. The server speaks the sentences and does not use the blocks' structure yet.

Paragraph, heading, list item and block quote text is spoken, with emphasis markers removed and links read by their text. Code blocks (or `SpeakableOptions::code_notice` in their place), tables, raw HTML, images, footnote definitions and bare URLs are not. A reference or footnote definition applies only to uses in its own or a later block; an earlier use is read by its text.

## Setup

From the repository root:

```bash
bash scripts/fetch-sherpa-onnx.sh # .tools/sherpa-onnx: the native libraries, checked against a pinned SHA-256 (20 MB)
bash scripts/fetch-asr-model.sh   # .tools/asr: sherpa-onnx-nemo-streaming-fast-conformer-transducer-en-480ms-int8 (130 MB) and sherpa-onnx-online-punct-en-2024-08-06 (30 MB)
bash scripts/fetch-tts-model.sh   # .tools/tts/kokoro-multi-lang-v1_0 (380 MB)
cargo run -p server --bin tomori-server -- --speech-root .
```

`TransducerConfig::from_root`, `PunctuationConfig::from_root`, `KokoroConfig::from_root` and `WhisperConfig::from_root` expect this layout. `bash scripts/fetch-asr-model.sh whisper` also fetches Whisper `small.en` (640 MB download) for the benchmark.

## Building and linking

The `sherpa-onnx` crate links prebuilt sherpa-onnx and ONNX Runtime libraries from the sherpa-onnx GitHub release that matches the crate version. Its build script would download them itself without checking a checksum, so the build instead uses only the archive that `scripts/fetch-sherpa-onnx.sh` has downloaded and verified:

- The script pins the SHA-256 of the static library archive for macOS (arm64 and x86_64) and Linux (x86_64 and aarch64), downloads the one for this machine into `.tools/sherpa-onnx`, and refuses an archive that does not match. Run it again after changing the pinned version.
- `.cargo/config.toml` sets `SHERPA_ONNX_ARCHIVE_DIR` to `.tools/sherpa-onnx`, so the build script copies the archive from there instead of downloading. If the archive is missing, the build fails with `SHERPA_ONNX_ARCHIVE_DIR does not contain expected archive: …`; run the script.
- The build script unpacks the archive into `target/sherpa-onnx-prebuilt` (about 90 MB) and links whatever it finds there on later builds without checking it again. The script removes the copy in the workspace's target directory, as `cargo metadata` reports it, unless the verified archive is beside it. To be sure the libraries were built from the verified archive, delete `target/sherpa-onnx-prebuilt` and build again.
- An environment variable overrides the setting in `.cargo/config.toml`: setting `SHERPA_ONNX_ARCHIVE_DIR` yourself uses another directory, and setting `SHERPA_ONNX_LIB_DIR` links the libraries in that directory instead, as for CUDA below. Neither is checked. Cargo reads `.cargo/config.toml` from the directory it runs in, so build from inside the repository: `cargo build --manifest-path` run from elsewhere ignores the setting and lets the build script download an unchecked archive.

By default the libraries are linked statically and run on the CPU, so `tomori-server` needs no extra shared libraries at run time:

- macOS (arm64 or x86_64): links `libc++` and the Foundation framework from the system.
- Linux (x86_64 or aarch64): links `libstdc++`, `libm`, `libpthread` and `libdl`.

The `sherpa-onnx` dependency is pinned to 1.13.8. In sherpa-onnx 2.0, Kokoro skips any word missing from its lexicon, such as "Tomori", instead of pronouncing it with espeak-ng. Check this before upgrading.

### CUDA on Linux

The GPU build of sherpa-onnx is published only as shared libraries. To use it:

1. Run `bash scripts/fetch-sherpa-onnx.sh cuda`, which downloads, verifies and unpacks `sherpa-onnx-v1.13.8-cuda-12.x-cudnn-9.x-onnxruntime1.28.2-linux-x64-gpu.tar.bz2` into `.tools/sherpa-onnx` and prints its `lib` directory. It needs CUDA 12 and cuDNN 9 installed.
2. Build with the shared libraries:

   ```bash
   SHERPA_ONNX_LIB_DIR=$PWD/.tools/sherpa-onnx/sherpa-onnx-v1.13.8-cuda-12.x-cudnn-9.x-onnxruntime1.28.2-linux-x64-gpu/lib \
   	cargo build --release -p server --features speech-shared
   ```

   The build script sets an rpath to that directory, so keep the libraries there, or put them on `LD_LIBRARY_PATH`.
3. Run `tomori-server --speech-root . --speech-provider cuda`. A server built without `speech-shared` refuses `--speech-provider cuda`, because its static libraries contain only the CPU provider.

The CUDA path has not been tested yet. Kokoro and the punctuation model always run on the CPU.

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

Streaming models are fed in real time, in 100 ms frames, as the server receives them. The real-time factor is measured by decoding each clip again, directly, as fast as possible. With `--punctuation`, final transcripts are punctuated as the server punctuates them, in both passes, so the real-time factor includes punctuation, and the time each punctuation takes is reported. Word errors ignore case and punctuation.

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

- The FastConformer's only mistake was "week end" for "weekend". Its own transcripts are lowercase and unpunctuated; see below for punctuation.
- Kroko dropped the last word or two of several clips.
- NVIDIA's larger Nemotron 0.6B streaming model punctuates, but needed about 3 CPU seconds per audio second here, which is too slow for real time on this CPU.

### Punctuation

Measured on the same machine on 1 November 2026, at load average 55, with `--punctuation`:

- Punctuating a final transcript took 22 ms of wall time on average (max 75 ms) under that load; on its own, the model used 7.5–9 ms of processor time for utterances of 1 to 20 words, and 12 ms for 48 words. In back-to-back runs, the mean final after release was 0.11 s with punctuation (load average 55) and 0.07 s without (load average 150), of which punctuation itself accounts for about 0.02 s and the rest is noise on this busy machine; the table above is from an earlier run.
- 6 of the 8 Kokoro sentences matched their punctuated reference exactly, such as "Can you explain why the build failed on the Linux machine yesterday?". The other two read "What is the weather, like in Tokyo this weekend?", with a stray comma; one of them also has the recognizer's "week end".
- The LibriSpeech clips came back as "After early nightfall, the yellow lamps would light up here and there the squalid quarter of the brothels." and "Yet these thoughts affected Hester Prynne less with hope than apprehension.", with the names capitalized.
- sherpa-onnx's Chinese and English CT-Transformer was faster, about 3 ms for a sentence, but writes Chinese full-width punctuation ("，", "。") and does not restore capitals, so it is not used.
- Without the probe word described in `src/stt/punctuation.rs`, the CNN-BiLSTM model left the end of most utterances unpunctuated, such as "Run the tests" and "What do you think" without its question mark.
