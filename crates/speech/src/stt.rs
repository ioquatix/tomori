// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

//! Streaming speech recognition for push-to-talk utterances.
//!
//! [`Recognition`] owns one [`StreamingRecognizer`] on a dedicated thread. Each utterance is driven through an [`Utterance`] handle: audio is pushed while the participant is still speaking, decoded once as it arrives, and reported through the utterance's event sink as [`TranscriptEvent::Partial`] results, followed by exactly one [`TranscriptEvent::Final`] or [`TranscriptEvent::Failed`]. The server uses the [`TransducerRecognizer`]. See `design/speech.md`.
//!
//! Utterances from several participants may be active at once; they share the recognizer and are decoded in turn. Finishing utterances take priority over partial decodes. An utterance whose audio waits for more than [`BACKLOG`] samples to be decoded fails with [`Error::Behind`].
//!
//! A panic in the recognizer disables recognition until the process restarts: it is logged once, and every open and later utterance fails with [`Error::Stopped`].

mod resample;
mod transducer;
mod whisper;

use std::{
    collections::VecDeque,
    panic::{self, AssertUnwindSafe},
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
        mpsc::{self, Receiver, Sender, TryRecvError},
    },
    thread::{self, JoinHandle},
};

use protocol::AudioFrame;

pub use resample::Resampler;
pub use transducer::{TransducerConfig, TransducerRecognizer};
pub use whisper::{WhisperConfig, WhisperRecognizer};

use crate::{
    Error, Stopper,
    stop::{Exited, panic_message},
};

/// The sample rate that recognizers receive.
pub const RATE: u32 = 16_000;

/// The most audio, in samples at [`RATE`], that one utterance may have waiting to be decoded: 5 seconds.
///
/// The streaming transducer holds back about a second of audio for its chunk and look-ahead, and on the reference Mac decodes at well under real time even when the machine is heavily loaded; see `readme.md`. An utterance more than 5 seconds behind shows partial words that are already stale and would deliver its transcript late by as much, so it fails visibly instead, and the audio waiting for one utterance stays bounded at 160 KB.
pub const BACKLOG: usize = RATE as usize * 5;

/// The transcript so far: committed words that will not change, and the latest tentative words after them.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Transcript {
    pub committed: String,
    pub tentative: String,
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
    Begin {
        key: u64,
        id: u64,
        sink: Sink,
        backlog: Arc<AtomicUsize>,
    },
    Audio {
        key: u64,
        samples: Vec<i16>,
    },
    /// The utterance's audio waiting to be decoded passed [`BACKLOG`].
    Behind {
        key: u64,
    },
    End {
        key: u64,
    },
    Cancel {
        key: u64,
    },
    /// Finish utterances that have ended, discard the rest, then exit.
    Shutdown,
    /// Fail every utterance and exit at once.
    Stop,
}

/// The recognition service: one recognizer thread shared by all utterances.
pub struct Recognition {
    commands: Sender<Command>,
    next: u64,
    thread: Option<JoinHandle<()>>,
    exited: Exited,
}

