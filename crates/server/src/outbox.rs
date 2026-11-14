// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use event_listener::{Event, EventListener};
use tomori_protocol::{Frame, ImageChunk, ServerMessage};

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

/// Images waiting to be sent on one connection, that its client asked for: further requests are refused until these
/// are sent. A client asks only for images its space shows that it does not have, at most a few dozen.
const IMAGE_LIMIT: usize = 32;

/// Something to send on one connection, in order.
#[derive(Debug)]
pub enum Outgoing {
    Message(ServerMessage),
    /// The current state of the participant's space, rendered when sent so that changes coalesce.
    Space,
    /// The current lobby listing, rendered when sent.
    Spaces,
    /// The cards of the participant's space, rendered when sent, so that a burst of changes, such as several cards
    /// placed, sends one. See `design/github.md`.
    Cards,
    /// What is projected in the participant's space, rendered when sent, so that a burst of changes sends one. See
    /// `design/projection.md`.
    Projection,
}

#[derive(Debug)]
pub enum Popped {
    Control(Outgoing),
    /// An encoded audio frame.
    Audio(Vec<u8>),
    /// An encoded [`tomori_protocol::FrameKind::Image`] frame: part of an image.
    Image(Vec<u8>),
    Empty,
    Closed,
}

/// An item of the agent speech stream: an announcement or an encoded frame, kept in order.
#[derive(Debug)]
enum Agent {
    Message(ServerMessage),
    Audio(Vec<u8>),
}

/// An image being sent, from `offset`, by its SHA-256.
struct Transfer {
    hash: [u8; 32],
    png: Arc<[u8]>,
    offset: usize,
}

