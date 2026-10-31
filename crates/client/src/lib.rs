// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

//! The Tomori client: one long-lived duplex connection to the server, with automatic reconnection, plus real-time playback of received audio.
//!
//! See `design/server.md` and `design/live-audio.md`.

pub mod playback;
