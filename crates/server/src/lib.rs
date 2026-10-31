// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

//! The Tomori server: spaces bound to agent sessions, shared by participants over one duplex connection each.
//!
//! Each client opens `POST /connect` and keeps it open. The request body carries client frames and the response body carries server frames for the life of the connection; see `design/server.md`. The server hosts several spaces, each bound to exactly one backend session, and keeps one shared state per space; see `design/spaces.md`. Participant audio is relayed within a space and push-to-talk audio is handed to a [`Transcriber`]; see `design/live-audio.md`. With the `speech` feature, [`RecognitionTranscriber`] transcribes with the speech crate, and a synthesizer given to [`Server::with_synthesizer`] speaks each space's assistant replies as one real-time agent speech stream; see `design/speech.md`.

mod application;
mod backends;
mod connection;
mod hub;
mod limits;
mod outbox;
mod projection;
mod queue;
#[cfg(feature = "speech")]
mod recognition;
mod server;
mod space;
mod transcriber;
mod voice;

pub mod mixer;

#[cfg(feature = "fake")]
pub mod fake;
#[cfg(feature = "fake")]
pub mod test_support;

pub use application::Application;
pub use backends::{AnyBackend, AnySession, Backends};
#[cfg(feature = "speech")]
pub use recognition::RecognitionTranscriber;
pub use server::{Server, bind};
pub use transcriber::{NullTranscriber, Transcriber, TranscriptSink, UtteranceId};

/// The server name reported in [`protocol::ServerMessage::Welcome`].
pub const SERVER_NAME: &str = "tomori";
