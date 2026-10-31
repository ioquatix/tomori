// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

//! Streaming speech recognition, speakable text selection and synthesis for the Tomori server. See `design/speech.md`.
//!
//! - [`stt`]: per-utterance streaming recognition with local-agreement commit.
//! - [`speakable`]: selects stable, speakable sentences from streaming Markdown.
//! - [`tts`]: ordered synthesis of those sentences.
//!
//! Engines run in supervised worker processes; see `worker/readme.md`.

mod error;
pub mod speakable;
pub mod stt;
pub mod tts;
mod worker;

pub use error::Error;
pub use speakable::{Speakable, SpeakableOptions};
pub use stt::{Recognition, Recognizer, RecognizerConfig, TranscriptEvent, Utterance};
pub use tts::{AudioChunk, Speech, SpeechEvent, Synthesizer};
pub use worker::WorkerCommand;
