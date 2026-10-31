// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

use protocol::{Activity, ClientMessage, QueueState, Role, ServerMessage};
use server::test_support::{TIMEOUT, TestServer};

#[test]
fn health_reports_the_protocol_version() {
    let server = TestServer::start();
    let (status, body) = server.get("/health");
    assert_eq!(status, 200);
    let health: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(health["protocol"], protocol::PROTOCOL_VERSION);
    assert_eq!(server.get("/missing").0, 404);
}

#[test]
fn hello_is_answered_with_a_welcome_and_the_lobby() {
    let server = TestServer::start();
    let ada = server.connect("ada", "Ada");
    let bob = server.connect("bob", "Bob");
    assert_ne!(ada.source, bob.source);
    let spaces = ada.wait_for(|message| match message {
        ServerMessage::Spaces { spaces } => Some(spaces.clone()),
        _ => None,
    });
    assert!(spaces.is_empty());
}

#[test]
fn messages_before_hello_are_rejected() {
    let server = TestServer::start();
    let mut client = server_client(&server);
    client.send(&ClientMessage::ListSpaces);
    assert_eq!(client.error(), "Send hello first.");
}

fn server_client(server: &TestServer) -> server::test_support::TestClient {
    server::test_support::TestClient::open(server.address)
}

#[test]
fn spoken_input_is_delivered_and_the_reply_streams_back() {
    let server = TestServer::start();
    let mut ada = server.connect("ada", "Ada");
    let space = ada.create_space("Demo");
    assert_eq!(space.activity, Activity::Idle);
    assert_eq!(space.participants.len(), 1);

    ada.say("u1", "hello there");
    let transcript = ada.wait_for(|message| match message {
        ServerMessage::Transcript {
            utterance,
            text,
            finished: true,
        } if utterance == "u1" => Some(text.clone()),
        _ => None,
    });
    assert_eq!(transcript, "hello there");

    let state = ada.space(|state| {
        state.activity == Activity::Idle
            && state
                .messages
                .iter()
                .any(|message| message.completed && message.role == Role::Assistant)
    });
    assert_eq!(state.queue[0].state, QueueState::Delivered);
    assert_eq!(state.messages[0].text, "Ada: hello there");
    assert_eq!(state.messages[1].text, "You said: Ada: hello there");

    let prompts = server.backend.started()[0].log().prompts;
    assert_eq!(prompts.len(), 1);
    assert_eq!(prompts[0].text, "Ada: hello there");
}

#[test]
fn ending_the_upload_ends_the_connection() {
    let server = TestServer::start();
    let mut ada = server.connect("ada", "Ada");
    ada.finish();
    assert!(ada.is_closed(TIMEOUT));
}
