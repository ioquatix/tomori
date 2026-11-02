// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

//! Whole-clip speech recognition for push-to-talk utterances.
//!
//! The [`OfflineRecognizer`] decodes a complete clip, once, so it is more accurate than the streaming [`super::StreamingRecognizer`]. The cost is latency: nothing is reported until the participant stops speaking, and the final transcript arrives once the whole clip is decoded, a couple of seconds after they let go, instead of almost at once.
//!
//! [`OfflineRecognition`] owns one recognizer on a dedicated thread. Each utterance is driven through an [`OfflineUtterance`]: audio is buffered and resampled to 16 kHz, and when the participant stops the clip is decoded once and reported through the utterance's sink as exactly one [`TranscriptEvent::Final`] or [`TranscriptEvent::Failed`]. There are no partial events.
//!
//! A panic in the recognizer disables recognition until the process restarts: it is logged once, and every open and later utterance fails with [`Error::Stopped`].

use std::{
    collections::VecDeque,
    panic::{self, AssertUnwindSafe},
    sync::mpsc::{self, Receiver, Sender},
    thread::{self, JoinHandle},
};

use protocol::AudioFrame;

use crate::{
    Error, Stopper,
    stop::Exited,
    stt::{RATE, Resampler, TranscriptEvent},
};

/// The most audio, in samples at [`RATE`], that one utterance may buffer: 30 seconds, the offline recognizer's limit.
///
/// Push-to-talk utterances are a few seconds; one that would buffer more than this is failing to end, so its audio is cut at the limit and it fails visibly when it ends.
const MAX_SAMPLES: usize = RATE as usize * 30;

/// Why an utterance fails once recognition has stopped: a panic disabled it.
const STOPPED_MESSAGE: &str = "speech recognition stopped after an internal error";

type Sink = Box<dyn FnMut(TranscriptEvent) + Send>;

/// An offline speech recognition engine, which decodes a complete clip, once. It cannot see a clip that is still being spoken, so it reports nothing while the participant is speaking.
pub trait OfflineRecognizer: Send {
    /// Transcribe the complete 16 kHz mono clip.
    fn transcribe(&mut self, samples: &[i16]) -> Result<String, Error>;
}

impl<R: OfflineRecognizer + ?Sized> OfflineRecognizer for Box<R> {
    fn transcribe(&mut self, samples: &[i16]) -> Result<String, Error> {
        (**self).transcribe(samples)
    }
}

enum Command {
    Begin {
        key: u64,
        sink: Sink,
    },
    Audio {
        key: u64,
        samples: Vec<i16>,
    },
    End {
        key: u64,
    },
    Cancel {
        key: u64,
    },
    /// Fail every utterance and exit at once.
    Stop,
}

/// One utterance being buffered for a whole-clip decode.
struct OfflineEntry {
    key: u64,
    sink: Sink,
    /// The audio of the utterance, resampled to 16 kHz.
    samples: Vec<i16>,
    /// Whether the participant has stopped speaking; the clip is ready to decode.
    ending: bool,
}

struct OfflineEngine<R: OfflineRecognizer> {
    /// The recognizer, until a panic disables it or the engine stops.
    recognizer: Option<R>,
    commands: Receiver<Command>,
    /// Buffered utterances, in the order they began.
    active: VecDeque<OfflineEntry>,
    /// A panic has disabled recognition; every later utterance fails at once.
    disabled: bool,
}

