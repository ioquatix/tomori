// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

//! Kokoro synthesis in-process through sherpa-onnx, on a thread owned by the synthesizer.

use std::{
    cell::Cell,
    path::{Path, PathBuf},
    rc::Rc,
    sync::mpsc::{self, Receiver, Sender},
    thread::{self, JoinHandle},
};

use sherpa_onnx::{
    GenerationConfig, OfflineTts, OfflineTtsConfig, OfflineTtsKokoroModelConfig,
    OfflineTtsModelConfig,
};

use super::{AudioChunk, Synthesizer};
use crate::Error;

/// The number of voices in the Kokoro multi-language v1.0 model.
const VOICES: u32 = 54;

/// The longest segment accepted, in characters. Segments come from [`crate::Speakable`], which yields sentences far shorter than this.
const TEXT_LIMIT: usize = 2000;

/// The files of the `kokoro-multi-lang-v1_0` model that synthesis uses.
const FILES: [&str; 5] = [
    "model.onnx",
    "voices.bin",
    "tokens.txt",
    "lexicon-us-en.txt",
    "espeak-ng-data",
];

/// Where to find the model and how to speak.
#[derive(Clone, Debug)]
pub struct KokoroConfig {
    /// The `kokoro-multi-lang-v1_0` directory: `model.onnx`, `voices.bin`, `tokens.txt`, `lexicon-us-en.txt` and `espeak-ng-data`.
    pub model: PathBuf,
    /// The speaker id, 0 to 53.
    pub voice: u32,
    pub speed: f32,
    /// The ONNX Runtime threads used for synthesis.
    pub threads: u32,
}

impl KokoroConfig {
    /// The model fetched by `scripts/fetch-tts-model.sh` under the repository `root`: `.tools/tts/kokoro-multi-lang-v1_0`.
    pub fn from_root(root: &Path) -> Self {
        Self {
            model: root.join(".tools/tts/kokoro-multi-lang-v1_0"),
            // `af_heart`.
            voice: 3,
            speed: 1.0,
            threads: 2,
        }
    }

    fn check(&self) -> Result<(), Error> {
        if self.voice >= VOICES {
            return Err(Error::Unavailable(format!(
                "Kokoro voice {} is out of range 0 to {}",
                self.voice,
                VOICES - 1
            )));
        }
        if !FILES.iter().all(|file| self.model.join(file).exists()) {
            return Err(Error::Unavailable(format!(
                "the Kokoro model is not installed in {}; run bash scripts/fetch-tts-model.sh",
                self.model.display()
            )));
        }
        Ok(())
    }

    fn sherpa(&self) -> OfflineTtsConfig {
        let file = |name: &str| Some(self.model.join(name).to_string_lossy().into_owned());
        OfflineTtsConfig {
            model: OfflineTtsModelConfig {
                kokoro: OfflineTtsKokoroModelConfig {
                    model: file("model.onnx"),
                    voices: file("voices.bin"),
                    tokens: file("tokens.txt"),
                    lexicon: file("lexicon-us-en.txt"),
                    // espeak-ng pronounces words that the lexicon lacks.
                    data_dir: file("espeak-ng-data"),
                    ..Default::default()
                },
                num_threads: i32::try_from(self.threads).unwrap_or(1).max(1),
                provider: Some("cpu".into()),
                debug: false,
                ..Default::default()
            },
            // Generate and deliver one sentence at a time, so the first sentence of a segment is heard before the rest is synthesized.
            max_num_sentences: 1,
            ..Default::default()
        }
    }
}

/// What the synthesis thread reports for a request.
enum Message {
    Audio(Vec<f32>),
    Done,
    Failed(Error),
}

struct Request {
    text: String,
    voice: i32,
    speed: f32,
    replies: Sender<Message>,
}

/// A [`Synthesizer`] that runs Kokoro in this process.
///
/// The model is loaded and run on a thread owned by the synthesizer, so synthesis never runs on the caller's scheduler. Each sentence's audio is passed to the caller as soon as it is generated. A segment that fails is reported to its caller and not retried.
pub struct KokoroSynthesizer {
    requests: Option<Sender<Request>>,
    thread: Option<JoinHandle<()>>,
    voice: u32,
    speed: f32,
    sample_rate: u32,
    speakers: u32,
}

