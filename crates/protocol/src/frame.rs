// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

use std::fmt;

use crate::{AudioError, AudioFrame, ClientMessage, ServerMessage};

/// The largest payload accepted in one frame.
pub const MAX_FRAME_BYTES: usize = 1024 * 1024;

const HEADER_BYTES: usize = 5;

/// The payload type of a [`Frame`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum FrameKind {
    /// A UTF-8 JSON control message.
    Control = 0,
    /// A binary [`AudioFrame`].
    Audio = 1,
}

/// One unit of a connection body: a kind byte, a little-endian `u32` payload length, then the payload.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Frame {
    pub kind: FrameKind,
    pub payload: Vec<u8>,
}

#[derive(Debug)]
pub enum FrameError {
    /// The kind byte is not a known [`FrameKind`].
    UnknownKind(u8),
    /// The declared payload length exceeds [`MAX_FRAME_BYTES`].
    TooLarge(usize),
    /// A control payload is not a valid message.
    Json(serde_json::Error),
    /// An audio payload is malformed.
    Audio(AudioError),
    /// The frame has a different kind than the caller expected.
    UnexpectedKind(FrameKind),
    /// The stream ended part way through a frame.
    Truncated,
}

impl fmt::Display for FrameError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownKind(kind) => write!(formatter, "unknown frame kind {kind}"),
            Self::TooLarge(length) => {
                write!(formatter, "frame of {length} bytes exceeds the limit")
            }
            Self::Json(error) => write!(formatter, "invalid control message: {error}"),
            Self::Audio(error) => write!(formatter, "invalid audio frame: {error}"),
            Self::UnexpectedKind(kind) => write!(formatter, "unexpected {kind:?} frame"),
            Self::Truncated => write!(formatter, "connection ended inside a frame"),
        }
    }
}

impl std::error::Error for FrameError {}

impl Frame {
    pub fn control(message: &impl serde::Serialize) -> Self {
        Self {
            kind: FrameKind::Control,
            // Serializing our own message types cannot fail: they contain no maps with non-string keys.
            payload: serde_json::to_vec(message).expect("control messages serialize"),
        }
    }

    pub fn client(message: &ClientMessage) -> Self {
        Self::control(message)
    }

    pub fn server(message: &ServerMessage) -> Self {
        Self::control(message)
    }

    pub fn audio(frame: &AudioFrame) -> Self {
        Self {
            kind: FrameKind::Audio,
            payload: frame.encode(),
        }
    }

    /// Append the encoded frame to `output`.
    pub fn encode_into(&self, output: &mut Vec<u8>) {
        debug_assert!(self.payload.len() <= MAX_FRAME_BYTES);
        output.reserve(HEADER_BYTES + self.payload.len());
        output.push(self.kind as u8);
        output.extend_from_slice(&(self.payload.len() as u32).to_le_bytes());
        output.extend_from_slice(&self.payload);
    }

    pub fn encode(&self) -> Vec<u8> {
        let mut output = Vec::new();
        self.encode_into(&mut output);
        output
    }

    pub fn to_client_message(&self) -> Result<ClientMessage, FrameError> {
        self.json()
    }

    pub fn to_server_message(&self) -> Result<ServerMessage, FrameError> {
        self.json()
    }

    pub fn to_audio(&self) -> Result<AudioFrame, FrameError> {
        if self.kind != FrameKind::Audio {
            return Err(FrameError::UnexpectedKind(self.kind));
        }
        AudioFrame::decode(&self.payload).map_err(FrameError::Audio)
    }

    fn json<T: serde::de::DeserializeOwned>(&self) -> Result<T, FrameError> {
        if self.kind != FrameKind::Control {
            return Err(FrameError::UnexpectedKind(self.kind));
        }
        serde_json::from_slice(&self.payload).map_err(FrameError::Json)
    }
}

/// Incrementally split a byte stream into frames. Input can arrive in arbitrary chunks.
#[derive(Default)]
pub struct FrameDecoder {
    buffer: Vec<u8>,
    offset: usize,
}

