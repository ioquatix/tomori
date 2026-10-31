// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

use std::collections::VecDeque;
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::task::{Context, Poll, Waker};

use protocol::{AudioFrame, ClientMessage, Frame, Gate};

/// The membership a reconnecting client restores after its Hello.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Membership {
    pub space: Option<String>,
    pub gate: Gate,
}

impl Membership {
    /// The messages that restore this membership on a new connection, in order.
    pub fn messages(&self) -> Vec<ClientMessage> {
        let mut messages = Vec::new();
        if let Some(space) = &self.space {
            messages.push(ClientMessage::Join {
                space: space.clone(),
            });
        }
        if self.gate != Gate::Off {
            messages.push(ClientMessage::Gate { gate: self.gate });
        }
        messages
    }
}

/// Why a message was not accepted for sending.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SendError {
    /// The client has been closed.
    Closed,
    /// `Hello`, `Ping` and `Pong` are sent by the client itself.
    Reserved,
}

impl std::fmt::Display for SendError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Closed => write!(formatter, "the client is closed"),
            Self::Reserved => write!(
                formatter,
                "hello and keepalive messages are sent by the client itself"
            ),
        }
    }
}

impl std::error::Error for SendError {}

/// Why an utterance could not reach recognition intact. The client sends nothing more for it, so no transcript will arrive: a later `UtteranceEnd` or `UtteranceCancel` for it is not sent.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UtteranceFailure {
    /// The client was disconnected when the utterance began, or the connection ended before the utterance's end and all of its audio were sent.
    Disconnected,
    /// The link was too congested to queue the utterance's audio within [`crate::Options::outgoing_utterance_limit`]. The client sent `UtteranceCancel` in its place.
    Congested,
}

impl std::fmt::Display for UtteranceFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Disconnected => write!(formatter, "the connection to the server was lost"),
            Self::Congested => write!(
                formatter,
                "the connection was too slow to send the recording"
            ),
        }
    }
}

/// An utterance that failed in the client before reaching recognition intact.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FailedUtterance {
    pub utterance: String,
    pub failure: UtteranceFailure,
}

/// What the connection should do next with its request body.
#[derive(Debug, PartialEq, Eq)]
pub enum Batch {
    /// Encoded frames to write.
    Frames(Vec<u8>),
    /// The client is closing and every control message has been taken: finish the request body.
    Closed,
}

/// The utterance the application has begun and not yet ended or cancelled.
struct OpenUtterance {
    utterance: String,
    /// Identifies this utterance's queued frames.
    generation: u64,
    /// Failed in the client: its audio is no longer protected and its end is not sent.
    failed: bool,
    /// This utterance's frames queued and not yet taken.
    queued: usize,
}

/// Microphone audio and the utterance control messages that stay in order with it.
enum Ordered {
    /// A frame, with the generation of the utterance it belongs to when it must not be dropped.
    Audio {
        frame: AudioFrame,
        utterance: Option<u64>,
    },
    /// `UtteranceBegin`, `UtteranceEnd` or `UtteranceCancel`, with its position among all queued messages.
    Utterance { order: u64, message: ClientMessage },
}

struct State {
    /// Keepalive messages, sent before anything else so that queued audio never delays them.
    keepalive: VecDeque<ClientMessage>,
    /// Other control messages, each with its position among all queued messages.
    control: VecDeque<(u64, ClientMessage)>,
    /// Audio frames and utterance control messages, never reordered relative to each other.
    stream: VecDeque<Ordered>,
    /// The position of the last queued message.
    order: u64,
    /// The generation of the last utterance begun.
    generation: u64,
    /// Queued frames that congestion may drop.
    droppable: usize,
    audio_limit: usize,
    utterance_limit: usize,
    utterance: Option<OpenUtterance>,
    membership: Membership,
    /// A `Leave` the server has not confirmed, sent again after the next Hello because the server rejoins the last space on reconnection.
    leave_unconfirmed: bool,
    connected: bool,
    closed: bool,
    /// Alternates between control and the stream when both are waiting, so neither starves the other.
    prefer_stream: bool,
    dropped_audio: u64,
    waker: Option<Waker>,
}

impl State {
    fn next_order(&mut self) -> u64 {
        self.order += 1;
        self.order
    }

    /// The position of the first queued utterance control message, which no later control message may overtake.
    fn first_utterance_order(&self) -> Option<u64> {
        self.stream.iter().find_map(|item| match item {
            Ordered::Utterance { order, .. } => Some(*order),
            Ordered::Audio { .. } => None,
        })
    }