impl KokoroSynthesizer {
    /// Load the model on a new synthesis thread, waiting until it is ready so that the first segment does not wait for it.
    pub fn new(config: &KokoroConfig) -> Result<Self, Error> {
        config.check()?;
        let sherpa = config.sherpa();
        let (ready_sender, ready) = mpsc::channel();
        let (requests, receiver) = mpsc::channel::<Request>();
        let thread = thread::Builder::new()
            .name("speech-kokoro".into())
            .spawn(move || {
                let Some(tts) = OfflineTts::create(&sherpa) else {
                    let _ = ready_sender.send(Err(Error::Unavailable(
                        "sherpa-onnx could not load the Kokoro model".into(),
                    )));
                    return;
                };
                let _ = ready_sender.send(Ok((tts.sample_rate(), tts.num_speakers())));
                serve(&tts, receiver);
            })
            .map_err(|error| Error::Unavailable(format!("could not start Kokoro: {error}")))?;
        let (sample_rate, speakers) = match ready.recv() {
            Ok(Ok(description)) => description,
            Ok(Err(error)) => {
                let _ = thread.join();
                return Err(error);
            }
            Err(_) => {
                let _ = thread.join();
                return Err(Error::Unavailable(
                    "the Kokoro thread stopped while loading the model".into(),
                ));
            }
        };
        let synthesizer = Self {
            requests: Some(requests),
            thread: Some(thread),
            voice: config.voice,
            speed: config.speed,
            sample_rate: u32::try_from(sample_rate).unwrap_or(0),
            speakers: u32::try_from(speakers).unwrap_or(0),
        };
        if synthesizer.sample_rate == 0 {
            return Err(Error::Unavailable(
                "the Kokoro model reports no sample rate".into(),
            ));
        }
        if synthesizer.voice >= synthesizer.speakers {
            return Err(Error::Unavailable(format!(
                "Kokoro voice {} is out of range; the model has {} voices",
                synthesizer.voice, synthesizer.speakers
            )));
        }
        Ok(synthesizer)
    }

    /// The sample rate of the audio produced, 24 kHz for Kokoro.
    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    /// The number of voices the model has.
    pub fn speakers(&self) -> u32 {
        self.speakers
    }
}

/// Serve requests on the synthesis thread until the synthesizer is dropped.
fn serve(tts: &OfflineTts, requests: Receiver<Request>) {
    for request in requests {
        let replies = request.replies.clone();
        let streamed = Rc::new(Cell::new(false));
        let callback = {
            let streamed = Rc::clone(&streamed);
            move |samples: &[f32], _progress: f32| {
                if samples.is_empty() {
                    return true;
                }
                streamed.set(true);
                // Stop early if the caller has gone away.
                replies.send(Message::Audio(samples.to_vec())).is_ok()
            }
        };
        let generation = GenerationConfig {
            sid: request.voice,
            speed: request.speed,
            ..Default::default()
        };
        let audio = tts.generate_with_config(&request.text, &generation, Some(callback));
        let reply = match audio {
            None => Message::Failed(Error::Engine("Kokoro could not synthesize the text".into())),
            Some(audio) if !streamed.get() => {
                // A build that does not call back still returns the whole segment.
                if audio.samples().is_empty() {
                    Message::Failed(Error::Engine("Kokoro produced no audio".into()))
                } else {
                    let _ = request
                        .replies
                        .send(Message::Audio(audio.samples().to_vec()));
                    Message::Done
                }
            }
            Some(_) => Message::Done,
        };
        let _ = request.replies.send(reply);
    }
}

/// Convert samples from -1 to 1 into 16-bit PCM.
fn to_pcm(samples: &[f32]) -> Vec<i16> {
    samples
        .iter()
        .map(|sample| (sample.clamp(-1.0, 1.0) * 32767.0).round() as i16)
        .collect()
}

