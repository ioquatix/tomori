// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

use std::collections::VecDeque;
use std::sync::Mutex;

use event_listener::{Event, EventListener};
use protocol::ServerMessage;

/// Participant audio frames waiting for one connection. Older frames are dropped beyond this, so playback stays real time.
const AUDIO_LIMIT: usize = 32;

/// Agent speech frames waiting for one connection: one second at 20 ms per frame.
///
/// The space sends agent speech at its playback rate, so only a stalled connection reaches this. Frames beyond it could no longer play in real time, so the oldest are dropped, which the client sees as a sequence gap.
const AGENT_AUDIO_LIMIT: usize = 50;

/// Error replies waiting for one connection. Further errors are dropped until these are sent, so a client that repeats an invalid request faster than it reads cannot grow the queue.
const ERROR_LIMIT: usize = 8;

/// Control items waiting for one connection. Presence and partial transcripts replace their queued predecessors and errors are capped, so only a client that keeps making requests without reading the replies reaches this; its connection is closed rather than queueing without bound.
const CONTROL_LIMIT: usize = 1024;

/// Something to send on one connection, in order.
#[derive(Debug)]
pub enum Outgoing {
    Message(ServerMessage),
    /// The current state of the participant's space, rendered when sent so that changes coalesce.
    Space,
    /// The current lobby listing, rendered when sent.
    Spaces,
}

#[derive(Debug)]
pub enum Popped {
    Control(Outgoing),
    /// An encoded audio frame.
    Audio(Vec<u8>),
    Empty,
    Closed,
}

/// An item of the agent speech stream: an announcement or an encoded frame, kept in order.
#[derive(Debug)]
enum Agent {
    Message(ServerMessage),
    Audio(Vec<u8>),
}

/// The outbound queue of one connection. Producers never block; the connection body drains it.
///
/// Keepalive messages go first, then control messages, then the agent speech stream, then participant audio. The agent speech stream has its own queue so that participant audio never displaces it; see `design/live-audio.md`.
#[derive(Default)]
pub struct Outbox {
    inner: Mutex<Inner>,
    ready: Event,
}

#[derive(Default)]
struct Inner {
    /// `Ping` and `Pong`, which must not wait behind anything else.
    keepalive: VecDeque<ServerMessage>,
    control: VecDeque<Outgoing>,
    agent: VecDeque<Agent>,
    agent_frames: usize,
    audio: VecDeque<Vec<u8>>,
    space_pending: bool,
    spaces_pending: bool,
    /// Queued [`ServerMessage::Error`] replies.
    errors: usize,
    closed: bool,
}

impl Inner {
    /// Replace a queued message that `message` supersedes, returning false if there is none: a relayed pose from the same source, or a partial transcript of the same utterance.
    fn supersede(&mut self, message: &mut ServerMessage) -> bool {
        let slot = self.control.iter_mut().find_map(|item| match item {
            Outgoing::Message(queued) if supersedes(message, queued) => Some(queued),
            _ => None,
        });
        match slot {
            Some(queued) => {
                std::mem::swap(queued, message);
                true
            }
            None => false,
        }
    }
}

/// Whether `newer` makes `queued` pointless to send.
fn supersedes(newer: &ServerMessage, queued: &ServerMessage) -> bool {
    match (newer, queued) {
        (
            ServerMessage::Presence { source, .. },
            ServerMessage::Presence {
                source: queued_source,
                ..
            },
        ) => source == queued_source,
        (
            ServerMessage::Transcript {
                utterance,
                finished: false,
                ..
            },
            ServerMessage::Transcript {
                utterance: queued_utterance,
                finished: false,
                ..
            },
        ) => utterance == queued_utterance,
        _ => false,
    }
}

impl Outbox {
    pub fn new() -> Self {
        Self::default()
    }

