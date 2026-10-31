// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

use std::time::Duration;

use backend::{AnswerValue, Event, Field, FieldKind, FieldOption, Permission, Question};
use protocol::{Activity, ClientMessage, Decision, Gate, QueueState, ServerMessage, SpaceState};
use server::fake::FakeBackend;
use server::test_support::{TIMEOUT, TestClient, TestServer, speech};

/// Errors received within `window`, skipping other messages.
fn errors_within(client: &TestClient, window: Duration) -> Vec<String> {
    let mut errors = Vec::new();
    while let Some(message) = client.next_message(window) {
        if let ServerMessage::Error { error } = message {
            errors.push(error);
        }
    }
    errors
}

/// Join `space` and wait until the snapshot lists `count` participants.
fn join(client: &mut TestClient, space: &str, count: usize) -> SpaceState {
    client.send(&ClientMessage::Join {
        space: space.to_owned(),
    });
    client.space(|state| state.id == space && state.participants.len() == count)
}

/// Two participants in one space, created by Ada.
fn pair(server: &TestServer) -> (TestClient, TestClient, String) {
    let mut ada = server.connect("ada", "Ada");
    let mut bob = server.connect("bob", "Bob");
    let space = ada.create_space("Pair").id;
    join(&mut bob, &space, 2);
    ada.space(|state| state.participants.len() == 2);
    (ada, bob, space)
}

#[test]
fn queued_input_is_visible_to_everyone_and_anyone_can_cancel_it() {
    let server = TestServer::with_backend(FakeBackend::manual());
    let (mut ada, mut bob, _) = pair(&server);

    ada.say("u1", "first");
    ada.space(|state| {
        state.activity == Activity::Running
            && state
                .queue
                .iter()
                .any(|entry| entry.state == QueueState::Delivered)
    });

    bob.say("u2", "second");
    let state = ada.space(|state| state.queue.iter().any(|entry| entry.speaker == "Bob"));
    let entry = state
        .queue
        .iter()
        .find(|entry| entry.speaker == "Bob")
        .unwrap()
        .clone();
    assert_eq!(entry.state, QueueState::Queued);
    assert_eq!(entry.text, "second");

    // Ada cancels Bob's queued entry; both see it go.
    ada.send(&ClientMessage::CancelQueued {
        entry: entry.id.clone(),
    });
    bob.space(|state| !state.queue.iter().any(|queued| queued.id == entry.id));

    // A delivered entry cannot be cancelled.
    let delivered = state
        .queue
        .iter()
        .find(|entry| entry.state == QueueState::Delivered)
        .unwrap()
        .id
        .clone();
    bob.send(&ClientMessage::CancelQueued { entry: delivered });
    assert_eq!(bob.error(), "That input was already sent to the agent.");

    // The next entry waits until the agent is idle, then is delivered.
    bob.say("u3", "third");
    bob.space(|state| {
        state
            .queue
            .iter()
            .any(|entry| entry.text == "third" && entry.state == QueueState::Queued)
    });
    let session = server.backend.started()[0].clone();
    assert_eq!(session.log().prompts.len(), 1);
    session.reply("done");
    bob.space(|state| {
        state
            .queue
            .iter()
            .any(|entry| entry.text == "third" && entry.state == QueueState::Delivered)
    });

    let prompts: Vec<String> = session
        .log()
        .prompts
        .into_iter()
        .map(|prompt| prompt.text)
        .collect();
    assert_eq!(prompts, ["Ada: first", "Bob: third"]);
}

#[test]
fn a_repeated_utterance_is_queued_once() {
    let server = TestServer::with_backend(FakeBackend::manual());
    let (mut ada, _bob, _) = pair(&server);
    ada.say("u1", "first");
    ada.say("u2", "second");
    ada.say("u3", "third");
    // Messages on one connection are handled in order, so this snapshot includes the repeat.
    let state = ada.space(|state| state.queue.iter().any(|entry| entry.text == "third"));
    assert_eq!(
        state
            .queue
            .iter()
            .filter(|entry| entry.text == "second")
            .count(),
        1
    );
}

