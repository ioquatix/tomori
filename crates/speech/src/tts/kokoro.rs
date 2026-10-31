// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

//! Kokoro synthesis through a supervised sherpa-onnx worker process (`worker/tts_worker.py`), ported from the bridge's `bridge/tts.mjs`.

use std::path::{Path, PathBuf};

use base64::{Engine as _, engine::general_purpose::STANDARD};
use serde_json::{Value, json};

use super::{AudioChunk, Synthesizer};
use crate::{
    Error,
    worker::{Flow, Worker, WorkerCommand},
};

/// The number of voices in the Kokoro multi-language v1.0 model.
const VOICES: u32 = 54;

/// Where to find the Python environment, worker script and model.
#[derive(Clone, Debug)]
pub struct KokoroConfig {
    pub python: PathBuf,
    pub script: PathBuf,
    /// The `kokoro-multi-lang-v1_0` directory: `model.onnx`, `voices.bin`, `tokens.txt`, `lexicon-us-en.txt` and `espeak-ng-data`.
    pub model: PathBuf,
    /// The speaker id, 0 to 53.
    pub voice: u32,
    pub speed: f32,
    pub threads: u32,
}

impl KokoroConfig {
    /// The layout under the repository `root`: the Python environment `.tools/asr-venv` (with `sherpa-onnx` installed) and the model fetched by `scripts/fetch-tts-model.sh`.
    pub fn from_root(root: &Path) -> Self {
        Self {
            python: root.join(".tools/asr-venv/bin/python"),
            script: Path::new(env!("CARGO_MANIFEST_DIR")).join("worker/tts_worker.py"),
            model: root.join(".tools/tts/kokoro-multi-lang-v1_0"),
            // `af_heart`.
            voice: 3,
            speed: 1.0,
            threads: 2,
        }
    }

    fn command(&self) -> Result<WorkerCommand, Error> {
        if self.voice >= VOICES {
            return Err(Error::Unavailable(format!(
                "Kokoro voice {} is out of range 0 to {}",
                self.voice,
                VOICES - 1
            )));
        }
        let files = [
            "model.onnx",
            "voices.bin",
            "tokens.txt",
            "lexicon-us-en.txt",
            "espeak-ng-data",
        ];
        if !files.iter().all(|file| self.model.join(file).exists()) {
            return Err(Error::Unavailable(
                "the Kokoro model is not installed; run bash scripts/fetch-tts-model.sh".into(),
            ));
        }
        if !self.python.exists() {
            return Err(Error::Unavailable(format!(
                "missing Python environment {}; see crates/speech/worker/readme.md",
                self.python.display()
            )));
        }
        if !self.script.exists() {
            return Err(Error::Unavailable(format!(
                "missing worker script {}",
                self.script.display()
            )));
        }
        Ok(WorkerCommand::new("kokoro", &self.python)
            .argument(&self.script)
            .env("TOMORI_TTS_MODEL", &self.model)
            .env("TOMORI_TTS_THREADS", self.threads.to_string()))
    }
}

/// A [`Synthesizer`] backed by the Kokoro worker. The worker is started on first use (or by [`KokoroWorker::start`]) and restarted after a failure.
pub struct KokoroWorker {
    worker: Worker,
    voice: u32,
    speed: f32,
}

impl KokoroWorker {
    /// Check that the model and environment are installed. The worker is not started yet.
    pub fn new(config: &KokoroConfig) -> Result<Self, Error> {
        Ok(Self {
            worker: Worker::new(config.command()?),
            voice: config.voice,
            speed: config.speed,
        })
    }

    /// Start the worker and wait for its model to load, so that the first segment does not wait for it.
    pub fn start(&mut self) -> Result<(), Error> {
        self.worker.start()?;
        let speakers = self
            .description()
            .and_then(|ready| ready.get("num_speakers"))
            .and_then(Value::as_u64);
        if let Some(speakers) = speakers
            && u64::from(self.voice) >= speakers
        {
            return Err(Error::Unavailable(format!(
                "Kokoro voice {} is out of range; the model has {speakers} voices",
                self.voice
            )));
        }
        Ok(())
    }

    /// The worker's ready message, describing the model, sample rate and voice count.
    pub fn description(&self) -> Option<&Value> {
        self.worker.ready()
    }
}

