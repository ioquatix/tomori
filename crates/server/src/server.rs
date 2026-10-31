// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

use std::collections::BTreeMap;
use std::future::{Future, pending};
use std::io;
use std::net::SocketAddr;
use std::pin::pin;
use std::sync::Arc;
use std::time::Duration;

use backend::{Backend, Event, Session, SessionSpec, Snapshot};
use base_http_server::{Error, Statistics};
use base_scheduler::{
    Cancellation, OperationError, OperationResult, SocketAccept, SocketAddress, SocketBind,
    SocketClose, SocketListen, SocketOpen, SocketReuseAddress, SocketSetOption,
    SocketStreamReading, SocketStreamWriting, SocketType, Timers,
};
use futures_lite::future::zip;
use futures_util::future::{Either, select};
use futures_util::stream::{FuturesUnordered, StreamExt};
use protocol::Keepalive;

use crate::admission::{Admission, Admitted, DEFAULT_REQUEST_TIMEOUT, Watch};
use crate::application::Application;
use crate::backends::{AnySession, Backends};
use crate::hub::{CreateRequest, Hub, SpaceCommand};
use crate::transcriber::Transcriber;
use crate::voice::{Voice, Voices};
use crate::workspace::{
    DEFAULT_MAX_SPACES, DIRECTORY_UNAVAILABLE, RESOLVE_TIMEOUT, Resolving, SpaceSlots,
    WorkspaceRoots,
};

/// A Tomori server: the HTTP application plus the supervisor that owns every space's backend session.
///
/// [`run`](Self::run) serves until cancelled or serving fails. All work is owned by that one future: connections, space drivers and the keepalive sweep are joined by it. No detached tasks are created. Speech recognition and synthesis run on the speech crate's own threads, which outlive a space only to finish speech already queued.
pub struct Server {
    hub: Arc<Hub>,
    jobs: async_channel::Receiver<CreateRequest>,
    application: Application,
    voices: Voices,
    keepalive: Keepalive,
    admission: Admission,
    request_timeout: Duration,
    retention: Duration,
    /// Where spaces may run; `None` until configured, which means the current directory when the server runs.
    roots: Option<WorkspaceRoots>,
    slots: Arc<SpaceSlots>,
    /// How long cancelled connections may take to finish once serving ends; see [`DEFAULT_DRAIN_TIMEOUT`].
    drain_timeout: Duration,
}

/// How long a disconnected participant keeps their identity, audio source and space, so that a headset that slept or walked out of range for a while comes back where it was. See `design/spaces.md`.
pub const DEFAULT_PARTICIPANT_RETENTION: Duration = Duration::from_secs(12 * 60 * 60);

/// The most connections served at once; further peers wait in the listen backlog.
const MAX_CONNECTIONS: usize = 256;

/// How long accepting pauses after the process or system has run out of descriptors, buffers or memory, so that the loop does not spin while connections finish and free them.
const ACCEPT_BACKOFF: Duration = Duration::from_millis(100);

