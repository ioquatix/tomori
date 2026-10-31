// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

use std::time::Duration;

use protocol::Keepalive;

use crate::Backoff;

/// How a [`crate::Client`] reaches and identifies itself to the server.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Options {
    /// `host:port`, or a host alone for [`protocol::DEFAULT_PORT`]. Resolved again on every connection attempt.
    pub address: String,
    /// A stable identity, so the server recognizes a reconnect as the same participant.
    pub participant: String,
    /// The display name used for speaker attribution.
    pub name: String,
    pub credentials: Option<String>,
    pub backoff: Backoff,
    /// The limit for establishing the connection and for receiving the server's Welcome.
    pub connect_timeout: Duration,
    /// How long closing waits for the server to finish the connection after queued control messages are sent.
    pub close_timeout: Duration,
    /// When to ping a quiet server and when to give up on a silent one. A connection that receives nothing for [`protocol::Keepalive::timeout`] ends with [`crate::Disconnect::Timeout`] and reconnects.
    pub keepalive: Keepalive,
    /// Outgoing live audio frames queued while the link is congested; older frames beyond this are dropped. At 20 ms per frame the default is 160 ms. Frames of an open utterance are not counted or dropped; see [`Self::outgoing_utterance_limit`].
    pub outgoing_audio_limit: usize,
    /// Outgoing frames of the open utterance queued while the link is congested. They are never dropped individually, because recognition needs every one; beyond this limit the utterance fails with [`crate::UtteranceFailure::Congested`]. At 20 ms per frame the default is 30 seconds.
    pub outgoing_utterance_limit: usize,
    /// Received audio frames held for the application; newer frames are dropped when it falls this far behind.
    pub incoming_audio_limit: usize,
}

impl Options {
    pub fn new(
        address: impl Into<String>,
        participant: impl Into<String>,
        name: impl Into<String>,
    ) -> Self {
        Self {
            address: address.into(),
            participant: participant.into(),
            name: name.into(),
            credentials: None,
            backoff: Backoff::default(),
            connect_timeout: Duration::from_secs(5),
            close_timeout: Duration::from_secs(1),
            keepalive: Keepalive::default(),
            outgoing_audio_limit: 8,
            outgoing_utterance_limit: 1500,
            incoming_audio_limit: 256,
        }
    }
}
