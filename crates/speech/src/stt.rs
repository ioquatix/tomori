// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

//! Streaming speech recognition for push-to-talk utterances.
//!
//! [`Recognition`] owns one recognizer on a dedicated thread. Each utterance is driven through an [`Utterance`] handle: audio is pushed while the participant is still speaking, decoded incrementally, and reported through the utterance's event sink as [`TranscriptEvent::Partial`] results, followed by exactly one [`TranscriptEvent::Final`] or [`TranscriptEvent::Failed`]. See `design/speech.md`.
//!
//! The recognizer is either a [`StreamingRecognizer`], which keeps per-utterance state and decodes each piece of audio once as it arrives, or a window [`Recognizer`], which is decoded repeatedly over the uncommitted audio with local-agreement commit. The server uses the streaming [`TransducerRecognizer`].
//!
//! Utterances from several participants may be active at once; they share the recognizer and are decoded in turn. Finishing utterances take priority over partial decodes.

mod policy;
mod resample;
mod transducer;
mod whisper;

use std::{
    collections::VecDeque,
    sync::mpsc::{self, Receiver, Sender, TryRecvError},
    thread::{self, JoinHandle},
    time::Duration,
};

use protocol::AudioFrame;

pub use policy::Transcript;
pub use resample::Resampler;
pub use transducer::{TransducerConfig, TransducerRecognizer};
pub use whisper::{WhisperConfig, WhisperRecognizer};

use crate::Error;
use policy::Policy;

/// The sample rate that recognizers receive.
pub const RATE: u32 = 16_000;

/// A recognized word with times in seconds from the start of the decoded window.
#[derive(Clone, Debug, PartialEq)]
pub struct Word {
    /// The word as the engine produced it, possibly with leading space and punctuation.
    pub text: String,
    pub start: f32,
    pub end: f32,
}

/// A window speech recognition engine, which decodes a whole window of audio at once and reports word times. [`Recognition::spawn`] decodes it incrementally with local-agreement commit.
pub trait Recognizer: Send {
    /// Decode `audio` (16 kHz mono) into words. `prompt` is the preceding committed text of the same utterance, for context.
    fn decode(&mut self, audio: &[i16], prompt: Option<&str>) -> Result<Vec<Word>, Error>;
}

impl<R: Recognizer + ?Sized> Recognizer for Box<R> {
    fn decode(&mut self, audio: &[i16], prompt: Option<&str>) -> Result<Vec<Word>, Error> {
        (**self).decode(audio, prompt)
    }
}

/// A streaming speech recognition engine, which decodes audio as it arrives and keeps each utterance's state between pieces of audio, so nothing is decoded twice.
pub trait StreamingRecognizer: Send {
    /// Open the recognition stream of a new utterance.
    fn open(&mut self) -> Result<Box<dyn RecognitionStream>, Error>;
}

impl<R: StreamingRecognizer + ?Sized> StreamingRecognizer for Box<R> {
    fn open(&mut self) -> Result<Box<dyn RecognitionStream>, Error> {
        (**self).open()
    }
}

/// The recognition state of one utterance of a [`StreamingRecognizer`].
pub trait RecognitionStream: Send {
    /// Append 16 kHz mono audio. It is decoded by [`RecognitionStream::decode`].
    fn accept(&mut self, samples: &[i16]);
    /// Whether accepted audio is ready to be decoded.
    fn due(&self) -> bool;
    /// Decode all accepted audio that is ready.
    fn decode(&mut self) -> Result<(), Error>;
    /// The transcript so far.
    fn transcript(&self) -> Transcript;
    /// The participant finished speaking: decode the rest of the accepted audio and return the complete transcript.
    fn finish(&mut self) -> Result<String, Error>;
}

/// Tuning for incremental decoding of a window [`Recognizer`].
#[derive(Clone, Debug)]
pub struct RecognizerConfig {
    /// Decode again after this much new audio.
    pub refresh: Duration,
    /// Shorter windows are not decoded until the utterance ends.
    pub minimum_audio: Duration,
    /// The longest uncommitted window. Whisper decodes at most 30 seconds.
    pub maximum_window: Duration,
    /// When a full window has no agreement, commit words ending at least this long before the end of the window.
    pub force_margin: Duration,
    /// Root-mean-square level (0 to 1) below which every 100 ms frame counts as silence.
    pub silence_threshold: f32,
    /// The number of committed words passed to the recognizer as context.
    pub prompt_words: usize,
}

impl Default for RecognizerConfig {
    fn default() -> Self {
        Self {
            refresh: Duration::from_secs(1),
            minimum_audio: Duration::from_millis(330),
            maximum_window: Duration::from_secs(24),
            force_margin: Duration::from_secs(4),
            silence_threshold: 0.003,
            prompt_words: 40,
        }
    }
}

/// Progress of one utterance.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TranscriptEvent {
    /// The transcript so far, sent while the participant is still speaking.
    Partial(Transcript),
    /// The complete transcript. It may be empty if nothing was said.
    Final(String),
    /// No transcript could be produced; nothing should be sent to the agent.
    Failed(Error),
}

