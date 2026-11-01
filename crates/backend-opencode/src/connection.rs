// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

//! The connection to the OpenCode server, on its own thread.
//!
//! [`base_http_client::Client`] and its [`Driver`] borrow from a scheduler and
//! the driver must be polled alongside every call, so the whole connection
//! lives on one dedicated thread that owns its scheduler. The [`Backend`] and
//! its sessions are `'static` handles that talk to this thread over channels:
//! every session owns an event channel the thread feeds, and commands (prompts,
//! replies, snapshots) travel one way and are answered on a per-command
//! channel. The thread reads the server's one global event stream and fans its
//! events out to the sessions they name.

use std::collections::{HashMap, HashSet};
use std::sync::Mutex;

use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};

use backend::{Event, SessionSpec, SessionSummary, Snapshot, Worker};
use base_http::body::ReadableBody;
use base_http_client::{Client, Endpoint};
use base_scheduler::{
    AddressHints, AddressInfoCollection, AddressInfoView, AddressResolution, AddressService,
    Cancellation, SocketClose, SocketConnect, SocketOpen, SocketSetOption, SocketStreamReading,
    SocketStreamWriting, TcpNoDelay,
};
use base_scheduler_direct::{DirectOptions, DirectScheduler};
use futures_lite::future::block_on;
use serde_json::Value;

use crate::api::{self, Failure};
use crate::endpoint::Resolved;
use crate::wire;

/// The sender side of one session's event channel.
#[derive(Clone)]
pub struct Events {
    tx: async_channel::Sender<Event>,
}

impl Events {
    pub fn new() -> (Self, async_channel::Receiver<Event>) {
        let (tx, rx) = async_channel::bounded(1024);
        (Self { tx }, rx)
    }

    /// Deliver an event, dropping it when the channel is full or the session
    /// has gone: a session that misses events refetches its snapshot.
    pub fn deliver(&self, event: Event) {
        let _ = self.tx.try_send(event);
    }
}

/// A registered session: what the thread needs to serve it. Its id is the map
/// key.
pub struct Registration {
    pub events: Events,
    /// The messages this session has reported as started.
    pub seen: Mutex<HashSet<String>>,
}

impl Registration {
    pub fn new() -> (Self, async_channel::Receiver<Event>) {
        let (events, rx) = Events::new();
        (
            Self {
                events,
                seen: Mutex::new(HashSet::new()),
            },
            rx,
        )
    }
}

/// One command for the thread, carrying the channel to answer it on.
///
/// Every reply channel carries a `backend::Error`, not an `api::Failure`: the
/// thread maps failures to the neutral error before answering.
pub enum Command {
    Sessions {
        directory: String,
        reply: async_channel::Sender<backend::Result<Vec<SessionSummary>>>,
    },
    Create {
        spec: SessionSpec,
        reply: async_channel::Sender<backend::Result<(String, async_channel::Receiver<Event>)>>,
    },
    Attach {
        id: String,
        directory: String,
        reply: async_channel::Sender<backend::Result<async_channel::Receiver<Event>>>,
    },
    Snapshot {
        id: String,
        reply: async_channel::Sender<backend::Result<Snapshot>>,
    },
    Prompt {
        id: String,
        key: String,
        text: String,
        reply: async_channel::Sender<backend::Result<()>>,
    },
    Interrupt {
        id: String,
        reply: async_channel::Sender<backend::Result<()>>,
    },
    Decide {
        id: String,
        request: String,
        decision: String,
        reply: async_channel::Sender<backend::Result<()>>,
    },
    Answer {
        id: String,
        form: String,
        answer: Value,
        reply: async_channel::Sender<backend::Result<()>>,
    },
    /// Close `id`'s session; when it is the last one, stop the connection.
    Close { id: String },
}

/// The commands channel, shared by the backend and every session handle.
#[derive(Clone)]
pub struct Commands {
    tx: async_channel::Sender<Command>,
}

impl Commands {
    pub fn new() -> (Self, async_channel::Receiver<Command>) {
        let (tx, rx) = async_channel::bounded(256);
        (Self { tx }, rx)
    }

