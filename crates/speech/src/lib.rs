// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

//! Streaming speech recognition, speakable text selection and synthesis for the Tomori server. See `design/speech.md`.

pub mod speakable;

pub use speakable::{Speakable, SpeakableOptions};