impl Recognition {
    /// Recognize with `recognizer`, which decodes each piece of audio once as it arrives.
    pub fn spawn(recognizer: impl StreamingRecognizer + 'static) -> Self {
        let (commands, receiver) = mpsc::channel();
        let exited = Exited::default();
        let guard = exited.guard();
        let thread = thread::Builder::new()
            .name("speech-recognition".into())
            .spawn(move || {
                // Dropped last, once the recognizer has been unloaded.
                let _guard = guard;
                Engine::new(Box::new(recognizer), receiver).run();
            })
            .expect("spawn the recognition thread");
        Self {
            commands,
            next: 0,
            thread: Some(thread),
            exited,
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
        let backlog = Arc::new(AtomicUsize::new(0));
        let _ = self.commands.send(Command::Begin {
            key,
            id,
            sink: Box::new(sink),
            backlog: Arc::clone(&backlog),
        });
        Utterance {
            key,
            commands: self.commands.clone(),
            resampler: Resampler::new(sample_rate, RATE),
            backlog,
            behind: false,
            open: true,
        }
    }

    /// A handle that stops the recognition thread: every open utterance fails with [`Error::Stopped`], the recognizer is unloaded, and the thread exits.
    pub fn stopper(&self) -> Stopper {
        let commands = self.commands.clone();
        Stopper::new(
            move || {
                let _ = commands.send(Command::Stop);
            },
            self.exited.clone(),
        )
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
    /// Samples sent and not yet decoded, shared with the recognition thread.
    backlog: Arc<AtomicUsize>,
    behind: bool,
    open: bool,
}

impl Utterance {
    /// Append audio at the utterance's sample rate. Once the audio waiting to be decoded passes [`BACKLOG`], the utterance fails with [`Error::Behind`] and later audio is not sent.
    pub fn push(&mut self, samples: &[i16]) {
        if self.behind {
            return;
        }
        let mut resampled = Vec::new();
        self.resampler.process(samples, &mut resampled);
        if resampled.is_empty() {
            return;
        }
        let waiting = self.backlog.fetch_add(resampled.len(), Ordering::SeqCst) + resampled.len();
        if waiting > BACKLOG {
            self.behind = true;
            let _ = self.commands.send(Command::Behind { key: self.key });
            return;
        }
        let _ = self.commands.send(Command::Audio {
            key: self.key,
            samples: resampled,
        });
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

    /// The participant finished speaking: decode the rest and deliver the final transcript.
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

struct Active {
    key: u64,
    id: u64,
    sink: Sink,
    stream: Box<dyn RecognitionStream>,
    ending: bool,
    reported: Transcript,
    /// Samples sent and not yet decoded, shared with the [`Utterance`].
    backlog: Arc<AtomicUsize>,
    /// Samples accepted since the last decode.
    accepted: usize,
}

impl Active {
    fn fail(mut self, error: Error) {
        (self.sink)(TranscriptEvent::Failed(error));
    }
}

struct Engine {
    /// The recognizer, until a panic disables recognition or the engine stops.
    recognizer: Option<Box<dyn StreamingRecognizer>>,
    commands: Receiver<Command>,
    /// Active utterances, in the order they will next be served.
    active: VecDeque<Active>,
    connected: bool,
    disabled: bool,
}

impl Engine {
    fn new(recognizer: Box<dyn StreamingRecognizer>, commands: Receiver<Command>) -> Self {
        Self {
            recognizer: Some(recognizer),
            commands,
            active: VecDeque::new(),
            connected: true,
            disabled: false,
        }
    }

    fn run(mut self) {
        loop {
            match panic::catch_unwind(AssertUnwindSafe(|| self.turn())) {
                Ok(true) => {}
                Ok(false) => return,
                Err(panic) => self.disable(&panic_message(&*panic)),
            }
        }
    }

    /// Apply the waiting commands and serve one utterance. Returns whether the engine keeps running.
    fn turn(&mut self) -> bool {
        // Apply every queued command before decoding, so that a slow decode coalesces the audio that arrived meanwhile.
        let work = self
            .active
            .iter()
            .any(|active| active.ending || active.stream.due());
        if !work {
            if !self.connected && self.active.is_empty() {
                return false;
            }
            match self.commands.recv() {
                Ok(command) => {
                    if !self.apply(command) {
                        return false;
                    }
                }
                Err(_) => self.disconnect(),
            }
        }
        loop {
            match self.commands.try_recv() {
                Ok(command) => {
                    if !self.apply(command) {
                        return false;
                    }
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    self.disconnect();
                    break;
                }
            }
        }
        self.step();
        true
    }

    /// Once the service shuts down, utterances that have not ended can never end.
    fn disconnect(&mut self) {
        if self.connected {
            self.connected = false;
            self.active.retain(|active| active.ending);
        }
    }

    /// Disable recognition after a panic: fail every open utterance, unload the recognizer, and fail later utterances at once.
    fn disable(&mut self, message: &str) {
        if !self.disabled {
            self.disabled = true;
            log::error!(
                "Speech recognition stopped after a panic ({message}); it is unavailable until the server restarts."
            );
        }
        let active = std::mem::take(&mut self.active);
        let recognizer = self.recognizer.take();
        // Failing an utterance or unloading the recognizer may panic again; that must not end the thread.
        let _ = panic::catch_unwind(AssertUnwindSafe(move || {
            for active in active {
                active.fail(Error::Stopped(UNAVAILABLE.into()));
            }
            drop(recognizer);
        }));
    }

    /// Fail every utterance and unload the recognizer, as the server is shutting down.
    fn stop(&mut self) {
        for active in std::mem::take(&mut self.active) {
            active.fail(Error::Stopped("the server is shutting down".into()));
        }
        self.recognizer = None;
    }

    /// Apply `command`. Returns whether the engine keeps running.
    fn apply(&mut self, command: Command) -> bool {
        match command {
            Command::Begin {
                key,
                id,
                mut sink,
                backlog,
            } => {
                let Some(recognizer) = self.recognizer.as_mut() else {
                    sink(TranscriptEvent::Failed(Error::Stopped(UNAVAILABLE.into())));
                    return true;
                };
                match recognizer.open() {
                    Ok(stream) => self.active.push_back(Active {
                        key,
                        id,
                        sink,
                        stream,
                        ending: false,
                        reported: Transcript::default(),
                        backlog,
                        accepted: 0,
                    }),
                    Err(error) => {
                        log::warn!("Transcription of utterance {id} could not start: {error}");
                        sink(TranscriptEvent::Failed(error));
                    }
                }
            }
            Command::Audio { key, samples } => {
                if let Some(active) = self.find(key)
                    && !active.ending
                {
                    active.stream.accept(&samples);
                    active.accepted += samples.len();
                }
            }
            Command::Behind { key } => {
                if let Some(index) = self.active.iter().position(|active| active.key == key) {
                    let active = self.active.remove(index).expect("index is in range");
                    log::warn!(
                        "Transcription of utterance {} fell too far behind the audio.",
                        active.id
                    );
                    active.fail(Error::Behind);
                }
            }
            Command::End { key } => {
                if let Some(active) = self.find(key) {
                    active.ending = true;
                }
            }
            Command::Cancel { key } => self.active.retain(|active| active.key != key),
            Command::Shutdown => self.disconnect(),
            Command::Stop => {
                self.stop();
                return false;
            }
        }
        true
    }

    fn find(&mut self, key: u64) -> Option<&mut Active> {
        self.active.iter_mut().find(|active| active.key == key)
    }

    /// Serve one utterance: finish the first that has ended, otherwise decode the next that is due.
    ///
    /// The utterance stays in `active` while the recognizer works on it, so that if the recognizer panics, [`Engine::disable`] reports the failure.
    fn step(&mut self) {
        let index = self
            .active
            .iter()
            .position(|active| active.ending)
            .or_else(|| self.active.iter().position(|active| active.stream.due()));
        let Some(index) = index else {
            return;
        };
        let active = &mut self.active[index];
        if active.ending {
            let result = active.stream.finish();
            let mut active = self.active.remove(index).expect("index is in range");
            match result {
                Ok(text) => (active.sink)(TranscriptEvent::Final(text)),
                Err(error) => {
                    log::warn!("Transcription of utterance {} failed: {error}", active.id);
                    active.fail(error);
                }
            }
            return;
        }
        match active.stream.decode() {
            Ok(()) => {
                active.backlog.fetch_sub(active.accepted, Ordering::SeqCst);
                active.accepted = 0;
                let transcript = active.stream.transcript();
                if transcript != active.reported {
                    active.reported = transcript.clone();
                    (active.sink)(TranscriptEvent::Partial(transcript));
                }
                // Rotate to the back so that concurrent utterances take turns.
                let active = self.active.remove(index).expect("index is in range");
                self.active.push_back(active);
            }
            Err(error) => {
                let active = self.active.remove(index).expect("index is in range");
                log::warn!("Transcription of utterance {} failed: {error}", active.id);
                active.fail(error);
            }
        }
    }
}

/// Why utterances fail once a panic has disabled recognition.
const UNAVAILABLE: &str = "speech recognition stopped after an internal error";

#[cfg(test)]
mod tests;
