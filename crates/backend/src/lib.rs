// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

//! The backend-neutral interface between the Tomori server and coding agents.
//!
//! Backends report structured facts as [`Event`]s. They never produce text for
//! display or speech; the server owns that projection. See
//! `design/agent-backends.md`.

use std::fmt;
use std::future::Future;

use serde::{Deserialize, Serialize};

/// What a backend supports, so the server can offer only working actions.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Capabilities {
    /// Existing sessions can be listed and attached.
    pub attach: bool,
    /// A running turn can be interrupted.
    pub interrupt: bool,
    /// Structured questions can be answered through [`Session::answer`].
    pub questions: bool,
    /// Delegated worker sessions are reported.
    pub workers: bool,
}

/// How to start a new session.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionSpec {
    /// The working directory of the agent.
    pub directory: String,
    pub title: String,
}

/// An existing session that can be attached.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionSummary {
    pub id: String,
    pub title: String,
    pub directory: String,
}

/// Input delivered to the agent as one user turn.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Prompt {
    /// Unique per queue entry. A backend that is asked to send the same key twice delivers it at most once.
    pub key: String,
    /// The text to deliver, already including speaker attribution if enabled.
    pub text: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    User,
    Assistant,
}

/// The outcome of a permission request.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Decision {
    AllowOnce,
    Deny,
}

/// A tool permission request, described with structured facts.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Permission {
    pub id: String,
    /// The tool name, such as `Bash` or `Edit`.
    pub tool: String,
    /// The tool input as reported by the agent.
    pub input: serde_json::Value,
    /// The agent's own short description of the action, when it provides one.
    pub description: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FieldKind {
    /// Free text.
    Text,
    /// Exactly one of the options.
    Choice,
    /// Any number of the options.
    MultipleChoice,
    /// A field Tomori cannot collect; the question must be completed outside Tomori.
    Unsupported,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FieldOption {
    pub label: String,
    pub description: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Field {
    pub key: String,
    pub title: String,
    pub description: Option<String>,
    pub kind: FieldKind,
    pub options: Vec<FieldOption>,
    /// A choice field also accepts free text.
    pub custom: bool,
}

/// A structured question from the agent, answered one field at a time.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Question {
    pub id: String,
    pub title: String,
    pub fields: Vec<Field>,
}

/// The answer to one field. The server matches spoken text to options before calling the backend.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", content = "value", rename_all = "snake_case")]
pub enum AnswerValue {
    Text(String),
    Choices(Vec<String>),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkerState {
    Running,
    Waiting,
    Idle,
    Completed,
    Error,
    Unknown,
}

/// A delegated worker, such as a Claude Code subagent or an OpenCode child session.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Worker {
    pub id: String,
    pub parent: Option<String>,
    pub title: String,
    pub state: WorkerState,
}

/// A part of a message in a [`Snapshot`].
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Part {
    Text {
        text: String,
    },
    Reasoning {
        text: String,
    },
    Tool {
        id: String,
        tool: String,
        input: serde_json::Value,
        finished: bool,
    },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SnapshotMessage {
    pub id: String,
    pub role: Role,
    pub parts: Vec<Part>,
    pub completed: bool,
}

/// The current state of a session, used on attach and to resynchronize after a lost event.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Snapshot {
    pub busy: Option<bool>,
    pub messages: Vec<SnapshotMessage>,
    pub permission: Option<Permission>,
    pub question: Option<Question>,
    pub workers: Vec<Worker>,
}

/// A fact reported by a session, in order.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Event {
    /// The agent started working on a turn.
    Busy,
    /// The agent finished its turn and will accept the next prompt.
    Idle,
    /// A prompt with this key was accepted by the agent.
    Delivered {
        key: String,
    },
    MessageStarted {
        message: String,
        role: Role,
    },
    /// Assistant text, streamed as it is generated. Concatenating the deltas of a message gives its text.
    TextDelta {
        message: String,
        text: String,
    },
    MessageCompleted {
        message: String,
    },
    ToolStarted {
        message: String,
        tool: String,
        id: String,
        input: serde_json::Value,
    },
    ToolFinished {
        id: String,
        error: Option<String>,
    },
    PermissionRequested(Permission),
    /// The permission was resolved, by Tomori or by the agent itself.
    PermissionResolved {
        id: String,
    },
    QuestionAsked(Question),
    QuestionResolved {
        id: String,
    },
    WorkersChanged {
        workers: Vec<Worker>,
    },
    /// A recoverable problem. The session continues.
    Error {
        message: String,
    },
    /// The session ended and produces no more events.
    Closed {
        reason: Option<String>,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ErrorKind {
    /// The session, request or question does not exist or is no longer pending.
    NotFound,
    /// The backend does not support the operation.
    Unsupported,
    /// The request is invalid, such as an answer for the wrong field.
    Invalid,
    /// The agent process or service is unavailable.
    Unavailable,
    /// The session has closed.
    Closed,
    Other,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Error {
    pub kind: ErrorKind,
    pub message: String,
}

impl Error {
    pub fn new(kind: ErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{:?}: {}", self.kind, self.message)
    }
}

impl std::error::Error for Error {}

pub type Result<T> = std::result::Result<T, Error>;

/// A kind of agent, such as Claude Code or OpenCode.
pub trait Backend: Send + Sync {
    type Session: Session;

    /// The stable identifier used in [`SessionSpec`]-creating requests, such as `claude-code`.
    fn name(&self) -> &str;

    fn capabilities(&self) -> Capabilities;

    /// Existing sessions that can be attached. Empty when attaching is unsupported.
    fn sessions(&self, directory: &str)
    -> impl Future<Output = Result<Vec<SessionSummary>>> + Send;

    /// Start a new session.
    fn create(&self, spec: SessionSpec) -> impl Future<Output = Result<Self::Session>> + Send;

    /// Bind to an existing session.
    fn attach(
        &self,
        id: &str,
        directory: &str,
    ) -> impl Future<Output = Result<Self::Session>> + Send;
}

/// One live agent session, bound to one space.
///
/// Methods take `&self` so that one task can wait on [`Session::next_event`] while others send commands.
pub trait Session: Send + Sync + 'static {
    fn id(&self) -> String;

    /// The next event, waiting until one is available. Returns [`Event::Closed`] once, then errors with [`ErrorKind::Closed`].
    fn next_event(&self) -> impl Future<Output = Result<Event>> + Send;

    fn snapshot(&self) -> impl Future<Output = Result<Snapshot>> + Send;

    /// Deliver a prompt. Only called while the session is idle; the server queues input otherwise.
    fn send(&self, prompt: Prompt) -> impl Future<Output = Result<()>> + Send;

    fn decide(
        &self,
        permission: &str,
        decision: Decision,
    ) -> impl Future<Output = Result<()>> + Send;

    fn answer(
        &self,
        question: &str,
        answers: Vec<(String, AnswerValue)>,
    ) -> impl Future<Output = Result<()>> + Send;

    fn interrupt(&self) -> impl Future<Output = Result<()>> + Send;

    /// Stop the session's process or connection. The agent's own history is kept.
    fn close(&self) -> impl Future<Output = Result<()>> + Send;
}