type Sink = Box<dyn FnMut(TranscriptEvent) + Send>;

enum Command {
    Begin { key: u64, id: u64, sink: Sink },
    Audio { key: u64, samples: Vec<i16> },
    End { key: u64 },
    Cancel { key: u64 },
    Shutdown,
}

/// The recognition service: one recognizer thread shared by all utterances.
pub struct Recognition {
    commands: Sender<Command>,
    next: u64,
    thread: Option<JoinHandle<()>>,
}

impl Recognition {
    /// Recognize with a window `recognizer`, decoded incrementally with local-agreement commit as `config` describes.
    pub fn spawn(recognizer: impl Recognizer + 'static, config: RecognizerConfig) -> Self {
        Self::with_decoder(Decoder::Window {
            recognizer: Box::new(recognizer),
            config,
        })
    }

    /// Recognize with a streaming `recognizer`, which decodes each piece of audio once as it arrives.
    pub fn spawn_streaming(recognizer: impl StreamingRecognizer + 'static) -> Self {
        Self::with_decoder(Decoder::Streaming(Box::new(recognizer)))
    }

    fn with_decoder(decoder: Decoder) -> Self {
        let (commands, receiver) = mpsc::channel();
        let thread = thread::Builder::new()
            .name("speech-recognition".into())
            .spawn(move || Engine::new(decoder, receiver).run())
            .expect("spawn the recognition thread");
        Self {
            commands,
            next: 0,
            thread: Some(thread),
        }
    }

    /// Begin an utterance whose audio arrives at `sample_rate`. `id` identifies it in logs. Events are delivered to `sink` on the recognition thread, so it should not block.
    pub fn begin(
        &mut self,
        id: u64,
        sample_rate: u32,
        sink: impl FnMut(TranscriptEvent) + Send + 'static,
    ) -> Utterance {
        self.next += 1;
        let key = self.next;
        let _ = self.commands.send(Command::Begin {
            key,
            id,
            sink: Box::new(sink),
        });
        Utterance {
            key,
            commands: self.commands.clone(),
            resampler: Resampler::new(sample_rate, RATE),
            open: true,
        }
    }
}