    /// Fail the open utterance because its audio cannot be queued: drop its queued frames and cancel it in their place.
    fn fail_congested(&mut self) -> Option<FailedUtterance> {
        let open = self.utterance.as_mut()?;
        open.failed = true;
        open.queued = 0;
        let generation = open.generation;
        let utterance = open.utterance.clone();
        let before = self.stream.len();
        self.stream.retain(|item| {
            !matches!(item, Ordered::Audio { utterance: Some(queued), .. } if *queued == generation)
        });
        self.dropped_audio += (before - self.stream.len()) as u64;
        let order = self.next_order();
        self.stream.push_back(Ordered::Utterance {
            order,
            message: ClientMessage::UtteranceCancel {
                utterance: utterance.clone(),
            },
        });
        Some(FailedUtterance {
            utterance,
            failure: UtteranceFailure::Congested,
        })
    }
}

/// Outgoing traffic shared between application threads and the connection.
///
/// Control messages are delivered at most once. While disconnected they wait for the next connection, except that membership changes (`Join`, `Leave`, `Gate`) are folded into the [`Membership`] restored after Hello, stale `Presence` is discarded, and utterance messages are handled as below. A message already taken for a connection that then fails may be lost; the application learns of that connection's end from the [`crate::ConnectionState`] change.
///
/// Utterance control messages (`UtteranceBegin`, `UtteranceEnd`, `UtteranceCancel`) and audio frames are sent in exactly the order they were queued, so recognition receives an utterance's audio whole. Other control messages may overtake queued audio, but never overtake an earlier utterance control message, nor are they overtaken by a later one.
///
/// Live audio is real-time: it is dropped while disconnected, and when the link is congested the oldest queued frames beyond the audio limit are dropped instead of buffering without bound. Frames of the open utterance, `Agent` or `Live`, are never dropped this way, because every `Agent` frame must reach recognition and a gap between them fails the utterance (`design/live-audio.md`). They are bounded separately by the utterance limit: beyond it, or if the connection ends before the utterance is sent, the utterance fails visibly rather than reaching recognition with a gap. Its queued frames are dropped, `UtteranceCancel` replaces them on a live connection, its later end is not sent, and a [`FailedUtterance`] is returned for the application.
pub struct Outbox {
    state: Mutex<State>,
}