impl FrameDecoder {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push(&mut self, data: &[u8]) {
        // Compact consumed input before growing, so a long-lived connection does not accumulate it.
        if self.offset > 0 && self.offset == self.buffer.len() {
            self.buffer.clear();
            self.offset = 0;
        } else if self.offset > 64 * 1024 {
            self.buffer.drain(..self.offset);
            self.offset = 0;
        }
        self.buffer.extend_from_slice(data);
    }

    /// Return the next complete frame, or `None` if more input is needed.
    pub fn next_frame(&mut self) -> Result<Option<Frame>, FrameError> {
        let available = &self.buffer[self.offset..];
        if available.len() < HEADER_BYTES {
            return Ok(None);
        }
        let kind = match available[0] {
            0 => FrameKind::Control,
            1 => FrameKind::Audio,
            kind => return Err(FrameError::UnknownKind(kind)),
        };
        let length = u32::from_le_bytes(available[1..HEADER_BYTES].try_into().unwrap()) as usize;
        if length > MAX_FRAME_BYTES {
            return Err(FrameError::TooLarge(length));
        }
        if available.len() < HEADER_BYTES + length {
            return Ok(None);
        }
        let payload = available[HEADER_BYTES..HEADER_BYTES + length].to_vec();
        self.offset += HEADER_BYTES + length;
        Ok(Some(Frame { kind, payload }))
    }

    /// Check that the stream ended on a frame boundary.
    pub fn finish(&self) -> Result<(), FrameError> {
        if self.offset == self.buffer.len() {
            Ok(())
        } else {
            Err(FrameError::Truncated)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Gate, PROTOCOL_VERSION};

    #[test]
    fn frames_round_trip_across_arbitrary_chunk_boundaries() {
        let hello = ClientMessage::Hello {
            protocol: PROTOCOL_VERSION,
            participant: "quest-1".into(),
            name: "Samuel".into(),
            credentials: None,
        };
        let audio = AudioFrame {
            gate: Gate::Agent,
            source: 0,
            sequence: 7,
            timestamp: 123_456,
            sample_rate: 16_000,
            samples: vec![1, -2, 3, i16::MIN, i16::MAX],
        };
        let mut bytes = Frame::client(&hello).encode();
        Frame::audio(&audio).encode_into(&mut bytes);

        for chunk in [1, 2, 3, 7, bytes.len()] {
            let mut decoder = FrameDecoder::new();
            let mut frames = Vec::new();
            for piece in bytes.chunks(chunk) {
                decoder.push(piece);
                while let Some(frame) = decoder.next_frame().unwrap() {
                    frames.push(frame);
                }
            }
            decoder.finish().unwrap();
            assert_eq!(frames.len(), 2);
            assert_eq!(frames[0].to_client_message().unwrap(), hello);
            assert_eq!(frames[1].to_audio().unwrap(), audio);
        }
    }

    #[test]
    fn rejects_unknown_kinds_oversized_frames_and_truncation() {
        let mut decoder = FrameDecoder::new();
        decoder.push(&[9, 0, 0, 0, 0]);
        assert!(matches!(
            decoder.next_frame(),
            Err(FrameError::UnknownKind(9))
        ));

        let mut decoder = FrameDecoder::new();
        decoder.push(&[0]);
        decoder.push(&((MAX_FRAME_BYTES as u32 + 1).to_le_bytes()));
        assert!(matches!(decoder.next_frame(), Err(FrameError::TooLarge(_))));

        let mut decoder = FrameDecoder::new();
        decoder.push(&[0, 4, 0, 0, 0, b'{']);
        assert!(decoder.next_frame().unwrap().is_none());
        assert!(matches!(decoder.finish(), Err(FrameError::Truncated)));
    }

    #[test]
    fn kind_mismatches_are_errors() {
        let frame = Frame::server(&ServerMessage::Left);
        assert!(matches!(
            frame.to_audio(),
            Err(FrameError::UnexpectedKind(FrameKind::Control))
        ));
    }
}
