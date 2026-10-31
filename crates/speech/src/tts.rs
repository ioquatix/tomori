// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

//! Ordered speech synthesis for a space's agent speech stream.
//!
//! [`Speech`] owns one [`Synthesizer`] on a dedicated thread and synthesizes queued segments in order, delivering audio as soon as each chunk is produced. Synthesis never waits for playback, so segment N+1 is synthesized while segment N plays. There is deliberately no way to interrupt, skip or truncate queued speech: participant input never stops agent speech (`design/speech.md`).

mod kokoro;

use std::{
    sync::mpsc::{self, Sender},
    thread::{self, JoinHandle},
};

pub use kokoro::{KokoroConfig, KokoroSynthesizer};

use crate::Error;

/// A block of synthesized 16-bit mono PCM.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AudioChunk {
    pub sample_rate: u32,
    pub samples: Vec<i16>,
}

/// A speech synthesis engine.
pub trait Synthesizer: Send {
    /// Synthesize `text`, passing each chunk of audio to `output` as it is produced.
    fn synthesize(&mut self, text: &str, output: &mut dyn FnMut(AudioChunk)) -> Result<(), Error>;
}

impl<S: Synthesizer + ?Sized> Synthesizer for Box<S> {
    fn synthesize(&mut self, text: &str, output: &mut dyn FnMut(AudioChunk)) -> Result<(), Error> {
        (**self).synthesize(text, output)
    }
}

/// Progress of the speech stream. Events for a segment arrive in order, and segments arrive in the order they were queued.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SpeechEvent {
    /// Synthesis of a segment started.
    Started { segment: u64, text: String },
    /// Audio for a segment, ready to play.
    Audio { segment: u64, chunk: AudioChunk },
    /// The segment's audio is complete.
    Finished { segment: u64 },
    /// The segment could not be synthesized. Audio already delivered for it stays valid; later segments continue.
    Failed { segment: u64, error: Error },
}

struct Job {
    segment: u64,
    text: String,
}

/// The speech stream of one space.
pub struct Speech {
    jobs: Option<Sender<Job>>,
    next: u64,
    thread: Option<JoinHandle<()>>,
}

impl Speech {
    /// Start the synthesis thread. Events are delivered to `sink` on that thread, so it should not block.
    pub fn spawn(
        mut synthesizer: impl Synthesizer + 'static,
        mut sink: impl FnMut(SpeechEvent) + Send + 'static,
    ) -> Self {
        let (jobs, receiver) = mpsc::channel::<Job>();
        let thread = thread::Builder::new()
            .name("speech-synthesis".into())
            .spawn(move || {
                for Job { segment, text } in receiver {
                    sink(SpeechEvent::Started {
                        segment,
                        text: text.clone(),
                    });
                    let result = synthesizer.synthesize(&text, &mut |chunk| {
                        if !chunk.samples.is_empty() {
                            sink(SpeechEvent::Audio { segment, chunk });
                        }
                    });
                    match result {
                        Ok(()) => sink(SpeechEvent::Finished { segment }),
                        Err(error) => {
                            log::warn!("Speech synthesis of segment {segment} failed: {error}");
                            sink(SpeechEvent::Failed { segment, error });
                        }
                    }
                }
            })
            .expect("spawn the synthesis thread");
        Self {
            jobs: Some(jobs),
            next: 0,
            thread: Some(thread),
        }
    }

    /// Queue a segment, such as a sentence from [`crate::Speakable`]. Returns its segment number. Blank text is skipped and returns `None`.
    pub fn speak(&mut self, text: &str) -> Option<u64> {
        let text = text.trim();
        if text.is_empty() {
            return None;
        }
        self.next += 1;
        let segment = self.next;
        let _ = self.jobs.as_ref()?.send(Job {
            segment,
            text: text.to_owned(),
        });
        Some(segment)
    }

