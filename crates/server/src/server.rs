// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

use std::future::pending;
use std::net::SocketAddr;
use std::pin::pin;
use std::sync::Arc;

use backend::{Backend, Event, Session, SessionSpec, Snapshot};
use base_http_server::{Options, Statistics};
use base_scheduler::{
    Cancellation, OperationResult, SocketAccept, SocketBind, SocketClose, SocketListen, SocketOpen,
    SocketReuseAddress, SocketSetOption, SocketStreamReading, SocketStreamWriting, SocketType,
};
use futures_lite::future::zip;
use futures_util::future::{Either, select};
use futures_util::stream::{FuturesUnordered, StreamExt};

use crate::application::Application;
use crate::backends::{AnySession, Backends};
use crate::hub::{CreateRequest, Hub, SpaceCommand};
use crate::transcriber::Transcriber;

/// A Tomori server: the HTTP application plus the supervisor that owns every space's backend session.
///
/// [`run`](Self::run) serves until cancelled. All work is owned by that one future: connections are joined by the HTTP server, and space drivers by the supervisor. No detached tasks are created.
pub struct Server {
    hub: Arc<Hub>,
    jobs: async_channel::Receiver<CreateRequest>,
    application: Application,
}

/// Open a listening TCP socket.
pub async fn bind<S>(
    scheduler: &S,
    address: SocketAddr,
    cancellation: &Cancellation,
) -> OperationResult<S::Socket>
where
    S: SocketOpen + SocketBind + SocketListen + SocketSetOption<SocketReuseAddress>,
{
    let socket = scheduler
        .socket_open(address.into(), SocketType::Stream, None, cancellation)
        .await?;
    scheduler
        .socket_set_option(&socket, SocketReuseAddress, true, cancellation)
        .await?;
    scheduler
        .socket_bind(&socket, address, cancellation)
        .await?;
    scheduler.socket_listen(&socket, 128, cancellation).await?;
    Ok(socket)
}

impl Server {
    pub fn new(backends: Backends, transcriber: Arc<dyn Transcriber>) -> Self {
        let (sender, jobs) = async_channel::unbounded();
        let hub = Hub::new(backends, transcriber, sender);
        Self {
            application: Application::new(hub.clone()),
            hub,
            jobs,
        }
    }

    /// Serve `listener` until `cancellation` is cancelled, then close every session and connection.
    pub async fn run<S>(
        &self,
        scheduler: &S,
        listener: S::Socket,
        cancellation: &Cancellation,
    ) -> Result<Statistics, base_http_server::Error>
    where
        S: SocketAccept + SocketStreamReading + SocketStreamWriting + SocketClose,
        S::Socket: 'static,
    {
        let options = Options::default();
        let serving = base_http_server::serve(
            scheduler,
            listener,
            &self.application,
            &options,
            cancellation,
        );
        let (served, ()) = zip(serving, self.supervise(cancellation)).await;
        served
    }

    /// Start requested spaces and drive their sessions until cancelled.
    async fn supervise(&self, cancellation: &Cancellation) {
        let mut spaces = FuturesUnordered::new();
        let mut cancelled = pin!(cancellation.cancelled());
        loop {
            let job = {
                let job = pin!(self.jobs.recv());
                let finished = pin!(async {
                    if spaces.is_empty() {
                        pending::<()>().await;
                    }
                    spaces.next().await;
                });
                match select(cancelled.as_mut(), select(job, finished)).await {
                    Either::Left(_) => break,
                    Either::Right((Either::Left((job, _)), _)) => job,
                    Either::Right((Either::Right(_), _)) => continue,
                }
            };
            match job {
                Ok(request) => spaces.push(run_space(self.hub.clone(), request)),
                Err(_) => break,
            }
        }
        self.hub.shutdown();
        while spaces.next().await.is_some() {}
    }
}

/// Start one space's session, then drive it until it closes or the server shuts down.
async fn run_space(hub: Arc<Hub>, request: CreateRequest) {
    let Some(backend) = hub.backends.get(&request.backend) else {
        return;
    };
    let session = match &request.session {
        Some(id) => backend.attach(id, &request.directory).await,
        None => {
            backend
                .create(SessionSpec {
                    directory: request.directory.clone(),
                    title: request.title.clone(),
                })
                .await
        }
    };
    let session = match session {
        Ok(session) => session,
        Err(error) => {
            hub.create_failed(
                &request.creator,
                format!(
                    "Could not start the {} session: {}",
                    request.backend, error.message
                ),
            );
            return;
        }
    };
    let (snapshot, error) = match session.snapshot().await {
        Ok(snapshot) => (snapshot, None),
        Err(error) => (Snapshot::default(), Some(error.message)),
    };
    let (commands, receiver) = async_channel::unbounded();
    let space = hub.register_space(&request, session.id(), snapshot, commands);
    if let Some(error) = error {
        hub.space_event(&space, Event::Error { message: error });
    }
    let error = drive(&hub, &space, &session, receiver).await;
    hub.space_ended(&space, error);
    if let Err(error) = session.close().await {
        log::warn!("Closing the session of {space} failed: {error}");
    }
}

/// Forward session events to the hub while performing the space's commands. Returns why the session stopped, if it failed.
async fn drive(
    hub: &Hub,
    space: &str,
    session: &AnySession,
    commands: async_channel::Receiver<SpaceCommand>,
) -> Option<String> {
    let events = async {
        loop {
            match session.next_event().await {
                Ok(event) => {
                    let closed = matches!(event, Event::Closed { .. });
                    hub.space_event(space, event);
                    if closed {
                        return None;
                    }
                }
                Err(error) => return Some(error.message),
            }
        }
    };
    let commands = async {
        while let Ok(command) = commands.recv().await {
            match command {
                SpaceCommand::Send(prompt) => {
                    let key = prompt.key.clone();
                    let result = session.send(prompt).await;
                    hub.space_sent(space, &key, result);
                }
                SpaceCommand::Decide {
                    request,
                    decision,
                    participant,
                } => {
                    let result = session.decide(&request, decision).await;
                    hub.space_decided(space, &request, &participant, result);
                }
                SpaceCommand::Answer {
                    question,
                    answers,
                    participant,
                } => {
                    let result = session.answer(&question, answers).await;
                    hub.space_answered(space, &question, &participant, result);
                }
                SpaceCommand::Close => break,
            }
        }
        None
    };
    match select(pin!(events), pin!(commands)).await {
        Either::Left((error, _)) | Either::Right((error, _)) => error,
    }
}