    /// The raw sender, for fire-and-forget commands like close.
    pub fn tx(&self) -> &async_channel::Sender<Command> {
        &self.tx
    }

    /// Send a command built against a fresh reply channel, and wait for it.
    /// The reply channel carries the neutral `Result`, so this returns the
    /// command's own outcome, not a double wrap.
    pub async fn reply_with<T: Send + 'static>(
        &self,
        build: impl FnOnce(async_channel::Sender<std::result::Result<T, backend::Error>>) -> Command,
    ) -> std::result::Result<T, backend::Error> {
        let (reply, waiting) = async_channel::bounded(1);
        let command = build(reply);
        self.tx.send(command).await.map_err(|_| {
            backend::Error::new(backend::ErrorKind::Unavailable, "the connection has closed")
        })?;
        waiting.recv().await.map_err(|_| {
            backend::Error::new(backend::ErrorKind::Unavailable, "the connection has closed")
        })?
    }
}

fn map_failure(failure: Failure) -> backend::Error {
    let kind = match failure.status {
        Some(404) => backend::ErrorKind::NotFound,
        Some(status) if (400..500).contains(&status) => backend::ErrorKind::Invalid,
        _ => backend::ErrorKind::Unavailable,
    };
    backend::Error::new(kind, failure.message)
}

/// Run the connection until the last session closes, or the server drops.
/// Blocking: called on a dedicated thread.
pub fn run_thread(resolved: Resolved, commands: async_channel::Receiver<Command>) {
    if let Err(message) = block_on(drive(resolved, commands)) {
        eprintln!("the opencode connection ended: {message}");
    }
}

async fn drive(
    resolved: Resolved,
    commands: async_channel::Receiver<Command>,
) -> std::result::Result<(), String> {
    let scheduler = DirectScheduler::new(DirectOptions {
        threads: 2,
        capacity: 256,
    })
    .map_err(|error| format!("could not start the connection scheduler: {error}"))?;
    let result = run(&scheduler, resolved, commands).await;
    block_on(scheduler.shutdown());
    result
}

async fn run<'scope, S: Scheduler<'scope> + Send + Sync + 'static>(
    scheduler: &S,
    resolved: Resolved,
    commands: async_channel::Receiver<Command>,
) -> std::result::Result<(), String>
where
    <S as Scheduler<'scope>>::Socket: 'static,
{
    let cancellation = Cancellation::new();
    let address = scheduler
        .address_resolve(
            Some(resolved.host.trim_matches(['[', ']'])),
            AddressService::Port(resolved.port),
            AddressHints::default(),
            &cancellation,
        )
        .await
        .map_err(|error| format!("resolving {}: {error}", resolved.authority()))?
        .entries()
        .next()
        .map(|entry| entry.address())
        .ok_or_else(|| "the server's address did not resolve to any endpoint".to_owned())?;

    let endpoint = Endpoint::new(address);
    let (client, driver) = Client::new(scheduler, endpoint, base_http_client::Options::default())
        .map_err(|error| format!("creating the HTTP client: {error}"))?;

    // The driver must be polled for as long as the client is used, so it runs
    // alongside the connection loop in the same future; finishing either ends
    // both. The cancellation is shared so closing the connection unwinds both.
    let (driver_result, _) = futures_lite::future::zip(
        driver.run(&cancellation),
        service(&client, resolved, commands, &cancellation),
    )
    .await;
    driver_result.map_err(|error| error.to_string())
}

/// The scheduler capabilities the connection needs.
trait Scheduler<'scope>:
    AddressResolution
    + SocketOpen
    + SocketConnect
    + SocketClose
    + SocketSetOption<TcpNoDelay>
    + SocketStreamReading
    + SocketStreamWriting
    + 'scope
{
    type Socket: 'static;
}

impl<'scope, S> Scheduler<'scope> for S
where
    S: AddressResolution
        + SocketOpen
        + SocketConnect
        + SocketClose
        + SocketSetOption<TcpNoDelay>
        + SocketStreamReading
        + SocketStreamWriting
        + 'scope,
    S::Socket: 'static,
{
    type Socket = S::Socket;
}

