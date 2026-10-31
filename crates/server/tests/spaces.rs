// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

use std::sync::Arc;
use std::time::Duration;

use backend::{AnswerValue, Event, Field, FieldKind, FieldOption, Permission, Question};
use protocol::{Activity, ClientMessage, Decision, Gate, QueueState, ServerMessage, SpaceState};
use server::Server;
use server::fake::FakeBackend;
use server::test_support::{TIMEOUT, TestClient, TestServer, TextTranscriber, speech};

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
fn only_the_most_recent_empty_closed_spaces_are_kept() {
    let server = TestServer::with_server(FakeBackend::new(), |backends| {
        Server::new(backends, Arc::new(TextTranscriber::default())).with_max_spaces(64)
    });
    let mut ada = server.connect("ada", "Ada");
    let mut bob = server.connect("bob", "Bob");
    // Bob stays in the first space, so it is kept although it closes first.
    let first = ada.create_space("Space 0").id;
    join(&mut bob, &first, 2);
    let count = server::CLOSED_SPACES + 8;
    let mut ids = vec![first];
    for index in 0..count {
        if index > 0 {
            ids.push(ada.create_space(&format!("Space {index}")).id);
        }
        server.backend.started()[index].emit(Event::Closed { reason: None });
        ada.space(|state| state.activity == Activity::Closed);
        ada.send(&ClientMessage::Leave);
    }
    ada.send(&ClientMessage::ListSpaces);
    let listed: Vec<String> = ada.wait_for(|message| match message {
        ServerMessage::Spaces { spaces } if spaces.len() <= server::CLOSED_SPACES + 1 => {
            Some(spaces.iter().map(|space| space.id.clone()).collect())
        }
        _ => None,
    });
    let mut expected = vec![ids[0].clone()];
    expected.extend(ids[count - server::CLOSED_SPACES..].iter().cloned());
    assert_eq!(listed, expected);
}

#[test]
fn input_to_a_closed_space_fails_visibly() {
    let server = TestServer::with_backend(FakeBackend::manual());
    let (mut ada, _bob, space) = pair(&server);
    let lobby = server.connect("carol", "Carol");
    ada.say("u1", "first");
    ada.say("u2", "second");
    ada.space(|state| {
        state
            .queue
            .iter()
            .any(|entry| entry.text == "second" && entry.state == QueueState::Queued)
            && state
                .queue
                .iter()
                .any(|entry| entry.text == "first" && entry.state == QueueState::Delivered)
    });

    // The session ends: queued input fails instead of waiting forever, and the space shows it is closed.
    let session = server.backend.started()[0].clone();
    session.emit(Event::Closed {
        reason: Some("agent exited".to_owned()),
    });
    let state = ada.space(|state| state.activity == Activity::Closed);
    assert_eq!(state.error.as_deref(), Some("agent exited"));
    let states: Vec<(&str, QueueState)> = state
        .queue
        .iter()
        .map(|entry| (entry.text.as_str(), entry.state))
        .collect();
    assert_eq!(
        states,
        [
            ("first", QueueState::Delivered),
            ("second", QueueState::Failed)
        ]
    );
    lobby.wait_for(|message| match message {
        ServerMessage::Spaces { spaces } => spaces
            .iter()
            .any(|summary| summary.id == space && summary.activity == Activity::Closed)
            .then_some(()),
        _ => None,
    });

    // New input is refused rather than queued.
    ada.say("u3", "third");
    let error = ada.wait_for(|message| match message {
        ServerMessage::TranscriptFailed { utterance, error } if utterance == "u3" => {
            Some(error.clone())
        }
        _ => None,
    });
    assert!(error.contains("ended"), "{error}");
    let window = Duration::from_millis(200);
    while let Some(message) = ada.next_message(window) {
        if let ServerMessage::Space { state } = message {
            assert!(!state.queue.iter().any(|entry| entry.text == "third"));
        }
    }
    assert_eq!(session.log().prompts.len(), 1);
}

