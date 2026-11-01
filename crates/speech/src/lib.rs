// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

//! Streaming speech recognition, speakable text selection and synthesis for the Tomori server. See `design/speech.md`.
//!
//! - [`stt`]: per-utterance streaming recognition.
//! - [`speakable`]: splits streaming Markdown into closed document-level blocks and selects their speakable sentences.
//! - [`tts`]: ordered synthesis of those sentences.
//!
//! Engines run in this process through sherpa-onnx, on threads owned by this crate; see `readme.md`. A panic in an engine disables that engine: it is logged once, and every later request fails with [`Error::Stopped`]. Each engine thread can be stopped and waited for with a [`Stopper`] when the server shuts down.

mod error;
pub mod speakable;
mod stop;
pub mod stt;
pub mod tts;

/// The Markdown parser whose events [`Block`] carries.
pub use base_markdown;
pub use error::Error;
pub use speakable::{Block, BlockKind, Speakable, SpeakableOptions};
pub use stop::Stopper;
pub use stt::{Recognition, RecognitionStream, StreamingRecognizer, TranscriptEvent, Utterance};
pub use tts::{AudioChunk, Speech, SpeechEvent, Synthesizer};
