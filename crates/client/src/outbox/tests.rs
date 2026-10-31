// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

use super::*;
use protocol::{FrameDecoder, FrameKind, Pose};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::task::Wake;

#[derive(Debug, PartialEq)]
enum Item {
    Control(ClientMessage),
    Audio(u32),
}

struct Counter(AtomicUsize);

impl Wake for Counter {
    fn wake(self: Arc<Self>) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}

fn poll(outbox: &Outbox, size_limit: usize) -> Poll<Batch> {
    let waker = Waker::from(Arc::new(Counter(AtomicUsize::new(0))));
    outbox.poll_batch(&mut Context::from_waker(&waker), size_limit)
}

fn items(outbox: &Outbox) -> Vec<Item> {
    let Poll::Ready(Batch::Frames(bytes)) = poll(outbox, usize::MAX) else {
        return Vec::new();
    };
    let mut decoder = FrameDecoder::new();
    decoder.push(&bytes);
    let mut items = Vec::new();
    while let Some(frame) = decoder.next_frame().unwrap() {
        items.push(match frame.kind {
            FrameKind::Control => Item::Control(frame.to_client_message().unwrap()),
            FrameKind::Audio => Item::Audio(frame.to_audio().unwrap().sequence),
        });
    }
    decoder.finish().unwrap();
    items
}

fn audio(sequence: u32) -> AudioFrame {
    AudioFrame {
        gate: Gate::Live,
        source: 0,
        sequence,
        timestamp: 0,
        sample_rate: 16_000,
        samples: vec![0; 320],
    }
}

fn answer(text: &str) -> ClientMessage {
    ClientMessage::Answer {
        question: "q".into(),
        field: "f".into(),
        text: text.into(),
    }
}

fn presence(x: f32) -> ClientMessage {
    ClientMessage::Presence {
        head: Pose {
            position: [x, 0.0, 0.0],
            orientation: [0.0, 0.0, 0.0, 1.0],
        },
        left: None,
        right: None,
    }
}

#[test]
fn alternates_control_and_audio_so_neither_starves() {
    let outbox = Outbox::new(16, 64);
    outbox.connect();
    for index in 0..3 {
        outbox.send(answer(&index.to_string())).unwrap();
        outbox.send_audio(audio(index)).unwrap();
    }
    outbox.send_audio(audio(3)).unwrap();
    assert_eq!(
        items(&outbox),
        [
            Item::Control(answer("0")),
            Item::Audio(0),
            Item::Control(answer("1")),
            Item::Audio(1),
            Item::Control(answer("2")),
            Item::Audio(2),
            Item::Audio(3),
        ]
    );
}

#[test]
fn congestion_drops_the_oldest_audio_but_never_control() {
    let outbox = Outbox::new(2, 64);
    outbox.connect();
    for index in 0..100 {
        outbox.send(answer(&index.to_string())).unwrap();
    }
    for sequence in 0..5 {
        outbox.send_audio(audio(sequence)).unwrap();
    }
    assert_eq!(outbox.dropped_audio(), 3);
    let items = items(&outbox);
    let controls = items
        .iter()
        .filter(|item| matches!(item, Item::Control(_)))
        .count();
    assert_eq!(controls, 100);
    let audio: Vec<_> = items
        .iter()
        .filter_map(|item| match item {
            Item::Audio(sequence) => Some(*sequence),
            Item::Control(_) => None,
        })
        .collect();
    assert_eq!(audio, [3, 4]);
}

#[test]
fn batches_respect_the_size_limit() {
    let outbox = Outbox::new(16, 64);
    outbox.connect();
    for sequence in 0..4 {
        outbox.send_audio(audio(sequence)).unwrap();
    }
    let frame_size = Frame::audio(&audio(0)).encode().len();
    let Poll::Ready(Batch::Frames(bytes)) = poll(&outbox, frame_size + 1) else {
        panic!("expected frames");
    };
    assert_eq!(bytes.len(), frame_size * 2);
    assert_eq!(items(&outbox), [Item::Audio(2), Item::Audio(3)]);
}

