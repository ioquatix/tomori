// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

//! The set of agent backends this server can bind spaces to.
//!
//! Backends are concrete types from separate crates. To add one: add a variant to [`AnyBackend`] and [`AnySession`] (behind a Cargo feature if it is optional), a `From` conversion for each, and an arm to the `dispatch!` macro below. Nothing else in the server names a concrete backend.

use backend::{
    AnswerValue, Backend, Capabilities, Decision, Event, Prompt, Result, Session, SessionSpec,
    SessionSummary, Snapshot,
};

#[cfg(feature = "fake")]
use crate::fake::{FakeBackend, FakeSession};

/// One of the backends compiled into this server.
pub enum AnyBackend {
    #[cfg(feature = "fake")]
    Fake(FakeBackend),
}

/// A session of one of the backends compiled into this server.
pub enum AnySession {
    #[cfg(feature = "fake")]
    Fake(FakeSession),
}

/// Forward a method call to the concrete value inside an [`AnyBackend`] or [`AnySession`].
macro_rules! dispatch {
    ($value:expr, $inner:ident => $call:expr) => {
        match *$value {
            #[cfg(feature = "fake")]
            Self::Fake(ref $inner) => $call,
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