/// The outbound queue of one connection. Producers never block; the connection body drains it.
///
/// Keepalive messages go first, then control messages, then the agent speech stream, then participant audio, then
/// images, one chunk at a time, so that an image never holds up anything else for longer than a chunk takes to send. A
/// projection, which may be sent whole in one long frame, waits behind the audio already queued, agent speech and
/// participant audio both, so that sending it holds up only audio queued after it. See `design/server.md`. The agent speech stream has its own queue so that participant audio never displaces it; see `design/live-audio.md`.
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
    images: VecDeque<Transfer>,
    space_pending: bool,
    spaces_pending: bool,
    cards_pending: bool,
    projection_pending: bool,
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
            // A newer state of an environment makes one still queued pointless, and the events queued since already
            // happened to it: the newer one goes at the end, after them, so that nothing is applied twice.
            if let ServerMessage::EnvironmentState { environment, .. } = &message {
                inner.control.retain(|item| {
                    !matches!(
                        item,
                        Outgoing::Message(ServerMessage::EnvironmentState { environment: queued, .. })
                            if queued == environment
                    )
                });
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

    /// A `Ping` with `nonce`, sent before everything already queued.
    pub fn ping(&self, nonce: u64) {
        self.keepalive(ServerMessage::Ping { nonce });
    }

    /// A `Pong` echoing `nonce`, sent before everything already queued.
    pub fn pong(&self, nonce: u64) {
        self.keepalive(ServerMessage::Pong { nonce });
    }

    fn keepalive(&self, message: ServerMessage) {
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

    /// Send the cards of the participant's space soon. Repeated changes before they are sent produce one message.
    pub fn cards(&self) {
        self.update(|inner| {
            if inner.cards_pending {
                return false;
            }
            inner.cards_pending = true;
            inner.control.push_back(Outgoing::Cards);
            true
        });
    }

    /// Send what is projected in the participant's space soon. Repeated changes before it is sent produce one message.
    pub fn projection(&self) {
        self.update(|inner| {
            if inner.projection_pending {
                return false;
            }
            inner.projection_pending = true;
            inner.control.push_back(Outgoing::Projection);
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

    /// Send the image `png`, whose SHA-256 is `hash`, in chunks after everything else. Returns false, queueing nothing,
    /// if it is already queued, or [`IMAGE_LIMIT`] images are.
    pub fn image(&self, hash: [u8; 32], png: Arc<[u8]>) -> bool {
        let mut queued = false;
        self.update(|inner| {
            if inner.images.len() == IMAGE_LIMIT
                || inner.images.iter().any(|transfer| transfer.hash == hash)
            {
                return false;
            }
            inner.images.push_back(Transfer {
                hash,
                png,
                offset: 0,
            });
            queued = true;
            true
        });
        queued
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

    /// The next item: keepalive first, then control messages, then agent speech, then participant audio, except that a
    /// projection waits behind the audio already queued.
    pub fn pop(&self) -> Popped {
        let mut inner = self.inner.lock().unwrap();
        if let Some(message) = inner.keepalive.pop_front() {
            return Popped::Control(Outgoing::Message(message));
        }
        let audio_first = matches!(inner.control.front(), Some(Outgoing::Projection))
            && !(inner.agent.is_empty() && inner.audio.is_empty());
        if !audio_first && let Some(item) = inner.control.pop_front() {
            match item {
                Outgoing::Space => inner.space_pending = false,
                Outgoing::Spaces => inner.spaces_pending = false,
                Outgoing::Cards => inner.cards_pending = false,
                Outgoing::Projection => inner.projection_pending = false,
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
        if let Some(transfer) = inner.images.front_mut() {
            let end =
                (transfer.offset + tomori_protocol::IMAGE_CHUNK_BYTES).min(transfer.png.len());
            let chunk = ImageChunk::new(
                transfer.hash,
                transfer.offset as u32,
                transfer.png.len() as u32,
                transfer.png[transfer.offset..end].to_vec(),
            );
            transfer.offset = end;
            if end == transfer.png.len() {
                inner.images.pop_front();
            }
            return Popped::Image(Frame::image(&chunk).encode());
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
            || !inner.images.is_empty()
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
            replay: false,
        }
    }

    #[test]
    fn a_newer_environment_state_replaces_one_still_queued_after_the_events_since() {
        let outbox = Outbox::new();
        let payload = |value: u32| tomori_protocol::Payload::new(serde_json::json!(value));
        let state = |environment: &str, value| ServerMessage::EnvironmentState {
            environment: environment.into(),
            version: 1,
            state: payload(value),
        };
        let event = ServerMessage::EnvironmentEvent {
            environment: "garden".into(),
            version: 1,
            event: payload(2),
        };
        outbox.message(state("garden", 1));
        outbox.message(state("other", 1));
        outbox.message(event.clone());
        outbox.message(state("garden", 3));
        let popped: Vec<ServerMessage> = std::iter::from_fn(|| match outbox.pop() {
            Popped::Control(Outgoing::Message(message)) => Some(message),
            _ => None,
        })
        .collect();
        assert_eq!(popped, [state("other", 1), event, state("garden", 3)]);
    }

    #[test]
    fn images_go_in_chunks_after_everything_else_once_each_and_bounded() {
        let outbox = Outbox::new();
        let png: Arc<[u8]> = (0..tomori_protocol::IMAGE_CHUNK_BYTES * 2 + 1)
            .map(|index| index as u8)
            .collect();
        assert!(outbox.image([1; 32], png.clone()));
        // Asked for again while it is queued, it is not queued twice.
        assert!(!outbox.image([1; 32], png.clone()));
        assert!(outbox.is_ready());
        outbox.audio(vec![9]);
        outbox.message(ServerMessage::LEFT);
        let Popped::Control(_) = outbox.pop() else {
            unreachable!("control first");
        };
        let Popped::Audio(_) = outbox.pop() else {
            unreachable!("audio before images");
        };
        let mut joined = Vec::new();
        let mut chunks = 0;
        while let Popped::Image(bytes) = outbox.pop() {
            let mut decoder = tomori_protocol::FrameDecoder::new();
            decoder.push(&bytes);
            let chunk = decoder.next_frame().unwrap().unwrap().to_image().unwrap();
            assert_eq!((chunk.hash, chunk.offset as usize), ([1; 32], joined.len()));
            joined.extend(chunk.data);
            chunks += 1;
            // Speech that arrives part way through an image goes before the rest of it.
            if chunks == 1 {
                outbox.agent_audio(vec![7]);
                let Popped::Audio(frame) = outbox.pop() else {
                    unreachable!("speech before the rest of the image");
                };
                assert_eq!(frame, [7]);
            }
        }
        assert_eq!((chunks, &joined[..]), (3, &png[..]));
        assert!(!outbox.is_ready());
        // Sent, it may be asked for again; beyond the limit, more are refused.
        assert!(outbox.image([1; 32], png.clone()));
        for index in 2..=IMAGE_LIMIT as u8 {
            assert!(outbox.image([index; 32], png.clone()));
        }
        assert!(!outbox.image([0; 32], png));
    }

    #[test]
    fn snapshots_coalesce_and_control_precedes_audio() {
        let outbox = Outbox::new();
        outbox.audio(vec![1]);
        outbox.space();
        outbox.space();
        outbox.message(ServerMessage::LEFT);
        let Popped::Control(Outgoing::Space) = outbox.pop() else {
            unreachable!("expected Popped::Control(Outgoing::Space)");
        };
        let Popped::Control(Outgoing::Message(ServerMessage::Left { .. })) = outbox.pop() else {
            unreachable!(
                "expected Popped::Control(Outgoing::Message(ServerMessage::Left {{ .. }}))"
            );
        };
        let Popped::Audio(_) = outbox.pop() else {
            unreachable!("expected Popped::Audio(_)");
        };
        let Popped::Empty = outbox.pop() else {
            unreachable!("expected Popped::Empty");
        };
        outbox.space();
        let Popped::Control(Outgoing::Space) = outbox.pop() else {
            unreachable!("expected Popped::Control(Outgoing::Space)");
        };
        // Cards coalesce the same way.
        outbox.cards();
        outbox.cards();
        let Popped::Control(Outgoing::Cards) = outbox.pop() else {
            unreachable!("expected Popped::Control(Outgoing::Cards)");
        };
        let Popped::Empty = outbox.pop() else {
            unreachable!("expected Popped::Empty");
        };
        outbox.cards();
        let Popped::Control(Outgoing::Cards) = outbox.pop() else {
            unreachable!("expected Popped::Control(Outgoing::Cards)");
        };
        // And so does what is projected.
        outbox.projection();
        outbox.projection();
        let Popped::Control(Outgoing::Projection) = outbox.pop() else {
            unreachable!("expected Popped::Control(Outgoing::Projection)");
        };
        let Popped::Empty = outbox.pop() else {
            unreachable!("expected Popped::Empty");
        };
        outbox.projection();
        let Popped::Control(Outgoing::Projection) = outbox.pop() else {
            unreachable!("expected Popped::Control(Outgoing::Projection)");
        };
        outbox.close();
        let Popped::Closed = outbox.pop() else {
            unreachable!("expected Popped::Closed");
        };
    }

    #[test]
    fn a_projection_waits_behind_the_audio_already_queued_and_nothing_else_does() {
        let outbox = Outbox::new();
        let popped = |outbox: &Outbox| match outbox.pop() {
            Popped::Control(Outgoing::Projection) => "projection",
            Popped::Control(Outgoing::Message(ServerMessage::Left { .. })) => "left",
            Popped::Control(Outgoing::Message(ServerMessage::Speech { .. })) => "speech",
            Popped::Audio(frame) if frame == [1] => "agent",
            Popped::Audio(frame) if frame == [2] => "participant",
            Popped::Empty => "empty",
            _ => unreachable!("only what the test queued"),
        };
        // Agent speech and participant audio queued, then the projection and a message: the projection, which may be
        // one long frame, goes after the audio, and what was queued after it after it, in order.
        outbox.agent_message(speech(1));
        outbox.agent_audio(vec![1]);
        outbox.audio(vec![2]);
        outbox.projection();
        outbox.message(ServerMessage::LEFT);
        let order: Vec<&str> = (0..6).map(|_| popped(&outbox)).collect();
        assert_eq!(
            order,
            [
                "speech",
                "agent",
                "participant",
                "projection",
                "left",
                "empty"
            ]
        );
        // Any other control message goes before audio, as ever.
        outbox.agent_audio(vec![1]);
        outbox.message(ServerMessage::LEFT);
        outbox.projection();
        let order: Vec<&str> = (0..4).map(|_| popped(&outbox)).collect();
        assert_eq!(order, ["left", "agent", "projection", "empty"]);
        // With nothing of audio queued, the projection goes at once, and audio queued after it is sent after it.
        outbox.projection();
        assert_eq!(popped(&outbox), "projection");
        outbox.agent_audio(vec![1]);
        assert_eq!(popped(&outbox), "agent");
    }

    #[test]
    fn keepalive_precedes_queued_control_and_audio() {
        let outbox = Outbox::new();
        for index in 0..AUDIO_LIMIT {
            outbox.audio(vec![index as u8]);
        }
        outbox.agent_audio(vec![100]);
        outbox.message(ServerMessage::LEFT);
        outbox.pong(1);
        outbox.ping(2);
        let Popped::Control(Outgoing::Message(ServerMessage::Pong { nonce: 1 })) = outbox.pop()
        else {
            unreachable!(
                "expected Popped::Control(Outgoing::Message(ServerMessage::Pong {{ nonce: 1 }}))"
            );
        };
        let Popped::Control(Outgoing::Message(ServerMessage::Ping { nonce: 2 })) = outbox.pop()
        else {
            unreachable!(
                "expected Popped::Control(Outgoing::Message(ServerMessage::Ping {{ nonce: 2 }}))"
            );
        };
        let Popped::Control(Outgoing::Message(ServerMessage::Left { .. })) = outbox.pop() else {
            unreachable!(
                "expected Popped::Control(Outgoing::Message(ServerMessage::Left {{ .. }}))"
            );
        };
    }

    fn presence(source: u32, x: f32) -> ServerMessage {
        ServerMessage::Presence {
            source,
            head: tomori_protocol::Pose {
                position: [x, 0.0, 0.0],
                orientation: [0.0, 0.0, 0.0, 1.0],
            },
            left: None,
            right: None,
            hands: None,
            pointing: None,
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
        crate::test_log::record();
        let outbox = Outbox::new();
        for index in 0..100 {
            outbox.message(ServerMessage::Error {
                error: format!("error {index}"),
                space: None,
                panel: None,
                wall: None,
            });
        }
        outbox.message(ServerMessage::LEFT);
        let messages = drain(&outbox);
        assert_eq!(messages.len(), ERROR_LIMIT + 1);
        assert_eq!(messages.last(), Some(&ServerMessage::LEFT));
        // Once sent, errors are reported again.
        outbox.message(ServerMessage::Error {
            error: "again".into(),
            space: None,
            panel: None,
            wall: None,
        });
        assert_eq!(drain(&outbox).len(), 1);
        assert!(!crate::test_log::logged("An error reply was dropped").is_empty());
    }

    #[test]
    fn an_unread_control_queue_closes_the_connection() {
        crate::test_log::record();
        let outbox = Outbox::new();
        for _ in 0..CONTROL_LIMIT * 2 {
            outbox.message(ServerMessage::LEFT);
        }
        assert_eq!(drain(&outbox).len(), CONTROL_LIMIT);
        let Popped::Closed = outbox.pop() else {
            unreachable!("expected Popped::Closed");
        };
        assert!(!crate::test_log::logged("client is not reading control messages").is_empty());
    }

    #[test]
    fn audio_drops_the_oldest_frames() {
        let outbox = Outbox::new();
        for index in 0..AUDIO_LIMIT + 2 {
            outbox.audio(vec![index as u8]);
        }
        let Popped::Audio(first) = outbox.pop() else {
            unreachable!("expected audio");
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
        let Popped::Control(Outgoing::Message(ServerMessage::Speech { segment: 1, .. })) =
            outbox.pop()
        else {
            unreachable!(
                "expected Popped::Control(Outgoing::Message(ServerMessage::Speech {{ segment: 1, .. }}))"
            );
        };
        assert!(matches!(outbox.pop(), Popped::Audio(frame) if frame == [100]));
        assert!(matches!(outbox.pop(), Popped::Audio(frame) if frame == [101]));
        // An announcement stays behind the frames of the previous segment.
        let Popped::Control(Outgoing::Message(ServerMessage::Speech { segment: 2, .. })) =
            outbox.pop()
        else {
            unreachable!(
                "expected Popped::Control(Outgoing::Message(ServerMessage::Speech {{ segment: 2, .. }}))"
            );
        };
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
        let Popped::Control(Outgoing::Message(ServerMessage::Speech { segment: 1, .. })) =
            outbox.pop()
        else {
            unreachable!(
                "expected Popped::Control(Outgoing::Message(ServerMessage::Speech {{ segment: 1, .. }}))"
            );
        };
        assert!(matches!(outbox.pop(), Popped::Audio(frame) if frame == [3]));
        outbox.clear_audio();
        let Popped::Empty = outbox.pop() else {
            unreachable!("expected Popped::Empty");
        };
    }
}
