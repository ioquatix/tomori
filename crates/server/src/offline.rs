// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

//! A [`Transcriber`] that buffers each utterance and decodes it whole, once, with the offline recognizer. See `design/speech.md`.
//!
//! The offline recognizer is more accurate than the streaming one, but it cannot stream: it decodes a complete clip, so nothing is shown while the participant is still speaking, and the transcript arrives a couple of seconds after they let go, instead of almost at once.

use std::{
    collections::HashMap,
    sync::{Mutex, PoisonError},
};

use protocol::AudioFrame;
use speech::stt::{OfflineRecognition, OfflineRecognizer, OfflineUtterance, RATE};
use speech::{Error, Stopper};

use crate::transcriber::{Transcriber, TranscriptSink, UtteranceId};

/// Reported when recognition has stopped: a panic disabled it, or the server is shutting down. Trying again does not help, so the message does not suggest it.
const STOPPED: &str =
    "Speech recognition is unavailable until the server restarts, so nothing was sent.";

/// Offline speech recognition for push-to-talk utterances.
///
/// Each utterance's audio is buffered and decoded whole once the participant stops speaking. A recognizer error fails the utterance with a visible message; once a panic has disabled recognition, every utterance fails saying so, until the server restarts.
pub struct OfflineTranscriber {
    offline: Mutex<OfflineRecognition>,
    utterances: Mutex<HashMap<UtteranceId, OfflineUtterance>>,
}

impl OfflineTranscriber {
    /// Buffer and decode each utterance with an offline `recognizer`.
    pub fn new(recognizer: impl OfflineRecognizer + 'static) -> Self {
        Self {
            offline: Mutex::new(OfflineRecognition::spawn(recognizer)),
            utterances: Mutex::default(),
        }
    }

    /// A handle that stops the decode thread when the server shuts down; see `design/server.md`.
    pub fn stopper(&self) -> Stopper {
        self.offline
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .stopper()
    }

    fn utterances(&self) -> std::sync::MutexGuard<'_, HashMap<UtteranceId, OfflineUtterance>> {
        self.utterances
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }
}

impl Transcriber for OfflineTranscriber {
    fn begin(&self, utterance: UtteranceId, sink: TranscriptSink) {
        let handle = self
            .offline
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .begin(RATE, move |event| match event {
                speech::TranscriptEvent::Partial(_) => {
                    // The offline recognizer reports no partials; nothing is shown while the participant is speaking.
                }
                speech::TranscriptEvent::Final(text) => sink.finish(&text),
                speech::TranscriptEvent::Failed(Error::Stopped(_)) => sink.fail(STOPPED),
                speech::TranscriptEvent::Failed(error) => sink.fail(&format!(
                    "Transcription failed, so nothing was sent: {error}."
                )),
            });
        self.utterances().insert(utterance, handle);
    }

    /// Append the utterance's audio; it is buffered on the decode thread until the utterance ends.
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

    /// The hub cancels from inside a failure report, which can arrive while `begin` holds `offline`, so this must not take that lock.
    fn cancel(&self, utterance: UtteranceId) {
        let handle = self.utterances().remove(&utterance);
        if let Some(handle) = handle {
            handle.cancel();
        }
    }
}

/// An [`OfflineRecognizer`] for the in-process Whisper model: it decodes the buffered clip whole, once.
pub struct Whisper {
    inner: speech::stt::WhisperRecognizer,
}

impl Whisper {
    /// Load the Whisper model from `config`.
    pub fn new(config: &speech::stt::WhisperConfig) -> Result<Self, Error> {
        Ok(Self {
            inner: speech::stt::WhisperRecognizer::new(config)?,
        })
    }
}

impl OfflineRecognizer for Whisper {
    fn transcribe(&mut self, samples: &[i16]) -> Result<String, Error> {
        self.inner.transcribe(samples)
    }
}
