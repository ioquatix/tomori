// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

//! A scripted in-memory backend for tests and for running the server without an agent.
//!
//! In automatic mode, each prompt is answered immediately with a streamed echo. In manual mode, a turn stays busy until the test calls [`FakeSession::reply`], so input queues up behind it. Tests can inject any [`Event`] and inspect what reached the session.

use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use backend::{
    AnswerValue, Backend, Capabilities, Decision, Error, ErrorKind, Event, Prompt, Result, Role,
    Session, SessionSpec, SessionSummary, Snapshot,
};

/// The backend name used in `CreateSpace` requests.
pub const FAKE_BACKEND_NAME: &str = "fake";

/// Everything the server asked a [`FakeSession`] to do, in order.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct FakeLog {
    pub prompts: Vec<Prompt>,
    pub decisions: Vec<(String, Decision)>,
    pub answers: Vec<(String, Vec<(String, AnswerValue)>)>,
    pub interrupts: usize,
}

#[derive(Clone, Default)]
pub struct FakeBackend {
    shared: Arc<FakeShared>,
}

#[derive(Default)]
struct FakeShared {
    manual: AtomicBool,
    next: AtomicU64,
    sessions: Mutex<Vec<FakeSession>>,
}

impl FakeBackend {
    /// A backend whose sessions answer every prompt immediately.
    pub fn new() -> Self {
        Self::default()
    }

    /// A backend whose turns stay busy until [`FakeSession::reply`] is called.
    pub fn manual() -> Self {
        let backend = Self::default();
        backend.shared.manual.store(true, Ordering::SeqCst);
        backend
    }

    /// Every session created or attached so far, oldest first.
    pub fn started(&self) -> Vec<FakeSession> {
        self.shared.sessions.lock().unwrap().clone()
    }

    fn start(&self, title: &str, directory: &str) -> FakeSession {
        let number = self.shared.next.fetch_add(1, Ordering::SeqCst) + 1;
        let (sender, receiver) = async_channel::unbounded();
        let session = FakeSession {
            inner: Arc::new(FakeSessionInner {
                id: format!("fake-{number}"),
                title: title.to_owned(),
                directory: directory.to_owned(),
                manual: self.shared.manual.load(Ordering::SeqCst),
                sender,
                receiver,
                busy: AtomicBool::new(false),
                closed: AtomicBool::new(false),
                next_message: AtomicU64::new(0),
                delivered: Mutex::new(HashSet::new()),
                log: Mutex::new(FakeLog::default()),
            }),
        };
        self.shared.sessions.lock().unwrap().push(session.clone());
        session
    }
}

impl Backend for FakeBackend {
    type Session = FakeSession;

    fn name(&self) -> &str {
        FAKE_BACKEND_NAME
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            attach: true,
            interrupt: true,
            questions: true,
            workers: true,
        }
    }

    async fn sessions(&self, directory: &str) -> Result<Vec<SessionSummary>> {
        Ok(self
            .started()
            .iter()
            .filter(|session| session.inner.directory == directory)
            .map(|session| SessionSummary {
                id: session.inner.id.clone(),
                title: session.inner.title.clone(),
                directory: session.inner.directory.clone(),
            })
            .collect())
    }

    async fn create(&self, spec: SessionSpec) -> Result<FakeSession> {
        Ok(self.start(&spec.title, &spec.directory))
    }

    async fn attach(&self, id: &str, _directory: &str) -> Result<FakeSession> {
        self.started()
            .into_iter()
            .find(|session| session.inner.id == id)
            .ok_or_else(|| Error::new(ErrorKind::NotFound, format!("no fake session {id}")))
    }
}

/// A handle to one fake session. Clones share the session.
#[derive(Clone)]
pub struct FakeSession {
    inner: Arc<FakeSessionInner>,
}

struct FakeSessionInner {
    id: String,
    title: String,
    directory: String,
    manual: bool,
    sender: async_channel::Sender<Event>,
    receiver: async_channel::Receiver<Event>,
    busy: AtomicBool,
    closed: AtomicBool,
    next_message: AtomicU64,
    delivered: Mutex<HashSet<String>>,
    log: Mutex<FakeLog>,
}

