// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

//! Shared server state: participants, connections, spaces and open utterances, behind one lock.
//!
//! Every client action and backend event is applied synchronously under the lock. Work that may call out of the server — transcriber calls and backend operations — is deferred until the lock is released, or sent to the space's driver task.

use std::cell::Cell;
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, RwLock, Weak};
use std::time::{Duration, Instant};

use backend::{AnswerValue, Backend, ErrorKind, Event, Prompt, Snapshot};
use base_scheduler::Cancellation;
use event_listener::{Event as Notify, EventListener};
use protocol::{
    AudioFrame, ClientMessage, Frame, FrameKind, Gate, Keepalive, PROTOCOL_VERSION, Participant,
    ServerMessage, WorkspaceRoot,
};

use crate::backends::Backends;
use crate::limits;
use crate::outbox::{Outbox, Outgoing};
use crate::space::{AnswerOutcome, SpaceModel};
use crate::transcriber::{Transcriber, TranscriptSink, UtteranceId};
use crate::workspace::SpaceSlots;

pub type ConnectionId = u64;

/// The shortest time between snapshots of one space caused by agent events, so that a streaming reply sends about ten full snapshots a second however fast its text arrives. See `design/server.md`.
pub const SNAPSHOT_INTERVAL: Duration = Duration::from_millis(100);

/// The most closed spaces kept that nobody is in. Beyond this, the oldest are forgotten, so that sessions ending repeatedly cannot grow the server's memory without bound. See `design/spaces.md`.
pub const CLOSED_SPACES: usize = 32;

/// Work for a space's driver, which owns the backend session.
#[derive(Debug)]
pub enum SpaceCommand {
    Send(Prompt),
    Decide {
        request: String,
        decision: backend::Decision,
        participant: String,
    },
    Answer {
        question: String,
        answers: Vec<(String, AnswerValue)>,
        participant: String,
    },
    Close,
}

/// A request to start a space, handled by the supervisor because starting a session is asynchronous.
#[derive(Debug)]
pub struct CreateRequest {
    pub title: String,
    pub backend: String,
    pub directory: String,
    pub session: Option<String>,
    /// The participant who asked; they join the space once it exists, unless they have joined another space meanwhile.
    pub creator: String,
    /// The space the creator was in when they asked.
    pub creator_space: Option<String>,
}

/// A task for the supervisor, which runs outside the hub lock so it can call backends.
#[derive(Debug)]
pub enum Job {
    Create(CreateRequest),
    /// List the existing sessions of `backend` in `directory` for `participant`.
    ListSessions {
        participant: String,
        backend: String,
        directory: String,
    },
}

pub struct Hub {
    state: Mutex<HubState>,
    pub(crate) backends: Backends,
    transcriber: Arc<dyn Transcriber>,
    jobs: async_channel::Sender<Job>,
    /// Spaces whose sessions are starting or running, against the limit.
    slots: Arc<SpaceSlots>,
    /// The directories a space may run in, sent to clients in `Welcome` so they can choose one without typing a path.
    roots: RwLock<Vec<WorkspaceRoot>>,
    /// Notified when a space defers a snapshot, so that the server's snapshot timer can wake for it.
    deferred_snapshot: Notify,
    this: Weak<Hub>,
}

#[derive(Default)]
struct HubState {
    next_connection: ConnectionId,
    next_source: u32,
    /// Random for each server run and part of every space identifier, so a client restoring a space from an earlier run never joins an unrelated space that reused its number.
    run: String,
    next_space: u64,
    next_utterance: u64,
    next_ping: u64,
    /// Orders closed spaces by when they closed; see [`SpaceRecord::closed`].
    next_closed: u64,
    connections: HashMap<ConnectionId, ConnectionRecord>,
    participants: HashMap<String, ParticipantRecord>,
    /// In creation order.
    spaces: Vec<SpaceRecord>,
    utterances: HashMap<UtteranceId, UtteranceRecord>,
    /// Set once shutdown has begun: no further spaces are registered.
    shutting_down: bool,
}

struct ConnectionRecord {
    outbox: Arc<Outbox>,
    participant: Option<String>,
    utterance: Option<OpenUtterance>,
    /// The client's id for the utterance the server last closed by failing it, so that an end or cancel already on its way is ignored rather than refused.
    failed: Option<String>,
    /// Push-to-talk frames that arrived while no utterance was open, which are relayed but not transcribed.
    stray_frames: u64,
    /// Whether the last push-to-talk frame was stray, so that each run of them is logged once.
    straying: bool,
    liveness: Liveness,
}

/// Whether the HTTP server is writing a connection's output, shared by the connection body and the keepalive sweep.
#[derive(Default)]
pub struct Output {
    /// True from when the body hands out a chunk until the HTTP server asks for the next one, which it does once the chunk is written.
    pending: AtomicBool,
    /// Chunks written so far.
    written: AtomicU64,
}

impl Output {
    /// The body handed out a chunk to write.
    pub fn handed_out(&self) {
        self.pending.store(true, Ordering::Release);
    }

    /// The HTTP server asked for more, so the previous chunk, if any, was written.
    pub fn asked(&self) {
        if self.pending.swap(false, Ordering::AcqRel) {
            self.written.fetch_add(1, Ordering::AcqRel);
        }
    }
}

/// What the keepalive sweep knows about one connection.
struct Liveness {
    /// Set by the connection whenever data arrives, and cleared by the sweep.
    received: Arc<AtomicBool>,
    /// When the sweep last saw data arrive, or first saw the connection.
    last: Option<Instant>,
    /// When the sweep first saw the connection, until it says Hello.
    opened: Option<Instant>,
    greeted: bool,
    pinged: Option<Instant>,
    output: Arc<Output>,
    /// Chunks written when the sweep last looked.
    written: u64,
    /// When the sweep first saw the current write pending without progress.
    stalled: Option<Instant>,
    /// Ends this connection's HTTP exchange, including a write blocked on a peer that is gone.
    cancellation: Cancellation,
}

struct OpenUtterance {
    client: String,
    id: UtteranceId,
    /// The sequence number of the next frame, after an agent frame of this utterance. A gap between two agent frames means agent audio was lost mid-utterance; see `design/speech.md`. A frame of another gate clears it, so the agent frame after it is not checked.
    ///
    /// The first frame is not checked against frames before `UtteranceBegin`: the client legitimately skips sequence numbers there, by not sending frames while the gate is off and by dropping congested live audio. It never drops frames of an open utterance, and frames and control share one ordered stream, so the first frame after `UtteranceBegin` is the utterance's first.
    expected: Option<u32>,
}

