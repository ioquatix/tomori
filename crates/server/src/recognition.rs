// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

//! A [`Transcriber`] backed by the speech crate's streaming [`Recognition`]. See `design/speech.md`.

use std::collections::HashMap;
use std::sync::{Mutex, PoisonError};

use protocol::AudioFrame;
use speech::stt::{RATE, Transcript};
use speech::{
    Recognition, Recognizer, RecognizerConfig, StreamingRecognizer, TranscriptEvent, Utterance,
};

use crate::transcriber::{Transcriber, TranscriptSink, UtteranceId};

/// Reported when recognition stops without reporting a result, such as when its thread panicked.
const STOPPED: &str = "Speech recognition stopped before the transcript was ready, so nothing was sent. Please try again.";

/// Streaming speech recognition for push-to-talk utterances.
///
/// One recognizer thread serves every utterance on the server. Partial transcripts are reported while the participant is speaking; a recognizer error, including a missing model or a recognizer thread that stopped, fails the utterance with a visible message.
pub struct RecognitionTranscriber {
    recognition: Mutex<Recognition>,
    utterances: Mutex<HashMap<UtteranceId, Utterance>>,
}

impl RecognitionTranscriber {
    /// Transcribe with a window `recognizer`, decoded incrementally with local-agreement commit.
    pub fn new(recognizer: impl Recognizer + 'static, config: RecognizerConfig) -> Self {
        Self::with_recognition(Recognition::spawn(recognizer, config))
    }

    /// Transcribe with a streaming `recognizer`, which decodes the audio once, as it arrives.
    pub fn streaming(recognizer: impl StreamingRecognizer + 'static) -> Self {
        Self::with_recognition(Recognition::spawn_streaming(recognizer))
    }

    fn with_recognition(recognition: Recognition) -> Self {
        Self {
            recognition: Mutex::new(recognition),
            utterances: Mutex::default(),
        }
    }

    fn utterances(&self) -> std::sync::MutexGuard<'_, HashMap<UtteranceId, Utterance>> {
        self.utterances
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }
}

impl Transcriber for RecognitionTranscriber {
    fn begin(&self, utterance: UtteranceId, sink: TranscriptSink) {
        let mut reporter = Reporter {
            sink,
            reported: false,
        };
        // The sample rate is adapted to each frame as it arrives.
        let handle = self
            .recognition
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .begin(utterance.0, RATE, move |event| reporter.report(event));
        self.utterances().insert(utterance, handle);
    }

    fn push(&self, utterance: UtteranceId, frame: &AudioFrame) {
        if let Some(handle) = self.utterances().get_mut(&utterance) {
            handle.push_frame(frame);
        }
    }

    fn end(&self, utterance: UtteranceId) {
        let handle = self.utterances().remove(&utterance);
        if let Some(handle) = handle {
            handle.end();
        }
    }

    /// The hub cancels from inside a failure report, which can arrive while `begin` holds `recognition`, so this must not take that lock.
    fn cancel(&self, utterance: UtteranceId) {
        let handle = self.utterances().remove(&utterance);
        if let Some(handle) = handle {
            handle.cancel();
        }
    }
}

/// Forwards one utterance's events to its sink. If recognition drops it without a final result, the utterance fails; after a cancellation the server ignores that report.
struct Reporter {
    sink: TranscriptSink,
    reported: bool,
}

impl Reporter {
    fn report(&mut self, event: TranscriptEvent) {
        match event {
            TranscriptEvent::Partial(transcript) => self.sink.partial(&text(&transcript)),
            TranscriptEvent::Final(text) => {
                self.reported = true;
                self.sink.finish(&text);
            }
            TranscriptEvent::Failed(error) => {
                self.reported = true;
                self.sink.fail(&format!(
                    "Transcription failed, so nothing was sent: {error}."
                ));
            }
        }
    }
}

impl Drop for Reporter {
    fn drop(&mut self) {
        if !self.reported {
            self.sink.fail(STOPPED);
        }
    }
}

/// The committed and tentative words as one line.
fn text(transcript: &Transcript) -> String {
    [transcript.committed.trim(), transcript.tentative.trim()]
        .into_iter()
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}