/// Wait until `backend` has `count` sessions whose creation is held.
fn wait_creating(backend: &FakeBackend, count: u64) {
    let deadline = std::time::Instant::now() + TIMEOUT;
    while backend.creating() != count {
        assert!(
            std::time::Instant::now() < deadline,
            "creation never started"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn shutdown_completes_while_a_space_is_being_created() {
    let backend = FakeBackend::new();
    backend.hold_creation();
    let server = TestServer::with_backend(backend.clone());
    let mut ada = server.connect("ada", "Ada");
    ada.send(&ClientMessage::CreateSpace {
        title: "Slow".to_owned(),
        backend: "fake".to_owned(),
        directory: "/tmp".to_owned(),
        session: None,
    });
    wait_creating(&backend, 1);

    // Shut down, and let the session finish starting only once shutdown has begun.
    let (done, stopped) = std::sync::mpsc::channel();
    let stopping = std::thread::spawn(move || {
        drop(server);
        done.send(()).unwrap();
    });
    assert!(ada.is_closed(TIMEOUT));
    backend.release_creation();
    stopped
        .recv_timeout(TIMEOUT)
        .expect("shutdown waited for ever on the new space");
    stopping.join().unwrap();
    let session = backend.started()[0].clone();
    assert!(session.log().closed);
}

#[test]
fn a_creator_who_moved_on_is_not_moved_into_the_new_space() {
    let backend = FakeBackend::new();
    let server = TestServer::with_backend(backend.clone());
    let mut ada = server.connect("ada", "Ada");
    let mut bob = server.connect("bob", "Bob");
    let other = bob.create_space("Other").id;

    backend.hold_creation();
    ada.send(&ClientMessage::CreateSpace {
        title: "Slow".to_owned(),
        backend: "fake".to_owned(),
        directory: "/tmp".to_owned(),
        session: None,
    });
    wait_creating(&backend, 1);
    join(&mut ada, &other, 2);
    backend.release_creation();

    // The space is created empty, and Ada stays in the space she went to.
    let carol = server.connect("carol", "Carol");
    let participants = carol.wait_for(|message| match message {
        ServerMessage::Spaces { spaces } => spaces
            .iter()
            .find(|space| space.title == "Slow")
            .map(|space| space.participants),
        _ => None,
    });
    assert_eq!(participants, 0);
    let window = Duration::from_millis(200);
    while let Some(message) = ada.next_message(window) {
        if let ServerMessage::Space { state } = message {
            assert_eq!(state.id, other, "Ada was moved into the new space");
        }
    }
}

#[test]
fn a_repeated_utterance_is_queued_once() {
    let server = TestServer::with_backend(FakeBackend::manual());
    let (mut ada, _bob, _) = pair(&server);
    let finished = |client: &TestClient, id: &str| {
        client.wait_for(|message| match message {
            ServerMessage::Transcript {
                utterance,
                text,
                finished: true,
            } if utterance == id => Some(text.clone()),
            ServerMessage::TranscriptFailed { utterance, error } if utterance == id => {
                panic!("{id} failed: {error}")
            }
            _ => None,
        })
    };
    ada.say("u1", "first");
    ada.say("u2", "second");
    assert_eq!(finished(&ada, "u2"), "second");
    // The client finalizes the same utterance again, as after a reconnect. It is acknowledged with the text queued the first time, and not queued again.
    ada.say("u2", "second retried");
    assert_eq!(finished(&ada, "u2"), "second");
    ada.say("u3", "third");
    // Messages on one connection are handled in order, so this snapshot follows the repeat.
    let state = ada.space(|state| state.queue.iter().any(|entry| entry.text == "third"));
    let texts: Vec<&str> = state
        .queue
        .iter()
        .map(|entry| entry.text.as_str())
        .collect();
    assert_eq!(texts, ["first", "second", "third"]);
}

#[test]
fn utterances_of_different_participants_never_share_a_key() {
    let server = TestServer::with_backend(FakeBackend::manual());
    let mut first = server.connect("a", "First");
    let mut second = server.connect("a/b", "Second");
    let space = first.create_space("Keys").id;
    join(&mut second, &space, 2);
    first.space(|state| state.participants.len() == 2);
    let finished = |client: &TestClient, id: &str| {
        client.wait_for(|message| match message {
            ServerMessage::Transcript {
                utterance,
                text,
                finished: true,
            } if utterance == id => Some(text.clone()),
            ServerMessage::TranscriptFailed { utterance, error } if utterance == id => {
                panic!("{id} failed: {error}")
            }
            _ => None,
        })
    };
    // Joined with `/`, both would be `a/b/c`.
    first.say("b/c", "from first");
    assert_eq!(finished(&first, "b/c"), "from first");
    second.say("c", "from second");
    assert_eq!(finished(&second, "c"), "from second");
    let state = first.space(|state| state.queue.len() == 2);
    let texts: Vec<&str> = state
        .queue
        .iter()
        .map(|entry| entry.text.as_str())
        .collect();
    assert_eq!(texts, ["from first", "from second"]);
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
        options: Vec::new(),
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
        option: None,
    });
    bob.send(&ClientMessage::Decide {
        request: approval.id.clone(),
        decision: Decision::Deny,
        option: None,
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
        option: None,
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
            options: Vec::new(),
        }));
    }

    // The oldest request is presented first, and the second waits its turn.
    let state = bob.space(|state| state.approval.as_ref().is_some_and(|a| a.id == "p1"));
    assert_eq!(state.activity, Activity::Waiting);
    ada.send(&ClientMessage::Decide {
        request: "p2".to_owned(),
        decision: Decision::Deny,
        option: None,
    });
    assert_eq!(ada.error(), "That approval is no longer pending.");

    ada.send(&ClientMessage::Decide {
        request: "p1".to_owned(),
        decision: Decision::AllowOnce,
        option: None,
    });
    let state = bob.space(|state| state.approval.as_ref().is_some_and(|a| a.id == "p2"));
    assert_eq!(state.activity, Activity::Waiting);
    ada.send(&ClientMessage::Decide {
        request: "p2".to_owned(),
        decision: Decision::Deny,
        option: None,
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
fn an_offered_option_is_granted() {
    let server = TestServer::start();
    let (mut ada, bob, _) = pair(&server);
    let session = server.backend.started()[0].clone();
    session.emit(Event::PermissionRequested(Permission {
        id: "p1".to_owned(),
        tool: "Bash".to_owned(),
        input: serde_json::json!({"command": "touch marker.txt"}),
        description: None,
        options: vec![backend::PermissionOption {
            id: "suggestions".to_owned(),
            grants: vec![backend::Grant::Rule {
                tool: "Bash".to_owned(),
                content: Some("touch marker.txt".to_owned()),
                scope: backend::GrantScope::Local,
            }],
        }],
    }));

    let state = bob.space(|state| state.approval.is_some());
    assert_eq!(
        state.approval.unwrap().options,
        [protocol::ApprovalOption {
            id: "suggestions".to_owned(),
            label: "Yes, and don't ask again for Bash(touch marker.txt) in this project".to_owned(),
        }]
    );
    ada.space(|state| state.approval.is_some());

    // Malformed or unoffered choices are refused and leave the approval pending.
    let decide = |decision, option: Option<&str>| ClientMessage::Decide {
        request: "p1".to_owned(),
        decision,
        option: option.map(str::to_owned),
    };
    ada.send(&decide(Decision::Grant, None));
    assert_eq!(ada.error(), "Choose an option to grant.");
    ada.send(&decide(Decision::AllowOnce, Some("suggestions")));
    assert_eq!(ada.error(), "Only a grant decision takes an option.");
    ada.send(&decide(Decision::Grant, Some("always")));
    assert_eq!(ada.error(), "That option is not offered for this approval.");
    assert!(session.log().decisions.is_empty());

    ada.send(&decide(Decision::Grant, Some("suggestions")));
    bob.space(|state| state.approval.is_none() && state.activity != Activity::Waiting);
    assert_eq!(
        session.log().decisions,
        [(
            "p1".to_owned(),
            backend::Decision::Grant {
                option: "suggestions".to_owned()
            }
        )]
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
fn sequence_numbers_skipped_before_an_utterance_do_not_fail_it() {
    let server = TestServer::start();
    let (mut ada, _bob, _) = pair(&server);
    ada.send_audio(Gate::Live, speech("ab"));
    // Frames the client did not send, while its gate was off or because congested live audio was dropped.
    ada.skip_sequence(5);
    ada.say("u1", "hello");
    let text = ada.wait_for(|message| match message {
        ServerMessage::Transcript {
            utterance,
            text,
            finished: true,
        } if utterance == "u1" => Some(text.clone()),
        ServerMessage::TranscriptFailed { utterance, error } if utterance == "u1" => {
            panic!("u1 failed: {error}")
        }
        _ => None,
    });
    assert_eq!(text, "hello");
}

#[test]
fn skipped_live_frames_within_an_utterance_do_not_fail_it() {
    let server = TestServer::start();
    let (mut ada, _bob, _) = pair(&server);
    ada.send(&ClientMessage::UtteranceBegin {
        utterance: "u1".to_owned(),
    });
    ada.send_audio(Gate::Agent, speech("he"));
    // The gate switches to live and back; some live frames were not sent.
    ada.skip_sequence(2);
    ada.send_audio(Gate::Live, speech("--"));
    ada.skip_sequence(3);
    ada.send_audio(Gate::Agent, speech("llo"));
    ada.send(&ClientMessage::UtteranceEnd {
        utterance: "u1".to_owned(),
    });
    let text = ada.wait_for(|message| match message {
        ServerMessage::Transcript {
            utterance,
            text,
            finished: true,
        } if utterance == "u1" => Some(text.clone()),
        ServerMessage::TranscriptFailed { utterance, error } if utterance == "u1" => {
            panic!("u1 failed: {error}")
        }
        _ => None,
    });
    assert_eq!(text, "hello");
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

#[test]
fn a_streaming_reply_sends_paced_snapshots_ending_with_the_latest_state() {
    let server = TestServer::with_backend(FakeBackend::manual());
    let (ada, _bob, _) = pair(&server);
    let session = server.backend.started()[0].clone();
    // Drain the snapshots of joining.
    while ada.next_message(Duration::from_millis(200)).is_some() {}

    // About 250 deltas over half a second: unpaced, nearly every one would produce a full snapshot.
    let message = session.start_message();
    let started = std::time::Instant::now();
    for _ in 0..250 {
        session.delta(&message, "x");
        std::thread::sleep(Duration::from_millis(2));
    }
    let elapsed = started.elapsed();

    let mut snapshots = 0;
    let state = ada.wait_for(|message| match message {
        ServerMessage::Space { state } => {
            snapshots += 1;
            state
                .messages
                .last()
                .is_some_and(|last| last.text.len() == 250)
                .then(|| (**state).clone())
        }
        _ => None,
    });
    // The latest state arrives without a further event to carry it.
    assert_eq!(state.messages.last().unwrap().text, "x".repeat(250));
    // At most one snapshot per interval, plus the first and the deferred last one.
    let limit = (elapsed.as_millis() / 100) as usize + 3;
    assert!(
        snapshots <= limit,
        "{snapshots} snapshots in {elapsed:?}, expected at most {limit}"
    );
    assert!(snapshots >= 2, "snapshots were not sent while streaming");
}

/// End `client`'s connection and wait until the server has released it.
fn hang_up(mut client: TestClient) {
    client.finish();
    assert!(client.is_closed(TIMEOUT));
}

#[test]
fn leaving_right_after_hello_overrides_the_rejoin() {
    let server = TestServer::start();
    let (ada, bob, space) = pair(&server);
    hang_up(ada);
    bob.space(|state| state.participants.len() == 1);

    // Ada left while disconnected; her client says so as soon as it is welcomed, without waiting for the rejoin.
    let mut ada = TestClient::open(server.address);
    ada.hello("ada", "Ada");
    ada.send(&ClientMessage::Leave);
    ada.wait_for(|message| matches!(message, ServerMessage::Left).then_some(()));
    let spaces = ada.wait_for(|message| match message {
        ServerMessage::Spaces { spaces } => Some(spaces.clone()),
        _ => None,
    });
    let listed = spaces.iter().find(|listed| listed.id == space).unwrap();
    assert_eq!(listed.participants, 1);
    bob.space(|state| state.participants.len() == 1);

    // The leave is remembered: reconnecting again does not rejoin.
    hang_up(ada);
    let again = server.connect("ada", "Ada");
    again.wait_for(|message| match message {
        ServerMessage::Spaces { .. } => Some(()),
        ServerMessage::Space { .. } => panic!("rejoined a space that was left"),
        _ => None,
    });

    // Leaving while in the lobby is harmless.
    let mut carol = server.connect("carol", "Carol");
    carol.send(&ClientMessage::Leave);
    carol.wait_for(|message| matches!(message, ServerMessage::Left).then_some(()));
    carol.wait_for(|message| match message {
        ServerMessage::Spaces { .. } => Some(()),
        ServerMessage::Error { error } => panic!("leaving the lobby failed: {error}"),
        _ => None,
    });
}

#[test]
fn a_participant_gone_longer_than_the_retention_is_forgotten() {
    let retention = Duration::from_millis(800);
    let server = TestServer::with_server(FakeBackend::new(), move |backends| {
        server::Server::new(
            backends,
            std::sync::Arc::new(server::test_support::TextTranscriber::default()),
        )
        .with_participant_retention(retention)
    });
    let (ada, bob, space) = pair(&server);
    let source = ada.source;

    // Within the retention, Ada keeps her source and rejoins.
    hang_up(ada);
    let ada = server.connect("ada", "Ada");
    assert_eq!(ada.source, source);
    ada.space(|state| state.id == space);

    // After it, she is a new participant in the lobby.
    hang_up(ada);
    bob.space(|state| state.participants.len() == 1);
    std::thread::sleep(retention * 2);
    let ada = server.connect("ada", "Ada");
    assert_ne!(ada.source, source);
    ada.wait_for(|message| match message {
        ServerMessage::Spaces { .. } => Some(()),
        ServerMessage::Space { .. } => panic!("a forgotten participant rejoined"),
        _ => None,
    });
}