/// How long cancelled connections may take to finish once serving ends, before they are abandoned so that shutdown completes. See `design/server.md`.
pub const DEFAULT_DRAIN_TIMEOUT: Duration = Duration::from_secs(5);

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
            drain_timeout: DEFAULT_DRAIN_TIMEOUT,
        }
    }

    /// Run spaces only in these directories or beneath them. Without this, the only root is the current directory when the server runs. See `design/workspaces.md`.
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
    ///
    /// A failure to serve, such as the listener breaking, also ends the spaces and returns the error, so that the process exits instead of living on without accepting connections. Transient accept errors are not failures; see [`Self::serve`].
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
        // Ends the spaces, the sweep and the pacing once serving has ended for any reason.
        let services = cancellation.child();
        let serving = async {
            let served = self.serve(scheduler, listener, cancellation).await;
            services.cancel();
            served
        };
        let (served, ((), ((), ()))) = zip(
            serving,
            zip(
                self.supervise(scheduler, &services),
                zip(
                    self.sweep(scheduler, &services),
                    self.pace(scheduler, &services),
                ),
            ),
        )
        .await;
        served
    }

    /// Send space snapshots that were deferred to keep a streaming reply within [`crate::hub::SNAPSHOT_INTERVAL`], when they are due. Sleeps until a space defers one, and wakes when another space defers one, which may be due sooner.
    async fn pace<S: Timers>(&self, scheduler: &S, cancellation: &Cancellation) {
        let mut timer = scheduler.timer();
        loop {
            let listener = self.hub.listen_snapshots();
            match self.hub.snapshot_deadline() {
                Some(deadline) => {
                    let due = scheduler.timer_wait_until(&mut timer, deadline, cancellation);
                    match select(pin!(due), pin!(listener)).await {
                        Either::Left((Ok(()), _)) => self.hub.flush_snapshots(scheduler.now()),
                        Either::Left((Err(error), _)) => {
                            if !cancellation.is_cancelled() {
                                log::error!("The snapshot timer failed: {error}");
                            }
                            return;
                        }
                        // A later deferral: compute the earliest deadline again.
                        Either::Right(_) => {}
                    }
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

    /// Accept and serve HTTP connections until cancelled or accepting fails.
    ///
    /// Transient accept errors, such as a peer that reset before it was accepted or running out of descriptors, are logged and accepting continues, pausing for [`ACCEPT_BACKOFF`] when out of resources. Only cancellation or another accept error, such as a closed scheduler or a broken listener, ends serving. Either way, the listener is closed first, so that new peers are refused rather than left queued with nobody accepting, and then the connections are cancelled and given the drain timeout to finish before they are abandoned.
    ///
    /// TODO(upstream): this follows `base_http_server::serve`, which passes every handler one cancellation shared by all connections. The keepalive sweep must end one silent connection, including a response write blocked on a peer that is gone, so each connection here gets its own child cancellation. `serve` also ends on any accept error, and joins its connections without a time limit before closing its listener. Use `serve` again once it offers per-connection cancellation, keeps accepting after transient accept errors, and closes its listener before a bounded drain.
    async fn serve<S>(
        &self,
        scheduler: &S,
        listener: S::Socket,
        cancellation: &Cancellation,
    ) -> Result<Statistics, Error>
    where
        S: SocketAccept + SocketStreamReading + SocketStreamWriting + SocketClose + Timers,
        S::Socket: 'static,
    {
        let options = base_http_server::Options::default().http1;
        let connections_cancellation = cancellation.child();
        let mut connections = FuturesUnordered::new();
        let mut tracker = Tracker::default();
        let mut timer = scheduler.timer();
        // Accept attempts that failed for want of resources since a connection was last accepted.
        let mut exhausted = 0_u64;
        let result: Result<(), Error> = async {
            loop {
                if connections.len() == MAX_CONNECTIONS {
                    let finished = {
                        let next = pin!(connections.next());
                        match select(next, pin!(cancellation.cancelled())).await {
                            Either::Left((finished, _)) => finished,
                            Either::Right(_) => return Err(Error::Cancelled),
                        }
                    };
                    tracker.record(finished.expect("connections are waiting"));
                    continue;
                }
                // Keep this accept alive while existing connections complete, so an accepted peer is never discarded.
                let accepted = completing(
                    scheduler.socket_accept(&listener, cancellation),
                    &mut connections,
                    &mut tracker,
                )
                .await;
                let (socket, peer) = match accepted {
                    Ok(accepted) => accepted,
                    Err(OperationError::Cancelled(_)) => return Err(Error::Cancelled),
                    Err(OperationError::Io(error)) => match accept_retry(&error) {
                        Some(Retry::Now) => {
                            log::info!("Could not accept a connection, so accepting the next: {error}.");
                            continue;
                        }
                        Some(Retry::Later) => {
                            if exhausted == 0 {
                                log::warn!(
                                    "Could not accept a connection: {error}. Trying again every {ACCEPT_BACKOFF:?} while connections finish."
                                );
                            }
                            exhausted += 1;
                            let deadline = scheduler.now() + ACCEPT_BACKOFF;
                            completing(
                                scheduler.timer_wait_until(&mut timer, deadline, cancellation),
                                &mut connections,
                                &mut tracker,
                            )
                            .await
                            .map_err(|error| failed("The accept back-off timer", error))?;
                            continue;
                        }
                        None => {
                            log::error!("Could not accept connections, so serving ends: {error}.");
                            return Err(Error::Io(error));
                        }
                    },
                };
                if exhausted > 0 {
                    log::info!("Accepting connections again after {exhausted} failed attempts.");
                    exhausted = 0;
                }
                tracker.statistics.connections += 1;
                let serial = tracker.statistics.connections;
                let connection = connections_cancellation.child();
                let options = &options;
                // Each connection is watched so that one which never completes a request is closed, freeing its slot.
                let watch = self.admission.watch(connection.clone());
                tracker.open.insert(
                    serial,
                    Open {
                        peer,
                        watch: watch.clone(),
                    },
                );
                log::debug!("Accepted connection {serial} from {peer}.");
                let handler = Admitted {
                    application: &self.application,
                    watch,
                };
                connections.push(async move {
                    let outcome = base_http_server::serve_connection(
                        scheduler,
                        socket,
                        &handler,
                        options,
                        &connection,
                    )
                    .await;
                    (serial, outcome)
                });
            }
        }
        .await;
        // Refuse new peers at once: a listener left open while connections drain queues them with nobody accepting.
        let closed = scheduler.socket_close(listener).await;
        if let Err(error) = &closed {
            log::warn!("Closing the listener failed: {error}.");
        }
        connections_cancellation.cancel();
        self.drain(scheduler, &mut connections, &mut tracker).await;
        // Every way the loop ends because of cancellation yields `Error::Cancelled`, so a failure is still reported when shutdown begins during the drain.
        match result {
            Err(Error::Cancelled) | Ok(()) => {
                closed.map(|()| tracker.statistics).map_err(Error::from)
            }
            Err(error) => Err(error),
        }
    }

    /// Wait at most the drain timeout for cancelled connections to finish, then abandon the rest, whose sockets close as they are dropped.
    async fn drain<S, C>(
        &self,
        scheduler: &S,
        connections: &mut FuturesUnordered<C>,
        tracker: &mut Tracker,
    ) where
        S: Timers,
        C: Future<Output = Finished>,
    {
        if connections.is_empty() {
            return;
        }
        log::debug!("Waiting for {} connections to finish.", connections.len());
        // The server's cancellation has usually been cancelled already, and must not end this wait.
        let never = Cancellation::never();
        let mut timer = scheduler.timer();
        let deadline = scheduler.now() + self.drain_timeout;
        let mut expiry = pin!(scheduler.timer_wait_until(&mut timer, deadline, &never));
        while !connections.is_empty() {
            match select(expiry.as_mut(), connections.next()).await {
                Either::Left((result, _)) => {
                    if let Err(error) = result {
                        log::warn!("The connection drain timer failed: {error}.");
                    }
                    log::warn!(
                        "Stopped waiting for {} connections that had not finished {:?} after being cancelled.",
                        connections.len(),
                        self.drain_timeout
                    );
                    for (serial, open) in &tracker.open {
                        log::warn!(
                            "Abandoned connection {serial} from {}, {}.",
                            open.peer,
                            open.watch.state()
                        );
                    }
                    return;
                }
                Either::Right((finished, _)) => {
                    tracker.record(finished.expect("connections are waiting"))
                }
            }
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
    async fn supervise<S: Timers>(&self, scheduler: &S, cancellation: &Cancellation) {
        let roots = match &self.roots {
            Some(roots) => roots.clone(),
            None => WorkspaceRoots::current().unwrap_or_else(|error| {
                log::error!("No workspace roots, so every directory is refused: {error}.");
                WorkspaceRoots::none()
            }),
        };
        for root in roots.roots() {
            log::info!("Spaces may run in {} or beneath it.", root.display());
        }
        let resolving = Arc::new(Resolving::default());
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
                    scheduler,
                    self.hub.clone(),
                    request,
                    self.voices.clone(),
                    Places {
                        roots: &roots,
                        resolving: &resolving,
                        slots: &self.slots,
                    },
                    cancellation,
                )),
                Err(_) => break,
            }
        }
        self.hub.shutdown();
        while spaces.next().await.is_some() {}
    }
}

/// A connection's serial number and how it ended.
type Finished = (u64, Result<u64, Error>);

/// The connections being served, and what has been served.
#[derive(Default)]
struct Tracker {
    statistics: Statistics,
    /// Connections that have not finished, by serial number.
    open: BTreeMap<u64, Open>,
}

/// A connection that has not finished.
struct Open {
    peer: SocketAddress,
    watch: Arc<Watch>,
}

impl Tracker {
    /// Count one finished connection.
    fn record(&mut self, (serial, outcome): Finished) {
        let peer = match self.open.remove(&serial) {
            Some(open) => open.peer.to_string(),
            None => "an unknown peer".to_owned(),
        };
        match outcome {
            Ok(requests) => {
                self.statistics.requests += requests;
                log::debug!("Connection {serial} from {peer} finished after {requests} requests.");
            }
            Err(Error::Cancelled) => log::debug!("Connection {serial} from {peer} was cancelled."),
            Err(error) => {
                log::info!(
                    "Connection {serial} from {peer} failed: {}.",
                    describe(&error)
                );
                self.statistics.failed_connections += 1;
            }
        }
    }
}

/// Wait for `future` while recording connections that finish meanwhile, so that they free their descriptors and slots.
async fn completing<F, C>(
    future: F,
    connections: &mut FuturesUnordered<C>,
    tracker: &mut Tracker,
) -> F::Output
where
    F: Future,
    C: Future<Output = Finished>,
{
    let mut future = pin!(future);
    loop {
        let completed = pin!(async {
            if connections.is_empty() {
                pending().await
            } else {
                connections.next().await.expect("connections are waiting")
            }
        });
        match select(future.as_mut(), completed).await {
            Either::Left((output, _)) => return output,
            Either::Right((finished, _)) => tracker.record(finished),
        }
    }
}

/// How to continue after an accept error.
#[derive(Debug, PartialEq)]
enum Retry {
    /// The error concerned one peer only, such as one that reset before it was accepted.
    Now,
    /// The process or system is out of descriptors, buffers or memory, which finishing connections may free.
    Later,
}

/// Whether accepting should continue after `error`, and when. Other errors, such as a closed scheduler or a broken listener, end serving.
fn accept_retry(error: &io::Error) -> Option<Retry> {
    match error.kind() {
        io::ErrorKind::ConnectionAborted
        | io::ErrorKind::ConnectionReset
        | io::ErrorKind::Interrupted => return Some(Retry::Now),
        io::ErrorKind::OutOfMemory => return Some(Retry::Later),
        _ => {}
    }
    #[cfg(unix)]
    match error.raw_os_error() {
        Some(libc::EPROTO) => return Some(Retry::Now),
        Some(libc::EMFILE | libc::ENFILE | libc::ENOBUFS | libc::ENOMEM) => {
            return Some(Retry::Later);
        }
        _ => {}
    }
    None
}

/// The error of `what`, which is logged unless it is cancellation.
fn failed(what: &str, error: OperationError) -> Error {
    match error {
        OperationError::Cancelled(_) => Error::Cancelled,
        OperationError::Io(error) => {
            log::error!("{what} failed, so serving ends: {error}.");
            Error::Io(error)
        }
    }
}

/// An HTTP server error with its causes, which its own message omits for I/O errors.
fn describe(error: &dyn std::error::Error) -> String {
    let mut description = error.to_string();
    let mut source = error.source();
    while let Some(cause) = source {
        let cause_text = cause.to_string();
        // Some errors repeat their cause in their own message.
        if !description.ends_with(&cause_text) {
            description.push_str(": ");
            description.push_str(&cause_text);
        }
        source = cause.source();
    }
    description
}

/// Where spaces may run, and how many.
struct Places<'a> {
    roots: &'a WorkspaceRoots,
    resolving: &'a Arc<Resolving>,
    slots: &'a Arc<SpaceSlots>,
}