impl Outbox {
    pub fn new(audio_limit: usize, utterance_limit: usize) -> Self {
        Self {
            state: Mutex::new(State {
                keepalive: VecDeque::new(),
                control: VecDeque::new(),
                stream: VecDeque::new(),
                order: 0,
                generation: 0,
                droppable: 0,
                audio_limit: audio_limit.max(1),
                utterance_limit: utterance_limit.max(1),
                utterance: None,
                membership: Membership::default(),
                leave_unconfirmed: false,
                connected: false,
                closed: false,
                prefer_stream: false,
                dropped_audio: 0,
                waker: None,
            }),
        }
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn wake(mut state: MutexGuard<'_, State>) {
        let waker = state.waker.take();
        drop(state);
        if let Some(waker) = waker {
            waker.wake();
        }
    }

    /// Queue a control message. Returns the utterance that failed as a result, if any.
    pub fn send(&self, message: ClientMessage) -> Result<Option<FailedUtterance>, SendError> {
        let mut state = self.lock();
        if state.closed {
            return Err(SendError::Closed);
        }
        // Messages superseded by the restored membership, or too stale to deliver later, are not queued while disconnected.
        let deferred = match &message {
            ClientMessage::Hello { .. }
            | ClientMessage::Ping { .. }
            | ClientMessage::Pong { .. } => return Err(SendError::Reserved),
            ClientMessage::UtteranceBegin { .. }
            | ClientMessage::UtteranceEnd { .. }
            | ClientMessage::UtteranceCancel { .. } => {
                return Ok(Self::send_utterance(state, message));
            }
            ClientMessage::Join { space } => {
                state.membership.space = Some(space.clone());
                state.leave_unconfirmed = false;
                true
            }
            ClientMessage::Leave => {
                state.membership.space = None;
                state.leave_unconfirmed = true;
                true
            }
            ClientMessage::Gate { gate } => {
                state.membership.gate = *gate;
                true
            }
            ClientMessage::Presence { .. } => !state.connected,
            _ => false,
        };
        if deferred && !state.connected {
            return Ok(None);
        }
        if let ClientMessage::Presence { .. } = message {
            // Only the latest pose matters, so a congested link does not accumulate presence updates.
            if let Some((_, queued)) = state
                .control
                .iter_mut()
                .find(|(_, queued)| matches!(queued, ClientMessage::Presence { .. }))
            {
                *queued = message;
                return Ok(None);
            }
        }
        let order = state.next_order();
        state.control.push_back((order, message));
        Self::wake(state);
        Ok(None)
    }

    /// Forget the open utterance without sending anything, because the server has already ended it. Its queued frames are still sent in order. Does nothing if `utterance` is not the open one.
    pub fn abandon_utterance(&self, utterance: &str) {
        self.lock()
            .utterance
            .take_if(|open| open.utterance == utterance);
    }

    fn send_utterance(
        mut state: MutexGuard<'_, State>,
        message: ClientMessage,
    ) -> Option<FailedUtterance> {
        match &message {
            ClientMessage::UtteranceBegin { utterance } => {
                // A begin that cannot be sent now cannot be delivered with its audio later.
                let failed = !state.connected;
                state.generation += 1;
                state.utterance = Some(OpenUtterance {
                    utterance: utterance.clone(),
                    generation: state.generation,
                    failed,
                    queued: 0,
                });
                if failed {
                    return Some(FailedUtterance {
                        utterance: utterance.clone(),
                        failure: UtteranceFailure::Disconnected,
                    });
                }
            }
            ClientMessage::UtteranceEnd { utterance }
            | ClientMessage::UtteranceCancel { utterance } => {
                // The ended utterance's queued frames stay protected, so the end follows all of them.
                let open = state.utterance.take_if(|open| open.utterance == *utterance);
                // A failed utterance has already been reported, and nothing is open on the server for an end sent while disconnected.
                if open.is_some_and(|open| open.failed) || !state.connected {
                    return None;
                }
            }
            _ => unreachable!("only utterance control messages are ordered with audio"),
        }
        let order = state.next_order();
        state
            .stream
            .push_back(Ordered::Utterance { order, message });
        Self::wake(state);
        None
    }

    /// Queue a microphone frame. Returns the utterance that failed because its audio could not be queued, if any.
    pub fn send_audio(&self, frame: AudioFrame) -> Result<Option<FailedUtterance>, SendError> {
        let mut state = self.lock();
        if state.closed {
            return Err(SendError::Closed);
        }
        if frame.gate == Gate::Off {
            return Ok(None);
        }
        if !state.connected {
            state.dropped_audio += 1;
            return Ok(None);
        }
        let limit = state.utterance_limit;
        let utterance = match state.utterance.as_mut() {
            // Live frames of the open utterance are kept too, so that its sequence numbers have no gap the server would take for lost agent audio.
            Some(open) if !open.failed => {
                if open.queued >= limit {
                    state.dropped_audio += 1;
                    let failed = state.fail_congested();
                    Self::wake(state);
                    return Ok(failed);
                }
                open.queued += 1;
                Some(open.generation)
            }
            _ => None,
        };
        state.stream.push_back(Ordered::Audio { frame, utterance });
        if utterance.is_none() {
            state.droppable += 1;
            while state.droppable > state.audio_limit {
                let index = state
                    .stream
                    .iter()
                    .position(|item| {
                        matches!(
                            item,
                            Ordered::Audio {
                                utterance: None,
                                ..
                            }
                        )
                    })
                    .expect("droppable frames are queued");
                state.stream.remove(index);
                state.droppable -= 1;
                state.dropped_audio += 1;
            }
        }
        Self::wake(state);
        Ok(None)
    }

    /// Queue a `Ping` or `Pong` ahead of all other traffic. Keepalive messages belong to one connection, so they are discarded while disconnected.
    pub(crate) fn keepalive(&self, message: ClientMessage) {
        debug_assert!(matches!(
            message,
            ClientMessage::Ping { .. } | ClientMessage::Pong { .. }
        ));
        let mut state = self.lock();
        if state.closed || !state.connected {
            return;
        }
        state.keepalive.push_back(message);
        Self::wake(state);
    }

    /// Outgoing audio frames dropped because the client was disconnected or the link was congested.
    pub fn dropped_audio(&self) -> u64 {
        self.lock().dropped_audio
    }

    pub fn membership(&self) -> Membership {
        self.lock().membership.clone()
    }

    /// Follow the space the server reports, so a reconnection does not rejoin a space the server left on the client's behalf. A queued `Join` or `Leave` takes precedence; any later message for an in-flight one corrects it.
    pub fn joined(&self, space: Option<String>) {
        let mut state = self.lock();
        let pending = state.control.iter().any(|(_, message)| {
            matches!(message, ClientMessage::Join { .. } | ClientMessage::Leave)
        });
        if !pending {
            state.membership.space = space;
            state.leave_unconfirmed = false;
        }
    }

    pub fn is_closed(&self) -> bool {
        self.lock().closed
    }

    /// Stop accepting messages. Queued control messages are still delivered before the request body finishes.
    pub fn close(&self) {
        let mut state = self.lock();
        state.closed = true;
        Self::wake(state);
    }

    /// Start a connection: return the messages that restore membership, which supersede any queued membership changes.
    ///
    /// The server rejoins a reconnecting participant's last space, so a `Leave` it has not confirmed with `Left` is sent again, whether it was sent while disconnected or lost with the previous connection. A redundant `Leave` is harmless.
    pub fn connect(&self) -> Vec<ClientMessage> {
        let mut state = self.lock();
        state.connected = true;
        state.stream.clear();
        state.droppable = 0;
        state.control.retain(|(_, message)| {
            !matches!(
                message,
                ClientMessage::Join { .. }
                    | ClientMessage::Leave
                    | ClientMessage::Gate { .. }
                    | ClientMessage::Presence { .. }
            )
        });
        let mut messages = Vec::new();
        if state.membership.space.is_none() && state.leave_unconfirmed {
            messages.push(ClientMessage::Leave);
        }
        messages.extend(state.membership.messages());
        messages
    }

    /// The connection ended. Unsent control messages wait for the next one; queued audio is discarded.
    ///
    /// Returns the utterances that can no longer reach recognition intact: the open one, and any whose end had not been sent.
    pub fn disconnect(&self) -> Vec<FailedUtterance> {
        let mut state = self.lock();
        state.connected = false;
        state.keepalive.clear();
        let mut failed = Vec::new();
        for item in std::mem::take(&mut state.stream) {
            match item {
                Ordered::Audio { .. } => state.dropped_audio += 1,
                Ordered::Utterance {
                    message: ClientMessage::UtteranceEnd { utterance },
                    ..
                } => failed.push(utterance),
                Ordered::Utterance { .. } => {}
            }
        }
        state.droppable = 0;
        if let Some(open) = state.utterance.as_mut().filter(|open| !open.failed) {
            open.failed = true;
            open.queued = 0;
            failed.push(open.utterance.clone());
        }
        state.waker = None;
        failed
            .into_iter()
            .map(|utterance| FailedUtterance {
                utterance,
                failure: UtteranceFailure::Disconnected,
            })
            .collect()
    }

    /// Take up to roughly `size_limit` bytes of encoded frames, keepalive first, then alternating control and the ordered stream of audio and utterance control, or wait for some.
    pub fn poll_batch(&self, context: &mut Context<'_>, size_limit: usize) -> Poll<Batch> {
        let mut state = self.lock();
        let mut output = Vec::new();
        while let Some(message) = state.keepalive.pop_front() {
            Frame::client(&message).encode_into(&mut output);
        }
        while output.len() < size_limit {
            let control = state.control.front().map(|(order, _)| *order);
            let take_stream = match (control, state.stream.front()) {
                (None, None) => break,
                (Some(_), None) => false,
                (None, Some(_)) => true,
                // An utterance control message waits for earlier control messages.
                (Some(control), Some(Ordered::Utterance { order, .. })) if control < *order => {
                    false
                }
                // A control message waits for earlier utterance control messages, even behind audio.
                (Some(control), Some(_))
                    if state
                        .first_utterance_order()
                        .is_some_and(|order| order < control) =>
                {
                    true
                }
                (Some(_), Some(_)) => state.prefer_stream,
            };
            if take_stream {
                match state.stream.pop_front().expect("the stream is waiting") {
                    Ordered::Audio { frame, utterance } => {
                        match utterance {
                            Some(generation) => {
                                if let Some(open) = state
                                    .utterance
                                    .as_mut()
                                    .filter(|open| open.generation == generation)
                                {
                                    open.queued -= 1;
                                }
                            }
                            None => state.droppable -= 1,
                        }
                        Frame::audio(&frame).encode_into(&mut output);
                    }
                    Ordered::Utterance { message, .. } => {
                        Frame::client(&message).encode_into(&mut output);
                    }
                }
                state.prefer_stream = false;
            } else {
                let (_, message) = state.control.pop_front().expect("control is waiting");
                Frame::client(&message).encode_into(&mut output);
                state.prefer_stream = true;
            }
        }
        if !output.is_empty() {
            Poll::Ready(Batch::Frames(output))
        } else if state.closed {
            Poll::Ready(Batch::Closed)
        } else {
            state.waker = Some(context.waker().clone());
            Poll::Pending
        }
    }

    /// Wait for the next batch.
    pub async fn next_batch(&self, size_limit: usize) -> Batch {
        std::future::poll_fn(|context| self.poll_batch(context, size_limit)).await
    }
}

#[cfg(test)]
mod tests;
