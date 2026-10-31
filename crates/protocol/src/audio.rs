// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

use std::fmt;

use serde::{Deserialize, Serialize};

/// The fixed header size of an encoded [`AudioFrame`].
pub const AUDIO_HEADER_BYTES: usize = 24;

/// The [`AudioFrame::source`] of the space's agent speech stream.
pub const AGENT_SOURCE: u32 = u32::MAX;

const VERSION: u8 = 1;

/// Where a participant's microphone audio goes. See `design/live-audio.md`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[repr(u8)]
pub enum Gate {
    /// Nothing is sent. Clients do not send frames with this gate.
    #[default]
    Off = 0,
    /// Heard by other participants and the spectator feed.
    Live = 1,
    /// Heard by other participants and the spectator feed, and transcribed for the agent.
    Agent = 2,
}

/// A self-contained block of 16-bit mono PCM, so that the transport can change without affecting the layers above.
///
/// Encoded as a 24-byte little-endian header followed by the samples:
/// version `u8`, gate `u8`, reserved `u16`, source `u32`, sequence `u32`,
/// sample rate `u32`, timestamp `u64` (microseconds on the sender's clock).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AudioFrame {
    pub gate: Gate,
    /// The participant number assigned by the server, or [`AGENT_SOURCE`]. Clients send zero; the server sets it when forwarding.
    pub source: u32,
    /// Increments by one per frame from each source, so receivers can detect gaps.
    pub sequence: u32,
    pub timestamp: u64,
    pub sample_rate: u32,
    pub samples: Vec<i16>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum AudioError {
    Truncated,
    Version(u8),
    Gate(u8),
    OddLength,
    SampleRate(u32),
}

impl fmt::Display for AudioError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Truncated => write!(formatter, "header is truncated"),
            Self::Version(version) => write!(formatter, "unsupported version {version}"),
            Self::Gate(gate) => write!(formatter, "unknown gate {gate}"),
            Self::OddLength => write!(formatter, "sample data has an odd length"),
            Self::SampleRate(rate) => write!(formatter, "unsupported sample rate {rate}"),
        }
    }
}

impl std::error::Error for AudioError {}

impl AudioFrame {
    pub fn encode(&self) -> Vec<u8> {
        let mut output = Vec::with_capacity(AUDIO_HEADER_BYTES + self.samples.len() * 2);
        output.push(VERSION);
        output.push(self.gate as u8);
        output.extend_from_slice(&0u16.to_le_bytes());
        output.extend_from_slice(&self.source.to_le_bytes());
        output.extend_from_slice(&self.sequence.to_le_bytes());
        output.extend_from_slice(&self.sample_rate.to_le_bytes());
        output.extend_from_slice(&self.timestamp.to_le_bytes());
        for sample in &self.samples {
            output.extend_from_slice(&sample.to_le_bytes());
        }
        output
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, AudioError> {
        if bytes.len() < AUDIO_HEADER_BYTES {
            return Err(AudioError::Truncated);
        }
        if bytes[0] != VERSION {
            return Err(AudioError::Version(bytes[0]));
        }
        let gate = match bytes[1] {
            0 => Gate::Off,
            1 => Gate::Live,
            2 => Gate::Agent,
            gate => return Err(AudioError::Gate(gate)),
        };
        let word =
            |offset: usize| u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap());
        let sample_rate = word(12);
        if !(8_000..=48_000).contains(&sample_rate) {
            return Err(AudioError::SampleRate(sample_rate));
        }
        let data = &bytes[AUDIO_HEADER_BYTES..];
        if !data.len().is_multiple_of(2) {
            return Err(AudioError::OddLength);
        }
        Ok(Self {
            gate,
            source: word(4),
            sequence: word(8),
            sample_rate,
            timestamp: u64::from_le_bytes(bytes[16..24].try_into().unwrap()),
            samples: data
                .as_chunks::<2>()
                .0
                .iter()
                .map(|pair| i16::from_le_bytes([pair[0], pair[1]]))
                .collect(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame() -> AudioFrame {
        AudioFrame {
            gate: Gate::Live,
            source: AGENT_SOURCE,
            sequence: u32::MAX,
            timestamp: u64::MAX,
            sample_rate: 24_000,
            samples: vec![0, 1, -1],
        }
    }

    #[test]
    fn encodes_a_fixed_header_followed_by_samples() {
        let bytes = frame().encode();
        assert_eq!(bytes.len(), AUDIO_HEADER_BYTES + 6);
        assert_eq!(AudioFrame::decode(&bytes).unwrap(), frame());
    }

    #[test]
    fn rejects_malformed_frames() {
        let bytes = frame().encode();
        assert_eq!(AudioFrame::decode(&bytes[..10]), Err(AudioError::Truncated));
        assert_eq!(
            AudioFrame::decode(&bytes[..bytes.len() - 1]),
            Err(AudioError::OddLength)
        );
        let mut wrong = bytes.clone();
        wrong[0] = 2;
        assert_eq!(AudioFrame::decode(&wrong), Err(AudioError::Version(2)));
        let mut wrong = bytes.clone();
        wrong[1] = 3;
        assert_eq!(AudioFrame::decode(&wrong), Err(AudioError::Gate(3)));
        let mut wrong = bytes;
        wrong[12..16].copy_from_slice(&1u32.to_le_bytes());
        assert_eq!(AudioFrame::decode(&wrong), Err(AudioError::SampleRate(1)));
    }
}
