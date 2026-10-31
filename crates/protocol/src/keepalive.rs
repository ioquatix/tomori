// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

use std::time::Duration;

/// Connection liveness timing, used the same way by clients and the server.
///
/// Any data received on a connection counts as liveness. A side that has received nothing for [`Self::interval`] sends [`crate::ClientMessage::Ping`] or [`crate::ServerMessage::Ping`], repeating each interval, and the peer answers with the matching Pong. A side that has received nothing for [`Self::timeout`] closes the connection. See `design/connection-liveness.md`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Keepalive {
    /// How long without receiving anything before sending a Ping.
    pub interval: Duration,
    /// How long without receiving anything before the connection is considered lost. Longer than [`Self::interval`], so that a Ping can be answered first.
    pub timeout: Duration,
}

impl Keepalive {
    pub const DEFAULT: Self = Self {
        interval: Duration::from_secs(5),
        timeout: Duration::from_secs(15),
    };
}

impl Default for Keepalive {
    fn default() -> Self {
        Self::DEFAULT
    }
}
