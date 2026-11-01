// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

//! One live OpenCode session: a `'static` handle over the connection thread.
//!
//! The handle owns one end of the session's event channel and shares the
//! connection's commands channel. [`next_event`] reads the channel; the other
//! methods send a command and wait for its reply.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use backend::{AnswerValue, Decision, ErrorKind, Event, Prompt, Result, Session, Snapshot};

use crate::connection::{Command, Commands};
use crate::wire;

/// One session bound to the connection thread.
pub struct OpenCodeSession {
    inner: Arc<Inner>,
}

struct Inner {
    id: String,
    events: async_channel::Receiver<Event>,
    commands: Commands,
    closed: AtomicBool,
}

impl OpenCodeSession {
    /// Build the handle from the thread's reply to a create or attach command.
    pub fn new(id: String, events: async_channel::Receiver<Event>, commands: Commands) -> Self {
        Self {
            inner: Arc::new(Inner {
                id,
                events,
                commands,
                closed: AtomicBool::new(false),
            }),
        }
    }
}

impl Session for OpenCodeSession {
    fn id(&self) -> String {
        self.inner.id.clone()
    }

    fn next_event(&self) -> impl std::future::Future<Output = Result<Event>> + Send {
        let inner = Arc::clone(&self.inner);
        async move {
            let event = inner.events.recv().await.map_err(|error| {
                let kind = if inner.closed.load(Ordering::Acquire) {
                    ErrorKind::Closed
                } else {
                    ErrorKind::Unavailable
                };
                backend::Error::new(kind, error.to_string())
            })?;
            if matches!(event, Event::Closed { .. }) {
                inner.closed.store(true, Ordering::Release);
            }
            Ok(event)
        }
    }

    fn snapshot(&self) -> impl std::future::Future<Output = Result<Snapshot>> + Send {
        let id = self.inner.id.clone();
        let commands = self.inner.commands.clone();
        async move {
            commands
                .reply_with(move |reply| Command::Snapshot { id, reply })
                .await
        }
    }

    fn send(&self, prompt: Prompt) -> impl std::future::Future<Output = Result<()>> + Send {
        let id = self.inner.id.clone();
        let commands = self.inner.commands.clone();
        async move {
            commands
                .reply_with(move |reply| Command::Prompt {
                    id,
                    key: prompt.key,
                    text: prompt.text,
                    reply,
                })
                .await
        }
    }

    fn decide(
        &self,
        permission: &str,
        decision: Decision,
    ) -> impl std::future::Future<Output = Result<()>> + Send {
        let id = self.inner.id.clone();
        let commands = self.inner.commands.clone();
        async move {
            // OpenCode's reply is a flat `once` / `always` / `reject`; the
            // session's single standing grant is what `always` applies.
            let decision = match decision {
                Decision::AllowOnce => "once",
                Decision::Grant { .. } => "always",
                Decision::Deny => "reject",
            };
            let decision = decision.to_owned();
            commands
                .reply_with(move |reply| Command::Decide {
                    id,
                    request: permission.to_owned(),
                    decision,
                    reply,
                })
                .await
        }
    }

    fn answer(
        &self,
        question: &str,
        answers: Vec<(String, AnswerValue)>,
    ) -> impl std::future::Future<Output = Result<()>> + Send {
        let id = self.inner.id.clone();
        let commands = self.inner.commands.clone();
        async move {
            let answer = wire::form_answer(&answers);
            commands
                .reply_with(move |reply| Command::Answer {
                    id,
                    form: question.to_owned(),
                    answer,
                    reply,
                })
                .await
        }
    }

    fn interrupt(&self) -> impl std::future::Future<Output = Result<()>> + Send {
        let id = self.inner.id.clone();
        let commands = self.inner.commands.clone();
        async move {
            commands
                .reply_with(move |reply| Command::Interrupt { id, reply })
                .await
        }
    }

    fn close(&self) -> impl std::future::Future<Output = Result<()>> + Send {
        let id = self.inner.id.clone();
        let commands = self.inner.commands.clone();
        let inner = Arc::clone(&self.inner);
        async move {
            inner.closed.store(true, Ordering::Release);
            // Closing the last session stops the thread; there is no reply
            // channel for a close, so fire and forget.
            let _ = commands.tx().try_send(Command::Close { id });
            Ok(())
        }
    }
}