fn decode(message: &Value) -> Result<AudioChunk, Error> {
    let sample_rate = message
        .get("sample_rate")
        .and_then(Value::as_u64)
        .and_then(|rate| u32::try_from(rate).ok())
        .filter(|&rate| rate > 0)
        .ok_or_else(|| {
            Error::Worker(format!(
                "kokoro worker chunk without a sample rate: {message}"
            ))
        })?;
    let audio = message
        .get("audio")
        .and_then(Value::as_str)
        .ok_or_else(|| Error::Worker(format!("kokoro worker wrote {message}")))?;
    let bytes = STANDARD
        .decode(audio)
        .map_err(|error| Error::Worker(format!("kokoro worker audio: {error}")))?;
    if bytes.len() % 2 != 0 {
        return Err(Error::Worker(
            "kokoro worker audio has an odd length".into(),
        ));
    }
    let samples = bytes
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| i16::from_le_bytes(*pair))
        .collect();
    Ok(AudioChunk {
        sample_rate,
        samples,
    })
}

impl Synthesizer for KokoroWorker {
    fn synthesize(&mut self, text: &str, output: &mut dyn FnMut(AudioChunk)) -> Result<(), Error> {
        self.start()?;
        let request = json!({ "text": text, "voice": self.voice, "speed": self.speed });
        self.worker.request(request, |message| {
            if message.get("done").and_then(Value::as_bool) == Some(true) {
                return Ok(Flow::Done);
            }
            output(decode(message)?);
            Ok(Flow::Continue)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn repository() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
    }

    #[test]
    fn decodes_audio_chunks() {
        let message =
            json!({ "id": 1, "sample_rate": 24_000, "audio": STANDARD.encode([1u8, 0, 255, 255]) });
        assert_eq!(
            decode(&message),
            Ok(AudioChunk {
                sample_rate: 24_000,
                samples: vec![1, -1]
            })
        );
        assert!(matches!(
            decode(&json!({ "id": 1, "audio": "" })),
            Err(Error::Worker(_))
        ));
    }

    #[test]
    fn reports_a_missing_installation() {
        let config = KokoroConfig::from_root(Path::new("/nonexistent"));
        assert!(matches!(
            KokoroWorker::new(&config),
            Err(Error::Unavailable(_))
        ));
    }

    #[test]
    fn defaults_to_the_heart_voice() {
        // Speaker 3 of the Kokoro v1.0 voice table is `af_heart`.
        assert_eq!(KokoroConfig::from_root(Path::new("/")).voice, 3);
    }

    #[test]
    fn rejects_an_unknown_voice() {
        let mut config = KokoroConfig::from_root(&repository());
        config.voice = 54;
        assert!(matches!(
            KokoroWorker::new(&config),
            Err(Error::Unavailable(message)) if message.contains("voice")
        ));
    }

    /// Requires the Kokoro model and `sherpa-onnx` in `.tools/asr-venv`; see `worker/readme.md`.
    #[test]
    #[ignore = "requires the Kokoro model and sherpa-onnx"]
    fn synthesizes_a_sentence() {
        let mut worker = KokoroWorker::new(&KokoroConfig::from_root(&repository())).unwrap();
        let mut samples = 0;
        let mut rate = 0;
        worker
            .synthesize("Hello from Tomori.", &mut |chunk| {
                rate = chunk.sample_rate;
                samples += chunk.samples.len();
            })
            .unwrap();
        assert_eq!(rate, 24_000);
        assert!(samples > 24_000 / 4, "{samples}");
    }

    /// Speaks a sentence with Kokoro and recognizes it with Whisper. Requires both engines.
    #[test]
    #[ignore = "requires the Kokoro and faster-whisper engines"]
    fn round_trips_through_recognition() {
        use crate::stt::{RATE, Recognizer, Resampler, WhisperConfig, WhisperWorker};

        let root = repository();
        let mut worker = KokoroWorker::new(&KokoroConfig::from_root(&root)).unwrap();
        let mut audio = Vec::new();
        let mut resampler = None;
        worker
            .synthesize(
                "The quick brown fox jumps over the lazy dog.",
                &mut |chunk| {
                    resampler
                        .get_or_insert_with(|| Resampler::new(chunk.sample_rate, RATE))
                        .process(&chunk.samples, &mut audio);
                },
            )
            .unwrap();
        let mut whisper = WhisperWorker::new(&WhisperConfig::from_root(&root)).unwrap();
        let words = whisper.decode(&audio, None).unwrap();
        let text: String = words.iter().map(|word| word.text.as_str()).collect();
        let text = text.to_lowercase();
        assert!(text.contains("quick brown fox"), "{text}");
    }
}
