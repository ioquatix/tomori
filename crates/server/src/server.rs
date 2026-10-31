// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

use std::future::pending;
use std::net::SocketAddr;
use std::pin::pin;
use std::sync::Arc;
use std::time::Duration;

use backend::{Backend, Event, Session, SessionSpec, Snapshot};
use base_http_server::{Error, Statistics};
use base_scheduler::{
    Cancellation, OperationResult, SocketAccept, SocketBind, SocketClose, SocketListen, SocketOpen,
    SocketReuseAddress, SocketSetOption, SocketStreamReading, SocketStreamWriting, SocketType,
    Timers,
};
use futures_lite::future::zip;
use futures_util::future::{Either, select};
use futures_util::stream::{FuturesUnordered, StreamExt};
use protocol::Keepalive;

use crate::admission::{Admission, Admitted, DEFAULT_REQUEST_TIMEOUT};
use crate::application::Application;
use crate::backends::{AnySession, Backends};
use crate::hub::{CreateRequest, Hub, SpaceCommand};
use crate::transcriber::Transcriber;
use crate::voice::{Voice, Voices};
use crate::workspace::{DEFAULT_MAX_SPACES, DIRECTORY_UNAVAILABLE, SpaceSlots, WorkspaceRoots};

/// A Tomori server: the HTTP application plus the supervisor that owns every space's backend session.
///
/// [`run`](Self::run) serves until cancelled. All work is owned by that one future: connections, space drivers and the keepalive sweep are joined by it. No detached tasks are created. Speech recognition and synthesis run on the speech crate's own threads, which outlive a space only to finish speech already queued.
pub struct Server {
    hub: Arc<Hub>,
    jobs: async_channel::Receiver<CreateRequest>,
    application: Application,
    voices: Voices,
    keepalive: Keepalive,
    admission: Admission,
    request_timeout: Duration,
    retention: Duration,
    /// Where spaces may run; `None` until configured, which means the home directory when the server runs.
    roots: Option<WorkspaceRoots>,
    slots: Arc<SpaceSlots>,
}

/// How long a disconnected participant keeps their identity, audio source and space, so that a headset that slept or walked out of range for a while comes back where it was. See `design/spaces.md`.
pub const DEFAULT_PARTICIPANT_RETENTION: Duration = Duration::from_secs(12 * 60 * 60);

