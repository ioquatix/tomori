// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

use std::collections::VecDeque;
use std::sync::Mutex;

use event_listener::{Event, EventListener};
use protocol::ServerMessage;

/// Audio frames waiting for one connection. Older frames are dropped beyond this, so playback stays real time.
const AUDIO_LIMIT: usize = 32;

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

/// The outbound queue of one connection. Producers never block; the connection body drains it.
#[derive(Default)]
pub struct Outbox {
    inner: Mutex<Inner>,
    ready: Event,
}

#[derive(Default)]
struct Inner {
    control: VecDeque<Outgoing>,
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

    pub fn audio(&self, frame: Vec<u8>) {
        self.update(|inner| {
            if inner.audio.len() == AUDIO_LIMIT {
                inner.audio.pop_front();
            }
            inner.audio.push_back(frame);
            true
        });
    }

    /// Discard queued audio, such as when the participant moves to another space.
    pub fn clear_audio(&self) {
        self.inner.lock().unwrap().audio.clear();
    }

    /// Stop the connection after the frames already queued.
    pub fn close(&self) {
        self.inner.lock().unwrap().closed = true;
        self.ready.notify(usize::MAX);
    }

    /// The next item: control messages first, then audio.
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
        inner.closed || !inner.control.is_empty() || !inner.audio.is_empty()
    }

    /// Wait for the outbox to change. Check [`is_ready`](Self::is_ready) after creating the listener to avoid a lost wakeup.
    pub fn listen(&self) -> EventListener {
        self.ready.listen()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
