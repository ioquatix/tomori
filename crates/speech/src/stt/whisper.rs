// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

//! Whisper recognition through a supervised faster-whisper worker process (`worker/asr_worker.py`).

use std::{
    ffi::OsString,
    path::{Path, PathBuf},
};

use base64::{Engine as _, engine::general_purpose::STANDARD};
use serde::Deserialize;
use serde_json::{Value, json};

use super::{Recognizer, Word};
use crate::{
    Error,
    worker::{Flow, Worker, WorkerCommand},
};

/// Where to find the Python environment, worker script and model.
#[derive(Clone, Debug)]
pub struct WhisperConfig {
    pub python: PathBuf,
    pub script: PathBuf,
    /// A CTranslate2 Whisper model directory containing `model.bin`.
    pub model: PathBuf,
    /// `cuda` or `cpu`; detected by the worker when `None`.
    pub device: Option<String>,
    pub language: String,
    /// Directories prepended to `LD_LIBRARY_PATH`, for CUDA libraries installed in the environment.
    pub library_path: Vec<PathBuf>,
}

impl WhisperConfig {
    /// The layout created by `scripts/setup-pc-transcription.sh` under the repository `root`: `.tools/asr-venv` and `.tools/asr/faster-whisper-small.en`.
    pub fn from_root(root: &Path) -> Self {
        let venv = root.join(".tools/asr-venv");
        let mut library_path = Vec::new();
        if let Ok(entries) = std::fs::read_dir(venv.join("lib")) {
            for entry in entries.flatten() {
                let nvidia = entry.path().join("site-packages/nvidia");
                if entry.file_name().to_string_lossy().starts_with("python3.") {
                    library_path.push(nvidia.join("cublas/lib"));
                    library_path.push(nvidia.join("cudnn/lib"));
                }
            }
        }
        Self {
            python: venv.join("bin/python"),
            script: Path::new(env!("CARGO_MANIFEST_DIR")).join("worker/asr_worker.py"),
            model: root.join(".tools/asr/faster-whisper-small.en"),
            device: None,
            language: "en".into(),
            library_path,
        }
    }

    fn command(&self) -> Result<WorkerCommand, Error> {
        if !self.python.exists() || !self.model.join("model.bin").exists() {
            return Err(Error::Unavailable(
                "PC transcription is not installed; run bash scripts/setup-pc-transcription.sh"
                    .into(),
            ));
        }
        if !self.script.exists() {
            return Err(Error::Unavailable(format!(
                "missing worker script {}",
                self.script.display()
            )));
        }
        let mut command = WorkerCommand::new("whisper", &self.python)
            .argument(&self.script)
            .env("TOMORI_ASR_MODEL", &self.model)
            .env("TOMORI_ASR_LANGUAGE", &self.language);
        if let Some(device) = &self.device {
            command = command.env("TOMORI_ASR_DEVICE", device);
        }
        if !self.library_path.is_empty() {
            let mut paths = self.library_path.clone();
            if let Some(existing) = std::env::var_os("LD_LIBRARY_PATH") {
                paths.extend(std::env::split_paths(&existing));
            }
            let joined: OsString = std::env::join_paths(paths)
                .map_err(|error| Error::Unavailable(error.to_string()))?;
            command = command.env("LD_LIBRARY_PATH", joined);
        }
        Ok(command)
    }
}

/// A [`Recognizer`] backed by the faster-whisper worker. The worker is started on first use (or by [`WhisperWorker::start`]) and restarted after a failure.
pub struct WhisperWorker {
    worker: Worker,
}

#[derive(Deserialize)]
struct WireWord {
    text: String,
    start: f32,
    end: f32,
}

impl WhisperWorker {
    /// Check that the environment is installed. The worker is not started yet.
    pub fn new(config: &WhisperConfig) -> Result<Self, Error> {
        Ok(Self {
            worker: Worker::new(config.command()?),
        })
    }

    /// Start the worker and wait for its model to load, so that the first utterance does not wait for it.
    pub fn start(&mut self) -> Result<(), Error> {
        self.worker.start()
    }

    /// The worker's ready message, describing the model and device.
    pub fn description(&self) -> Option<&Value> {
        self.worker.ready()
    }
}

impl Recognizer for WhisperWorker {
    fn decode(&mut self, audio: &[i16], prompt: Option<&str>) -> Result<Vec<Word>, Error> {
        let bytes: Vec<u8> = audio
            .iter()
            .flat_map(|sample| sample.to_le_bytes())
            .collect();
        let request = json!({ "audio": STANDARD.encode(bytes), "prompt": prompt });
        let mut words = None;
        self.worker.request(request, |message| {
            let list = message
                .get("words")
                .cloned()
                .ok_or_else(|| Error::Worker(format!("whisper worker wrote {message}")))?;
            let list: Vec<WireWord> = serde_json::from_value(list)
                .map_err(|error| Error::Worker(format!("whisper worker words: {error}")))?;
            words = Some(
                list.into_iter()
                    .map(|word| Word {
                        text: word.text,
                        start: word.start,
                        end: word.end,
                    })
                    .collect(),
            );
            Ok(Flow::Done)
        })?;
        Ok(words.unwrap_or_default())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Requires `scripts/setup-pc-transcription.sh`. Run from the repository with `cargo test -p speech -- --ignored`.
    #[test]
    #[ignore = "requires the faster-whisper environment and model"]
    fn decodes_silence_and_tone_without_failing() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let mut worker = WhisperWorker::new(&WhisperConfig::from_root(&root)).unwrap();
        worker.start().unwrap();
        let audio = vec![0i16; 16_000];
        assert!(worker.decode(&audio, None).is_ok());
    }

    #[test]
    fn reports_a_missing_installation() {
        let config = WhisperConfig::from_root(Path::new("/nonexistent"));
        assert!(matches!(
            WhisperWorker::new(&config),
            Err(Error::Unavailable(_))
        ));
    }
}
