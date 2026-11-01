// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

//! The set of agent backends this server can bind spaces to.
//!
//! Backends are concrete types from separate crates. To add one: add a variant to [`AnyBackend`] and [`AnySession`] (behind a Cargo feature if it is optional), a `From` conversion for each, and an arm to the `dispatch!` macro below. Nothing else in the server names a concrete backend.

// Built without any backend, the backend enums are empty and every match on them is vacuous.
#![cfg_attr(
    not(any(feature = "fake", feature = "claude-code", feature = "opencode")),
    allow(unused_variables, unused_mut, unreachable_code)
)]

use backend::{
    AnswerValue, Backend, Capabilities, Decision, Event, Prompt, Result, Session, SessionSpec,
    SessionSummary, Snapshot,
};

#[cfg(feature = "fake")]
use crate::fake::{FakeBackend, FakeSession};
#[cfg(feature = "claude-code")]
use backend_claude_code::{ClaudeCode, ClaudeCodeSession};
#[cfg(feature = "opencode")]
use backend_opencode::{OpenCode, OpenCodeSession};

/// One of the backends compiled into this server.
pub enum AnyBackend {
    #[cfg(feature = "fake")]
    Fake(FakeBackend),
    #[cfg(feature = "claude-code")]
    ClaudeCode(ClaudeCode),
    #[cfg(feature = "opencode")]
    OpenCode(OpenCode),
}

/// A session of one of the backends compiled into this server.
// One session is held per space, so the variant size difference is not worth an allocation.
#[allow(clippy::large_enum_variant)]
pub enum AnySession {
    #[cfg(feature = "fake")]
    Fake(FakeSession),
    #[cfg(feature = "claude-code")]
    ClaudeCode(ClaudeCodeSession),
    #[cfg(feature = "opencode")]
    OpenCode(OpenCodeSession),
}

/// Forward a method call to the concrete value inside an [`AnyBackend`] or [`AnySession`].
macro_rules! dispatch {
    ($value:expr, $inner:ident => $call:expr) => {
        match *$value {
            #[cfg(feature = "fake")]
            Self::Fake(ref $inner) => $call,
            #[cfg(feature = "claude-code")]
            Self::ClaudeCode(ref $inner) => $call,
            #[cfg(feature = "opencode")]
            Self::OpenCode(ref $inner) => $call,
        }
    };
}

#[cfg(feature = "fake")]
impl From<FakeBackend> for AnyBackend {
    fn from(backend: FakeBackend) -> Self {
        Self::Fake(backend)
    }
}

#[cfg(feature = "fake")]
impl From<FakeSession> for AnySession {
    fn from(session: FakeSession) -> Self {
        Self::Fake(session)
    }
}

#[cfg(feature = "claude-code")]
impl From<ClaudeCode> for AnyBackend {
    fn from(backend: ClaudeCode) -> Self {
        Self::ClaudeCode(backend)
    }
}

#[cfg(feature = "claude-code")]
impl From<ClaudeCodeSession> for AnySession {
    fn from(session: ClaudeCodeSession) -> Self {
        Self::ClaudeCode(session)
    }
}

#[cfg(feature = "opencode")]
impl From<OpenCode> for AnyBackend {
    fn from(backend: OpenCode) -> Self {
        Self::OpenCode(backend)
    }
}

#[cfg(feature = "opencode")]
impl From<OpenCodeSession> for AnySession {
    fn from(session: OpenCodeSession) -> Self {
        Self::OpenCode(session)
    }
}

impl Backend for AnyBackend {
    type Session = AnySession;

    fn name(&self) -> &str {
        dispatch!(self, backend => backend.name())
    }

    fn capabilities(&self) -> Capabilities {
        dispatch!(self, backend => backend.capabilities())
    }

    async fn sessions(&self, directory: &str) -> Result<Vec<SessionSummary>> {
        dispatch!(self, backend => backend.sessions(directory).await)
    }

    async fn create(&self, spec: SessionSpec) -> Result<AnySession> {
        dispatch!(self, backend => backend.create(spec).await.map(AnySession::from))
    }

    async fn attach(&self, id: &str, directory: &str) -> Result<AnySession> {
        dispatch!(self, backend => backend.attach(id, directory).await.map(AnySession::from))
    }
}

impl Session for AnySession {
    fn id(&self) -> String {
        dispatch!(self, session => session.id())
    }

    async fn next_event(&self) -> Result<Event> {
        dispatch!(self, session => session.next_event().await)
    }

    async fn snapshot(&self) -> Result<Snapshot> {
        dispatch!(self, session => session.snapshot().await)
    }

    async fn send(&self, prompt: Prompt) -> Result<()> {
        dispatch!(self, session => session.send(prompt).await)
    }

    async fn decide(&self, permission: &str, decision: Decision) -> Result<()> {
        dispatch!(self, session => session.decide(permission, decision).await)
    }

    async fn answer(&self, question: &str, answers: Vec<(String, AnswerValue)>) -> Result<()> {
        dispatch!(self, session => session.answer(question, answers).await)
    }

    async fn interrupt(&self) -> Result<()> {
        dispatch!(self, session => session.interrupt().await)
    }

    async fn close(&self) -> Result<()> {
        dispatch!(self, session => session.close().await)
    }
}

/// The backends a server offers, looked up by [`Backend::name`].
#[derive(Default)]
pub struct Backends {
    backends: Vec<AnyBackend>,
}

impl Backends {
    pub fn new() -> Self {
        Self::default()
    }

    /// Add a backend. A later backend with the same name is never selected.
    pub fn with(mut self, backend: impl Into<AnyBackend>) -> Self {
        self.backends.push(backend.into());
        self
    }

    pub fn get(&self, name: &str) -> Option<&AnyBackend> {
        self.backends.iter().find(|backend| backend.name() == name)
    }

    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.backends.iter().map(|backend| backend.name())
    }
}