#[test]
fn waits_and_wakes_when_traffic_arrives() {
    let outbox = Outbox::new(16, 64);
    outbox.connect();
    let counter = Arc::new(Counter(AtomicUsize::new(0)));
    let waker = Waker::from(counter.clone());
    let mut context = Context::from_waker(&waker);
    assert!(outbox.poll_batch(&mut context, 1024).is_pending());
    outbox.send(ClientMessage::ListSpaces).unwrap();
    assert_eq!(counter.0.load(Ordering::SeqCst), 1);
    assert!(matches!(
        outbox.poll_batch(&mut context, 1024),
        Poll::Ready(Batch::Frames(_))
    ));
}

#[test]
fn disconnected_traffic_is_deferred_or_dropped() {
    let outbox = Outbox::new(16, 64);
    outbox.send_audio(audio(0)).unwrap();
    assert_eq!(outbox.dropped_audio(), 1);
    outbox
        .send(ClientMessage::Join {
            space: "alpha".into(),
        })
        .unwrap();
    outbox
        .send(ClientMessage::Gate { gate: Gate::Live })
        .unwrap();
    outbox.send(presence(1.0)).unwrap();
    outbox.send(answer("kept")).unwrap();
    assert_eq!(
        outbox.membership(),
        Membership {
            space: Some("alpha".into()),
            gate: Gate::Live
        }
    );

    let restore = outbox.connect();
    assert_eq!(
        restore,
        [
            ClientMessage::Join {
                space: "alpha".into()
            },
            ClientMessage::Gate { gate: Gate::Live },
        ]
    );
    assert_eq!(items(&outbox), [Item::Control(answer("kept"))]);
}

#[test]
fn restored_membership_supersedes_unsent_membership_changes() {
    let outbox = Outbox::new(16, 64);
    outbox.connect();
    outbox
        .send(ClientMessage::Join {
            space: "alpha".into(),
        })
        .unwrap();
    outbox.send(ClientMessage::Leave).unwrap();
    outbox
        .send(ClientMessage::Join {
            space: "beta".into(),
        })
        .unwrap();
    outbox.send(presence(1.0)).unwrap();
    outbox.send(answer("kept")).unwrap();
    outbox.send_audio(audio(0)).unwrap();
    outbox.disconnect();
    assert_eq!(outbox.dropped_audio(), 1);

    let restore = outbox.connect();
    assert_eq!(
        restore,
        [ClientMessage::Join {
            space: "beta".into()
        }]
    );
    assert_eq!(items(&outbox), [Item::Control(answer("kept"))]);

    outbox.send(ClientMessage::Leave).unwrap();
    outbox
        .send(ClientMessage::Gate { gate: Gate::Off })
        .unwrap();
    assert!(outbox.membership().messages().is_empty());
}

#[test]
fn presence_is_coalesced_to_the_latest_pose() {
    let outbox = Outbox::new(16, 64);
    outbox.connect();
    outbox.send(presence(1.0)).unwrap();
    outbox.send(answer("between")).unwrap();
    outbox.send(presence(2.0)).unwrap();
    assert_eq!(
        items(&outbox),
        [
            Item::Control(presence(2.0)),
            Item::Control(answer("between"))
        ]
    );
}

#[test]
fn gated_off_audio_is_never_sent() {
    let outbox = Outbox::new(16, 64);
    outbox.connect();
    let mut frame = audio(0);
    frame.gate = Gate::Off;
    outbox.send_audio(frame).unwrap();
    assert!(poll(&outbox, 1024).is_pending());
    assert_eq!(outbox.dropped_audio(), 0);
}

