// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

use std::fmt;
use std::time::Duration;

use protocol::ServerMessage;

use crate::FailedUtterance;

/// Why a connection ended or could not be established.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Disconnect {
    /// The server address could not be resolved.
    Address(String),
    /// Connecting, reading or writing failed.
    Transport(String),
    /// The server answered `/connect` with an unsuccessful status.
    Status(u16),
    /// The server sent something that violates the protocol, including an incompatible version.
    Protocol(String),
    /// Connecting or the server's Welcome took too long, or an established connection received nothing for [`protocol::Keepalive::timeout`].
    Timeout,
    /// The server ended the connection.
    Closed,
}

impl fmt::Display for Disconnect {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Address(error) => write!(formatter, "cannot resolve the server address: {error}"),
            Self::Transport(error) => write!(formatter, "connection failed: {error}"),
            Self::Status(status) => write!(formatter, "server responded with status {status}"),
            Self::Protocol(error) => write!(formatter, "protocol error: {error}"),
            Self::Timeout => write!(formatter, "timed out waiting for the server"),
            Self::Closed => write!(formatter, "server closed the connection"),
        }
    }
}

impl std::error::Error for Disconnect {}

/// The client's connection to the server.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ConnectionState {
    /// Opening a connection. `attempt` counts from one and resets after a connection that stayed up for [`crate::Backoff::stable_duration`].
    Connecting { attempt: u32 },
    /// The server has welcomed this client. Membership has been restored.
    Connected {
        /// This client's participant number, which other clients see as the audio frame source.
        source: u32,
        server: String,
    },
    /// The connection ended; another attempt follows after `retry_in`.
    Disconnected {
        reason: Disconnect,
        retry_in: Duration,
    },
    /// The client was closed and will not reconnect.
    Closed,
}

impl ConnectionState {
    pub fn is_connected(&self) -> bool {
        matches!(self, Self::Connected { .. })
    }
}

/// Something the client observed, delivered in order.
#[derive(Clone, Debug, PartialEq)]
pub enum Event {
    State(ConnectionState),
    /// A control message from the server. `Welcome` is reported as [`ConnectionState::Connected`] instead.
    Message(ServerMessage),
    /// An utterance failed in the client and will produce no transcript. See [`crate::Sender::send`].
    UtteranceFailed(FailedUtterance),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn describes_disconnections() {
        let cases = [
            (
                Disconnect::Address("x".into()),
                "cannot resolve the server address: x",
            ),
            (Disconnect::Transport("y".into()), "connection failed: y"),
            (Disconnect::Status(404), "server responded with status 404"),
            (Disconnect::Protocol("z".into()), "protocol error: z"),
            (Disconnect::Timeout, "timed out waiting for the server"),
            (Disconnect::Closed, "server closed the connection"),
        ];
        for (reason, text) in cases {
            assert_eq!(reason.to_string(), text);
        }
        assert!(
            ConnectionState::Connected {
                source: 1,
                server: "s".into()
            }
            .is_connected()
        );
        assert!(!ConnectionState::Closed.is_connected());
    }
}