struct ParticipantRecord {
    name: String,
    source: u32,
    connection: Option<ConnectionId>,
    space: Option<String>,
    /// The space to rejoin when this participant reconnects.
    last_space: Option<String>,
    /// When the participant's last connection ended, while it has none.
    departed: Option<Instant>,
}

struct SpaceRecord {
    model: SpaceModel,
    commands: async_channel::Sender<SpaceCommand>,
    /// When participants were last told that the space changed. Cells, because telling them only needs shared access to the state.
    touched: Cell<Option<Instant>>,
    /// A change caused by agent events that participants have not been told about yet; see [`SNAPSHOT_INTERVAL`].
    deferred: Cell<bool>,
    /// Once the space has closed, when relative to other spaces, so that the least recently closed are forgotten first.
    closed: Option<u64>,
}

/// An utterance whose transcript has not been reported yet.
struct UtteranceRecord {
    participant: String,
    client: String,
    space: String,
}

/// A transcriber call to make once the lock is released.
enum Deferred {
    Begin(UtteranceId),
    Push(UtteranceId, AudioFrame),
    End(UtteranceId),
    Cancel(UtteranceId),
}

/// A token for this server run, so space identifiers are not reused after a restart: 64 random bits as hexadecimal. `RandomState` is seeded from the operating system's random source.
fn run_token() -> String {
    use std::hash::{BuildHasher, Hasher};
    let mut hasher = std::collections::hash_map::RandomState::new().build_hasher();
    hasher.write_u128(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos(),
    );
    format!("{:016x}", hasher.finish())
}

/// An error reply, cut to [`limits::ERROR_BYTES`].
fn error_message(error: impl Into<String>) -> ServerMessage {
    ServerMessage::Error {
        error: limits::truncate(&error.into(), limits::ERROR_BYTES),
        space: None,
    }
}

impl HubState {
    fn outbox(&self, participant: &str) -> Option<&Arc<Outbox>> {
        let connection = self.participants.get(participant)?.connection?;
        Some(&self.connections.get(&connection)?.outbox)
    }

    fn send(&self, participant: &str, message: ServerMessage) {
        if let Some(outbox) = self.outbox(participant) {
            outbox.message(message);
        }
    }

    fn error(&self, participant: &str, error: impl Into<String>) {
        self.send(participant, error_message(error));
    }

    fn space(&self, id: &str) -> Option<&SpaceRecord> {
        self.spaces.iter().find(|space| space.model.id == id)
    }

    fn space_mut(&mut self, id: &str) -> Option<&mut SpaceRecord> {
        self.spaces.iter_mut().find(|space| space.model.id == id)
    }

    /// Tell everyone in the space, and the lobby, that the space changed.
    fn touch_space(&self, id: &str) {
        if let Some(space) = self.space(id) {
            space.touched.set(Some(Instant::now()));
            space.deferred.set(false);
            for participant in &space.model.participants {
                if let Some(outbox) = self.outbox(participant) {
                    outbox.space();
                }
            }
        }
        self.touch_lobby();
    }

    /// Tell the space's participants that it changed because of an agent event, at most once per [`SNAPSHOT_INTERVAL`]. A change within the interval is deferred to its end, when the snapshot rendered then carries the latest state; returns true if so.
    fn touch_space_paced(&self, id: &str, now: Instant) -> bool {
        let Some(space) = self.space(id) else {
            return false;
        };
        if space.deferred.get() {
            return false;
        }
        if space
            .touched
            .get()
            .is_some_and(|touched| now < touched + SNAPSHOT_INTERVAL)
        {
            space.deferred.set(true);
            return true;
        }
        self.touch_space(id);
        false
    }

    /// Record when spaces closed since this was last called.
    fn stamp_closed(&mut self) {
        for space in &mut self.spaces {
            if space.closed.is_none() && space.model.is_closed() {
                space.closed = Some(self.next_closed);
                self.next_closed += 1;
            }
        }
    }

    /// Forget the least recently closed spaces that nobody is in, beyond [`CLOSED_SPACES`]. Returns whether any were forgotten.
    fn forget_closed(&mut self) -> bool {
        self.stamp_closed();
        let occupied: HashSet<&str> = self
            .participants
            .values()
            .filter_map(|record| record.space.as_deref())
            .collect();
        let mut forgettable: Vec<(u64, String)> = self
            .spaces
            .iter()
            .filter(|space| {
                space.model.participants.is_empty() && !occupied.contains(space.model.id.as_str())
            })
            .filter_map(|space| Some((space.closed?, space.model.id.clone())))
            .collect();
        forgettable.sort_unstable();
        let Some(excess) = forgettable.len().checked_sub(CLOSED_SPACES) else {
            return false;
        };
        let forgotten: HashSet<&String> = forgettable[..excess].iter().map(|(_, id)| id).collect();
        self.spaces
            .retain(|space| !forgotten.contains(&space.model.id));
        excess > 0
    }

    /// Tell every greeted participant outside a space that the listing changed.
    fn touch_lobby(&self) {
        for record in self.participants.values() {
            if record.space.is_none()
                && let Some(connection) = record.connection.and_then(|id| self.connections.get(&id))
            {
                connection.outbox.spaces();
            }
        }
    }

    /// Hand the next queued input to the driver, if the space can accept it.
    fn pump(&mut self, id: &str) {
        let Some(space) = self.space_mut(id) else {
            return;
        };
        if let Some(prompt) = space.model.next_prompt() {
            let key = prompt.key.clone();
            if space.commands.try_send(SpaceCommand::Send(prompt)).is_err() {
                space
                    .model
                    .sent(&key, Err("the agent session has stopped".to_owned()));
            }
        }
    }

    fn participants(&self, space: &SpaceModel) -> Vec<Participant> {
        space
            .participants
            .iter()
            .filter_map(|id| {
                let record = self.participants.get(id)?;
                Some(Participant {
                    id: id.clone(),
                    name: record.name.clone(),
                    source: record.source,
                })
            })
            .collect()
    }

    /// Take a participant out of their space. A closed space left empty may be forgotten; see [`CLOSED_SPACES`].
    fn leave(&mut self, participant: &str) {
        self.depart(participant);
        self.forget_closed();
    }

    fn depart(&mut self, participant: &str) {
        let Some(record) = self.participants.get_mut(participant) else {
            return;
        };
        let Some(space) = record.space.take() else {
            return;
        };
        if let Some(record) = self.space_mut(&space) {
            record.model.participants.retain(|id| id != participant);
        }
        self.touch_space(&space);
    }

