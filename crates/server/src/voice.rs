// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

//! The agent speech stream of each space. See `design/speech.md` and `design/live-audio.md`.
//!
//! A space's driver feeds every backend event to its [`Voice`]. Each assistant message's text deltas pass through a [`speech::Speakable`], and each document-level block that closes is queued, in order, as one segment on the space's one [`speech::Speech`]. Nothing participants do stops, pauses or skips queued speech.
//!
//! Synthesis runs ahead of playback on the speech crate's thread. A pacer thread per space announces each segment with [`protocol::ServerMessage::Speech`] and then sends its audio as 20 ms [`protocol::AudioFrame`]s from [`protocol::AGENT_SOURCE`] at the rate it plays, slightly ahead, to whoever is in the space at that moment. Only that thread touches the hub, and only briefly to queue each frame.

#[cfg(feature = "speech")]
pub(crate) use enabled::{Voice, Voices};

#[cfg(not(feature = "speech"))]
pub(crate) use disabled::{Voice, Voices};

#[cfg(not(feature = "speech"))]
mod disabled {
    use std::sync::Arc;

    use backend::Event;

    use crate::hub::Hub;

    /// Without the `speech` feature, spaces have no agent speech.
    #[derive(Clone, Default)]
    pub struct Voices(());

    impl Voices {
        pub fn start(&self, _hub: &Arc<Hub>, _space: &str) -> Option<Voice> {
            None
        }
    }

    pub enum Voice {}

    impl Voice {
        pub fn observe(&mut self, _event: &Event) {
            match *self {}
        }
    }
}

#[cfg(feature = "speech")]
mod enabled {
    use std::collections::{HashMap, VecDeque};
    use std::panic::{self, AssertUnwindSafe};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
    use std::sync::{Arc, Mutex, PoisonError, Weak};
    use std::thread;
    use std::time::{Duration, Instant};

    use backend::{Event, Role};
    use protocol::{AGENT_SOURCE, AudioFrame, Gate, ServerMessage};
    use speech::{
        AudioChunk, Block, Error, Speakable, SpeakableOptions, Speech, SpeechEvent, Synthesizer,
    };

    use crate::hub::Hub;

    /// Frames per second of agent speech: 20 ms frames.
    const FRAMES_PER_SECOND: u32 = 50;

    /// The longest piece of text in one speech segment. Synthesis is in whole document-level blocks, as the incremental parser hands them over, so a block keeps the cadence of the paragraph it came from; the limit only splits the pathological block, a single line of several thousand characters, at a word.
    const MAXIMUM_SEGMENT: usize = 2100;

    /// The speakable filter of an assistant message, splitting at most [`MAXIMUM_SEGMENT`] bytes at a time.
    fn speakable() -> Speakable {
        Speakable::with_options(SpeakableOptions {
            code_notice: None,
            maximum_segment: MAXIMUM_SEGMENT,
        })
    }

    /// How far ahead of its playback time a frame is sent, to cover network jitter. Kept well below the client's jitter buffer limit.
    const LEAD: Duration = Duration::from_millis(100);

    /// The synthesizer shared by every space. Spaces take turns segment by segment; each space's own segments stay in order.
    ///
    /// A panic in the synthesizer disables it for every space, since it may be left inconsistent; the space whose segment panicked reports the panic, and the others fail with [`Error::Stopped`].
    #[derive(Clone)]
    struct Shared(Arc<Engine>);

    struct Engine {
        synthesizer: Mutex<Box<dyn Synthesizer>>,
        disabled: AtomicBool,
    }

    impl Synthesizer for Shared {
        fn synthesize(
            &mut self,
            text: &str,
            output: &mut dyn FnMut(AudioChunk),
        ) -> Result<(), Error> {
            let engine = &*self.0;
            let mut synthesizer = engine
                .synthesizer
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            if engine.disabled.load(Ordering::Acquire) {
                return Err(Error::Stopped(
                    "speech synthesis stopped after an internal error".into(),
                ));
            }
            panic::catch_unwind(AssertUnwindSafe(|| synthesizer.synthesize(text, output)))
                .unwrap_or_else(|panic| {
                    engine.disabled.store(true, Ordering::Release);
                    panic::resume_unwind(panic)
                })
        }
    }

    /// Starts the agent speech stream of each space, if the server has a synthesizer.
    #[derive(Clone, Default)]
    pub struct Voices {
        synthesizer: Option<Shared>,
    }

    impl Voices {
        pub fn new(synthesizer: impl Synthesizer + 'static) -> Self {
            Self {
                synthesizer: Some(Shared(Arc::new(Engine {
                    synthesizer: Mutex::new(Box::new(synthesizer)),
                    disabled: AtomicBool::new(false),
                }))),
            }
        }

