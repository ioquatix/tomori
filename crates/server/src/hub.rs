// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

//! Shared server state: participants, connections, spaces and open utterances, behind one lock.
//!
//! Every client action and backend event is applied synchronously under the lock. Work that may call out of the server — transcriber calls and backend operations — is deferred until the lock is released, or sent to the space's driver task.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, Weak};
use std::time::Instant;

use backend::{AnswerValue, Backend, ErrorKind, Event, Prompt, Snapshot};
use base_scheduler::Cancellation;
use protocol::{
    AudioFrame, ClientMessage, Frame, FrameKind, Gate, Keepalive, PROTOCOL_VERSION, Participant,
    ServerMessage,
};

use crate::backends::Backends;
use crate::outbox::{Outbox, Outgoing};
use crate::space::{AnswerOutcome, SpaceModel};
use crate::transcriber::{Transcriber, TranscriptSink, UtteranceId};

pub type ConnectionId = u64;

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
    /// The participant who asked; they join the space once it exists.
    pub creator: String,
}

pub struct Hub {
    state: Mutex<HubState>,
    pub(crate) backends: Backends,
    transcriber: Arc<dyn Transcriber>,
    jobs: async_channel::Sender<CreateRequest>,
    this: Weak<Hub>,
}

#[derive(Default)]
struct HubState {
    next_connection: ConnectionId,
    next_source: u32,
    next_space: u64,
    next_utterance: u64,
    next_ping: u64,
    connections: HashMap<ConnectionId, ConnectionRecord>,
    participants: HashMap<String, ParticipantRecord>,
    /// In creation order.
    spaces: Vec<SpaceRecord>,
    utterances: HashMap<UtteranceId, UtteranceRecord>,
}

struct ConnectionRecord {
    outbox: Arc<Outbox>,
    participant: Option<String>,
    last_sequence: Option<u32>,
    utterance: Option<OpenUtterance>,
    liveness: Liveness,
}

/// What the keepalive sweep knows about one connection.
struct Liveness {
    /// Set by the connection whenever data arrives, and cleared by the sweep.
    received: Arc<AtomicBool>,
    /// When the sweep last saw data arrive, or first saw the connection.
    last: Option<Instant>,
    pinged: Option<Instant>,
    /// Ends this connection's HTTP exchange, including a write blocked on a peer that is gone.
    cancellation: Cancellation,
}

struct OpenUtterance {
    client: String,
    id: UtteranceId,
    frames: usize,
}

struct ParticipantRecord {
    name: String,
    source: u32,
    connection: Option<ConnectionId>,
    space: Option<String>,
    /// The space to rejoin when this participant reconnects.
    last_space: Option<String>,
}

