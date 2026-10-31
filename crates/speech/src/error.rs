// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

use std::fmt;

/// Why recognition or synthesis could not produce a result.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Error {
    /// The engine is not installed or configured; the message says how to set it up.
    Unavailable(String),
    /// The engine has stopped and serves no further requests until the server restarts: it panicked, its thread ended, or the server is shutting down.
    Stopped(String),
    /// The engine rejected the request.
    Engine(String),
    /// Recognition fell further behind the audio than [`crate::stt::BACKLOG`] allows.
    Behind,
}

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unavailable(message) => write!(formatter, "speech engine unavailable: {message}"),
            Self::Stopped(message) => write!(formatter, "speech engine stopped: {message}"),
            Self::Engine(message) => write!(formatter, "speech engine error: {message}"),
            Self::Behind => write!(
                formatter,
                "speech recognition fell more than {} seconds behind the audio",
                crate::stt::BACKLOG / crate::stt::RATE as usize
            ),
        }
    }
}

impl std::error::Error for Error {}