        /// Start the speech stream of `space`, or `None` when the server has no synthesizer.
        pub fn start(&self, hub: &Arc<Hub>, space: &str) -> Option<Voice> {
            let synthesizer = self.synthesizer.clone()?;
            let (sender, events) = mpsc::channel();
            let labels = Arc::new(Mutex::new(HashMap::new()));
            let closing = Arc::new(AtomicBool::new(false));
            let pacer = Pacer::new(
                Arc::downgrade(hub),
                space.to_owned(),
                labels.clone(),
                closing.clone(),
                events,
            );
            if let Err(error) = thread::Builder::new()
                .name("speech-pacer".into())
                .spawn(move || pacer.run())
            {
                log::error!("Could not start agent speech for {space}: {error}");
                hub.speech_failed(space, "Agent speech could not be started.".to_owned());
                return None;
            }
            let speech = Speech::spawn(synthesizer, move |event| {
                // The pacer only stops once the hub is gone, after which speech has nowhere to go.
                let _ = sender.send(event);
            });
            Some(Voice {
                speech,
                labels,
                closing,
                speaking: HashMap::new(),
            })
        }
    }

    /// The agent speech stream of one space, fed with the space's backend events.
    ///
    /// Only assistant messages that start while the space is running are spoken: their text deltas from `MessageStarted` to `MessageCompleted`. History from the session snapshot, user messages, reasoning and tool activity are never spoken.
    pub struct Voice {
        speech: Speech,
        /// The message of each queued segment, for its announcement.
        labels: Arc<Mutex<HashMap<u64, String>>>,
        /// Set when the space stops, so that the pacer can tell an orderly end from a failure.
        closing: Arc<AtomicBool>,
        /// Assistant messages being spoken.
        speaking: HashMap<String, Speakable>,
    }

    impl Voice {
        pub fn observe(&mut self, event: &Event) {
            match event {
                Event::MessageStarted {
                    message,
                    role: Role::Assistant,
                } => {
                    self.speaking
                        .entry(message.clone())
                        .or_insert_with(speakable);
                }
                Event::TextDelta { message, text } => {
                    if let Some(speakable) = self.speaking.get_mut(message) {
                        let blocks = speakable.push(text);
                        self.speak(message, blocks);
                    }
                }
                Event::MessageCompleted { message } => {
                    if let Some(mut speakable) = self.speaking.remove(message) {
                        let blocks = speakable.finish();
                        self.speak(message, blocks);
                    }
                }
                // An incomplete message's unclosed block is not stable, so it is not spoken. Queued segments still finish.
                Event::Closed { .. } => self.speaking.clear(),
                _ => {}
            }
        }

        fn speak(&mut self, message: &str, blocks: Vec<Block>) {
            // Holding the labels across `speak` means the pacer finds each segment's label once synthesis starts it.
            let mut labels = self.labels.lock().unwrap_or_else(PoisonError::into_inner);
            for block in blocks {
                // One segment per block, its sentences joined: the voice keeps the paragraph's cadence, and `Speech::speak` skips a block with nothing to say.
                if let Some(segment) = self.speech.speak(&block.sentences.join(" ")) {
                    labels.insert(segment, message.to_owned());
                }
            }
        }
    }

    impl Drop for Voice {
        /// Queued segments are still synthesized and played; see [`Speech`].
        fn drop(&mut self) {
            self.closing.store(true, Ordering::SeqCst);
        }
    }

    enum Item {
        Announce(ServerMessage),
        Frame { sample_rate: u32, samples: Vec<i16> },
        Failed(String),
    }

    /// Turns synthesized audio into a real-time frame stream for one space.
    struct Pacer {
        hub: Weak<Hub>,
        space: String,
        labels: Arc<Mutex<HashMap<u64, String>>>,
        closing: Arc<AtomicBool>,
        events: Receiver<SpeechEvent>,
        connected: bool,
        pending: VecDeque<Item>,
        /// The segment that started but has produced no audio yet: its number, message and text.
        announcing: Option<(u64, String, String)>,
        /// Samples short of a whole frame, at `carry_rate`.
        carry: Vec<i16>,
        carry_rate: u32,
        /// When the next frame should start playing. Behind the current time after the stream has been idle.
        clock: Option<Instant>,
        epoch: Instant,
        sequence: u32,
        /// Whether synthesis has been reported as stopped.
        stopped: bool,
    }

    /// Reported once per space when synthesis has stopped: a panic disabled it, or the server is shutting down.
    const SYNTHESIS_STOPPED: &str =
        "Speech synthesis is unavailable until the server restarts, so replies are not spoken.";

    impl Pacer {
        fn new(
            hub: Weak<Hub>,
            space: String,
            labels: Arc<Mutex<HashMap<u64, String>>>,
            closing: Arc<AtomicBool>,
            events: Receiver<SpeechEvent>,
        ) -> Self {
            Self {
                hub,
                space,
                labels,
                closing,
                events,
                connected: true,
                pending: VecDeque::new(),
                announcing: None,
                carry: Vec::new(),
                carry_rate: 0,
                clock: None,
                epoch: Instant::now(),
                sequence: 0,
                stopped: false,
            }
        }

