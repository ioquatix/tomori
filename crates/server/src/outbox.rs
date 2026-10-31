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
/// Control messages go first, then the agent speech stream, then participant audio. The agent speech stream has its own queue so that participant audio never displaces it; see `design/live-audio.md`.
#[derive(Default)]
pub struct Outbox {
    inner: Mutex<Inner>,
    ready: Event,
}

#[derive(Default)]
struct Inner {
    control: VecDeque<Outgoing>,
    agent: VecDeque<Agent>,
    agent_frames: usize,
    audio: VecDeque<Vec<u8>>,
    space_pending: bool,
    spaces_pending: bool,
    closed: bool,
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

    pub fn message(&self, message: ServerMessage) {
        self.update(|inner| {
            inner.control.push_back(Outgoing::Message(message));
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

    /// The next item: control messages first, then agent speech, then participant audio.
    pub fn pop(&self) -> Popped {
        let mut inner = self.inner.lock().unwrap();
        if let Some(item) = inner.control.pop_front() {
            match item {
                Outgoing::Space => inner.space_pending = false,
                Outgoing::Spaces => inner.spaces_pending = false,
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