    fn update(&self, change: impl FnOnce(&mut Inner) -> bool) {
        let notify = {
            let mut inner = self.inner.lock().unwrap();
            !inner.closed && change(&mut inner)
        };
        if notify {
            self.ready.notify(usize::MAX);
        }
    }

    /// A control message, in order with the others. A relayed pose replaces a queued pose from the same source, and a partial transcript a queued partial transcript of the same utterance, keeping its place; errors beyond [`ERROR_LIMIT`] are dropped. See `design/server.md`.
    pub fn message(&self, mut message: ServerMessage) {
        self.update(|inner| {
            if inner.supersede(&mut message) {
                return true;
            }
            if matches!(message, ServerMessage::Error { .. }) {
                if inner.errors == ERROR_LIMIT {
                    log::debug!(
                        "An error reply was dropped because the client is not reading them."
                    );
                    return false;
                }
                inner.errors += 1;
            }
            if inner.control.len() == CONTROL_LIMIT {
                log::warn!("Closing a connection whose client is not reading control messages.");
                inner.closed = true;
                return true;
            }
            inner.control.push_back(Outgoing::Message(message));
            true
        });
    }

    /// A `Ping` or `Pong`, sent before everything already queued.
    pub fn keepalive(&self, message: ServerMessage) {
        debug_assert!(matches!(
            message,
            ServerMessage::Ping { .. } | ServerMessage::Pong { .. }
        ));
        self.update(|inner| {
            inner.keepalive.push_back(message);
            true
        });
    }

    /// Send the space state soon. Repeated changes before it is sent produce one snapshot.
    pub fn space(&self) {
        self.update(|inner| {
            if inner.space_pending {
                return false;
            }
            inner.space_pending = true;
            inner.control.push_back(Outgoing::Space);
            true
        });
    }

    /// Send the lobby listing soon. Repeated changes before it is sent produce one listing.
    pub fn spaces(&self) {
        self.update(|inner| {
            if inner.spaces_pending {
                return false;
            }
            inner.spaces_pending = true;
            inner.control.push_back(Outgoing::Spaces);
            true
        });
    }

    /// A participant's audio frame.
    pub fn audio(&self, frame: Vec<u8>) {
        self.update(|inner| {
            if inner.audio.len() == AUDIO_LIMIT {
                inner.audio.pop_front();
            }
            inner.audio.push_back(frame);
            true
        });
    }

    /// An announcement in the agent speech stream, sent after the agent frames already queued.
    pub fn agent_message(&self, message: ServerMessage) {
        self.update(|inner| {
            inner.agent.push_back(Agent::Message(message));
            true
        });
    }

    /// An agent speech frame.
    pub fn agent_audio(&self, frame: Vec<u8>) {
        self.update(|inner| {
            if inner.agent_frames == AGENT_AUDIO_LIMIT
                && let Some(index) = inner
                    .agent
                    .iter()
                    .position(|item| matches!(item, Agent::Audio(_)))
            {
                inner.agent.remove(index);
                inner.agent_frames -= 1;
            }
            inner.agent.push_back(Agent::Audio(frame));
            inner.agent_frames += 1;
            true
        });
    }

    /// Discard queued audio and agent speech, such as when the participant moves to another space.
    pub fn clear_audio(&self) {
        let mut inner = self.inner.lock().unwrap();
        inner.audio.clear();
        inner.agent.clear();
        inner.agent_frames = 0;
    }

    /// Stop the connection after the frames already queued.
    pub fn close(&self) {
        self.inner.lock().unwrap().closed = true;
        self.ready.notify(usize::MAX);
    }