impl FakeSession {
    /// Report an event as if the agent produced it.
    pub fn emit(&self, event: Event) {
        match event {
            Event::Busy => self.inner.busy.store(true, Ordering::SeqCst),
            Event::Idle => self.inner.busy.store(false, Ordering::SeqCst),
            _ => {}
        }
        // The receiver lives as long as the session, so sending cannot fail.
        let _ = self.inner.sender.try_send(event);
    }

    /// Stream an assistant reply in two deltas, complete it, and end the turn.
    pub fn reply(&self, text: &str) {
        let number = self.inner.next_message.fetch_add(1, Ordering::SeqCst) + 1;
        let message = format!("{}-reply-{number}", self.inner.id);
        self.emit(Event::MessageStarted {
            message: message.clone(),
            role: Role::Assistant,
        });
        let middle = text.char_indices().nth(text.chars().count() / 2);
        let (first, second) = text.split_at(middle.map_or(text.len(), |(index, _)| index));
        for part in [first, second] {
            if !part.is_empty() {
                self.emit(Event::TextDelta {
                    message: message.clone(),
                    text: part.to_owned(),
                });
            }
        }
        self.emit(Event::MessageCompleted { message });
        self.emit(Event::Idle);
    }

    pub fn log(&self) -> FakeLog {
        self.inner.log.lock().unwrap().clone()
    }

    pub fn is_busy(&self) -> bool {
        self.inner.busy.load(Ordering::SeqCst)
    }
}

impl Session for FakeSession {
    fn id(&self) -> String {
        self.inner.id.clone()
    }

    async fn next_event(&self) -> Result<Event> {
        if self.inner.closed.load(Ordering::SeqCst) {
            return Err(Error::new(ErrorKind::Closed, "the fake session closed"));
        }
        let event = self
            .inner
            .receiver
            .recv()
            .await
            .map_err(|_| Error::new(ErrorKind::Closed, "the fake session closed"))?;
        if matches!(event, Event::Closed { .. }) {
            self.inner.closed.store(true, Ordering::SeqCst);
        }
        Ok(event)
    }

    async fn snapshot(&self) -> Result<Snapshot> {
        Ok(Snapshot {
            busy: Some(self.is_busy()),
            ..Snapshot::default()
        })
    }

    async fn send(&self, prompt: Prompt) -> Result<()> {
        if !self
            .inner
            .delivered
            .lock()
            .unwrap()
            .insert(prompt.key.clone())
        {
            return Ok(());
        }
        self.inner.log.lock().unwrap().prompts.push(prompt.clone());
        self.emit(Event::Delivered {
            key: prompt.key.clone(),
        });
        self.emit(Event::Busy);
        if !self.inner.manual {
            self.reply(&format!("You said: {}", prompt.text));
        }
        Ok(())
    }

    async fn decide(&self, permission: &str, decision: Decision) -> Result<()> {
        self.inner
            .log
            .lock()
            .unwrap()
            .decisions
            .push((permission.to_owned(), decision));
        self.emit(Event::PermissionResolved {
            id: permission.to_owned(),
        });
        Ok(())
    }

    async fn answer(&self, question: &str, answers: Vec<(String, AnswerValue)>) -> Result<()> {
        self.inner
            .log
            .lock()
            .unwrap()
            .answers
            .push((question.to_owned(), answers));
        self.emit(Event::QuestionResolved {
            id: question.to_owned(),
        });
        Ok(())
    }

    async fn interrupt(&self) -> Result<()> {
        self.inner.log.lock().unwrap().interrupts += 1;
        self.emit(Event::Idle);
        Ok(())
    }

    async fn close(&self) -> Result<()> {
        if !self.inner.closed.load(Ordering::SeqCst) {
            self.emit(Event::Closed { reason: None });
        }
        Ok(())
    }
}