/// Wraps a future in a `FusedFuture`, pinning its inner future, since
/// `select!` requires every branch to implement it and the channel and body
/// futures do not.
struct Fused<F: Future> {
    inner: Pin<Box<F>>,
    done: bool,
}

impl<F: Future> Fused<F> {
    fn new(inner: F) -> Self {
        Self {
            inner: Box::pin(inner),
            done: false,
        }
    }
}

impl<F: Future> Future for Fused<F> {
    type Output = F::Output;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        if self.done {
            unreachable!("a fused future was polled after finishing");
        }
        let output = self.inner.as_mut().poll(cx);
        if output.is_ready() {
            self.done = true;
        }
        output
    }
}

impl<F: Future> futures_core::FusedFuture for Fused<F> {
    fn is_terminated(&self) -> bool {
        self.done
    }
}

/// Read the server's global event stream and service commands, alternating
/// reads over the stream and command channel reads, until the last session
/// closes or the stream ends.
async fn service(
    client: &Client<'_>,
    resolved: Resolved,
    commands: async_channel::Receiver<Command>,
    cancellation: &Cancellation,
) {
    let mut request = base_http::Request::get(base_http::Path::from("/api/event".to_owned()));
    if let Some(authorization) = resolved.authorization_header()
        && let Ok(field) = base_http::Field::new("authorization", authorization)
    {
        let _ = request.fields.append(field);
    }
    let mut stream = match client.call(request, cancellation).await {
        Ok(response) => response.body,
        Err(failure) => {
            eprintln!("opening the event stream: {failure}");
            return;
        }
    };

    let mut registrations = HashMap::<String, Registration>::new();
    let mut pending = String::new();

    loop {
        futures_util::select_biased! {
            command = Fused::new(commands.recv()) => {
                let Ok(command) = command else {
                    break;
                };
                if !handle_command(command, client, &resolved, &mut registrations, cancellation).await {
                    break;
                }
            }
            result = Fused::new(stream.read(cancellation)) => {
                let frame = match result {
                    Ok(frame) => frame,
                    Err(error) => {
                        eprintln!("the opencode event stream failed: {error}; closing the connection");
                        break;
                    },
                };
                let end = frame.is_end();
                if let base_http::body::BodyFrame::Data { data, .. } = frame {
                    pending.push_str(&String::from_utf8_lossy(&data));
                    while let Some(index) = pending.find("\n\n") {
                        let group = pending[..index + 2].to_owned();
                        pending = pending[index + 2..].to_owned();
                        if let Some(payload) = sse_payload(&group) {
                            dispatch_events(
                                &payload,
                                &mut registrations,
                                client,
                                &resolved,
                                cancellation,
                            )
                            .await;
                        }
                    }
                }
                if end {
                    eprintln!("the opencode event stream ended; closing the connection");
                    break;
                }
            }
        }
    }

    // Deliver Closed to every session still bound, so no session waits forever
    // for its event channel to fill.
    for registration in registrations.values() {
        registration.events.deliver(Event::Closed {
            reason: Some("the connection ended".to_owned()),
        });
    }
}

/// The JSON payload of one SSE group, `None` for keep-alive and comment groups.
fn sse_payload(group: &str) -> Option<String> {
    let mut data = Vec::new();
    for line in group.lines() {
        if let Some(payload) = line.strip_prefix("data:") {
            data.push(payload.trim_start());
        }
    }
    if data.is_empty() {
        None
    } else {
        Some(data.join("\n"))
    }
}

/// Register `id` in the map, returning its receiver.
fn insert_registration(
    registrations: &mut HashMap<String, Registration>,
    id: String,
) -> async_channel::Receiver<Event> {
    let (registration, rx) = Registration::new();
    registrations.insert(id, registration);
    rx
}