    /// Stop accepting segments and wait until every queued segment has been synthesized.
    pub fn close(mut self) {
        self.jobs = None;
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl Drop for Speech {
    /// Queued segments are still synthesized in the background; dropping never truncates speech.
    fn drop(&mut self) {
        self.jobs = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        sync::{Arc, Mutex},
        time::{Duration, Instant},
    };

    /// Produces one chunk per word, with a delay, and fails on text containing "fail".
    struct Fake {
        delay: Duration,
        log: Arc<Mutex<Vec<String>>>,
    }

    impl Synthesizer for Fake {
        fn synthesize(
            &mut self,
            text: &str,
            output: &mut dyn FnMut(AudioChunk),
        ) -> Result<(), Error> {
            self.log.lock().unwrap().push(text.to_owned());
            for (index, _) in text.split_whitespace().enumerate() {
                thread::sleep(self.delay);
                output(AudioChunk {
                    sample_rate: 24_000,
                    samples: vec![index as i16; 10],
                });
                if text.contains("fail") {
                    return Err(Error::Engine("cannot say that".into()));
                }
            }
            Ok(())
        }
    }

    fn spawn(delay: Duration) -> (Speech, mpsc::Receiver<SpeechEvent>, Arc<Mutex<Vec<String>>>) {
        let log = Arc::default();
        let (sender, receiver) = mpsc::channel();
        let speech = Speech::spawn(
            Fake {
                delay,
                log: Arc::clone(&log),
            },
            move |event| {
                let _ = sender.send(event);
            },
        );
        (speech, receiver, log)
    }

    #[test]
    fn synthesizes_segments_in_order() {
        let (mut speech, receiver, _) = spawn(Duration::from_millis(1));
        assert_eq!(speech.speak("one two"), Some(1));
        assert_eq!(speech.speak("   "), None);
        assert_eq!(speech.speak("three"), Some(2));
        speech.close();
        let events: Vec<_> = receiver.iter().collect();
        let chunk = |value| AudioChunk {
            sample_rate: 24_000,
            samples: vec![value; 10],
        };
        assert_eq!(
            events,
            [
                SpeechEvent::Started {
                    segment: 1,
                    text: "one two".into()
                },
                SpeechEvent::Audio {
                    segment: 1,
                    chunk: chunk(0)
                },
                SpeechEvent::Audio {
                    segment: 1,
                    chunk: chunk(1)
                },
                SpeechEvent::Finished { segment: 1 },
                SpeechEvent::Started {
                    segment: 2,
                    text: "three".into()
                },
                SpeechEvent::Audio {
                    segment: 2,
                    chunk: chunk(0)
                },
                SpeechEvent::Finished { segment: 2 },
            ]
        );
    }

    #[test]
    fn delivers_audio_before_later_segments_are_queued() {
        let (mut speech, receiver, _) = spawn(Duration::ZERO);
        speech.speak("first sentence");
        // The first audio arrives while the reply is still streaming.
        let event = receiver.recv_timeout(Duration::from_secs(5)).unwrap();
        assert!(matches!(event, SpeechEvent::Started { segment: 1, .. }));
        let event = receiver.recv_timeout(Duration::from_secs(5)).unwrap();
        assert!(matches!(event, SpeechEvent::Audio { segment: 1, .. }));
        speech.speak("second sentence");
        speech.close();
        assert!(
            receiver
                .iter()
                .any(|event| event == SpeechEvent::Finished { segment: 2 })
        );
    }

    #[test]
    fn a_failed_segment_is_reported_and_later_segments_continue() {
        let (mut speech, receiver, _) = spawn(Duration::ZERO);
        speech.speak("please fail now");
        speech.speak("recovered");
        speech.close();
        let events: Vec<_> = receiver.iter().collect();
        assert!(events.contains(&SpeechEvent::Failed {
            segment: 1,
            error: Error::Engine("cannot say that".into())
        }));
        assert!(events.contains(&SpeechEvent::Finished { segment: 2 }));
    }

    #[test]
    fn queued_speech_is_never_truncated_by_dropping_the_handle() {
        let (mut speech, receiver, log) = spawn(Duration::from_millis(5));
        for index in 0..5 {
            speech.speak(&format!("segment {index}"));
        }
        let started = Instant::now();
        drop(speech);
        // Dropping returns immediately; synthesis continues in the background.
        assert!(started.elapsed() < Duration::from_millis(40));
        let finished = receiver
            .iter()
            .filter(|event| matches!(event, SpeechEvent::Finished { .. }))
            .count();
        assert_eq!(finished, 5);
        assert_eq!(log.lock().unwrap().len(), 5);
    }

    #[test]
    fn speaks_the_first_sentence_while_the_reply_is_still_streaming() {
        let (mut speech, receiver, _) = spawn(Duration::ZERO);
        let mut speakable = crate::Speakable::new();
        for delta in ["Hello there. This is", " a reply that is still"] {
            for sentence in speakable.push(delta) {
                speech.speak(&sentence);
            }
        }
        // The reply has not finished, but the first sentence is already audible.
        let event = receiver.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_eq!(
            event,
            SpeechEvent::Started {
                segment: 1,
                text: "Hello there.".into()
            }
        );
        assert!(matches!(
            receiver.recv_timeout(Duration::from_secs(5)).unwrap(),
            SpeechEvent::Audio { segment: 1, .. }
        ));
        for sentence in speakable.push(" arriving.") {
            speech.speak(&sentence);
        }
        for sentence in speakable.finish() {
            speech.speak(&sentence);
        }
        speech.close();
        let texts: Vec<_> = receiver
            .iter()
            .filter_map(|event| match event {
                SpeechEvent::Started { text, .. } => Some(text),
                _ => None,
            })
            .collect();
        assert_eq!(texts, ["This is a reply that is still arriving."]);
    }
}