#[test]
fn hello_is_reserved_and_closing_drains_control() {
    let outbox = Outbox::new(16, 64);
    outbox.connect();
    let hello = ClientMessage::Hello {
        protocol: 1,
        participant: "p".into(),
        name: "n".into(),
        credentials: None,
    };
    assert_eq!(outbox.send(hello), Err(SendError::Reserved));
    outbox.send(answer("last")).unwrap();
    outbox.close();
    assert!(outbox.is_closed());
    assert_eq!(outbox.send(answer("late")), Err(SendError::Closed));
    assert_eq!(outbox.send_audio(audio(0)), Err(SendError::Closed));
    assert_eq!(items(&outbox), [Item::Control(answer("last"))]);
    assert_eq!(poll(&outbox, 1024), Poll::Ready(Batch::Closed));
    assert_eq!(SendError::Closed.to_string(), "the client is closed");
    assert!(SendError::Reserved.to_string().contains("hello"));
}

#[test]
fn membership_follows_the_server_unless_a_change_is_queued() {
    let outbox = Outbox::new(8, 64);
    outbox.connect();
    outbox
        .send(ClientMessage::Join { space: "a".into() })
        .unwrap();

    // The queued join takes precedence over a report for an earlier space.
    outbox.joined(None);
    assert_eq!(outbox.membership().space.as_deref(), Some("a"));

    items(&outbox);
    outbox.joined(Some("a".into()));
    assert_eq!(outbox.membership().space.as_deref(), Some("a"));

    // The server left the space on the client's behalf, so a reconnection does not rejoin it.
    outbox.joined(None);
    outbox.disconnect();
    assert!(outbox.connect().is_empty());
}

#[test]
fn keepalive_precedes_queued_traffic_and_is_not_deferred() {
    let outbox = Outbox::new(16, 64);
    // Keepalive belongs to one connection: nothing is kept while disconnected.
    outbox.keepalive(ClientMessage::Ping { nonce: 0 });
    outbox.connect();
    assert!(poll(&outbox, 1024).is_pending());

    for sequence in 0..4 {
        outbox.send_audio(audio(sequence)).unwrap();
    }
    outbox.send(answer("a")).unwrap();
    outbox.keepalive(ClientMessage::Pong { nonce: 1 });
    outbox.keepalive(ClientMessage::Ping { nonce: 2 });
    // Even a batch limit smaller than one frame carries every keepalive message, first.
    let Poll::Ready(Batch::Frames(bytes)) = poll(&outbox, 1) else {
        panic!("expected frames");
    };
    let mut decoder = FrameDecoder::new();
    decoder.push(&bytes);
    let mut messages = Vec::new();
    while let Some(frame) = decoder.next_frame().unwrap() {
        messages.push(frame.to_client_message().unwrap());
    }
    assert_eq!(
        messages,
        [
            ClientMessage::Pong { nonce: 1 },
            ClientMessage::Ping { nonce: 2 }
        ]
    );

    outbox.keepalive(ClientMessage::Ping { nonce: 3 });
    outbox.disconnect();
    outbox.connect();
    assert_eq!(items(&outbox), [Item::Control(answer("a"))]);

    for message in [
        ClientMessage::Ping { nonce: 4 },
        ClientMessage::Pong { nonce: 4 },
    ] {
        assert_eq!(outbox.send(message), Err(SendError::Reserved));
    }
}

#[test]
fn a_leave_the_server_has_not_confirmed_is_restored() {
    let outbox = Outbox::new(16, 64);
    outbox.connect();
    outbox
        .send(ClientMessage::Join { space: "a".into() })
        .unwrap();
    items(&outbox);
    outbox.joined(Some("a".into()));

    // Leaving while disconnected: the server would rejoin the space it remembers.
    outbox.disconnect();
    outbox.send(ClientMessage::Leave).unwrap();
    assert_eq!(outbox.connect(), [ClientMessage::Leave]);

    // Still unconfirmed when that connection ends, so it is sent again.
    outbox.disconnect();
    assert_eq!(outbox.connect(), [ClientMessage::Leave]);

    // The server's `Left` confirms it.
    outbox.joined(None);
    outbox.disconnect();
    assert!(outbox.connect().is_empty());

    // A leave lost with its connection is sent again too.
    outbox
        .send(ClientMessage::Join { space: "b".into() })
        .unwrap();
    outbox.send(ClientMessage::Leave).unwrap();
    items(&outbox);
    outbox.disconnect();
    assert_eq!(outbox.connect(), [ClientMessage::Leave]);

    // Joining again supersedes it.
    outbox.disconnect();
    outbox
        .send(ClientMessage::Join { space: "c".into() })
        .unwrap();
    assert_eq!(
        outbox.connect(),
        [ClientMessage::Join { space: "c".into() }]
    );
}

