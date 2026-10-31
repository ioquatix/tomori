// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

//! Every server frame stays within the protocol's frame limit, whatever clients and the agent send. See `design/server.md`.

use protocol::{ClientMessage, MAX_FRAME_BYTES, ServerMessage};
use server::test_support::{TIMEOUT, TestClient, TestServer};

/// The snapshot budget documented in `design/server.md`.
const BUDGET: usize = MAX_FRAME_BYTES / 4;

fn json_len<T: serde::Serialize>(value: &T) -> usize {
    serde_json::to_vec(value).unwrap().len()
}

#[test]
fn oversized_client_strings_are_rejected_without_repeating_them() {
    let server = TestServer::start();
    let mut ada = server.connect("ada", "Ada");

    // About a megabyte of JSON once escaped, which an echoing error reply could not carry.
    let space: String = "\u{200b}".repeat(340_000);
    ada.send(&ClientMessage::Join { space });
    assert_eq!(ada.error(), "That space does not exist.");

    ada.send(&ClientMessage::CreateSpace {
        title: "A".repeat(600_000),
        backend: "fake".into(),
        directory: "/tmp".into(),
        session: None,
    });
    let error = ada.error();
    assert!(error.contains("title") && error.len() < 100, "{error}");

    ada.send(&ClientMessage::CreateSpace {
        title: "T".into(),
        backend: "B".repeat(600_000),
        directory: "/tmp".into(),
        session: None,
    });
    let error = ada.error();
    assert!(!error.contains("BBB"), "{error}");

    ada.send(&ClientMessage::UtteranceBegin {
        utterance: "u".repeat(10_000),
    });
    let error = ada.error();
    assert!(error.contains("utterance") && error.len() < 100, "{error}");

    ada.send(&ClientMessage::UtteranceEnd {
        utterance: "u".repeat(10_000),
    });
    assert_eq!(ada.error(), "That utterance is not open.");

    // The connection and the server keep working.
    ada.send(&ClientMessage::ListSpaces);
    ada.wait_for(|message| matches!(message, ServerMessage::Spaces { .. }).then_some(()));
    assert_eq!(server.get("/health").0, 200);
}

#[test]
fn an_oversized_hello_is_refused() {
    let server = TestServer::start();
    for (participant, name) in [
        ("p".repeat(1000), "Ada".to_owned()),
        ("ada".to_owned(), "N".repeat(1000)),
    ] {
        let mut client = TestClient::open(server.address);
        client.send(&ClientMessage::Hello {
            protocol: protocol::PROTOCOL_VERSION,
            participant,
            name,
            credentials: None,
        });
        let error = client.error();
        assert!(
            error.len() < 100 && !error.contains("ppp") && !error.contains("NNN"),
            "{error}"
        );
        assert!(client.is_closed(TIMEOUT));
    }
}

#[test]
fn the_lobby_listing_stays_within_the_budget() {
    let server = TestServer::start();
    let lobby = server.connect("lobby", "Lobby");
    let mut creator = server.connect("creator", "Creator");
    let directory = format!("/{}", "d".repeat(4000));
    for index in 0..100 {
        creator.send(&ClientMessage::CreateSpace {
            title: format!("Space {index}"),
            backend: "fake".into(),
            directory: directory.clone(),
            session: None,
        });
        let title = format!("Space {index}");
        creator.space(|state| state.title == title);
    }
    // The lobby client decodes every frame it receives, so an oversized one would fail here.
    let spaces = lobby.wait_for(|message| match message {
        ServerMessage::Spaces { spaces }
            if spaces.iter().any(|space| space.title == "Space 99") =>
        {
            Some(spaces.clone())
        }
        _ => None,
    });
    assert!(json_len(&spaces) <= BUDGET);
    assert_eq!(spaces.len(), 100);
}

#[test]
fn a_long_conversation_is_trimmed_to_the_budget() {
    let server = TestServer::start();
    let mut ada = server.connect("ada", "Ada");
    ada.create_space("Long");
    let session = server.backend.started()[0].clone();
    // Control characters take six bytes each in JSON, so each message is far larger than its length.
    let text = "\u{1}".repeat(19_000);
    let mut last = String::new();
    for _ in 0..20 {
        last = session.stream(&[&text]);
    }
    let state = ada.space(|state| {
        state
            .messages
            .last()
            .is_some_and(|message| message.id == last)
    });
    assert!(json_len(&state) <= BUDGET);
    assert!(state.messages.len() < 20);
}

/// The error reported for `utterance`.
fn transcript_failure(client: &TestClient, utterance: &str) -> String {
    client.wait_for(|message| match message {
        ServerMessage::TranscriptFailed {
            utterance: failed,
            error,
        } if failed == utterance => Some(error.clone()),
        _ => None,
    })
}

#[test]
fn queued_input_is_bounded() {
    let server = TestServer::with_backend(server::fake::FakeBackend::manual());
    let mut ada = server.connect("ada", "Ada");
    ada.create_space("Busy");

    // A transcript longer than the limit is refused rather than queued.
    ada.say("long", &"a".repeat(5000));
    let error = transcript_failure(&ada, "long");
    assert!(error.contains("too long"), "{error}");

    // The first entry is delivered and keeps the agent busy; the rest wait, up to the limit.
    for index in 0..17 {
        ada.say(&format!("u{index}"), &format!("entry {index}"));
    }
    let error = transcript_failure(&ada, "u16");
    assert!(error.contains("queue is full"), "{error}");
    let state = ada.space(|state| state.queue.iter().any(|entry| entry.text == "entry 15"));
    assert!(!state.queue.iter().any(|entry| entry.text == "entry 16"));
}
