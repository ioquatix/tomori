// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

use std::fmt;

/// Why recognition or synthesis could not produce a result.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Error {
    /// The engine is not installed or configured; the message says how to set it up.
    Unavailable(String),
    /// The worker process failed, exited or stopped responding. It is restarted for the next request.
    Worker(String),
    /// The engine rejected the request.
    Engine(String),
    /// Recognition could not keep the audio without discarding speech.
    Overflow,
}

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unavailable(message) => write!(formatter, "speech engine unavailable: {message}"),
            Self::Worker(message) => write!(formatter, "speech worker failed: {message}"),
            Self::Engine(message) => write!(formatter, "speech engine error: {message}"),
            Self::Overflow => write!(
                formatter,
                "could not find a safe transcription boundary; no message was sent"
            ),
        }
    }
}

impl std::error::Error for Error {}
