// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

//! The OpenCode backend for the Tomori server.
//!
//! One backend binds to one running `opencode serve` process. It owns a
//! dedicated thread that holds the scheduler, HTTP client, driver, and the
//! server's one global event stream; sessions are `'static` handles that talk
//! to that thread over channels. Permissions and questions are answered only
//! when a participant acts. See `design/agent-backends.md`.

use std::sync::Mutex;

use backend::{Backend, Capabilities, Error, ErrorKind, Result, SessionSpec, SessionSummary};

mod api;
mod connection;
mod endpoint;
mod session;
mod wire;

pub use session::OpenCodeSession;

/// The backend name, as used in create-space requests.
pub const NAME: &str = "opencode";

/// How to reach the OpenCode server.
#[derive(Clone, Debug, Default)]
pub struct Config {
    /// The explicit URL, from `--opencode-url`. `None` falls back to the
    /// `OPENCODE_URL` environment variable, the XDG service file, then the
    /// well-known default.
    pub url: Option<String>,
}

/// One connection to the OpenCode server, shared by every session it hosts.
struct Connection {
    commands: connection::Commands,
    /// The thread, kept alive for the connection's lifetime.
    #[allow(dead_code)]
    thread: Option<std::thread::JoinHandle<()>>,
}

/// The OpenCode backend.
pub struct OpenCode {
    config: Config,
    /// `None` until the first use; `Some(Ok(_))` once the thread is running;
    /// `Some(Err(_))` if starting the thread failed.
    connection: Mutex<Option<std::result::Result<Connection, Error>>>,
}

impl OpenCode {
    pub fn new(config: Config) -> Self {
        Self {
            config,
            connection: Mutex::new(None),
        }
    }

    /// The shared connection's commands channel, starting the thread on first
    /// use. The sender is cloned out so no borrow of the locked `Connection`
    /// escapes the guard.
    fn connection(&self) -> Result<connection::Commands> {
        let mut guard = self
            .connection
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        match guard.as_mut() {
            Some(Ok(connection)) => Ok(connection.commands.clone()),
            Some(Err(error)) => Err(error.clone()),
            None => {
                let resolved = endpoint::resolve(self.config.url.as_deref());
                let (commands, receiver) = connection::Commands::new();
                let thread = std::thread::Builder::new()
                    .name("opencode-connection".to_owned())
                    .spawn(move || {
                        connection::run_thread(resolved, receiver);
                    })
                    .map_err(|error| {
                        Error::new(
                            ErrorKind::Unavailable,
                            format!("could not start the connection thread: {error}"),
                        )
                    })?;
                let value = Connection {
                    commands: commands.clone(),
                    thread: Some(thread),
                };
                *guard = Some(Ok(value));
                Ok(commands)
            }
        }
    }
}

impl Backend for OpenCode {
    type Session = OpenCodeSession;

    fn name(&self) -> &str {
        NAME
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
        let commands = self.connection()?;
        let directory = directory.to_owned();
        commands
            .reply_with(move |reply| connection::Command::Sessions { directory, reply })
            .await
    }

    async fn create(&self, spec: SessionSpec) -> Result<OpenCodeSession> {
        let commands = self.connection()?;
        let (id, events) = commands
            .reply_with(move |reply| connection::Command::Create { spec, reply })
            .await?;
        Ok(OpenCodeSession::new(id, events, commands))
    }

    async fn attach(&self, id: &str, directory: &str) -> Result<OpenCodeSession> {
        let commands = self.connection()?;
        let id = id.to_owned();
        let directory = directory.to_owned();
        let attach_id = id.clone();
        let events = commands
            .reply_with(move |reply| connection::Command::Attach {
                id: attach_id,
                directory,
                reply,
            })
            .await?;
        Ok(OpenCodeSession::new(id, events, commands))
    }
}
