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

/// What the connection should do next with its request body.
#[derive(Debug, PartialEq, Eq)]
pub enum Batch {
    /// Encoded frames to write.
    Frames(Vec<u8>),
    /// The client is closing and every control message has been taken: finish the request body.
    Closed,
}

struct State {
    /// Keepalive messages, sent before anything else so that queued audio never delays them.
    keepalive: VecDeque<ClientMessage>,
    control: VecDeque<ClientMessage>,
    audio: VecDeque<AudioFrame>,
    audio_limit: usize,
    membership: Membership,
    connected: bool,
    closed: bool,
    /// Alternates between control and audio when both are waiting, so neither starves the other.
    prefer_audio: bool,
    dropped_audio: u64,
    waker: Option<Waker>,
}

/// Outgoing traffic shared between application threads and the connection.
///
/// Control messages are never dropped: while disconnected they wait for the next connection, except that membership changes (`Join`, `Leave`, `Gate`) are folded into the [`Membership`] restored after Hello, and stale `Presence` is discarded. Live audio is real-time: it is dropped while disconnected, and when the link is congested the oldest queued frames are dropped beyond the limit instead of buffering without bound.
pub struct Outbox {
    state: Mutex<State>,
}

impl Outbox {
    pub fn new(audio_limit: usize) -> Self {
        Self {
            state: Mutex::new(State {
                keepalive: VecDeque::new(),
                control: VecDeque::new(),
                audio: VecDeque::new(),
                audio_limit: audio_limit.max(1),
                membership: Membership::default(),
                connected: false,
                closed: false,
                prefer_audio: false,
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

    pub fn send(&self, message: ClientMessage) -> Result<(), SendError> {
        let mut state = self.lock();
        if state.closed {
            return Err(SendError::Closed);
        }
        // Messages superseded by the restored membership, or too stale to deliver later, are not queued while disconnected.
        let deferred = match &message {
            ClientMessage::Hello { .. }
            | ClientMessage::Ping { .. }
            | ClientMessage::Pong { .. } => return Err(SendError::Reserved),
            ClientMessage::Join { space } => {
                state.membership.space = Some(space.clone());
                true
            }
            ClientMessage::Leave => {
                state.membership.space = None;
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
            return Ok(());
        }
        if let ClientMessage::Presence { .. } = message {
            // Only the latest pose matters, so a congested link does not accumulate presence updates.
            if let Some(queued) = state
                .control
                .iter_mut()
                .find(|queued| matches!(queued, ClientMessage::Presence { .. }))
            {
                *queued = message;
                return Ok(());
            }
        }
        state.control.push_back(message);
        Self::wake(state);
        Ok(())
    }

    pub fn send_audio(&self, frame: AudioFrame) -> Result<(), SendError> {
        let mut state = self.lock();
        if state.closed {
            return Err(SendError::Closed);
        }
        if frame.gate == Gate::Off {
            return Ok(());
        }
        if !state.connected {
            state.dropped_audio += 1;
            return Ok(());
        }
        state.audio.push_back(frame);
        while state.audio.len() > state.audio_limit {
            state.audio.pop_front();
            state.dropped_audio += 1;
        }
        Self::wake(state);
        Ok(())
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
        let pending = state
            .control
            .iter()
            .any(|message| matches!(message, ClientMessage::Join { .. } | ClientMessage::Leave));
        if !pending {
            state.membership.space = space;
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
    pub fn connect(&self) -> Vec<ClientMessage> {
        let mut state = self.lock();
        state.connected = true;
        state.audio.clear();
        state.control.retain(|message| {
            !matches!(
                message,
                ClientMessage::Join { .. }
                    | ClientMessage::Leave
                    | ClientMessage::Gate { .. }
                    | ClientMessage::Presence { .. }
            )
        });
        state.membership.messages()
    }

    /// The connection ended. Unsent control messages wait for the next one; queued audio is discarded.
    pub fn disconnect(&self) {
        let mut state = self.lock();
        state.connected = false;
        state.keepalive.clear();
        state.dropped_audio += state.audio.len() as u64;
        state.audio.clear();
        state.waker = None;
    }

    /// Take up to roughly `size_limit` bytes of encoded frames, keepalive first, then alternating control and audio, or wait for some.
    pub fn poll_batch(&self, context: &mut Context<'_>, size_limit: usize) -> Poll<Batch> {
        let mut state = self.lock();
        let mut output = Vec::new();
        while let Some(message) = state.keepalive.pop_front() {
            Frame::client(&message).encode_into(&mut output);
        }
        while output.len() < size_limit {
            let audio = if state.prefer_audio {
                !state.audio.is_empty()
            } else {
                state.control.is_empty() && !state.audio.is_empty()
            };
            if audio {
                let frame = state.audio.pop_front().expect("audio is waiting");
                Frame::audio(&frame).encode_into(&mut output);
                state.prefer_audio = false;
            } else if let Some(message) = state.control.pop_front() {
                Frame::client(&message).encode_into(&mut output);
                state.prefer_audio = true;
            } else {
                break;
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
