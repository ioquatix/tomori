// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

//! The Tomori client: one long-lived duplex connection to the server, with automatic reconnection, plus real-time playback of received audio.
//!
//! See `design/server.md` and `design/live-audio.md`.

pub mod playback;

mod backoff;
mod client;
mod connection_state;
mod options;
mod outbox;
mod session;

pub use backoff::Backoff;
pub use client::{Client, Sender};
pub use connection_state::{ConnectionState, Disconnect, Event};
pub use options::Options;
pub use outbox::{Membership, SendError};