/// The most connections served at once; further peers wait in the listen backlog.
const MAX_CONNECTIONS: usize = 256;

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
        let slots = SpaceSlots::new(DEFAULT_MAX_SPACES);
        let hub = Hub::new(backends, transcriber, sender, slots.clone());
        Self {
            application: Application::new(hub.clone()),
            hub,
            jobs,
            voices: Voices::default(),
            keepalive: Keepalive::default(),
            admission: Admission::default(),
            request_timeout: DEFAULT_REQUEST_TIMEOUT,
            retention: DEFAULT_PARTICIPANT_RETENTION,
            roots: None,
            slots,
        }
    }

    /// Run spaces only in these directories or beneath them. Without this, the only root is the home directory. See `design/workspaces.md`.
    pub fn with_workspace_roots(mut self, roots: WorkspaceRoots) -> Self {
        self.roots = Some(roots);
        self
    }

    /// Use `roots` unless roots were already given.
    #[cfg(feature = "fake")]
    pub(crate) fn with_default_workspace_roots(mut self, roots: WorkspaceRoots) -> Self {
        self.roots.get_or_insert(roots);
        self
    }

    /// Change how many spaces may have a session starting or running at once; see [`DEFAULT_MAX_SPACES`].
    pub fn with_max_spaces(self, limit: usize) -> Self {
        self.slots.set_limit(limit);
        self
    }

    /// Change how long a disconnected participant is remembered; see [`DEFAULT_PARTICIPANT_RETENTION`].
    pub fn with_participant_retention(mut self, retention: Duration) -> Self {
        self.retention = retention;
        self
    }

    /// Change how long a connection may go without a complete request head, or a `/connect` exchange without Hello, before it is closed. The default is 10 seconds. See `design/server.md`.
    pub fn with_request_timeout(mut self, timeout: Duration) -> Self {
        self.request_timeout = timeout;
        self
    }

    /// Change when quiet connections are pinged and when silent ones are released. See `design/connection-liveness.md`.
    pub fn with_keepalive(mut self, keepalive: Keepalive) -> Self {
        self.keepalive = keepalive;
        self
    }

    /// Speak each space's assistant replies with `synthesizer`, shared by every space. Without one, spaces are silent.
    #[cfg(feature = "speech")]
    pub fn with_synthesizer(mut self, synthesizer: impl speech::Synthesizer + 'static) -> Self {
        self.voices = Voices::new(synthesizer);
        self
    }

    /// Serve `listener` until `cancellation` is cancelled, then close every session and connection.
    pub async fn run<S>(
        &self,
        scheduler: &S,
        listener: S::Socket,
        cancellation: &Cancellation,
    ) -> Result<Statistics, Error>
    where
        S: SocketAccept + SocketStreamReading + SocketStreamWriting + SocketClose + Timers,
        S::Socket: 'static,
    {
        let ((served, ()), ((), ())) = zip(
            zip(
                self.serve(scheduler, listener, cancellation),
                self.supervise(cancellation),
            ),
            zip(
                self.sweep(scheduler, cancellation),
                self.pace(scheduler, cancellation),
            ),
        )
        .await;
        served
    }

    /// Send space snapshots that were deferred to keep a streaming reply within [`crate::hub::SNAPSHOT_INTERVAL`], when they are due. Sleeps until a space defers one.
    async fn pace<S: Timers>(&self, scheduler: &S, cancellation: &Cancellation) {
        let mut timer = scheduler.timer();
        loop {
            let listener = self.hub.listen_snapshots();
            match self.hub.snapshot_deadline() {
                Some(deadline) => {
                    if let Err(error) = scheduler
                        .timer_wait_until(&mut timer, deadline, cancellation)
                        .await
                    {
                        if !cancellation.is_cancelled() {
                            log::error!("The snapshot timer failed: {error}");
                        }
                        return;
                    }
                    self.hub.flush_snapshots(scheduler.now());
                }
                None => {
                    if let Either::Right(_) =
                        select(pin!(listener), pin!(cancellation.cancelled())).await
                    {
                        return;
                    }
                }
            }
        }
    }

    /// Accept and serve HTTP connections until cancelled.
    ///
    /// TODO(upstream): this follows `base_http_server::serve`, which passes every handler one cancellation shared by all connections. The keepalive sweep must end one silent connection, including a response write blocked on a peer that is gone, so each connection here gets its own child cancellation. Use `serve` again once it offers per-connection cancellation.
    async fn serve<S>(
        &self,
        scheduler: &S,
        listener: S::Socket,
        cancellation: &Cancellation,
    ) -> Result<Statistics, Error>
    where
        S: SocketAccept + SocketStreamReading + SocketStreamWriting + SocketClose,
        S::Socket: 'static,
    {
        let options = base_http_server::Options::default().http1;
        let connections_cancellation = cancellation.child();
        let mut connections = FuturesUnordered::new();
        let mut statistics = Statistics::default();
        let result: Result<(), Error> = async {
            loop {
                if connections.len() == MAX_CONNECTIONS {
                    let outcome = {
                        let next = pin!(connections.next());
                        match select(next, pin!(cancellation.cancelled())).await {
                            Either::Left((outcome, _)) => outcome,
                            Either::Right(_) => return Err(Error::Cancelled),
                        }
                    };
                    record(&mut statistics, outcome.expect("connections are waiting"));
                    continue;
                }
                // Keep this accept alive while existing connections complete, so an accepted peer is never discarded.
                let accepted = {
                    let mut accept = pin!(scheduler.socket_accept(&listener, cancellation));
                    loop {
                        let completed = pin!(async {
                            if connections.is_empty() {
                                pending().await
                            } else {
                                connections.next().await.expect("connections are waiting")
                            }
                        });
                        match select(accept.as_mut(), completed).await {
                            Either::Left((accepted, _)) => break accepted,
                            Either::Right((outcome, _)) => record(&mut statistics, outcome),
                        }
                    }
                };
                let (socket, _) = accepted.map_err(Error::from)?;
                statistics.connections += 1;
                let connection = connections_cancellation.child();
                let options = &options;
                // Each connection is watched so that one which never completes a request is closed, freeing its slot.
                let handler = Admitted {
                    application: &self.application,
                    watch: self.admission.watch(connection.clone()),
                };
                connections.push(async move {
                    base_http_server::serve_connection(
                        scheduler,
                        socket,
                        &handler,
                        options,
                        &connection,
                    )
                    .await
                });
            }
        }
        .await;
        connections_cancellation.cancel();
        while let Some(outcome) = connections.next().await {
            record(&mut statistics, outcome);
        }
        scheduler.socket_close(listener).await?;
        match result {
            Err(_) if cancellation.is_cancelled() => Ok(statistics),
            Ok(()) => Ok(statistics),
            Err(error) => Err(error),
        }
    }

    /// Run the keepalive and request deadline sweeps until cancelled, often enough that a connection is released within a quarter of the shortest interval after its deadline.
    async fn sweep<S: Timers>(&self, scheduler: &S, cancellation: &Cancellation) {
        let period = (self
            .keepalive
            .interval
            .min(self.keepalive.timeout)
            .min(self.request_timeout)
            .min(self.retention)
            / 4)
        .max(Duration::from_millis(1));
        let mut timer = scheduler.timer();
        loop {
            let deadline = scheduler.now() + period;
            match scheduler
                .timer_wait_until(&mut timer, deadline, cancellation)
                .await
            {
                Ok(()) => {
                    let now = scheduler.now();
                    self.admission.sweep(now, self.request_timeout);
                    self.hub.sweep(now, &self.keepalive, self.request_timeout);
                    self.hub.forget_departed(now, self.retention);
                }
                Err(error) => {
                    if !cancellation.is_cancelled() {
                        log::error!("The keepalive timer failed: {error}");
                    }
                    return;
                }
            }
        }
    }

    /// Start requested spaces and drive their sessions until cancelled.
    async fn supervise(&self, cancellation: &Cancellation) {
        let roots = match &self.roots {
            Some(roots) => roots.clone(),
            None => WorkspaceRoots::home().unwrap_or_else(|error| {
                log::error!("No workspace roots, so every directory is refused: {error}.");
                WorkspaceRoots::none()
            }),
        };
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
                Ok(request) => spaces.push(run_space(
                    self.hub.clone(),
                    request,
                    self.voices.clone(),
                    &roots,
                )),
                Err(_) => break,
            }
        }
        self.hub.shutdown();
        while spaces.next().await.is_some() {}
    }
}