/// Feed one complete SSE event to every session it names; refresh the worker
/// list of sessions whose children changed.
async fn dispatch_events(
    event: &str,
    registrations: &mut HashMap<String, Registration>,
    client: &Client<'_>,
    resolved: &Resolved,
    cancellation: &Cancellation,
) {
    let Ok(value) = serde_json::from_str::<Value>(event) else {
        return;
    };
    let Some(session_id) = wire::session_id_of(&value) else {
        return;
    };
    let refresh = registrations.get(&session_id).is_some()
        && matches!(
            value.get("type").and_then(Value::as_str),
            Some("session.created" | "session.deleted" | "session.forked")
        );
    let Some(registration) = registrations.get_mut(&session_id) else {
        return;
    };
    // Deliver into the registration behind its own scoped borrow so it ends
    // before `refresh_workers` re-borrows the map.
    {
        let event = match wire::event_for(
            &value,
            &session_id,
            &mut registration
                .seen
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
        ) {
            Some(event) => event,
            None => return,
        };
        registration.events.deliver(event);
    }
    if refresh {
        refresh_workers(client, resolved, registrations, &session_id, cancellation).await;
    }
}

/// Re-fetch a session's children after one of them changed, and report them.
async fn refresh_workers(
    client: &Client<'_>,
    resolved: &Resolved,
    registrations: &HashMap<String, Registration>,
    id: &str,
    cancellation: &Cancellation,
) {
    let Some(registration) = registrations.get(id) else {
        return;
    };
    match api::child_sessions(client, resolved, id, cancellation).await {
        Ok(children) => {
            let workers = children
                .iter()
                .filter_map(wire::worker_from_session)
                .collect::<Vec<Worker>>();
            registration
                .events
                .deliver(Event::WorkersChanged { workers });
        }
        Err(failure) => {
            eprintln!("refreshing the workers of {id}: {}", failure.message);
        }
    }
}

/// Service one command, returning whether the connection should stay open.
async fn handle_command(
    command: Command,
    client: &Client<'_>,
    resolved: &Resolved,
    registrations: &mut HashMap<String, Registration>,
    cancellation: &Cancellation,
) -> bool {
    match command {
        Command::Close { id } => {
            if let Some(registration) = registrations.remove(&id) {
                registration.events.deliver(Event::Closed { reason: None });
            }
            registrations.is_empty()
        }
        Command::Sessions { directory, reply } => {
            let result = api::list_sessions(client, resolved, &directory, cancellation)
                .await
                .map(|sessions| {
                    sessions
                        .iter()
                        .filter_map(|info| {
                            Some(SessionSummary {
                                id: info.get("id")?.as_str()?.to_owned(),
                                title: info
                                    .get("title")
                                    .and_then(Value::as_str)
                                    .unwrap_or_default()
                                    .to_owned(),
                                directory: info
                                    .get("location")
                                    .and_then(|location| location.get("directory"))
                                    .and_then(Value::as_str)
                                    .unwrap_or_default()
                                    .to_owned(),
                            })
                        })
                        .collect()
                })
                .map_err(map_failure);
            reply.send(result).await.ok();
            true
        }
        Command::Create { spec, reply } => {
            let result = api::create_session(
                client,
                resolved,
                &spec.directory,
                Some(&spec.title),
                cancellation,
            )
            .await
            .and_then(|info| {
                let id = info
                    .get("id")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
                    .ok_or_else(|| Failure {
                        status: None,
                        message: "the server did not return the new session's id".to_owned(),
                    })?;
                let rx = insert_registration(registrations, id.clone());
                Ok((id, rx))
            })
            .map_err(map_failure);
            reply.send(result).await.ok();
            true
        }
        Command::Attach {
            id,
            directory,
            reply,
        } => {
            let result = api::session_info(client, resolved, &id, cancellation)
                .await
                .and_then(|info| {
                    let session_directory = info
                        .get("location")
                        .and_then(|location| location.get("directory"))
                        .and_then(Value::as_str)
                        .unwrap_or_default();
                    if session_directory != directory {
                        return Err(Failure {
                            status: Some(404),
                            message: "the session is not in that directory".to_owned(),
                        });
                    }
                    Ok(())
                })
                .map(|()| insert_registration(registrations, id.clone()))
                .map_err(map_failure);
            reply.send(result).await.ok();
            true
        }
        Command::Snapshot { id, reply } => {
            let result = snapshot(client, resolved, &id, cancellation)
                .await
                .map_err(map_failure);
            reply.send(result).await.ok();
            true
        }
        Command::Prompt {
            id,
            key,
            text,
            reply,
        } => {
            let result = api::send_prompt(client, resolved, &id, &key, &text, cancellation)
                .await
                .map_err(map_failure);
            reply.send(result).await.ok();
            true
        }
        Command::Interrupt { id, reply } => {
            let result = api::interrupt(client, resolved, &id, cancellation)
                .await
                .map_err(map_failure);
            reply.send(result).await.ok();
            true
        }
        Command::Decide {
            id,
            request,
            decision,
            reply,
        } => {
            let result =
                api::reply_permission(client, resolved, &id, &request, &decision, cancellation)
                    .await
                    .map_err(map_failure);
            reply.send(result).await.ok();
            true
        }
        Command::Answer {
            id,
            form,
            answer,
            reply,
        } => {
            let result = api::reply_form(client, resolved, &id, &form, &answer, cancellation)
                .await
                .map_err(map_failure);
            reply.send(result).await.ok();
            true
        }
    }
}

