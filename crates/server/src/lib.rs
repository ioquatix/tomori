// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

//! The Tomori server: spaces bound to agent sessions, shared by participants over one duplex connection each.
//!
//! Each client opens `POST /connect` and keeps it open. The request body carries client frames and the response body carries server frames for the life of the connection; see `design/server.md`. The server hosts several spaces, each bound to exactly one backend session, and keeps one shared state per space; see `design/spaces.md`. Participant audio is relayed within a space and push-to-talk audio is handed to a [`Transcriber`]; see `design/live-audio.md`. With the `speech` feature, [`RecognitionTranscriber`] transcribes with the speech crate, and a synthesizer given to [`Server::with_synthesizer`] speaks each space's assistant replies as one real-time agent speech stream; see `design/speech.md`.
//!
//! [`Server::builder`] builds and runs a server from the `tomori-server` command line, with backends and environments of another project's own registered beside Tomori's; the `tomori-server` executable is its default build. See [`Builder`] and `design/builders.md`.
mod access;
mod admission;
mod agent_state;
mod ambient;
mod application;
pub mod auth_command;
mod backends;
mod builder;
mod cards;
mod channel;
mod command;
mod connection;
mod diagram;
mod door;
mod environments;
mod events;
#[cfg(unix)]
mod exit;
mod follow;
mod highlighting;
mod hub;
mod images;
mod inbound;
mod instructions;
pub mod kept;
mod limits;
mod mcp;
pub mod mixer;
#[cfg(feature = "speech")]
pub mod offline;
mod option;
mod outbox;
mod pages;
mod patches;
pub mod places;
pub mod presence;
mod process;
mod projection;
mod proximity;
mod queue;
#[cfg(feature = "speech")]
mod recognition;
mod retention;
mod seating;
mod server;
mod server_sides;
mod space;
mod spawns;
mod stage;
mod system;
pub mod timers;
mod transcriber;
mod versions;
mod voice;
mod watch;
mod workspace;
mod zones;

#[cfg(feature = "fake")]
pub mod fake;
#[cfg(test)]
mod test_log;
#[cfg(feature = "fake")]
pub mod test_support;

pub use access::{Access, Grant};
pub use application::Application;
pub use backends::{AnyBackend, AnySession, Backends};
pub use builder::Builder;
pub use cards::{CardSource, Failure, Fetched, FetchedCard, Rate};
pub use environments::{
    Environment, EnvironmentWall, FOLLOW_HEIGHTS, STATUS_SLOTS, StatusWall, WALL_NAME_BYTES,
    valid_wall_name,
};
pub use hub::CLOSED_SPACES;
pub use hub::agent_actions::ACT_TIMEOUT;
pub use hub::agent_inspect::{
    FULL_PER_MINUTE, INSPECT_TIMEOUT, INSPECTION_BYTES, RECHECK_EVERY, RESULT_BYTES,
};
pub use hub::{
    Closing, KNOCK_COOLDOWN, KNOCK_TIMEOUT, KNOCKS_PER_SPACE, Opener, PAIRING_ATTEMPTS,
    PAIRING_OPEN_FOR, PAIRING_TIMEOUT, PAIRING_WINDOW, PairingEvent, PairingState, PairingView,
    REFUSALS_TOLD_EVERY, REQUEST_RECENT, RESTORING, REVEAL_TIMEOUT,
};
#[cfg(feature = "speech")]
pub use offline::OfflineTranscriber;
pub use option::CommandOption;
#[cfg(feature = "speech")]
pub use recognition::RecognitionTranscriber;
pub use server::{
    COUNTS_INTERVAL, DEFAULT_MAX_VISITORS, DEFAULT_PARTICIPANT_RETENTION, Pairing, Server, bind,
};
/// Where the Tomori wall's home and the agent's are in an environment that gives none, about home: the default layout.
/// See `design/environments.md`.
pub use stage::{AGENT as DEFAULT_AGENT, default_stage};
pub use transcriber::{NullTranscriber, Transcriber, TranscriptSink, UtteranceId};
pub use voice::SpeechMode;
pub use workspace::{
    DEFAULT_MAX_SPACES, DIRECTORY_UNAVAILABLE, MAX_RESOLVING, Unavailable, WorkspaceRoots,
};

/// The server name reported in [`tomori_protocol::ServerMessage::Welcome`].
pub const SERVER_NAME: &str = "tomori";