    fn join(&mut self, participant: &str, space: &str) {
        // Not forgotten until the participant is in the space they join, which might be a closed one.
        self.depart(participant);
        let Some(record) = self.space_mut(space) else {
            return;
        };
        if !record.model.participants.iter().any(|id| id == participant) {
            record.model.participants.push(participant.to_owned());
        }
        if let Some(record) = self.participants.get_mut(participant) {
            record.space = Some(space.to_owned());
            record.last_space = Some(space.to_owned());
        }
        if let Some(outbox) = self.outbox(participant) {
            outbox.clear_audio();
        }
        self.forget_closed();
        self.touch_space(space);
    }

    /// The greeted participant of a connection and their current space.
    fn member(&self, connection: ConnectionId) -> Option<(String, Option<String>)> {
        let participant = self.connections.get(&connection)?.participant.clone()?;
        let space = self.participants.get(&participant)?.space.clone();
        Some((participant, space))
    }

    /// Close the open utterance of a connection, if any.
    fn abandon_utterance(&mut self, connection: ConnectionId, deferred: &mut Vec<Deferred>) {
        if let Some(open) = self
            .connections
            .get_mut(&connection)
            .and_then(|record| record.utterance.take())
        {
            self.utterances.remove(&open.id);
            deferred.push(Deferred::Cancel(open.id));
        }
    }
}

impl Hub {
    pub fn new(
        backends: Backends,
        transcriber: Arc<dyn Transcriber>,
        jobs: async_channel::Sender<Job>,
        roots: Vec<WorkspaceRoot>,
        slots: Arc<SpaceSlots>,
    ) -> Arc<Self> {
        Arc::new_cyclic(|this| Self {
            state: Mutex::new(HubState {
                run: run_token(),
                ..HubState::default()
            }),
            backends,
            transcriber,
            jobs,
            slots,
            roots: RwLock::new(roots),
            deferred_snapshot: Notify::new(),
            this: this.clone(),
        })
    }

    /// Set the workspace roots a client can choose from, as in [`crate::Server::with_workspace_roots`].
    pub(crate) fn set_roots(&self, roots: Vec<WorkspaceRoot>) {
        *self
            .roots
            .write()
            .expect("workspace roots are never left poisoned") = roots;
    }