async fn snapshot(
    client: &Client<'_>,
    resolved: &Resolved,
    id: &str,
    cancellation: &Cancellation,
) -> std::result::Result<Snapshot, Failure> {
    let parts = api::SnapshotParts::fetch(client, resolved, id, cancellation).await?;
    let messages = snapshot_messages(&parts.messages);
    Ok(Snapshot {
        busy: None,
        messages,
        permission: parts.permission(),
        question: parts.question(),
        workers: parts.workers(),
    })
}

/// The session's message list as snapshot messages.
fn snapshot_messages(value: &Value) -> Vec<backend::SnapshotMessage> {
    let Some(messages) = value
        .get("data")
        .and_then(Value::as_array)
        .or_else(|| value.as_array())
    else {
        return Vec::new();
    };
    messages.iter().filter_map(snapshot_message).collect()
}

fn snapshot_message(value: &Value) -> Option<backend::SnapshotMessage> {
    let id = value.get("id")?.as_str()?.to_owned();
    let role = match value.get("type")?.as_str()? {
        "user" => backend::Role::User,
        "assistant" => backend::Role::Assistant,
        _ => return None,
    };
    let parts = match role {
        backend::Role::User => vec![backend::Part::Text {
            text: value
                .get("text")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
        }],
        backend::Role::Assistant => value
            .get("content")
            .and_then(Value::as_array)
            .map(|content| {
                content
                    .iter()
                    .filter_map(|part| match part.get("type")?.as_str()? {
                        "text" => Some(backend::Part::Text {
                            text: part
                                .get("text")
                                .and_then(Value::as_str)
                                .unwrap_or_default()
                                .to_owned(),
                        }),
                        "reasoning" => Some(backend::Part::Reasoning {
                            text: part
                                .get("text")
                                .and_then(Value::as_str)
                                .unwrap_or_default()
                                .to_owned(),
                        }),
                        "tool" => Some(backend::Part::Tool {
                            id: part
                                .get("id")
                                .and_then(Value::as_str)
                                .unwrap_or_default()
                                .to_owned(),
                            tool: part
                                .get("name")
                                .and_then(Value::as_str)
                                .unwrap_or_default()
                                .to_owned(),
                            input: part
                                .get("state")
                                .and_then(|state| state.get("input"))
                                .cloned()
                                .unwrap_or(Value::Null),
                            finished: matches!(
                                part.get("state")
                                    .and_then(|state| state.get("status"))
                                    .and_then(Value::as_str),
                                Some("completed") | Some("error")
                            ),
                        }),
                        _ => None,
                    })
                    .collect::<Vec<backend::Part>>()
            })
            .unwrap_or_default(),
    };
    let completed = match role {
        backend::Role::User => true,
        backend::Role::Assistant => value
            .get("time")
            .and_then(|time| time.get("completed"))
            .is_some(),
    };
    Some(backend::SnapshotMessage {
        id,
        role,
        parts,
        completed,
    })
}