struct SpaceRecord {
    model: SpaceModel,
    commands: async_channel::Sender<SpaceCommand>,
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
        self.send(
            participant,
            ServerMessage::Error {
                error: error.into(),
            },
        );
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
            for participant in &space.model.participants {
                if let Some(outbox) = self.outbox(participant) {
                    outbox.space();
                }
            }
        }
        self.touch_lobby();
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

    fn leave(&mut self, participant: &str) {
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
        self.leave(participant);
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
        jobs: async_channel::Sender<CreateRequest>,
    ) -> Arc<Self> {
        Arc::new_cyclic(|this| Self {
            state: Mutex::new(HubState::default()),
            backends,
            transcriber,
            jobs,
            this: this.clone(),
        })
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

    /// Register a connection. `cancellation` must end only this connection's exchange: the keepalive sweep cancels it when the connection goes silent. The connection sets the returned flag whenever data arrives.
    pub fn connect(
        &self,
        cancellation: Cancellation,
    ) -> (ConnectionId, Arc<Outbox>, Arc<AtomicBool>) {
        let mut state = self.lock();
        state.next_connection += 1;
        let id = state.next_connection;
        let outbox = Arc::new(Outbox::new());
        let received = Arc::new(AtomicBool::new(false));
        state.connections.insert(
            id,
            ConnectionRecord {
                outbox: outbox.clone(),
                participant: None,
                last_sequence: None,
                utterance: None,
                liveness: Liveness {
                    received: received.clone(),
                    last: None,
                    pinged: None,
                    cancellation,
                },
            },
        );
        (id, outbox, received)
    }

    /// Ping connections that have been quiet for the keepalive interval, and release those that have received nothing for the keepalive timeout as if they had disconnected. Called periodically with the current time; a connection's quiet time is measured from the first sweep that sees it or the last that saw data arrive.
    pub fn sweep(&self, now: Instant, keepalive: &Keepalive) {
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
                if quiet >= keepalive.timeout {
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
            log::info!("Connection {id} received nothing within the keepalive timeout.");
            // Release the participant now; cancelling also ends the exchange, even one blocked writing to a peer that is gone.
            self.disconnect(id);
            cancellation.cancel();
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
                let current = state
                    .participants
                    .get(&participant)
                    .is_some_and(|record| record.connection == Some(connection));
                if current {
                    state.leave(&participant);
                    if let Some(record) = state.participants.get_mut(&participant) {
                        record.connection = None;
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
            record.outbox.message(ServerMessage::Error { error });
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
                record.outbox.message(ServerMessage::Error {
                    error: "Send hello first.".to_owned(),
                });
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
            outbox.message(ServerMessage::Error {
                error: "This connection already said hello.".to_owned(),
            });
            return;
        }
        if protocol != PROTOCOL_VERSION {
            outbox.message(ServerMessage::Error {
                error: format!(
                    "Unsupported protocol {protocol}; this server speaks {PROTOCOL_VERSION}."
                ),
            });
            outbox.close();
            return;
        }
        if participant.is_empty() {
            outbox.message(ServerMessage::Error {
                error: "A participant identity is required.".to_owned(),
            });
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
                },
            );
        }
        let record = state
            .participants
            .get_mut(&participant)
            .expect("inserted above");
        record.name = name;
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
        state
            .connections
            .get_mut(&connection)
            .expect("checked above")
            .participant = Some(participant.clone());
        outbox.message(ServerMessage::Welcome {
            protocol: PROTOCOL_VERSION,
            server: crate::SERVER_NAME.to_owned(),
            source,
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
                let Some(found) = self.backends.get(&backend) else {
                    let names: Vec<&str> = self.backends.names().collect();
                    state.error(
                        participant,
                        format!(
                            "Unknown backend {backend:?}; available: {}.",
                            names.join(", ")
                        ),
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
                let request = CreateRequest {
                    title,
                    backend,
                    directory,
                    session,
                    creator: participant.to_owned(),
                };
                if self.jobs.try_send(request).is_err() {
                    state.error(participant, "The server is shutting down.");
                }
            }
            ClientMessage::Join { space: target } => {
                if state.space(&target).is_none() {
                    state.error(participant, format!("No space {target:?}."));
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
                state
                    .connections
                    .get_mut(&connection)
                    .expect("checked above")
                    .utterance = Some(OpenUtterance {
                    client: utterance,
                    id,
                    frames: 0,
                });
                deferred.push(Deferred::Begin(id));
            }
            ClientMessage::UtteranceEnd { utterance }
            | ClientMessage::UtteranceCancel { utterance }
                if !state.connections[&connection]
                    .utterance
                    .as_ref()
                    .is_some_and(|open| open.client == utterance) =>
            {
                state.error(participant, format!("No open utterance {utterance:?}."));
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
            ClientMessage::Decide { request, decision } => {
                let Some(space) = in_space(state) else {
                    return;
                };
                let record = state.space_mut(&space).expect("checked above");
                match record.model.claim_approval(&request) {
                    Ok(()) => {
                        let decision = match decision {
                            protocol::Decision::AllowOnce => backend::Decision::AllowOnce,
                            protocol::Decision::Deny => backend::Decision::Deny,
                        };
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
            let Some((participant, Some(space))) = state.member(connection) else {
                return;
            };
            let record = state
                .connections
                .get_mut(&connection)
                .expect("member has a connection");
            let contiguous = record
                .last_sequence
                .is_none_or(|last| last.wrapping_add(1) == frame.sequence);
            record.last_sequence = Some(frame.sequence);
            if frame.gate == Gate::Off {
                return;
            }
            if frame.gate == Gate::Agent
                && let Some(open) = record.utterance.as_mut()
            {
                if open.frames > 0 && !contiguous {
                    let open = record.utterance.take().expect("matched above");
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
                    open.frames += 1;
                    deferred.push(Deferred::Push(open.id, frame.clone()));
                }
            }
            let source = state.participants[&participant].source;
            let encoded = Frame::audio(&AudioFrame { source, ..frame }).encode();
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
                    text: text.to_owned(),
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
        let failure = if text.is_empty() {
            Some("Nothing was heard, so nothing was sent.")
        } else if state
            .participants
            .get(&record.participant)
            .is_none_or(|participant| participant.space.as_deref() != Some(&record.space))
        {
            Some("You left the space before the transcript was ready, so nothing was sent.")
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
        // The key makes a repeated finalization of the same client utterance idempotent.
        let key = format!("{}/{}", record.participant, record.client);
        if let Some(space) = state.space_mut(&record.space) {
            space.model.enqueue(key, speaker, text.to_owned());
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
        let mut state = self.lock();
        if let Some(record) = state.utterances.remove(&utterance) {
            state.send(
                &record.participant,
                ServerMessage::TranscriptFailed {
                    utterance: record.client,
                    error: error.to_owned(),
                },
            );
        }
    }

    /// Add a space for a newly started session, and move its creator into it.
    pub(crate) fn register_space(
        &self,
        request: &CreateRequest,
        session: String,
        snapshot: Snapshot,
        commands: async_channel::Sender<SpaceCommand>,
    ) -> String {
        let mut state = self.lock();
        state.next_space += 1;
        let id = format!("space-{}", state.next_space);
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
        state.spaces.push(SpaceRecord { model, commands });
        if state
            .participants
            .get(&request.creator)
            .is_some_and(|record| record.connection.is_some())
        {
            state.join(&request.creator, &id);
        }
        state.pump(&id);
        state.touch_lobby();
        id
    }

    pub(crate) fn create_failed(&self, creator: &str, error: String) {
        self.lock().error(creator, error);
    }

    pub(crate) fn space_event(&self, space: &str, event: Event) {
        let mut state = self.lock();
        let Some(record) = state.space_mut(space) else {
            return;
        };
        record.model.apply(event);
        state.pump(space);
        state.touch_space(space);
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
        let Err(error) = result else {
            return;
        };
        let mut state = self.lock();
        if let Some(record) = state.space_mut(space)
            && !matches!(error.kind, ErrorKind::NotFound | ErrorKind::Closed)
        {
            record.model.restore_approval(request);
        }
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
        let Err(error) = result else {
            return;
        };
        let mut state = self.lock();
        if let Some(record) = state.space_mut(space)
            && !matches!(error.kind, ErrorKind::NotFound | ErrorKind::Closed)
        {
            record.model.restore_question(question);
        }
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
        let encoded = Frame::audio(frame).encode();
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
                Some(ServerMessage::Space {
                    state: Box::new(space.model.state(participants)),
                })
            }
            Outgoing::Spaces => {
                let state = self.lock();
                Some(ServerMessage::Spaces {
                    spaces: state
                        .spaces
                        .iter()
                        .map(|space| space.model.summary())
                        .collect(),
                })
            }
        }
    }

    /// Ask every space driver to close its session, and end every connection.
    pub fn shutdown(&self) {
        self.jobs.close();
        let state = self.lock();
        for space in &state.spaces {
            let _ = space.commands.try_send(SpaceCommand::Close);
        }
        for connection in state.connections.values() {
            connection.outbox.close();
        }
    }
}
