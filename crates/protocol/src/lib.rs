// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

//! Wire protocol shared by Tomori clients and the Tomori server.
//!
//! Each client keeps one long-lived, bidirectional connection to the server.
//! Both directions carry a sequence of [`Frame`]s: JSON control messages
//! ([`ClientMessage`] and [`ServerMessage`]) and binary [`AudioFrame`]s. See
//! `design/server.md`.

mod audio;
mod frame;
mod keepalive;
mod message;
mod space;

pub use audio::{AGENT_SOURCE, AUDIO_HEADER_BYTES, AudioError, AudioFrame, Gate};
pub use frame::{Frame, FrameDecoder, FrameError, FrameKind, MAX_FRAME_BYTES};
pub use keepalive::Keepalive;
pub use message::{ClientMessage, Decision, Pose, ServerMessage};
pub use space::{
    Activity, Approval, ApprovalOption, Message, Participant, Presentation, PresentationKind,
    Question, QueueEntry, QueueState, Role, SpaceState, SpaceSummary, Worker, WorkerState,
};

/// The protocol version sent in [`ClientMessage::Hello`] and [`ServerMessage::Welcome`].
pub const PROTOCOL_VERSION: u32 = 1;

/// The HTTP path that clients use to open their connection.
pub const CONNECT_PATH: &str = "/connect";

/// The media type of a connection body in either direction.
pub const CONTENT_TYPE: &str = "application/x-tomori-frames";

/// The default server port.
pub const DEFAULT_PORT: u16 = 8765;