fn agent(sequence: u32) -> AudioFrame {
    AudioFrame {
        gate: Gate::Agent,
        ..audio(sequence)
    }
}

fn begin(utterance: &str) -> ClientMessage {
    ClientMessage::UtteranceBegin {
        utterance: utterance.into(),
    }
}

fn end(utterance: &str) -> ClientMessage {
    ClientMessage::UtteranceEnd {
        utterance: utterance.into(),
    }
}

fn cancel(utterance: &str) -> ClientMessage {
    ClientMessage::UtteranceCancel {
        utterance: utterance.into(),
    }
}

/// The utterance control messages and audio sequences, in the order sent.
fn utterance_items(items: &[Item]) -> Vec<&Item> {
    items
        .iter()
        .filter(|item| match item {
            Item::Audio(_) => true,
            Item::Control(message) => matches!(
                message,
                ClientMessage::UtteranceBegin { .. }
                    | ClientMessage::UtteranceEnd { .. }
                    | ClientMessage::UtteranceCancel { .. }
            ),
        })
        .collect()
}

#[test]
fn utterance_begin_is_not_overtaken_by_its_audio() {
    let outbox = Outbox::new(16, 64);
    outbox.connect();
    // A backlog of control messages used to delay the begin while its audio went ahead.
    for index in 0..4 {
        outbox.send(answer(&index.to_string())).unwrap();
    }
    outbox.send(begin("u")).unwrap();
    for sequence in 0..4 {
        outbox.send_audio(agent(sequence)).unwrap();
    }
    outbox.send(end("u")).unwrap();
    let items = items(&outbox);
    assert_eq!(
        utterance_items(&items),
        [
            &Item::Control(begin("u")),
            &Item::Audio(0),
            &Item::Audio(1),
            &Item::Audio(2),
            &Item::Audio(3),
            &Item::Control(end("u")),
        ]
    );
    // The earlier control messages still precede the begin.
    let position = |wanted: &Item| items.iter().position(|item| item == wanted).unwrap();
    assert!(position(&Item::Control(answer("3"))) < position(&Item::Control(begin("u"))));
}

#[test]
fn utterance_end_is_not_sent_before_its_trailing_audio() {
    let outbox = Outbox::new(16, 64);
    outbox.connect();
    outbox.send(begin("u")).unwrap();
    for sequence in 0..6 {
        outbox.send_audio(agent(sequence)).unwrap();
    }
    outbox.send(end("u")).unwrap();
    // A control message queued after the end does not overtake it either.
    outbox.send(answer("after")).unwrap();
    let mut expected = vec![Item::Control(begin("u"))];
    expected.extend((0..6).map(Item::Audio));
    expected.push(Item::Control(end("u")));
    expected.push(Item::Control(answer("after")));
    assert_eq!(items(&outbox), expected);
}

#[test]
fn congestion_drops_live_audio_but_never_frames_of_the_open_utterance() {
    let outbox = Outbox::new(2, 64);
    outbox.connect();
    for sequence in 0..5 {
        outbox.send_audio(audio(sequence)).unwrap();
    }
    outbox.send(begin("u")).unwrap();
    for sequence in 10..30 {
        outbox.send_audio(agent(sequence)).unwrap();
    }
    outbox.send(end("u")).unwrap();
    for sequence in 40..45 {
        outbox.send_audio(audio(sequence)).unwrap();
    }
    let mut expected = vec![Item::Control(begin("u"))];
    expected.extend((10..30).map(Item::Audio));
    expected.push(Item::Control(end("u")));
    expected.extend([Item::Audio(43), Item::Audio(44)]);
    assert_eq!(items(&outbox), expected);
    assert_eq!(outbox.dropped_audio(), 8);
}