    fn lock(&self) -> MutexGuard<'_, HubState> {
        self.state.lock().expect("hub state is never left poisoned")
    }

    fn run(&self, deferred: Vec<Deferred>) {
        for action in deferred {
            match action {
                Deferred::Begin(id) => self.transcriber.begin(
                    id,
                    TranscriptSink {
                        hub: self.this.clone(),
                        utterance: id,
                    },
                ),
                Deferred::Push(id, frame) => self.transcriber.push(id, &frame),
                Deferred::End(id) => self.transcriber.end(id),
                Deferred::Cancel(id) => self.transcriber.cancel(id),
            }
        }
    }

    /// Register a connection. `cancellation` must end only this connection's exchange: the keepalive sweep cancels it when the connection goes silent. The connection sets the returned flag whenever data arrives, and reports its writes to the returned [`Output`].
    pub fn connect(
        &self,
        cancellation: Cancellation,
    ) -> (ConnectionId, Arc<Outbox>, Arc<AtomicBool>, Arc<Output>) {
        let mut state = self.lock();
        state.next_connection += 1;
        let id = state.next_connection;
        let outbox = Arc::new(Outbox::new());
        let received = Arc::new(AtomicBool::new(false));
        let output = Arc::new(Output::default());
        state.connections.insert(
            id,
            ConnectionRecord {
                outbox: outbox.clone(),
                participant: None,
                utterance: None,
                failed: None,
                stray_frames: 0,
                straying: false,
                liveness: Liveness {
                    received: received.clone(),
                    last: None,
                    opened: None,
                    greeted: false,
                    pinged: None,
                    output: output.clone(),
                    written: 0,
                    stalled: None,
                    cancellation,
                },
            },
        );
        (id, outbox, received, output)
    }

    /// Ping connections that have been quiet for the keepalive interval, and release those that have received nothing for the keepalive timeout as if they had disconnected. Release, in the same way, connections whose current write has made no progress for the keepalive timeout: a peer that does not read can receive neither audio nor state. Also release connections that have not said Hello within `hello_timeout`, so that a peer cannot hold a connection slot by keeping a connection alive without joining. Called periodically with the current time; a connection's quiet time is measured from the first sweep that sees it or the last that saw data arrive.
    pub fn sweep(&self, now: Instant, keepalive: &Keepalive, hello_timeout: Duration) {
        let mut expired = Vec::new();
        {
            let mut state = self.lock();
            let HubState {
                connections,
                next_ping,
                ..
            } = &mut *state;
            for (&id, record) in connections.iter_mut() {
                let liveness = &mut record.liveness;
                if liveness.received.swap(false, Ordering::AcqRel) || liveness.last.is_none() {
                    liveness.last = Some(now);
                }
                let quiet = now.saturating_duration_since(liveness.last.unwrap_or(now));
                let opened = *liveness.opened.get_or_insert(now);
                let written = liveness.output.written.load(Ordering::Acquire);
                if liveness.output.pending.load(Ordering::Acquire) && written == liveness.written {
                    liveness.stalled.get_or_insert(now);
                } else {
                    liveness.stalled = None;
                }
                liveness.written = written;
                let stalled = liveness
                    .stalled
                    .is_some_and(|since| now.saturating_duration_since(since) >= keepalive.timeout);
                if !liveness.greeted && now.saturating_duration_since(opened) >= hello_timeout {
                    log::info!("Connection {id} did not say Hello in time.");
                    expired.push((id, liveness.cancellation.clone()));
                } else if quiet >= keepalive.timeout {
                    log::info!("Connection {id} received nothing within the keepalive timeout.");
                    expired.push((id, liveness.cancellation.clone()));
                } else if stalled {
                    log::info!("Connection {id} accepted no output within the keepalive timeout.");
                    expired.push((id, liveness.cancellation.clone()));
                } else if quiet >= keepalive.interval
                    && liveness.pinged.is_none_or(|pinged| {
                        now.saturating_duration_since(pinged) >= keepalive.interval
                    })
                {
                    *next_ping += 1;
                    record
                        .outbox
                        .keepalive(ServerMessage::Ping { nonce: *next_ping });
                    liveness.pinged = Some(now);
                }
            }
        }
        for (id, cancellation) in expired {
            // Release the participant now; cancelling also ends the exchange, even one blocked writing to a peer that is gone.
            self.disconnect(id);
            cancellation.cancel();
        }
    }

    /// Forget participants whose last connection ended at least `retention` before `now`. Until then, a participant who reconnects keeps their audio source and rejoins their space; afterwards they are a new participant. See `design/spaces.md`.
    pub fn forget_departed(&self, now: Instant, retention: Duration) {
        let mut state = self.lock();
        let before = state.participants.len();
        state.participants.retain(|_, record| {
            record.connection.is_some()
                || record
                    .departed
                    .is_none_or(|departed| now.saturating_duration_since(departed) < retention)
        });
        let forgotten = before - state.participants.len();
        if forgotten > 0 {
            log::info!("Forgot {forgotten} participants who left more than {retention:?} ago.");
        }
    }

    pub fn disconnect(&self, connection: ConnectionId) {
        let mut deferred = Vec::new();
        {
            let mut state = self.lock();
            state.abandon_utterance(connection, &mut deferred);
            if let Some(record) = state.connections.remove(&connection)
                && let Some(participant) = record.participant
            {
                if record.stray_frames > 0 {
                    log::info!(
                        "{participant} sent {} push-to-talk frames outside any utterance on this connection.",
                        record.stray_frames
                    );
                }
                let current = state
                    .participants
                    .get(&participant)
                    .is_some_and(|record| record.connection == Some(connection));
                if current {
                    state.leave(&participant);
                    if let Some(record) = state.participants.get_mut(&participant) {
                        record.connection = None;
                        record.departed = Some(Instant::now());
                    }
                }
            }
        }
        self.run(deferred);
    }

    /// Apply one frame received from a client.
    pub fn receive(&self, connection: ConnectionId, frame: Frame) {
        match frame.kind {
            FrameKind::Control => match frame.to_client_message() {
                Ok(message) => self.handle(connection, message),
                Err(error) => self.reply_error(connection, format!("Invalid message: {error}")),
            },
            FrameKind::Audio => match frame.to_audio() {
                Ok(audio) => self.audio(connection, audio),
                Err(error) => self.reply_error(connection, format!("Invalid audio: {error}")),
            },
        }
    }

    fn reply_error(&self, connection: ConnectionId, error: String) {
        if let Some(record) = self.lock().connections.get(&connection) {
            record.outbox.message(error_message(error));
        }
    }

    fn handle(&self, connection: ConnectionId, message: ClientMessage) {
        match message {
            // Keepalive is answered whether or not the connection has said hello. Receiving it already counted as liveness.
            ClientMessage::Ping { nonce } => {
                if let Some(record) = self.lock().connections.get(&connection) {
                    record.outbox.keepalive(ServerMessage::Pong { nonce });
                }
                return;
            }
            ClientMessage::Pong { .. } => return,
            _ => {}
        }
        let mut deferred = Vec::new();
        {
            let mut state = self.lock();
            if let ClientMessage::Hello {
                protocol,
                participant,
                name,
                ..
            } = message
            {
                self.hello(
                    &mut state,
                    connection,
                    protocol,
                    participant,
                    name,
                    &mut deferred,
                );
            } else if let Some((participant, space)) = state.member(connection) {
                self.act(
                    &mut state,
                    connection,
                    &participant,
                    space.as_deref(),
                    message,
                    &mut deferred,
                );
            } else if let Some(record) = state.connections.get(&connection) {
                record.outbox.message(error_message("Send hello first."));
            }
        }
        self.run(deferred);
    }

    fn hello(
        &self,
        state: &mut HubState,
        connection: ConnectionId,
        protocol: u32,
        participant: String,
        name: String,
        deferred: &mut Vec<Deferred>,
    ) {
        let Some(outbox) = state
            .connections
            .get(&connection)
            .map(|record| record.outbox.clone())
        else {
            return;
        };
        if state.connections[&connection].participant.is_some() {
            outbox.message(error_message("This connection already said hello."));
            return;
        }
        if protocol != PROTOCOL_VERSION {
            outbox.message(error_message(format!(
                "Unsupported protocol {protocol}; this server speaks {PROTOCOL_VERSION}."
            )));
            outbox.close();
            return;
        }
        let invalid = if participant.is_empty() {
            Err("A participant identity is required.".to_owned())
        } else if name.trim().is_empty() {
            Err("A name is required.".to_owned())
        } else if name.trim_start().starts_with('/') {
            // The name begins each attributed prompt, where a leading slash could read as a command to the agent.
            Err("A name must not start with \"/\".".to_owned())
        } else {
            limits::check(
                &participant,
                limits::PARTICIPANT_BYTES,
                "participant identity",
            )
            .and_then(|()| limits::check(&name, limits::NAME_BYTES, "name"))
        };
        if let Err(error) = invalid {
            outbox.message(error_message(error));
            outbox.close();
            return;
        }
        if !state.participants.contains_key(&participant) {
            state.next_source += 1;
            let source = state.next_source;
            state.participants.insert(
                participant.clone(),
                ParticipantRecord {
                    name: String::new(),
                    source,
                    connection: None,
                    space: None,
                    last_space: None,
                    departed: None,
                },
            );
        }
        let record = state
            .participants
            .get_mut(&participant)
            .expect("inserted above");
        record.name = name;
        record.departed = None;
        let source = record.source;
        let previous = record.connection.replace(connection);
        let rejoin = record.space.clone().or_else(|| record.last_space.clone());
        // A reconnect supersedes the previous connection of the same participant.
        if let Some(previous) = previous.filter(|previous| *previous != connection) {
            state.abandon_utterance(previous, deferred);
            if let Some(old) = state.connections.get_mut(&previous) {
                old.participant = None;
                old.outbox.close();
            }
        }
        let record = state
            .connections
            .get_mut(&connection)
            .expect("checked above");
        record.participant = Some(participant.clone());
        record.liveness.greeted = true;
        let backends = self.backends.info();
        let roots = self
            .roots
            .read()
            .expect("workspace roots are never left poisoned")
            .clone();
        outbox.message(ServerMessage::Welcome {
            protocol: PROTOCOL_VERSION,
            server: crate::SERVER_NAME.to_owned(),
            source,
            backends,
            roots,
        });
        match rejoin.filter(|space| state.space(space).is_some()) {
            Some(space) => state.join(&participant, &space),
            None => outbox.spaces(),
        }
    }

    fn act(
        &self,
        state: &mut HubState,
        connection: ConnectionId,
        participant: &str,
        space: Option<&str>,
        message: ClientMessage,
        deferred: &mut Vec<Deferred>,
    ) {
        let outbox = state.connections[&connection].outbox.clone();
        // Actions on the current space.
        let in_space = |state: &HubState| -> Option<String> {
            match space {
                Some(space) if state.space(space).is_some() => Some(space.to_owned()),
                _ => {
                    state.error(participant, "Join a space first.");
                    None
                }
            }
        };
        match message {
            ClientMessage::Hello { .. } => unreachable!("handled by hello"),
            ClientMessage::Ping { .. } | ClientMessage::Pong { .. } => {
                unreachable!("handled before membership")
            }
            ClientMessage::ListSpaces => outbox.spaces(),
            ClientMessage::CreateSpace {
                title,
                backend,
                directory,
                session,
            } => {
                let checked = limits::check(&title, limits::TITLE_BYTES, "title")
                    .and_then(|()| limits::check(&directory, limits::DIRECTORY_BYTES, "directory"))
                    .and_then(|()| match &session {
                        Some(session) => limits::check(session, limits::SESSION_BYTES, "session"),
                        None => Ok(()),
                    });
                if let Err(error) = checked {
                    state.error(participant, error);
                    return;
                }
                let Some(found) = self.backends.get(&backend) else {
                    let names: Vec<&str> = self.backends.names().collect();
                    state.error(
                        participant,
                        format!("Unknown backend; available: {}.", names.join(", ")),
                    );
                    return;
                };
                if session.is_some() && !found.capabilities().attach {
                    state.error(
                        participant,
                        format!("The {backend} backend cannot bind an existing session."),
                    );
                    return;
                }
                // Refuse early when the limit is already reached. The supervisor checks the directory outside the lock and only then takes a place, so that a refused directory never holds one.
                if self.slots.is_full() {
                    state.error(participant, self.slots.full_error());
                    return;
                }
                let request = CreateRequest {
                    title,
                    backend,
                    directory,
                    session,
                    creator: participant.to_owned(),
                    creator_space: space.map(str::to_owned),
                };
                if self.jobs.try_send(Job::Create(request)).is_err() {
                    state.error(participant, "The server is shutting down.");
                }
            }
            ClientMessage::ListSessions { backend, directory } => {
                let checked = limits::check(&backend, limits::NAME_BYTES, "backend")
                    .and_then(|()| limits::check(&directory, limits::DIRECTORY_BYTES, "directory"));
                if let Err(error) = checked {
                    state.error(participant, error);
                    return;
                }
                let Some(found) = self.backends.get(&backend) else {
                    let names: Vec<&str> = self.backends.names().collect();
                    state.error(
                        participant,
                        format!("Unknown backend; available: {}.", names.join(", ")),
                    );
                    return;
                };
                if !found.capabilities().attach {
                    state.error(
                        participant,
                        format!("The {backend} backend cannot list existing sessions."),
                    );
                    return;
                }
                if self
                    .jobs
                    .try_send(Job::ListSessions {
                        participant: participant.to_owned(),
                        backend,
                        directory,
                    })
                    .is_err()
                {
                    state.error(participant, "The server is shutting down.");
                }
            }
            ClientMessage::Join { space: target } => {
                if let Err(error) =
                    limits::check(&target, limits::IDENTIFIER_BYTES, "space identifier")
                {
                    state.error(participant, error);
                    return;
                }
                if state.space(&target).is_none() {
                    // Naming the space lets the client tell this refusal from other errors; it is bounded above.
                    state.send(
                        participant,
                        ServerMessage::Error {
                            error: "That space does not exist.".to_owned(),
                            space: Some(target),
                        },
                    );
                    return;
                }
                state.join(participant, &target);
            }
            ClientMessage::Leave => {
                state.abandon_utterance(connection, deferred);
                // Announce leaving before the lobby listing that leaving produces.
                outbox.clear_audio();
                outbox.message(ServerMessage::Left);
                state.leave(participant);
                if let Some(record) = state.participants.get_mut(participant) {
                    record.last_space = None;
                }
                outbox.spaces();
            }
            ClientMessage::Gate { .. } => {
                // Each audio frame carries its own gate; the announcement needs no state yet.
            }
            ClientMessage::UtteranceBegin { utterance } => {
                if let Err(error) =
                    limits::check(&utterance, limits::UTTERANCE_BYTES, "utterance identifier")
                {
                    state.error(participant, error);
                    return;
                }
                let Some(space) = in_space(state) else {
                    return;
                };
                if let Some(open) = state
                    .connections
                    .get_mut(&connection)
                    .and_then(|record| record.utterance.take())
                {
                    state.utterances.remove(&open.id);
                    deferred.push(Deferred::Cancel(open.id));
                    outbox.message(ServerMessage::TranscriptFailed {
                        utterance: open.client,
                        error: "A new utterance started before this one ended.".to_owned(),
                    });
                }
                state.next_utterance += 1;
                let id = UtteranceId(state.next_utterance);
                state.utterances.insert(
                    id,
                    UtteranceRecord {
                        participant: participant.to_owned(),
                        client: utterance.clone(),
                        space,
                    },
                );
                let record = state
                    .connections
                    .get_mut(&connection)
                    .expect("checked above");
                record.straying = false;
                record.failed = None;
                record.utterance = Some(OpenUtterance {
                    client: utterance,
                    id,
                    expected: None,
                });
                deferred.push(Deferred::Begin(id));
            }
            ClientMessage::UtteranceEnd { utterance }
            | ClientMessage::UtteranceCancel { utterance }
                if state.connections[&connection].failed.as_ref() == Some(&utterance) =>
            {
                // Already failed and reported: the client sent this before it heard.
                if let Some(record) = state.connections.get_mut(&connection) {
                    record.failed = None;
                }
            }
            ClientMessage::UtteranceEnd { utterance }
            | ClientMessage::UtteranceCancel { utterance }
                if !state.connections[&connection]
                    .utterance
                    .as_ref()
                    .is_some_and(|open| open.client == utterance) =>
            {
                state.error(participant, "That utterance is not open.");
            }
            ClientMessage::UtteranceEnd { .. } => {
                let open = state
                    .connections
                    .get_mut(&connection)
                    .and_then(|record| record.utterance.take())
                    .expect("checked by the previous arm");
                deferred.push(Deferred::End(open.id));
            }
            ClientMessage::UtteranceCancel { .. } => {
                state.abandon_utterance(connection, deferred);
            }
            ClientMessage::CancelQueued { entry } => {
                let Some(space) = in_space(state) else {
                    return;
                };
                let record = state.space_mut(&space).expect("checked above");
                match record.model.cancel(&entry) {
                    Ok(()) => state.touch_space(&space),
                    Err(error) => state.error(participant, error),
                }
            }
            ClientMessage::Decide {
                request,
                decision,
                option,
            } => {
                let checked = limits::check(&request, limits::IDENTIFIER_BYTES, "request")
                    .and_then(|()| match &option {
                        Some(option) => limits::check(option, limits::IDENTIFIER_BYTES, "option"),
                        None => Ok(()),
                    });
                if let Err(error) = checked {
                    state.error(participant, error);
                    return;
                }
                let decision = match (decision, option) {
                    (protocol::Decision::AllowOnce, None) => backend::Decision::AllowOnce,
                    (protocol::Decision::Deny, None) => backend::Decision::Deny,
                    (protocol::Decision::Grant, Some(option)) => {
                        backend::Decision::Grant { option }
                    }
                    (protocol::Decision::Grant, None) => {
                        state.error(participant, "Choose an option to grant.");
                        return;
                    }
                    (_, Some(_)) => {
                        state.error(participant, "Only a grant decision takes an option.");
                        return;
                    }
                };
                let Some(space) = in_space(state) else {
                    return;
                };
                let record = state.space_mut(&space).expect("checked above");
                let option = match &decision {
                    backend::Decision::Grant { option } => Some(option.as_str()),
                    _ => None,
                };
                match record.model.claim_approval(&request, option) {
                    Ok(()) => {
                        let sent = record.commands.try_send(SpaceCommand::Decide {
                            request: request.clone(),
                            decision,
                            participant: participant.to_owned(),
                        });
                        if sent.is_err() {
                            record.model.restore_approval(&request);
                            state.error(participant, "The agent session has stopped.");
                        }
                        state.touch_space(&space);
                    }
                    Err(error) => state.error(participant, error),
                }
            }
            ClientMessage::Answer {
                question,
                field,
                text,
            } => {
                let checked = limits::check(&text, limits::ANSWER_BYTES, "answer")
                    .and_then(|()| limits::check(&question, limits::IDENTIFIER_BYTES, "question"))
                    .and_then(|()| limits::check(&field, limits::IDENTIFIER_BYTES, "field"));
                if let Err(error) = checked {
                    state.error(participant, error);
                    return;
                }
                let Some(space) = in_space(state) else {
                    return;
                };
                let record = state.space_mut(&space).expect("checked above");
                match record.model.answer(&question, &field, &text) {
                    Ok(AnswerOutcome::Recorded) => state.touch_space(&space),
                    Ok(AnswerOutcome::Complete(answers)) => {
                        let sent = record.commands.try_send(SpaceCommand::Answer {
                            question: question.clone(),
                            answers,
                            participant: participant.to_owned(),
                        });
                        if sent.is_err() {
                            record.model.restore_question(&question);
                            state.error(participant, "The agent session has stopped.");
                        }
                        state.touch_space(&space);
                    }
                    Err(error) => state.error(participant, error),
                }
            }
            ClientMessage::Presence { head, left, right } => {
                let Some(space) = space.and_then(|space| state.space(space)) else {
                    return;
                };
                let source = state.participants[participant].source;
                for other in &space.model.participants {
                    if other != participant {
                        state.send(
                            other,
                            ServerMessage::Presence {
                                source,
                                head,
                                left,
                                right,
                            },
                        );
                    }
                }
            }
            ClientMessage::SetAttribution { enabled } => {
                let Some(space) = in_space(state) else {
                    return;
                };
                state
                    .space_mut(&space)
                    .expect("checked above")
                    .model
                    .set_attribution(enabled);
                state.touch_space(&space);
            }
        }
    }

    /// Relay a participant's audio to the rest of their space, and hand push-to-talk audio to recognition.
    fn audio(&self, connection: ConnectionId, frame: AudioFrame) {
        let mut deferred = Vec::new();
        {
            let mut state = self.lock();
            let Some(record) = state.connections.get_mut(&connection) else {
                return;
            };
            let gap = match record.utterance.as_mut() {
                Some(open) if frame.gate == Gate::Agent => {
                    let gap = open
                        .expected
                        .is_some_and(|expected| expected != frame.sequence);
                    open.expected = Some(frame.sequence.wrapping_add(1));
                    gap
                }
                Some(open) => {
                    // Only agent audio is recognized, so only a gap between agent frames loses part of the utterance.
                    open.expected = None;
                    false
                }
                None => false,
            };
            let Some((participant, Some(space))) = state.member(connection) else {
                return;
            };
            let record = state
                .connections
                .get_mut(&connection)
                .expect("member has a connection");
            if frame.gate == Gate::Off {
                return;
            }
            if frame.gate == Gate::Agent && record.utterance.is_none() {
                record.stray_frames += 1;
                if !record.straying {
                    record.straying = true;
                    log::warn!(
                        "Push-to-talk audio from {participant} arrived outside an utterance; it is relayed but not transcribed."
                    );
                }
            }
            if frame.gate == Gate::Agent
                && let Some(open) = record.utterance.as_mut()
            {
                if gap {
                    let open = record.utterance.take().expect("matched above");
                    record.failed = Some(open.client.clone());
                    state.utterances.remove(&open.id);
                    deferred.push(Deferred::Cancel(open.id));
                    state.send(
                        &participant,
                        ServerMessage::TranscriptFailed {
                            utterance: open.client,
                            error: "Some of the recording was lost, so nothing was sent. Please try again."
                                .to_owned(),
                        },
                    );
                } else {
                    deferred.push(Deferred::Push(open.id, frame.clone()));
                }
            }
            let source = state.participants[&participant].source;
            // The relayed frame is the size of the frame received, which was within the limit.
            let Ok(encoded) = Frame::audio(&AudioFrame { source, ..frame }).try_encode() else {
                return;
            };
            if let Some(space) = state.space(&space) {
                for other in &space.model.participants {
                    if *other != participant
                        && let Some(outbox) = state.outbox(other)
                    {
                        outbox.audio(encoded.clone());
                    }
                }
            }
        }
        self.run(deferred);
    }

    pub(crate) fn transcript_partial(&self, utterance: UtteranceId, text: &str) {
        let state = self.lock();
        if let Some(record) = state.utterances.get(&utterance) {
            state.send(
                &record.participant,
                ServerMessage::Transcript {
                    utterance: record.client.clone(),
                    text: limits::truncate(text, limits::QUEUED_TEXT_BYTES),
                    finished: false,
                },
            );
        }
    }

    pub(crate) fn transcript_finished(&self, utterance: UtteranceId, text: &str) {
        let mut state = self.lock();
        let Some(record) = state.utterances.remove(&utterance) else {
            return;
        };
        let text = text.trim();
        // The key makes a repeated finalization of the same client utterance idempotent: it is acknowledged with the text queued the first time, and nothing is queued again. The participant's length comes first, so that no two participants' utterances share a key whatever their identifiers contain.
        let key = format!(
            "{}:{}/{}",
            record.participant.len(),
            record.participant,
            record.client
        );
        if let Some(queued) = state
            .space(&record.space)
            .and_then(|space| space.model.queued_text(&key))
        {
            let text = queued.to_owned();
            state.send(
                &record.participant,
                ServerMessage::Transcript {
                    utterance: record.client,
                    text,
                    finished: true,
                },
            );
            return;
        }
        let failure = if state
            .space(&record.space)
            .is_some_and(|space| space.model.is_closed())
        {
            Some("The agent session has ended, so nothing was sent.")
        } else if text.is_empty() {
            Some("Nothing was heard, so nothing was sent.")
        } else if text.len() > limits::QUEUED_TEXT_BYTES {
            Some("The recording was too long, so nothing was sent. Please say it in shorter parts.")
        } else if state
            .participants
            .get(&record.participant)
            .is_none_or(|participant| participant.space.as_deref() != Some(&record.space))
        {
            Some("You left the space before the transcript was ready, so nothing was sent.")
        } else if state
            .space(&record.space)
            .is_some_and(|space| space.model.waiting() >= limits::QUEUED_ENTRIES)
        {
            Some("The queue is full, so nothing was sent. Please wait for the agent to catch up.")
        } else {
            None
        };
        if let Some(error) = failure {
            state.send(
                &record.participant,
                ServerMessage::TranscriptFailed {
                    utterance: record.client,
                    error: error.to_owned(),
                },
            );
            return;
        }
        let speaker = state.participants[&record.participant].name.clone();
        if let Some(space) = state.space_mut(&record.space) {
            let queued =
                space
                    .model
                    .enqueue(key, record.participant.clone(), speaker, text.to_owned());
            debug_assert!(queued, "repeats are acknowledged above");
        }
        state.send(
            &record.participant,
            ServerMessage::Transcript {
                utterance: record.client,
                text: text.to_owned(),
                finished: true,
            },
        );
        state.pump(&record.space);
        state.touch_space(&record.space);
    }

    pub(crate) fn transcript_failed(&self, utterance: UtteranceId, error: &str) {
        let mut deferred = Vec::new();
        {
            let mut state = self.lock();
            let Some(record) = state.utterances.remove(&utterance) else {
                return;
            };
            // Recognition can fail while the utterance is still open: close it, so that the client may forget it without cancelling and its next utterance does not fail this one again. The transcriber is still told, as every utterance it began ends with an end or a cancel.
            for connection in state.connections.values_mut() {
                if let Some(open) = connection.utterance.take_if(|open| open.id == utterance) {
                    connection.failed = Some(open.client);
                    deferred.push(Deferred::Cancel(utterance));
                }
            }
            state.send(
                &record.participant,
                ServerMessage::TranscriptFailed {
                    utterance: record.client,
                    error: error.to_owned(),
                },
            );
        }
        self.run(deferred);
    }

    /// Add a space for a newly started session, and move its creator into it unless they have joined another space since asking. Returns `None`, registering nothing, once shutdown has begun: the caller must close the session.
    pub(crate) fn register_space(
        &self,
        request: &CreateRequest,
        session: String,
        snapshot: Snapshot,
        commands: async_channel::Sender<SpaceCommand>,
    ) -> Option<String> {
        let mut state = self.lock();
        if state.shutting_down {
            return None;
        }
        state.next_space += 1;
        let id = format!("space-{}-{}", state.run, state.next_space);
        let model = SpaceModel::new(
            id.clone(),
            request.title.clone(),
            request.backend.clone(),
            request.directory.clone(),
            session,
            snapshot,
        );
        log::info!(
            "Space {id} {:?} is bound to {} session {}.",
            model.title,
            model.backend,
            model.session
        );
        state.spaces.push(SpaceRecord {
            model,
            commands,
            touched: Cell::new(None),
            deferred: Cell::new(false),
            closed: None,
        });
        let creator_waiting = state
            .participants
            .get(&request.creator)
            .is_some_and(|record| {
                record.connection.is_some()
                    && (record.space.is_none() || record.space == request.creator_space)
            });
        if creator_waiting {
            state.join(&request.creator, &id);
        }
        state.pump(&id);
        state.forget_closed();
        state.touch_lobby();
        Some(id)
    }

    pub(crate) fn create_failed(&self, creator: &str, error: String) {
        self.lock().error(creator, error);
    }

    /// Report a backend's session list to `participant`, its failures as an error.
    pub(crate) fn sessions(
        &self,
        participant: &str,
        result: Result<Vec<backend::SessionSummary>, String>,
    ) {
        let message = match result {
            Ok(sessions) => ServerMessage::Sessions {
                sessions: sessions
                    .into_iter()
                    .map(|session| protocol::SessionSummary {
                        id: session.id,
                        title: session.title,
                        directory: session.directory,
                    })
                    .collect(),
            },
            Err(error) => ServerMessage::Error {
                error: limits::truncate(&error, limits::ERROR_BYTES),
                space: None,
            },
        };
        self.lock().send(participant, message);
    }

    pub(crate) fn space_event(&self, space: &str, event: Event) {
        let mut state = self.lock();
        let Some(record) = state.space_mut(space) else {
            return;
        };
        record.model.apply(event);
        state.stamp_closed();
        state.pump(space);
        if state.touch_space_paced(space, Instant::now()) {
            self.deferred_snapshot.notify(usize::MAX);
        }
    }

    /// When the earliest deferred snapshot is due, if any.
    pub fn snapshot_deadline(&self) -> Option<Instant> {
        let state = self.lock();
        state
            .spaces
            .iter()
            .filter(|space| space.deferred.get())
            .filter_map(|space| Some(space.touched.get()? + SNAPSHOT_INTERVAL))
            .min()
    }

    /// Wait for a space to defer a snapshot. Check [`snapshot_deadline`](Self::snapshot_deadline) after creating the listener to avoid a lost wakeup.
    pub fn listen_snapshots(&self) -> EventListener {
        self.deferred_snapshot.listen()
    }

    /// Send the deferred snapshots that are due at `now`.
    pub fn flush_snapshots(&self, now: Instant) {
        let state = self.lock();
        let due: Vec<String> = state
            .spaces
            .iter()
            .filter(|space| {
                space.deferred.get()
                    && space
                        .touched
                        .get()
                        .is_none_or(|touched| touched + SNAPSHOT_INTERVAL <= now)
            })
            .map(|space| space.model.id.clone())
            .collect();
        for space in due {
            state.touch_space(&space);
        }
    }

    pub(crate) fn space_sent(&self, space: &str, key: &str, result: backend::Result<()>) {
        let mut state = self.lock();
        let Some(record) = state.space_mut(space) else {
            return;
        };
        record
            .model
            .sent(key, result.map_err(|error| error.message));
        state.pump(space);
        state.touch_space(space);
    }

    pub(crate) fn space_decided(
        &self,
        space: &str,
        request: &str,
        participant: &str,
        result: backend::Result<()>,
    ) {
        let mut state = self.lock();
        let Err(error) = result else {
            if let Some(record) = state.space_mut(space) {
                record.model.decided(request);
            }
            state.pump(space);
            state.touch_space(space);
            return;
        };
        if let Some(record) = state.space_mut(space) {
            if matches!(error.kind, ErrorKind::NotFound | ErrorKind::Closed) {
                // The request is gone, so nothing is pending any more.
                record.model.decided(request);
            } else {
                record.model.restore_approval(request);
            }
        }
        state.pump(space);
        state.error(
            participant,
            format!("The decision was not accepted: {}", error.message),
        );
        state.touch_space(space);
    }

    pub(crate) fn space_answered(
        &self,
        space: &str,
        question: &str,
        participant: &str,
        result: backend::Result<()>,
    ) {
        let mut state = self.lock();
        let Err(error) = result else {
            if let Some(record) = state.space_mut(space) {
                record.model.answered(question);
            }
            state.pump(space);
            state.touch_space(space);
            return;
        };
        if let Some(record) = state.space_mut(space) {
            if matches!(error.kind, ErrorKind::NotFound | ErrorKind::Closed) {
                // The request is gone, so nothing is pending any more.
                record.model.answered(question);
            } else {
                record.model.restore_question(question);
            }
        }
        state.pump(space);
        state.error(
            participant,
            format!("The answer was not accepted: {}", error.message),
        );
        state.touch_space(space);
    }

    /// The driver stopped: the session produces no more events.
    pub(crate) fn space_ended(&self, space: &str, error: Option<String>) {
        let mut state = self.lock();
        let Some(record) = state.space_mut(space) else {
            return;
        };
        if !record.model.is_closed() {
            record.model.apply(Event::Closed { reason: error });
        }
        // Forgotten before the lobby listing that touching the space produces.
        state.forget_closed();
        state.touch_space(space);
    }

    /// Announce an agent speech segment to the space's current participants, in order with its frames.
    #[cfg_attr(not(feature = "speech"), allow(dead_code))]
    pub(crate) fn agent_speech(&self, space: &str, message: ServerMessage) {
        let state = self.lock();
        let Some(record) = state.space(space) else {
            return;
        };
        for participant in &record.model.participants {
            if let Some(outbox) = state.outbox(participant) {
                outbox.agent_message(message.clone());
            }
        }
    }

    /// Send one agent speech frame to the space's current participants. A participant who joined mid-segment receives from here on.
    #[cfg_attr(not(feature = "speech"), allow(dead_code))]
    pub(crate) fn agent_audio(&self, space: &str, frame: &AudioFrame) {
        let Ok(encoded) = Frame::audio(frame).try_encode() else {
            log::warn!("An agent speech frame for {space} exceeds the frame limit.");
            return;
        };
        let state = self.lock();
        let Some(record) = state.space(space) else {
            return;
        };
        for participant in &record.model.participants {
            if let Some(outbox) = state.outbox(participant) {
                outbox.agent_audio(encoded.clone());
            }
        }
    }

    /// Tell the space's participants that agent speech could not be produced.
    #[cfg_attr(not(feature = "speech"), allow(dead_code))]
    pub(crate) fn speech_failed(&self, space: &str, error: String) {
        let state = self.lock();
        let Some(record) = state.space(space) else {
            return;
        };
        for participant in &record.model.participants {
            state.error(participant, error.clone());
        }
    }

    /// Render a deferred outgoing item for one connection.
    pub fn render(&self, connection: ConnectionId, item: Outgoing) -> Option<ServerMessage> {
        match item {
            Outgoing::Message(message) => Some(message),
            Outgoing::Space => {
                let state = self.lock();
                let (_, space) = state.member(connection)?;
                let space = state.space(&space?)?;
                let participants = state.participants(&space.model);
                let mut state = space.model.state(participants);
                limits::fit_state(&mut state);
                Some(ServerMessage::Space {
                    state: Box::new(state),
                })
            }
            Outgoing::Spaces => {
                let state = self.lock();
                let mut spaces: Vec<_> = state
                    .spaces
                    .iter()
                    .map(|space| space.model.summary())
                    .collect();
                limits::fit_spaces(&mut spaces);
                Some(ServerMessage::Spaces { spaces })
            }
        }
    }

    /// Ask every space driver to close its session, and end every connection.
    pub fn shutdown(&self) {
        self.jobs.close();
        let mut state = self.lock();
        // Spaces still being created are closed as soon as their session starts; see `register_space`.
        state.shutting_down = true;
        for space in &state.spaces {
            let _ = space.commands.try_send(SpaceCommand::Close);
        }
        for connection in state.connections.values() {
            connection.outbox.close();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transcriber::NullTranscriber;

    fn hub() -> Arc<Hub> {
        let (jobs, _) = async_channel::unbounded();
        Hub::new(
            Backends::new(),
            Arc::new(NullTranscriber::new()),
            jobs,
            Vec::new(),
            SpaceSlots::new(crate::workspace::DEFAULT_MAX_SPACES),
        )
    }

    #[test]
    fn a_connection_whose_output_is_not_accepted_is_released_even_while_it_sends() {
        let hub = hub();
        let keepalive = Keepalive {
            interval: Duration::from_secs(5),
            timeout: Duration::from_secs(15),
        };
        let hello = Duration::from_secs(3600);
        let blocked = Cancellation::new();
        let (_, _, blocked_received, blocked_output) = hub.connect(blocked.clone());
        let flowing = Cancellation::new();
        let (_, _, flowing_received, flowing_output) = hub.connect(flowing.clone());

        let start = Instant::now();
        // Both peers keep sending. One write is never accepted; the other connection's writes complete.
        blocked_output.handed_out();
        for second in 0..=20 {
            blocked_received.store(true, Ordering::Release);
            flowing_received.store(true, Ordering::Release);
            flowing_output.asked();
            flowing_output.handed_out();
            hub.sweep(start + Duration::from_secs(second), &keepalive, hello);
            assert_eq!(blocked.is_cancelled(), second >= 15, "after {second} s");
        }
        assert!(!flowing.is_cancelled());
    }
}