impl<R: OfflineRecognizer> OfflineEngine<R> {
    fn new(commands: Receiver<Command>, recognizer: R) -> Self {
        Self {
            recognizer: Some(recognizer),
            commands,
            active: VecDeque::new(),
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

    /// Apply the waiting commands, then decode the first utterance that has ended. Returns whether the engine keeps running.
    fn turn(&mut self) -> bool {
        let command = match self.commands.recv() {
            Ok(command) => command,
            Err(_) => {
                self.drain();
                return false;
            }
        };
        if !self.apply(command) {
            return false;
        }
        while let Ok(command) = self.commands.try_recv() {
            if !self.apply(command) {
                return false;
            }
        }
        self.step();
        true
    }
    /// Decode the first utterance that has ended.
    fn step(&mut self) {
        let Some(index) = self.active.iter().position(|entry| entry.ending) else {
            return;
        };
        let mut entry = self.active.remove(index).expect("index is in range");
        self.finish(&mut entry);
    }

    /// Decode `entry`'s buffered audio and report the result to its sink.
    fn finish(&mut self, entry: &mut OfflineEntry) {
        if entry.samples.is_empty() {
            // Nothing was said; the transcript is empty, as the streaming path reports it.
            (entry.sink)(TranscriptEvent::Final(String::new()));
            return;
        }
        let text = match panic::catch_unwind(AssertUnwindSafe(|| self.decode(&entry.samples))) {
            Ok(text) => text,
            Err(panic) => {
                self.disable(&panic_message(&*panic));
                (entry.sink)(TranscriptEvent::Failed(Error::Stopped(
                    STOPPED_MESSAGE.into(),
                )));
                return;
            }
        };
        match text {
            Ok(text) => (entry.sink)(TranscriptEvent::Final(text)),
            Err(error) => {
                log::warn!("Transcription of utterance {} failed: {error}", entry.key);
                (entry.sink)(TranscriptEvent::Failed(error));
            }
        }
    }

    fn decode(&mut self, samples: &[i16]) -> Result<String, Error> {
        self.recognizer
            .as_mut()
            .ok_or_else(|| Error::Stopped(STOPPED_MESSAGE.to_owned()))?
            .transcribe(samples)
    }

    /// Fail every open utterance and unload the recognizer.
    fn drain(&mut self) {
        for mut entry in self.active.drain(..) {
            (entry.sink)(TranscriptEvent::Failed(Error::Stopped(
                "the server is shutting down".into(),
            )));
        }
        self.recognizer = None;
    }

    /// Disable recognition after a panic: fail every open utterance and unload the recognizer.
    fn disable(&mut self, message: &str) {
        if !self.disabled {
            self.disabled = true;
            log::error!(
                "Speech recognition stopped after a panic ({message}); it is unavailable until the server restarts."
            );
        }
        self.drain();
    }

    /// Apply `command`. Returns whether the engine keeps running.
    fn apply(&mut self, command: Command) -> bool {
        match command {
            Command::Begin { key, mut sink } => {
                if self.recognizer.is_none() {
                    sink(TranscriptEvent::Failed(Error::Stopped(
                        STOPPED_MESSAGE.into(),
                    )));
                    return true;
                }
                self.active.push_back(OfflineEntry {
                    key,
                    sink,
                    samples: Vec::new(),
                    ending: false,
                });
            }
            Command::Audio { key, samples } => {
                if let Some(entry) = self.find(key)
                    && !entry.ending
                {
                    if entry.samples.len() + samples.len() > MAX_SAMPLES {
                        // The clip is longer than the recognizer will decode; it fails when it ends.
                        entry.samples.truncate(MAX_SAMPLES);
                        (entry.sink)(TranscriptEvent::Failed(Error::Engine(format!(
                            "Your speech was longer than {} seconds, so it could not be transcribed.",
                            MAX_SAMPLES / RATE as usize
                        ))));
                        entry.ending = true;
                        return true;
                    }
                    entry.samples.extend(samples);
                }
            }
            Command::End { key } => {
                if let Some(entry) = self.find(key) {
                    entry.ending = true;
                }
            }
            Command::Cancel { key } => {
                self.active.retain(|entry| entry.key != key);
            }
            Command::Stop => {
                self.drain();
                return false;
            }
        }
        true
    }

    fn find(&mut self, key: u64) -> Option<&mut OfflineEntry> {
        self.active.iter_mut().find(|entry| entry.key == key)
    }
}

/// The offline recognition service: one recognizer thread shared by all utterances.
pub struct OfflineRecognition {
    commands: Sender<Command>,
    next: u64,
    exited: Exited,
    /// Join the decode thread when the server stops it, so a long decode does not outlive the server.
    thread: Option<JoinHandle<()>>,
}

impl OfflineRecognition {
    /// Recognize with `recognizer`, which decodes each clip once, whole.
    pub fn spawn(recognizer: impl OfflineRecognizer + 'static) -> Self {
        let (commands, receiver) = mpsc::channel();
        let exited = Exited::default();
        let guard = exited.guard();
        let thread = thread::Builder::new()
            .name("speech-recognition-offline".into())
            .spawn(move || {
                // Dropped last, once the recognizer has been unloaded.
                let _guard = guard;
                OfflineEngine::new(receiver, recognizer).run();
            })
            .expect("spawn the offline recognition thread");
        Self {
            commands,
            next: 0,
            exited,
            thread: Some(thread),
        }
    }

    /// Begin an utterance whose audio arrives at `sample_rate`. Events are delivered to `sink` on the recognition thread, so it should not block.
    pub fn begin(
        &mut self,
        sample_rate: u32,
        sink: impl FnMut(TranscriptEvent) + Send + 'static,
    ) -> OfflineUtterance {
        self.next += 1;
        let key = self.next;
        let _ = self.commands.send(Command::Begin {
            key,
            sink: Box::new(sink),
        });
        OfflineUtterance {
            key,
            commands: self.commands.clone(),
            resampler: Resampler::new(sample_rate, RATE),
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

impl Drop for OfflineRecognition {
    /// Waits for the decode thread to finish; in-progress decodes are abandoned and the recognizer is unloaded.
    fn drop(&mut self) {
        let _ = self.commands.send(Command::Stop);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// One utterance being buffered for a whole-clip decode. Dropping it without calling [`OfflineUtterance::end`] cancels it.
pub struct OfflineUtterance {
    key: u64,
    commands: Sender<Command>,
    /// Resamples the utterance's audio to 16 kHz between frames.
    resampler: Resampler,
    open: bool,
}

impl OfflineUtterance {
    /// Append audio at the utterance's sample rate; it is buffered until the utterance ends.
    pub fn push(&mut self, samples: &[i16]) {
        if !self.open {
            return;
        }
        let mut resampled = Vec::new();
        self.resampler.process(samples, &mut resampled);
        if resampled.is_empty() {
            return;
        }
        let _ = self.commands.send(Command::Audio {
            key: self.key,
            samples: resampled,
        });
    }

    /// Append the samples of a frame, adapting to its sample rate.
    ///
    /// The caller is responsible for frame order; a missing frame must fail the utterance (`design/live-audio.md`), for example by cancelling it.
    pub fn push_frame(&mut self, frame: &AudioFrame) {
        if frame.sample_rate != self.resampler.input_rate() {
            self.resampler = Resampler::new(frame.sample_rate, RATE);
        }
        self.push(&frame.samples);
    }

    /// The participant finished speaking: decode the buffered clip and deliver the final transcript.
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

impl Drop for OfflineUtterance {
    fn drop(&mut self) {
        if self.open {
            let _ = self.commands.send(Command::Cancel { key: self.key });
        }
    }
}

/// The message of a caught panic.
fn panic_message(panic: &(dyn std::any::Any + Send)) -> String {
    if let Some(message) = panic.downcast_ref::<&str>() {
        (*message).to_owned()
    } else if let Some(message) = panic.downcast_ref::<String>() {
        message.clone()
    } else {
        "unknown panic".to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    /// A fake offline recognizer that returns `text` for every clip.
    struct Fake(String);

    impl OfflineRecognizer for Fake {
        fn transcribe(&mut self, _samples: &[i16]) -> Result<String, Error> {
            Ok(self.0.clone())
        }
    }

    #[test]
    fn decodes_the_buffered_clip_when_it_ends() {
        let (sender, receiver) = mpsc::channel();
        let mut offline = OfflineRecognition::spawn(Fake("the final words".to_owned()));
        let mut handle = offline.begin(RATE, move |event| {
            let _ = sender.send(event);
        });
        // No event is delivered while the participant is speaking.
        handle.push(&vec![0; 1600]);
        assert!(receiver.try_recv().is_err());
        handle.end();
        let event = receiver.recv().unwrap();
        assert!(
            matches!(&event, TranscriptEvent::Final(text) if text == "the final words"),
            "expected Final, got {event:?}"
        );
    }

    #[test]
    fn a_cancelled_utterance_reports_nothing() {
        let (sender, receiver) = mpsc::channel();
        let mut offline = OfflineRecognition::spawn(Fake("hello".to_owned()));
        let handle = offline.begin(RATE, move |event| {
            let _ = sender.send(event);
        });
        handle.cancel();
        assert!(receiver.try_recv().is_err());
    }

    #[test]
    fn an_empty_clip_is_an_empty_transcript() {
        let (sender, receiver) = mpsc::channel();
        let mut offline = OfflineRecognition::spawn(Fake("hello".to_owned()));
        let handle = offline.begin(RATE, move |event| {
            let _ = sender.send(event);
        });
        handle.end();
        let event = receiver.recv().unwrap();
        assert!(
            matches!(&event, TranscriptEvent::Final(text) if text.is_empty()),
            "expected an empty Final, got {event:?}"
        );
    }

    #[test]
    fn stopping_fails_open_utterances() {
        let (sender, receiver) = mpsc::channel();
        let mut offline = OfflineRecognition::spawn(Fake("hello".to_owned()));
        let handle = offline.begin(RATE, move |event| {
            let _ = sender.send(event);
        });
        let stopper = offline.stopper();
        stopper.stop();
        assert!(stopper.wait(std::time::Instant::now() + std::time::Duration::from_secs(5)));
        drop(handle);
        let event = receiver.recv().unwrap();
        assert!(
            matches!(&event, TranscriptEvent::Failed(Error::Stopped(_))),
            "expected Failed(Stopped), got {event:?}"
        );
    }
}