/// Start one space's session, then drive it until it closes or the server shuts down.
async fn run_space<S: Timers>(
    scheduler: &S,
    hub: Arc<Hub>,
    mut request: CreateRequest,
    voices: Voices,
    places: Places<'_>,
    cancellation: &Cancellation,
) {
    let Some(backend) = hub.backends.get(&request.backend) else {
        return;
    };
    // Creating and attaching both run the agent in the directory, so both are confined to the workspace roots. The space shows the canonical directory.
    match resolve(scheduler, &places, &request.directory, cancellation).await {
        Some(directory) => request.directory = directory,
        None => {
            hub.create_failed(&request.creator, DIRECTORY_UNAVAILABLE.to_owned());
            return;
        }
    }
    // Only an accepted directory takes a place, which is held until the session has closed or failed to start. Spaces still starting count, so concurrent requests cannot pass the limit together.
    let Some(_slot) = places.slots.acquire() else {
        hub.create_failed(&request.creator, places.slots.full_error());
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

/// The canonical form of a client's directory, checked on its own thread for at most [`RESOLVE_TIMEOUT`] so that a slow file system cannot stall the server, or `None` if it is refused.
async fn resolve<S: Timers>(
    scheduler: &S,
    places: &Places<'_>,
    directory: &str,
    cancellation: &Cancellation,
) -> Option<String> {
    let resolved = places
        .roots
        .resolve_in_background(directory, places.resolving)
        .ok()?;
    let mut timer = scheduler.timer();
    let deadline = scheduler.now() + RESOLVE_TIMEOUT;
    let timeout = scheduler.timer_wait_until(&mut timer, deadline, cancellation);
    match select(pin!(resolved.recv()), pin!(timeout)).await {
        Either::Left((result, _)) => result.ok()?.ok(),
        // The wait fails instead when the server is shutting down.
        Either::Right((Ok(_), _)) => {
            log::warn!(
                "Refused directory {directory:?}: checking it took longer than {RESOLVE_TIMEOUT:?}."
            );
            None
        }
        Either::Right((Err(_), _)) => None,
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

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::io::{Read, Write};
    use std::net::TcpStream;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::mpsc::{Receiver, channel};
    use std::time::Instant;

    use base_scheduler::{
        BufferPool, BufferPoolOptions, BufferPooling, Clock, IoBuf, SocketLocalAddress,
        SocketOperation, SocketStreamReader, SocketStreamWriter,
    };
    use base_scheduler_direct::{DirectOptions, DirectScheduler, DirectSocket};
    use futures_lite::future::block_on;

    use super::*;
    use crate::transcriber::NullTranscriber;

    /// A direct scheduler whose accepts first fail with queued errors, and whose accepted sockets can be made never to finish closing, as a connection that ignores cancellation would.
    struct Faulty {
        inner: DirectScheduler,
        accept_errors: Mutex<VecDeque<io::Error>>,
        accepts: AtomicUsize,
        stall_connection_closes: bool,
    }

    /// A socket, and whether it was accepted rather than listening.
    struct Tagged {
        socket: DirectSocket,
        accepted: bool,
    }

    impl SocketOperation for Faulty {
        type Socket = Tagged;
    }

    impl SocketAccept for Faulty {
        async fn socket_accept(
            &self,
            listener: &Tagged,
            cancellation: &Cancellation,
        ) -> OperationResult<(Tagged, SocketAddress)> {
            self.accepts.fetch_add(1, Ordering::Relaxed);
            let error = self.accept_errors.lock().unwrap().pop_front();
            if let Some(error) = error {
                return Err(error.into());
            }
            let (socket, peer) = self
                .inner
                .socket_accept(&listener.socket, cancellation)
                .await?;
            let socket = Tagged {
                socket,
                accepted: true,
            };
            Ok((socket, peer))
        }
    }

    impl SocketClose for Faulty {
        async fn socket_close(&self, socket: Tagged) -> io::Result<()> {
            if socket.accepted && self.stall_connection_closes {
                let _held = socket;
                return pending().await;
            }
            self.inner.socket_close(socket.socket).await
        }

        async fn socket_close_submit(&self, socket: Tagged) -> io::Result<()> {
            self.inner.socket_close_submit(socket.socket).await
        }
    }

    impl BufferPooling for Faulty {
        type Pool = <DirectScheduler as BufferPooling>::Pool;

        fn buffer_pool(&self, options: BufferPoolOptions) -> io::Result<Self::Pool> {
            self.inner.buffer_pool(options)
        }
    }

    impl SocketStreamReading for Faulty {
        fn socket_stream_reader<'scheduler>(
            &'scheduler self,
            socket: &Tagged,
        ) -> io::Result<
            impl SocketStreamReader<Buffer = <Self::Pool as BufferPool>::Buffer>
            + 'scheduler
            + use<'scheduler>,
        > {
            self.inner.socket_stream_reader(&socket.socket)
        }

        fn socket_stream_reader_with_pool<'scheduler>(
            &'scheduler self,
            socket: &Tagged,
            pool: Self::Pool,
        ) -> io::Result<
            impl SocketStreamReader<Buffer = <Self::Pool as BufferPool>::Buffer>
            + 'scheduler
            + use<'scheduler>,
        > {
            self.inner
                .socket_stream_reader_with_pool(&socket.socket, pool)
        }
    }

    impl SocketStreamWriting for Faulty {
        fn socket_stream_writer<'scheduler>(
            &'scheduler self,
            socket: &Tagged,
        ) -> io::Result<
            impl SocketStreamWriter<Buffer = <Self::Pool as BufferPool>::Buffer>
            + 'scheduler
            + use<'scheduler>,
        > {
            self.inner.socket_stream_writer(&socket.socket)
        }

        fn socket_stream_writer_with_buffer<'scheduler, B: IoBuf>(
            &'scheduler self,
            socket: &Tagged,
        ) -> io::Result<impl SocketStreamWriter<Buffer = B> + 'scheduler + use<'scheduler, B>>
        {
            self.inner
                .socket_stream_writer_with_buffer::<B>(&socket.socket)
        }
    }

    impl Clock for Faulty {
        fn now(&self) -> Instant {
            self.inner.now()
        }

        async fn sleep_until(
            &self,
            deadline: Instant,
            cancellation: &Cancellation,
        ) -> OperationResult<()> {
            self.inner.sleep_until(deadline, cancellation).await
        }
    }

    impl Timers for Faulty {
        type Timer = <DirectScheduler as Timers>::Timer;

        fn timer(&self) -> Self::Timer {
            self.inner.timer()
        }

        async fn timer_wait_until(
            &self,
            timer: &mut Self::Timer,
            deadline: Instant,
            cancellation: &Cancellation,
        ) -> OperationResult<()> {
            self.inner
                .timer_wait_until(timer, deadline, cancellation)
                .await
        }
    }

    /// A server running on its own thread with a [`Faulty`] scheduler.
    struct Running {
        address: SocketAddr,
        cancellation: Cancellation,
        /// Receives what `run` returned, and how many accepts were attempted.
        finished: Receiver<(Result<Statistics, Error>, usize)>,
    }

    fn start(
        accept_errors: Vec<io::Error>,
        stall_connection_closes: bool,
        drain_timeout: Duration,
    ) -> Running {
        let cancellation = Cancellation::new();
        let (address_sender, address) = channel();
        let (sender, finished) = channel();
        let running = cancellation.clone();
        std::thread::spawn(move || {
            let scheduler = Faulty {
                inner: DirectScheduler::new(DirectOptions {
                    threads: 2,
                    capacity: 256,
                })
                .unwrap(),
                accept_errors: Mutex::new(accept_errors.into()),
                accepts: AtomicUsize::new(0),
                stall_connection_closes,
            };
            let mut server = Server::new(Backends::new(), Arc::new(NullTranscriber::new()))
                .with_workspace_roots(WorkspaceRoots::none());
            server.drain_timeout = drain_timeout;
            let result = block_on(async {
                let address = "127.0.0.1:0".parse().unwrap();
                let listener = bind(&scheduler.inner, address, &running).await.unwrap();
                let address = scheduler.inner.socket_local_address(&listener).unwrap();
                address_sender.send(address).unwrap();
                let listener = Tagged {
                    socket: listener,
                    accepted: false,
                };
                server.run(&scheduler, listener, &running).await
            });
            let accepts = scheduler.accepts.load(Ordering::Relaxed);
            block_on(scheduler.inner.shutdown());
            let _ = sender.send((result, accepts));
        });
        Running {
            address: address.recv().unwrap(),
            cancellation,
            finished,
        }
    }

    /// Request `/health` and return the response, reading only as far as the end of its body, so that a socket the server never closes does not matter.
    fn health(address: SocketAddr) -> String {
        let mut stream = TcpStream::connect(address).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        write!(
            stream,
            "GET /health HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n"
        )
        .unwrap();
        let mut response = Vec::new();
        let mut buffer = [0; 1024];
        while !response.ends_with(b"}\n") {
            let count = stream.read(&mut buffer).unwrap();
            assert!(count > 0, "the response ended early");
            response.extend_from_slice(&buffer[..count]);
        }
        String::from_utf8(response).unwrap()
    }

    #[test]
    fn transient_accept_errors_are_retried() {
        for kind in [
            io::ErrorKind::ConnectionAborted,
            io::ErrorKind::ConnectionReset,
            io::ErrorKind::Interrupted,
        ] {
            assert_eq!(accept_retry(&io::Error::from(kind)), Some(Retry::Now));
        }
        assert_eq!(
            accept_retry(&io::Error::from(io::ErrorKind::OutOfMemory)),
            Some(Retry::Later)
        );
        #[cfg(unix)]
        {
            assert_eq!(
                accept_retry(&io::Error::from_raw_os_error(libc::EPROTO)),
                Some(Retry::Now)
            );
            for code in [libc::EMFILE, libc::ENFILE, libc::ENOBUFS, libc::ENOMEM] {
                assert_eq!(
                    accept_retry(&io::Error::from_raw_os_error(code)),
                    Some(Retry::Later)
                );
            }
            assert_eq!(
                accept_retry(&io::Error::from_raw_os_error(libc::EBADF)),
                None
            );
        }
        // A closed scheduler ends serving.
        let closed = io::Error::new(io::ErrorKind::BrokenPipe, "scheduler is closed");
        assert_eq!(accept_retry(&closed), None);
    }

    #[test]
    fn serving_continues_after_transient_accept_errors() {
        #[cfg_attr(not(unix), allow(unused_mut))]
        let mut errors = vec![
            io::Error::from(io::ErrorKind::ConnectionAborted),
            io::Error::from(io::ErrorKind::ConnectionReset),
            io::Error::from(io::ErrorKind::OutOfMemory),
        ];
        #[cfg(unix)]
        errors.extend([
            io::Error::from_raw_os_error(libc::EMFILE),
            io::Error::from_raw_os_error(libc::EPROTO),
            io::Error::from_raw_os_error(libc::ENFILE),
        ]);
        let failures = errors.len();
        let running = start(errors, false, DEFAULT_DRAIN_TIMEOUT);
        assert!(health(running.address).starts_with("HTTP/1.1 200"));
        assert!(health(running.address).starts_with("HTTP/1.1 200"));
        running.cancellation.cancel();
        let (result, accepts) = running
            .finished
            .recv_timeout(Duration::from_secs(10))
            .unwrap();
        let statistics = result.unwrap();
        assert_eq!(statistics.connections, 2);
        assert_eq!(statistics.requests, 2);
        assert!(accepts > failures + 2, "only {accepts} accepts");
    }

    #[test]
    fn a_failure_to_accept_ends_the_server() {
        let broken = io::Error::other("the listener is broken");
        let running = start(vec![broken], false, DEFAULT_DRAIN_TIMEOUT);
        // Nothing cancels the server: the spaces, sweep and pacing end because serving failed.
        let (result, _) = running
            .finished
            .recv_timeout(Duration::from_secs(10))
            .unwrap();
        assert!(matches!(result, Err(Error::Io(_))), "{result:?}");
        assert!(!running.cancellation.is_cancelled());
        assert!(TcpStream::connect(running.address).is_err());
    }

    #[test]
    fn the_listener_closes_before_a_bounded_drain() {
        let drain = Duration::from_secs(1);
        let running = start(Vec::new(), true, drain);
        // This connection is served, then never finishes closing.
        assert!(health(running.address).starts_with("HTTP/1.1 200"));
        let cancelled = Instant::now();
        running.cancellation.cancel();
        // New peers are refused while the connection drains, rather than queued.
        while TcpStream::connect(running.address).is_ok() {
            assert!(cancelled.elapsed() < drain / 2, "the listener stayed open");
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(
            running.finished.try_recv().is_err(),
            "the drain ended early"
        );
        let (result, _) = running
            .finished
            .recv_timeout(Duration::from_secs(10))
            .unwrap();
        let elapsed = cancelled.elapsed();
        assert!(elapsed >= drain, "the drain ended after {elapsed:?}");
        assert!(elapsed < drain * 4, "the drain took {elapsed:?}");
        assert_eq!(result.unwrap().connections, 1);
    }
}
