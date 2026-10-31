// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

//! The Tomori client: one long-lived duplex connection to the server, with automatic reconnection, plus real-time playback of received audio.
//!
//! See `design/server.md` and `design/live-audio.md`.

pub mod playback;

/// Finding servers on the local network with Bonjour: [`discovery::browse`] yields servers to list, and [`discovery::Server::address`] gives the address for [`Options::new`]. On Android, hold a Wi-Fi multicast lock while browsing; see the [`discovery`] crate documentation.
pub use discovery;

mod backoff;
mod client;
mod connection_state;
mod events;
mod options;
mod outbox;
mod resolve;
mod session;

pub use backoff::Backoff;
pub use client::{Client, Sender};
pub use connection_state::{ConnectionState, Disconnect, Event};
pub use events::Events;
pub use options::Options;
pub use outbox::{FailedUtterance, Membership, SendError, UtteranceFailure};
pub use protocol::Keepalive;