    /// The next item: keepalive first, then control messages, then agent speech, then participant audio.
    pub fn pop(&self) -> Popped {
        let mut inner = self.inner.lock().unwrap();
        if let Some(message) = inner.keepalive.pop_front() {
            return Popped::Control(Outgoing::Message(message));
        }
        if let Some(item) = inner.control.pop_front() {
            match item {
                Outgoing::Space => inner.space_pending = false,
                Outgoing::Spaces => inner.spaces_pending = false,
                Outgoing::Message(ServerMessage::Error { .. }) => inner.errors -= 1,
                Outgoing::Message(_) => {}
            }
            return Popped::Control(item);
        }
        match inner.agent.pop_front() {
            Some(Agent::Message(message)) => return Popped::Control(Outgoing::Message(message)),
            Some(Agent::Audio(frame)) => {
                inner.agent_frames -= 1;
                return Popped::Audio(frame);
            }
            None => {}
        }
        if let Some(frame) = inner.audio.pop_front() {
            return Popped::Audio(frame);
        }
        if inner.closed {
            Popped::Closed
        } else {
            Popped::Empty
        }
    }

    pub fn is_ready(&self) -> bool {
        let inner = self.inner.lock().unwrap();
        inner.closed
            || !inner.keepalive.is_empty()
            || !inner.control.is_empty()
            || !inner.agent.is_empty()
            || !inner.audio.is_empty()
    }