        fn run(mut self) {
            loop {
                let now = Instant::now();
                let wait = match self.pending.front() {
                    None if !self.connected => return,
                    None => None,
                    Some(Item::Frame { .. }) => {
                        let clock = self.clock.filter(|clock| *clock >= now).unwrap_or(now);
                        let due = clock.checked_sub(LEAD).unwrap_or(clock);
                        Some(due.saturating_duration_since(now))
                    }
                    Some(_) => Some(Duration::ZERO),
                };
                match wait {
                    None => match self.events.recv() {
                        Ok(event) => self.handle(event),
                        Err(_) => self.disconnect(),
                    },
                    Some(wait) if !wait.is_zero() => {
                        if self.connected {
                            match self.events.recv_timeout(wait) {
                                Ok(event) => self.handle(event),
                                Err(RecvTimeoutError::Timeout) => {}
                                Err(RecvTimeoutError::Disconnected) => self.disconnect(),
                            }
                        } else {
                            thread::sleep(wait);
                        }
                    }
                    Some(_) => {
                        let item = self.pending.pop_front().expect("checked above");
                        if !self.send(item, now) {
                            return;
                        }
                    }
                }
            }
        }

        /// Send one item to the space. Returns false once the server has stopped.
        fn send(&mut self, item: Item, now: Instant) -> bool {
            let Some(hub) = self.hub.upgrade() else {
                return false;
            };
            match item {
                Item::Announce(message) => hub.agent_speech(&self.space, message),
                Item::Failed(error) => hub.speech_failed(&self.space, error),
                Item::Frame {
                    sample_rate,
                    samples,
                } => {
                    let clock = self.clock.filter(|clock| *clock >= now).unwrap_or(now);
                    let duration =
                        Duration::from_secs_f64(samples.len() as f64 / f64::from(sample_rate));
                    let frame = AudioFrame {
                        gate: Gate::Live,
                        source: AGENT_SOURCE,
                        sequence: self.sequence,
                        timestamp: clock.duration_since(self.epoch).as_micros() as u64,
                        sample_rate,
                        samples,
                    };
                    self.sequence = self.sequence.wrapping_add(1);
                    self.clock = Some(clock + duration);
                    hub.agent_audio(&self.space, &frame);
                }
            }
            true
        }

        fn handle(&mut self, event: SpeechEvent) {
            match event {
                SpeechEvent::Started { segment, text } => {
                    self.flush();
                    let message = self
                        .labels
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .remove(&segment)
                        .unwrap_or_default();
                    self.announcing = Some((segment, message, text));
                }
                SpeechEvent::Audio { chunk, .. } => self.audio(chunk),
                SpeechEvent::Finished { .. } => {
                    self.flush();
                    self.announcing = None;
                }
                SpeechEvent::Failed { error, .. } => {
                    self.flush();
                    self.announcing = None;
                    if matches!(error, speech::Error::Stopped(_)) {
                        // Synthesis stays stopped until the server restarts, so say so once rather than for every sentence.
                        if !self.stopped {
                            self.stopped = true;
                            self.pending
                                .push_back(Item::Failed(SYNTHESIS_STOPPED.to_owned()));
                        }
                    } else {
                        self.pending.push_back(Item::Failed(format!(
                            "Part of the reply could not be spoken: {error}."
                        )));
                    }
                }
            }
        }

        fn audio(&mut self, chunk: AudioChunk) {
            if chunk.sample_rate == 0 {
                return;
            }
            if chunk.sample_rate != self.carry_rate {
                self.flush();
                self.carry_rate = chunk.sample_rate;
            }
            if let Some((segment, message, text)) = self.announcing.take() {
                self.pending
                    .push_back(Item::Announce(ServerMessage::Speech {
                        segment: u32::try_from(segment).unwrap_or(u32::MAX),
                        message,
                        text,
                        sample_rate: chunk.sample_rate,
                    }));
            }
            self.carry.extend_from_slice(&chunk.samples);
            let size = (chunk.sample_rate / FRAMES_PER_SECOND).max(1) as usize;
            while self.carry.len() >= size {
                let samples = self.carry.drain(..size).collect();
                self.pending.push_back(Item::Frame {
                    sample_rate: chunk.sample_rate,
                    samples,
                });
            }
        }

        /// Send the rest of a segment as a short frame.
        fn flush(&mut self) {
            if !self.carry.is_empty() {
                self.pending.push_back(Item::Frame {
                    sample_rate: self.carry_rate,
                    samples: std::mem::take(&mut self.carry),
                });
            }
        }

        fn disconnect(&mut self) {
            self.connected = false;
            self.flush();
            if !self.closing.load(Ordering::SeqCst) {
                self.pending.push_back(Item::Failed(
                    "Agent speech stopped unexpectedly; replies in this space will not be spoken."
                        .to_owned(),
                ));
            }
        }
    }
}