/// The text Kokoro is given: trimmed, without NUL characters, which sherpa-onnx cannot accept.
fn prepare(text: &str) -> Result<String, Error> {
    let text = text.replace('\0', " ");
    let text = text.trim();
    if text.is_empty() || text.chars().count() > TEXT_LIMIT {
        return Err(Error::Engine(format!(
            "speech requires 1 to {TEXT_LIMIT} characters"
        )));
    }
    Ok(text.to_owned())
}

impl Synthesizer for KokoroSynthesizer {
    fn synthesize(&mut self, text: &str, output: &mut dyn FnMut(AudioChunk)) -> Result<(), Error> {
        let text = prepare(text)?;
        let stopped = || Error::Stopped("the Kokoro synthesis thread stopped".into());
        let (replies, receiver) = mpsc::channel();
        let request = Request {
            text,
            voice: i32::try_from(self.voice).expect("the voice was checked against the model"),
            speed: self.speed,
            replies,
        };
        self.requests
            .as_ref()
            .ok_or_else(stopped)?
            .send(request)
            .map_err(|_| stopped())?;
        loop {
            match receiver.recv() {
                Ok(Message::Audio(samples)) => output(AudioChunk {
                    sample_rate: self.sample_rate,
                    samples: to_pcm(&samples),
                }),
                Ok(Message::Done) => return Ok(()),
                Ok(Message::Failed(error)) => return Err(error),
                Err(_) => return Err(stopped()),
            }
        }
    }
}

impl Drop for KokoroSynthesizer {
    /// Waits for the segment being synthesized, if any, then unloads the model.
    fn drop(&mut self) {
        self.requests = None;
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn repository() -> PathBuf {
        match std::env::var_os("TOMORI_SPEECH_ROOT") {
            Some(root) => PathBuf::from(root),
            None => Path::new(env!("CARGO_MANIFEST_DIR")).join("../.."),
        }
    }

    #[test]
    fn converts_samples_to_pcm() {
        assert_eq!(
            to_pcm(&[0.0, 1.0, -1.0, 2.0, 0.5]),
            [0, 32767, -32767, 32767, 16384]
        );
    }

    #[test]
    fn prepares_text_for_the_engine() {
        assert_eq!(prepare(" Hello\0there. "), Ok("Hello there.".to_owned()));
        assert!(matches!(prepare("  "), Err(Error::Engine(_))));
        assert!(matches!(
            prepare(&"a".repeat(TEXT_LIMIT + 1)),
            Err(Error::Engine(_))
        ));
    }

    #[test]
    fn reports_a_missing_installation() {
        let config = KokoroConfig::from_root(Path::new("/nonexistent"));
        assert!(matches!(
            KokoroSynthesizer::new(&config),
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
            KokoroSynthesizer::new(&config),
            Err(Error::Unavailable(message)) if message.contains("voice")
        ));
    }

    /// Requires the Kokoro model; see `readme.md`. Set `TOMORI_SPEECH_ROOT` to use models outside this checkout.
    #[test]
    #[ignore = "requires the Kokoro model"]
    fn synthesizes_sentences_as_they_are_generated() {
        let mut synthesizer =
            KokoroSynthesizer::new(&KokoroConfig::from_root(&repository())).unwrap();
        assert_eq!(synthesizer.speakers(), VOICES);
        let mut chunks = Vec::new();
        synthesizer
            .synthesize(
                "Hello from Tomori. This is a second sentence.",
                &mut |chunk| chunks.push(chunk),
            )
            .unwrap();
        // Each sentence arrives as its own chunk.
        assert!(chunks.len() >= 2, "{} chunks", chunks.len());
        assert!(chunks.iter().all(|chunk| chunk.sample_rate == 24_000));
        let samples: usize = chunks.iter().map(|chunk| chunk.samples.len()).sum();
        assert!(samples > 24_000, "{samples}");
        // The synthesizer serves further segments.
        let mut more = 0;
        synthesizer
            .synthesize("Again.", &mut |chunk| more += chunk.samples.len())
            .unwrap();
        assert!(more > 0);
    }
}