#[test]
fn the_first_decision_wins() {
    let server = TestServer::start();
    let (mut ada, mut bob, _) = pair(&server);
    let session = server.backend.started()[0].clone();
    session.emit(Event::PermissionRequested(Permission {
        id: "p1".to_owned(),
        tool: "Bash".to_owned(),
        input: serde_json::json!({"command": "ls -la"}),
        description: Some("List the files".to_owned()),
    }));

    let state = bob.space(|state| state.approval.is_some());
    let approval = state.approval.unwrap();
    assert_eq!(state.activity, Activity::Waiting);
    assert_eq!(approval.title, "Bash");
    assert!(
        approval.text.contains("List the files"),
        "{}",
        approval.text
    );
    assert!(approval.text.contains("ls -la"), "{}", approval.text);
    assert!(
        approval.text.ends_with("A: allow once. B: deny."),
        "{}",
        approval.text
    );
    ada.space(|state| state.approval.is_some());

    ada.send(&ClientMessage::Decide {
        request: approval.id.clone(),
        decision: Decision::AllowOnce,
    });
    bob.send(&ClientMessage::Decide {
        request: approval.id.clone(),
        decision: Decision::Deny,
    });

    let window = Duration::from_millis(300);
    let mut errors = errors_within(&ada, window);
    errors.extend(errors_within(&bob, window));
    assert_eq!(errors, ["That approval is no longer pending."]);

    let decisions = session.log().decisions;
    assert_eq!(decisions.len(), 1);
    assert_eq!(decisions[0].0, "p1");

    // Once resolved, later decisions are still refused.
    ada.send(&ClientMessage::Decide {
        request: approval.id,
        decision: Decision::Deny,
    });
    assert_eq!(ada.error(), "That approval is no longer pending.");
}

#[test]
fn parallel_permissions_are_decided_in_turn() {
    let server = TestServer::start();
    let (mut ada, bob, _) = pair(&server);
    let session = server.backend.started()[0].clone();
    for id in ["p1", "p2"] {
        session.emit(Event::PermissionRequested(Permission {
            id: id.to_owned(),
            tool: "Bash".to_owned(),
            input: serde_json::Value::Null,
            description: None,
        }));
    }

    // The oldest request is presented first, and the second waits its turn.
    let state = bob.space(|state| state.approval.as_ref().is_some_and(|a| a.id == "p1"));
    assert_eq!(state.activity, Activity::Waiting);
    ada.send(&ClientMessage::Decide {
        request: "p2".to_owned(),
        decision: Decision::Deny,
    });
    assert_eq!(ada.error(), "That approval is no longer pending.");

    ada.send(&ClientMessage::Decide {
        request: "p1".to_owned(),
        decision: Decision::AllowOnce,
    });
    let state = bob.space(|state| state.approval.as_ref().is_some_and(|a| a.id == "p2"));
    assert_eq!(state.activity, Activity::Waiting);
    ada.send(&ClientMessage::Decide {
        request: "p2".to_owned(),
        decision: Decision::Deny,
    });
    // Once the backend confirms the last decision, nothing is pending.
    bob.space(|state| state.approval.is_none() && state.activity != Activity::Waiting);

    assert_eq!(
        session.log().decisions,
        [
            ("p1".to_owned(), backend::Decision::AllowOnce),
            ("p2".to_owned(), backend::Decision::Deny)
        ]
    );
}

#[test]
fn a_question_is_answered_by_any_participant() {
    let server = TestServer::start();
    let (mut ada, mut bob, _) = pair(&server);
    let session = server.backend.started()[0].clone();
    session.emit(Event::QuestionAsked(Question {
        id: "q1".to_owned(),
        title: "Preferences".to_owned(),
        fields: vec![Field {
            key: "colour".to_owned(),
            title: "Colour".to_owned(),
            description: None,
            kind: FieldKind::Choice,
            options: vec![
                FieldOption {
                    label: "Red".to_owned(),
                    description: None,
                },
                FieldOption {
                    label: "Blue".to_owned(),
                    description: None,
                },
            ],
            custom: false,
        }],
    }));

    let question = ada
        .space(|state| state.question.is_some())
        .question
        .unwrap();
    assert!(question.supported);
    assert_eq!(question.field, "colour");
    assert!(question.text.contains("2. Blue"), "{}", question.text);

    bob.space(|state| state.question.is_some());
    bob.send(&ClientMessage::Answer {
        question: question.id.clone(),
        field: question.field.clone(),
        text: "2".to_owned(),
    });
    ada.space(|state| state.question.is_none());
    assert_eq!(
        session.log().answers,
        [(
            "q1".to_owned(),
            vec![(
                "colour".to_owned(),
                AnswerValue::Choices(vec!["Blue".to_owned()])
            )]
        )]
    );

    ada.send(&ClientMessage::Answer {
        question: question.id,
        field: question.field,
        text: "Red".to_owned(),
    });
    assert_eq!(ada.error(), "That question is no longer pending.");
}

#[test]
fn attribution_can_be_turned_off_per_space() {
    let server = TestServer::start();
    let (mut ada, bob, _) = pair(&server);
    ada.send(&ClientMessage::SetAttribution { enabled: false });
    bob.space(|state| !state.attribution);
    ada.say("u1", "no names please");
    ada.space(|state| {
        state
            .queue
            .iter()
            .any(|entry| entry.state == QueueState::Delivered)
    });

    ada.send(&ClientMessage::SetAttribution { enabled: true });
    bob.space(|state| state.attribution);
    ada.say("u2", "names again");
    bob.space(|state| {
        state
            .queue
            .iter()
            .filter(|entry| entry.state == QueueState::Delivered)
            .count()
            == 2
    });

    let prompts: Vec<String> = server.backend.started()[0]
        .log()
        .prompts
        .into_iter()
        .map(|prompt| prompt.text)
        .collect();
    assert_eq!(prompts, ["no names please", "Ada: names again"]);
}