#[test]
fn congestion_keeps_live_frames_within_the_open_utterance() {
    let outbox = Outbox::new(2, 64);
    outbox.connect();
    outbox.send(begin("u")).unwrap();
    // The gate switches to live and back while the utterance is open.
    outbox.send_audio(agent(0)).unwrap();
    for sequence in 1..6 {
        outbox.send_audio(audio(sequence)).unwrap();
    }
    outbox.send_audio(agent(6)).unwrap();
    outbox.send(end("u")).unwrap();
    let mut expected = vec![Item::Control(begin("u"))];
    expected.extend((0..7).map(Item::Audio));
    expected.push(Item::Control(end("u")));
    assert_eq!(items(&outbox), expected);
    assert_eq!(outbox.dropped_audio(), 0);
}

#[test]
fn an_utterance_beyond_the_congestion_limit_fails_visibly() {
    let outbox = Outbox::new(2, 3);
    outbox.connect();
    outbox.send(begin("u")).unwrap();
    for sequence in 0..3 {
        assert_eq!(outbox.send_audio(agent(sequence)), Ok(None));
    }
    assert_eq!(
        outbox.send_audio(agent(3)),
        Ok(Some(FailedUtterance {
            utterance: "u".into(),
            failure: UtteranceFailure::Congested
        }))
    );
    // Later frames are only live audio now, and the end is not sent.
    for sequence in 4..8 {
        assert_eq!(outbox.send_audio(agent(sequence)), Ok(None));
    }
    assert_eq!(outbox.send(end("u")), Ok(None));
    assert_eq!(
        items(&outbox),
        [
            Item::Control(begin("u")),
            Item::Control(cancel("u")),
            Item::Audio(6),
            Item::Audio(7),
        ]
    );

    // Frames already sent are followed by the cancellation, so recognition never sees a gap.
    outbox.send(begin("v")).unwrap();
    outbox.send_audio(agent(10)).unwrap();
    assert_eq!(items(&outbox), [Item::Control(begin("v")), Item::Audio(10)]);
    for sequence in 11..15 {
        outbox.send_audio(agent(sequence)).unwrap();
    }
    assert_eq!(items(&outbox), [Item::Control(cancel("v"))]);
}

#[test]
fn utterances_interrupted_by_disconnection_fail_visibly() {
    let outbox = Outbox::new(16, 64);
    let disconnected = |utterance: &str| FailedUtterance {
        utterance: utterance.into(),
        failure: UtteranceFailure::Disconnected,
    };

    // Begun while disconnected: its audio cannot be delivered.
    assert_eq!(outbox.send(begin("a")), Ok(Some(disconnected("a"))));
    assert_eq!(outbox.send(end("a")), Ok(None));

    outbox.connect();
    // Ended but not yet sent, then still open, when the connection ends.
    outbox.send(begin("b")).unwrap();
    outbox.send_audio(agent(0)).unwrap();
    outbox.send(end("b")).unwrap();
    outbox.send(begin("c")).unwrap();
    outbox.send_audio(agent(1)).unwrap();
    assert_eq!(outbox.disconnect(), [disconnected("b"), disconnected("c")]);
    assert_eq!(outbox.send(end("c")), Ok(None));
    outbox.connect();
    assert!(poll(&outbox, 1024).is_pending());

    // An utterance sent completely is not reported.
    outbox.send(begin("d")).unwrap();
    outbox.send_audio(agent(2)).unwrap();
    outbox.send(end("d")).unwrap();
    items(&outbox);
    assert!(outbox.disconnect().is_empty());
}
