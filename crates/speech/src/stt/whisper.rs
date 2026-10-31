// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

//! Whisper recognition in-process through sherpa-onnx, for comparison with the streaming recognizer.
//!
//! The Whisper models that sherpa-onnx publishes are exported without cross-attention outputs, so they report no word times. Local-agreement commit needs word times to drop committed audio, so [`WhisperRecognizer`] transcribes whole windows and does not implement [`super::Recognizer`].

use std::path::{Path, PathBuf};

use sherpa_onnx::{OfflineRecognizer, OfflineRecognizerConfig, OfflineWhisperModelConfig};

use super::RATE;
use crate::Error;

/// Where to find a sherpa-onnx Whisper model and how to run it.
#[derive(Clone, Debug)]
pub struct WhisperConfig {
    /// The `sherpa-onnx-whisper-small.en` directory: `small.en-encoder.int8.onnx`, `small.en-decoder.int8.onnx` and `small.en-tokens.txt`.
    pub model: PathBuf,
    /// The prefix of the model's file names, such as `small.en`.
    pub name: String,
    pub language: String,
    /// The ONNX Runtime threads used for decoding.
    pub threads: u32,
    /// The ONNX Runtime execution provider: `cpu`, or `cuda` with the shared CUDA build of sherpa-onnx.
    pub provider: String,
}

impl WhisperConfig {
    /// The model fetched by `scripts/fetch-asr-model.sh whisper` under the repository `root`: `.tools/asr/sherpa-onnx-whisper-small.en`.
    pub fn from_root(root: &Path) -> Self {
        Self {
            model: root.join(".tools/asr/sherpa-onnx-whisper-small.en"),
            name: "small.en".into(),
            language: "en".into(),
            threads: 4,
            provider: "cpu".into(),
        }
    }

    fn sherpa(&self) -> Result<OfflineRecognizerConfig, Error> {
        let file = |suffix: &str| self.model.join(format!("{}-{suffix}", self.name));
        let files = [
            file("encoder.int8.onnx"),
            file("decoder.int8.onnx"),
            file("tokens.txt"),
        ];
        if !files.iter().all(|file| file.exists()) {
            return Err(Error::Unavailable(format!(
                "the Whisper model is not installed in {}; run bash scripts/fetch-asr-model.sh whisper",
                self.model.display()
            )));
        }
        let [encoder, decoder, tokens] =
            files.map(|file| Some(file.to_string_lossy().into_owned()));
        let mut config = OfflineRecognizerConfig::default();
        config.model_config.whisper = OfflineWhisperModelConfig {
            encoder,
            decoder,
            language: Some(self.language.clone()),
            task: Some("transcribe".into()),
            tail_paddings: -1,
            ..Default::default()
        };
        config.model_config.tokens = tokens;
        config.model_config.num_threads = i32::try_from(self.threads).unwrap_or(1).max(1);
        config.model_config.provider = Some(self.provider.clone());
        Ok(config)
    }
}

/// Whisper, loaded once and run in this process.
pub struct WhisperRecognizer {
    recognizer: OfflineRecognizer,
}

impl WhisperRecognizer {
    /// Load the model.
    pub fn new(config: &WhisperConfig) -> Result<Self, Error> {
        let recognizer = OfflineRecognizer::create(&config.sherpa()?).ok_or_else(|| {
            Error::Unavailable(format!(
                "sherpa-onnx could not load the Whisper model in {}",
                config.model.display()
            ))
        })?;
        Ok(Self { recognizer })
    }

    /// Transcribe `audio`, 16 kHz mono and at most 30 seconds long.
    pub fn transcribe(&mut self, audio: &[i16]) -> Result<String, Error> {
        if audio.len() > RATE as usize * 30 {
            return Err(Error::Engine("Whisper decodes at most 30 seconds".into()));
        }
        let samples: Vec<f32> = audio
            .iter()
            .map(|&sample| f32::from(sample) / 32768.0)
            .collect();
        let stream = self.recognizer.create_stream();
        stream.accept_waveform(RATE as i32, &samples);
        self.recognizer.decode(&stream);
        let result = stream
            .get_result()
            .ok_or_else(|| Error::Engine("Whisper returned no result".into()))?;
        Ok(result.text.trim().to_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reports_a_missing_installation() {
        let config = WhisperConfig::from_root(Path::new("/nonexistent"));
        assert!(matches!(
            WhisperRecognizer::new(&config),
            Err(Error::Unavailable(_))
        ));
    }

    /// Requires the Whisper model; see `readme.md`. Set `TOMORI_SPEECH_ROOT` to use models outside this checkout.
    #[test]
    #[ignore = "requires the Whisper model"]
    fn decodes_silence_without_failing() {
        let root = match std::env::var_os("TOMORI_SPEECH_ROOT") {
            Some(root) => PathBuf::from(root),
            None => Path::new(env!("CARGO_MANIFEST_DIR")).join("../.."),
        };
        let mut whisper = WhisperRecognizer::new(&WhisperConfig::from_root(&root)).unwrap();
        assert!(whisper.transcribe(&vec![0; RATE as usize]).is_ok());
    }
}