#[test]
fn audio_is_relayed_within_the_space_with_the_server_assigned_source() {
    let server = TestServer::start();
    let (mut ada, bob, _) = pair(&server);
    let carol = server.connect("carol", "Carol");
    let window = Duration::from_millis(300);

    // Off frames carry nothing and are not relayed.
    ada.send_audio(Gate::Off, vec![0; 320]);
    ada.send_audio(Gate::Live, vec![7; 320]);
    let frame = bob.next_audio(TIMEOUT).expect("live audio");
    assert_eq!(frame.gate, Gate::Live);
    assert_eq!(frame.source, ada.source);
    assert_eq!(frame.samples, vec![7; 320]);

    // Agent audio is relayed too, and transcribed for the speaker.
    ada.say("u1", "hi");
    let frame = bob.next_audio(TIMEOUT).expect("agent audio");
    assert_eq!(frame.gate, Gate::Agent);
    assert_eq!(frame.source, ada.source);
    assert_eq!(frame.samples, speech("hi"));
    let transcript = ada.wait_for(|message| match message {
        ServerMessage::Transcript {
            utterance,
            text,
            finished: true,
        } if utterance == "u1" => Some(text.clone()),
        _ => None,
    });
    assert_eq!(transcript, "hi");

    // Nobody hears themselves, and nobody outside the space hears anything.
    assert!(ada.next_audio(window).is_none());
    assert!(carol.next_audio(window).is_none());
    assert!(bob.next_audio(window).is_none());
}

#[test]
fn a_lost_frame_fails_the_utterance() {
    let server = TestServer::start();
    let (mut ada, _bob, _) = pair(&server);
    ada.send(&ClientMessage::UtteranceBegin {
        utterance: "u1".to_owned(),
    });
    ada.send_audio(Gate::Agent, speech("ab"));
    ada.skip_sequence(2);
    ada.send_audio(Gate::Agent, speech("cd"));
    ada.send(&ClientMessage::UtteranceEnd {
        utterance: "u1".to_owned(),
    });
    let error = ada.wait_for(|message| match message {
        ServerMessage::TranscriptFailed { utterance, error } if utterance == "u1" => {
            Some(error.clone())
        }
        _ => None,
    });
    assert!(error.contains("lost"), "{error}");

    // The connection keeps working, and nothing was sent to the agent.
    ada.say("u2", "again");
    ada.space(|state| {
        state
            .queue
            .iter()
            .any(|entry| entry.state == QueueState::Delivered)
    });
    let prompts = server.backend.started()[0].log().prompts;
    assert_eq!(prompts.len(), 1);
    assert_eq!(prompts[0].text, "Ada: again");
}

#[test]
fn reconnecting_keeps_the_identity_and_rejoins_the_space() {
    let server = TestServer::start();
    let (ada, bob, space) = pair(&server);

    let again = server.connect("ada", "Ada");
    assert_eq!(again.source, ada.source);
    let state = again.space(|state| state.id == space);
    assert_eq!(state.participants.len(), 2);
    assert!(ada.is_closed(TIMEOUT));

    let state = bob.space(|state| state.participants.len() == 2);
    let ada_entries = state
        .participants
        .iter()
        .filter(|participant| participant.id == "ada")
        .count();
    assert_eq!(ada_entries, 1);
}

#[test]
fn participants_move_between_spaces_and_the_lobby() {
    let server = TestServer::start();
    let mut ada = server.connect("ada", "Ada");
    let mut bob = server.connect("bob", "Bob");
    let carol = server.connect("carol", "Carol");
    let first = ada.create_space("First").id;
    let second = bob.create_space("Second").id;

    // Only participants in the lobby receive the spaces list.
    let spaces = carol.wait_for(|message| match message {
        ServerMessage::Spaces { spaces } if spaces.len() == 2 => Some(spaces.clone()),
        _ => None,
    });
    assert!(spaces.iter().any(|space| space.id == first));

    join(&mut ada, &second, 2);
    bob.space(|state| state.participants.len() == 2);

    ada.send(&ClientMessage::Leave);
    ada.wait_for(|message| matches!(message, ServerMessage::Left).then_some(()));
    let spaces = ada.wait_for(|message| match message {
        ServerMessage::Spaces { spaces } => Some(spaces.clone()),
        _ => None,
    });
    let second = spaces.iter().find(|space| space.id == second).unwrap();
    assert_eq!(second.participants, 1);
    bob.space(|state| state.participants.len() == 1);

    ada.send(&ClientMessage::Answer {
        question: "q".to_owned(),
        field: "f".to_owned(),
        text: "x".to_owned(),
    });
    assert_eq!(ada.error(), "Join a space first.");
}
