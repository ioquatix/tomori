// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

use std::collections::HashMap;
use std::sync::{Mutex, Weak};

use protocol::AudioFrame;

use crate::hub::Hub;

/// Identifies one push-to-talk utterance for the lifetime of the server.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct UtteranceId(pub u64);

/// Speech recognition for push-to-talk audio. See `design/speech.md` and `design/live-audio.md`.
///
/// The server calls these methods in order for each utterance: [`begin`](Self::begin), any number of [`push`](Self::push), then exactly one of [`end`](Self::end) or [`cancel`](Self::cancel). Utterances from different participants interleave. Methods are never called while server locks are held, and must not block on recognition: decode on another thread or task and report through the [`TranscriptSink`] given to `begin`.
///
/// The sink may be called from any thread. After `end`, the implementation reports exactly one [`TranscriptSink::finish`] or [`TranscriptSink::fail`]; nothing is queued for the agent unless `finish` is called with non-empty text. After `cancel`, further reports are ignored.
pub trait Transcriber: Send + Sync {
    fn begin(&self, utterance: UtteranceId, sink: TranscriptSink);

    /// The next agent-gated frame of the utterance. Frames arrive in sequence order; the server fails the utterance itself if a frame is missing.
    fn push(&self, utterance: UtteranceId, frame: &AudioFrame);

    /// The participant finished speaking. Decode the remaining audio and report the final transcript.
    fn end(&self, utterance: UtteranceId);

    /// Discard the utterance without a transcript.
    fn cancel(&self, utterance: UtteranceId);
}

/// Where a [`Transcriber`] reports results for one utterance. Cheap to clone.
#[derive(Clone, Debug)]
pub struct TranscriptSink {
    pub(crate) hub: Weak<Hub>,
    pub(crate) utterance: UtteranceId,
}

impl TranscriptSink {
    pub fn utterance(&self) -> UtteranceId {
        self.utterance
    }

    /// Report the best transcript so far. Shown to the speaker while recording.
    pub fn partial(&self, text: &str) {
        if let Some(hub) = self.hub.upgrade() {
            hub.transcript_partial(self.utterance, text);
        }
    }

    /// Report the final transcript. Non-empty text is queued for the agent on behalf of the speaker.
    pub fn finish(&self, text: &str) {
        if let Some(hub) = self.hub.upgrade() {
            hub.transcript_finished(self.utterance, text);
        }
    }

    /// Report that no transcript could be produced. The speaker is told and nothing is queued.
    pub fn fail(&self, error: &str) {
        if let Some(hub) = self.hub.upgrade() {
            hub.transcript_failed(self.utterance, error);
        }
    }
}

/// A transcriber for servers without speech recognition: every utterance fails visibly when it ends.
pub struct NullTranscriber {
    reason: String,
    sinks: Mutex<HashMap<UtteranceId, TranscriptSink>>,
}

impl Default for NullTranscriber {
    fn default() -> Self {
        Self::new()
    }
}

impl NullTranscriber {
    pub fn new() -> Self {
        Self::with_reason("Speech recognition is not available on this server.")
    }

    /// Fail every utterance with `reason`, such as how to install the missing engine.
    pub fn with_reason(reason: impl Into<String>) -> Self {
        Self {
            reason: reason.into(),
            sinks: Mutex::default(),
        }
    }
}

impl Transcriber for NullTranscriber {
    fn begin(&self, utterance: UtteranceId, sink: TranscriptSink) {
        self.sinks.lock().unwrap().insert(utterance, sink);
    }

    fn push(&self, _utterance: UtteranceId, _frame: &AudioFrame) {}

    fn end(&self, utterance: UtteranceId) {
        let sink = self.sinks.lock().unwrap().remove(&utterance);
        if let Some(sink) = sink {
            sink.fail(&self.reason);
        }
    }

    fn cancel(&self, utterance: UtteranceId) {
        self.sinks.lock().unwrap().remove(&utterance);
    }
}
