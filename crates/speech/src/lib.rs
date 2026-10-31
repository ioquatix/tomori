// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

//! Streaming speech recognition, speakable text selection and synthesis for the Tomori server. See `design/speech.md`.
//!
//! - [`stt`]: per-utterance streaming recognition.
//! - [`speakable`]: selects stable, speakable sentences from streaming Markdown.
//! - [`tts`]: ordered synthesis of those sentences.
//!
//! Engines run in this process through sherpa-onnx, on threads owned by this crate; see `readme.md`.

mod error;
pub mod speakable;
pub mod stt;
pub mod tts;

pub use error::Error;
pub use speakable::{Speakable, SpeakableOptions};
pub use stt::{
    Recognition, RecognitionStream, Recognizer, RecognizerConfig, StreamingRecognizer,
    TranscriptEvent, Utterance,
};
pub use tts::{AudioChunk, Speech, SpeechEvent, Synthesizer};
