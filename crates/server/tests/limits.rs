// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

//! Every server frame stays within the protocol's frame limit, whatever clients and the agent send. See `design/server.md`.

use protocol::{ClientMessage, MAX_FRAME_BYTES, QueueState, ServerMessage};
use std::sync::Arc;

use server::Server;
use server::fake::FakeBackend;
use server::test_support::{TIMEOUT, TestClient, TestServer, TextTranscriber};

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
        ("ada".to_owned(), String::new()),
        ("ada".to_owned(), " \t ".to_owned()),
        // A name begins each attributed prompt, so it must not read as a slash command.
        ("ada".to_owned(), "/clear".to_owned()),
        ("ada".to_owned(), " /compact".to_owned()),
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
    const SPACES: usize = 320;
    let server = TestServer::with_server(FakeBackend::new(), |backends| {
        Server::new(backends, Arc::new(TextTranscriber::default())).with_max_spaces(SPACES)
    });
    let lobby = server.connect("lobby", "Lobby");
    let mut creator = server.connect("creator", "Creator");
    // Spaces run in existing directories beneath the workspace roots, and a path is at most 1024 bytes on macOS, so many spaces in a long directory exceed the budget instead.
    let mut directory = std::env::temp_dir()
        .canonicalize()
        .unwrap()
        .join(format!("tomori-lobby-{}", std::process::id()));
    let scratch = directory.clone();
    for part in ['a', 'b', 'c', 'd'] {
        directory.push(part.to_string().repeat(220));
    }
    std::fs::create_dir_all(&directory).unwrap();
    let directory = directory.to_str().unwrap().to_owned();
    assert!(json_len(&directory) * SPACES > BUDGET);
    for index in 0..SPACES {
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
            if spaces
                .iter()
                .any(|space| space.title == format!("Space {}", SPACES - 1)) =>
        {
            Some(spaces.clone())
        }
        _ => None,
    });
    let _ = std::fs::remove_dir_all(scratch);
    assert!(json_len(&spaces) <= BUDGET);
    assert_eq!(spaces.len(), SPACES);
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
    ada.say("u0", "entry 0");
    ada.space(|state| {
        state
            .queue
            .iter()
            .any(|entry| entry.text == "entry 0" && entry.state == QueueState::Delivered)
    });
    for index in 1..=17 {
        ada.say(&format!("u{index}"), &format!("entry {index}"));
    }
    // Snapshots are sent when entries are queued, before the failure of the one that did not fit.
    let mut latest = None;
    let error = ada.wait_for(|message| match message {
        ServerMessage::Space { state } => {
            latest = Some(state.clone());
            None
        }
        ServerMessage::TranscriptFailed { utterance, error } if utterance == "u17" => {
            Some(error.clone())
        }
        _ => None,
    });
    assert!(error.contains("queue is full"), "{error}");
    let state = latest.expect("a snapshot was sent");
    let texts: Vec<&str> = state
        .queue
        .iter()
        .map(|entry| entry.text.as_str())
        .collect();
    assert_eq!(texts.len(), 17, "{texts:?}");
    assert_eq!(texts.last(), Some(&"entry 16"));
}