    /// Wait for the outbox to change. Check [`is_ready`](Self::is_ready) after creating the listener to avoid a lost wakeup.
    pub fn listen(&self) -> EventListener {
        self.ready.listen()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn speech(segment: u32) -> ServerMessage {
        ServerMessage::Speech {
            segment,
            message: "m".into(),
            text: "Hello.".into(),
            sample_rate: 24_000,
        }
    }

    #[test]
    fn snapshots_coalesce_and_control_precedes_audio() {
        let outbox = Outbox::new();
        outbox.audio(vec![1]);
        outbox.space();
        outbox.space();
        outbox.message(ServerMessage::Left);
        assert!(matches!(outbox.pop(), Popped::Control(Outgoing::Space)));
        assert!(matches!(
            outbox.pop(),
            Popped::Control(Outgoing::Message(ServerMessage::Left))
        ));
        assert!(matches!(outbox.pop(), Popped::Audio(_)));
        assert!(matches!(outbox.pop(), Popped::Empty));
        outbox.space();
        assert!(matches!(outbox.pop(), Popped::Control(Outgoing::Space)));
        outbox.close();
        assert!(matches!(outbox.pop(), Popped::Closed));
    }

    #[test]
    fn keepalive_precedes_queued_control_and_audio() {
        let outbox = Outbox::new();
        for index in 0..AUDIO_LIMIT {
            outbox.audio(vec![index as u8]);
        }
        outbox.agent_audio(vec![100]);
        outbox.message(ServerMessage::Left);
        outbox.keepalive(ServerMessage::Pong { nonce: 1 });
        outbox.keepalive(ServerMessage::Ping { nonce: 2 });
        assert!(matches!(
            outbox.pop(),
            Popped::Control(Outgoing::Message(ServerMessage::Pong { nonce: 1 }))
        ));
        assert!(matches!(
            outbox.pop(),
            Popped::Control(Outgoing::Message(ServerMessage::Ping { nonce: 2 }))
        ));
        assert!(matches!(
            outbox.pop(),
            Popped::Control(Outgoing::Message(ServerMessage::Left))
        ));
    }

    fn presence(source: u32, x: f32) -> ServerMessage {
        ServerMessage::Presence {
            source,
            head: protocol::Pose {
                position: [x, 0.0, 0.0],
                orientation: [0.0, 0.0, 0.0, 1.0],
            },
            left: None,
            right: None,
        }
    }

    fn partial(utterance: &str, text: &str) -> ServerMessage {
        ServerMessage::Transcript {
            utterance: utterance.into(),
            text: text.into(),
            finished: false,
        }
    }

    fn drain(outbox: &Outbox) -> Vec<ServerMessage> {
        let mut messages = Vec::new();
        while let Popped::Control(Outgoing::Message(message)) = outbox.pop() {
            messages.push(message);
        }
        messages
    }

    #[test]
    fn presence_and_partial_transcripts_keep_only_the_latest() {
        let outbox = Outbox::new();
        for step in 0..1000 {
            outbox.message(presence(1, step as f32));
            outbox.message(presence(2, -(step as f32)));
            outbox.message(partial("u", &format!("text {step}")));
        }
        outbox.message(ServerMessage::Transcript {
            utterance: "u".into(),
            text: "done".into(),
            finished: true,
        });
        assert_eq!(
            drain(&outbox),
            vec![
                presence(1, 999.0),
                presence(2, -999.0),
                partial("u", "text 999"),
                ServerMessage::Transcript {
                    utterance: "u".into(),
                    text: "done".into(),
                    finished: true,
                },
            ]
        );
    }

    #[test]
    fn errors_are_capped_until_they_are_sent() {
        let outbox = Outbox::new();
        for index in 0..100 {
            outbox.message(ServerMessage::Error {
                error: format!("error {index}"),
                space: None,
            });
        }
        outbox.message(ServerMessage::Left);
        let messages = drain(&outbox);
        assert_eq!(messages.len(), ERROR_LIMIT + 1);
        assert_eq!(messages.last(), Some(&ServerMessage::Left));
        // Once sent, errors are reported again.
        outbox.message(ServerMessage::Error {
            error: "again".into(),
            space: None,
        });
        assert_eq!(drain(&outbox).len(), 1);
    }

    #[test]
    fn an_unread_control_queue_closes_the_connection() {
        let outbox = Outbox::new();
        for _ in 0..CONTROL_LIMIT * 2 {
            outbox.message(ServerMessage::Left);
        }
        assert_eq!(drain(&outbox).len(), CONTROL_LIMIT);
        assert!(matches!(outbox.pop(), Popped::Closed));
    }

    #[test]
    fn audio_drops_the_oldest_frames() {
        let outbox = Outbox::new();
        for index in 0..AUDIO_LIMIT + 2 {
            outbox.audio(vec![index as u8]);
        }
        let Popped::Audio(first) = outbox.pop() else {
            panic!("expected audio");
        };
        assert_eq!(first, vec![2]);
    }

    #[test]
    fn participant_audio_never_displaces_agent_speech() {
        let outbox = Outbox::new();
        outbox.agent_message(speech(1));
        outbox.agent_audio(vec![100]);
        for index in 0..AUDIO_LIMIT * 2 {
            outbox.audio(vec![index as u8]);
        }
        outbox.agent_audio(vec![101]);
        outbox.agent_message(speech(2));
        outbox.agent_audio(vec![102]);
        assert!(matches!(
            outbox.pop(),
            Popped::Control(Outgoing::Message(ServerMessage::Speech { segment: 1, .. }))
        ));
        assert!(matches!(outbox.pop(), Popped::Audio(frame) if frame == [100]));
        assert!(matches!(outbox.pop(), Popped::Audio(frame) if frame == [101]));
        // An announcement stays behind the frames of the previous segment.
        assert!(matches!(
            outbox.pop(),
            Popped::Control(Outgoing::Message(ServerMessage::Speech { segment: 2, .. }))
        ));
        assert!(matches!(outbox.pop(), Popped::Audio(frame) if frame == [102]));
        assert!(matches!(outbox.pop(), Popped::Audio(frame) if frame == [AUDIO_LIMIT as u8]));
    }

    #[test]
    fn a_stalled_connection_drops_only_stale_agent_frames() {
        let outbox = Outbox::new();
        outbox.agent_message(speech(1));
        for index in 0..AGENT_AUDIO_LIMIT + 3 {
            outbox.agent_audio(vec![index as u8]);
        }
        assert!(matches!(
            outbox.pop(),
            Popped::Control(Outgoing::Message(ServerMessage::Speech { segment: 1, .. }))
        ));
        assert!(matches!(outbox.pop(), Popped::Audio(frame) if frame == [3]));
        outbox.clear_audio();
        assert!(matches!(outbox.pop(), Popped::Empty));
    }
}