impl Drop for Recognition {
    /// Waits for utterances that have already ended to finish; utterances still open are discarded without events.
    fn drop(&mut self) {
        let _ = self.commands.send(Command::Shutdown);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// One utterance being recognized. Dropping it without calling [`Utterance::end`] cancels it.
pub struct Utterance {
    key: u64,
    commands: Sender<Command>,
    resampler: Resampler,
    open: bool,
}

impl Utterance {
    /// Append audio at the utterance's sample rate.
    pub fn push(&mut self, samples: &[i16]) {
        let mut resampled = Vec::new();
        self.resampler.process(samples, &mut resampled);
        if !resampled.is_empty() {
            let _ = self.commands.send(Command::Audio {
                key: self.key,
                samples: resampled,
            });
        }
    }

    /// Append the samples of a frame, adapting to its sample rate.
    ///
    /// The caller is responsible for frame order: a missing frame must fail the utterance (`design/live-audio.md`), for example by cancelling it and reporting the gap.
    pub fn push_frame(&mut self, frame: &AudioFrame) {
        if frame.sample_rate != self.resampler.input_rate() {
            self.resampler = Resampler::new(frame.sample_rate, RATE);
        }
        self.push(&frame.samples);
    }

    /// The participant finished speaking: decode the uncommitted tail and deliver the final transcript.
    pub fn end(mut self) {
        self.open = false;
        let _ = self.commands.send(Command::End { key: self.key });
    }

    /// Discard the utterance without a transcript. No further events are delivered.
    pub fn cancel(mut self) {
        self.open = false;
        let _ = self.commands.send(Command::Cancel { key: self.key });
    }
}

impl Drop for Utterance {
    fn drop(&mut self) {
        if self.open {
            let _ = self.commands.send(Command::Cancel { key: self.key });
        }
    }
}

/// The recognizer and how utterances are decoded with it.
enum Decoder {
    Window {
        recognizer: Box<dyn Recognizer>,
        config: RecognizerConfig,
    },
    Streaming(Box<dyn StreamingRecognizer>),
}

/// The decoding state of one utterance.
enum State {
    Window(Policy),
    Streaming(Box<dyn RecognitionStream>),
}

impl State {
    fn push(&mut self, samples: &[i16]) {
        match self {
            Self::Window(policy) => policy.push(samples),
            Self::Streaming(stream) => stream.accept(samples),
        }
    }

    fn due(&self) -> bool {
        match self {
            Self::Window(policy) => policy.due(),
            Self::Streaming(stream) => stream.due(),
        }
    }

    fn transcript(&self) -> Transcript {
        match self {
            Self::Window(policy) => policy.transcript(),
            Self::Streaming(stream) => stream.transcript(),
        }
    }

    /// Decode what is due.
    fn decode(&mut self, decoder: &mut Decoder) -> Result<(), Error> {
        match (self, decoder) {
            (Self::Window(policy), Decoder::Window { recognizer, .. }) => {
                policy.decode(&mut **recognizer)
            }
            (Self::Streaming(stream), _) => stream.decode(),
            (Self::Window(_), Decoder::Streaming(_)) => {
                unreachable!("window state is only created for a window decoder")
            }
        }
    }

    /// Decode the rest and return the complete transcript.
    fn finish(&mut self, decoder: &mut Decoder) -> Result<String, Error> {
        match (self, decoder) {
            (Self::Window(policy), Decoder::Window { recognizer, .. }) => {
                policy.finish(&mut **recognizer)
            }
            (Self::Streaming(stream), _) => stream.finish(),
            (Self::Window(_), Decoder::Streaming(_)) => {
                unreachable!("window state is only created for a window decoder")
            }
        }
    }
}

struct Active {
    key: u64,
    id: u64,
    sink: Sink,
    state: State,
    ending: bool,
    reported: Transcript,
}

struct Engine {
    decoder: Decoder,
    commands: Receiver<Command>,
    /// Active utterances, in the order they will next be served.
    active: VecDeque<Active>,
    connected: bool,
}

impl Engine {
    fn new(decoder: Decoder, commands: Receiver<Command>) -> Self {
        Self {
            decoder,
            commands,
            active: VecDeque::new(),
            connected: true,
        }
    }

    fn run(mut self) {
        loop {
            // Apply every queued command before decoding, so that a slow decode coalesces the audio that arrived meanwhile.
            let work = self
                .active
                .iter()
                .any(|active| active.ending || active.state.due());
            if !work {
                if !self.connected && self.active.is_empty() {
                    return;
                }
                match self.commands.recv() {
                    Ok(command) => self.apply(command),
                    Err(_) => {
                        self.disconnect();
                        continue;
                    }
                }
            }
            loop {
                match self.commands.try_recv() {
                    Ok(command) => self.apply(command),
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => {
                        self.disconnect();
                        break;
                    }
                }
            }
            self.step();
        }
    }

    /// Once the service shuts down, utterances that have not ended can never end.
    fn disconnect(&mut self) {
        if self.connected {
            self.connected = false;
            self.active.retain(|active| active.ending);
        }
    }

    fn apply(&mut self, command: Command) {
        match command {
            Command::Begin { key, id, mut sink } => {
                let state = match &mut self.decoder {
                    Decoder::Window { config, .. } => State::Window(Policy::new(config.clone())),
                    Decoder::Streaming(recognizer) => match recognizer.open() {
                        Ok(stream) => State::Streaming(stream),
                        Err(error) => {
                            log::warn!("Transcription of utterance {id} could not start: {error}");
                            sink(TranscriptEvent::Failed(error));
                            return;
                        }
                    },
                };
                self.active.push_back(Active {
                    key,
                    id,
                    sink,
                    state,
                    ending: false,
                    reported: Transcript::default(),
                });
            }
            Command::Audio { key, samples } => {
                if let Some(active) = self.find(key)
                    && !active.ending
                {
                    active.state.push(&samples);
                }
            }
            Command::End { key } => {
                if let Some(active) = self.find(key) {
                    active.ending = true;
                }
            }
            Command::Cancel { key } => self.active.retain(|active| active.key != key),
            Command::Shutdown => self.disconnect(),
        }
    }

    fn find(&mut self, key: u64) -> Option<&mut Active> {
        self.active.iter_mut().find(|active| active.key == key)
    }

    /// Serve one utterance: finish the first that has ended, otherwise decode the next that is due.
    fn step(&mut self) {
        let index = self
            .active
            .iter()
            .position(|active| active.ending)
            .or_else(|| self.active.iter().position(|active| active.state.due()));
        let Some(index) = index else {
            return;
        };
        let mut active = self.active.remove(index).expect("index is in range");
        if active.ending {
            match active.state.finish(&mut self.decoder) {
                Ok(text) => (active.sink)(TranscriptEvent::Final(text)),
                Err(error) => {
                    log::warn!("Transcription of utterance {} failed: {error}", active.id);
                    (active.sink)(TranscriptEvent::Failed(error));
                }
            }
            return;
        }
        match active.state.decode(&mut self.decoder) {
            Ok(()) => {
                let transcript = active.state.transcript();
                if transcript != active.reported {
                    active.reported = transcript.clone();
                    (active.sink)(TranscriptEvent::Partial(transcript));
                }
                // Rotate to the back so that concurrent utterances take turns.
                self.active.push_back(active);
            }
            Err(error) => {
                log::warn!("Transcription of utterance {} failed: {error}", active.id);
                (active.sink)(TranscriptEvent::Failed(error));
            }
        }
    }
}

#[cfg(test)]
mod tests;