/// Count one finished connection.
fn record(statistics: &mut Statistics, outcome: Result<u64, Error>) {
    match outcome {
        Ok(requests) => statistics.requests += requests,
        Err(Error::Cancelled) => {}
        Err(error) => {
            log::debug!("A connection failed: {error}");
            statistics.failed_connections += 1;
        }
    }
}

/// Start one space's session, then drive it until it closes or the server shuts down.
async fn run_space(
    hub: Arc<Hub>,
    mut request: CreateRequest,
    voices: Voices,
    roots: &WorkspaceRoots,
) {
    let Some(backend) = hub.backends.get(&request.backend) else {
        return;
    };
    // Creating and attaching both run the agent in the directory, so both are confined to the workspace roots. The space shows the canonical directory.
    match roots.resolve_str(&request.directory) {
        Ok(directory) => request.directory = directory,
        Err(_) => {
            hub.create_failed(&request.creator, DIRECTORY_UNAVAILABLE.to_owned());
            return;
        }
    }
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
    let Some(space) = hub.register_space(&request, session.id(), snapshot, commands) else {
        // The server began shutting down while the session started.
        if let Err(error) = session.close().await {
            log::warn!("Closing a session started during shutdown failed: {error}");
        }
        return;
    };
    if let Some(error) = error {
        hub.space_event(&space, Event::Error { message: error });
    }
    let voice = voices.start(&hub, &space);
    let error = drive(&hub, &space, &session, receiver, voice).await;
    hub.space_ended(&space, error);
    if let Err(error) = session.close().await {
        log::warn!("Closing the session of {space} failed: {error}");
    }
}

/// Forward session events to the hub and the space's voice while performing the space's commands. Returns why the session stopped, if it failed.
async fn drive(
    hub: &Hub,
    space: &str,
    session: &AnySession,
    commands: async_channel::Receiver<SpaceCommand>,
    mut voice: Option<Voice>,
) -> Option<String> {
    let events = async {
        loop {
            match session.next_event().await {
                Ok(event) => {
                    let closed = matches!(event, Event::Closed { .. });
                    // Speech segmentation and queueing never wait for synthesis, and happen outside the hub lock.
                    if let Some(voice) = voice.as_mut() {
                        voice.observe(&event);
                    }
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
