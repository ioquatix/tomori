// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

//! The conversation's tests: connecting, the lobby and joining, the hand menu's sections, the agent's questions and
//! replies, the transcript, and where the agent and its workers stand.

use super::*;
use std::time::Duration;
use tomori_client::server_choice::{Source, USB};
use tomori_protocol::TranscriptStage;
use tomori_protocol::{
    AgentInstructions, Approval, ApprovalOption, Message, Participant, Question, QueueEntry,
};

const SPACE: &str = "space-1";

fn connected() -> Event {
    connected_with(vec![], vec![])
}

fn connected_with(backends: Vec<BackendInfo>, roots: Vec<WorkspaceRoot>) -> Event {
    Event::State(ConnectionState::Connected {
        source: 1,
        server: "studio".into(),
        role: ConnectionRole::Owner,
        certificate: None,
        backends,
        roots,
        environments: sky(),
        utterance_limit: None,
        name: None,
    })
}

/// The environments today's server offers.
fn sky() -> Vec<EnvironmentInfo> {
    vec![EnvironmentInfo {
        id: "sky".into(),
        label: "Sky".into(),
        version: 1,
    }]
}

/// A backend list for the menu tests.
fn backends() -> Vec<BackendInfo> {
    vec![
        BackendInfo {
            name: "fake".into(),
            attach: true,
            avatar: None,
        },
        BackendInfo {
            name: "plain".into(),
            attach: false,
            avatar: None,
        },
    ]
}

/// A directory list for the menu tests.
fn roots() -> Vec<WorkspaceRoot> {
    vec![WorkspaceRoot {
        path: "/work/tomori".into(),
        label: "tomori".into(),
    }]
}

/// Connected to a server that offers `backends` and `roots`, in the lobby.
fn in_lobby(backends: Vec<BackendInfo>, roots: Vec<WorkspaceRoot>) -> Conversation {
    let mut conversation = crate::test_support::conversation("Ada");
    conversation.connect(Choice::new("Studio", "10.0.0.1:8765", Source::Discovered));
    conversation.take_actions();
    conversation.apply(connected_with(backends, roots));
    conversation.take_actions();
    conversation
}

fn space(change: impl FnOnce(&mut SpaceState)) -> Event {
    let mut state = SpaceState {
        id: SPACE.into(),
        title: "Tomori".into(),
        backend: "opencode".into(),
        directory: "/work/tomori".into(),
        environment: "sky".into(),
        activity: Activity::Idle,
        participants: vec![Participant {
            name: "Ada".into(),
            source: 1,
            engagement: None,
        }],
        ..Default::default()
    };
    change(&mut state);
    Event::Message(ServerMessage::Space {
        state: Box::new(state),
    })
}

fn message(id: &str, role: Role, text: &str) -> Message {
    Message {
        id: id.into(),
        role,
        text: text.into(),
        completed: true,
    }
}

fn approval(options: &[(&str, &str)]) -> Approval {
    Approval {
        id: "request-1".into(),
        title: "Bash".into(),
        text: "cargo test".into(),
        options: options
            .iter()
            .map(|(id, label)| ApprovalOption {
                id: (*id).into(),
                label: (*label).into(),
            })
            .collect(),
    }
}

/// An entry spoken by `speaker`. Ada is this client, participant 1 as connected in these tests; anyone else is participant 2.
fn entry(id: &str, speaker: &str, state: QueueState) -> QueueEntry {
    QueueEntry {
        id: id.into(),
        source: if speaker == "Ada" { 1 } else { 2 },
        speaker: speaker.into(),
        text: format!("text of {id}"),
        state,
    }
}

fn sent(conversation: &mut Conversation) -> Vec<ClientMessage> {
    conversation
        .take_actions()
        .into_iter()
        .filter_map(|action| match action {
            Action::Send(message) => Some(message),
            _ => None,
        })
        .collect()
}

/// The server's refusal of a join of `space`.
fn refused(space: &str) -> Event {
    Event::Message(ServerMessage::Error {
        error: "That space does not exist.".into(),
        space: Some(space.into()),
        panel: None,
        wall: None,
    })
}

/// A conversation connected to a server and joined to [`SPACE`], with any request it shows there long enough to be chosen.
#[test]
fn a_ping_or_pong_changes_nothing_and_presence_keeps_only_the_others_right_hands_in_a_space() {
    let mut conversation = joined(|_| {});
    let before = conversation.right_hands();
    conversation.apply(Event::Message(ServerMessage::Ping { nonce: 1 }));
    conversation.apply(Event::Message(ServerMessage::Pong { nonce: 1 }));
    assert_eq!(conversation.right_hands(), before);
    assert!(conversation.take_actions().is_empty());
    // Outside a space, a right hand is not kept.
    let mut lobby = Conversation::new("Ada");
    lobby.apply(Event::Message(ServerMessage::Presence {
        source: 2,
        head: tomori_protocol::Pose::HOME,
        left: None,
        right: Some(tomori_protocol::Pose::HOME),
        hands: None,
        pointing: None,
    }));
    assert!(lobby.people.is_empty());
}

fn joined(change: impl FnOnce(&mut SpaceState)) -> Conversation {
    let mut conversation = crate::test_support::conversation("Ada");
    conversation.connect(Choice::new("Studio", "10.0.0.1:8765", Source::Discovered));
    conversation.apply(connected());
    conversation.apply(space(change));
    conversation.take_actions();
    settle(&mut conversation);
    conversation
}

/// Let the request shown have been there for longer than [`REQUEST_GRACE`].
fn settle(conversation: &mut Conversation) {
    conversation.request_at = None;
}

/// Hold B long enough to leave a space, then release it.
fn hold_b(conversation: &mut Conversation) {
    conversation.hold_back(Some(Duration::ZERO));
    conversation.hold_back(Some(LEAVE_HOLD));
    conversation.hold_back(None);
}

/// Hold B for less than it takes to leave, then release it.
fn hold_b_briefly(conversation: &mut Conversation) {
    conversation.hold_back(Some(LEAVE_HOLD / 2));
    conversation.hold_back(None);
}

fn assert_fits(conversation: &mut Conversation) {
    let text = conversation.text();
    assert!(text.lines().count() <= ROWS, "{text}");
    assert!(
        text.lines().all(|line| line.chars().count() <= COLUMNS),
        "{text}"
    );
}

#[test]
fn connects_lists_and_joins() {
    let mut conversation = crate::test_support::conversation("Ada");
    assert!(conversation.is_browsing());
    conversation.set_servers(vec![
        Choice::new("Studio", "10.0.0.1:8765", Source::Discovered),
        Choice::new("USB", USB, Source::Usb),
    ]);
    assert_fits(&mut conversation);
    conversation.navigate(0, 1);
    assert!(!conversation.accept());
    assert_eq!(
        conversation.take_actions(),
        [Action::Connect(Choice::new("USB", USB, Source::Usb))]
    );
    assert_eq!(conversation.mode(), Mode::Lobby);
    assert!(!conversation.is_browsing());

    conversation.apply(connected());
    assert_eq!(sent(&mut conversation), [ClientMessage::ListSpaces]);
    conversation.apply(Event::Message(ServerMessage::Spaces {
        spaces: vec![SpaceSummary {
            id: SPACE.into(),
            title: "Tomori".into(),
            backend: "opencode".into(),
            directory: "/work/tomori".into(),
            environment: "sky".into(),
            activity: Activity::Running,
            participants: 2,
            attention: None,
            attention_age: None,
            folder: None,
            active_age: None,
            latest: None,
        }],
    }));
    assert!(conversation.text().contains("> Tomori [running, 2 here]"));
    assert_fits(&mut conversation);
    assert!(!conversation.accept());
    assert_eq!(
        sent(&mut conversation),
        [ClientMessage::Join {
            space: SPACE.into()
        }]
    );
    conversation.apply(space(|_| {}));
    assert_eq!(conversation.mode(), Mode::Space);
}

fn listed(conversation: &mut Conversation, spaces: Vec<SpaceSummary>) {
    conversation.apply(Event::Message(ServerMessage::Spaces { spaces }));
}

fn summary(id: &str, title: &str) -> SpaceSummary {
    SpaceSummary {
        id: id.into(),
        title: title.into(),
        backend: "fake".into(),
        directory: "/work/tomori".into(),
        activity: Activity::Idle,
        participants: 1,
        attention: None,
        attention_age: None,
        folder: None,
        active_age: None,
        latest: None,
        environment: "sky".into(),
    }
}

/// A space listed as needing `attention` for `age` seconds.
fn needing(id: &str, title: &str, attention: tomori_protocol::Attention, age: u32) -> SpaceSummary {
    SpaceSummary {
        attention: Some(attention),
        attention_age: Some(age),
        participants: 0,
        ..summary(id, title)
    }
}

/// A discovered server advertising that `waiting` of its spaces need input and `replies` have a reply.
fn advertising(label: &str, address: &str, waiting: u32, replies: u32) -> Choice {
    let mut choice = Choice::new(label, address, Source::Discovered);
    choice.spaces = Some(3);
    choice.participants = Some(0);
    choice.waiting = Some(waiting);
    choice.problems = Some(0);
    choice.replies = Some(replies);
    choice
}

#[test]
fn a_server_needing_attention_glows_and_offers_a_jump() {
    let mut conversation = crate::test_support::conversation("Ada");
    conversation.set_servers(vec![
        advertising("Laptop", "10.0.0.2:8765", 1, 2),
        advertising("Desktop", "10.0.0.1:8765", 0, 0),
    ]);
    let view = conversation.arrival().unwrap();
    let keys: Vec<_> = view.items.iter().map(|item| item.key.clone()).collect();
    assert_eq!(
        keys,
        [
            arrival::Key::Server("10.0.0.2:8765".into()),
            arrival::Key::Jump("10.0.0.2:8765".into()),
            arrival::Key::Server("10.0.0.1:8765".into()),
        ]
    );
    assert_eq!(
        view.items[0].kind,
        arrival::Kind::Server {
            source: Source::Discovered,
            available: true,
            attention: Some(tomori_protocol::Attention::Input),
        }
    );
    assert_eq!(
        view.items[0].detail.as_deref(),
        Some("1 needs you, 2 replies")
    );
    assert_eq!(view.items[2].detail.as_deref(), Some("3 spaces, 0 people"));
    assert!(
        conversation
            .text()
            .contains("Laptop (1 needs you, 2 replies)")
    );
    assert_fits(&mut conversation);
}

#[test]
fn a_jump_connects_and_joins_the_most_urgent_space_in_one_choice() {
    crate::test_support::log_everything();
    let mut conversation = crate::test_support::conversation("Ada");
    let laptop = advertising("Laptop", "10.0.0.2:8765", 1, 1);
    conversation.set_servers(vec![laptop.clone()]);
    conversation.choose_object(&arrival::Key::Jump("10.0.0.2:8765".into()));
    assert_eq!(
        conversation.take_actions(),
        [Action::Connect(laptop.clone())]
    );
    assert_eq!(conversation.server(), Some(&laptop));
    conversation.apply(connected());
    assert_eq!(sent(&mut conversation), [ClientMessage::ListSpaces]);
    assert!(
        conversation
            .arrival()
            .unwrap()
            .status
            .unwrap()
            .contains("Jumping to what needs you on Laptop")
    );
    listed(
        &mut conversation,
        vec![
            summary("idle", "Idle"),
            needing("reply", "Notes", tomori_protocol::Attention::Reply, 600),
            needing("deploy", "Deploy", tomori_protocol::Attention::Input, 30),
        ],
    );
    assert_eq!(
        sent(&mut conversation),
        [ClientMessage::Join {
            space: "deploy".into()
        }]
    );
    // The space comes towards the participant as it is joined, as when chosen by hand.
    assert_eq!(
        conversation.arrival().unwrap().joining,
        Some(arrival::Key::Space("deploy".into()))
    );
    // A later listing does not jump again.
    listed(&mut conversation, vec![summary("idle", "Idle")]);
    assert!(sent(&mut conversation).is_empty());
    conversation.apply(space(|state| state.id = "deploy".into()));
    assert_eq!(conversation.mode(), Mode::Space);
}

#[test]
fn a_jump_whose_attention_was_dealt_with_stays_in_the_lobby_and_says_so() {
    let mut conversation = crate::test_support::conversation("Ada");
    conversation.set_servers(vec![advertising("Laptop", "10.0.0.2:8765", 1, 0)]);
    conversation.choose_object(&arrival::Key::Jump("10.0.0.2:8765".into()));
    conversation.apply(connected());
    conversation.take_actions();
    listed(&mut conversation, vec![summary("deploy", "Deploy")]);
    assert!(sent(&mut conversation).is_empty());
    assert_eq!(conversation.mode(), Mode::Lobby);
    assert_eq!(
        conversation.arrival().unwrap().status.as_deref(),
        Some("Nothing on Laptop needs you now.")
    );
    assert_fits(&mut conversation);
}

#[test]
fn a_jump_moves_on_from_a_space_the_server_rejoined() {
    let mut conversation = crate::test_support::conversation("Ada");
    conversation.set_servers(vec![advertising("Laptop", "10.0.0.2:8765", 0, 1)]);
    conversation.choose_object(&arrival::Key::Jump("10.0.0.2:8765".into()));
    conversation.apply(connected());
    conversation.take_actions();
    // The server put this participant back in the space they were last in, before listing.
    conversation.apply(space(|_| {}));
    assert_eq!(conversation.mode(), Mode::Space);
    listed(
        &mut conversation,
        vec![
            summary(SPACE, "Tomori"),
            needing("notes", "Notes", tomori_protocol::Attention::Reply, 5),
        ],
    );
    assert_eq!(
        sent(&mut conversation),
        [ClientMessage::Join {
            space: "notes".into()
        }]
    );
    conversation.apply(space(|state| {
        state.id = "notes".into();
        state.title = "Notes".into();
    }));
    assert_eq!(
        conversation.entered(),
        Some(arrival::Key::Space("notes".into()))
    );

    // Already put in the space that needs attention: joining it says it has been seen.
    let mut conversation = crate::test_support::conversation("Ada");
    conversation.set_servers(vec![advertising("Laptop", "10.0.0.2:8765", 0, 1)]);
    conversation.choose_object(&arrival::Key::Jump("10.0.0.2:8765".into()));
    conversation.apply(connected());
    conversation.take_actions();
    conversation.apply(space(|_| {}));
    assert!(sent(&mut conversation).is_empty());
    listed(
        &mut conversation,
        vec![needing(
            SPACE,
            "Tomori",
            tomori_protocol::Attention::Input,
            5,
        )],
    );
    assert_eq!(
        sent(&mut conversation),
        [ClientMessage::Join {
            space: SPACE.into()
        }]
    );
    conversation.apply(space(|_| {}));
    assert_eq!(conversation.mode(), Mode::Space);
    assert!(conversation.arrival().is_none());
}

#[test]
fn a_space_the_server_puts_you_in_unasked_is_joined_as_your_choice() {
    // A fresh launch connects, and the server puts the participant back in their last space.
    let mut conversation = crate::test_support::conversation("Ada");
    conversation.connect(Choice::new("Studio", "10.0.0.1:8765", Source::Discovered));
    conversation.apply(connected());
    conversation.take_actions();
    conversation.apply(space(|_| {}));
    assert_eq!(conversation.mode(), Mode::Space);
    // Joining it clears what it needed while nobody was there, now that the participant sees it.
    assert_eq!(
        sent(&mut conversation),
        [ClientMessage::Join {
            space: SPACE.into()
        }]
    );
    conversation.apply(space(|_| {}));
    assert!(sent(&mut conversation).is_empty());
    // A space joined on request is not joined again.
    conversation.back();
    conversation.take_actions();
    conversation.apply(Event::Message(ServerMessage::LEFT));
    listed(&mut conversation, vec![summary(SPACE, "Tomori")]);
    conversation.accept();
    conversation.take_actions();
    conversation.apply(space(|_| {}));
    assert!(sent(&mut conversation).is_empty());
}

#[test]
fn a_late_update_from_a_rejoined_space_does_not_lose_the_jump() {
    let mut conversation = crate::test_support::conversation("Ada");
    conversation.set_servers(vec![advertising("Laptop", "10.0.0.2:8765", 0, 1)]);
    conversation.choose_object(&arrival::Key::Jump("10.0.0.2:8765".into()));
    conversation.apply(connected());
    conversation.apply(space(|_| {}));
    listed(
        &mut conversation,
        vec![
            summary(SPACE, "Tomori"),
            needing("notes", "Notes", tomori_protocol::Attention::Reply, 5),
        ],
    );
    conversation.take_actions();
    // The space the server rejoined changes once more before the join is answered.
    conversation.apply(space(|state| state.activity = Activity::Running));
    conversation.apply(refused("notes"));
    assert_eq!(conversation.mode(), Mode::Lobby);
    assert_eq!(
        conversation.arrival().unwrap().status.as_deref(),
        Some("Notes has ended.")
    );
}

#[test]
fn a_jump_to_a_space_that_has_gone_says_it_ended() {
    let mut conversation = crate::test_support::conversation("Ada");
    conversation.set_servers(vec![advertising("Laptop", "10.0.0.2:8765", 1, 0)]);
    conversation.choose_object(&arrival::Key::Jump("10.0.0.2:8765".into()));
    conversation.apply(connected());
    conversation.take_actions();
    listed(
        &mut conversation,
        vec![needing(
            "deploy",
            "Deploy",
            tomori_protocol::Attention::Input,
            1,
        )],
    );
    conversation.take_actions();
    conversation.apply(refused("deploy"));
    assert_eq!(conversation.mode(), Mode::Lobby);
    assert_eq!(
        conversation.arrival().unwrap().status.as_deref(),
        Some("Deploy has ended.")
    );
}

#[test]
fn another_choice_before_the_listing_takes_over_from_a_jump() {
    let mut conversation = crate::test_support::conversation("Ada");
    conversation.set_servers(vec![advertising("Laptop", "10.0.0.2:8765", 1, 0)]);
    conversation.choose_object(&arrival::Key::Jump("10.0.0.2:8765".into()));
    conversation.apply(connected());
    conversation.take_actions();
    // Going back to the servers and connecting by hand is no jump.
    conversation.back();
    assert_eq!(conversation.take_actions(), [Action::Disconnect]);
    conversation.choose_object(&arrival::Key::Server("10.0.0.2:8765".into()));
    conversation.apply(connected());
    conversation.take_actions();
    listed(
        &mut conversation,
        vec![
            summary("idle", "Idle"),
            needing("deploy", "Deploy", tomori_protocol::Attention::Input, 1),
        ],
    );
    assert!(sent(&mut conversation).is_empty());
    // The selection starts on the space that needs attention, so A joins it.
    assert!(conversation.text().contains("> Deploy [needs you]"));
    // A jump object for a server that cannot be joined does nothing.
    let mut conversation = crate::test_support::conversation("Ada");
    let mut old = advertising("Old", "10.0.0.3:8765", 1, 0);
    old.unavailable = Some("protocol 2, needs 1".into());
    conversation.set_servers(vec![old]);
    conversation.choose_object(&arrival::Key::Jump("10.0.0.3:8765".into()));
    assert!(conversation.take_actions().is_empty());
    assert_eq!(conversation.mode(), Mode::Servers);
}

#[test]
fn spaces_are_marked_in_place_and_the_selection_starts_on_the_most_urgent() {
    let mut conversation = in_lobby(backends(), roots());
    let spaces = vec![
        summary("idle", "Idle"),
        needing("notes", "Notes", tomori_protocol::Attention::Reply, 900),
        needing("broken", "Broken", tomori_protocol::Attention::Problem, 10),
        needing("deploy", "Deploy", tomori_protocol::Attention::Input, 20),
        needing("tests", "Tests", tomori_protocol::Attention::Input, 120),
    ];
    listed(&mut conversation, spaces.clone());
    let view = conversation.arrival().unwrap();
    // The listing's order is kept; only the selection moves.
    let keys: Vec<_> = view.items.iter().map(|item| item.key.clone()).collect();
    assert_eq!(
        keys[..5],
        spaces
            .iter()
            .map(|space| arrival::Key::Space(space.id.clone()))
            .collect::<Vec<_>>()
    );
    assert!(view.items[4].selected);
    let details: Vec<_> = view
        .items
        .iter()
        .take(5)
        .map(|item| {
            // Beneath the folder and environment, what the space needs, or else what its agent is doing.
            let detail = item.detail.clone().unwrap();
            let status = detail.lines().nth(1).unwrap();
            status.split(" · ").next().unwrap().to_owned()
        })
        .collect();
    assert_eq!(
        details,
        ["idle", "new reply", "problem", "needs you", "needs you"]
    );
    let arrival::Kind::Space {
        attention: Some(tomori_protocol::Attention::Problem),
        ..
    } = view.items[2].kind
    else {
        unreachable!("arrival::Kind::Space")
    };
    assert_eq!(view.status.as_deref(), Some("Tests needs you"));
    assert!(conversation.text().contains("Tests needs you"));
    assert_fits(&mut conversation);

    // A later listing keeps the participant's own selection, even as attention moves.
    conversation.navigate(0, -1);
    listed(&mut conversation, spaces.iter().rev().cloned().collect());
    assert!(conversation.text().contains("> Deploy [needs you]"));
    conversation.accept();
    assert_eq!(
        sent(&mut conversation),
        [ClientMessage::Join {
            space: "deploy".into()
        }]
    );
}

#[test]
fn the_remembered_server_is_not_connected_while_only_another_needs_attention() {
    crate::test_support::log_everything();
    let start = Instant::now();
    let mut conversation = crate::test_support::conversation("Ada");
    let mut remembered = advertising("Desktop", "10.0.0.1:8765", 0, 0);
    remembered.source = Source::Remembered;
    // Browsing finds the remembered server first, and the other a moment later.
    conversation.set_servers(vec![remembered.clone()]);
    conversation.tick(start);
    conversation.tick(start + AUTOMATIC_SETTLE / 2);
    assert!(conversation.take_actions().is_empty());
    conversation.set_servers(vec![
        remembered.clone(),
        advertising("Laptop", "10.0.0.2:8765", 1, 0),
    ]);
    conversation.tick(start + AUTOMATIC_SETTLE);
    assert!(conversation.take_actions().is_empty());
    assert_eq!(conversation.mode(), Mode::Servers);
    // Having held back, it gives up, so nothing connects later as the other's attention is dealt with.
    conversation.set_servers(vec![
        remembered.clone(),
        advertising("Laptop", "10.0.0.2:8765", 0, 0),
    ]);
    conversation.tick(start + AUTOMATIC_SETTLE * 4);
    assert!(conversation.take_actions().is_empty());

    // When the remembered server needs attention too, it is connected.
    let mut conversation = crate::test_support::conversation("Ada");
    remembered.replies = Some(1);
    conversation.set_servers(vec![
        remembered.clone(),
        advertising("Laptop", "10.0.0.2:8765", 1, 0),
    ]);
    conversation.tick(start);
    conversation.tick(start + AUTOMATIC_SETTLE);
    assert_eq!(conversation.take_actions(), [Action::Connect(remembered)]);
}

fn sessions() -> Event {
    Event::Message(ServerMessage::Sessions {
        sessions: vec![
            tomori_protocol::SessionSummary {
                id: "ses_1".into(),
                title: "Tomori opencode support".into(),
                directory: "/work/tomori".into(),
                busy: false,
            },
            tomori_protocol::SessionSummary {
                id: "ses_2".into(),
                title: "Other work".into(),
                directory: "/work/tomori".into(),
                busy: false,
            },
        ],
    })
}

fn keys(items: &[arrival::Item]) -> Vec<arrival::Key> {
    items.iter().map(|item| item.key.clone()).collect()
}

#[test]
fn the_create_object_is_shown_directly_when_there_are_no_spaces() {
    let mut conversation = in_lobby(backends(), roots());
    listed(&mut conversation, vec![]);
    let view = conversation.arrival().unwrap();
    assert_eq!(keys(&view.items), [arrival::Key::Create]);
    assert!(view.items[0].selected);
    assert_eq!(view.ring, None);
    assert_eq!(view.status.as_deref(), Some("No spaces yet; create one."));
    assert!(conversation.text().contains("> + New space"));
    assert_fits(&mut conversation);

    // With spaces, it sits after them in the same view.
    listed(&mut conversation, vec![summary("one", "One")]);
    let view = conversation.arrival().unwrap();
    assert_eq!(
        keys(&view.items),
        [arrival::Key::Space("one".into()), arrival::Key::Create]
    );
    // The create object stays selected as the spaces change.
    assert!(view.items[1].selected);
}

/// Told the role `role`, as the server does whenever it changes.
fn role(role: ConnectionRole) -> Event {
    Event::Message(ServerMessage::Role { role })
}

/// Connected over TLS with `certificate`, as `role`, in the lobby.
fn connected_securely(
    role: ConnectionRole,
    certificate: tomori_client::Fingerprint,
) -> Conversation {
    let mut conversation = crate::test_support::conversation("Ada");
    conversation.connect(Choice::new("Studio", "10.0.0.1:8765", Source::Discovered));
    conversation.take_actions();
    conversation.apply(Event::State(ConnectionState::Connected {
        source: 1,
        server: "studio".into(),
        role,
        certificate: Some(certificate),
        backends: vec![],
        roots: vec![],
        environments: sky(),
        utterance_limit: None,
        name: None,
    }));
    conversation.take_actions();
    conversation
}

fn pairing_label(conversation: &Conversation) -> Option<String> {
    conversation
        .arrival()
        .expect("arrival shows")
        .items
        .into_iter()
        .find(|item| item.key == arrival::Key::Pair)
        .map(|item| item.label)
}

#[test]
fn a_token_the_server_sends_as_the_participant_leaves_it_is_not_passed_on() {
    let certificate = tomori_client::Fingerprint::of(b"server certificate");
    let mut conversation = connected_securely(ConnectionRole::Visitor, certificate);
    conversation.choose_object(&arrival::Key::Pair);
    conversation.knock = Some(tomori_protocol::KnockState::Pending);
    conversation.take_actions();
    conversation.leave_server();
    // In the same frame, before the connection closes: the pairing completes, and the knock is answered.
    conversation.apply(Event::Message(ServerMessage::Paired {
        token: "tomori_paired".into(),
    }));
    conversation.apply(Event::Message(ServerMessage::Admitted {
        token: "tomori_admitted".into(),
    }));
    assert_eq!(conversation.take_actions(), [Action::Disconnect]);
    assert!(conversation.pairing.is_none() && conversation.notice.is_none());
}

#[test]
fn a_visitor_pairs_with_a_code_derived_from_the_certificate() {
    use tomori_security::pairing::{Nonce, code, display};
    let certificate = tomori_client::Fingerprint::of(b"server certificate");
    let mut conversation = connected_securely(ConnectionRole::Visitor, certificate);
    assert_eq!(pairing_label(&conversation).as_deref(), Some("Pair"));

    conversation.choose_object(&arrival::Key::Pair);
    let [ClientMessage::PairBegin { commitment }] = &sent(&mut conversation)[..] else {
        unreachable!("a commitment is sent");
    };
    let commitment = commitment.clone();
    assert_eq!(pairing_label(&conversation).as_deref(), Some("Pairing..."));
    // Debugged, the pairing holds neither its value nor, later, its code.
    let debugged = format!("{:?}", conversation.pairing);
    assert!(
        debugged.starts_with("Some(Committed([redacted], "),
        "{debugged}"
    );

    let server = Nonce::generate();
    conversation.apply(Event::Message(ServerMessage::PairNonce {
        nonce: server.to_hex(),
    }));
    let [ClientMessage::PairReveal { nonce }] = &sent(&mut conversation)[..] else {
        unreachable!("the value is revealed");
    };
    let device = Nonce::parse(nonce).unwrap();
    // The revealed value is the one committed to, and the code is derived from it, the server's and the certificate.
    assert_eq!(device.commitment(), commitment);
    let expected = display(&code(certificate, device, server));
    assert_eq!(pairing_label(&conversation), Some(expected.clone()));
    assert!(conversation.status().unwrap().contains(&expected));
    assert_eq!(conversation.menu_model().pairing, Some(expected.clone()));
    let debugged = format!("{:?}", conversation.pairing);
    assert!(
        debugged.starts_with("Some(Waiting([redacted], ") && !debugged.contains(&expected),
        "{debugged}"
    );
    // The server waits for the operator to confirm it: the code stays shown, and the device waits too.
    conversation.apply(Event::Message(ServerMessage::PairWaiting));
    conversation.expire_pairing(Instant::now());
    assert!(conversation.status().unwrap().contains(&expected));
    assert!(conversation.take_actions().is_empty());

    conversation.apply(Event::Message(ServerMessage::Paired {
        token: "tomori_paired".into(),
    }));
    assert_eq!(
        conversation.take_actions(),
        [Action::Paired {
            token: "tomori_paired".into(),
        }]
    );
    assert!(conversation.status().unwrap().contains("now an owner"));
}

#[test]
fn the_device_gives_up_on_a_pairing_left_unconfirmed() {
    let certificate = tomori_client::Fingerprint::of(b"server certificate");
    let mut conversation = connected_securely(ConnectionRole::Visitor, certificate);
    conversation.choose_object(&arrival::Key::Pair);
    conversation.take_actions();
    conversation.expire_pairing(Instant::now());
    assert!(conversation.take_actions().is_empty());
    conversation.expire_pairing(Instant::now() + PAIRING_PATIENCE);
    assert_eq!(sent(&mut conversation), [ClientMessage::PairCancel]);
    assert_eq!(
        conversation.status().as_deref(),
        Some("The pairing was not confirmed in time.")
    );
    assert_eq!(pairing_label(&conversation).as_deref(), Some("Pair"));
    conversation.expire_pairing(Instant::now() + PAIRING_PATIENCE);
    assert!(conversation.take_actions().is_empty());
}

#[test]
fn a_failed_or_cancelled_pairing_starts_over() {
    let certificate = tomori_client::Fingerprint::of(b"server certificate");
    let mut conversation = connected_securely(ConnectionRole::Visitor, certificate);
    conversation.choose_object(&arrival::Key::Pair);
    conversation.take_actions();
    conversation.apply(Event::Message(ServerMessage::PairFailed {
        error: "The pairing was declined.".into(),
    }));
    assert_eq!(
        conversation.status().as_deref(),
        Some("The pairing was declined.")
    );
    assert_eq!(pairing_label(&conversation).as_deref(), Some("Pair"));

    // Choosing it again while pairing cancels.
    conversation.choose_object(&arrival::Key::Pair);
    conversation.take_actions();
    conversation.choose_object(&arrival::Key::Pair);
    assert_eq!(sent(&mut conversation), [ClientMessage::PairCancel]);
    // A value from a server that was not asked is ignored.
    conversation.apply(Event::Message(ServerMessage::PairNonce {
        nonce: tomori_security::pairing::Nonce::generate().to_hex(),
    }));
    assert!(conversation.take_actions().is_empty());
}

#[test]
fn a_device_refused_because_pairing_is_closed_says_so_and_does_not_ask_again() {
    let certificate = tomori_client::Fingerprint::of(b"server certificate");
    let mut conversation = connected_securely(ConnectionRole::Visitor, certificate);
    conversation.choose_object(&arrival::Key::Pair);
    conversation.take_actions();
    conversation.apply(Event::Message(ServerMessage::PairFailed {
        error: tomori_protocol::PAIRING_CLOSED.into(),
    }));
    let item = conversation
        .arrival()
        .unwrap()
        .items
        .into_iter()
        .find(|item| item.key == arrival::Key::Pair)
        .unwrap();
    assert_eq!(
        (item.label.as_str(), item.detail.as_deref()),
        ("Pairing is closed", Some("Ask an owner to open it"))
    );
    assert_eq!(
        conversation.menu_model().pairing.as_deref(),
        Some("Pairing is closed")
    );
    assert_eq!(
        conversation.status().as_deref(),
        Some(tomori_protocol::PAIRING_CLOSED)
    );
    // Nothing asks again by itself, however long it waits.
    conversation.expire_pairing(Instant::now() + PAIRING_PATIENCE * 2);
    assert!(conversation.take_actions().is_empty());
    // Chosen again, it asks again.
    conversation.choose_object(&arrival::Key::Pair);
    assert!(matches!(
        &sent(&mut conversation)[..],
        [ClientMessage::PairBegin { .. }]
    ));
    assert_eq!(pairing_label(&conversation).as_deref(), Some("Pairing..."));
    // Another refusal is shown as it is.
    conversation.apply(Event::Message(ServerMessage::PairFailed {
        error: "The pairing was declined.".into(),
    }));
    assert_eq!(pairing_label(&conversation).as_deref(), Some("Pair"));
}

#[test]
fn an_owner_opens_and_closes_pairing_from_the_hand_menu() {
    let certificate = tomori_client::Fingerprint::of(b"server certificate");
    let mut owner = connected_securely(ConnectionRole::Owner, certificate);
    // A server that does not say offers nothing.
    assert_eq!(owner.menu_model().pairing_window, None);
    owner.toggle_pairing_window();
    assert!(sent(&mut owner).is_empty());
    assert_eq!(
        owner.menu_model().notice.as_deref(),
        Some("This server does not open pairing for new devices.")
    );

    owner.apply(Event::Message(ServerMessage::PairingWindow {
        closes_in: None,
    }));
    assert_eq!(owner.menu_model().pairing_window, Some(None));
    owner.choose_menu(crate::hand_menu::Choice::PairingWindow);
    assert_eq!(sent(&mut owner), [ClientMessage::OpenPairing]);
    assert_eq!(
        owner.menu_model().notice.as_deref(),
        Some("Opening pairing for a new device...")
    );
    owner.apply(Event::Message(ServerMessage::PairingWindow {
        closes_in: Some(120),
    }));
    let now = Instant::now();
    let left = owner.pairing_window_left(now).unwrap().unwrap();
    assert!((119..=120).contains(&left), "{left}");
    assert_eq!(
        owner.pairing_window_left(now + Duration::from_secs(200)),
        Some(Some(0))
    );
    assert_eq!(
        owner.menu_model().notice.as_deref(),
        Some("Pairing is open: choose the new device's pairing object now.")
    );
    owner.toggle_pairing_window();
    assert_eq!(sent(&mut owner), [ClientMessage::ClosePairing]);
    owner.apply(Event::Message(ServerMessage::PairingWindow {
        closes_in: None,
    }));
    assert_eq!(
        owner.menu_model().notice.as_deref(),
        Some("Pairing is closed.")
    );
    // A change someone else makes shows in the row, without a notice.
    owner.apply(Event::Message(ServerMessage::PairingWindow {
        closes_in: Some(60),
    }));
    assert_eq!(
        owner.menu_model().notice.as_deref(),
        Some("Pairing is closed.")
    );
    assert!(owner.menu_model().pairing_window.flatten().is_some());
    // It belongs to the connection.
    owner.apply(Event::State(ConnectionState::Connecting { attempt: 2 }));
    assert_eq!(owner.menu_model().pairing_window, None);

    // Anyone but an owner is offered nothing, whatever a server sends.
    let mut visitor = connected_securely(ConnectionRole::Visitor, certificate);
    visitor.apply(Event::Message(ServerMessage::PairingWindow {
        closes_in: None,
    }));
    assert_eq!(visitor.menu_model().pairing_window, None);
    visitor.toggle_pairing_window();
    assert!(sent(&mut visitor).is_empty());
    assert_eq!(
        visitor.menu_model().notice.as_deref(),
        Some("Only owners can open pairing.")
    );
}

#[test]
fn pairing_is_offered_over_tls_to_anyone_but_an_owner() {
    let certificate = tomori_client::Fingerprint::of(b"server certificate");
    assert!(pairing_label(&connected_securely(ConnectionRole::Owner, certificate)).is_none());
    assert!(
        pairing_label(&connected_securely(
            ConnectionRole::Collaborator,
            certificate
        ))
        .is_some()
    );
    // Over plain HTTP there is no certificate to derive a code from.
    let mut plain = in_lobby(vec![], vec![]);
    plain.apply(role(ConnectionRole::Visitor));
    assert!(pairing_label(&plain).is_none());
    plain.pair();
    assert!(plain.take_actions().is_empty());
}

#[test]
fn a_connection_admitted_only_to_pair_lists_no_spaces() {
    let certificate = tomori_client::Fingerprint::of(b"server certificate");
    let mut conversation = crate::test_support::conversation("Ada");
    conversation.connect(Choice::new("Studio", "10.0.0.1:8765", Source::Discovered));
    conversation.take_actions();
    conversation.apply(Event::State(ConnectionState::Connected {
        source: 1,
        server: "studio".into(),
        role: ConnectionRole::Pairing,
        certificate: Some(certificate),
        backends: vec![],
        roots: vec![],
        environments: sky(),
        utterance_limit: None,
        name: None,
    }));
    assert!(sent(&mut conversation).is_empty());
    let view = conversation.arrival().unwrap();
    assert_eq!(keys(&view.items), [arrival::Key::Pair]);
    assert_eq!(
        view.status.as_deref(),
        Some("This server admits only paired devices: pair to join.")
    );
}

#[test]
fn a_visitor_knocks_and_is_admitted() {
    let certificate = tomori_client::Fingerprint::of(b"server certificate");
    let mut conversation = connected_securely(ConnectionRole::Visitor, certificate);
    // Not in a space: nothing to knock on.
    assert!(!conversation.can_knock());
    conversation.apply(space(|_| {}));
    conversation.take_actions();
    assert!(conversation.can_knock());
    conversation.knock();
    assert_eq!(sent(&mut conversation), [ClientMessage::Knock]);
    conversation.apply(Event::Message(ServerMessage::KnockState {
        state: tomori_protocol::KnockState::Pending,
    }));
    assert_eq!(
        conversation.knock_state(),
        Some(tomori_protocol::KnockState::Pending)
    );
    assert!(!conversation.can_knock());
    assert!(
        conversation
            .status()
            .unwrap()
            .contains("waiting for someone to let you in")
    );

    conversation.apply(Event::Message(ServerMessage::Admitted {
        token: "tomori_admitted".into(),
    }));
    conversation.apply(role(ConnectionRole::Collaborator));
    assert_eq!(
        conversation.take_actions(),
        [Action::Admitted {
            token: "tomori_admitted".into(),
        }]
    );
    assert_eq!(conversation.knock_state(), None);
    assert!(!conversation.can_knock());
}

#[test]
fn an_admission_this_device_did_not_ask_for_is_ignored() {
    crate::test_support::log_everything();
    let certificate = tomori_client::Fingerprint::of(b"server certificate");
    let mut conversation = connected_securely(ConnectionRole::Visitor, certificate);
    conversation.apply(space(|_| {}));
    conversation.take_actions();
    conversation.apply(Event::Message(ServerMessage::Admitted {
        token: "tomori_unasked".into(),
    }));
    assert!(conversation.take_actions().is_empty());
}

#[test]
fn a_participant_sees_and_answers_knocks_only_while_taking_part() {
    let certificate = tomori_client::Fingerprint::of(b"server certificate");
    let mut conversation = connected_securely(ConnectionRole::Owner, certificate);
    conversation.apply(space(|_| {}));
    conversation.take_actions();
    let vic = tomori_protocol::KnockRequest {
        id: "knock-1".into(),
        name: "Vic".into(),
    };
    conversation.apply(Event::Message(ServerMessage::Knocks {
        knocks: vec![vic.clone()],
    }));
    assert_eq!(conversation.knocks(), [vic]);
    conversation.decide_knock("knock-1", true);
    assert_eq!(
        sent(&mut conversation),
        [ClientMessage::DecideKnock {
            knock: "knock-1".into(),
            admit: true
        }]
    );
    // Knocks belong to the space, and to participants.
    conversation.apply(Event::Message(ServerMessage::LEFT));
    assert!(conversation.knocks().is_empty());
    let mut visitor = connected_securely(ConnectionRole::Visitor, certificate);
    visitor.apply(Event::Message(ServerMessage::Knocks {
        knocks: vec![tomori_protocol::KnockRequest {
            id: "knock-2".into(),
            name: "Wes".into(),
        }],
    }));
    assert!(visitor.knocks().is_empty());
    visitor.decide_knock("knock-2", false);
    assert!(sent(&mut visitor).is_empty());
}

#[test]
fn a_declined_knock_is_said_and_may_be_tried_again() {
    let certificate = tomori_client::Fingerprint::of(b"server certificate");
    let mut conversation = connected_securely(ConnectionRole::Visitor, certificate);
    conversation.apply(space(|_| {}));
    for (state, said) in [
        (
            tomori_protocol::KnockState::Declined,
            "Your knock was declined.",
        ),
        (
            tomori_protocol::KnockState::Expired,
            "Nobody answered your knock in time.",
        ),
        (
            tomori_protocol::KnockState::Withdrawn,
            "Your knock was withdrawn.",
        ),
    ] {
        conversation.apply(Event::Message(ServerMessage::KnockState { state }));
        assert_eq!(conversation.status().as_deref(), Some(said));
        assert!(conversation.can_knock());
    }
}

#[test]
fn only_owners_are_offered_the_create_object() {
    let mut conversation = in_lobby(vec![], vec![]);
    conversation.apply(role(ConnectionRole::Visitor));
    assert_eq!(conversation.role(), ConnectionRole::Visitor);
    listed(&mut conversation, vec![]);
    let view = conversation.arrival().unwrap();
    assert!(view.items.is_empty());
    assert_eq!(view.status.as_deref(), Some("No spaces yet."));
    assert!(!conversation.text().contains("New space"));
    // Choosing it anyway, as a stale pointer might, opens nothing.
    conversation.choose_object(&arrival::Key::Create);
    assert!(!conversation.text().contains("NEW SPACE"));

    listed(&mut conversation, vec![summary("one", "One")]);
    let view = conversation.arrival().unwrap();
    assert_eq!(keys(&view.items), [arrival::Key::Space("one".into())]);
    assert!(!conversation.text().contains("New space"));
}

#[test]
fn a_visitor_watches_but_does_not_talk_or_act() {
    let mut conversation = joined(|state| state.approval = Some(approval(&[])));
    conversation.apply(role(ConnectionRole::Visitor));
    conversation.talk(true);
    assert_eq!(conversation.status().as_deref(), Some(VISITING));
    assert!(conversation.take_actions().is_empty());
    conversation.toggle_live();
    assert!(conversation.take_actions().is_empty());
    conversation.accept();
    assert!(sent(&mut conversation).is_empty());

    // Admitted as a collaborator, they can, until they become a visitor again, which closes the microphone.
    conversation.apply(role(ConnectionRole::Collaborator));
    conversation.toggle_live();
    assert!(conversation.live);
    conversation.apply(role(ConnectionRole::Visitor));
    assert!(!conversation.live);
    conversation.apply(role(ConnectionRole::Collaborator));
    conversation.take_actions();
    conversation.accept();
    let [ClientMessage::Decide { .. }] = sent(&mut conversation)[..] else {
        unreachable!("[ClientMessage::Decide")
    };
}

#[test]
fn the_stick_creates_a_space_and_a_backend_without_attaching_skips_the_session_ring() {
    let mut conversation = in_lobby(backends(), roots());
    listed(&mut conversation, vec![]);
    // A on the create object opens the backend ring in place.
    assert!(!conversation.accept());
    assert!(conversation.text().contains("NEW SPACE: BACKEND"));
    assert_fits(&mut conversation);
    let view = conversation.arrival().unwrap();
    let ring = view.ring.unwrap();
    assert_eq!(ring.step, arrival::Step::Backend);
    assert_eq!(
        keys(&ring.items),
        [
            arrival::Key::Backend("fake".into()),
            arrival::Key::Backend("plain".into())
        ]
    );
    assert_eq!(view.items, conversation.arrival().unwrap().items);
    assert_eq!(
        view.items.last().unwrap().kind,
        arrival::Kind::Create { open: true }
    );

    conversation.navigate(0, 1);
    conversation.accept();
    assert!(conversation.text().contains("NEW SPACE: FOLDER"));
    conversation.accept();
    assert_eq!(
        sent(&mut conversation),
        [ClientMessage::CreateSpace {
            title: "plain in tomori".into(),
            backend: "plain".into(),
            directory: "/work/tomori".into(),
            session: None,
            environment: Some("sky".into()),
            instructions: None,
        }]
    );
    // Back at the spaces, with the rings closed.
    let view = conversation.arrival().unwrap();
    assert_eq!(view.ring, None);
    assert!(conversation.text().contains("SPACES"));
}

#[test]
fn pointing_creates_a_space_attached_to_an_existing_session() {
    let mut conversation = in_lobby(backends(), roots());
    listed(&mut conversation, vec![summary("one", "One")]);
    conversation.choose_object(&arrival::Key::Create);
    conversation.choose_object(&arrival::Key::Backend("fake".into()));
    conversation.choose_object(&arrival::Key::Directory("/work/tomori".into()));
    // The backend attaches, so its sessions in the folder are asked for and the session ring opens.
    assert_eq!(
        sent(&mut conversation),
        [ClientMessage::ListSessions {
            backend: "fake".into(),
            directory: "/work/tomori".into(),
        }]
    );
    let ring = conversation.arrival().unwrap().ring.unwrap();
    assert_eq!(ring.step, arrival::Step::Session);
    assert_eq!(keys(&ring.items), [arrival::Key::Session(None)]);
    conversation.apply(sessions());
    let ring = conversation.arrival().unwrap().ring.unwrap();
    assert_eq!(ring.items.len(), 3);
    assert_eq!(ring.items[1].label, "Tomori opencode support");
    assert!(conversation.text().contains("NEW SPACE: SESSION"));
    assert_fits(&mut conversation);

    conversation.choose_object(&arrival::Key::Session(Some("ses_1".into())));
    assert_eq!(
        sent(&mut conversation),
        [ClientMessage::CreateSpace {
            title: "Tomori opencode support".into(),
            backend: "fake".into(),
            directory: "/work/tomori".into(),
            session: Some("ses_1".into()),
            environment: Some("sky".into()),
            instructions: None,
        }]
    );
    assert_eq!(conversation.arrival().unwrap().ring, None);
}

#[test]
fn a_chooses_the_highlighted_session_not_the_ring_selection() {
    let mut conversation = in_lobby(backends(), roots());
    listed(&mut conversation, vec![summary("one", "One")]);
    conversation.choose_object(&arrival::Key::Create);
    conversation.choose_object(&arrival::Key::Backend("fake".into()));
    conversation.choose_object(&arrival::Key::Directory("/work/tomori".into()));
    sent(&mut conversation);
    conversation.apply(sessions());
    // The ring's own selection is still "New session"; the ray highlights
    // an existing session, and A must choose that one.
    let highlighted = arrival::Key::Session(Some("ses_1".into()));
    assert!(!conversation.press_a(Some(&highlighted)));
    assert!(matches!(
        sent(&mut conversation).as_slice(),
        [ClientMessage::CreateSpace { session: Some(session), .. }] if session == "ses_1"
    ));
}

#[test]
fn a_without_a_highlight_accepts_the_ring_selection() {
    let mut conversation = in_lobby(backends(), roots());
    listed(&mut conversation, vec![summary("one", "One")]);
    conversation.choose_object(&arrival::Key::Create);
    conversation.choose_object(&arrival::Key::Backend("fake".into()));
    conversation.choose_object(&arrival::Key::Directory("/work/tomori".into()));
    sent(&mut conversation);
    conversation.apply(sessions());
    assert!(!conversation.press_a(None));
    let [ClientMessage::CreateSpace { session: None, .. }] = sent(&mut conversation).as_slice()
    else {
        unreachable!("[ClientMessage::CreateSpace")
    };
}

#[test]
fn a_new_session_is_offered_first_on_the_session_ring() {
    let mut conversation = in_lobby(backends(), roots());
    listed(&mut conversation, vec![]);
    conversation.accept();
    conversation.accept();
    conversation.accept();
    conversation.apply(sessions());
    conversation.take_actions();
    assert!(conversation.text().contains("> New session"));
    conversation.accept();
    assert_eq!(
        sent(&mut conversation),
        [ClientMessage::CreateSpace {
            title: "fake in tomori".into(),
            backend: "fake".into(),
            directory: "/work/tomori".into(),
            session: None,
            environment: Some("sky".into()),
            instructions: None,
        }]
    );
}

/// `count` sessions, the newest first, each beneath the folder but the first, which is in it, and the second busy.
fn many_sessions(count: usize) -> Event {
    Event::Message(ServerMessage::Sessions {
        sessions: (0..count)
            .map(|index| tomori_protocol::SessionSummary {
                id: format!("ses_{index}"),
                title: format!("Session {index}"),
                directory: match index {
                    0 => "/work/tomori".into(),
                    _ => format!("/work/tomori/crates/c{index}"),
                },
                busy: index == 1,
            })
            .collect(),
    })
}

/// The session ring's keys, as their session identifiers, with "New session" as `new` and "More sessions" as
/// `more`.
fn session_ring(conversation: &Conversation) -> Vec<String> {
    conversation
        .arrival()
        .unwrap()
        .ring
        .unwrap()
        .items
        .into_iter()
        .map(|item| match item.key {
            arrival::Key::Session(id) => id.unwrap_or_else(|| "new".into()),
            // Only "More sessions" is on the ring besides the sessions.
            _ => "more".into(),
        })
        .collect()
}

/// More sessions than fit on the ring are shown a page at a time, newest first, and "More sessions" steps through
/// the pages, after the last returning to the first; every one can be reached and attached, by pointing or the
/// stick.
#[test]
fn many_sessions_are_shown_a_page_at_a_time() {
    let mut conversation = in_lobby(backends(), roots());
    listed(&mut conversation, vec![]);
    conversation.choose_object(&arrival::Key::Create);
    conversation.choose_object(&arrival::Key::Backend("fake".into()));
    conversation.choose_object(&arrival::Key::Directory("/work/tomori".into()));
    sent(&mut conversation);
    conversation.apply(many_sessions(SESSION_PAGE * 2 + 3));
    let page = |first: usize, count: usize| {
        std::iter::once("new".to_owned())
            .chain((first..first + count).map(|index| format!("ses_{index}")))
            .chain(std::iter::once("more".to_owned()))
            .collect::<Vec<_>>()
    };
    assert_eq!(session_ring(&conversation), page(0, SESSION_PAGE));
    let more = conversation.arrival().unwrap().ring.unwrap().items[SESSION_PAGE + 1].clone();
    assert_eq!(more.label, "More sessions");
    assert_eq!(more.detail.as_deref(), Some("1-8 of 19"));
    assert_fits(&mut conversation);

    conversation.choose_object(&arrival::Key::MoreSessions);
    assert_eq!(
        session_ring(&conversation),
        page(SESSION_PAGE, SESSION_PAGE)
    );
    assert!(sent(&mut conversation).is_empty());
    // The stick stays on "More sessions", so A steps on, to the last page, then back to the first.
    conversation.accept();
    assert_eq!(session_ring(&conversation), page(SESSION_PAGE * 2, 3));
    assert!(conversation.text().contains("> More sessions"));
    conversation.accept();
    assert_eq!(session_ring(&conversation), page(0, SESSION_PAGE));

    // A session beyond the first page is attached in its own directory, beneath the folder.
    conversation.choose_object(&arrival::Key::MoreSessions);
    conversation.choose_object(&arrival::Key::MoreSessions);
    conversation.choose_object(&arrival::Key::Session(Some("ses_17".into())));
    assert_eq!(
        sent(&mut conversation),
        [ClientMessage::CreateSpace {
            title: "Session 17".into(),
            backend: "fake".into(),
            directory: "/work/tomori/crates/c17".into(),
            session: Some("ses_17".into()),
            environment: Some("sky".into()),
            instructions: None,
        }]
    );
}

/// Each session says beneath its title whether the agent is busy in it and where beneath the folder it is.
#[test]
fn sessions_show_whether_they_are_busy_and_where_they_are() {
    let mut conversation = in_lobby(backends(), roots());
    listed(&mut conversation, vec![]);
    conversation.choose_object(&arrival::Key::Create);
    conversation.choose_object(&arrival::Key::Backend("fake".into()));
    conversation.choose_object(&arrival::Key::Directory("/work/tomori".into()));
    sent(&mut conversation);
    conversation.apply(many_sessions(3));
    let details: Vec<_> = conversation.arrival().unwrap().ring.unwrap().items[1..]
        .iter()
        .map(|item| item.detail.clone())
        .collect();
    assert_eq!(
        details,
        [
            None,
            Some("busy, crates/c1".into()),
            Some("crates/c2".into())
        ]
    );
    // Fewer than a page need no "More sessions".
    assert_eq!(
        session_ring(&conversation),
        ["new", "ses_0", "ses_1", "ses_2"]
    );
    let in_folder = |directory: &str, busy| tomori_protocol::SessionSummary {
        id: "s".into(),
        title: "t".into(),
        directory: directory.into(),
        busy,
    };
    assert_eq!(
        session_detail(&in_folder("/work/tomori", true), "/work/tomori").as_deref(),
        Some("busy")
    );
    assert_eq!(
        session_detail(&in_folder("/elsewhere", false), "/work/tomori").as_deref(),
        Some("/elsewhere")
    );
    // A session without a directory is attached in the folder.
    conversation.apply(Event::Message(ServerMessage::Sessions {
        sessions: vec![in_folder("", false)],
    }));
    conversation.choose_object(&arrival::Key::Session(Some("s".into())));
    assert!(matches!(
        sent(&mut conversation).as_slice(),
        [ClientMessage::CreateSpace { directory, .. }] if directory == "/work/tomori"
    ));
}

#[test]
fn stepping_back_returns_to_the_previous_ring() {
    let mut conversation = in_lobby(backends(), roots());
    listed(&mut conversation, vec![summary("one", "One")]);
    conversation.choose_object(&arrival::Key::Create);
    conversation.choose_object(&arrival::Key::Backend("fake".into()));
    conversation.choose_object(&arrival::Key::Directory("/work/tomori".into()));
    conversation.apply(sessions());
    let step = |conversation: &Conversation| {
        conversation.arrival().unwrap().ring.map(|ring| {
            (
                ring.step,
                keys(
                    &ring
                        .items
                        .into_iter()
                        .filter(|item| item.selected)
                        .collect::<Vec<_>>(),
                ),
            )
        })
    };
    // B steps back, with what was chosen there selected.
    conversation.back();
    assert_eq!(
        step(&conversation),
        Some((
            arrival::Step::Directory,
            vec![arrival::Key::Directory("/work/tomori".into())]
        ))
    );
    // So does choosing the create object while its rings are open.
    conversation.choose_object(&arrival::Key::Create);
    assert_eq!(
        step(&conversation),
        Some((
            arrival::Step::Backend,
            vec![arrival::Key::Backend("fake".into())]
        ))
    );
    conversation.back();
    assert_eq!(step(&conversation), None);
    conversation.take_actions();
    // From the spaces, B returns to the servers.
    conversation.back();
    assert_eq!(conversation.take_actions(), [Action::Disconnect]);
    assert_eq!(conversation.mode(), Mode::Servers);
}

#[test]
fn pointing_chooses_servers_spaces_and_the_connected_server() {
    let mut conversation = crate::test_support::conversation("Ada");
    let mut old = Choice::new("Old", "10.0.0.3:8765", Source::Discovered);
    old.unavailable = Some("protocol 1, needs 2".into());
    let studio = Choice::new("Studio", "10.0.0.1:8765", Source::Discovered);
    conversation.set_servers(vec![
        old,
        studio.clone(),
        Choice::new("USB", USB, Source::Usb),
    ]);
    let view = conversation.arrival().unwrap();
    assert_eq!(view.items.len(), 3);
    assert_eq!(
        view.items[0].kind,
        arrival::Kind::Server {
            source: Source::Discovered,
            available: false,
            attention: None,
        }
    );
    // An incompatible server is shown but can't be chosen.
    conversation.choose_object(&arrival::Key::Server("10.0.0.3:8765".into()));
    assert!(conversation.take_actions().is_empty());
    assert!(
        conversation
            .arrival()
            .unwrap()
            .status
            .unwrap()
            .contains("Cannot join Old")
    );
    conversation.choose_object(&arrival::Key::Server("10.0.0.1:8765".into()));
    assert_eq!(conversation.take_actions(), [Action::Connect(studio)]);
    conversation.apply(connected());
    listed(
        &mut conversation,
        vec![summary("one", "One"), summary("two", "Two")],
    );
    conversation.take_actions();
    let view = conversation.arrival().unwrap();
    assert_eq!(view.connected.unwrap().label, "Studio");
    assert_eq!(view.heading, "Spaces on Studio");

    conversation.choose_object(&arrival::Key::Space("two".into()));
    assert_eq!(
        sent(&mut conversation),
        [ClientMessage::Join {
            space: "two".into()
        }]
    );
    // The connected server returns to the servers.
    conversation.choose_object(&arrival::Key::Connected("10.0.0.1:8765".into()));
    assert_eq!(conversation.take_actions(), [Action::Disconnect]);
    assert!(conversation.is_browsing());
}

#[test]
fn a_ring_key_from_another_ring_is_ignored() {
    let mut conversation = in_lobby(backends(), roots());
    listed(&mut conversation, vec![]);
    conversation.choose_object(&arrival::Key::Directory("/work/tomori".into()));
    assert_eq!(conversation.arrival().unwrap().ring, None);
    conversation.choose_object(&arrival::Key::Create);
    conversation.choose_object(&arrival::Key::Directory("/work/tomori".into()));
    assert_eq!(
        conversation.arrival().unwrap().ring.unwrap().step,
        arrival::Step::Backend
    );
    assert!(sent(&mut conversation).is_empty());
}

/// Walk the creation rings from the create object, choosing the first backend, the first folder and a new session.
fn create_in(conversation: &mut Conversation) {
    listed(conversation, vec![]);
    conversation.accept();
    conversation.accept();
    conversation.accept();
    conversation.apply(sessions());
    conversation.take_actions();
    conversation.accept();
}

#[test]
fn several_environments_are_chosen_after_the_folder() {
    let mut conversation = in_lobby(backends(), roots());
    conversation.apply(Event::State(ConnectionState::Connected {
        source: 1,
        server: "studio".into(),
        role: ConnectionRole::Owner,
        certificate: None,
        backends: backends(),
        roots: roots(),
        environments: vec![
            EnvironmentInfo {
                id: "sky".into(),
                label: "Sky".into(),
                version: 1,
            },
            EnvironmentInfo {
                id: "observe".into(),
                label: "Observe".into(),
                version: 1,
            },
        ],
        utterance_limit: None,
        name: None,
    }));
    conversation.take_actions();
    create_in(&mut conversation);
    assert!(sent(&mut conversation).is_empty());
    let text = conversation.text();
    assert!(text.contains("NEW SPACE: ENVIRONMENT 1/2"), "{text}");
    assert!(text.contains("> Sky") && text.contains("Observe"), "{text}");
    assert_fits(&mut conversation);
    let ring = conversation.arrival().unwrap().ring.unwrap();
    assert_eq!(ring.step, arrival::Step::Environment);

    // B steps back to the session ring without creating anything; choosing again offers the environments again.
    conversation.back();
    assert!(conversation.text().contains("NEW SPACE: SESSION"));
    assert!(sent(&mut conversation).is_empty());
    conversation.accept();
    conversation.choose_object(&arrival::Key::Environment("observe".into()));
    assert_eq!(
        sent(&mut conversation),
        [ClientMessage::CreateSpace {
            title: "fake in tomori".into(),
            backend: "fake".into(),
            directory: "/work/tomori".into(),
            session: None,
            environment: Some("observe".into()),
            instructions: None,
        }]
    );
    assert!(conversation.text().contains("SPACES"));
}

#[test]
fn an_owner_gives_the_next_space_or_the_joined_one_instructions() {
    // Outside a space, they go with the next space created, once.
    let mut conversation = in_lobby(backends(), roots());
    conversation.instruct("  Mind the docs. ");
    create_in(&mut conversation);
    assert!(matches!(
        sent(&mut conversation).as_slice(),
        [ClientMessage::CreateSpace { instructions: Some(text), .. }] if text == "Mind the docs."
    ));
    create_in(&mut conversation);
    let [
        ClientMessage::CreateSpace {
            instructions: None, ..
        },
    ] = sent(&mut conversation).as_slice()
    else {
        unreachable!("[ClientMessage::CreateSpace")
    };

    // In a space whose agent takes them, they change the space's own; otherwise nothing is sent.
    let layers = AgentInstructions {
        tomori: "Be brief.".into(),
        ..Default::default()
    };
    let mut conversation = joined(|state| state.instructions = Some(layers.clone()));
    conversation.instruct("Mind the tests.");
    assert_eq!(
        sent(&mut conversation),
        [ClientMessage::SetInstructions {
            text: "Mind the tests.".into()
        }]
    );
    let model = conversation.menu_model();
    assert_eq!(model.instructions, Some(layers));
    let mut plain = joined(|_| {});
    plain.instruct("Mind the tests.");
    assert!(sent(&mut plain).is_empty());
    assert!(plain.text().contains("takes no instructions"));
    // Only owners can.
    conversation.apply(Event::Message(ServerMessage::Role {
        role: ConnectionRole::Collaborator,
    }));
    conversation.instruct("Ignore the tests.");
    assert!(sent(&mut conversation).is_empty());
    assert!(conversation.text().contains("Only owners"));
    conversation.apply(Event::Message(ServerMessage::Role {
        role: ConnectionRole::Owner,
    }));
    conversation.instruct(&"x".repeat(INSTRUCTIONS_BYTES + 1));
    assert!(sent(&mut conversation).is_empty());
    assert!(conversation.text().contains("at most 4096 bytes"));
}

#[test]
fn a_server_offering_no_environments_binds_its_own() {
    let mut conversation = in_lobby(backends(), roots());
    conversation.apply(Event::State(ConnectionState::Connected {
        source: 1,
        server: "studio".into(),
        role: ConnectionRole::Owner,
        certificate: None,
        backends: backends(),
        roots: roots(),
        environments: vec![],
        utterance_limit: None,
        name: None,
    }));
    conversation.take_actions();
    create_in(&mut conversation);
    let [
        ClientMessage::CreateSpace {
            environment: None, ..
        },
    ] = sent(&mut conversation).as_slice()
    else {
        unreachable!("[ClientMessage::CreateSpace")
    };
}

#[test]
fn the_server_list_and_the_lobby_say_while_the_microphone_request_is_unanswered() {
    let status = |conversation: &Conversation| conversation.arrival().and_then(|view| view.status);
    let mut conversation = crate::test_support::conversation("Ada");
    assert_eq!(
        status(&conversation).as_deref(),
        Some("Searching the network...")
    );
    // On the server list, as after a reinstall, the status says it, in the world and on the panel.
    conversation.microphone_request(true);
    assert_eq!(status(&conversation).as_deref(), Some(MICROPHONE_REQUEST));
    assert!(conversation.text().contains(MICROPHONE_REQUEST));
    // An error outranks it.
    conversation.error = Some("No network.".into());
    assert_eq!(status(&conversation).as_deref(), Some("No network."));
    assert!(!conversation.text().contains(MICROPHONE_REQUEST));
    conversation.error = None;
    // In the lobby, connected, it is the notice; connecting clears that, and the next check notes it again.
    conversation.connect(Choice::new("Studio", "10.0.0.1:8765", Source::Discovered));
    conversation.apply(connected());
    listed(&mut conversation, vec![summary("a", "Deploy")]);
    conversation.microphone_request(true);
    assert_eq!(status(&conversation).as_deref(), Some(MICROPHONE_REQUEST));
    assert!(conversation.text().contains(MICROPHONE_REQUEST));
    // Answered, neither says it any more.
    conversation.microphone_request(false);
    assert_ne!(status(&conversation).as_deref(), Some(MICROPHONE_REQUEST));
    let mut conversation = crate::test_support::conversation("Ada");
    conversation.microphone_request(true);
    conversation.microphone_request(false);
    assert_eq!(
        status(&conversation).as_deref(),
        Some("Searching the network...")
    );
}

#[test]
fn arrival_and_the_lobby_use_the_grid_and_a_space_its_environment() {
    let mut conversation = crate::test_support::conversation("Ada");
    assert_eq!(conversation.environment(), Environment::MINIMAL);
    conversation.connect(Choice::new("Studio", "10.0.0.1:8765", Source::Discovered));
    conversation.apply(connected());
    assert_eq!(conversation.environment(), Environment::MINIMAL);
    conversation.apply(space(|_| {}));
    assert_eq!(conversation.environment(), crate::test_support::SKY);
    // Leaving returns to the grid at once, before the server confirms.
    hold_b(&mut conversation);
    assert_eq!(conversation.mode(), Mode::Lobby);
    assert_eq!(conversation.environment(), Environment::MINIMAL);
    conversation.apply(Event::Message(ServerMessage::LEFT));
    assert_eq!(conversation.environment(), Environment::MINIMAL);
    // Returning to the servers keeps the grid.
    conversation.apply(space(|_| {}));
    assert_eq!(conversation.environment(), crate::test_support::SKY);
    conversation.apply(Event::Message(ServerMessage::LEFT));
    conversation.back();
    assert_eq!(conversation.mode(), Mode::Servers);
    assert_eq!(conversation.environment(), Environment::MINIMAL);
}

#[test]
fn a_space_in_an_unknown_environment_shows_the_grid_and_names_it() {
    let mut conversation = joined(|state| state.environment = "observe".into());
    assert_eq!(conversation.environment(), Environment::MINIMAL);
    let text = conversation.text();
    assert!(text.contains("Unknown environment: observe"), "{text}");
    assert_fits(&mut conversation);
    // An older server names none.
    conversation.apply(space(|state| state.environment = String::new()));
    assert_eq!(conversation.environment(), Environment::MINIMAL);
    assert!(
        conversation
            .text()
            .contains("The server named no environment.")
    );
    // A known one is drawn, and the notice goes.
    conversation.apply(space(|_| {}));
    assert_eq!(conversation.environment(), crate::test_support::SKY);
    assert!(!conversation.text().contains("environment"));
}

#[test]
fn a_space_at_another_version_shows_the_grid_and_names_the_version() {
    // The space's state names the version; this build presents Sky at version 1.
    let mut conversation = joined(|state| state.environment_version = Some(1));
    assert_eq!(conversation.environment(), crate::test_support::SKY);
    assert!(!conversation.text().contains("version"));
    conversation.apply(space(|state| state.environment_version = Some(2)));
    assert_eq!(conversation.environment(), Environment::MINIMAL);
    let text = conversation.text();
    assert!(
        text.contains("Sky needs version 2; this app has 1"),
        "{text}"
    );
    assert_fits(&mut conversation);
    assert_eq!(
        conversation
            .menu_model()
            .space
            .unwrap()
            .environment
            .as_deref(),
        Some("Sky needs version 2; this app has 1")
    );
    // One it lacks names the version the space needs.
    conversation.apply(space(|state| {
        state.environment = "observe".into();
        state.environment_version = Some(3);
    }));
    assert_eq!(conversation.environment(), Environment::MINIMAL);
    assert!(
        conversation
            .text()
            .contains("Unknown environment: observe version 3")
    );
    // A build that registers it at that version draws it.
    const OBSERVE: Environment = Environment::new("observe").with_version(3);
    conversation.set_drawable(vec![Environment::MINIMAL, OBSERVE]);
    assert_eq!(conversation.environment(), OBSERVE);
    assert!(!conversation.text().contains("observe"));
}

#[test]
fn a_change_the_build_cannot_present_falls_back_to_the_grid_with_a_warning_and_back() {
    // In Sky, as an owner elsewhere changes the space for everyone.
    let mut conversation = joined(|state| state.environment_version = Some(1));
    assert_eq!(conversation.mode(), Mode::Space);
    assert_eq!(conversation.environment(), crate::test_support::SKY);
    let warned = |conversation: &mut Conversation| {
        (
            conversation.environment(),
            conversation.text(),
            conversation.menu_model().space.unwrap().environment,
        )
    };
    // To one this build lacks: the grid, with a warning on the panel and in the hand menu.
    conversation.apply(space(|state| {
        state.environment = "observe".into();
        state.environment_version = Some(2);
    }));
    let (environment, text, menu) = warned(&mut conversation);
    assert_eq!(environment, Environment::MINIMAL);
    assert!(
        text.contains("Unknown environment: observe version 2"),
        "{text}"
    );
    assert_eq!(
        menu.as_deref(),
        Some("Unknown environment: observe version 2")
    );
    // To Sky at a version this build does not present: the grid, naming the version.
    conversation.apply(space(|state| state.environment_version = Some(2)));
    let (environment, text, menu) = warned(&mut conversation);
    assert_eq!(environment, Environment::MINIMAL);
    assert!(
        text.contains("Sky needs version 2; this app has 1"),
        "{text}"
    );
    assert_eq!(menu.as_deref(), Some("Sky needs version 2; this app has 1"));
    // Back to what it presents: Sky again, and the warning goes.
    conversation.apply(space(|state| state.environment_version = Some(1)));
    let (environment, text, menu) = warned(&mut conversation);
    assert_eq!(environment, crate::test_support::SKY);
    assert!(!text.contains("version"), "{text}");
    assert_eq!(menu.as_deref(), Some("Sky"));
}

#[test]
fn the_github_prototype_is_presented_over_the_grid_without_a_notice() {
    let mut conversation = joined(|state| {
        state.environment = "github".into();
        state.environment_version = Some(1);
    });
    assert_eq!(conversation.environment(), Environment::MINIMAL);
    assert!(!conversation.text().contains("environment"));
    assert_eq!(
        conversation
            .menu_model()
            .space
            .unwrap()
            .environment
            .as_deref(),
        Some("github")
    );
    // At a version this build does not present, it says so.
    conversation.apply(space(|state| {
        state.environment = "github".into();
        state.environment_version = Some(2);
    }));
    assert!(
        conversation
            .text()
            .contains("github needs version 2; this app has 1")
    );
}

#[test]
fn hello_lists_what_this_build_presents_once_each_at_its_version() {
    use tomori_client::connection::Session;
    let mut conversation = crate::test_support::conversation("Ada");
    let listed = |conversation: &Conversation| -> Vec<(String, u32)> {
        conversation
            .environments()
            .unwrap()
            .into_iter()
            .map(|presented| (presented.id, presented.version))
            .collect()
    };
    assert_eq!(
        listed(&conversation),
        [
            ("minimal".to_owned(), 1),
            ("sky".to_owned(), 1),
            ("github".to_owned(), 1)
        ]
    );
    // A build that draws the GitHub prototype itself, or lists the grid again, lists each once.
    conversation.set_drawable(vec![
        crate::test_support::GITHUB.with_version(2),
        Environment::MINIMAL,
        Environment::new("floor"),
    ]);
    assert_eq!(
        listed(&conversation),
        [
            ("minimal".to_owned(), 1),
            ("github".to_owned(), 2),
            ("floor".to_owned(), 1)
        ]
    );
}

#[test]
fn the_grid_is_always_at_version_1_and_others_at_the_version_registered() {
    use tomori_client::connection::Session;
    let mut conversation = joined(|state| {
        state.environment = "minimal".into();
        state.environment_version = Some(1);
    });
    // A scenery registered for the grid at another version is drawn with it, at version 1; Sky and the GitHub
    // prototype are at the versions registered.
    conversation.set_drawable(vec![
        Environment::MINIMAL.with_version(3),
        crate::test_support::SKY.with_version(2),
        crate::test_support::GITHUB.with_version(2),
    ]);
    let listed: Vec<(String, u32)> = conversation
        .environments()
        .unwrap()
        .into_iter()
        .map(|presented| (presented.id, presented.version))
        .collect();
    assert_eq!(
        listed,
        [
            ("minimal".to_owned(), 1),
            ("sky".to_owned(), 2),
            ("github".to_owned(), 2)
        ]
    );
    assert_eq!(conversation.environment(), Environment::MINIMAL);
    assert!(!conversation.text().contains("version"));
    // So a space at the grid's other version, as Hello never lists, says so, as one at Sky's version 1 does.
    conversation.apply(space(|state| {
        state.environment = "minimal".into();
        state.environment_version = Some(3);
    }));
    assert!(
        conversation
            .text()
            .contains("minimal needs version 3; this app has 1")
    );
    conversation.apply(space(|state| state.environment_version = Some(1)));
    assert!(
        conversation
            .text()
            .contains("Sky needs version 1; this app has 2")
    );
    conversation.apply(space(|state| state.environment_version = Some(2)));
    assert_eq!(conversation.environment().version(), 2);
    assert_eq!(conversation.environment(), crate::test_support::SKY);
    conversation.apply(space(|state| {
        state.environment = "github".into();
        state.environment_version = Some(2);
    }));
    assert!(!conversation.text().contains("version"));
}

#[test]
fn the_lobby_lists_every_space_whatever_its_environment() {
    // Offered only Sky, the lobby still lists a space in an environment this build cannot present.
    let mut conversation = in_lobby(Vec::new(), Vec::new());
    let mut topology = summary("b", "Topology");
    topology.environment = "observe".into();
    listed(&mut conversation, vec![summary("a", "Night"), topology]);
    let text = conversation.text();
    assert!(
        text.contains("Night") && text.contains("Topology"),
        "{text}"
    );
}

#[test]
fn a_build_that_draws_its_own_environment_shows_it() {
    const OBSERVE: Environment = Environment::new("observe");
    let mut conversation = joined(|state| state.environment = "observe".into());
    conversation.set_drawable(vec![Environment::MINIMAL, OBSERVE]);
    assert_eq!(conversation.environment(), OBSERVE);
    assert!(!conversation.text().contains("Unknown environment"));
    // Without Sky, a space in Sky shows the grid and names it.
    conversation.apply(space(|state| state.environment = "sky".into()));
    assert_eq!(conversation.environment(), Environment::MINIMAL);
    assert!(conversation.text().contains("Unknown environment: sky"));
}

#[test]
fn a_declared_locomotion_applies_only_in_a_space_in_a_drawn_environment() {
    use crate::locomotion::{Locomotion, Surface};
    const OBSERVE: Environment = Environment::new("observe");
    let teleport = Locomotion::teleport(vec![Surface::disc(glam::Vec3::ZERO, 5.0)]);
    // Tomori's own build declares none.
    let mut conversation = joined(|state| state.environment = "observe".into());
    assert_eq!(conversation.locomotion(), Locomotion::None);
    conversation.set_locomotion(vec![(OBSERVE, teleport.clone())]);
    // Shown as the grid, it has none.
    assert_eq!(conversation.locomotion(), Locomotion::None);
    assert_eq!(conversation.menu_model().travel, None);
    conversation.set_drawable(vec![Environment::MINIMAL, OBSERVE]);
    assert_eq!(conversation.locomotion(), teleport);
    // This headset can turn travel off, staying room scale, and on again, from the hand menu.
    assert_eq!(conversation.menu_model().travel, Some(true));
    conversation.choose_menu(crate::hand_menu::Choice::Travel);
    assert_eq!(conversation.locomotion(), Locomotion::None);
    assert_eq!(conversation.menu_model().travel, Some(false));
    conversation.choose_menu(crate::hand_menu::Choice::Travel);
    assert_eq!(conversation.locomotion(), teleport);
    // Sky, which declares none, and the lobby have none.
    conversation.apply(space(|state| state.environment = "sky".into()));
    assert_eq!(conversation.locomotion(), Locomotion::None);
    conversation.apply(space(|state| state.environment = "observe".into()));
    assert_eq!(conversation.locomotion(), teleport);
    // At a version this build does not have, the space is shown as the grid, which declares none.
    conversation.apply(space(|state| {
        state.environment = "observe".into();
        state.environment_version = Some(2);
    }));
    assert_eq!(conversation.locomotion(), Locomotion::None);
    conversation.apply(space(|state| state.environment = "observe".into()));
    assert_eq!(conversation.locomotion(), teleport);
    hold_b(&mut conversation);
    assert_eq!(conversation.locomotion(), Locomotion::None);
}

#[test]
fn a_ring_home_stands_its_workers_and_its_drawn_ring_at_home_while_the_agent_goes_elsewhere() {
    use crate::anchors::{Anchor, HomeRing};
    use tomori_protocol::cluster::{Formation, RING};
    use tomori_protocol::{AgentMovement, Anchors, Pose};
    let agent = Pose::at([-1.0, 1.5, -4.0]).facing_towards([0.0, 1.6, 0.0]);
    let ring = tomori_protocol::cluster::Anchor::new("home", agent)
        .with_formation(Formation::Ring)
        .with_sweep(240.0);
    let called = Pose::at([1.0, 1.6, -1.0]).facing_towards([0.0, 1.6, 0.0]);
    let at_home = |state: &mut SpaceState| {
        state.environment = "sky".into();
        state.movement = Some(AgentMovement::default());
        state.anchors = Anchors::new()
            .with_agent(agent)
            .with_workers(agent)
            .with_cluster(ring.clone());
    };
    let mut conversation = joined(at_home);
    let home = conversation.arrangement();
    assert_eq!(
        (home.formation, home.sweep, home.extent),
        (Formation::Ring, Some(240.0), None)
    );
    let drawn = home.home_ring();
    assert_eq!(drawn.centre, Anchor::from_pose(&agent).unwrap().position);
    assert_eq!(drawn.radius, RING);
    assert!((drawn.sweep - 240f32.to_radians()).abs() < 1e-5);
    // Towards the eyes at the arrival, about the vertical.
    assert!(drawn.towards.x > 0.0 && drawn.towards.z > 0.9 && drawn.towards.y == 0.0);
    // Called elsewhere, the agent goes, and its workers and its ring stay home.
    conversation.apply(space(|state| {
        at_home(state);
        state.away = Some(called);
        state.partner = Some([0.0, 1.6, 0.0]);
    }));
    let away = conversation.arrangement();
    assert!(away.away());
    assert_eq!(away.home_ring(), drawn);
    assert_eq!(
        (away.workers, away.formation),
        (home.workers, Formation::Ring)
    );
    // A sweep out of bounds is the whole ring.
    let mut wide = ring.clone();
    wide.sweep = Some(400.0);
    conversation.apply(space(|state| {
        at_home(state);
        state.anchors = Anchors::new().with_agent(agent).with_cluster(wide.clone());
    }));
    assert_eq!(conversation.arrangement().sweep, None);
    // Sky, which says nothing, stands its workers on today's arc, its ring whole about the agent's home.
    conversation.apply(space(|state| {
        state.environment = "sky".into();
        state.movement = Some(AgentMovement::default());
        state.away = Some(called);
    }));
    let sky = conversation.arrangement();
    assert_eq!((sky.formation, sky.extent), (Formation::Arc, None));
    assert_eq!(sky.home_ring(), HomeRing::WHOLE);
    assert_eq!(HomeRing::default(), HomeRing::WHOLE);
    assert_eq!(sky.ring, tomori_protocol::cluster::RingAt::Floor);
    // A home floating high, its anchor asking for its ring under it, on the arc and without room: the ring lies at
    // the swarm's foot beneath it, staying there as the agent goes.
    let high = Pose::at([0.0, 5.3, -4.0]).facing_towards([0.0, 1.6, 0.0]);
    let floating = tomori_protocol::cluster::Anchor::new("home", high)
        .with_ring(tomori_protocol::cluster::RingAt::UnderHome);
    conversation.apply(space(|state| {
        state.environment = "sky".into();
        state.movement = Some(AgentMovement::default());
        state.away = Some(called);
        state.anchors = Anchors::new()
            .with_agent(high)
            .with_workers(high)
            .with_cluster(floating.clone());
    }));
    let floating = conversation.arrangement();
    assert_eq!(floating.formation, Formation::Arc);
    let drawn = floating.home_ring();
    assert_eq!(drawn.centre, Anchor::from_pose(&high).unwrap().position);
    assert!((drawn.height - 3.9).abs() < 1e-5, "{drawn:?}");
}

#[test]
fn the_arrangement_follows_the_joined_space_s_anchors_and_its_drawn_environment_s_placement() {
    use crate::anchors::{Anchor, Placement, Placing, WorkerPlacement};
    use tomori_protocol::{Anchors, Pose};
    const OBSERVE: Environment = Environment::new("observe");
    struct AtOrigin;
    impl WorkerPlacement for AtOrigin {
        fn place(&self, origin: Anchor, _worker: &Placing<'_>) -> glam::Vec3 {
            origin.position
        }
    }
    let placement: Placement = std::sync::Arc::new(AtOrigin);
    let agent = Pose {
        position: [2.0, 1.5, -4.0],
        orientation: [0.0, 0.0, 0.0, 1.0],
    };
    let far = Pose {
        position: [0.0, 0.0, -35.0],
        ..agent
    };
    let observe = |state: &mut SpaceState| {
        state.environment = "observe".into();
        state.anchors = Anchors::new().with_agent(agent).with_workers(far);
    };
    // Outside a space, the default layout.
    let lobby = in_lobby(Vec::new(), Vec::new());
    let arrangement = lobby.arrangement();
    assert_eq!(
        (arrangement.agent, arrangement.workers),
        (Anchor::AGENT, Anchor::AGENT)
    );
    assert!(arrangement.placement.is_none());
    // In a space, its anchors, each that is valid, whether or not this build draws its environment: the workers
    // about the agent's home, as the workers anchor given is too far.
    let mut conversation = joined(observe);
    conversation.set_worker_placements(vec![(OBSERVE, placement.clone())]);
    assert_eq!(conversation.worker_placements().len(), 1);
    let arrangement = conversation.arrangement();
    assert_eq!(arrangement.agent, Anchor::from_pose(&agent).unwrap());
    assert_eq!(arrangement.home, arrangement.agent);
    assert_eq!(arrangement.workers, arrangement.home);
    assert!(!arrangement.moved());
    // Shown as the grid, the default arc.
    assert!(arrangement.placement.is_none());
    conversation.set_drawable(vec![Environment::MINIMAL, OBSERVE]);
    // Drawn, from the moment the space is joined: the conversation does not wait for the scenery to grow in.
    let drawn = conversation.arrangement().placement.expect("drawn");
    assert!(std::sync::Arc::ptr_eq(&drawn, &placement));
    assert_eq!(
        drawn.place(Anchor::AGENT, &Placing::new("ses_1", 0)),
        Anchor::AGENT.position
    );
    // At a version this build does not have, the grid again; and in another environment, none.
    conversation.apply(space(|state| {
        observe(state);
        state.environment_version = Some(2);
    }));
    assert!(conversation.arrangement().placement.is_none());
    conversation.apply(space(|state| {
        state.environment = "sky".into();
        state.anchors = Anchors::new().with_workers(agent);
    }));
    let sky = conversation.arrangement();
    assert!(sky.placement.is_none());
    assert_eq!(sky.agent, Anchor::AGENT);
    assert_eq!(sky.workers, Anchor::from_pose(&agent).unwrap());
    assert_eq!(sky.audience, crate::anchors::EYES);
    // Away, the agent stands there, its home where it was, and its workers stay home, gathered about the eyes there.
    let went = tomori_protocol::Pose::at([1.0, 1.5, -1.0]).facing_towards([0.0, 1.5, 0.0]);
    conversation.apply(space(|state| {
        state.environment = "sky".into();
        state.anchors = Anchors::new().with_workers(agent);
        state.away = Some(went);
    }));
    let away = conversation.arrangement();
    assert!(away.moved() && away.away());
    assert_eq!(away.agent, Anchor::from_pose(&went).unwrap());
    assert_eq!(away.home, Anchor::AGENT);
    assert_eq!(away.workers, sky.workers);
    assert_eq!(away.audience, crate::anchors::EYES);
    // The home cluster's room, where the environment gives it, with its anchor where it says; one of room it may
    // not have is today's arc at the workers anchor.
    let yard = tomori_protocol::Pose::at([0.0, 1.4, -5.0]).facing_towards([0.0, 1.6, 0.0]);
    let room = tomori_protocol::cluster::Anchor::new("home", yard).with_extent(6.0, 2.5, 3.0);
    conversation.apply(space(|state| {
        state.environment = "sky".into();
        state.anchors = Anchors::new()
            .with_workers(agent)
            .with_cluster(room.clone());
    }));
    let roomy = conversation.arrangement();
    assert_eq!(roomy.workers, Anchor::from_pose(&yard).unwrap());
    assert!(roomy.clusters.is_empty() && roomy.layouts.is_empty());
    // Its other anchors, and the server's seats, are taken as they are sent, for the scene to lay out.
    let rack = tomori_protocol::cluster::Anchor::new("rack/a12", yard);
    let seats = vec![tomori_protocol::cluster::Layout::new("rack/a12", 1).with_seat("ses_1", 0)];
    conversation.apply(space(|state| {
        state.environment = "sky".into();
        state.anchors = Anchors::new()
            .with_workers(agent)
            .with_cluster(room.clone())
            .with_cluster(rack.clone());
        state.clusters = seats.clone();
    }));
    let placed = conversation.arrangement();
    assert_eq!((placed.clusters, placed.layouts), (vec![rack], seats));
    assert_eq!(
        roomy.extent,
        Some(tomori_protocol::cluster::Extent::new(6.0, 2.5, 3.0))
    );
    assert!(!roomy.away());
    for (unusable, extent) in [
        (room.clone().with_extent(0.1, 2.5, 3.0), true),
        (tomori_protocol::cluster::Anchor::new("home", yard), false),
    ] {
        assert_eq!(unusable.extent.is_some(), extent);
        conversation.apply(space(|state| {
            state.environment = "sky".into();
            state.anchors = Anchors::new()
                .with_workers(agent)
                .with_cluster(unusable.clone());
        }));
        let arc = conversation.arrangement();
        assert_eq!(
            (arc.workers, arc.extent),
            (Anchor::from_pose(&agent).unwrap(), None)
        );
    }
    let mut far = room.clone();
    far.pose.position[0] = f32::NAN;
    conversation.apply(space(|state| {
        state.environment = "sky".into();
        state.anchors = Anchors::new().with_workers(agent).with_cluster(far);
    }));
    assert_eq!(conversation.arrangement().extent, None);
    // Away, the home cluster keeps its room.
    conversation.apply(space(|state| {
        state.environment = "sky".into();
        state.anchors = Anchors::new().with_cluster(room.clone());
        state.away = Some(went);
    }));
    let left = conversation.arrangement();
    assert_eq!((left.workers, left.extent), (roomy.workers, roomy.extent));
    // Leaving, the default layout.
    conversation.apply(space(observe));
    hold_b(&mut conversation);
    assert!(conversation.arrangement().placement.is_none());
    assert_eq!(conversation.arrangement().agent, Anchor::AGENT);
}

#[test]
fn a_participant_sends_the_agent_home_and_calls_it_from_the_hand_menu() {
    use tomori_protocol::{AgentMovement, Movement};
    let went = tomori_protocol::Pose::at([1.0, 1.5, -1.0]).facing_towards([0.0, 1.5, 0.0]);
    // At home, or away on a server that does not say how the agent moves, nothing is offered and nothing is sent.
    let mut conversation = joined(|state| state.movement = Some(AgentMovement::default()));
    assert!(!conversation.menu_model().agent_away);
    let mut older = joined(|state| state.away = Some(went));
    let model = older.menu_model();
    assert!(!model.agent_away && !model.calls && model.movement.is_none());
    older.send_agent_home();
    older.call_agent();
    older.set_agent_movement(AgentMovement::default());
    assert!(sent(&mut older).is_empty());
    // Away, the Space section offers to send it home, which tells everyone and stands it there at once.
    conversation.apply(space(|state| {
        state.movement = Some(AgentMovement::default());
        state.away = Some(went);
    }));
    assert!(conversation.menu_model().agent_away);
    conversation.choose_menu(crate::hand_menu::Choice::AgentHome);
    assert_eq!(sent(&mut conversation), [ClientMessage::SendAgentHome]);
    assert!(!conversation.arrangement().moved());
    assert!(!conversation.menu_model().agent_away);
    // From a server that says how it moves, it is called here, and an owner steps how it moves and how long it
    // waits before going home.
    let mut newer = joined(|state| state.movement = Some(AgentMovement::default()));
    let model = newer.menu_model();
    assert!(model.calls && !model.agent_away);
    assert_eq!(model.movement, Some((AgentMovement::default(), true)));
    newer.choose_menu(crate::hand_menu::Choice::CallAgent);
    newer.choose_menu(crate::hand_menu::Choice::AgentMovement);
    newer.choose_menu(crate::hand_menu::Choice::AgentReturn);
    assert_eq!(
        sent(&mut newer),
        [
            ClientMessage::CallAgent,
            ClientMessage::SetAgentMovement {
                movement: AgentMovement::new(Movement::Follow, Some(90))
            },
            ClientMessage::SetAgentMovement {
                movement: AgentMovement::new(Movement::Asked, Some(120))
            },
        ]
    );
    // Pinned near them, it is not offered again, but sending it home is; pinned far, it is.
    newer.observe_head(Some(tomori_protocol::Pose::at([0.0, 1.6, 0.0])));
    let pinned = |at: [f32; 3]| {
        space(move |state| {
            state.movement = Some(AgentMovement::new(Movement::Follow, Some(90)));
            state.away = Some(tomori_protocol::Pose::at(at));
            state.pinned = true;
        })
    };
    newer.apply(pinned([0.5, 1.6, -1.0]));
    let model = newer.menu_model();
    assert!(!model.calls && model.agent_away);
    newer.apply(pinned([6.0, 1.6, -1.0]));
    assert!(newer.menu_model().calls);
    // From a server that takes no pins, neither stay nor follow is offered or sent.
    assert_eq!(newer.menu_model().pins, None);
    newer.pin_agent(false);
    assert!(sent(&mut newer).is_empty());
    newer.choose_menu(crate::hand_menu::Choice::AgentHome);
    assert_eq!(sent(&mut newer), [ClientMessage::SendAgentHome]);
    // Kept home, it is never called.
    newer.apply(space(|state| {
        state.movement = Some(AgentMovement::new(Movement::Home, Some(90)));
    }));
    assert!(!newer.menu_model().calls);
    newer.call_agent();
    assert!(sent(&mut newer).is_empty());
    // A collaborator reads how it moves, but changes nothing.
    newer.apply(Event::Message(ServerMessage::Role {
        role: ConnectionRole::Collaborator,
    }));
    assert_eq!(
        newer.menu_model().movement.map(|(_, owner)| owner),
        Some(false)
    );
    newer.choose_menu(crate::hand_menu::Choice::AgentMovement);
    assert!(sent(&mut newer).is_empty());
    // Outside a space, nothing.
    let mut lobby = crate::test_support::conversation("Ada");
    lobby.choose_menu(crate::hand_menu::Choice::AgentReturn);
    lobby.choose_menu(crate::hand_menu::Choice::AgentMovement);
    lobby.send_agent_home();
    assert!(lobby.menu_model().movement.is_none());
    assert!(!lobby.menu_model().calls);
}

#[test]
fn a_participant_pins_the_agent_or_lets_it_follow_again_from_the_hand_menu() {
    use tomori_protocol::{AgentMovement, Movement};
    let following = |pinned: bool| {
        space(move |state| {
            state.movement = Some(AgentMovement::new(Movement::Follow, Some(90)));
            state.pins = true;
            state.pinned = pinned;
        })
    };
    let mut conversation = joined(|_| {});
    // Following, unpinned, it is asked to stay, and only that.
    conversation.apply(following(false));
    assert_eq!(conversation.menu_model().pins, Some(true));
    conversation.choose_menu(crate::hand_menu::Choice::AgentStay);
    conversation.pin_agent(false);
    assert_eq!(
        sent(&mut conversation),
        [ClientMessage::PinAgent { pinned: true }]
    );
    // Pinned, it is asked to follow again, and only that.
    conversation.apply(following(true));
    assert_eq!(conversation.menu_model().pins, Some(false));
    conversation.choose_menu(crate::hand_menu::Choice::AgentFollow);
    conversation.pin_agent(true);
    assert_eq!(
        sent(&mut conversation),
        [ClientMessage::PinAgent { pinned: false }]
    );
    // Moving only when asked, or kept home, neither is offered.
    for mode in [Movement::Asked, Movement::Home] {
        conversation.apply(space(move |state| {
            state.movement = Some(AgentMovement::new(mode, Some(90)));
            state.pins = true;
        }));
        assert_eq!(conversation.menu_model().pins, None);
    }
    // Nor to a visitor.
    conversation.apply(following(false));
    conversation.apply(Event::Message(ServerMessage::Role {
        role: ConnectionRole::Visitor,
    }));
    assert_eq!(conversation.menu_model().pins, None);
    conversation.pin_agent(true);
    assert!(sent(&mut conversation).is_empty());
}

#[test]
fn a_participant_has_the_agent_walk_with_them_or_stop_from_the_hand_menu() {
    use tomori_protocol::{AgentMovement, Movement};
    // This participant is number 1.
    let walking = |mode: Movement, walks: bool, walking: Option<u32>| {
        space(move |state| {
            state.movement = Some(AgentMovement::new(mode, Some(90)));
            state.walks = walks;
            state.walking = walking;
        })
    };
    let mut conversation = joined(|_| {});
    // Walking with nobody, or with someone else, it is asked to walk with them, and only that, wherever it may move.
    for (mode, walker) in [(Movement::Follow, None), (Movement::Asked, Some(2))] {
        conversation.apply(walking(mode, true, walker));
        assert_eq!(conversation.menu_model().walks, Some(true));
        conversation.choose_menu(crate::hand_menu::Choice::AgentWalk);
        conversation.walk_agent(false);
        assert_eq!(
            sent(&mut conversation),
            [ClientMessage::WalkAgent { walking: true }]
        );
    }
    // Walking with them, it is asked to stop, and only that.
    conversation.apply(walking(Movement::Follow, true, Some(1)));
    assert_eq!(conversation.menu_model().walks, Some(false));
    conversation.choose_menu(crate::hand_menu::Choice::AgentWalkEnd);
    conversation.walk_agent(true);
    assert_eq!(
        sent(&mut conversation),
        [ClientMessage::WalkAgent { walking: false }]
    );
    // Kept home, or from a server that takes no walks, neither is offered.
    for (mode, walks) in [(Movement::Home, true), (Movement::Follow, false)] {
        conversation.apply(walking(mode, walks, None));
        assert_eq!(conversation.menu_model().walks, None);
        conversation.walk_agent(true);
        assert!(sent(&mut conversation).is_empty());
    }
    // Nor to a visitor.
    conversation.apply(walking(Movement::Follow, true, None));
    conversation.apply(Event::Message(ServerMessage::Role {
        role: ConnectionRole::Visitor,
    }));
    assert_eq!(conversation.menu_model().walks, None);
    conversation.walk_agent(true);
    assert!(sent(&mut conversation).is_empty());
    // Nor outside a space.
    let mut lobby = crate::test_support::conversation("Ada");
    lobby.walk_agent(true);
    assert!(lobby.menu_model().walks.is_none());
}

#[test]
fn while_the_agent_follows_the_speaker_its_workers_stay_home_and_it_faces_whoever_it_turned_to() {
    let away = tomori_protocol::Pose::at([1.0, 1.7, -1.0]).facing_towards([0.0, 1.6, 0.0]);
    // Gone to someone: its workers stay home, gathered about the eyes there.
    let conversation = joined(|state| {
        state.movement = Some(tomori_protocol::AgentMovement::default());
        state.away = Some(away);
    });
    let arrangement = conversation.arrangement();
    assert!(arrangement.moved());
    assert_eq!(arrangement.workers, arrangement.home);
    assert_eq!(arrangement.audience, crate::anchors::EYES);
    // Away, it knows how far it stands from whoever it stands for, or else whoever it turned to; at home, or from
    // a server that says neither, or nonsense, nothing.
    let near = |partner: Option<[f32; 3]>, facing: Option<[f32; 3]>, went| {
        joined(|state| {
            state.away = went;
            state.partner = partner;
            state.facing = facing;
        })
        .arrangement()
        .nearness()
    };
    let beside = Some([1.0, 1.6, 0.5]);
    let close = |near: Option<f32>, want: f32| near.is_some_and(|near| (near - want).abs() < 1e-4);
    assert!(close(near(beside, None, Some(away)), 1.5));
    assert!(close(near(None, Some([1.0, 1.6, 2.0]), Some(away)), 3.0));
    assert_eq!(near(beside, None, None), None);
    assert_eq!(near(None, None, Some(away)), None);
    assert_eq!(near(Some([f32::NAN, 0.0, 0.0]), None, Some(away)), None);
    // Sent home, it is home for whoever sent it, standing for nobody.
    let mut conversation = joined(|state| {
        state.movement = Some(tomori_protocol::AgentMovement::default());
        state.away = Some(away);
        state.partner = beside;
    });
    conversation.send_agent_home();
    assert_eq!(conversation.arrangement().nearness(), None);
}

#[test]
fn a_space_shows_the_avatar_its_backend_names_where_this_build_has_it_and_says_which_it_lacks() {
    use crate::avatar::{Avatar, Factory};
    let factory: Factory = std::sync::Arc::new(|_: &str| -> Box<dyn Avatar> {
        Box::new(crate::avatar::SwarmAvatar::default())
    });
    let missing =
        |conversation: &Conversation| conversation.menu_model().space.unwrap().missing_avatar;
    let river = |state: &mut SpaceState| {
        state.avatar = Some("river".into());
        state.speech = SpeechSetting::Brief;
    };
    // Outside a space, none: the swarm.
    let mut lobby = in_lobby(Vec::new(), Vec::new());
    lobby.set_avatars(vec![("river".into(), factory.clone())]);
    assert!(lobby.avatar().is_none());
    // A space whose backend names none shows the swarm, and says nothing of it.
    let mut conversation = joined(|_| {});
    assert!(conversation.avatar().is_none());
    assert!(!conversation.text().contains("avatar"));
    assert_eq!(missing(&conversation), None);
    // One this build lacks: the swarm, named only in an owner's hand menu, on a row of its own, as a diagnostic, and
    // never on the panel, whose status line says what it always says.
    conversation.apply(space(river));
    assert!(conversation.avatar().is_none());
    let text = conversation.text();
    assert!(text.contains("Mic off | Brief | 1 here\n"), "{text}");
    assert!(!text.contains("avatar"), "{text}");
    assert_fits(&mut conversation);
    assert_eq!(missing(&conversation).as_deref(), Some("river"));
    assert_eq!(
        conversation.menu_model().space.unwrap().running,
        "opencode in tomori"
    );
    // A collaborator or a visitor sees the default swarm, and is told nothing of it.
    for role in [ConnectionRole::Collaborator, ConnectionRole::Visitor] {
        let mut other = crate::test_support::conversation("Bo");
        other.connect(Choice::new("Studio", "10.0.0.1:8765", Source::Discovered));
        other.apply(Event::State(ConnectionState::Connected {
            source: 2,
            server: "studio".into(),
            role,
            certificate: None,
            backends: vec![],
            roots: vec![],
            environments: sky(),
            utterance_limit: None,
            name: None,
        }));
        other.apply(space(river));
        assert!(other.avatar().is_none());
        assert_eq!(missing(&other), None, "{role:?}");
        assert!(!other.text().contains("avatar"));
    }
    conversation.apply(space(river));
    // Registered, it is chosen for the space, and the notice goes.
    conversation.set_avatars(vec![("river".into(), factory.clone())]);
    assert_eq!(conversation.avatars().len(), 1);
    let choice = conversation.avatar().expect("this build has it");
    assert_eq!((choice.space, choice.avatar), (SPACE, "river"));
    assert!(std::sync::Arc::ptr_eq(choice.factory, &factory));
    assert!(!choice.failures.contains("river"));
    // The renderer's failures, shared with it, go with the choice, for the coordinator to fall back on.
    let failures = crate::avatar::Failures::default();
    conversation.set_avatar_failures(failures.clone());
    failures.report("river");
    let choice = conversation.avatar().expect("this build has it");
    assert!(choice.failures.contains("river"));
    assert_eq!(
        (choice.factory)(SPACE).bounds().radius,
        crate::avatar::SWARM_RADIUS
    );
    assert!(!conversation.text().contains("avatar"));
    assert_eq!(missing(&conversation), None);
    // The swarm, named, is built in: shown without a notice, unless the build registers its own.
    conversation.apply(space(|state| state.avatar = Some("swarm".into())));
    assert!(conversation.avatar().is_none());
    assert!(!conversation.text().contains("avatar"));
    conversation.set_avatars(vec![("swarm".into(), factory.clone())]);
    assert_eq!(conversation.avatar().unwrap().avatar, "swarm");
    // An identifier a client does not read is none.
    for unreadable in ["river\n", "riv\u{202E}er", "river\u{200B}"] {
        conversation.apply(space(|state| state.avatar = Some(unreadable.into())));
        assert!(conversation.avatar().is_none());
        assert!(!conversation.text().contains("avatar"));
    }
    // Leaving, none.
    conversation.set_avatars(vec![("river".into(), factory)]);
    conversation.apply(space(river));
    assert!(conversation.avatar().is_some());
    hold_b(&mut conversation);
    assert!(conversation.avatar().is_none());
}

#[test]
fn the_server_list_shows_advertised_counts() {
    let mut conversation = crate::test_support::conversation("Ada");
    let mut studio = Choice::new("Studio", "10.0.0.1:8765", Source::Discovered);
    studio.spaces = Some(2);
    studio.participants = Some(1);
    let usb = Choice::new("USB", "127.0.0.1:8765", Source::Usb);
    conversation.set_servers(vec![studio, usb]);
    let text = conversation.text();
    assert!(text.contains("> Studio (2 spaces, 1 person)\n"), "{text}");
    assert!(text.contains("\nUSB 127.0.0.1:8765\n"), "{text}");
}

#[test]
fn an_incompatible_server_cannot_be_chosen() {
    let mut conversation = crate::test_support::conversation("Ada");
    let mut old = Choice::new("Old", "10.0.0.3:8765", Source::Discovered);
    old.unavailable = Some("protocol 1, needs 2".into());
    conversation.set_servers(vec![old]);
    assert!(conversation.text().contains("(Old - protocol 1, needs 2)"));
    conversation.accept();
    assert!(conversation.take_actions().is_empty());
    assert_eq!(conversation.mode(), Mode::Servers);
    assert!(conversation.text().contains("Cannot join Old"));
}

#[test]
fn approval_with_options_sends_grant_with_the_chosen_option() {
    let mut conversation = joined(|state| {
        state.activity = Activity::Waiting;
        state.approval = Some(approval(&[
            ("project", "Always allow in this project"),
            ("session", "Allow for this session"),
        ]));
    });
    assert!(conversation.is_choosing());
    assert!(conversation.text().contains("1/4: Allow once"));
    conversation.choose(1);
    conversation.choose(1);
    assert!(conversation.text().contains("3/4: Allow for this session"));
    conversation.choose(1);
    conversation.choose(1);
    assert!(conversation.text().contains("4/4: Deny"));
    conversation.choose(-1);
    conversation.choose(-1);
    assert_fits(&mut conversation);
    assert!(!conversation.accept());
    assert_eq!(
        sent(&mut conversation),
        [ClientMessage::Decide {
            request: "request-1".into(),
            decision: Decision::Grant,
            option: Some("project".into()),
        }]
    );
    // Further presses neither decide again nor leave while the decision is on its way.
    assert!(!conversation.accept());
    conversation.back();
    assert!(sent(&mut conversation).is_empty());
    assert_eq!(conversation.mode(), Mode::Space);
    assert!(conversation.text().contains("Sending decision..."));
}

#[test]
fn deny_is_an_explicit_choice_and_b_never_decides() {
    let mut conversation = joined(|state| state.approval = Some(approval(&[])));
    assert!(conversation.is_choosing());
    assert!(conversation.text().contains("1/2: Allow once"));
    // B, pressed or held, neither denies nor leaves while an approval waits.
    conversation.back();
    hold_b_briefly(&mut conversation);
    assert!(sent(&mut conversation).is_empty());
    assert_eq!(conversation.mode(), Mode::Space);
    conversation.choose(1);
    assert!(conversation.text().contains("2/2: Deny"));
    conversation.accept();
    assert_eq!(
        sent(&mut conversation),
        [ClientMessage::Decide {
            request: "request-1".into(),
            decision: Decision::Deny,
            option: None,
        }]
    );

    let mut conversation = joined(|state| state.approval = Some(approval(&[])));
    conversation.accept();
    assert_eq!(
        sent(&mut conversation),
        [ClientMessage::Decide {
            request: "request-1".into(),
            decision: Decision::AllowOnce,
            option: None,
        }]
    );
}

#[test]
fn no_decision_is_sent_without_a_press() {
    let mut conversation = joined(|_| {});
    for _ in 0..3 {
        conversation.apply(space(|state| {
            state.approval = Some(approval(&[("a", "A")]));
        }));
        conversation.apply(connected());
        conversation.navigate(1, 1);
        conversation.choose(1);
        conversation.refresh();
        assert_fits(&mut conversation);
    }
    assert!(
        sent(&mut conversation)
            .iter()
            .all(|message| !matches!(message, ClientMessage::Decide { .. }))
    );
}

#[test]
fn a_new_approval_resets_the_choice_and_a_refused_decision_can_be_retried() {
    let mut conversation = joined(|state| state.approval = Some(approval(&[("a", "A")])));
    conversation.choose(1);
    conversation.accept();
    conversation.take_actions();
    conversation.apply(Event::Message(ServerMessage::Error {
        error: "That request is no longer pending.".into(),
        space: None,
        panel: None,
        wall: None,
    }));
    assert!(conversation.is_choosing());
    assert!(conversation.text().contains("That request"));

    conversation.apply(space(|state| {
        let mut next = approval(&[("b", "B")]);
        next.id = "request-2".into();
        state.approval = Some(next);
    }));
    assert!(conversation.text().contains("1/3: Allow once"));
    // A pressed as it appears does nothing, then decides once it has been shown for a moment.
    conversation.accept();
    assert!(sent(&mut conversation).is_empty());
    assert!(conversation.text().contains("Read the request"));
    settle(&mut conversation);
    conversation.accept();
    assert_eq!(
        sent(&mut conversation),
        [ClientMessage::Decide {
            request: "request-2".into(),
            decision: Decision::AllowOnce,
            option: None,
        }]
    );
}

#[test]
fn a_standing_option_is_shown_in_full() {
    let label =
        "Yes, and don't ask again for Bash commands starting with cargo test in this project";
    let mut conversation = joined(|state| state.approval = Some(approval(&[("always", label)])));
    conversation.choose(1);
    let text = conversation.text();
    assert_eq!(text.lines().count(), ROWS);
    let shown = text.split_whitespace().collect::<Vec<_>>().join(" ");
    assert!(shown.contains(&format!("2/3: {label}")), "{text}");
    // Even with a problem shown above it.
    conversation.apply(Event::Message(ServerMessage::Error {
        error: "The mode was not changed: the agent refused it, for a reason long enough to wrap."
            .into(),
        space: None,
        panel: None,
        wall: None,
    }));
    let text = conversation.text();
    let shown = text.split_whitespace().collect::<Vec<_>>().join(" ");
    assert!(shown.contains(&format!("2/3: {label}")), "{text}");
    assert_fits(&mut conversation);
}

#[test]
fn activity_is_never_ready_unless_the_server_says_idle() {
    let mut conversation = crate::test_support::conversation("Ada");
    assert_eq!(conversation.activity(), SessionActivity::Unselected);

    let cases = [
        (Activity::Idle, SessionActivity::Ready),
        (Activity::Running, SessionActivity::Working),
        (Activity::Waiting, SessionActivity::Waiting),
        (Activity::Closed, SessionActivity::Unavailable),
        (Activity::Unknown, SessionActivity::Unknown),
    ];
    for (activity, expected) in cases {
        conversation = joined(|state| state.activity = activity);
        assert_eq!(conversation.activity(), expected, "{activity:?}");
    }

    let conversation = joined(|state| {
        state.queue = vec![entry("1", "Bea", QueueState::Queued)];
    });
    assert_eq!(conversation.activity(), SessionActivity::Working);
    let conversation = joined(|state| state.question = Some(question()));
    assert_eq!(conversation.activity(), SessionActivity::Waiting);

    let mut conversation = joined(|_| {});
    conversation.apply(Event::State(ConnectionState::Disconnected {
        reason: tomori_client::Disconnect::Timeout,
        retry_in: Duration::from_secs(2),
    }));
    assert_eq!(conversation.activity(), SessionActivity::Unavailable);
    assert!(conversation.text().contains("Offline, retry in 2s"));
    assert_fits(&mut conversation);
}

#[test]
fn the_joined_space_is_current_only_while_connected_and_gone_after_leaving() {
    assert!(
        crate::test_support::conversation("Ada")
            .joined_space()
            .is_none()
    );
    let mut conversation = joined(|state| {
        state.workers = vec![tomori_protocol::Worker {
            id: "w".into(),
            parent: None,
            title: "Explore".into(),
            state: tomori_protocol::WorkerState::Running,
        }]
    });
    let (space, current) = conversation.joined_space().unwrap();
    assert_eq!(
        (space.id.as_str(), space.workers.len(), current),
        (SPACE, 1, true)
    );
    conversation.apply(Event::State(ConnectionState::Disconnected {
        reason: tomori_client::Disconnect::Timeout,
        retry_in: Duration::from_secs(2),
    }));
    assert!(!conversation.joined_space().unwrap().1);
    conversation.apply(connected());
    assert!(conversation.joined_space().unwrap().1);
    hold_b(&mut conversation);
    assert!(conversation.joined_space().is_none());
}

#[test]
fn an_unknown_activity_from_a_newer_server_is_shown_as_unknown() {
    let state: SpaceState = serde_json::from_str(
        r#"{"id":"s","title":"t","backend":"b","directory":"/","activity":"thinking",
        "messages":[],"queue":[],"approval":null,"question":null,"workers":[],
        "presentations":[],"participants":[],"attribution":true,"error":null}"#,
    )
    .unwrap();
    let mut conversation = joined(|_| {});
    conversation.apply(Event::Message(ServerMessage::Space {
        state: Box::new(state),
    }));
    assert_eq!(conversation.activity(), SessionActivity::Unknown);
    assert!(conversation.text().starts_with("[unknown] t"));
}

fn question() -> Question {
    Question {
        id: "question-1".into(),
        title: "Which test?".into(),
        text: "Pick one".into(),
        field: "test".into(),
        supported: true,
    }
}

#[test]
fn holding_x_while_a_question_is_shown_answers_it() {
    let mut conversation = joined(|state| {
        state.speech = SpeechSetting::Brief;
        state.voice_answers = true;
        state.question = Some(question());
        state.workers = vec![tomori_protocol::Worker {
            id: "worker-1".into(),
            parent: None,
            title: "Run the tests".into(),
            state: tomori_protocol::WorkerState::Running,
        }];
    });
    let text = conversation.text();
    assert!(!text.contains("on the PC"), "{text}");
    assert!(text.ends_with("Hold X to answer | hold B leave"), "{text}");
    // An answer is about the question, so a worker pointed at as X is pressed is not sent with it.
    conversation.point_at(Some("worker-1"));
    conversation.talk(true);
    let actions = conversation.take_actions();
    assert!(
        matches!(
            &actions[..],
            [Action::BeginUtterance(Utterance { answer: Some(target), naming: false, attention, .. })]
                if target.question == "question-1" && target.field == "test" && attention.is_empty()
        ),
        "{actions:?}"
    );
    assert!(conversation.text().contains("MIC ANSWER"));
    conversation.talk(false);
    assert_eq!(conversation.take_actions(), [Action::EndUtterance]);

    // A question Tomori cannot answer, or a server that cannot route answers, is answered on the PC, and X talks to the agent.
    for change in [
        (|state: &mut SpaceState| {
            state.voice_answers = true;
            state.question = Some(Question {
                supported: false,
                ..question()
            });
        }) as fn(&mut SpaceState),
        // A server that reports a speech setting but does not say it routes answers would queue them.
        |state: &mut SpaceState| {
            state.speech = SpeechSetting::Brief;
            state.question = Some(question());
        },
    ] {
        let mut conversation = joined(change);
        assert!(conversation.text().contains("on the PC"));
        conversation.talk(true);
        let [
            Action::BeginUtterance(Utterance {
                answer: None,
                naming: false,
                ..
            }),
        ] = &conversation.take_actions()[..]
        else {
            unreachable!("[Action::BeginUtterance")
        };
        assert!(conversation.text().contains("MIC AGENT"));
    }
}

#[test]
fn a_new_name_is_said_with_x_once_chosen_and_the_servers_name_is_shown() {
    crate::test_support::log_everything();
    use crate::hand_menu::Choice as MenuChoice;
    use crate::recording::Phase;
    let mut conversation = joined(|state| {
        state.voice_answers = true;
        state.question = Some(question());
        state.workers = vec![tomori_protocol::Worker {
            id: "worker-1".into(),
            parent: None,
            title: "Run the tests".into(),
            state: tomori_protocol::WorkerState::Running,
        }];
    });
    // An older server says no name, so the suggested one is shown and nothing renames.
    assert_eq!(conversation.display_name(), "Ada");
    assert!(!conversation.menu_model().renames);
    conversation.choose_menu(MenuChoice::Rename);
    assert!(!conversation.menu_model().naming);

    // A newer server says the name it keeps, which may differ.
    conversation.apply(Event::State(ConnectionState::Connected {
        source: 1,
        server: "studio".into(),
        role: ConnectionRole::Owner,
        certificate: None,
        backends: vec![],
        roots: vec![],
        environments: vec![],
        utterance_limit: None,
        name: Some("Quest 3".into()),
    }));
    conversation.take_actions();
    assert_eq!(conversation.menu_model().name, "Quest 3");
    assert!(conversation.menu_model().renames);
    // Chosen and chosen again, nothing changes after all.
    conversation.choose_menu(MenuChoice::Rename);
    conversation.choose_menu(MenuChoice::Rename);
    assert!(!conversation.menu_model().naming);
    conversation.choose_menu(MenuChoice::Rename);
    assert!(conversation.menu_model().naming);
    assert!(
        conversation
            .text()
            .ends_with("Hold X to say your name | B cancel")
    );

    // X then says the name, which answers no question and is about no worker.
    conversation.point_at(Some("worker-1"));
    conversation.talk(true);
    let actions = conversation.take_actions();
    let [
        Action::BeginUtterance(Utterance {
            id: utterance,
            answer: None,
            naming: true,
            attention,
            ..
        }),
    ] = &actions[..]
    else {
        unreachable!("{actions:?}")
    };
    assert!(attention.is_empty(), "{attention:?}");
    let utterance = utterance.clone();
    assert!(!conversation.menu_model().naming, "once only");
    assert!(conversation.text().contains("MIC NAME: listening..."));
    let now = Instant::now();
    assert!(matches!(
        conversation.recording_phase(now),
        Some(Phase::Naming { heard, .. }) if heard.is_empty()
    ));
    conversation.point_at(Some("worker-1"));
    conversation.apply(Event::Message(ServerMessage::Transcript {
        utterance: utterance.clone(),
        text: "Ada Love".into(),
        finished: false,
    }));
    assert!(conversation.text().contains("MIC NAME: Ada Love"));
    conversation.talk(false);
    assert_eq!(conversation.take_actions(), [Action::EndUtterance]);
    assert_eq!(
        conversation.recording_phase(now),
        Some(Phase::Renaming {
            what: Naming::Name,
            heard: "Ada Love".into()
        })
    );
    conversation.apply(Event::Message(ServerMessage::Transcript {
        utterance,
        text: "Ada Lovelace".into(),
        finished: true,
    }));
    assert_eq!(conversation.display_name(), "Ada Lovelace");
    assert_eq!(
        conversation.recording_phase(Instant::now()),
        Some(Phase::Named {
            what: Naming::Name,
            name: "Ada Lovelace".into()
        })
    );
    assert_eq!(
        conversation.recording_phase(Instant::now() + crate::recording::OUTCOME),
        None
    );

    // Offline, nothing is sent.
    conversation.apply(Event::State(ConnectionState::Disconnected {
        reason: tomori_client::Disconnect::Closed,
        retry_in: Duration::from_secs(1),
    }));
    conversation.take_actions();
    conversation.rename("Ada");
    assert!(sent(&mut conversation).is_empty());
    assert_eq!(
        conversation.menu_model().notice.as_deref(),
        Some("This server does not change names.")
    );

    // A rename from elsewhere is shown too, and the next server starts afresh.
    conversation.apply(Event::Message(ServerMessage::Name { name: "Ada".into() }));
    assert_eq!(conversation.menu_model().name, "Ada");
    conversation.choose_menu(MenuChoice::Rename);
    conversation.connect(Choice::new("Other", "10.0.0.2:8765", Source::Discovered));
    assert_eq!(conversation.display_name(), "Ada");
    assert!(!conversation.menu_model().naming);
}

#[test]
fn words_begin_naming_the_newest_message_of_the_agent_shown_as_x_is_pressed() {
    let message = |id: &str, role, text: &str| tomori_protocol::Message {
        id: id.into(),
        role,
        text: text.into(),
        completed: true,
    };
    let mut conversation = joined(|state| {
        state.messages = vec![
            message("m1", Role::Assistant, "Should I do A?"),
            // Neither the participant's own words nor a step that says nothing are the agent's latest.
            message("u1", Role::User, "Ada: Hmm."),
            message("m2", Role::Assistant, "  "),
        ];
    });
    let begun = |conversation: &mut Conversation| {
        conversation.talk(true);
        let actions = conversation.take_actions();
        let [Action::BeginUtterance(begun)] = &actions[..] else {
            unreachable!("{actions:?}")
        };
        begun.seen.clone()
    };
    assert_eq!(begun(&mut conversation).as_deref(), Some("m1"));
    // What arrives while X is held does not change it: it is captured as the words begin.
    conversation.apply(space(|state| {
        state.messages = vec![
            message("m1", Role::Assistant, "Should I do A?"),
            message("m3", Role::Assistant, "Should I do B?"),
        ];
    }));
    conversation.take_actions();
    conversation.talk(false);
    conversation.take_actions();
    assert_eq!(begun(&mut conversation).as_deref(), Some("m3"));
    // Nothing the agent said yet names nothing, as an older client sends.
    let mut quiet = joined(|_| {});
    assert_eq!(begun(&mut quiet), None);
}

/// Press X, which begins words that say the space's new title; returns the utterance.
fn say_title(conversation: &mut Conversation) -> String {
    conversation.talk(true);
    let actions = conversation.take_actions();
    let [Action::BeginUtterance(begun)] = &actions[..] else {
        unreachable!("{actions:?}")
    };
    assert!(begun.titling && !begun.naming, "{actions:?}");
    assert!(begun.answer.is_none() && begun.attention.is_empty());
    begun.id.clone()
}

#[test]
fn the_lobby_says_where_each_space_is_what_it_is_doing_and_its_newest_reply() {
    let mut conversation = crate::test_support::conversation("Ada");
    conversation.connect(Choice::new("Studio", "10.0.0.1:8765", Source::Discovered));
    conversation.apply(connected());
    let described = SpaceSummary {
        folder: Some("ioquatix/tomori".into()),
        active_age: Some(300),
        latest: Some("All 42 tests pass.".into()),
        participants: 2,
        ..summary("space-1", "Tomori")
    };
    let older = SpaceSummary {
        directory: "/work/base-rs/".into(),
        environment: String::new(),
        participants: 0,
        ..summary("space-2", "Base")
    };
    listed(&mut conversation, vec![described, older]);
    let view = conversation.arrival().unwrap();
    assert_eq!(
        view.items[0].detail.as_deref(),
        Some("ioquatix/tomori · Sky\nidle · 2 here · 5m ago\n“All 42 tests pass.”")
    );
    // From an older server, the folder is the directory's last part, and nothing is made up.
    assert_eq!(view.items[1].detail.as_deref(), Some("base-rs\nidle"));
    let text = conversation.text();
    // Its folder last, cut to the console's width.
    assert!(
        text.contains("> Tomori [idle, 2 here, 5m ago] ioquati"),
        "{text}"
    );
    assert!(text.contains("Base [idle]"), "{text}");
    for (seconds, said) in [
        (0, "just now"),
        (59, "just now"),
        (60, "1m ago"),
        (3599, "59m ago"),
        (3600, "1h ago"),
        (86_399, "23h ago"),
        (86_400, "1d ago"),
    ] {
        assert_eq!(ago(seconds), said);
    }
}

#[test]
fn an_owner_says_the_space_s_new_title_and_x_says_it_again_until_b_cancels() {
    use crate::hand_menu::Choice as MenuChoice;
    use crate::recording::Phase;
    // From a server that does not rename spaces, nothing is offered, and X talks to the agent.
    let mut conversation = joined(|_| {});
    assert!(!conversation.menu_model().space.unwrap().retitles);
    conversation.choose_menu(MenuChoice::RenameSpace);
    assert!(!conversation.menu_model().titling);
    say(&mut conversation, false);

    let mut conversation = joined(|state| state.renames = true);
    assert!(conversation.menu_model().space.unwrap().retitles);
    // Choosing it twice changes nothing after all; choosing a name instead stops waiting for the title.
    conversation.choose_menu(MenuChoice::RenameSpace);
    conversation.choose_menu(MenuChoice::RenameSpace);
    assert!(!conversation.menu_model().titling);
    conversation.choose_menu(MenuChoice::RenameSpace);
    assert!(conversation.menu_model().titling);
    assert!(
        conversation
            .text()
            .ends_with("Hold X to say the title | B cancel")
    );
    let utterance = say_title(&mut conversation);
    assert!(!conversation.menu_model().titling, "once only");
    let Some(Phase::Naming {
        what: Naming::Title,
        ..
    }) = conversation.recording_phase(Instant::now())
    else {
        unreachable!("saying the title")
    };
    assert!(conversation.text().contains("MIC TITLE"));
    conversation.talk(false);
    conversation.take_actions();
    // Refused, the title is not changed, and X says it again until B cancels.
    conversation.apply(Event::Message(ServerMessage::TranscriptFailed {
        utterance,
        error: "No title was heard.".into(),
    }));
    assert!(conversation.menu_model().titling);
    assert_eq!(
        conversation.error.as_deref(),
        Some("Title not changed: No title was heard.")
    );
    assert_eq!(
        conversation.recording_phase(Instant::now()),
        Some(Phase::NameFailed {
            what: Naming::Title,
            reason: "No title was heard.".into()
        })
    );
    let again = say_title(&mut conversation);
    conversation.talk(false);
    conversation.take_actions();
    conversation.apply(Event::Message(ServerMessage::Transcript {
        utterance: again,
        text: "Release planning".into(),
        finished: true,
    }));
    assert_eq!(
        conversation.recording_phase(Instant::now()),
        Some(Phase::Named {
            what: Naming::Title,
            name: "Release planning".into()
        })
    );
    // The participant's own name is not the title.
    assert_eq!(conversation.display_name(), "Ada");
    say(&mut conversation, false);
    conversation.talk(false);
    conversation.take_actions();

    // B stops waiting for the title, and leaving the space does too.
    conversation.choose_menu(MenuChoice::RenameSpace);
    conversation.back();
    assert!(!conversation.menu_model().titling);
    conversation.choose_menu(MenuChoice::RenameSpace);
    conversation.leave();
    assert!(!conversation.menu_model().titling);

    // A collaborator is offered none of it.
    let mut conversation = joined(|state| state.renames = true);
    conversation.apply(role(ConnectionRole::Collaborator));
    assert!(!conversation.menu_model().space.unwrap().retitles);
    conversation.choose_menu(MenuChoice::RenameSpace);
    assert!(!conversation.menu_model().titling);
    conversation.retitle("Mine");
    assert_eq!(
        conversation.menu_model().notice.as_deref(),
        Some("Only owners can rename a space.")
    );
    assert!(conversation.take_actions().is_empty());
}

#[test]
fn a_change_of_title_is_told_with_who_made_it_and_an_owner_lets_it_follow_its_session() {
    use crate::hand_menu::Choice as MenuChoice;
    let mut conversation = joined(|state| state.renames = true);
    // Following its session, it says only what the space is now called.
    conversation.apply(space(|state| {
        state.renames = true;
        state.title = "Tomori development".into();
    }));
    assert_eq!(
        conversation.menu_model().notice.as_deref(),
        Some("This space is now called “Tomori development”.")
    );
    // Renamed by the agent, or an owner, it says by whom, and the menu shows it.
    conversation.apply(space(|state| {
        state.renames = true;
        state.title = "Release planning".into();
        state.renamed_by = Some(tomori_protocol::Renamer::Agent);
    }));
    assert_eq!(
        conversation.menu_model().notice.as_deref(),
        Some("Tomori renamed this space to “Release planning”.")
    );
    let menu = conversation.menu_model();
    assert_eq!(
        menu.space.as_ref().unwrap().renamed_by.as_deref(),
        Some("Tomori")
    );
    conversation.apply(space(|state| {
        state.renames = true;
        state.title = "Ada's".into();
        state.renamed_by = Some(tomori_protocol::Renamer::Participant { name: "Bob".into() });
    }));
    assert_eq!(
        conversation.menu_model().notice.as_deref(),
        Some("Bob renamed this space to “Ada's”.")
    );
    // Someone a newer server names that this build does not know.
    conversation.apply(space(|state| {
        state.renames = true;
        state.title = "Theirs".into();
        state.renamed_by = Some(tomori_protocol::Renamer::Unknown);
    }));
    assert_eq!(
        conversation.menu_model().notice.as_deref(),
        Some("someone renamed this space to “Theirs”.")
    );
    conversation.apply(space(|state| {
        state.renames = true;
        state.title = "Ada's".into();
        state.renamed_by = Some(tomori_protocol::Renamer::Participant { name: "Bob".into() });
    }));
    // The same title again tells nothing.
    conversation.notice = None;
    conversation.apply(space(|state| {
        state.renames = true;
        state.title = "Ada's".into();
    }));
    assert_eq!(conversation.menu_model().notice, None);
    // Following its session again is asked of the server with an empty title, as typing nothing on the console is.
    conversation.take_actions();
    conversation.choose_menu(MenuChoice::FollowSession);
    assert_eq!(
        conversation.take_actions(),
        [Action::Send(ClientMessage::RenameSpace {
            title: String::new()
        })]
    );
    conversation.retitle("  Release planning ");
    assert_eq!(
        conversation.take_actions(),
        [Action::Send(ClientMessage::RenameSpace {
            title: "Release planning".into()
        })]
    );
    assert_eq!(
        conversation.menu_model().notice.as_deref(),
        Some("Renaming this space to “Release planning”.")
    );
    // From a server that does not rename spaces, or outside a space, it says why not.
    let mut conversation = joined(|_| {});
    conversation.retitle("Nope");
    assert_eq!(
        conversation.menu_model().notice.as_deref(),
        Some("This server cannot rename spaces.")
    );
    conversation.leave();
    conversation.retitle("Nope");
    assert_eq!(
        conversation.menu_model().notice.as_deref(),
        Some("Join a space to rename it.")
    );
}

/// A conversation on a server that renames, in a space, choosing to say a new name.
fn naming() -> Conversation {
    use crate::hand_menu::Choice as MenuChoice;
    let mut conversation = joined(|_| {});
    conversation.apply(Event::Message(ServerMessage::Name {
        name: "Quest 3".into(),
    }));
    conversation.choose_menu(MenuChoice::Rename);
    assert!(conversation.menu_model().naming);
    conversation
}

/// Press X, which begins words that say a new name, or that are for the agent, as `naming` says; returns the utterance.
fn say(conversation: &mut Conversation, naming: bool) -> String {
    conversation.talk(true);
    let actions = conversation.take_actions();
    let [Action::BeginUtterance(begun)] = &actions[..] else {
        unreachable!("{actions:?}")
    };
    assert_eq!(begun.naming, naming, "{actions:?}");
    begun.id.clone()
}

#[test]
fn a_name_not_changed_is_said_again_with_x_until_b_cancels() {
    use crate::recording::Phase;
    // Refused, not heard, or failed in transcription, the name is not changed and X says it again.
    for error in [
        "That name is taken.",
        "no speech heard",
        "The model failed.",
    ] {
        let mut conversation = naming();
        let utterance = say(&mut conversation, true);
        conversation.talk(false);
        conversation.take_actions();
        conversation.apply(Event::Message(ServerMessage::TranscriptFailed {
            utterance,
            error: error.into(),
        }));
        assert!(conversation.menu_model().naming, "{error}");
        assert_eq!(
            conversation.error.as_deref(),
            Some(format!("Name not changed: {error}").as_str())
        );
        let text = conversation.text();
        assert!(text.contains("Name not changed: "), "{text}");
        assert!(text.ends_with("Hold X to try again | B cancel"), "{text}");
        assert_eq!(
            conversation.recording_phase(Instant::now()),
            Some(Phase::NameFailed {
                what: Naming::Name,
                reason: error.into()
            })
        );
        // B stops waiting for the name and dismisses why, and X then talks to the agent.
        conversation.back();
        assert!(!conversation.menu_model().naming);
        assert!(!conversation.text().contains("Name not changed"));
        assert_eq!(conversation.recording_phase(Instant::now()), None);
        say(&mut conversation, false);
    }

    // Tried again, the name changes, and X talks to the agent after that.
    let mut conversation = naming();
    let first = say(&mut conversation, true);
    conversation.talk(false);
    conversation.take_actions();
    conversation.apply(Event::Message(ServerMessage::TranscriptFailed {
        utterance: first,
        error: "That name is taken.".into(),
    }));
    let again = say(&mut conversation, true);
    // Recording again, the failure is no longer shown on the indicator.
    let Some(Phase::Naming { .. }) = conversation.recording_phase(Instant::now()) else {
        unreachable!("Some")
    };
    conversation.talk(false);
    conversation.take_actions();
    conversation.apply(Event::Message(ServerMessage::Transcript {
        utterance: again,
        text: "Ada".into(),
        finished: true,
    }));
    assert_eq!(conversation.display_name(), "Ada");
    assert!(!conversation.menu_model().naming);
    say(&mut conversation, false);

    // B before saying anything cancels too, leaving a problem shown for B to dismiss next.
    let mut conversation = naming();
    conversation.fail("Something else.");
    conversation.back();
    assert!(!conversation.menu_model().naming);
    assert!(conversation.text().contains("Something else."));
    conversation.back();
    assert!(!conversation.text().contains("Something else."));
}

#[test]
fn a_name_whose_words_are_lost_is_said_again() {
    use crate::recording::Phase;
    // Words that could not be sent, or recorded, rename nobody, and X says the name again.
    let lost: [fn(&mut Conversation, &str); 3] = [
        |conversation, utterance| {
            conversation.apply(Event::UtteranceFailed(tomori_client::FailedUtterance {
                utterance: utterance.into(),
                failure: UtteranceFailure::Congested,
            }))
        },
        |conversation, _| conversation.microphone_failed("The microphone failed."),
        |conversation, _| {
            conversation.apply(Event::State(ConnectionState::Disconnected {
                reason: tomori_client::Disconnect::Closed,
                retry_in: Duration::from_secs(1),
            }))
        },
    ];
    for lose in lost {
        let mut conversation = naming();
        let utterance = say(&mut conversation, true);
        lose(&mut conversation, &utterance);
        assert!(conversation.menu_model().naming);
        let Some(Phase::NameFailed { .. }) = conversation.recording_phase(Instant::now()) else {
            unreachable!("Some")
        };
        assert!(conversation.text().contains("Name not changed: "));
    }
    // Released and on their way when the connection drops, the words may yet rename, and X says the name again meanwhile.
    let mut conversation = naming();
    say(&mut conversation, true);
    conversation.talk(false);
    conversation.apply(Event::State(ConnectionState::Disconnected {
        reason: tomori_client::Disconnect::Closed,
        retry_in: Duration::from_secs(1),
    }));
    assert!(conversation.menu_model().naming);
    // Taken off or out of focus while saying it, nothing is said, and X says it again.
    let mut conversation = naming();
    say(&mut conversation, true);
    conversation.stop_talking();
    assert!(conversation.menu_model().naming);
    assert_eq!(conversation.take_actions(), [Action::CancelUtterance]);
    say(&mut conversation, true);
}

#[test]
fn a_new_name_is_said_only_in_the_space_it_was_chosen_in() {
    // Leaving the space stops waiting for the name.
    let mut conversation = naming();
    hold_b(&mut conversation);
    conversation.apply(Event::Message(ServerMessage::LEFT));
    assert!(!conversation.menu_model().naming);
    // Watching as a visitor, who cannot record, nothing offers to say it.
    let mut conversation = naming();
    conversation.apply(Event::Message(ServerMessage::Role {
        role: ConnectionRole::Visitor,
    }));
    assert!(!conversation.text().contains("say your name"));
}

#[test]
fn a_chosen_deny_survives_updates_to_the_space() {
    let mut conversation = joined(|state| state.approval = Some(approval(&[])));
    conversation.choose(1);
    // Streaming text, presence and queue changes all resend the space.
    conversation.apply(space(|state| {
        state.approval = Some(approval(&[]));
        state.messages = vec![message("m1", Role::Assistant, "Working")];
    }));
    assert!(conversation.text().contains("2/2: Deny"));
    conversation.accept();
    assert_eq!(
        sent(&mut conversation),
        [ClientMessage::Decide {
            request: "request-1".into(),
            decision: Decision::Deny,
            option: None,
        }]
    );
}

#[test]
fn an_approval_arriving_during_an_undo_sends_the_cancel_and_takes_a_for_itself() {
    let queue = || vec![entry("1", "Ada", QueueState::Queued)];
    let mut conversation = joined(|state| {
        state.queue = queue();
        state.speech = SpeechSetting::Brief;
    });
    conversation.select_queued(1);
    conversation.accept();
    assert!(sent(&mut conversation).is_empty());
    conversation.refresh();
    conversation.apply(space(|state| {
        state.queue = queue();
        state.modes = vec![tomori_protocol::Mode {
            id: "plan".into(),
            name: "Plan".into(),
            description: None,
        }];
        state.approval = Some(approval(&[]));
    }));
    // The cancel goes now, and the list is closed, so the next A decides what is shown.
    assert_eq!(
        sent(&mut conversation),
        [ClientMessage::CancelQueued { entry: "1".into() }]
    );
    assert!(conversation.text().contains("APPROVAL"));
    conversation.tick_space(Instant::now() + CANCEL_UNDO);
    assert!(sent(&mut conversation).is_empty());
}

#[test]
fn leaving_sends_a_cancel_still_in_its_undo_time() {
    let mut conversation =
        joined(|state| state.queue = vec![entry("1", "Ada", QueueState::Queued)]);
    conversation.select_queued(1);
    conversation.accept();
    hold_b(&mut conversation);
    assert_eq!(
        sent(&mut conversation),
        [
            ClientMessage::CancelQueued { entry: "1".into() },
            ClientMessage::Leave
        ]
    );
}

#[test]
fn paging_and_following_use_the_rows_left_by_the_strip_and_problem() {
    let long: String = (0..30).map(|line| format!("line {line}\n")).collect();
    let mut conversation = joined(|state| {
        state.messages = vec![message("m1", Role::Assistant, &long)];
        state.queue = vec![
            entry("1", "Bea", QueueState::Queued),
            entry("2", "Bea", QueueState::Queued),
        ];
    });
    conversation.apply(Event::Message(ServerMessage::Error {
        error: "Something went wrong.".into(),
        space: None,
        panel: None,
        wall: None,
    }));
    // Following the newest message lands on its last page, as drawn.
    conversation.apply(space(|state| {
        state.messages = vec![message("m1", Role::Assistant, &long)];
        state.queue = vec![
            entry("1", "Bea", QueueState::Queued),
            entry("2", "Bea", QueueState::Queued),
        ];
    }));
    let text = conversation.text();
    assert!(text.contains("line 29"), "{text}");
    assert_fits(&mut conversation);
    // And paging back up and down again reaches it.
    for _ in 0..20 {
        conversation.navigate(0, -1);
    }
    assert!(conversation.text().contains("line 0"));
    for _ in 0..20 {
        conversation.navigate(0, 1);
    }
    assert!(conversation.text().contains("line 29"));
}

#[test]
fn the_list_keeps_its_selection_and_description_in_view() {
    let mut conversation = joined(|state| {
        state.speech = SpeechSetting::Brief;
        state.modes = (1..=6)
            .map(|index| tomori_protocol::Mode {
                id: format!("m{index}"),
                name: format!("Mode {index}"),
                description: Some(format!(
                    "The description of mode {index}, long enough that it has to wrap onto a second row."
                )),
            })
            .collect();
        state.queue = vec![entry("1", "Bea", QueueState::Queued)];
    });
    conversation.apply(Event::Message(ServerMessage::Error {
        error: "A problem long enough to take both of its rows on the panel, so the list has less room.".into(),
        space: None,
        panel: None,
        wall: None,
    }));
    conversation.refresh();
    for _ in 0..5 {
        conversation.navigate(0, 1);
    }
    let text = conversation.text();
    assert!(text.contains("> Mode: Mode 6"), "{text}");
    assert!(text.contains("The description of mode 6"), "{text}");
    assert_fits(&mut conversation);
    conversation.navigate(0, 1);
    let text = conversation.text();
    assert!(text.contains("> Speech: brief"), "{text}");
    assert!(text.contains("Replies speak"), "{text}");
    assert_fits(&mut conversation);
}

#[test]
fn a_problem_in_a_space_survives_y_and_a_reconnect() {
    let mut conversation = joined(|_| {});
    conversation.apply(Event::State(ConnectionState::Disconnected {
        reason: tomori_client::Disconnect::Timeout,
        retry_in: Duration::from_secs(1),
    }));
    conversation.apply(Event::Message(ServerMessage::Error {
        error: "The mode was not changed.".into(),
        space: None,
        panel: None,
        wall: None,
    }));
    conversation.apply(connected());
    conversation.refresh();
    assert!(conversation.text().contains("The mode was not changed."));
}

#[test]
fn following_shows_the_newest_lines_when_a_problem_takes_rows() {
    let long: String = (0..30).map(|line| format!("line {line}\n")).collect();
    let mut conversation =
        joined(|state| state.messages = vec![message("m1", Role::Assistant, &long)]);
    assert!(conversation.text().contains("line 29"));
    // A problem arrives without the space changing.
    conversation.apply(Event::Message(ServerMessage::Error {
        error: "Something went wrong.".into(),
        space: None,
        panel: None,
        wall: None,
    }));
    assert!(conversation.text().contains("line 29"));
    conversation.back();
    assert!(conversation.text().contains("line 29"));
}

#[test]
fn selecting_in_an_empty_queue_selects_nothing() {
    let mut conversation = joined(|_| {});
    for direction in [1, -1] {
        conversation.select_queued(direction);
        conversation.accept();
        assert!(sent(&mut conversation).is_empty());
    }
}

#[test]
fn a_visitor_cannot_select_or_cancel_queued_entries() {
    let mut conversation =
        joined(|state| state.queue = vec![entry("1", "Bea", QueueState::Queued)]);
    conversation.apply(Event::Message(ServerMessage::Role {
        role: ConnectionRole::Visitor,
    }));
    assert!(!conversation.selects_queue());
    conversation.select_queued(1);
    conversation.accept();
    conversation.tick_space(Instant::now() + CANCEL_UNDO);
    assert!(sent(&mut conversation).is_empty());
    assert!(conversation.text().contains("Visitors can watch"));
}

#[test]
fn redraws_a_message_whose_text_changes_to_the_same_length() {
    let mut conversation = joined(|state| {
        state.messages = vec![message("m1", Role::Assistant, "Hello")];
    });
    assert!(conversation.text().contains("Hello"));
    conversation.apply(space(|state| {
        state.messages = vec![message("m1", Role::Assistant, "Howdy")];
    }));
    let text = conversation.text();
    assert!(text.contains("Howdy") && !text.contains("Hello"));
}

#[test]
fn reconnecting_accepts_the_space_the_server_rejoins() {
    let mut conversation = joined(|state| {
        state.messages = vec![message("m1", Role::Assistant, "Hello")];
    });
    conversation.apply(Event::State(ConnectionState::Disconnected {
        reason: tomori_client::Disconnect::Closed,
        retry_in: Duration::from_secs(1),
    }));
    conversation.apply(Event::State(ConnectionState::Connecting { attempt: 2 }));
    assert_eq!(conversation.mode(), Mode::Space);
    conversation.apply(connected());
    // The client restores membership itself; nothing is asked of the server.
    assert!(conversation.take_actions().is_empty());
    conversation.apply(space(|state| {
        state.messages = vec![
            message("m1", Role::Assistant, "Hello"),
            message("m2", Role::Assistant, "Welcome back"),
        ];
    }));
    assert_eq!(conversation.activity(), SessionActivity::Ready);
    assert!(conversation.text().contains("Welcome back"));
}

#[test]
fn a_space_lost_while_reconnecting_returns_to_the_lobby() {
    let mut conversation = joined(|_| {});
    conversation.apply(Event::State(ConnectionState::Disconnected {
        reason: tomori_client::Disconnect::Closed,
        retry_in: Duration::from_secs(1),
    }));
    conversation.apply(connected());
    conversation.apply(refused(SPACE));
    assert_eq!(conversation.mode(), Mode::Lobby);
    // Leaving keeps the client from asking for the space again on the next reconnect; the server lists spaces after it.
    assert_eq!(sent(&mut conversation), [ClientMessage::Leave]);
    conversation.apply(Event::Message(ServerMessage::LEFT));
    assert_eq!(conversation.mode(), Mode::Lobby);
    assert!(conversation.take_actions().is_empty());
    // The lobby says the space has ended, rather than showing the server's error, and B dismisses it before it leaves the server.
    let text = conversation.text();
    assert!(text.contains("Tomori has ended."), "{text}");
    assert!(!text.contains("ERROR"), "{text}");
    assert!(text.contains("B dismiss"), "{text}");
    conversation.back();
    assert_eq!(conversation.mode(), Mode::Lobby);
    assert!(conversation.take_actions().is_empty());
    let text = conversation.text();
    assert!(
        !text.contains("has ended") && text.contains("B servers"),
        "{text}"
    );
    conversation.back();
    assert_eq!(conversation.mode(), Mode::Servers);
}

#[test]
fn an_ended_space_hidden_by_the_connection_status_is_not_offered_for_dismissal() {
    let mut conversation = joined(|_| {});
    conversation.apply(connected());
    conversation.apply(refused(SPACE));
    conversation.apply(Event::Message(ServerMessage::LEFT));
    assert!(conversation.text().contains("B dismiss"));
    conversation.apply(Event::State(ConnectionState::Connecting { attempt: 2 }));
    let text = conversation.text();
    assert!(
        !text.contains("has ended") && text.contains("B servers"),
        "{text}"
    );
}

#[test]
fn another_error_while_rejoining_stays_in_the_space() {
    let mut conversation = joined(|_| {});
    conversation.apply(Event::State(ConnectionState::Disconnected {
        reason: tomori_client::Disconnect::Closed,
        retry_in: Duration::from_secs(1),
    }));
    conversation.apply(connected());
    // Such as a decision sent before the connection dropped, refused once it was delivered.
    conversation.apply(Event::Message(ServerMessage::Error {
        error: "That request is no longer pending.".into(),
        space: None,
        panel: None,
        wall: None,
    }));
    assert_eq!(conversation.mode(), Mode::Space);
    assert!(conversation.take_actions().is_empty());
    // Nor does a refusal of some other space.
    conversation.apply(refused("space-other"));
    assert_eq!(conversation.mode(), Mode::Space);
    conversation.apply(space(|_| {}));
    assert_eq!(conversation.mode(), Mode::Space);
}

#[test]
fn a_refused_join_from_the_lobby_leaves() {
    crate::test_support::log_everything();
    let mut conversation = crate::test_support::conversation("Ada");
    conversation.connect(Choice::new("Studio", "10.0.0.1:8765", Source::Discovered));
    conversation.apply(connected());
    conversation.apply(Event::Message(ServerMessage::Spaces {
        spaces: vec![SpaceSummary {
            id: SPACE.into(),
            title: "Tomori".into(),
            backend: "opencode".into(),
            directory: "/work/tomori".into(),
            environment: "sky".into(),
            activity: Activity::Idle,
            participants: 0,
            attention: None,
            attention_age: None,
            folder: None,
            active_age: None,
            latest: None,
        }],
    }));
    conversation.take_actions();
    conversation.accept();
    conversation.take_actions();
    conversation.apply(refused(SPACE));
    assert_eq!(conversation.mode(), Mode::Lobby);
    assert_eq!(sent(&mut conversation), [ClientMessage::Leave]);
    // The gone space is no longer offered while the listing that follows the leave is awaited.
    let text = conversation.text();
    assert!(!text.contains("Joining...") && !text.contains("> Tomori"));
    assert!(text.contains("SPACES 0/0"));
    assert!(text.contains("Tomori has ended."), "{text}");
}

#[test]
fn a_refused_decision_in_a_space_stays_in_the_space() {
    let mut conversation = joined(|_| {});
    conversation.apply(Event::Message(ServerMessage::Error {
        error: "That approval was already decided.".into(),
        space: None,
        panel: None,
        wall: None,
    }));
    assert_eq!(conversation.mode(), Mode::Space);
}

#[test]
fn the_server_rejoining_after_a_reconnect_in_the_lobby_enters_the_space() {
    let mut conversation = crate::test_support::conversation("Ada");
    conversation.connect(Choice::new("Studio", "10.0.0.1:8765", Source::Override));
    conversation.apply(connected());
    conversation.apply(space(|_| {}));
    assert_eq!(conversation.mode(), Mode::Space);
}

#[test]
fn updates_sent_before_a_leave_was_handled_are_ignored() {
    let mut conversation = joined(|_| {});
    // Pressing B, or holding it briefly, does not leave.
    conversation.back();
    hold_b_briefly(&mut conversation);
    assert_eq!(conversation.mode(), Mode::Space);
    assert!(conversation.text().contains("Hold B to leave"));
    assert!(sent(&mut conversation).is_empty());
    hold_b(&mut conversation);
    assert_eq!(sent(&mut conversation), [ClientMessage::Leave]);
    assert_eq!(conversation.mode(), Mode::Lobby);
    conversation.apply(space(|_| {}));
    assert_eq!(conversation.mode(), Mode::Lobby);
    conversation.apply(Event::Message(ServerMessage::LEFT));
    // Leaving is followed by a listing, so none is requested.
    assert!(conversation.take_actions().is_empty());
    conversation.apply(space(|_| {}));
    assert_eq!(conversation.mode(), Mode::Space);
}

#[test]
fn being_left_by_the_server_returns_to_the_lobby_and_lists() {
    let mut conversation = joined(|_| {});
    conversation.apply(Event::Message(ServerMessage::LEFT));
    assert_eq!(conversation.mode(), Mode::Lobby);
    assert_eq!(sent(&mut conversation), [ClientMessage::ListSpaces]);
}

#[test]
fn a_selected_queued_entry_is_cancelled_with_a_after_an_undo_time() {
    let mut conversation = joined(|state| {
        state.queue = vec![
            entry("1", "Ada", QueueState::Queued),
            entry("2", "Bea", QueueState::Queued),
            entry("4", "Ada", QueueState::Sending),
        ];
    });
    // B never cancels.
    conversation.back();
    assert!(sent(&mut conversation).is_empty());
    // The right stick selects among the waiting entries, anyone's.
    conversation.select_queued(1);
    conversation.select_queued(1);
    assert!(conversation.text().contains("> Bea queued: text of 2"));
    assert!(conversation.text().ends_with("A cancel entry | B deselect"));
    assert!(!conversation.accept());
    assert!(conversation.text().contains("x Bea cancelling, A undoes"));
    // Nothing is sent until the undo time is up.
    conversation.tick_space(Instant::now());
    assert!(sent(&mut conversation).is_empty());
    conversation.tick_space(Instant::now() + CANCEL_UNDO);
    assert_eq!(
        sent(&mut conversation),
        [ClientMessage::CancelQueued { entry: "2".into() }]
    );

    conversation.apply(space(|state| {
        state.queue = vec![
            entry("1", "Ada", QueueState::Queued),
            entry("4", "Ada", QueueState::Sending),
        ];
    }));
    // A again within the undo time keeps the entry.
    conversation.select_queued(-1);
    assert!(conversation.text().contains("> You queued: text of 1"));
    conversation.accept();
    conversation.accept();
    conversation.tick_space(Instant::now() + CANCEL_UNDO);
    assert!(sent(&mut conversation).is_empty());
    assert!(conversation.text().contains("> You queued: text of 1"));
    // B deselects, and an entry sent meanwhile is no longer cancelled.
    conversation.back();
    assert!(!conversation.text().contains("> You"));
    conversation.select_queued(1);
    conversation.accept();
    conversation.apply(space(|state| {
        state.queue = vec![entry("1", "Ada", QueueState::Sending)];
    }));
    conversation.tick_space(Instant::now() + CANCEL_UNDO);
    assert!(sent(&mut conversation).is_empty());
    assert!(
        conversation
            .text()
            .contains("Not cancelled: already sent or gone.")
    );
}

#[test]
fn another_participant_with_your_name_does_not_own_your_entries() {
    let mut conversation = joined(|state| {
        let mut namesake = entry("1", "Ada", QueueState::Queued);
        namesake.source = 2;
        state.queue = vec![namesake, entry("2", "Ada", QueueState::Queued)];
    });
    let text = conversation.text();
    assert!(text.contains("- Ada queued: text of 1"), "{text}");
    assert!(text.contains("- You queued: text of 2"), "{text}");
}

#[test]
fn your_entries_follow_the_number_the_server_gives_you() {
    let mut conversation = joined(|state| {
        state.queue = vec![entry("1", "Ada", QueueState::Queued)];
    });
    assert!(conversation.text().contains("- You queued"));
    // Still yours while the connection is down, since a reconnect keeps the number.
    conversation.apply(Event::State(ConnectionState::Connecting { attempt: 1 }));
    assert!(conversation.text().contains("- You queued"));
    // A server that numbers you differently, such as after it restarts, does not see the entry as yours.
    conversation.apply(Event::State(ConnectionState::Connected {
        source: 2,
        server: "studio".into(),
        role: ConnectionRole::Owner,
        certificate: None,
        backends: vec![],
        roots: vec![],
        environments: sky(),
        utterance_limit: None,
        name: None,
    }));
    assert!(conversation.text().contains("- Ada queued"));
    // Choosing another server forgets the number until it welcomes you.
    conversation.connect(Choice::new("Other", "10.0.0.2:8765", Source::Discovered));
    assert_eq!(conversation.source, None);
}

#[test]
fn the_queue_is_always_shown_as_its_own_strip() {
    let mut conversation = joined(|state| {
        state.messages = vec![message("m1", Role::User, "Hi")];
        state.queue = vec![
            entry("1", "Bea", QueueState::Sending),
            entry("2", "Ada", QueueState::Queued),
            entry("3", "Cy", QueueState::Delivered),
        ];
    });
    let text = conversation.text();
    assert!(text.contains("USER 1/1"), "{text}");
    assert!(text.contains("- Bea sending: text of 1"), "{text}");
    assert!(text.contains("- You queued: text of 2"), "{text}");
    assert!(!text.contains("text of 3"), "{text}");
    // It is not an item after the last message.
    conversation.navigate(1, 0);
    assert!(conversation.text().contains("USER 1/1"));
    // A longer queue keeps the selected entry in view.
    conversation.apply(space(|state| {
        state.queue = (1..=5)
            .map(|id| entry(&id.to_string(), "Bea", QueueState::Queued))
            .collect();
    }));
    for _ in 0..4 {
        conversation.select_queued(1);
    }
    let text = conversation.text();
    assert!(text.contains("> Bea queued: text of 4"), "{text}");
    assert!(text.contains("5 in the queue"), "{text}");
    assert_fits(&mut conversation);
}

#[test]
fn pages_through_long_messages_and_follows_new_ones_from_the_last_page() {
    let long = (0..30).map(|i| format!("line {i}\n")).collect::<String>();
    let mut conversation = joined(|state| {
        state.messages = vec![
            message("m1", Role::User, &long),
            message("m2", Role::Assistant, &long),
        ];
    });
    // Following shows the newest message's last page.
    let total = pages(&long).len();
    assert!(total > 1);
    assert!(
        conversation
            .text()
            .contains(&format!("p{total}/{total} ASSISTANT 2/2"))
    );
    conversation.navigate(-1, 0);
    assert!(
        conversation
            .text()
            .contains(&format!("p1/{total} USER 1/2"))
    );
    conversation.navigate(0, 1);
    assert!(
        conversation
            .text()
            .contains(&format!("p2/{total} USER 1/2"))
    );

    // While reading an older message, new ones do not move the view.
    conversation.apply(space(|state| {
        state.messages = vec![
            message("m1", Role::User, &long),
            message("m2", Role::Assistant, &long),
            message("m3", Role::Assistant, "New"),
        ];
    }));
    assert!(conversation.text().contains("USER 1/3"));

    // Returning to the newest page follows again.
    conversation.navigate(1, 0);
    conversation.navigate(1, 0);
    assert!(conversation.text().contains("ASSISTANT 3/3"));
    conversation.apply(space(|state| {
        state.messages = vec![
            message("m1", Role::User, &long),
            message("m2", Role::Assistant, &long),
            message("m3", Role::Assistant, "New"),
            message("m4", Role::Assistant, "Newer"),
        ];
    }));
    assert!(conversation.text().contains("ASSISTANT 4/4"));
    assert_fits(&mut conversation);
}

#[test]
fn everything_fits_the_panel() {
    let long = "Very long reply ".repeat(1000);
    let mut conversation = joined(|state| {
        state.title = "A title far too long for one line of the panel".into();
        state.messages = vec![message("m1", Role::Assistant, &long)];
        state.queue = (0..20)
            .map(|i| entry(&i.to_string(), "Ada", QueueState::Queued))
            .collect();
        state.error = Some(long.clone());
    });
    assert_fits(&mut conversation);
    conversation.navigate(1, 0);
    assert_fits(&mut conversation);
    conversation.apply(space(|state| {
        let mut long_approval = approval(&[("a", &long)]);
        long_approval.text = long.clone();
        state.approval = Some(long_approval);
    }));
    conversation.choose(1);
    assert_fits(&mut conversation);
    conversation.apply(space(|state| state.question = Some(question())));
    assert!(
        conversation
            .text()
            .contains("Answer this question on the PC.")
    );
    assert_fits(&mut conversation);

    let mut conversation = crate::test_support::conversation("Ada");
    conversation.set_servers(
        (0..20)
            .map(|i| Choice::new(long.clone(), format!("10.0.0.{i}:8765"), Source::Discovered))
            .collect(),
    );
    conversation.navigate(0, 15);
    assert!(conversation.text().contains("SERVERS 16/20"));
    assert!(conversation.text().contains("> "));
    assert_fits(&mut conversation);
    conversation.accept();
    conversation.fail(long.clone());
    assert_fits(&mut conversation);
    conversation.apply(Event::Message(ServerMessage::Spaces {
        spaces: (0..20)
            .map(|i| SpaceSummary {
                id: i.to_string(),
                title: long.clone(),
                backend: "opencode".into(),
                directory: "/".into(),
                environment: "sky".into(),
                activity: Activity::Unknown,
                participants: 0,
                attention: None,
                attention_age: None,
                folder: None,
                active_age: None,
                latest: None,
            })
            .collect(),
    }));
    conversation.navigate(0, 19);
    assert_fits(&mut conversation);

    let paged = pages(&(0..200).map(|i| format!("line {i}\n")).collect::<String>());
    assert!(paged.last().unwrap().last().unwrap().contains("199"));
    assert!(short(&"🦀".repeat(200)).chars().count() <= COLUMNS);
}

fn modes(current: &str) -> impl FnOnce(&mut SpaceState) {
    let current = current.to_owned();
    move |state| {
        state.mode = Some(current);
        state.modes = [
            ("default", "Manual"),
            ("acceptEdits", "Accept edits"),
            ("plan", "Plan"),
            ("auto", "Auto"),
        ]
        .map(|(id, name)| tomori_protocol::Mode {
            id: id.into(),
            name: name.into(),
            description: (id == "auto").then(|| "Decides for itself.".into()),
        })
        .into();
    }
}

#[test]
fn y_opens_a_list_of_the_agents_modes_and_a_chooses_one() {
    let mut conversation = joined(modes("auto"));
    assert!(
        conversation
            .text()
            .lines()
            .next()
            .unwrap()
            .starts_with("[idle | Auto] Tomori")
    );
    assert!(conversation.text().ends_with("Y modes | hold B leave"));
    assert_fits(&mut conversation);
    // The list opens on the current mode, with each mode's description.
    conversation.refresh();
    let text = conversation.text();
    assert!(text.contains("> Mode: Auto (now)"), "{text}");
    assert!(text.contains("Decides for itself."), "{text}");
    assert_fits(&mut conversation);
    // Choosing a mode directly sends it, without stepping through the others.
    conversation.navigate(0, -1);
    conversation.navigate(0, -1);
    assert!(conversation.text().contains("> Mode: Accept edits"));
    conversation.accept();
    assert_eq!(
        sent(&mut conversation),
        [ClientMessage::SetMode {
            mode: "acceptEdits".into()
        }]
    );
    assert!(!conversation.text().contains("Mode: Plan"));
    // Y or B closes it without choosing.
    conversation.refresh();
    conversation.refresh();
    conversation.refresh();
    conversation.back();
    assert!(sent(&mut conversation).is_empty());
    assert!(!conversation.text().contains("Mode: Plan"));
}

#[test]
fn y_fetches_the_space_again_when_there_is_nothing_to_choose() {
    let mut conversation = joined(|_| {});
    assert!(conversation.text().starts_with("[idle] Tomori"));
    assert!(conversation.text().ends_with("Y refresh | hold B leave"));
    conversation.refresh();
    assert_eq!(
        sent(&mut conversation),
        [ClientMessage::Join {
            space: SPACE.into()
        }]
    );
}

#[test]
fn the_list_switches_the_spaces_speech_between_brief_and_full() {
    let mut conversation = joined(|state| state.speech = SpeechSetting::Brief);
    assert!(conversation.text().contains("| Brief |"));
    conversation.refresh();
    assert!(conversation.text().contains("> Speech: brief"));
    conversation.accept();
    assert_eq!(
        sent(&mut conversation),
        [ClientMessage::SetSpeech {
            speech: SpeechSetting::Full
        }]
    );
    conversation.apply(space(|state| state.speech = SpeechSetting::Full));
    conversation.refresh();
    conversation.accept();
    assert_eq!(
        sent(&mut conversation),
        [ClientMessage::SetSpeech {
            speech: SpeechSetting::Brief
        }]
    );
}

#[test]
fn b_in_the_lobby_returns_to_the_server_list() {
    let mut conversation = crate::test_support::conversation("Ada");
    conversation.connect(Choice::new("Studio", "10.0.0.1:8765", Source::Discovered));
    conversation.take_actions();
    conversation.back();
    assert_eq!(conversation.take_actions(), [Action::Disconnect]);
    assert!(conversation.is_browsing());
    assert_eq!(conversation.server(), None);
    conversation.refresh();
    assert_eq!(conversation.take_actions(), [Action::Rescan]);
}

fn transcript(utterance: &str, text: &str, finished: bool) -> Event {
    Event::Message(ServerMessage::Transcript {
        utterance: utterance.into(),
        text: text.into(),
        finished,
    })
}

/// Press X and return the utterance it began.
fn press(conversation: &mut Conversation) -> String {
    conversation.talk(true);
    let actions = conversation.take_actions();
    let [Action::BeginUtterance(utterance)] = actions.as_slice() else {
        unreachable!("recording begins")
    };
    utterance.id.clone()
}

#[test]
fn holding_x_records_an_utterance_and_releasing_sends_it() {
    let mut conversation = joined(|_| {});
    assert!(conversation.text().contains("Mic off"));
    assert!(conversation.text().contains("X talk"));
    let utterance = press(&mut conversation);
    assert!(conversation.recording().is_some());
    assert!(conversation.text().contains("MIC AGENT: listening..."));
    assert!(conversation.text().contains("Release X to send"));
    // Held: no second utterance.
    conversation.talk(true);
    assert!(conversation.take_actions().is_empty());

    conversation.apply(transcript(&utterance, "hello the", false));
    assert!(conversation.text().contains("MIC AGENT: hello the"));
    conversation.talk(false);
    assert_eq!(conversation.take_actions(), [Action::EndUtterance]);
    assert_eq!(conversation.recording(), None);
    assert!(conversation.text().contains("Sending: hello the..."));
    conversation.apply(transcript(&utterance, "hello there", false));
    assert!(conversation.text().contains("Sending: hello there..."));
    // The status line says what the server says it is doing with them.
    for (stage, said) in [
        (TranscriptStage::Waiting, "Waiting: hello there..."),
        (
            TranscriptStage::Transcribing,
            "Transcribing: hello there...",
        ),
    ] {
        conversation.apply(Event::Message(ServerMessage::TranscriptProgress {
            utterance: utterance.clone(),
            stage,
            part: None,
        }));
        assert!(conversation.text().contains(said));
    }
    conversation.apply(transcript(&utterance, "Hello there.", true));
    assert!(conversation.text().contains("Mic off"));
    // Releasing again does nothing.
    conversation.talk(false);
    assert!(conversation.take_actions().is_empty());
}

#[test]
fn the_worker_pointed_at_as_x_is_pressed_is_sent_as_the_words_begin() {
    let worker = |id: &str, title: &str| tomori_protocol::Worker {
        id: id.into(),
        parent: None,
        title: title.into(),
        state: tomori_protocol::WorkerState::Running,
    };
    let mut conversation = joined(|space| {
        space.workers = vec![
            worker("ses_a", "Review commit 2657a52"),
            worker("ses_b", "Run the tests"),
        ]
    });
    let attending = |conversation: &mut Conversation| {
        conversation.talk(true);
        let actions = conversation.take_actions();
        let [Action::BeginUtterance(utterance)] = actions.as_slice() else {
            unreachable!("recording begins")
        };
        utterance.attention.clone()
    };
    let resource = tomori_protocol::Resource::worker;

    // Pointed at as X is pressed, then elsewhere and away before letting go: the worker it began with is kept,
    // and the words end naming nothing.
    conversation.point_at(Some("ses_a"));
    assert_eq!(attending(&mut conversation), [resource("ses_a")]);
    assert!(
        conversation
            .text()
            .contains("MIC AGENT about Review commit 2657a52")
    );
    assert_fits(&mut conversation);
    conversation.point_at(Some("ses_b"));
    conversation.point_at(None);
    assert!(
        conversation
            .text()
            .contains("MIC AGENT about Review commit 2657a52")
    );
    conversation.talk(false);
    assert_eq!(conversation.take_actions(), [Action::EndUtterance]);

    // Pointed at only while talking: nothing is sent.
    assert!(attending(&mut conversation).is_empty());
    assert!(conversation.text().contains("MIC AGENT: listening..."));
    conversation.point_at(Some("ses_b"));
    conversation.talk(false);
    assert_eq!(conversation.take_actions(), [Action::EndUtterance]);

    // Pointing that ended before X was pressed, or at a worker the space does not have, sends nothing.
    conversation.point_at(Some("ses_a"));
    conversation.point_at(None);
    assert!(attending(&mut conversation).is_empty());
    conversation.talk(false);
    conversation.take_actions();
    conversation.point_at(Some("ses_gone"));
    assert!(attending(&mut conversation).is_empty());
}

/// The cards of [`SPACE`]: issue 97, and pull request 164.
fn cards(space: &str) -> Event {
    let card = |number: u64, title: &str| tomori_protocol::Card {
        id: format!("github://o/r/issues/{number}"),
        number,
        kind: tomori_protocol::CardKind::Issue,
        title: title.into(),
        author: "ada".into(),
        labels: Vec::new(),
        checks: tomori_protocol::Checks::None,
        age: 0,
        placement: None,
    };
    Event::Message(ServerMessage::Cards {
        cards: Box::new(tomori_protocol::Cards {
            space: space.into(),
            source: "o/r".into(),
            cards: vec![card(97, "Physics for cards"), card(164, "Publish")],
            status: None,
        }),
    })
}

#[test]
fn a_space_that_shows_cards_is_watched_once_for_each_space_and_connection() {
    let mut conversation = joined(|_| {});
    assert_eq!(conversation.cards(), None);
    // The space starts showing cards: they are asked for once.
    conversation.apply(space(|space| space.cards = true));
    assert_eq!(sent(&mut conversation), [ClientMessage::WatchCards]);
    conversation.apply(space(|space| space.cards = true));
    assert!(sent(&mut conversation).is_empty());
    // Another space's cards, as from a space just left, are not kept.
    conversation.apply(cards("space-2"));
    assert_eq!(conversation.cards(), None);
    conversation.apply(cards(SPACE));
    assert_eq!(conversation.cards().unwrap().cards.len(), 2);
    // A new connection watches again.
    conversation.apply(Event::State(ConnectionState::Closed));
    conversation.apply(connected());
    conversation.take_actions();
    conversation.apply(space(|space| space.cards = true));
    assert_eq!(sent(&mut conversation), [ClientMessage::WatchCards]);
    // The space stops showing cards, as when an owner changes its environment: they are forgotten, and watched
    // again when it shows them again.
    conversation.apply(space(|_| {}));
    assert_eq!(conversation.cards(), None);
    conversation.apply(space(|space| space.cards = true));
    assert_eq!(sent(&mut conversation), [ClientMessage::WatchCards]);
    conversation.apply(cards(SPACE));
    // Leaving forgets them.
    conversation.apply(Event::Message(ServerMessage::LEFT));
    assert_eq!(conversation.cards(), None);
}

#[test]
fn a_visitor_watches_no_cards_until_let_in() {
    let mut conversation = joined(|_| {});
    conversation.apply(Event::Message(ServerMessage::Role {
        role: ConnectionRole::Visitor,
    }));
    conversation.take_actions();
    conversation.apply(space(|space| space.cards = true));
    assert!(sent(&mut conversation).is_empty());
    // Nor can it place one, should it have any.
    conversation.place_card("github://o/r/issues/97".into(), None);
    assert!(sent(&mut conversation).is_empty());
    conversation.apply(Event::Message(ServerMessage::Role {
        role: ConnectionRole::Collaborator,
    }));
    assert!(sent(&mut conversation).contains(&ClientMessage::WatchCards));
}

#[test]
fn cards_are_placed_and_the_card_pointed_at_as_x_is_pressed_is_sent_as_the_words_begin() {
    let mut conversation = joined(|space| space.cards = true);
    // Without cards there is nothing to place.
    conversation.place_card("github://o/r/issues/97".into(), None);
    assert!(sent(&mut conversation).is_empty());
    conversation.apply(cards(SPACE));
    let pose = tomori_protocol::Pose {
        position: [0.0, 1.0, -0.5],
        orientation: [0.0, 0.0, 0.0, 1.0],
    };
    conversation.place_card("github://o/r/issues/97".into(), Some(pose));
    assert_eq!(
        sent(&mut conversation),
        [ClientMessage::PlaceCard {
            card: "github://o/r/issues/97".into(),
            placement: Some(pose),
        }]
    );
    let card =
        |number: u64| tomori_protocol::Resource::card(format!("github://o/r/issues/{number}"));
    let attending = |conversation: &mut Conversation| {
        conversation.talk(true);
        let actions = conversation.take_actions();
        let [Action::BeginUtterance(utterance)] = actions.as_slice() else {
            unreachable!("recording begins")
        };
        utterance.attention.clone()
    };
    // The card pointed at as X is pressed is what the words begin attending to.
    conversation.point(Some(card(97)));
    assert_eq!(attending(&mut conversation), [card(97)]);
    assert!(
        conversation
            .text()
            .contains("MIC AGENT about #97 Physics for cards")
    );
    assert!(matches!(
        conversation.recording_phase(Instant::now()),
        Some(crate::recording::Phase::Recording { about: Some(about), .. }) if about == "#97 Physics for cards"
    ));
    // Pointing elsewhere while talking changes nothing, and the words end naming nothing.
    conversation.point(Some(card(164)));
    conversation.point(None);
    assert_eq!(conversation.pointing_at(), None);
    assert!(
        conversation
            .text()
            .contains("MIC AGENT about #97 Physics for cards")
    );
    conversation.talk(false);
    assert_eq!(conversation.take_actions(), [Action::EndUtterance]);
    // A card the space lacks, or a kind this client does not show, is not attended to.
    for target in [card(1), tomori_protocol::Resource::Unknown] {
        conversation.point(Some(target));
        assert!(attending(&mut conversation).is_empty());
        assert_eq!(conversation.pointing_at(), None);
        conversation.talk(false);
        conversation.take_actions();
    }
}

#[test]
fn live_toggle_stays_visible_and_x_returns_to_live() {
    let mut conversation = joined(|_| {});
    conversation.toggle_live();
    assert_eq!(conversation.take_actions(), [Action::StartLive(0)]);
    assert!(conversation.text().contains("MIC LIVE"));
    let utterance = press(&mut conversation);
    assert!(conversation.text().contains("MIC AGENT"));
    conversation.talk(false);
    assert_eq!(conversation.take_actions(), [Action::EndUtterance]);
    assert!(conversation.text().contains("MIC LIVE"));
    conversation.apply(transcript(&utterance, "hello", true));
    assert!(conversation.text().contains("MIC LIVE"));
    conversation.toggle_live();
    assert_eq!(conversation.take_actions(), [Action::StopLive]);
    assert!(conversation.text().contains("Mic off"));
}

#[test]
fn live_stops_on_focus_loss_disconnect_and_microphone_failure() {
    let mut conversation = joined(|_| {});
    conversation.toggle_live();
    conversation.take_actions();
    conversation.stop_live();
    assert_eq!(conversation.take_actions(), [Action::StopLive]);
    assert!(!conversation.live);
    conversation.toggle_live();
    conversation.take_actions();
    conversation.microphone_failed("no microphone");
    assert_eq!(conversation.take_actions(), [Action::StopLive]);
    assert!(!conversation.live);
    conversation.toggle_live();
    conversation.take_actions();
    conversation.apply(Event::State(ConnectionState::Closed));
    assert!(conversation.take_actions().contains(&Action::StopLive));
    assert!(!conversation.live);
}

#[test]
fn holding_b_says_when_it_left_once_per_hold() {
    let mut conversation = joined(|_| {});
    assert!(!conversation.hold_back(Some(Duration::ZERO)));
    assert!(!conversation.hold_back(Some(LEAVE_HOLD / 2)));
    assert!(conversation.hold_back(Some(LEAVE_HOLD)));
    assert!(!conversation.hold_back(Some(LEAVE_HOLD * 2)));
    assert!(!conversation.hold_back(None));
}

#[test]
fn a_press_with_nothing_to_do_or_that_acts_is_not_a_refusal() {
    let mut conversation = crate::test_support::conversation("Ada");
    conversation.press_a(None);
    assert!(!conversation.a_refused());
    let mut joined = joined(|space| space.approval = Some(approval(&[])));
    joined.request_at = Some(Instant::now());
    assert!(!joined.press_a(None));
    assert!(joined.a_refused());
    // The next press after the grace acts, and the refusal is forgotten.
    settle(&mut joined);
    assert!(!joined.press_a(None));
    assert!(!joined.a_refused());
}

#[test]
fn the_sound_section_steps_and_keeps_the_feedback_setting() {
    use feedback::{Level, Settings};
    // Each change is logged, sounds, vibration and mono or stereo, which this test evaluates itself rather than relying on another test in the binary to have turned logging on first.
    crate::test_support::log_everything();
    let mut conversation = crate::test_support::conversation("Ada");
    let kept = Settings {
        sounds: Level::Off,
        vibration: Level::Reduced,
        mono: false,
        ambience: feedback::Ambience::Low,
    };
    conversation.set_feedback(kept);
    assert_eq!(conversation.menu_model().feedback, kept);
    assert!(conversation.take_actions().is_empty());
    conversation.choose_menu(crate::hand_menu::Choice::Sounds);
    conversation.choose_menu(crate::hand_menu::Choice::Vibration);
    let full = Settings {
        sounds: Level::Full,
        ..kept
    };
    let stepped = Settings {
        vibration: Level::Off,
        ..full
    };
    assert_eq!(
        conversation.take_actions(),
        [Action::SetFeedback(full), Action::SetFeedback(stepped)]
    );
    assert_eq!(conversation.menu_model().feedback, stepped);
    // Mono audio turns on and off again, and is kept each time.
    conversation.choose_menu(crate::hand_menu::Choice::Mono);
    conversation.choose_menu(crate::hand_menu::Choice::Mono);
    let mono = Settings {
        mono: true,
        ..stepped
    };
    assert_eq!(
        conversation.take_actions(),
        [Action::SetFeedback(mono), Action::SetFeedback(stepped)]
    );
    // The ambience steps up from low, and is kept each time.
    conversation.choose_menu(crate::hand_menu::Choice::Ambience);
    let medium = Settings {
        ambience: feedback::Ambience::Medium,
        ..stepped
    };
    assert_eq!(conversation.take_actions(), [Action::SetFeedback(medium)]);
    assert_eq!(
        conversation.menu_model().feedback.ambience,
        feedback::Ambience::Medium
    );
}

#[test]
fn engagement_is_reported_once_per_change_and_connection_to_a_server_that_retains_speech() {
    // An older server, which reports no speech setting, is never sent it.
    let mut older = joined(|_| {});
    older.set_engagement(Engagement::Away);
    assert!(sent(&mut older).is_empty());

    // Only engagement reports matter here; the server putting the client in a space also has it join.
    let reports = |conversation: &mut Conversation| {
        sent(conversation)
            .into_iter()
            .filter(|message| matches!(message, ClientMessage::Engagement { .. }))
            .collect::<Vec<_>>()
    };
    let mut conversation = crate::test_support::conversation("Ada");
    conversation.connect(Choice::new("Studio", "10.0.0.1:8765", Source::Discovered));
    conversation.apply(connected());
    conversation.take_actions();
    // Nothing is reported in the lobby, then the engagement is reported on entering the space.
    conversation.set_engagement(Engagement::Engaged);
    assert!(reports(&mut conversation).is_empty());
    conversation.apply(space(|state| state.speech = SpeechSetting::Brief));
    let engaged = ClientMessage::Engagement {
        state: Engagement::Engaged,
    };
    assert_eq!(reports(&mut conversation), std::slice::from_ref(&engaged));
    conversation.apply(space(|state| state.speech = SpeechSetting::Brief));
    assert!(reports(&mut conversation).is_empty());
    conversation.set_engagement(Engagement::Distracted);
    conversation.set_engagement(Engagement::Engaged);
    assert_eq!(
        reports(&mut conversation),
        [
            ClientMessage::Engagement {
                state: Engagement::Distracted
            },
            engaged.clone()
        ]
    );
    // A new connection reports again, since the server forgets it with the old one.
    conversation.apply(connected());
    conversation.apply(space(|state| state.speech = SpeechSetting::Brief));
    assert_eq!(reports(&mut conversation), [engaged]);

    // A visitor sends no engagement, until admitted to act in the space.
    let mut visitor = crate::test_support::conversation("Ada");
    visitor.connect(Choice::new("Studio", "10.0.0.1:8765", Source::Discovered));
    visitor.apply(connected());
    visitor.apply(Event::Message(ServerMessage::Role {
        role: ConnectionRole::Visitor,
    }));
    visitor.apply(space(|state| state.speech = SpeechSetting::Brief));
    visitor.replay();
    let engagement = |messages: Vec<ClientMessage>| {
        messages
            .into_iter()
            .filter(|message| {
                matches!(
                    message,
                    ClientMessage::Engagement { .. } | ClientMessage::Replay { .. }
                )
            })
            .collect::<Vec<_>>()
    };
    assert!(engagement(sent(&mut visitor)).is_empty());
    visitor.apply(Event::Message(ServerMessage::Role {
        role: ConnectionRole::Collaborator,
    }));
    assert_eq!(
        engagement(sent(&mut visitor)),
        [ClientMessage::Engagement {
            state: Engagement::Engaged
        }]
    );

    // The spectator never reports, so it follows live speech.
    let mut spectator = crate::test_support::conversation("Spectator");
    spectator.follow_live_speech_only();
    spectator.connect(Choice::new("Studio", "10.0.0.1:8765", Source::Discovered));
    spectator.apply(connected());
    spectator.apply(space(|state| state.speech = SpeechSetting::Brief));
    assert!(
        !sent(&mut spectator)
            .iter()
            .any(|message| matches!(message, ClientMessage::Engagement { .. }))
    );
}

#[test]
fn taking_the_headset_off_turns_the_live_microphone_off_and_abandons_a_recording() {
    let mut conversation = joined(|state| state.speech = SpeechSetting::Brief);
    conversation.toggle_live();
    conversation.talk(true);
    conversation.take_actions();
    conversation.set_engagement(Engagement::Away);
    let actions = conversation.take_actions();
    assert!(actions.contains(&Action::StopLive), "{actions:?}");
    assert!(actions.contains(&Action::CancelUtterance), "{actions:?}");
    assert!(actions.contains(&Action::Send(ClientMessage::Engagement {
        state: Engagement::Away
    })));
    assert!(!conversation.live);
    assert_eq!(conversation.recording(), None);
    // Putting it back on does not reopen the microphone.
    conversation.set_engagement(Engagement::Engaged);
    assert!(
        !conversation
            .take_actions()
            .iter()
            .any(|action| matches!(action, Action::StartLive(_)))
    );
    assert!(!conversation.live);
}

#[test]
fn the_stick_clicks_mute_spoken_replies_and_play_the_latest_again() {
    crate::test_support::log_everything();
    let mut conversation = joined(|state| state.speech = SpeechSetting::Brief);
    conversation.toggle_mute();
    assert_eq!(conversation.take_actions(), [Action::SetAgentMuted(true)]);
    assert!(conversation.text().contains("Muted"));
    // Asking to hear the reply again unmutes, then asks the server to play it to this participant.
    conversation.replay();
    assert_eq!(
        conversation.take_actions(),
        [
            Action::SetAgentMuted(false),
            Action::Send(ClientMessage::Replay { message: None })
        ]
    );
    // While it plays, the same click stops it.
    conversation.set_replaying(true);
    assert!(conversation.text().contains("Replaying"));
    conversation.replay();
    assert_eq!(sent(&mut conversation), [ClientMessage::StopReplay]);
    conversation.set_replaying(false);

    // An older server cannot play replies again, so nothing is sent.
    let mut older = joined(|_| {});
    older.replay();
    assert!(sent(&mut older).is_empty());
    assert!(older.text().contains("cannot play replies again"));
    // Muting works with any server, as it is local.
    older.toggle_mute();
    assert_eq!(older.take_actions(), [Action::SetAgentMuted(true)]);
}

#[test]
fn participants_who_are_away_are_counted_apart() {
    let mut conversation = joined(|state| {
        state.participants.push(Participant {
            name: "Bob".into(),
            source: 2,
            engagement: Some(Engagement::Away),
        });
    });
    assert!(conversation.text().contains("1 here, 1 away"));
}

#[test]
fn focus_loss_cancels_queued_microphone_starts() {
    let mut conversation = joined(|_| {});
    conversation.toggle_live();
    conversation.talk(true);
    conversation.stop_live();
    conversation.stop_talking();
    assert!(conversation.take_actions().is_empty());
    assert!(!conversation.live);
    assert!(conversation.recording().is_none());
}

#[test]
fn failed_live_start_removes_a_later_queued_agent_start() {
    let mut conversation = joined(|_| {});
    conversation.toggle_live();
    conversation.talk(true);
    let Some(Action::StartLive(_)) = conversation.next_action() else {
        unreachable!("Some")
    };
    conversation.microphone_failed("capture failed");
    assert_eq!(conversation.next_action(), Some(Action::StopLive));
    assert_eq!(conversation.next_action(), None);
    assert!(conversation.recording().is_none());
}

#[test]
fn live_mute_precedes_a_queued_agent_end() {
    let mut conversation = joined(|_| {});
    conversation.toggle_live();
    conversation.take_actions();
    press(&mut conversation);
    conversation.talk(false);
    conversation.stop_live();
    assert_eq!(
        conversation.take_actions(),
        [Action::StopLive, Action::EndUtterance]
    );
}

#[test]
fn queued_microphone_actions_keep_their_original_connection_generation() {
    let mut conversation = joined(|_| {});
    conversation.set_capture_generation(7);
    conversation.toggle_live();
    conversation.set_capture_generation(8);
    assert_eq!(conversation.take_actions(), [Action::StartLive(7)]);
    conversation.talk(true);
    conversation.set_capture_generation(9);
    let Some(Action::BeginUtterance(Utterance { generation: 8, .. })) = conversation.next_action()
    else {
        unreachable!("Some")
    };
}

#[test]
fn capture_failure_before_a_queued_end_cancels_instead() {
    let mut conversation = joined(|_| {});
    conversation.toggle_live();
    conversation.take_actions();
    press(&mut conversation);
    conversation.talk(false);
    conversation.microphone_failed("capture failed");
    assert_eq!(
        conversation.take_actions(),
        [Action::StopLive, Action::CancelUtterance]
    );
    assert!(conversation.text().contains("capture failed"));
    assert!(!conversation.live);
}

#[test]
fn a_live_failure_after_an_utterance_was_ended_keeps_its_pending_transcript() {
    let mut conversation = joined(|_| {});
    conversation.toggle_live();
    conversation.take_actions();
    press(&mut conversation);
    conversation.talk(false);
    // The end has been performed: the utterance is with the server.
    conversation.take_actions();
    let Some(Talk::Sending { .. }) = conversation.talk else {
        unreachable!("Some")
    };
    // The live capture reopened afterwards fails.
    conversation.microphone_failed("capture failed");
    let Some(Talk::Sending { .. }) = conversation.talk else {
        unreachable!("Some")
    };
    assert_eq!(conversation.take_actions(), [Action::StopLive]);
    assert!(!conversation.live);
    // The panel no longer claims a live microphone, and the error is kept
    // to show once the pending transcript is resolved.
    assert!(!conversation.text().contains("MIC LIVE"));
    assert_eq!(conversation.error.as_deref(), Some("capture failed"));
    let utterance = conversation.talk.as_ref().unwrap().utterance().to_owned();
    conversation.apply(transcript(&utterance, "hello", true));
    assert!(conversation.talk.is_none());
    assert!(conversation.text().contains("capture failed"));
}

#[test]
fn an_utterance_whose_end_failed_is_not_left_sending() {
    let mut conversation = joined(|_| {});
    conversation.toggle_live();
    conversation.take_actions();
    press(&mut conversation);
    conversation.talk(false);
    // The end was taken to be performed, and failed while stopping.
    assert_eq!(conversation.take_actions(), [Action::EndUtterance]);
    conversation.utterance_not_sent("Microphone lost audio; not sent.");
    assert!(conversation.talk.is_none());
    assert_eq!(conversation.take_actions(), [Action::StopLive]);
    assert!(
        conversation
            .text()
            .contains("Microphone lost audio; not sent.")
    );
}

#[test]
fn a_failed_end_also_withdraws_a_later_utterance_queued_behind_it() {
    let mut conversation = joined(|_| {});
    conversation.toggle_live();
    conversation.take_actions();
    press(&mut conversation);
    conversation.talk(false);
    // A second press and release land before the first end is performed.
    conversation.talk(true);
    conversation.talk(false);
    let Some(Action::EndUtterance) = conversation.next_action() else {
        unreachable!("Some")
    };
    conversation.utterance_not_sent("Microphone lost audio; not sent.");
    assert!(conversation.talk.is_none());
    // The second utterance never began, so nothing of it is performed.
    assert_eq!(conversation.take_actions(), [Action::StopLive]);
}

#[test]
fn capture_failure_discards_an_unstarted_utterance_and_its_end() {
    let mut conversation = joined(|_| {});
    conversation.toggle_live();
    conversation.take_actions();
    conversation.talk(true);
    conversation.talk(false);
    conversation.microphone_failed("capture failed");
    assert_eq!(conversation.take_actions(), [Action::StopLive]);
    assert!(conversation.text().contains("capture failed"));
}

#[test]
fn utterances_are_unique_within_and_across_launches() {
    let mut conversation = joined(|_| {});
    let first = press(&mut conversation);
    conversation.talk(false);
    conversation.take_actions();
    let second = press(&mut conversation);
    assert_ne!(first, second);
    std::thread::sleep(Duration::from_millis(1));
    let mut relaunched = joined(|_| {});
    assert_ne!(press(&mut relaunched), first);
}

/// Joined, deciding on the wrist as the headset does.
fn on_wrist(change: impl FnOnce(&mut SpaceState)) -> Conversation {
    let mut conversation = joined(change);
    conversation.decide_on_wrist();
    conversation
}

#[test]
fn an_approval_is_decided_on_the_wrist_card_only_with_a_deliberate_a() {
    let mut conversation = on_wrist(|_| {});
    conversation.apply(space(|space| {
        space.approval = Some(approval(&[("always", "Yes, and don't ask again")]));
    }));
    // A away from the wrist decides nothing, and says where to decide, which is a refusal rather than a choice.
    assert!(!conversation.press_a(None));
    assert!(conversation.a_refused());
    assert!(sent(&mut conversation).is_empty());
    assert!(
        conversation
            .text()
            .contains("Turn your right wrist to decide")
    );
    // Opening the wrist focuses the card's title and gives A its grace again.
    conversation.wrist_opened();
    assert!(!conversation.wrist_accept());
    assert!(sent(&mut conversation).is_empty());
    conversation.wrist_step(1);
    // Refused within the grace, which is not a choice to feel.
    assert!(!conversation.wrist_accept());
    assert!(sent(&mut conversation).is_empty(), "within the grace");
    assert_eq!(
        conversation.notice.as_deref(),
        Some("Read the request before choosing.")
    );
    settle(&mut conversation);
    // B backs out without deciding, and never leaves.
    assert!(conversation.wrist_back());
    assert!(!conversation.wrist_back());
    assert!(sent(&mut conversation).is_empty());
    // Down to the title, past allowing once to the standing option, then A grants it.
    conversation.wrist_step(1);
    conversation.wrist_step(1);
    conversation.wrist_step(1);
    let Some(crate::transcript::Focus::Card(_)) = conversation.wrist_focus() else {
        unreachable!("Some")
    };
    assert!(conversation.wrist_accept());
    assert_eq!(
        sent(&mut conversation),
        [ClientMessage::Decide {
            request: "request-1".into(),
            decision: Decision::Grant,
            option: Some("always".into()),
        }]
    );
    // A second press sends nothing more.
    conversation.wrist_accept();
    assert!(sent(&mut conversation).is_empty());
}

#[test]
fn deny_is_the_last_button_on_the_card() {
    let mut conversation = on_wrist(|space| {
        space.approval = Some(approval(&[]));
    });
    conversation.wrist_opened();
    settle(&mut conversation);
    // Past allowing once.
    for _ in 0..2 {
        conversation.wrist_step(1);
    }
    let rows: Vec<String> = conversation
        .transcript(crate::transcript::WRIST_COLUMNS, false)
        .iter()
        .map(|row| row.plain().trim_end().to_owned())
        .collect();
    assert!(rows.contains(&"▸ Deny".to_owned()), "{rows:#?}");
    conversation.wrist_accept();
    assert_eq!(
        sent(&mut conversation),
        [ClientMessage::Decide {
            request: "request-1".into(),
            decision: Decision::Deny,
            option: None,
        }]
    );
}

#[test]
fn a_detached_transcript_is_moved_through_at_its_own_width() {
    // Ten rows at the wrist's width, six once detached and wider.
    let reply = "word ".repeat(46);
    let mut conversation = on_wrist(|space| {
        space.messages = vec![message("m1", Role::Assistant, &reply)];
    });
    conversation.transcript(crate::transcript::DETACHED_COLUMNS, false);
    conversation.wrist_step(-1);
    conversation.wrist_accept();
    assert!(conversation.wrist.expanded.is_empty(), "it shows in full");
    conversation.wrist_back();
    conversation.transcript(crate::transcript::WRIST_COLUMNS, false);
    conversation.wrist_step(-1);
    conversation.wrist_accept();
    assert!(conversation.wrist.expanded.contains("m1"));
}

#[test]
fn a_queued_entry_is_removed_on_the_wrist_after_the_undo_time() {
    let mut conversation = on_wrist(|space| {
        space.queue = vec![
            entry("q-1", "Bob", QueueState::Queued),
            entry("q-2", "Ada", QueueState::Queued),
        ];
    });
    // Down from following reaches the queue.
    conversation.wrist_step(1);
    assert!(conversation.wrist_accept());
    assert!(
        sent(&mut conversation).is_empty(),
        "not until the undo time"
    );
    let rows = conversation.transcript(crate::transcript::WRIST_COLUMNS, false);
    assert!(
        rows.iter()
            .any(|row| row.plain().contains("Removing… A undoes"))
    );
    // A again undoes it.
    assert!(conversation.wrist_accept());
    conversation.tick_space(Instant::now() + CANCEL_UNDO);
    assert!(sent(&mut conversation).is_empty());
    // Removed, it is sent once the undo time is over.
    conversation.wrist_accept();
    conversation.tick_space(Instant::now() + CANCEL_UNDO);
    assert_eq!(
        sent(&mut conversation),
        [ClientMessage::CancelQueued {
            entry: "q-1".into()
        }]
    );
    // An entry that started sending meanwhile is not cancelled.
    conversation.apply(space(|space| {
        space.queue = vec![entry("q-2", "Ada", QueueState::Queued)];
    }));
    conversation.wrist_step(1);
    conversation.wrist_accept();
    conversation.apply(space(|space| {
        space.queue = vec![entry("q-2", "Ada", QueueState::Sending)];
    }));
    conversation.tick_space(Instant::now() + CANCEL_UNDO);
    assert!(sent(&mut conversation).is_empty());
    assert_eq!(
        conversation.notice.as_deref(),
        Some("Not cancelled: already sent or gone.")
    );
}

#[test]
fn a_visitor_reads_the_wrist_but_acts_on_nothing() {
    let mut conversation = on_wrist(|space| {
        space.queue = vec![entry("q-1", "Bob", QueueState::Queued)];
        space.approval = Some(approval(&[]));
    });
    conversation.apply(Event::Message(ServerMessage::Role {
        role: ConnectionRole::Visitor,
    }));
    conversation.take_actions();
    conversation.wrist_opened();
    settle(&mut conversation);
    for _ in 0..4 {
        conversation.wrist_step(1);
        conversation.wrist_accept();
    }
    assert!(sent(&mut conversation).is_empty());
    let rows = conversation.transcript(crate::transcript::WRIST_COLUMNS, false);
    assert!(
        rows.iter()
            .any(|row| row.plain().contains("Waiting for a participant"))
    );
}

#[test]
fn the_wrist_cue_shows_a_request_or_a_problem() {
    let mut conversation = on_wrist(|_| {});
    assert_eq!(conversation.wrist_waiting(), None);
    conversation.apply(space(|space| space.error = Some("Lost the agent".into())));
    assert_eq!(conversation.wrist_waiting(), Some(true));
    conversation.apply(space(|space| {
        space.error = Some("Lost the agent".into());
        space.approval = Some(approval(&[]));
    }));
    assert_eq!(conversation.wrist_waiting(), Some(false));
    // B on the wrist lets go of the card's focus, then dismisses the problem, and never decides.
    conversation.apply(space(|space| space.error = Some("Lost the agent".into())));
    assert!(conversation.wrist_back());
    assert_eq!(conversation.wrist_waiting(), Some(true));
    assert!(conversation.wrist_back());
    assert_eq!(conversation.wrist_waiting(), None);
    assert!(sent(&mut conversation).is_empty());
}

#[test]
fn the_status_pill_says_what_the_agent_does_and_what_needs_you_first() {
    use crate::pill::Need;
    let mut conversation = on_wrist(|space| {
        space.activity = Activity::Running;
        space.title = "Fix the parser".into();
    });
    let now = Instant::now();
    let status = conversation.pill_status(now).unwrap();
    assert_eq!(status.activity, Activity::Running);
    assert_eq!(status.title, "Fix the parser");
    assert_eq!(status.need, None);
    conversation.apply(space(|space| {
        space.error = Some("Lost the agent".into());
        space.approval = Some(approval(&[]));
    }));
    // A request first, then a problem, then a notice.
    assert_eq!(
        conversation.pill_status(now).unwrap().need,
        Some(Need::Approval)
    );
    conversation.apply(space(|space| space.error = Some("Lost the agent".into())));
    assert_eq!(
        conversation.pill_status(now).unwrap().need,
        Some(Need::Problem("! 0s Lost the agent".into()))
    );
    // B lets go of the card's focus, then dismisses the problem.
    conversation.wrist_back();
    conversation.wrist_back();
    conversation.notice = Some("Not removed.".into());
    assert_eq!(
        conversation.pill_status(now).unwrap().need,
        Some(Need::Notice("Not removed.".into()))
    );
    // Holding B says that it leaves.
    conversation.hold_back(Some(Duration::from_millis(300)));
    assert_eq!(
        conversation.pill_status(now).unwrap().need,
        Some(Need::Leaving)
    );
    conversation.hold_back(None);
    // The Space section turns it off on this headset.
    conversation.choose_menu(crate::hand_menu::Choice::Pill);
    assert_eq!(conversation.pill_status(now), None);
    assert!(!conversation.menu_model().pill);
    conversation.choose_menu(crate::hand_menu::Choice::Pill);
    assert!(conversation.pill_status(now).is_some());
}

/// A frame at `at`, with agent speech `playing` on this device, as the connection reports it.
fn speech_frame(conversation: &mut Conversation, at: Instant, playing: bool) {
    Session::report(
        conversation,
        Report::Tick {
            now: at,
            browsing: false,
        },
    );
    Session::report(
        conversation,
        Report::AgentSpeech {
            playing,
            level: if playing { 0.3 } else { 0.0 },
        },
    );
}

#[test]
fn the_others_voices_playing_are_kept_from_each_frame_s_report() {
    let mut conversation = crate::test_support::conversation("Ada");
    assert!(conversation.voices().is_empty());
    Session::report(
        &mut conversation,
        Report::Voices([2, 3].into_iter().collect()),
    );
    assert_eq!(conversation.voices(), [2, 3]);
    Session::report(&mut conversation, Report::Voices(Default::default()));
    assert!(conversation.voices().is_empty());
}

/// Agent speech of `message` announced, live.
fn announce(conversation: &mut Conversation, message: &str) {
    conversation.apply(Event::Message(ServerMessage::Speech {
        segment: 1,
        message: message.into(),
        text: "Done.".into(),
        sample_rate: 24_000,
        replay: false,
    }));
}

/// The turn cues `conversation` makes the feedback play, as the connection observes it each frame.
fn turn_cues(conversation: &Conversation, feedback: &mut feedback::Feedback, at: Instant) -> usize {
    Session::observe(conversation, feedback, at);
    feedback
        .take_earcons()
        .filter(|(earcon, _)| *earcon == tomori_client::playback::Earcon::YourTurn)
        .count()
}

#[test]
fn the_pill_says_a_reply_is_waiting_while_it_is_held_and_through_the_pause_then_speaking() {
    use crate::pill::Speaking;
    let mut conversation = on_wrist(|space| space.activity = Activity::Idle);
    let start = Instant::now();
    let ms = Duration::from_millis;
    let first = |conversation: &mut Conversation, at: Instant| {
        crate::pill::rows(&conversation.pill_status(at).unwrap())[0].plain()
    };
    speech_frame(&mut conversation, start, false);
    assert!(first(&mut conversation, start).starts_with("● idle"));
    assert!(!conversation.speech().pending);
    // While they record, the server holds a reply for them: the pill says so, and the swarm prepares to speak.
    conversation.apply(Event::Message(ServerMessage::SpeechHeld { held: true }));
    for at in [start + ms(20), start + ms(5_000)] {
        speech_frame(&mut conversation, at, false);
        let status = conversation.pill_status(at).unwrap();
        assert!(status.waiting);
        assert_eq!(status.speaking, None);
        assert!(first(&mut conversation, at).starts_with("○ reply waiting"));
        assert!(conversation.speech().pending);
    }
    // Through the pause after they let go of X it is still held; once it is not, it waits until it plays.
    let released = start + ms(5_000) + crate::turn::HOLD * 4;
    speech_frame(&mut conversation, released, false);
    conversation.apply(Event::Message(ServerMessage::SpeechHeld { held: false }));
    assert!(first(&mut conversation, released + ms(50)).starts_with("○ reply waiting"));
    // Then it plays, and the pill says speaking.
    announce(&mut conversation, "m1");
    speech_frame(&mut conversation, released + ms(100), true);
    let status = conversation.pill_status(released + ms(100)).unwrap();
    assert!(!status.waiting);
    assert_eq!(status.speaking, Some(Speaking::Heard));
    assert!(first(&mut conversation, released + ms(100)).starts_with("◉ speaking"));
    assert!(!conversation.speech().pending);
    // A space that closes while a reply is held says it is closed, not that a reply waits.
    conversation.apply(Event::Message(ServerMessage::SpeechHeld { held: true }));
    conversation.apply(space(|space| space.activity = Activity::Closed));
    assert!(
        !conversation
            .pill_status(released + ms(200))
            .unwrap()
            .waiting
    );
}

#[test]
fn the_pill_and_swarm_say_speaking_through_a_turn_while_its_speech_is_to_come_and_idle_only_after()
{
    use crate::pill::Speaking;
    let mut conversation = on_wrist(|space| space.activity = Activity::Running);
    let mut feedback = feedback::Feedback::new(feedback::Settings::default());
    let start = Instant::now();
    let ms = Duration::from_millis;
    let label = |conversation: &mut Conversation, at: Instant| {
        let status = conversation.pill_status(at).unwrap();
        crate::pill::rows(&status)[0]
            .plain()
            .split("  ")
            .next()
            .unwrap()
            .to_owned()
    };
    // A sentence queued as the agent writes, before any of it plays: speaking.
    conversation.apply(space(|space| {
        space.activity = Activity::Running;
        space.speech_to_come = true;
    }));
    speech_frame(&mut conversation, start, false);
    assert_eq!(label(&mut conversation, start), "◉ speaking");
    assert!(conversation.speech().to_come);
    announce(&mut conversation, "m1");
    speech_frame(&mut conversation, start + ms(20), true);
    // Sent, the agent runs a tool: running, after the hold.
    conversation.apply(space(|space| space.activity = Activity::Running));
    let tool = start + ms(20) + crate::turn::HOLD;
    speech_frame(&mut conversation, tool, false);
    assert_eq!(label(&mut conversation, tool), "● running");
    assert!(!conversation.speech().to_come);
    // More is queued and its turn ends, while the next sentence takes far longer than the hold to synthesize: still
    // speaking, and not yet the participant's turn.
    conversation.apply(space(|space| {
        space.activity = Activity::Idle;
        space.speech_to_come = true;
    }));
    announce(&mut conversation, "m2");
    speech_frame(&mut conversation, tool + ms(1_000), true);
    let gap = tool + ms(1_000) + crate::turn::HOLD * 5;
    speech_frame(&mut conversation, gap, false);
    let status = conversation.pill_status(gap).unwrap();
    assert_eq!(status.speaking, Some(Speaking::Heard));
    assert_eq!(label(&mut conversation, gap), "◉ speaking");
    assert_eq!(turn_cues(&conversation, &mut feedback, gap), 0);
    // Its last sentence plays and nothing more is to come: idle once it has ended, and the participant's turn.
    speech_frame(&mut conversation, gap + ms(20), true);
    conversation.apply(space(|space| space.activity = Activity::Idle));
    let ended = gap + ms(20) + crate::turn::HOLD;
    speech_frame(&mut conversation, ended, false);
    assert_eq!(label(&mut conversation, ended), "● idle");
    assert_eq!(turn_cues(&conversation, &mut feedback, ended), 1);
    // While the connection is down, or out of the space, nothing is to come.
    conversation.apply(space(|space| space.speech_to_come = true));
    assert!(conversation.speech().to_come);
    conversation.apply(Event::State(ConnectionState::Connecting { attempt: 2 }));
    assert!(!conversation.speech().to_come);
    conversation.mode = Mode::Servers;
    assert!(!conversation.speech().to_come);
}

#[test]
fn the_pill_and_turn_follow_the_server_s_agent_state_smoothed_with_speech_here_layered_on_it() {
    use tomori_protocol::{AgentState, AgentStatus};
    let mut conversation = on_wrist(|space| space.activity = Activity::Idle);
    let mut feedback = feedback::Feedback::new(feedback::Settings::default());
    let start = Instant::now();
    let ms = Duration::from_millis;
    let mut sequence = 0;
    // The server says `state`, with `detail`, its session's activity saying otherwise, as an older client would see.
    let mut say = |conversation: &mut Conversation, state: AgentState, detail: Option<&str>| {
        sequence += 1;
        conversation.apply(space(|space| {
            space.activity = Activity::Running;
            space.agent = Some(AgentStatus {
                state,
                detail: detail.map(Into::into),
                sequence,
            });
        }));
    };
    let label = |conversation: &mut Conversation, at: Instant| {
        let status = conversation.pill_status(at).unwrap();
        crate::pill::rows(&status)[0]
            .plain()
            .split("  ")
            .next()
            .unwrap()
            .to_owned()
    };
    say(&mut conversation, AgentState::Thinking, None);
    // Shown as said, before any frame smoothed it.
    assert_eq!(label(&mut conversation, start), "● thinking");
    speech_frame(&mut conversation, start, false);
    say(
        &mut conversation,
        AgentState::Working,
        Some("running shell"),
    );
    speech_frame(&mut conversation, start + ms(20), false);
    assert_eq!(label(&mut conversation, start + ms(20)), "● running shell");
    // Speech playing here shows over its tool.
    announce(&mut conversation, "m1");
    speech_frame(&mut conversation, start + ms(40), true);
    assert_eq!(label(&mut conversation, start + ms(40)), "◉ speaking");
    // Thinking for a moment, briefer than the smoothing, never shows; once it lasts, it does.
    let quiet = start + ms(40) + crate::turn::HOLD;
    speech_frame(&mut conversation, quiet, false);
    say(&mut conversation, AgentState::Thinking, None);
    speech_frame(&mut conversation, quiet + ms(20), false);
    assert_eq!(label(&mut conversation, quiet + ms(20)), "● running shell");
    let lasted = quiet + ms(20) + crate::agent_view::BRIEF;
    speech_frame(&mut conversation, lasted, false);
    assert_eq!(label(&mut conversation, lasted), "● thinking");
    // A request waiting shows over speech playing here.
    say(&mut conversation, AgentState::Waiting, None);
    speech_frame(&mut conversation, lasted + ms(20), true);
    assert_eq!(label(&mut conversation, lasted + ms(20)), "● waiting");
    // No turn while the server says it is busy, though its session says idle.
    conversation.apply(space(|space| {
        space.activity = Activity::Idle;
        space.agent = Some(AgentStatus {
            state: AgentState::Thinking,
            detail: Some("stalled".into()),
            sequence: 100,
        });
    }));
    let ended = lasted + ms(20) + crate::turn::HOLD;
    speech_frame(&mut conversation, ended, false);
    assert_eq!(turn_cues(&conversation, &mut feedback, ended), 0);
    let lasted = ended + crate::agent_view::BRIEF;
    speech_frame(&mut conversation, lasted, false);
    assert_eq!(label(&mut conversation, lasted), "● thinking, stalled");
    // Idle, as the server says, though its session says running: the participant's turn.
    say(&mut conversation, AgentState::Idle, None);
    speech_frame(&mut conversation, lasted + ms(20), false);
    assert_eq!(turn_cues(&conversation, &mut feedback, lasted + ms(20)), 1);
    let idle = lasted + ms(20) + crate::agent_view::BRIEF;
    speech_frame(&mut conversation, idle, false);
    assert_eq!(label(&mut conversation, idle), "● idle");
    say(
        &mut conversation,
        AgentState::Error,
        Some("Lost the agent."),
    );
    speech_frame(&mut conversation, idle + ms(20), false);
    assert_eq!(
        label(&mut conversation, idle + ms(20)),
        "● error: Lost the agent."
    );
    // A state this build does not know, or none, is worked out from the session as before.
    say(&mut conversation, AgentState::Unknown, None);
    speech_frame(&mut conversation, idle + ms(40), false);
    assert!(conversation.agent().is_none());
    assert_eq!(label(&mut conversation, idle + ms(40)), "● running");
}

#[test]
fn moved_straight_into_another_space_its_agent_state_shows_at_once_whatever_its_sequence() {
    use tomori_protocol::{AgentState, AgentStatus};
    let mut conversation = on_wrist(|_| {});
    let start = Instant::now();
    let said = |state| AgentStatus {
        state,
        detail: None,
        sequence: 3,
    };
    conversation.apply(space(|space| {
        space.agent = Some(said(AgentState::Speaking))
    }));
    speech_frame(&mut conversation, start, false);
    assert_eq!(conversation.agent_state(), Some(AgentState::Speaking));
    // The server puts the participant straight into another space, unasked, whose agent is idle at the same sequence.
    conversation.apply(space(|space| {
        space.id = "space-2".into();
        space.agent = Some(said(AgentState::Idle));
    }));
    assert_eq!(conversation.agent_state(), Some(AgentState::Idle));
    speech_frame(&mut conversation, start + Duration::from_millis(20), false);
    assert_eq!(conversation.agent_state(), Some(AgentState::Idle));
}

#[test]
fn the_pill_says_speaking_while_speech_plays_then_settles_to_idle_with_your_turn_once() {
    use crate::pill::Speaking;
    let mut conversation = on_wrist(|space| space.activity = Activity::Idle);
    let mut feedback = feedback::Feedback::new(feedback::Settings::default());
    let start = Instant::now();
    let ms = Duration::from_millis;
    speech_frame(&mut conversation, start, false);
    assert_eq!(conversation.pill_status(start).unwrap().speaking, None);
    // The agent is idle on the server, but its reply still plays here.
    announce(&mut conversation, "m1");
    speech_frame(&mut conversation, start + ms(20), true);
    let status = conversation.pill_status(start + ms(20)).unwrap();
    assert_eq!(status.activity, Activity::Idle);
    assert_eq!(status.speaking, Some(Speaking::Heard));
    assert!(
        crate::pill::rows(&status)[0]
            .plain()
            .starts_with("◉ speaking")
    );
    assert_eq!(turn_cues(&conversation, &mut feedback, start + ms(20)), 0);
    // A breath between segments is still speaking, and no turn.
    speech_frame(&mut conversation, start + ms(400), false);
    assert_eq!(
        conversation.pill_status(start + ms(400)).unwrap().speaking,
        Some(Speaking::Heard)
    );
    assert_eq!(turn_cues(&conversation, &mut feedback, start + ms(400)), 0);
    // Once it has stopped for longer than the swarm's hold, the pill settles to idle, its dot pulsing once, and the turn is cued once.
    let ended = start + ms(20) + crate::turn::HOLD;
    speech_frame(&mut conversation, ended, false);
    let status = conversation.pill_status(ended).unwrap();
    assert_eq!(status.speaking, None);
    assert_eq!(
        crate::pill::rows(&status)[0].plain().split("  ").next(),
        Some("● idle")
    );
    assert!(
        conversation
            .pill_status(ended + crate::turn::PULSE / 2)
            .unwrap()
            .pulse
            > 250
    );
    assert_eq!(
        conversation
            .pill_status(ended + crate::turn::PULSE)
            .unwrap()
            .pulse,
        0
    );
    assert_eq!(turn_cues(&conversation, &mut feedback, ended), 1);
    speech_frame(&mut conversation, ended + ms(20), false);
    assert_eq!(turn_cues(&conversation, &mut feedback, ended + ms(20)), 0);
    // A reply with no speech is no turn.
    conversation.apply(space(|space| space.activity = Activity::Running));
    speech_frame(&mut conversation, ended + ms(1_000), false);
    conversation.apply(space(|space| space.activity = Activity::Idle));
    speech_frame(&mut conversation, ended + ms(2_000), false);
    assert_eq!(
        turn_cues(&conversation, &mut feedback, ended + ms(2_000)),
        0
    );
    assert_eq!(
        conversation.pill_status(ended + ms(2_300)).unwrap().pulse,
        0
    );
}

#[test]
fn speech_shows_over_running_and_the_turn_waits_for_the_agent_to_be_free() {
    use crate::pill::Speaking;
    let mut conversation = on_wrist(|space| space.activity = Activity::Running);
    let mut feedback = feedback::Feedback::new(feedback::Settings::default());
    let start = Instant::now();
    let ms = Duration::from_millis;
    speech_frame(&mut conversation, start, true);
    let status = conversation.pill_status(start).unwrap();
    assert_eq!(status.speaking, Some(Speaking::Heard));
    assert_eq!(status.activity, Activity::Running);
    // Speech ended, the agent still working: running again, and not yet the participant's turn.
    let ended = start + ms(1_000);
    speech_frame(&mut conversation, ended, false);
    assert_eq!(conversation.pill_status(ended).unwrap().speaking, None);
    assert_eq!(turn_cues(&conversation, &mut feedback, ended), 0);
    // Nor while an approval waits.
    conversation.apply(space(|space| {
        space.activity = Activity::Idle;
        space.approval = Some(approval(&[]));
    }));
    speech_frame(&mut conversation, ended + ms(100), false);
    assert_eq!(turn_cues(&conversation, &mut feedback, ended + ms(100)), 0);
    // Once it is idle with nothing waiting, it is, having spoken.
    conversation.apply(space(|space| {
        space.activity = Activity::Idle;
        space.approval = None;
    }));
    speech_frame(&mut conversation, ended + ms(200), false);
    assert_eq!(turn_cues(&conversation, &mut feedback, ended + ms(200)), 1);
}

#[test]
fn muted_speech_shows_as_speaking_muted_and_is_no_turn() {
    crate::test_support::log_everything();
    use crate::pill::Speaking;
    let mut conversation = on_wrist(|space| space.activity = Activity::Idle);
    let mut feedback = feedback::Feedback::new(feedback::Settings::default());
    conversation.toggle_mute();
    let start = Instant::now();
    speech_frame(&mut conversation, start, true);
    let status = conversation.pill_status(start).unwrap();
    assert_eq!(status.speaking, Some(Speaking::Muted));
    assert!(
        crate::pill::rows(&status)[0]
            .plain()
            .starts_with("◉ speaking, muted")
    );
    let ended = start + Duration::from_secs(1);
    speech_frame(&mut conversation, ended, false);
    assert_eq!(conversation.pill_status(ended).unwrap().speaking, None);
    assert_eq!(turn_cues(&conversation, &mut feedback, ended), 0);
    assert_eq!(conversation.pill_status(ended).unwrap().pulse, 0);
}

#[test]
fn speaking_before_a_reply_s_first_words_skips_it_as_it_comes_and_nothing_once_none_is_to_come() {
    crate::test_support::log_everything();
    let mut conversation = on_wrist(|space| space.activity = Activity::Idle);
    let start = Instant::now();
    let ms = Duration::from_millis;
    // An earlier reply, heard and ended; the next is queued, none of it playing yet.
    announce(&mut conversation, "m1");
    speech_frame(&mut conversation, start, true);
    conversation.apply(space(|space| space.speech_to_come = true));
    let pressed = start + crate::turn::HOLD * 4;
    speech_frame(&mut conversation, pressed, false);
    conversation.take_actions();
    conversation.talk(true);
    assert!(matches!(
        &conversation.take_actions()[..],
        [Action::SetAgentSkipped(true), Action::BeginUtterance(_)]
    ));
    conversation.talk(false);
    conversation.talk = None;
    conversation.take_actions();
    // Its first words are skipped, not heard as something other than what was said before.
    announce(&mut conversation, "m2");
    assert!(conversation.take_actions().is_empty());
    announce(&mut conversation, "m3");
    assert_eq!(
        conversation.take_actions(),
        [Action::SetAgentSkipped(false)]
    );
    // Spoken over again before any more plays, and nothing more comes: nothing stays skipped.
    speech_frame(&mut conversation, pressed + ms(20), false);
    conversation.talk(true);
    conversation.talk(false);
    conversation.talk = None;
    conversation.take_actions();
    conversation.apply(space(|space| space.speech_to_come = false));
    speech_frame(&mut conversation, pressed + ms(40), false);
    assert_eq!(
        conversation.take_actions(),
        [Action::SetAgentSkipped(false)]
    );
}

#[test]
fn speaking_over_the_agent_skips_the_rest_of_what_it_says_until_it_says_something_else() {
    crate::test_support::log_everything();
    use crate::pill::Speaking;
    let mut conversation = on_wrist(|space| space.activity = Activity::Idle);
    let mut feedback = feedback::Feedback::new(feedback::Settings::default());
    let start = Instant::now();
    let ms = Duration::from_millis;
    announce(&mut conversation, "m1");
    speech_frame(&mut conversation, start, true);
    conversation.take_actions();
    // X while it speaks skips the rest, before the microphone opens.
    conversation.talk(true);
    let actions = conversation.take_actions();
    assert!(matches!(
        &actions[..],
        [Action::SetAgentSkipped(true), Action::BeginUtterance(_)]
    ));
    speech_frame(&mut conversation, start + ms(20), true);
    conversation.talk(false);
    conversation.take_actions();
    // Still playing for the others, so still speaking, but skipped here.
    speech_frame(&mut conversation, start + ms(40), true);
    let status = conversation.pill_status(start + ms(40)).unwrap();
    assert_eq!(status.speaking, Some(Speaking::Skipped));
    assert!(
        crate::pill::rows(&status)[0]
            .plain()
            .starts_with("◉ speaking, skipped")
    );
    // More of the same message stays skipped.
    announce(&mut conversation, "m1");
    assert!(conversation.take_actions().is_empty());
    // What was skipped, ending, is no turn: the participant has spoken.
    let ended = start + ms(2_000);
    speech_frame(&mut conversation, ended, false);
    conversation.talk = None;
    speech_frame(&mut conversation, ended + ms(20), false);
    assert_eq!(turn_cues(&conversation, &mut feedback, ended + ms(20)), 0);
    // The agent's next message is heard.
    announce(&mut conversation, "m2");
    assert_eq!(
        conversation.take_actions(),
        [Action::SetAgentSkipped(false)]
    );
    speech_frame(&mut conversation, ended + ms(40), true);
    assert_eq!(
        conversation.pill_status(ended + ms(40)).unwrap().speaking,
        Some(Speaking::Heard)
    );
    // X long after speech ended skips nothing.
    let later = ended + ms(5_000);
    speech_frame(&mut conversation, later, false);
    conversation.take_actions();
    conversation.talk(true);
    assert!(matches!(
        &conversation.take_actions()[..],
        [Action::BeginUtterance(_)]
    ));
}

#[test]
fn speaking_over_a_replay_stops_it_and_asking_again_hears_what_was_skipped() {
    let mut conversation = on_wrist(|space| {
        space.activity = Activity::Idle;
        space.speech = SpeechSetting::Full;
    });
    let start = Instant::now();
    Session::report(&mut conversation, Report::Replaying(true));
    speech_frame(&mut conversation, start, true);
    conversation.take_actions();
    conversation.talk(true);
    let actions = conversation.take_actions();
    assert!(matches!(
        &actions[..],
        [
            Action::SetAgentSkipped(true),
            Action::Send(ClientMessage::StopReplay),
            Action::BeginUtterance(_)
        ]
    ));
    conversation.talk(false);
    Session::report(&mut conversation, Report::Replaying(false));
    conversation.take_actions();
    // The right stick plays it again, heard.
    conversation.replay();
    assert_eq!(
        conversation.take_actions(),
        [
            Action::SetAgentSkipped(false),
            Action::Send(ClientMessage::Replay { message: None })
        ]
    );
    // Leaving the space forgets a skip.
    announce(&mut conversation, "m1");
    speech_frame(&mut conversation, start + Duration::from_millis(20), true);
    conversation.talk(true);
    conversation.talk(false);
    conversation.take_actions();
    conversation.apply(Event::Message(ServerMessage::Left { deleted: None }));
    assert!(
        conversation
            .take_actions()
            .contains(&Action::SetAgentSkipped(false))
    );
}

#[test]
fn the_space_section_switches_mode_and_speech_and_leaves_once_confirmed() {
    let mut conversation = joined(|space| {
        space.speech = SpeechSetting::Brief;
        space.participants.push(Participant {
            name: "Bob".into(),
            source: 2,
            engagement: Some(Engagement::Away),
        });
    });
    let model = conversation.menu_model();
    let space = model.space.as_ref().unwrap();
    assert_eq!(space.running, "opencode in tomori");
    assert_eq!(space.here, "1 here, 1 away");
    assert_eq!(model.people.len(), 2);
    assert!(model.people[0].you && !model.people[0].away);
    assert!(!model.people[1].you && model.people[1].away);
    let colour = crate::speakers::colour(2);
    assert_eq!(model.people[1].colour, [colour[0], colour[1], colour[2]]);
    conversation.choose_menu(crate::hand_menu::Choice::Mode("plan".into()));
    conversation.choose_menu(crate::hand_menu::Choice::Speech);
    assert_eq!(
        sent(&mut conversation),
        [
            ClientMessage::SetMode {
                mode: "plan".into()
            },
            ClientMessage::SetSpeech {
                speech: SpeechSetting::Full
            }
        ]
    );
    conversation.choose_menu(crate::hand_menu::Choice::Mute);
    assert_eq!(conversation.take_actions(), [Action::SetAgentMuted(true)]);
    assert!(conversation.menu_model().muted);
    conversation.choose_menu(crate::hand_menu::Choice::Leave);
    assert_eq!(sent(&mut conversation), [ClientMessage::Leave]);
    assert!(conversation.joined_space().is_none());
}

/// The server's word that the space was deleted by `by`, with its session if `session`.
fn deleted_by(by: &str, session: bool) -> Event {
    Event::Message(ServerMessage::Left {
        deleted: Some(tomori_protocol::SpaceDeletion {
            by: by.into(),
            session,
        }),
    })
}

#[test]
fn an_owner_deletes_the_space_from_the_menu_and_the_lobby_says_so() {
    let deletes = |space: &mut SpaceState| {
        space.deletes = true;
        space.deletes_session = true;
    };
    let mut conversation = joined(deletes);
    assert_eq!(
        conversation.menu_model().space.unwrap().deletion,
        Some(true)
    );
    conversation.choose_menu(crate::hand_menu::Choice::Delete { session: false });
    assert_eq!(
        sent(&mut conversation),
        [ClientMessage::DeleteSpace {
            space: SPACE.into(),
            session: false,
        }]
    );
    // Still in the space until the server has deleted it.
    assert!(conversation.joined_space().is_some());
    conversation.apply(deleted_by("Ada", false));
    assert!(conversation.joined_space().is_none());
    assert_eq!(conversation.ended.as_deref(), Some("You deleted Tomori."));
    // The listing that follows comes unasked.
    assert!(sent(&mut conversation).is_empty());

    // With its session, the lobby says the session is being deleted, and then what became of it.
    for (error, said) in [
        (None, "You deleted Tomori and its agent session."),
        (
            Some("the agent is unreachable"),
            "You deleted Tomori, but its agent session was kept: the agent is unreachable",
        ),
    ] {
        let mut conversation = joined(deletes);
        conversation.choose_menu(crate::hand_menu::Choice::Delete { session: true });
        assert_eq!(
            sent(&mut conversation),
            [ClientMessage::DeleteSpace {
                space: SPACE.into(),
                session: true,
            }]
        );
        conversation.apply(deleted_by("Ada", true));
        assert_eq!(
            conversation.ended.as_deref(),
            Some("You deleted Tomori; deleting its agent session.")
        );
        conversation.apply(Event::Message(ServerMessage::SessionDeleted {
            error: error.map(str::to_owned),
        }));
        assert_eq!(conversation.ended.as_deref(), Some(said));
        // Said once: a stray answer later changes nothing.
        conversation.apply(Event::Message(ServerMessage::SessionDeleted {
            error: None,
        }));
        assert_eq!(conversation.ended.as_deref(), Some(said));
    }

    // A deletion the server refuses leaves the space as it was, and a later one by someone else is theirs.
    let mut conversation = joined(deletes);
    conversation.choose_menu(crate::hand_menu::Choice::Delete { session: true });
    conversation.apply(Event::Message(ServerMessage::Error {
        error: "Its agent session is bound to another space too, so nothing was deleted.".into(),
        space: None,
        panel: None,
        wall: None,
    }));
    assert!(conversation.joined_space().is_some());
    conversation.apply(deleted_by("Bob", false));
    assert_eq!(
        conversation.ended.as_deref(),
        Some("Tomori was deleted by Bob.")
    );
}

#[test]
fn a_space_deleted_by_someone_else_returns_everyone_to_the_lobby_saying_who() {
    let mut conversation = joined(|_| {});
    conversation.apply(deleted_by("Bob", false));
    assert!(conversation.joined_space().is_none());
    assert_eq!(
        conversation.ended.as_deref(),
        Some("Tomori was deleted by Bob.")
    );
    assert!(
        conversation
            .lobby_text()
            .iter()
            .any(|line| line.contains("Tomori was deleted by Bob."))
    );
    let mut conversation = joined(|_| {});
    conversation.apply(deleted_by("Bob", true));
    assert_eq!(
        conversation.ended.as_deref(),
        Some("Tomori was deleted by Bob, with its agent session.")
    );
    // An answer about a session this participant did not ask to delete says nothing.
    conversation.apply(Event::Message(ServerMessage::SessionDeleted {
        error: None,
    }));
    assert_eq!(
        conversation.ended.as_deref(),
        Some("Tomori was deleted by Bob, with its agent session.")
    );
    // In the lobby, a late one names no space; on the server list, a stray one changes nothing.
    let mut conversation = joined(|_| {});
    conversation.choose_menu(crate::hand_menu::Choice::Leave);
    conversation.apply(deleted_by("Bob", false));
    assert_eq!(
        conversation.ended.as_deref(),
        Some("The space was deleted by Bob.")
    );
    let mut conversation = crate::test_support::conversation("Ada");
    conversation.apply(deleted_by("Bob", false));
    assert!(conversation.ended.is_none());
}

#[test]
fn only_an_owner_deletes_and_only_what_the_server_deletes() {
    // A collaborator is offered nothing, and is refused without asking the server.
    let mut conversation = joined(|space| space.deletes = true);
    conversation.role = ConnectionRole::Collaborator;
    assert_eq!(conversation.menu_model().space.unwrap().deletion, None);
    conversation.choose_menu(crate::hand_menu::Choice::Delete { session: false });
    assert!(sent(&mut conversation).is_empty());
    assert_eq!(
        conversation.notice.as_deref(),
        Some("Only owners can delete a space.")
    );
    // An owner on an older server is offered nothing either.
    let mut conversation = joined(|_| {});
    assert_eq!(conversation.menu_model().space.unwrap().deletion, None);
    conversation.choose_menu(crate::hand_menu::Choice::Delete { session: false });
    assert!(sent(&mut conversation).is_empty());
    assert_eq!(
        conversation.notice.as_deref(),
        Some("This server does not delete spaces.")
    );
    // Nor the session, where its backend does not delete sessions.
    let mut conversation = joined(|space| space.deletes = true);
    assert_eq!(
        conversation.menu_model().space.unwrap().deletion,
        Some(false)
    );
    conversation.choose_menu(crate::hand_menu::Choice::Delete { session: true });
    assert!(sent(&mut conversation).is_empty());
    assert_eq!(
        conversation.notice.as_deref(),
        Some("This space's agent session cannot be deleted from here.")
    );
    // Outside a space, there is nothing to delete.
    conversation.choose_menu(crate::hand_menu::Choice::Leave);
    sent(&mut conversation);
    conversation.choose_menu(crate::hand_menu::Choice::Delete { session: false });
    assert!(sent(&mut conversation).is_empty());
    assert_eq!(conversation.notice.as_deref(), Some("Join a space first."));
}

#[test]
fn an_owner_changes_the_environment_from_the_menu_and_everyone_follows() {
    let mut conversation = joined(|space| space.environment_changes = true);
    conversation.environments.push(EnvironmentInfo {
        id: "minimal".into(),
        label: "Grid".into(),
        version: 1,
    });
    let offered = |conversation: &Conversation| {
        let model = conversation.menu_model();
        let space = model.space.unwrap();
        (space.environment, space.environments)
    };
    assert_eq!(
        offered(&conversation),
        (
            Some("Sky".into()),
            vec![
                ("sky".into(), "Sky".into(), true),
                ("minimal".into(), "Grid".into(), false)
            ]
        )
    );
    conversation.choose_menu(crate::hand_menu::Choice::Environment("minimal".into()));
    assert_eq!(
        sent(&mut conversation),
        [ClientMessage::SetEnvironment {
            environment: "minimal".into()
        }]
    );
    assert_eq!(
        conversation.notice.as_deref(),
        Some("Changing the environment to Grid for everyone.")
    );
    // Choosing the one the space is in, as the menu lists it even when this client cannot present it, asks the server for nothing.
    conversation.choose_menu(crate::hand_menu::Choice::Environment("sky".into()));
    assert!(sent(&mut conversation).is_empty());
    assert_eq!(
        conversation.notice.as_deref(),
        Some("The space is already in Sky.")
    );
    // The scenery changes only when the server says the space has, for everyone alike.
    assert_eq!(conversation.environment(), crate::test_support::SKY);
    conversation.apply(space(|space| {
        space.environment = "minimal".into();
        space.environment_changes = true;
    }));
    assert_eq!(conversation.environment(), Environment::MINIMAL);
    assert_eq!(
        offered(&conversation).1[1],
        ("minimal".into(), "Grid".into(), true)
    );
    // A space in an environment the server no longer offers shows it first, as the current one, saying that this
    // build cannot present it.
    conversation.apply(space(|space| {
        space.environment = "observe".into();
        space.environment_changes = true;
    }));
    let (name, environments) = offered(&conversation);
    assert_eq!(name.as_deref(), Some("Unknown environment: observe"));
    assert_eq!(
        environments[0],
        (
            "observe".into(),
            "Unknown environment: observe".into(),
            true
        )
    );
    assert_eq!(environments.len(), 3);
    // As does an offered one this build has at another version, from a server that offers it anyway.
    conversation.apply(space(|space| {
        space.environment = "minimal".into();
        space.environment_version = Some(2);
        space.environment_changes = true;
    }));
    assert_eq!(conversation.environment(), Environment::MINIMAL);
    let (name, environments) = offered(&conversation);
    assert_eq!(
        name.as_deref(),
        Some("Grid needs version 2; this app has 1")
    );
    assert_eq!(
        environments[1],
        (
            "minimal".into(),
            "Grid needs version 2; this app has 1".into(),
            true
        )
    );
}

#[test]
fn only_an_owner_on_a_server_that_changes_environments_is_offered_them() {
    // An older server names the environment but does not change it.
    let mut conversation = joined(|_| {});
    let shown = conversation.menu_model().space.unwrap();
    assert_eq!(shown.environment.as_deref(), Some("Sky"));
    assert!(shown.environments.is_empty());
    conversation.change_environment("sky");
    assert!(sent(&mut conversation).is_empty());
    assert_eq!(
        conversation.notice.as_deref(),
        Some("This server does not change a space's environment.")
    );
    // An even older one does not name it.
    conversation.apply(space(|space| space.environment.clear()));
    assert_eq!(conversation.menu_model().space.unwrap().environment, None);
    // Anyone but an owner is offered none, and asks for nothing.
    let mut conversation = joined(|space| space.environment_changes = true);
    conversation.apply(Event::Message(ServerMessage::Role {
        role: ConnectionRole::Collaborator,
    }));
    conversation.take_actions();
    assert!(
        conversation
            .menu_model()
            .space
            .unwrap()
            .environments
            .is_empty()
    );
    conversation.choose_menu(crate::hand_menu::Choice::Environment("sky".into()));
    assert!(sent(&mut conversation).is_empty());
    assert_eq!(
        conversation.notice.as_deref(),
        Some("Only owners can change the space's environment.")
    );
    // Outside a space there is nothing to change.
    let mut conversation = joined(|space| space.environment_changes = true);
    conversation.leave();
    conversation.take_actions();
    conversation.change_environment("sky");
    assert!(sent(&mut conversation).is_empty());
    assert_eq!(conversation.notice.as_deref(), Some("Join a space first."));
}

#[test]
fn a_visitor_cannot_switch_modes_from_the_menu() {
    let mut conversation = joined(|_| {});
    conversation.apply(Event::Message(ServerMessage::Role {
        role: ConnectionRole::Visitor,
    }));
    conversation.take_actions();
    conversation.choose_menu(crate::hand_menu::Choice::Mode("plan".into()));
    conversation.choose_menu(crate::hand_menu::Choice::Speech);
    assert!(sent(&mut conversation).is_empty());
}

#[test]
fn the_recording_indicator_follows_an_utterance_to_the_queue() {
    use crate::recording::Phase;
    let mut conversation = joined(|space| {
        space.queue = vec![entry("q-1", "Bob", QueueState::Queued)];
    });
    // The limit comes from the server's Welcome.
    assert_eq!(conversation.utterance_limit(), None);
    conversation.apply(Event::State(ConnectionState::Connected {
        source: 1,
        server: "studio".into(),
        role: ConnectionRole::Owner,
        certificate: None,
        backends: vec![],
        roots: vec![],
        environments: sky(),
        utterance_limit: Some(Duration::from_secs(300)),
        name: None,
    }));
    assert_eq!(
        conversation.utterance_limit(),
        Some(Duration::from_secs(300))
    );
    let now = Instant::now();
    assert_eq!(conversation.recording_phase(now), None);
    let utterance = press(&mut conversation);
    conversation.apply(Event::Message(ServerMessage::Transcript {
        utterance: utterance.clone(),
        text: "check the".into(),
        finished: false,
    }));
    let later = Instant::now() + Duration::from_secs(2);
    assert!(matches!(
        conversation.recording_phase(later),
        Some(Phase::Recording { elapsed, heard, answering: None, about: None })
            if elapsed >= Duration::from_secs(2) && heard == "check the"
    ));
    conversation.talk(false);
    let sending = |received| {
        Some(Phase::Sending {
            heard: "check the".into(),
            answering: false,
            received,
        })
    };
    // Sending until the server says it has the words, as an older server never does.
    assert_eq!(conversation.recording_phase(Instant::now()), sending(None));
    // Then what it is doing with them, as it says; another utterance's progress is not this one's.
    let progress = |utterance: &str, stage, part| {
        Event::Message(ServerMessage::TranscriptProgress {
            utterance: utterance.to_owned(),
            stage,
            part,
        })
    };
    conversation.apply(progress("earlier", TranscriptStage::Waiting, None));
    assert_eq!(conversation.recording_phase(Instant::now()), sending(None));
    let part = Some(tomori_protocol::TranscriptPart { number: 2, of: 3 });
    for (stage, part) in [
        (TranscriptStage::Transcribing, None),
        (TranscriptStage::Waiting, None),
        (TranscriptStage::Transcribing, part),
    ] {
        conversation.apply(progress(&utterance, stage, part));
        assert_eq!(
            conversation.recording_phase(Instant::now()),
            sending(Some(crate::recording::Received { stage, part }))
        );
    }
    conversation.apply(Event::Message(ServerMessage::Transcript {
        utterance: utterance.clone(),
        text: "check the CI".into(),
        finished: true,
    }));
    // Progress after the outcome changes nothing.
    conversation.apply(progress(&utterance, TranscriptStage::Transcribing, None));
    conversation.apply(space(|space| {
        space.queue = vec![
            entry("q-1", "Bob", QueueState::Queued),
            QueueEntry {
                text: "check the CI".into(),
                ..entry("q-2", "Ada", QueueState::Queued)
            },
        ];
    }));
    let now = Instant::now();
    assert_eq!(
        conversation.recording_phase(now),
        Some(Phase::Sent {
            answered: false,
            position: Some(2)
        })
    );
    // It shows for a moment, then goes.
    assert_eq!(
        conversation.recording_phase(now + crate::recording::OUTCOME),
        None
    );
}

#[test]
fn the_recording_indicator_shows_a_failure_until_it_is_dismissed() {
    use crate::recording::Phase;
    let mut conversation = joined(|_| {});
    let utterance = press(&mut conversation);
    conversation.talk(false);
    conversation.take_actions();
    conversation.apply(Event::Message(ServerMessage::TranscriptFailed {
        utterance,
        error: "Nothing was sent: longer than 5 minutes.".into(),
    }));
    let failed = Some(Phase::Failed {
        reason: "Nothing was sent: longer than 5 minutes.".into(),
    });
    assert_eq!(conversation.recording_phase(Instant::now()), failed);
    // It is shown for a few seconds, longer than other outcomes, so that why can be read, then fades while the problem stays.
    assert!(crate::recording::NOT_SENT > crate::recording::OUTCOME);
    let now = Instant::now();
    assert_eq!(
        conversation.recording_phase(now + crate::recording::OUTCOME),
        failed
    );
    assert_eq!(
        conversation.recording_phase(now + crate::recording::NOT_SENT),
        None
    );
    assert!(conversation.problem().is_some());
    // B dismisses it sooner, with the problem.
    assert_eq!(conversation.recording_phase(Instant::now()), failed);
    conversation.back();
    assert_eq!(conversation.recording_phase(Instant::now()), None);
    // Pressing X again replaces it.
    let utterance = press(&mut conversation);
    conversation.talk(false);
    conversation.take_actions();
    conversation.apply(Event::Message(ServerMessage::TranscriptFailed {
        utterance,
        error: "no speech heard".into(),
    }));
    press(&mut conversation);
    let Some(Phase::Recording { .. }) = conversation.recording_phase(Instant::now()) else {
        unreachable!("Some")
    };
}

#[test]
fn a_recording_with_no_speech_is_shown_only_briefly_on_the_card() {
    use crate::recording::Phase;
    let mut conversation = joined(|_| {});
    let silent = press(&mut conversation);
    conversation.talk(false);
    conversation.take_actions();
    conversation.apply(Event::Message(ServerMessage::TranscriptFailed {
        utterance: silent,
        error: tomori_protocol::NO_SPEECH.into(),
    }));
    let unheard = Some(Phase::Failed {
        reason: "no speech heard".into(),
    });
    let now = Instant::now();
    assert_eq!(conversation.recording_phase(now), unheard);
    assert_eq!(
        conversation.recording_phase(now + crate::recording::OUTCOME),
        unheard
    );
    assert_eq!(
        conversation.recording_phase(now + crate::recording::NOT_SENT),
        None
    );
    // Nothing said was lost, so it is no problem: not on the problem line, the pill or the right wrist.
    assert_eq!(conversation.problem(), None);
    assert!(!conversation.text().contains("no speech heard"));
    assert_eq!(conversation.pill_status(now).unwrap().need, None);
    assert_eq!(conversation.wrist_waiting(), None);
    // B dismisses it from the card sooner, and then has nothing more to dismiss.
    conversation.back();
    assert_eq!(conversation.recording_phase(Instant::now()), None);
    conversation.back();
    assert_eq!(
        conversation.notice.as_deref(),
        Some("Hold B to leave the space.")
    );
    // The next words' outcome replaces it on the card.
    let silent = press(&mut conversation);
    conversation.talk(false);
    conversation.take_actions();
    conversation.apply(Event::Message(ServerMessage::TranscriptFailed {
        utterance: silent,
        error: tomori_protocol::NO_SPEECH.into(),
    }));
    let utterance = press(&mut conversation);
    conversation.talk(false);
    conversation.take_actions();
    conversation.apply(transcript(&utterance, "Run the tests.", true));
    assert_eq!(
        conversation.recording_phase(Instant::now()),
        Some(Phase::Sent {
            answered: false,
            position: None
        })
    );
}

#[test]
fn the_next_words_replace_words_that_were_not_sent_on_the_card_the_problem_line_and_the_wrist() {
    use crate::recording::Phase;
    const FAILED: &str = "Speech recognition failed.";
    let mut conversation = joined(|_| {});
    let lost = press(&mut conversation);
    conversation.talk(false);
    conversation.take_actions();
    conversation.apply(Event::Message(ServerMessage::TranscriptFailed {
        utterance: lost,
        error: FAILED.into(),
    }));
    assert_eq!(
        conversation.recording_phase(Instant::now()),
        Some(Phase::Failed {
            reason: FAILED.into()
        })
    );
    assert_eq!(conversation.problem().as_deref(), Some(FAILED));
    // The card showed it, with the not-sent cue, so the right wrist does not cue it.
    assert_eq!(conversation.wrist_waiting(), None);
    for words in ["Run the tests.", "Then the lints."] {
        let utterance = press(&mut conversation);
        conversation.talk(false);
        conversation.take_actions();
        conversation.apply(transcript(&utterance, words, true));
        let now = Instant::now();
        assert_eq!(
            conversation.recording_phase(now),
            Some(Phase::Sent {
                answered: false,
                position: None
            })
        );
        assert_eq!(
            conversation.recording_phase(now + crate::recording::OUTCOME),
            None
        );
        assert_eq!(conversation.problem(), None);
        assert!(!conversation.text().contains(FAILED));
        assert_eq!(conversation.wrist_waiting(), None);
    }
    // A failure of earlier words, arriving while later words are recorded, is not on the card, so the wrist cues it until the later words replace it.
    let earlier = press(&mut conversation);
    conversation.talk(false);
    conversation.take_actions();
    let later = press(&mut conversation);
    conversation.apply(Event::Message(ServerMessage::TranscriptFailed {
        utterance: earlier,
        error: FAILED.into(),
    }));
    assert_eq!(conversation.wrist_waiting(), Some(true));
    conversation.talk(false);
    conversation.take_actions();
    conversation.apply(transcript(&later, "Merge it.", true));
    assert_eq!(conversation.problem(), None);
    assert_eq!(conversation.wrist_waiting(), None);
    // Another problem than the words' is not replaced by them.
    conversation.fail("Lost the agent.");
    let utterance = press(&mut conversation);
    conversation.talk(false);
    conversation.take_actions();
    conversation.apply(transcript(&utterance, "Are you there?", true));
    assert_eq!(conversation.problem().as_deref(), Some("Lost the agent."));
}

#[test]
fn the_recording_indicator_names_the_question_being_answered() {
    use crate::recording::Phase;
    let mut conversation = joined(|space| {
        space.voice_answers = true;
        space.question = Some(Question {
            id: "q".into(),
            title: "Which branch?".into(),
            text: "Pick one.".into(),
            field: "branch".into(),
            supported: true,
        });
    });
    let utterance = press(&mut conversation);
    assert!(matches!(
        conversation.recording_phase(Instant::now()),
        Some(Phase::Recording { answering: Some(field), .. }) if field == "Which branch?"
    ));
    conversation.talk(false);
    conversation.apply(Event::Message(ServerMessage::Transcript {
        utterance,
        text: "main".into(),
        finished: true,
    }));
    assert_eq!(
        conversation.recording_phase(Instant::now()),
        Some(Phase::Sent {
            answered: true,
            position: None
        })
    );
    // The live microphone shows as a badge.
    assert!(!conversation.live());
    conversation.toggle_live();
    assert!(conversation.live());
}

#[test]
fn a_failed_transcript_is_shown_on_its_own_line_until_dismissed() {
    let mut conversation = joined(|_| {});
    let utterance = press(&mut conversation);
    conversation.talk(false);
    conversation.take_actions();
    conversation.apply(Event::Message(ServerMessage::TranscriptFailed {
        utterance,
        error: "Speech recognition failed.".into(),
    }));
    assert!(
        conversation
            .text()
            .contains("! 0s Speech recognition failed.")
    );
    // Talking again does not wipe it before it is read.
    press(&mut conversation);
    let text = conversation.text();
    assert!(text.contains("MIC AGENT"), "{text}");
    assert!(text.contains("! 0s Speech recognition failed."), "{text}");
    conversation.talk(false);
    conversation.take_actions();
    // B dismisses it.
    conversation.back();
    assert!(!conversation.text().contains("Speech recognition failed."));
    assert!(conversation.take_actions().is_empty());
}

#[test]
fn a_failure_while_recording_closes_the_microphone() {
    let mut conversation = joined(|_| {});
    let utterance = press(&mut conversation);
    conversation.apply(Event::Message(ServerMessage::TranscriptFailed {
        utterance: utterance.clone(),
        error: "Some of the recording was lost, so nothing was sent. Please try again.".into(),
    }));
    // The server has already ended it, so it is not cancelled.
    assert_eq!(conversation.take_actions(), [Action::AbandonUtterance]);
    assert_eq!(conversation.recording(), None);
    conversation.talk(false);
    assert!(conversation.take_actions().is_empty());
    assert!(
        conversation
            .text()
            .contains("Some of the recording was lost")
    );

    let utterance = press(&mut conversation);
    conversation.apply(Event::UtteranceFailed(tomori_client::FailedUtterance {
        utterance,
        failure: UtteranceFailure::Congested,
    }));
    assert_eq!(conversation.take_actions(), [Action::CancelUtterance]);
    assert!(conversation.text().contains("Network too slow"));

    press(&mut conversation);
    conversation.microphone_failed("No microphone is available.");
    assert_eq!(conversation.take_actions(), [Action::CancelUtterance]);
    assert!(conversation.text().contains("No microphone is available."));
}

#[test]
fn losing_focus_the_connection_or_the_space_cancels_recording() {
    let mut conversation = joined(|_| {});
    press(&mut conversation);
    conversation.stop_talking();
    assert_eq!(conversation.take_actions(), [Action::CancelUtterance]);
    // A released utterance is still transcribed.
    press(&mut conversation);
    conversation.talk(false);
    conversation.take_actions();
    conversation.stop_talking();
    assert!(conversation.take_actions().is_empty());

    press(&mut conversation);
    conversation.apply(Event::State(ConnectionState::Disconnected {
        reason: tomori_client::Disconnect::Timeout,
        retry_in: Duration::from_secs(1),
    }));
    assert_eq!(conversation.take_actions(), [Action::CancelUtterance]);
    assert!(conversation.text().contains("Connection lost"));
    // Offline, nothing is recorded.
    conversation.talk(true);
    assert!(conversation.take_actions().is_empty());
    assert!(conversation.text().contains("Offline; cannot record."));

    conversation.apply(connected());
    conversation.take_actions();
    press(&mut conversation);
    hold_b(&mut conversation);
    assert_eq!(
        conversation.take_actions(),
        [Action::CancelUtterance, Action::Send(ClientMessage::Leave)]
    );
}

#[test]
fn x_outside_a_space_explains_itself() {
    let mut conversation = crate::test_support::conversation("Ada");
    conversation.connect(Choice::new("Studio", "10.0.0.1:8765", Source::Discovered));
    conversation.apply(connected());
    conversation.take_actions();
    conversation.talk(true);
    assert!(conversation.take_actions().is_empty());
    assert!(
        conversation
            .text()
            .contains("Join a space to talk to the agent.")
    );
}

#[test]
fn the_recording_panel_fits() {
    let long = "word ".repeat(200);
    let mut conversation = joined(|_| {});
    let utterance = press(&mut conversation);
    conversation.apply(transcript(&utterance, &long, false));
    assert_fits(&mut conversation);
    conversation.talk(false);
    assert_fits(&mut conversation);
}

#[test]
fn the_newest_words_heard_stay_in_view() {
    let mut conversation = joined(|_| {});
    let utterance = press(&mut conversation);
    let heard = format!("{} the newest words", "older words ".repeat(10));
    conversation.apply(transcript(&utterance, &heard, false));
    assert!(conversation.text().contains("MIC AGENT: …"));
    assert!(conversation.text().contains("the newest words"));
    conversation.talk(false);
    assert!(conversation.text().contains("the newest words..."));
}

#[test]
fn a_failure_is_not_hidden_behind_sending_or_recording() {
    let mut conversation = joined(|_| {});
    let first = press(&mut conversation);
    conversation.talk(false);
    conversation.take_actions();
    let second = press(&mut conversation);
    conversation.apply(Event::Message(ServerMessage::TranscriptFailed {
        utterance: first,
        error: "Speech recognition failed.".into(),
    }));
    let text = conversation.text();
    assert!(text.contains("MIC AGENT"), "{text}");
    assert!(text.contains("Speech recognition failed."), "{text}");
    conversation.talk(false);
    let text = conversation.text();
    assert!(text.contains("Sending: "), "{text}");
    assert!(text.contains("Speech recognition failed."), "{text}");
    assert_fits(&mut conversation);
    // The newer words' outcome replaces it.
    conversation.apply(transcript(&second, "Run the tests.", true));
    assert!(!conversation.text().contains("Speech recognition failed."));
    assert_eq!(conversation.problem(), None);
}

#[test]
fn leaving_the_server_forgets_words_being_sent() {
    let mut conversation = joined(|_| {});
    press(&mut conversation);
    conversation.talk(false);
    conversation.take_actions();
    conversation.connect(Choice::new("Other", "10.0.0.2:8765", Source::Discovered));
    assert_eq!(conversation.talk, None);

    let mut conversation = joined(|_| {});
    press(&mut conversation);
    conversation.talk(false);
    conversation.take_actions();
    // Leave the space, then the server.
    hold_b(&mut conversation);
    conversation.back();
    assert_eq!(conversation.mode, Mode::Servers);
    assert_eq!(conversation.talk, None);
}

#[test]
fn keeps_the_selection_when_lists_change() {
    let mut conversation = crate::test_support::conversation("Ada");
    conversation.set_servers(vec![
        Choice::new("A", "a", Source::Discovered),
        Choice::new("B", "b", Source::Discovered),
    ]);
    conversation.navigate(0, 1);
    conversation.set_servers(vec![
        Choice::new("C", "c", Source::Discovered),
        Choice::new("A", "a", Source::Discovered),
        Choice::new("B", "b", Source::Discovered),
    ]);
    conversation.accept();
    assert!(matches!(
        conversation.take_actions().as_slice(),
        [Action::Connect(choice)] if choice.address == "b"
    ));
}

#[test]
fn selects_the_used_server_again_when_browsing_finds_it_after_returning() {
    let mut conversation = crate::test_support::conversation("Ada");
    let usb = Choice::new("USB", USB, Source::Usb);
    let servers = vec![
        Choice::new("A", "a", Source::Discovered),
        Choice::new("B", "b", Source::Discovered),
        usb.clone(),
    ];
    conversation.set_servers(servers.clone());
    conversation.navigate(0, 1);
    conversation.accept();
    conversation.back();
    conversation.take_actions();
    // Browsing restarts with only the USB entry, then finds the servers again.
    conversation.set_servers(vec![usb.clone()]);
    conversation.set_servers(servers);
    conversation.accept();
    assert!(matches!(
        conversation.take_actions().as_slice(),
        [Action::Connect(choice)] if choice.address == "b"
    ));
    // Moving the selection while the server is missing forgets it.
    conversation.back();
    conversation.set_servers(vec![usb.clone()]);
    conversation.navigate(0, 1);
    conversation.set_servers(vec![usb.clone(), Choice::new("B", "b", Source::Discovered)]);
    assert!(conversation.text().contains("> USB"));
}

#[test]
fn connecting_to_another_server_while_the_chosen_one_is_missing_forgets_it() {
    let mut conversation = crate::test_support::conversation("Ada");
    let usb = Choice::new("USB", USB, Source::Usb);
    let b = Choice::new("B", "b", Source::Discovered);
    conversation.set_servers(vec![usb.clone(), b.clone()]);
    conversation.navigate(0, 1);
    conversation.accept();
    conversation.back();
    // B is missing when browsing restarts, and A connects to USB instead.
    conversation.set_servers(vec![usb.clone()]);
    conversation.accept();
    conversation.back();
    conversation.take_actions();
    conversation.set_servers(vec![usb.clone()]);
    conversation.set_servers(vec![b, usb]);
    let text = conversation.text();
    assert!(text.contains("> USB"), "{text}");
}

fn remembered() -> Choice {
    let mut choice = Choice::new("Studio", "10.0.0.2:8765", Source::Remembered);
    choice.instance = Some("studio".into());
    choice
}

#[test]
fn connects_to_the_remembered_server_when_it_appears_but_only_once() {
    let mut conversation = crate::test_support::conversation("Ada");
    conversation.set_servers(vec![Choice::new("USB", USB, Source::Usb)]);
    assert!(conversation.take_actions().is_empty());
    let start = Instant::now();
    conversation.tick(start);
    conversation.set_servers(vec![remembered(), Choice::new("USB", USB, Source::Usb)]);
    // Not at once: the headset waits a moment for other servers to be found.
    conversation.tick(start);
    conversation.tick(start + AUTOMATIC_SETTLE / 2);
    assert!(conversation.take_actions().is_empty());
    conversation.tick(start + AUTOMATIC_SETTLE);
    assert!(matches!(
        conversation.take_actions().as_slice(),
        [Action::Connect(choice)] if choice.address == "10.0.0.2:8765"
    ));
    assert_eq!(conversation.mode(), Mode::Lobby);
    // Returning to the list leaves the choice to the participant.
    conversation.back();
    conversation.take_actions();
    conversation.set_servers(vec![remembered(), Choice::new("USB", USB, Source::Usb)]);
    conversation.tick(start + AUTOMATIC_SETTLE * 3);
    conversation.tick(start + AUTOMATIC_SETTLE * 6);
    assert!(conversation.take_actions().is_empty());
    assert_eq!(conversation.mode(), Mode::Servers);
}

#[test]
fn moving_the_selection_stops_the_remembered_server_connecting() {
    let mut conversation = crate::test_support::conversation("Ada");
    conversation.set_servers(vec![
        Choice::new("Other", "10.0.0.3:8765", Source::Discovered),
        Choice::new("USB", USB, Source::Usb),
    ]);
    conversation.navigate(0, 1);
    conversation.set_servers(vec![
        remembered(),
        Choice::new("Other", "10.0.0.3:8765", Source::Discovered),
        Choice::new("USB", USB, Source::Usb),
    ]);
    let start = Instant::now();
    conversation.tick(start);
    conversation.tick(start + AUTOMATIC_SETTLE);
    assert!(conversation.take_actions().is_empty());
    assert_eq!(conversation.mode(), Mode::Servers);
}

#[test]
fn selects_the_best_server_until_one_is_chosen() {
    let mut conversation = crate::test_support::conversation("Ada");
    conversation.set_servers(vec![Choice::new("USB", USB, Source::Usb)]);
    conversation.set_servers(vec![
        Choice::new("Studio", "10.0.0.2:8765", Source::Discovered),
        Choice::new("USB", USB, Source::Usb),
    ]);
    conversation.accept();
    assert!(matches!(
        conversation.take_actions().as_slice(),
        [Action::Connect(choice)] if choice.address == "10.0.0.2:8765"
    ));
}

/// More of what the conversation does as the connection and the server tell it things.
/// What arrives late or out of turn, and the rarer ways through the rings and the wrist.
mod corners {
    use super::*;

    /// Connected to a server offering two environments, so that creating a space asks which.
    fn offering_two(backends: Vec<BackendInfo>) -> Conversation {
        let mut conversation = in_lobby(backends.clone(), roots());
        conversation.apply(Event::State(ConnectionState::Connected {
            source: 1,
            server: "studio".into(),
            role: ConnectionRole::Owner,
            certificate: None,
            backends,
            roots: roots(),
            environments: vec![
                EnvironmentInfo {
                    id: "sky".into(),
                    label: "Sky".into(),
                    version: 1,
                },
                EnvironmentInfo {
                    id: "grid".into(),
                    label: "Grid".into(),
                    version: 1,
                },
            ],
            utterance_limit: None,
            name: None,
        }));
        conversation.take_actions();
        conversation
    }

    /// The ring shown and the keys selected on it.
    fn ring(conversation: &Conversation) -> Option<(arrival::Step, Vec<arrival::Key>)> {
        conversation.arrival().unwrap().ring.map(|ring| {
            (
                ring.step,
                ring.items
                    .into_iter()
                    .filter(|item| item.selected)
                    .map(|item| item.key)
                    .collect(),
            )
        })
    }

    #[test]
    fn a_connection_or_leave_arriving_after_leaving_the_server_keeps_the_server_list() {
        let mut conversation = in_lobby(vec![], vec![]);
        conversation.choose_object(&arrival::Key::Connected("10.0.0.1:8765".into()));
        assert_eq!(conversation.take_actions(), [Action::Disconnect]);
        conversation.apply(connected());
        assert!(sent(&mut conversation).is_empty());
        assert_eq!(conversation.mode(), Mode::Servers);
        conversation.apply(Event::Message(ServerMessage::LEFT));
        assert!(sent(&mut conversation).is_empty());
        assert_eq!(conversation.mode(), Mode::Servers);
    }

    #[test]
    fn a_join_refused_without_naming_the_space_is_no_longer_awaited() {
        let mut conversation = in_lobby(vec![], vec![]);
        listed(&mut conversation, vec![summary("one", "One")]);
        conversation.accept();
        assert_eq!(
            sent(&mut conversation),
            [ClientMessage::Join {
                space: "one".into()
            }]
        );
        conversation.apply(Event::Message(ServerMessage::Error {
            error: "The server is busy.".into(),
            space: None,
            panel: None,
            wall: None,
        }));
        assert_eq!(conversation.joining, None);
        assert_eq!(conversation.error.as_deref(), Some("The server is busy."));
        assert_eq!(conversation.mode(), Mode::Lobby);
    }

    #[test]
    fn someone_elses_transcript_changes_nothing_here() {
        let mut conversation = joined(|_| {});
        conversation.talk(true);
        conversation.take_actions();
        for finished in [false, true] {
            conversation.apply(Event::Message(ServerMessage::Transcript {
                utterance: "someone-else".into(),
                text: "deploy it".into(),
                finished,
            }));
        }
        let Some(crate::recording::Phase::Recording { heard, .. }) =
            conversation.recording_phase(Instant::now())
        else {
            unreachable!("still recording");
        };
        assert_eq!(heard, "");
        assert!(conversation.outcome.is_none());
    }

    #[test]
    fn a_jump_finding_nothing_needs_you_stays_in_the_space_the_server_rejoined() {
        let mut conversation = crate::test_support::conversation("Ada");
        conversation.set_servers(vec![advertising("Laptop", "10.0.0.2:8765", 0, 1)]);
        conversation.choose_object(&arrival::Key::Jump("10.0.0.2:8765".into()));
        conversation.apply(connected());
        conversation.apply(space(|_| {}));
        conversation.take_actions();
        conversation.notice = None;
        listed(&mut conversation, vec![summary(SPACE, "Tomori")]);
        assert!(sent(&mut conversation).is_empty());
        assert_eq!(conversation.mode(), Mode::Space);
        assert_eq!(conversation.notice, None);
    }

    #[test]
    fn stepping_back_from_the_environments_returns_to_the_folder_without_a_session_ring() {
        let mut conversation = offering_two(backends());
        listed(&mut conversation, vec![]);
        conversation.choose_object(&arrival::Key::Create);
        conversation.choose_object(&arrival::Key::Backend("plain".into()));
        conversation.choose_object(&arrival::Key::Directory("/work/tomori".into()));
        assert!(sent(&mut conversation).is_empty());
        assert_eq!(
            ring(&conversation).map(|(step, _)| step),
            Some(arrival::Step::Environment)
        );
        conversation.back();
        assert_eq!(
            ring(&conversation),
            Some((
                arrival::Step::Directory,
                vec![arrival::Key::Directory("/work/tomori".into())]
            ))
        );
        assert!(sent(&mut conversation).is_empty());
    }

    #[test]
    fn a_folder_ring_with_no_folders_does_nothing() {
        let mut conversation = in_lobby(backends(), vec![]);
        listed(&mut conversation, vec![]);
        conversation.choose_object(&arrival::Key::Create);
        conversation.choose_object(&arrival::Key::Backend("fake".into()));
        conversation.accept();
        assert!(sent(&mut conversation).is_empty());
        assert_eq!(
            ring(&conversation).map(|(step, _)| step),
            Some(arrival::Step::Directory)
        );
    }

    #[test]
    fn the_queue_is_selected_only_with_nothing_else_to_choose() {
        let waiting = |space: &mut SpaceState| {
            space.queue = vec![entry("q-1", "Ada", QueueState::Queued)];
        };
        assert!(joined(waiting).selects_queue());
        assert!(!joined(|_| {}).selects_queue());
        let conversation = joined(|space| {
            waiting(space);
            space.approval = Some(approval(&[]));
        });
        assert!(!conversation.selects_queue());
        let mut conversation = joined(|space| {
            waiting(space);
            space.speech = SpeechSetting::Brief;
        });
        conversation.space_menu = Some(0);
        assert!(!conversation.selects_queue());
    }

    #[test]
    fn deciding_on_the_wrist_refuses_a_for_a_question_and_leaves_it_to_the_wrist_otherwise() {
        let mut conversation = joined(|space| space.question = Some(question()));
        conversation.decide_on_wrist();
        assert!(!conversation.accept());
        assert_eq!(
            conversation.notice.as_deref(),
            Some("Turn your right wrist to decide.")
        );
        let text = conversation.text();
        assert!(text.ends_with("Turn your right wrist to decide"), "{text}");

        let mut conversation = joined(|_| {});
        conversation.decide_on_wrist();
        assert!(conversation.accept());
        assert!(sent(&mut conversation).is_empty());
    }

    #[test]
    fn a_sent_outcome_names_no_place_once_the_space_is_left_or_the_words_are_delivered() {
        let mut conversation = joined(|_| {});
        conversation.talk(true);
        conversation.take_actions();
        let utterance = conversation.talk.as_ref().unwrap().utterance().to_owned();
        conversation.talk(false);
        conversation.apply(Event::Message(ServerMessage::Transcript {
            utterance,
            text: "text of q-1".into(),
            finished: true,
        }));
        // Delivered, the words wait nowhere.
        conversation.apply(space(|space| {
            space.queue = vec![entry("q-1", "Ada", QueueState::Delivered)];
        }));
        let sent_nowhere = Some(crate::recording::Phase::Sent {
            answered: false,
            position: None,
        });
        assert_eq!(conversation.recording_phase(Instant::now()), sent_nowhere);
        // Nor once the space is left.
        hold_b(&mut conversation);
        conversation.apply(Event::Message(ServerMessage::LEFT));
        assert_eq!(conversation.mode(), Mode::Lobby);
        assert_eq!(conversation.recording_phase(Instant::now()), sent_nowhere);
    }

    #[test]
    fn choosing_what_is_not_there_does_nothing() {
        let mut conversation = crate::test_support::conversation("Ada");
        conversation.set_servers(vec![advertising("Laptop", "10.0.0.2:8765", 0, 0)]);
        conversation.choose_object(&arrival::Key::Server("10.0.0.9:8765".into()));
        assert!(conversation.take_actions().is_empty());
        assert_eq!(conversation.mode(), Mode::Servers);

        let mut conversation = in_lobby(vec![], vec![]);
        listed(&mut conversation, vec![summary("one", "One")]);
        conversation.choose_object(&arrival::Key::Space("gone".into()));
        assert!(sent(&mut conversation).is_empty());

        let mut conversation = joined(|_| {});
        conversation.choose_object(&arrival::Key::Create);
        assert!(sent(&mut conversation).is_empty());
        assert_eq!(conversation.mode(), Mode::Space);
    }

    #[test]
    fn a_list_entry_that_is_not_there_has_no_description() {
        let mut conversation = joined(|space| space.speech = SpeechSetting::Brief);
        conversation.space_menu = Some(1);
        let text = conversation.text();
        assert!(text.contains("MODE AND SPEECH 2/1"), "{text}");
        assert!(
            text.contains("Speech: brief") && !text.contains("> "),
            "{text}"
        );
        assert!(!text.contains("Replies"), "{text}");
    }

    #[test]
    fn an_idle_space_whose_words_were_all_delivered_is_ready() {
        let conversation = joined(|space| {
            space.queue = vec![entry("q-1", "Bob", QueueState::Delivered)];
        });
        assert_eq!(conversation.activity(), SessionActivity::Ready);
    }

    #[test]
    fn a_problem_not_yet_shown_has_no_age() {
        let mut conversation = joined(|space| {
            space.messages = vec![message("m-1", Role::Assistant, "Done.")];
        });
        conversation.error = Some("Something broke.".into());
        assert_eq!(
            conversation.problem_lines(Instant::now()),
            ["! 0s Something broke."]
        );
    }
}

mod events {
    use super::*;
    use crate::scene::fixtures::{joined, lobby, space};

    /// The utterance a press of X began.
    fn talking(conversation: &mut Conversation) -> String {
        conversation.talk(true);
        conversation.take_actions();
        conversation.talk.as_ref().unwrap().utterance().to_owned()
    }

    fn failed(utterance: &str, failure: tomori_client::UtteranceFailure) -> Event {
        Event::UtteranceFailed(tomori_client::FailedUtterance {
            utterance: utterance.into(),
            failure,
        })
    }

    fn disconnected() -> Event {
        Event::State(ConnectionState::Disconnected {
            reason: tomori_client::Disconnect::Timeout,
            retry_in: Duration::from_secs(1),
        })
    }

    #[test]
    fn words_that_could_not_be_sent_say_why_and_others_words_change_nothing() {
        for (failure, said) in [
            (tomori_client::UtteranceFailure::Disconnected, LOST),
            (
                tomori_client::UtteranceFailure::Congested,
                "Network too slow; your words were not sent.",
            ),
        ] {
            let mut conversation = joined();
            let utterance = talking(&mut conversation);
            conversation.apply(failed("someone-else", failure));
            assert!(conversation.recording().is_some());
            conversation.apply(failed(&utterance, failure));
            assert!(conversation.recording().is_none());
            assert_eq!(conversation.error.as_deref(), Some(said));
        }
    }

    #[test]
    fn words_on_their_way_when_the_connection_drops_are_said_to_be_lost() {
        let mut conversation = joined();
        talking(&mut conversation);
        conversation.talk(false);
        conversation.apply(disconnected());
        assert!(conversation.talk.is_none());
        assert_eq!(
            conversation.notice.as_deref(),
            Some("Connection lost while sending your words.")
        );
    }

    #[test]
    fn an_admission_or_pairing_without_a_certificate_keeps_no_token() {
        let mut conversation = joined();
        conversation.knock = Some(tomori_protocol::KnockState::Pending);
        conversation.apply(Event::Message(ServerMessage::Admitted {
            token: "tomori_t".into(),
        }));
        conversation.apply(Event::Message(ServerMessage::Paired {
            token: "tomori_t".into(),
        }));
        assert!(conversation.take_actions().is_empty());
        assert_eq!(conversation.knock, None);
    }

    #[test]
    fn sessions_that_come_too_late_and_a_requested_leave_are_handled() {
        let mut conversation = lobby(ConnectionRole::Owner);
        conversation.apply(Event::Message(ServerMessage::Sessions { sessions: vec![] }));
        assert_eq!(
            conversation.error.as_deref(),
            Some("Too late to show those sessions.")
        );
        let mut conversation = joined();
        conversation.leaving = true;
        conversation.apply(Event::Message(ServerMessage::LEFT));
        assert_eq!(conversation.mode, Mode::Lobby);
        assert!(conversation.take_actions().is_empty());
    }

    #[test]
    fn a_join_refused_for_a_space_nobody_listed_says_the_space_ended() {
        let mut conversation = lobby(ConnectionRole::Owner);
        conversation.join_space("vanished");
        conversation.apply(Event::Message(ServerMessage::Error {
            error: "No such space.".into(),
            space: Some("vanished".into()),
            panel: None,
            wall: None,
        }));
        assert_eq!(conversation.ended.as_deref(), Some("The space has ended."));
    }

    #[test]
    fn a_partial_transcript_updates_what_was_heard_and_an_answer_is_marked_answered() {
        let mut conversation = joined();
        let utterance = talking(&mut conversation);
        conversation.apply(Event::Message(ServerMessage::Transcript {
            utterance: utterance.clone(),
            text: "fix".into(),
            finished: false,
        }));
        let phase = conversation.recording_phase(Instant::now()).unwrap();
        assert!(
            crate::recording::canvas(&phase, None, 0.0)
                .plain()
                .contains("\"fix\"")
        );
        conversation.apply(Event::Message(ServerMessage::Transcript {
            utterance,
            text: "fix it".into(),
            finished: true,
        }));
        assert_eq!(
            conversation.outcome.map(|(outcome, _)| outcome),
            Some(Outcome::Sent {
                answered: false,
                text: "fix it".into()
            })
        );
    }

    #[test]
    fn a_spaces_list_is_ignored_while_only_pairing() {
        let mut conversation = lobby(ConnectionRole::Pairing);
        conversation.apply(Event::Message(ServerMessage::Spaces {
            spaces: vec![tomori_protocol::SpaceSummary {
                id: "s1".into(),
                title: "Studio".into(),
                backend: "fake".into(),
                directory: "/tmp".into(),
                environment: String::new(),
                activity: tomori_protocol::Activity::Idle,
                participants: 0,
                attention: None,
                attention_age: None,
                folder: None,
                active_age: None,
                latest: None,
            }],
        }));
        assert!(conversation.spaces.is_empty());
    }

    #[test]
    fn agent_speech_is_logged_rather_than_shown() {
        crate::test_support::log_everything();
        let mut conversation = joined();
        conversation.apply(Event::Message(ServerMessage::Speech {
            segment: 1,
            message: "m1".into(),
            text: "Done.".into(),
            sample_rate: 24_000,
            replay: false,
        }));
        assert_eq!(conversation.notice, None);
        conversation.apply(space(|state| state.title = "Renamed".into()));
        assert_eq!(conversation.space.as_ref().unwrap().title, "Renamed");
    }

    fn summary(
        id: &str,
        attention: Option<tomori_protocol::Attention>,
    ) -> tomori_protocol::SpaceSummary {
        tomori_protocol::SpaceSummary {
            id: id.into(),
            title: "Deploy".into(),
            backend: "fake".into(),
            directory: "/tmp".into(),
            environment: String::new(),
            activity: tomori_protocol::Activity::Idle,
            participants: 0,
            attention,
            attention_age: None,
            folder: None,
            active_age: None,
            latest: None,
        }
    }

    #[test]
    fn the_lobby_says_what_the_most_urgent_space_needs() {
        use tomori_protocol::Attention;
        for (attention, said) in [
            (Attention::Problem, "Deploy has a problem"),
            (Attention::Reply, "Deploy has a new reply"),
            (Attention::Unknown, "Deploy needs attention"),
        ] {
            let mut conversation = lobby(ConnectionRole::Owner);
            conversation.apply(Event::Message(ServerMessage::Spaces {
                spaces: vec![summary("s1", Some(attention))],
            }));
            assert_eq!(conversation.attention_status().as_deref(), Some(said));
        }
    }

    #[test]
    fn hearing_a_reply_again_or_pairing_needs_a_space_or_a_connection() {
        let mut conversation = lobby(ConnectionRole::Owner);
        conversation.replay();
        assert_eq!(
            conversation.notice.as_deref(),
            Some("Join a space to hear a reply again.")
        );
        let mut conversation = joined();
        conversation.certificate = Some(tomori_security::Fingerprint::of(b"server"));
        conversation.role = ConnectionRole::Visitor;
        assert!(conversation.pairable());
        // Pairing in a space keeps the problem shown there.
        conversation.error = Some("A problem.".into());
        conversation.pair();
        assert_eq!(conversation.error.as_deref(), Some("A problem."));
        conversation.apply(disconnected());
        assert!(!conversation.pairable());
    }

    #[test]
    fn the_menu_names_the_mode_and_its_choices_act() {
        use crate::hand_menu::Choice;
        let mut conversation = lobby(ConnectionRole::Owner);
        conversation.apply(space(|state| {
            state.modes = vec![
                tomori_protocol::Mode {
                    id: "build".into(),
                    name: "Build".into(),
                    description: Some("Writes code".into()),
                },
                tomori_protocol::Mode {
                    id: "plan".into(),
                    name: "Plan".into(),
                    description: None,
                },
            ];
            state.mode = Some("build".into());
        }));
        let shown = conversation.menu_model().space.unwrap();
        assert_eq!(shown.mode.as_deref(), Some("Build"));
        assert_eq!(shown.modes[0].2.as_deref(), Some("Writes code"));
        conversation.apply(space(|state| {
            state.modes = vec![tomori_protocol::Mode {
                id: "plan".into(),
                name: "Plan".into(),
                description: None,
            }];
            state.mode = Some("custom".into());
        }));
        assert_eq!(
            conversation.menu_model().space.unwrap().mode.as_deref(),
            Some("custom")
        );
        for choice in [
            Choice::Knock,
            Choice::Admit("k1".into()),
            Choice::Decline("k1".into()),
            Choice::Pair,
            Choice::ClearInstructions,
            Choice::Mono,
        ] {
            conversation.choose_menu(choice);
        }
        assert!(conversation.feedback.mono);
        // Outside a space, speech and leaving do nothing.
        let mut conversation = lobby(ConnectionRole::Owner);
        conversation.choose_menu(Choice::Speech);
        conversation.choose_menu(Choice::Leave);
        assert!(conversation.take_actions().is_empty());
        conversation.instruct("");
        assert_eq!(
            conversation.notice.as_deref(),
            Some("The next space you create has no instructions of its own.")
        );
    }

    #[test]
    fn a_request_in_a_space_with_messages_focuses_the_wrist_on_it() {
        let mut conversation = joined();
        conversation.apply(space(|state| {
            state.messages = vec![tomori_protocol::Message {
                id: "m1".into(),
                role: tomori_protocol::Role::Assistant,
                text: "Running.".into(),
                completed: true,
            }];
            state.approval = Some(Approval {
                id: "p1".into(),
                title: "Bash".into(),
                text: "make".into(),
                options: Vec::new(),
            });
        }));
        assert_eq!(
            conversation.wrist.focus,
            Some(crate::transcript::Focus::Card(0))
        );
    }

    #[test]
    fn the_space_menu_stays_on_an_entry_when_the_list_shrinks() {
        let mut conversation = lobby(ConnectionRole::Owner);
        let modes = |count: usize| {
            (0..count)
                .map(|n| tomori_protocol::Mode {
                    id: format!("mode-{n}"),
                    name: format!("Mode {n}"),
                    description: None,
                })
                .collect::<Vec<_>>()
        };
        conversation.apply(space(|state| {
            state.modes = modes(3);
            state.mode = Some("mode-2".into());
        }));
        conversation.refresh();
        assert_eq!(conversation.space_menu, Some(2));
        conversation.apply(space(|state| state.modes = modes(1)));
        assert_eq!(conversation.space_menu, Some(0));
    }

    #[test]
    fn engagement_is_not_reported_while_disconnected() {
        let mut conversation = lobby(ConnectionRole::Owner);
        conversation.apply(space(|state| state.speech = SpeechSetting::Full));
        conversation.take_actions();
        conversation.apply(disconnected());
        conversation.set_engagement(tomori_protocol::Engagement::Away);
        assert!(conversation.take_actions().is_empty());
    }

    #[test]
    fn rings_and_lists_with_nothing_chosen_do_nothing() {
        let mut conversation = lobby(ConnectionRole::Owner);
        // Back on the space list, and creating with nothing chosen, do nothing.
        conversation.back_ring();
        conversation.create(None);
        // A on the backend ring with no backends, and on the environment ring with nothing to create, does nothing.
        conversation.step = LobbyStep::Backend;
        conversation.accept();
        assert_eq!(conversation.step, LobbyStep::Backend);
        conversation.step = LobbyStep::Environment;
        conversation.accept();
        // Back from the environment ring, for a backend that does not attach, is the folder ring.
        conversation.backends = backends();
        conversation.menu_backend = Some("plain".into());
        conversation.step = LobbyStep::Environment;
        conversation.back_ring();
        assert_eq!(conversation.step, LobbyStep::Directory);
        // A visitor's A on the space list's end opens nothing.
        let mut conversation = lobby(ConnectionRole::Visitor);
        conversation.accept();
        assert_eq!(conversation.step, LobbyStep::Spaces);
        // Without a space, its list and the stick's queue have nothing.
        assert!(conversation.menu_items().is_empty());
        assert!(conversation.wrist_items().is_empty());
        conversation.select_queued(1);
        conversation.mode = Mode::Space;
        conversation.navigate(0, 1);
        assert_eq!(conversation.space_menu, None);
        conversation.decide();
        assert!(conversation.take_actions().is_empty());
    }

    #[test]
    fn a_choice_made_elsewhere_is_refused_or_left_alone() {
        // Deciding on the wrist: A in front does not decide.
        let mut conversation = crate::scene::fixtures::approval_waiting();
        conversation.request_at = None;
        conversation.decide_on_wrist = true;
        assert!(!conversation.accept());
        assert_eq!(
            conversation.notice.as_deref(),
            Some("Turn your right wrist to decide.")
        );
        // A visitor cannot cancel a queued entry.
        let mut conversation = joined();
        conversation.role = ConnectionRole::Visitor;
        conversation.queue_selected = Some("q1".into());
        assert!(!conversation.accept());
        assert_eq!(conversation.notice.as_deref(), Some(VISITING));
        // A selected entry that is no longer waiting is no longer selected.
        let mut conversation = joined();
        conversation.queue_selected = Some("gone".into());
        conversation.tick_space(Instant::now());
        assert_eq!(conversation.queue_selected, None);
        // The space list's entry that is not there does nothing.
        let mut conversation = joined();
        conversation.space_menu = Some(9);
        conversation.accept();
        assert!(conversation.take_actions().is_empty());
    }

    #[test]
    fn the_wrist_refuses_a_visitor_and_removes_only_waiting_entries() {
        use crate::transcript::Focus;
        // A visitor is offered no choice to focus.
        let mut conversation = crate::scene::fixtures::approval_waiting();
        conversation.request_at = None;
        conversation.role = ConnectionRole::Visitor;
        conversation.wrist.focus = Some(Focus::Card(1));
        assert!(!conversation.wrist_accept());
        assert!(conversation.take_actions().is_empty());
        // A question's buttons are not an approval's: A decides nothing.
        let mut conversation = joined();
        conversation.apply(space(|state| {
            state.question = Some(Question {
                id: "q1".into(),
                title: "Which?".into(),
                text: "Which one?".into(),
                field: "Choice".into(),
                supported: true,
            });
            state.queue = vec![
                entry("e1", "Ada", QueueState::Queued),
                entry("e2", "Ada", QueueState::Queued),
            ];
        }));
        conversation.request_at = None;
        conversation.wrist.focus = Some(Focus::Card(1));
        assert!(!conversation.wrist_accept());
        // Removing one entry while another is being removed removes the other at once.
        conversation.cancelling = Some(("e1".into(), Instant::now() + CANCEL_UNDO));
        conversation.wrist.focus = Some(Focus::Queue("e2".into()));
        assert!(conversation.wrist_accept());
        // An entry that is not waiting is not removed.
        conversation.cancelling = None;
        conversation.wrist.focus = Some(Focus::Queue("e9".into()));
        assert!(!conversation.wrist_accept());
        // B dismisses a problem shown.
        conversation.wrist.focus = None;
        conversation.error = Some("A problem.".into());
        assert!(conversation.wrist_back());
        assert_eq!(conversation.error, None);
    }

    /// Each state a connection can be in other than connected, as the pill and the lobby say them.
    fn unconnected() -> [(ConnectionState, &'static str, &'static str); 4] {
        [
            (
                ConnectionState::Connecting { attempt: 1 },
                "Connecting...",
                "Connecting...",
            ),
            (
                ConnectionState::Connecting { attempt: 3 },
                "Connecting (attempt 3)...",
                "Connecting (attempt 3)...",
            ),
            (
                ConnectionState::Disconnected {
                    reason: tomori_client::Disconnect::Timeout,
                    retry_in: Duration::from_secs(2),
                },
                "Offline, retrying in 2 s",
                "Offline, retry in 2s: ",
            ),
            (ConnectionState::Closed, "Disconnected.", "Disconnected."),
        ]
    }

    #[test]
    fn the_pill_and_the_lobby_say_how_the_connection_is() {
        use crate::pill::Need;
        for (state, pill, status) in unconnected() {
            let mut conversation = joined();
            conversation.apply(Event::State(state.clone()));
            assert_eq!(
                conversation.pill_status(Instant::now()).unwrap().need,
                Some(Need::Connection(pill.into()))
            );
            let mut conversation = lobby(ConnectionRole::Owner);
            conversation.apply(Event::State(state));
            assert!(conversation.status().unwrap().starts_with(status));
        }
        // A question needs the participant before the connection does.
        let mut conversation = joined();
        conversation.apply(space(|state| {
            state.question = Some(Question {
                id: "q1".into(),
                title: "Which?".into(),
                text: "Which one?".into(),
                field: "Choice".into(),
                supported: true,
            });
            state.modes = vec![tomori_protocol::Mode {
                id: "build".into(),
                name: "Build".into(),
                description: None,
            }];
            state.mode = Some("custom".into());
        }));
        let status = conversation.pill_status(Instant::now()).unwrap();
        assert_eq!(status.need, Some(Need::Question));
        assert_eq!(status.mode.as_deref(), Some("custom"));
    }

    #[test]
    fn the_pill_says_how_much_of_a_long_frame_has_arrived_until_all_of_it_has() {
        use crate::pill::Need;
        let need = |conversation: &mut Conversation| {
            conversation.pill_status(Instant::now()).unwrap().need
        };
        let mut conversation = joined();
        assert_eq!(need(&mut conversation), None);
        let (received, length) = (1_800_000, 4_000_000);
        conversation.apply(Event::Receiving { received, length });
        assert_eq!(conversation.receiving(), Some((received, length)));
        assert_eq!(
            need(&mut conversation),
            Some(Need::Connection("Syncing space... 45%".into()))
        );
        // All of it arrived, it says nothing more.
        conversation.apply(Event::Receiving {
            received: length,
            length,
        });
        assert_eq!(conversation.receiving(), None);
        assert_eq!(need(&mut conversation), None);
        // Lost part way through, it says how the connection is instead, and nothing of syncing once connected again.
        conversation.apply(Event::Receiving {
            received: 1,
            length,
        });
        conversation.apply(Event::State(ConnectionState::Connecting { attempt: 1 }));
        assert_eq!(conversation.receiving(), None);
        assert_eq!(
            need(&mut conversation),
            Some(Need::Connection("Connecting...".into()))
        );
    }

    #[test]
    fn the_headset_states_today_s_panels_and_the_desktop_window_twice_them_both_with_sixteen_declared_walls_and_the_longest_frames()
     {
        use tomori_client::connection::Session;
        use tomori_protocol::{Limits, MOST_FRAME_BYTES};
        let longest = Limits::TODAY.with_frame_bytes(MOST_FRAME_BYTES as u32);
        let mut conversation = joined();
        // The Tomori wall, sixty-four of an environment's own and six or twelve added, six slots each; every panel an
        // environment may publish beside the Tomori wall's and those floating; and sixteen volumes of 1,024 nodes
        // together.
        assert_eq!(
            HEADSET_LIMITS,
            longest
                .with_projection(71, 426, 12, 180)
                .with_volumes(16, 1024)
        );
        assert!(
            HEADSET_LIMITS.panels as usize
                >= tomori_environment::PANEL_LIMIT
                    + (tomori_protocol::WALL_SLOTS + tomori_protocol::PROJECTION_FLOATING) as usize
        );
        assert_eq!(Session::limits(&conversation), HEADSET_LIMITS);
        assert_eq!(
            DESKTOP_LIMITS,
            longest
                .with_projection(77, 462, 24, 360)
                .with_volumes(16, 1024)
        );
        conversation.set_limits(DESKTOP_LIMITS);
        assert_eq!(Session::limits(&conversation), DESKTOP_LIMITS);
    }

    #[test]
    fn b_and_y_do_nothing_at_the_servers_and_dismiss_or_refetch_in_a_space() {
        let mut conversation = crate::test_support::conversation("Ada");
        conversation.back();
        assert_eq!(conversation.mode, Mode::Servers);
        let mut conversation = joined();
        conversation.apply(space(|state| {
            state.error = Some("The agent failed.".into())
        }));
        conversation.back();
        assert_eq!(conversation.dismissed.as_deref(), Some("The agent failed."));
        // Y on the session ring without its backend and folder asks for nothing.
        let mut conversation = lobby(ConnectionRole::Owner);
        conversation.step = LobbyStep::Session;
        conversation.refresh();
        assert!(conversation.take_actions().is_empty());
        // Y in a space whose state has not come does nothing.
        conversation.mode = Mode::Space;
        conversation.refresh();
        assert!(conversation.take_actions().is_empty());
    }

    #[test]
    fn live_audio_needs_a_space_and_a_connection() {
        let mut conversation = lobby(ConnectionRole::Owner);
        conversation.toggle_live();
        assert_eq!(
            conversation.notice.as_deref(),
            Some("Join a space to use live audio.")
        );
        let mut conversation = joined();
        conversation.apply(disconnected());
        conversation.toggle_live();
        assert_eq!(
            conversation.error.as_deref(),
            Some("Offline; cannot use live audio.")
        );
    }

    #[test]
    fn the_recording_says_what_it_is_about_and_what_it_answers() {
        let mut conversation = joined();
        conversation.apply(space(|state| {
            state.workers = vec![tomori_protocol::Worker {
                id: "w1".into(),
                parent: None,
                title: "Parser".into(),
                state: tomori_protocol::WorkerState::Running,
            }];
        }));
        conversation.point_at(Some("w1"));
        talking(&mut conversation);
        let phase = conversation.recording_phase(Instant::now()).unwrap();
        assert!(
            crate::recording::canvas(&phase, None, 0.0)
                .plain()
                .contains("Parser")
        );
        conversation.talk(false);
        let phase = conversation.recording_phase(Instant::now()).unwrap();
        assert_eq!(
            phase,
            crate::recording::Phase::Sending {
                heard: String::new(),
                answering: false,
                received: None,
            }
        );
        // An answer names the question it answers.
        let mut conversation = joined();
        conversation.apply(space(|state| {
            state.voice_answers = true;
            state.question = Some(Question {
                id: "q1".into(),
                title: "Which?".into(),
                text: "Which one?".into(),
                field: "Choice".into(),
                supported: true,
            });
        }));
        talking(&mut conversation);
        let phase = conversation.recording_phase(Instant::now()).unwrap();
        assert!(
            crate::recording::canvas(&phase, None, 0.0)
                .plain()
                .contains("Which?")
        );
    }

    #[test]
    fn a_decision_on_its_way_or_words_waiting_keep_the_agent_working() {
        let mut conversation = crate::scene::fixtures::approval_waiting();
        conversation.decided = Some("request-1".into());
        assert_eq!(conversation.activity(), SessionActivity::Working);
        let mut conversation = joined();
        conversation.apply(space(|state| {
            state.activity = Activity::Idle;
            state.queue = vec![entry("e1", "Bob", QueueState::Queued)];
        }));
        assert_eq!(conversation.activity(), SessionActivity::Working);
    }

    #[test]
    fn the_lobby_says_it_is_joining_or_admits_only_paired_devices() {
        let mut conversation = lobby(ConnectionRole::Owner);
        conversation.listed = true;
        conversation.join_space("s1");
        assert!(conversation.text().contains("Joining..."));
        assert_eq!(
            conversation.arrival().unwrap().status.as_deref(),
            Some("Joining...")
        );
        let mut conversation = lobby(ConnectionRole::Pairing);
        let text = conversation.text();
        assert!(text.contains("This server admits only paired device"));
    }

    #[test]
    fn empty_rings_say_what_is_missing_and_a_problem_is_dismissed_first() {
        let mut conversation = lobby(ConnectionRole::Owner);
        conversation.step = LobbyStep::Backend;
        let text = conversation.text();
        assert!(text.contains("The server has no backends"));
        conversation.step = LobbyStep::Directory;
        assert!(conversation.text().contains("The server offers no folders"));
        conversation.error = Some("A problem.".into());
        assert!(
            conversation
                .text()
                .ends_with("A choose | B dismiss | Y refresh")
        );
        // A session without a title is listed by its identifier.
        conversation.error = None;
        conversation.step = LobbyStep::Session;
        conversation.sessions = vec![tomori_protocol::SessionSummary {
            id: "ses_1".into(),
            title: "  ".into(),
            directory: "/tmp".into(),
            busy: false,
        }];
        assert!(conversation.text().contains("ses_1"));
    }

    #[test]
    fn the_space_panel_shows_each_state_it_can_be_in() {
        // Its state not yet come.
        let mut conversation = joined();
        conversation.space = None;
        assert_eq!(conversation.body(), ("Loading".into(), String::new()));
        let text = conversation.text();
        assert!(text.starts_with("SPACE\nLoading..."));
        // Answering out loud, connecting again, and leaving.
        let mut conversation = joined();
        conversation.apply(space(|state| {
            state.voice_answers = true;
            state.question = Some(Question {
                id: "q1".into(),
                title: "Which?".into(),
                text: "Which one?".into(),
                field: "Choice".into(),
                supported: true,
            });
        }));
        let utterance = talking(&mut conversation);
        conversation.apply(Event::Message(ServerMessage::Transcript {
            utterance,
            text: "the first".into(),
            finished: false,
        }));
        assert!(conversation.text().contains("MIC ANSWER: the first"));
        let mut conversation = joined();
        for (state, said) in [
            (
                ConnectionState::Connecting { attempt: 3 },
                "Connecting (attempt 3)...",
            ),
            (ConnectionState::Closed, "Disconnected."),
        ] {
            conversation.apply(Event::State(state));
            assert!(conversation.text().contains(said));
        }
        let mut conversation = joined();
        conversation.leave_hold = Some((Duration::from_millis(300), false));
        assert!(conversation.text().contains("Leaving: keep holding B..."));
        // The speech setting's entry, said in full.
        let mut conversation = joined();
        conversation.apply(space(|state| state.speech = SpeechSetting::Full));
        conversation.refresh();
        let text = conversation.text();
        assert!(text.contains("Speech: full"));
        assert!(text.contains("Replies are spoken in full."));
    }

    #[test]
    fn how_long_ago_is_said_in_seconds_minutes_or_hours_and_queue_states_are_named() {
        assert_eq!(age(Duration::from_secs(5)), "5s");
        assert_eq!(age(Duration::from_secs(120)), "2m");
        assert_eq!(age(Duration::from_secs(7_200)), "2h");
        assert_eq!(queue_label(QueueState::Delivered), "sent");
        assert_eq!(queue_label(QueueState::Failed), "failed");
        assert_eq!(activity_label(Activity::Closed), "closed");
    }
}

#[test]
fn as_its_connections_session_it_applies_what_the_connection_reports() {
    crate::test_support::log_everything();
    // The server given at startup is connected to as though it were chosen.
    let mut conversation = crate::test_support::conversation("Ada");
    assert!(Session::is_browsing(&conversation));
    let given = Choice::new("10.0.0.1:8765", "10.0.0.1:8765", Source::Override);
    Session::report(&mut conversation, Report::Connecting(given.clone()));
    assert_eq!(conversation.server(), Some(&given));
    assert_eq!(
        Session::next_action(&mut conversation),
        Some(Action::Connect(given))
    );
    assert_eq!(Session::next_action(&mut conversation), None);
    // The device's setting is shown, and what fails and what concerns security is said.
    let quiet = feedback::Settings {
        sounds: feedback::Level::Off,
        ..Default::default()
    };
    Session::report(&mut conversation, Report::Feedback(quiet));
    assert_eq!(conversation.menu_model().feedback, quiet);
    Session::report(&mut conversation, Report::Failed("Not connected.".into()));
    assert_eq!(conversation.problem().as_deref(), Some("Not connected."));
    Session::report(&mut conversation, Report::SecurityNotice("Changed.".into()));
    assert_eq!(
        conversation.menu_model().notice.as_deref(),
        Some("Changed.")
    );
    // In a space, the microphone records for the connection reported, and stops when focus is lost.
    let mut conversation = joined(|_| {});
    Session::apply(&mut conversation, role(ConnectionRole::Owner));
    Session::report(&mut conversation, Report::Generation(7));
    conversation.toggle_live();
    assert_eq!(conversation.take_actions(), [Action::StartLive(7)]);
    Session::report(&mut conversation, Report::Unfocused);
    assert_eq!(conversation.take_actions(), [Action::StopLive]);
    // A microphone that fails, or words that cannot be sent, are said.
    conversation.talk(true);
    conversation.take_actions();
    Session::report(
        &mut conversation,
        Report::MicrophoneFailed("Overrun.".into()),
    );
    assert_eq!(conversation.problem().as_deref(), Some("Overrun."));
    conversation.talk(true);
    conversation.talk(false);
    conversation.take_actions();
    Session::report(
        &mut conversation,
        Report::NotSent("The last read failed.".into()),
    );
    assert_eq!(
        conversation.problem().as_deref(),
        Some("The last read failed.")
    );
    // What plays again is shown.
    Session::report(&mut conversation, Report::Replaying(true));
    assert!(conversation.text().contains("Replaying"));
    // Agent speech playing on this device is kept for the swarm, until it stops.
    assert_eq!(conversation.speech(), SpeechActivity::default());
    Session::report(
        &mut conversation,
        Report::AgentSpeech {
            playing: true,
            level: 0.25,
        },
    );
    assert_eq!(
        conversation.speech(),
        SpeechActivity {
            playing: true,
            level: 0.25,
            ..SpeechActivity::default()
        }
    );
    Session::report(
        &mut conversation,
        Report::AgentSpeech {
            playing: false,
            level: 0.0,
        },
    );
    assert!(!conversation.speech().playing);
    // At the server list, the remembered server is connected to once it has been listed a while, but only while the list is seen.
    let start = Instant::now();
    let mut conversation = crate::test_support::conversation("Ada");
    let mut remembered = advertising("Desktop", "10.0.0.1:8765", 0, 0);
    remembered.source = Source::Remembered;
    Session::report(&mut conversation, Report::Servers(vec![remembered.clone()]));
    let tick = |now, browsing| Report::Tick { now, browsing };
    Session::report(&mut conversation, tick(start, true));
    Session::report(&mut conversation, tick(start + AUTOMATIC_SETTLE, false));
    assert!(conversation.take_actions().is_empty());
    Session::report(&mut conversation, tick(start + AUTOMATIC_SETTLE, true));
    assert_eq!(conversation.take_actions(), [Action::Connect(remembered)]);
}

#[test]
fn as_its_connections_session_it_says_what_waits_for_the_participant() {
    use tomori_client::playback::Earcon;
    let observed = |conversation: &Conversation| {
        let mut feedback = feedback::Feedback::new(feedback::Settings::default());
        feedback.set_haptics(true);
        let now = Instant::now();
        Session::observe(conversation, &mut feedback, now);
        let earcons: Vec<_> = feedback.take_earcons().map(|(earcon, _)| earcon).collect();
        (
            earcons,
            feedback.haptics(now + Duration::from_secs(1)).len(),
        )
    };
    let waiting = |state: &mut SpaceState| {
        state.approval = Some(Approval {
            id: "request-1".into(),
            title: "Bash".into(),
            text: "cargo test".into(),
            options: vec![],
        });
    };
    // A question waits.
    let asked = joined(|state| {
        state.question = Some(Question {
            id: "question-1".into(),
            title: "Which?".into(),
            text: "Which parser?".into(),
            field: "which".into(),
            supported: true,
        });
    });
    assert_eq!(observed(&asked).0, [Earcon::Request]);
    // An approval and a knock wait for an owner.
    let mut conversation = joined(waiting);
    conversation.apply(Event::Message(ServerMessage::Knocks {
        knocks: vec![tomori_protocol::KnockRequest {
            id: "knock-1".into(),
            name: "Grace".into(),
        }],
    }));
    let (heard, _) = observed(&conversation);
    assert_eq!(heard, [Earcon::Request, Earcon::Knock]);
    // Nothing waits for a visitor, who cannot decide or let anyone in.
    let mut visiting = joined(waiting);
    visiting.apply(role(ConnectionRole::Visitor));
    assert_eq!(observed(&visiting), (vec![], 0));
    // A recording ten seconds from the server's limit is felt.
    let mut conversation = crate::test_support::conversation("Ada");
    conversation.connect(Choice::new("Studio", "10.0.0.1:8765", Source::Discovered));
    conversation.apply(Event::State(ConnectionState::Connected {
        source: 1,
        server: "studio".into(),
        role: ConnectionRole::Owner,
        certificate: None,
        backends: vec![],
        roots: vec![],
        environments: sky(),
        utterance_limit: Some(Duration::from_secs(10)),
        name: None,
    }));
    conversation.apply(space(|_| {}));
    conversation.talk(true);
    assert_eq!(observed(&conversation), (vec![], 2));
}

#[test]
fn the_joined_space_s_environment_state_waits_for_its_client_side_which_asks_only_once_it_has_some()
{
    let mut conversation = joined(|state| {
        state.environment = "garden".into();
        state.environment_version = Some(1);
    });
    let payload = |value: u32| tomori_protocol::Payload::new(serde_json::json!(value));
    let state = |environment: &str, version, value| {
        Event::Message(ServerMessage::EnvironmentState {
            environment: environment.into(),
            version,
            state: payload(value),
        })
    };
    let event = |value| {
        Event::Message(ServerMessage::EnvironmentEvent {
            environment: "garden".into(),
            version: 1,
            event: payload(value),
        })
    };
    // Before the server has sent any state, it is not asked anything: it may not run the environment's side.
    assert!(!conversation.environment_intent("garden", 1, payload(0)));
    assert!(conversation.take_actions().is_empty());
    // Another environment's state, or another version's, is not the space's.
    conversation.apply(state("sky", 1, 1));
    conversation.apply(state("garden", 2, 1));
    assert!(conversation.environment_updates("garden", 1).is_empty());
    conversation.apply(state("garden", 1, 1));
    conversation.apply(event(2));
    assert!(conversation.environment_updates("garden", 2).is_empty());
    assert_eq!(
        conversation.environment_updates("garden", 1),
        [
            crate::channel::Update::State(payload(1)),
            crate::channel::Update::Event(payload(2))
        ]
    );
    assert!(conversation.environment_intent("garden", 1, payload(3)));
    assert_eq!(
        conversation.take_actions(),
        [Action::Send(ClientMessage::EnvironmentIntent {
            environment: "garden".into(),
            version: 1,
            intent: payload(3),
        })]
    );
    assert!(!conversation.environment_intent("garden", 2, payload(3)));
    // The same space's state again, in the same environment, keeps what was sent.
    conversation.apply(event(7));
    conversation.apply(space(|state| {
        state.environment = "garden".into();
        state.environment_version = Some(1);
    }));
    assert_eq!(
        conversation.environment_updates("garden", 1),
        [crate::channel::Update::Event(payload(7))]
    );
    // Moved to another environment and back, the space waits for its new binding's state, and asks nothing until then.
    conversation.apply(space(|state| state.environment = "sky".into()));
    conversation.apply(space(|state| {
        state.environment = "garden".into();
        state.environment_version = Some(1);
    }));
    conversation.apply(event(8));
    assert!(conversation.environment_updates("garden", 1).is_empty());
    assert!(!conversation.environment_intent("garden", 1, payload(3)));
    conversation.apply(state("garden", 1, 9));
    assert_eq!(
        conversation.environment_updates("garden", 1),
        [crate::channel::Update::State(payload(9))]
    );
    // A visitor only watches.
    conversation.apply(role(ConnectionRole::Visitor));
    assert!(!conversation.environment_intent("garden", 1, payload(3)));
    conversation.apply(role(ConnectionRole::Owner));
    // Leaving the space forgets its state; outside one there is nothing to take or ask.
    conversation.apply(Event::Message(ServerMessage::LEFT));
    conversation.apply(event(4));
    conversation.apply(state("garden", 1, 5));
    assert!(conversation.environment_updates("garden", 1).is_empty());
    assert!(!conversation.environment_intent("garden", 1, payload(3)));
    // A space from an older server, which names no version, takes the state at any.
    let mut older = joined(|state| state.environment = "garden".into());
    older.apply(state("garden", 1, 6));
    assert_eq!(
        older.environment_updates("garden", 1),
        [crate::channel::Update::State(payload(6))]
    );
}

/// What `space` projects: the Tomori wall, an added wall, and a panel on each.
fn projected(space: &str) -> Event {
    let wall = |id: &str, kind, x: f32| {
        tomori_protocol::Wall::new(
            id,
            kind,
            tomori_protocol::Pose::upright_facing([x, 1.5, -2.0], [0.0, 1.6, 0.0]),
        )
    };
    let on = |wall: &str| tomori_protocol::Place::Wall {
        wall: wall.into(),
        slot: 0,
    };
    Event::Message(ServerMessage::Projection {
        projection: Box::new(
            tomori_protocol::Projection::new(space, "sky")
                .with_walls(vec![
                    wall(
                        tomori_protocol::TOMORI_WALL,
                        tomori_protocol::WallKind::Tomori,
                        2.0,
                    ),
                    wall("wall-1", tomori_protocol::WallKind::Added, -2.0),
                    wall("wall-2", tomori_protocol::WallKind::Added, -6.0),
                ])
                .with_panels(vec![
                    tomori_protocol::Panel::new(
                        "panel-1",
                        "Build status",
                        "Green.",
                        on(tomori_protocol::TOMORI_WALL),
                    ),
                    tomori_protocol::Panel::new("panel-2", "", "## Steps\nOne.", on("wall-1")),
                ]),
        ),
    })
}

/// What `space` projects, as [`projected`] says, at `revision`.
fn projected_at(space: &str, revision: u64) -> tomori_protocol::Projection {
    let Event::Message(ServerMessage::Projection { projection }) = projected(space) else {
        unreachable!("a projection");
    };
    projection.with_revision(revision)
}

#[test]
fn the_joined_space_s_projection_is_patched_from_its_revision_and_asked_for_whole_after_a_gap() {
    let mut conversation = joined(|space| space.projection = true);
    assert!(Session::projection_patches(&conversation));
    assert!(Session::live_panels(&conversation));
    let whole = |projection: tomori_protocol::Projection| {
        Event::Message(ServerMessage::Projection {
            projection: Box::new(projection),
        })
    };
    let patch = |before: &tomori_protocol::Projection, after: &tomori_protocol::Projection| {
        Event::Message(ServerMessage::ProjectionPatch {
            patch: Box::new(tomori_protocol::ProjectionPatch::between(before, after)),
        })
    };
    let first = projected_at(SPACE, 3);
    conversation.apply(whole(first.clone()));
    // A panel dismissed, as a patch from the revision held.
    let mut second = first.clone().with_revision(4);
    second.panels.remove(1);
    conversation.apply(patch(&first, &second));
    assert_eq!(conversation.projection(), Some(&second));
    assert!(sent(&mut conversation).is_empty());
    // Another space's, as one on its way as the participant left it, is ignored.
    let elsewhere = projected_at("elsewhere", 4);
    conversation.apply(patch(&elsewhere, &elsewhere.clone().with_revision(5)));
    assert!(sent(&mut conversation).is_empty());
    // One from a revision after a patch missed is not applied, and the projection is asked for whole, once.
    let third = second.clone().with_revision(5);
    let mut fourth = third.clone().with_revision(6);
    fourth.panels[0].title = "Build status, green".into();
    conversation.apply(patch(&third, &fourth));
    conversation.apply(patch(&third, &fourth));
    assert_eq!(conversation.projection(), Some(&second));
    assert_eq!(
        sent(&mut conversation),
        [ClientMessage::ResyncProjection { revision: 4 }]
    );
    // Sent whole, it is patched from there again.
    conversation.apply(whole(fourth.clone()));
    let mut fifth = fourth.clone().with_revision(7);
    fifth.panels[0].place = tomori_protocol::Place::Free {
        pose: tomori_protocol::Pose::default(),
    };
    conversation.apply(patch(&fourth, &fifth));
    assert_eq!(conversation.projection(), Some(&fifth));
    assert!(sent(&mut conversation).is_empty());
}

#[test]
fn the_joined_space_s_projection_is_kept_and_its_panels_moved_and_dismissed() {
    let mut conversation = joined(|space| space.projection = true);
    // Nothing yet, and nothing to move.
    assert!(conversation.projection().is_none());
    conversation.place_panel(
        "panel-1".into(),
        crate::projection::Target::Wall(tomori_protocol::TOMORI_WALL.into(), 0),
    );
    assert!(!conversation.dismiss_panel("panel-1"));
    assert!(sent(&mut conversation).is_empty());
    // Another space's is ignored; the joined space's is kept.
    conversation.apply(projected("elsewhere"));
    assert!(conversation.projection().is_none());
    conversation.apply(projected(SPACE));
    assert_eq!(conversation.projection().unwrap().panels.len(), 2);
    let pose = tomori_protocol::Pose::default();
    conversation.place_panel("panel-1".into(), crate::projection::Target::Free(pose));
    conversation.place_panel(
        "panel-1".into(),
        crate::projection::Target::Wall("wall-1".into(), 3),
    );
    assert!(conversation.dismiss_panel("panel-2"));
    // One the space does not show is not dismissed.
    assert!(!conversation.dismiss_panel("panel-9"));
    assert_eq!(
        sent(&mut conversation),
        [
            ClientMessage::PlacePanel {
                panel: "panel-1".into(),
                wall: None,
                slot: None,
                pose: Some(pose),
                size: None,
                revision: None,
            },
            ClientMessage::PlacePanel {
                panel: "panel-1".into(),
                wall: Some("wall-1".into()),
                slot: Some(3),
                pose: None,
                size: None,
                revision: None,
            },
            ClientMessage::DismissPanel {
                panel: "panel-2".into()
            },
        ]
    );
    // A visitor moves and dismisses nothing, and is told so.
    conversation.apply(role(ConnectionRole::Visitor));
    assert!(!conversation.dismiss_panel("panel-1"));
    assert!(sent(&mut conversation).is_empty());
    conversation.apply(role(ConnectionRole::Owner));
    // A space whose server stops projecting shows nothing, and leaving forgets it.
    conversation.apply(space(|_| {}));
    assert!(conversation.projection().is_none());
    conversation.apply(space(|space| space.projection = true));
    assert!(conversation.projection().is_some());
    conversation.apply(Event::Message(ServerMessage::LEFT));
    assert!(conversation.projection.projection().is_none());
}

#[test]
fn a_chooses_the_choosable_row_pointed_at_by_its_id_and_the_version_seen_and_never_for_a_visitor() {
    let queue = |version: u64| {
        Event::Message(ServerMessage::Projection {
            projection: Box::new(
                tomori_protocol::Projection::new(SPACE, "sky").with_panels(vec![
                    tomori_protocol::Panel::new(
                        "panel-1",
                        "Queue",
                        "- pod7\n- [ ] pod112",
                        tomori_protocol::Place::Wall {
                            wall: "environment-0".into(),
                            slot: 0,
                        },
                    )
                    .with_rows(vec![
                        tomori_protocol::ListRow::new("pod7", false),
                        tomori_protocol::ListRow::new("pod112", true),
                    ])
                    .with_version(version),
                ]),
            ),
        })
    };
    let choose = |revision: Option<u64>| ClientMessage::ChoosePanelRow {
        panel: "panel-1".into(),
        row: "pod112".into(),
        revision,
        key: None,
    };
    let mut conversation = joined(|space| {
        space.projection = true;
        space.checks = true;
        space.choices = true;
    });
    conversation.apply(role(ConnectionRole::Collaborator));
    // With nothing projected, there is no row to choose.
    conversation.point_item(Some(("panel-1".to_owned(), 1)));
    assert_eq!(conversation.row_at("panel-1", 1), None);
    assert!(!conversation.choose_row());
    conversation.apply(queue(7));
    conversation.take_actions();
    // A row is found by its line, on a panel shown.
    assert_eq!(
        conversation.row_at("panel-1", 0),
        Some(&tomori_protocol::ListRow::new("pod7", false))
    );
    assert_eq!(conversation.row_at("panel-9", 0), None);
    assert_eq!(conversation.row_at("panel-1", 2), None);
    // A row that may not be chosen, or none, is not one A chooses.
    conversation.point_item(Some(("panel-1".to_owned(), 0)));
    assert_eq!(conversation.pointed_row(), None);
    assert!(conversation.press_a(None));
    conversation.point_item(None);
    assert_eq!(conversation.pointed_row(), None);
    // A choosable row is chosen, by its id and the version seen, before the box its text shows is ticked; and is
    // said to be just chosen.
    conversation.point_item(Some(("panel-1".to_owned(), 1)));
    assert_eq!(
        conversation.pointed_row(),
        Some(("panel-1".to_owned(), "pod112".to_owned()))
    );
    assert!(!conversation.press_a(None));
    assert!(!conversation.a_refused());
    assert_eq!(sent(&mut conversation), [choose(Some(7))]);
    assert!(conversation.just_chose("panel-1", "pod112"));
    assert!(!conversation.just_chose("panel-1", "pod7"));
    assert!(!conversation.just_chose("panel-9", "pod112"));
    // Refused, as of a row gone, it is no longer said to be chosen; a refusal of another panel leaves it so.
    crate::test_support::log_everything();
    let refused = |panel: &str| {
        Event::Message(ServerMessage::Error {
            error: "That row is no longer on the list.".into(),
            space: None,
            panel: Some(panel.into()),
            wall: None,
        })
    };
    conversation.apply(refused("panel-9"));
    assert!(conversation.just_chose("panel-1", "pod112"));
    conversation.apply(refused("panel-1"));
    assert!(!conversation.just_chose("panel-1", "pod112"));
    conversation.apply(refused("panel-1"));
    assert!(!conversation.just_chose("panel-1", "pod112"));
    assert!(!conversation.press_a(None));
    assert_eq!(sent(&mut conversation), [choose(Some(7))]);
    // From an older server, which keeps no versions, the choice says none.
    conversation.apply(queue(0));
    assert!(conversation.choose_row());
    assert_eq!(sent(&mut conversation), [choose(None)]);
    // A row with a key is chosen with the revision of its key as it was sent, never a key of its own.
    let keyed = tomori_protocol::Panel::new(
        "panel-1",
        "Queue",
        "- pod7\n- pod112",
        tomori_protocol::Place::Wall {
            wall: "environment-0".into(),
            slot: 0,
        },
    )
    .with_rows(vec![
        tomori_protocol::ListRow::new("pod7", false).with_key(40),
        tomori_protocol::ListRow::new("pod112", true).with_key(41),
    ])
    .with_version(8);
    conversation.apply(Event::Message(ServerMessage::Projection {
        projection: Box::new(
            tomori_protocol::Projection::new(SPACE, "sky").with_panels(vec![keyed]),
        ),
    }));
    assert!(conversation.choose_row());
    assert_eq!(
        sent(&mut conversation),
        [ClientMessage::ChoosePanelRow {
            panel: "panel-1".into(),
            row: "pod112".into(),
            revision: Some(8),
            key: Some(41),
        }]
    );
    // A server that takes no choices offers none.
    conversation.apply(space(|space| {
        space.projection = true;
        space.checks = true;
    }));
    assert_eq!(conversation.pointed_row(), None);
    // A visitor is told they only watch.
    conversation.apply(space(|space| {
        space.projection = true;
        space.choices = true;
    }));
    conversation.apply(role(ConnectionRole::Visitor));
    assert!(!conversation.press_a(None));
    assert!(conversation.a_refused());
    assert!(sent(&mut conversation).is_empty());
}

#[test]
fn a_ticks_or_unticks_the_checklist_item_pointed_at_for_everyone_after_the_list_and_never_for_a_visitor()
 {
    let agenda =
        |body: &str| {
            Event::Message(ServerMessage::Projection {
                projection: Box::new(tomori_protocol::Projection::new(SPACE, "sky").with_panels(
                    vec![tomori_protocol::Panel::new(
                        "panel-1",
                        "Agenda",
                        body,
                        tomori_protocol::Place::Wall {
                            wall: tomori_protocol::TOMORI_WALL.into(),
                            slot: 0,
                        },
                    )],
                )),
            })
        };
    let body = "To cover:\n\n- [ ] One\n- [x] Two\n- Three";
    // These panels say no revision, as an older server's do, so ticks say none, and go by their line alone.
    let tick = |line: u32, checked: bool| ClientMessage::CheckPanelItem {
        panel: "panel-1".into(),
        line,
        checked,
        revision: None,
    };
    let shown =
        |conversation: &Conversation| conversation.projection().unwrap().panels[0].body.clone();
    let mut conversation = joined(|space| {
        space.projection = true;
        space.checks = true;
        space.speech = SpeechSetting::Full;
    });
    conversation.decide_on_wrist();
    conversation.apply(role(ConnectionRole::Collaborator));
    conversation.apply(agenda(body));
    conversation.take_actions();
    // Pointing at nothing, at a line of no item, or at an item without a box, A does nothing new.
    for item in [None, Some(0), Some(4), Some(9)] {
        conversation.point_item(item.map(|line| ("panel-1".to_owned(), line)));
        assert!(conversation.press_a(None), "{item:?}");
        assert!(sent(&mut conversation).is_empty());
    }
    assert_eq!(conversation.item_checked("panel-1", 4), None);
    // Nor on a panel the space does not show, or with nothing projected.
    assert_eq!(conversation.item_checked("panel-9", 2), None);
    let mut unprojected = joined(|space| space.checks = true);
    unprojected.point_item(Some(("panel-1".to_owned(), 2)));
    assert_eq!(unprojected.item_checked("panel-1", 2), None);
    assert!(unprojected.press_a(None));
    // On an item with a box, it ticks it, showing it at once, and unticks it again.
    conversation.point_item(Some(("panel-1".to_owned(), 2)));
    assert_eq!(conversation.item_checked("panel-1", 2), Some(false));
    assert!(!conversation.press_a(None));
    assert!(!conversation.a_refused());
    assert_eq!(sent(&mut conversation), [tick(2, true)]);
    assert_eq!(
        shown(&conversation),
        "To cover:\n\n- [x] One\n- [x] Two\n- Three"
    );
    assert!(!conversation.press_a(None));
    assert_eq!(sent(&mut conversation), [tick(2, false)]);
    assert_eq!(shown(&conversation), body);
    // A code panel's lines are no items to tick.
    conversation.apply(Event::Message(ServerMessage::Projection {
        projection: Box::new(
            tomori_protocol::Projection::new(SPACE, "sky").with_panels(vec![
                tomori_protocol::Panel::new(
                    "panel-1",
                    "markdown",
                    "- [ ] One",
                    tomori_protocol::Place::Wall {
                        wall: tomori_protocol::TOMORI_WALL.into(),
                        slot: 0,
                    },
                )
                .with_code(tomori_protocol::Code::new("markdown", 1)),
            ]),
        ),
    }));
    assert_eq!(conversation.item_checked("panel-1", 0), None);
    // The server's projection says how it is.
    conversation.apply(agenda("- [x] One"));
    conversation.point_item(Some(("panel-1".to_owned(), 0)));
    assert_eq!(conversation.item_checked("panel-1", 0), Some(true));
    assert!(!conversation.press_a(None));
    assert_eq!(sent(&mut conversation), [tick(0, false)]);
    // The list comes first: open, A chooses from it, and ticks nothing.
    conversation.space_menu = Some(0);
    assert!(!conversation.press_a(None));
    assert!(matches!(
        sent(&mut conversation)[..],
        [ClientMessage::SetSpeech { .. }]
    ));
    // Just after a request appears, A is refused, and ticks nothing; once it has been read, A ticks what is pointed
    // at, ahead of saying to turn the wrist.
    conversation.apply(space(|space| {
        space.projection = true;
        space.checks = true;
        space.approval = Some(approval(&[]));
    }));
    conversation.apply(agenda("- [ ] One"));
    conversation.take_actions();
    conversation.request_at = Some(Instant::now());
    assert!(!conversation.press_a(None));
    assert!(conversation.a_refused());
    assert!(sent(&mut conversation).is_empty());
    settle(&mut conversation);
    assert!(!conversation.press_a(None));
    assert!(!conversation.a_refused());
    assert_eq!(sent(&mut conversation), [tick(0, true)]);
    // A server that takes no ticking is sent none, and A says to turn the wrist as before.
    conversation.apply(space(|space| {
        space.projection = true;
        space.approval = Some(approval(&[]));
    }));
    conversation.apply(agenda("- [ ] One"));
    conversation.take_actions();
    settle(&mut conversation);
    assert_eq!(conversation.item_checked("panel-1", 0), None);
    assert!(!conversation.press_a(None));
    assert!(sent(&mut conversation).is_empty());
    assert_eq!(
        conversation.notice.as_deref(),
        Some("Turn your right wrist to decide.")
    );
    // A visitor ticks nothing, and is told why.
    conversation.apply(space(|space| {
        space.projection = true;
        space.checks = true;
    }));
    conversation.apply(agenda("- [ ] One"));
    conversation.apply(role(ConnectionRole::Visitor));
    conversation.take_actions();
    assert!(!conversation.ticks());
    assert!(!conversation.press_a(None));
    assert!(conversation.a_refused());
    assert!(sent(&mut conversation).is_empty());
    assert_eq!(conversation.notice.as_deref(), Some(VISITING));
    assert_eq!(shown(&conversation), "- [ ] One");
}

#[test]
fn a_tick_says_the_revision_it_saw_and_one_refused_shows_the_panel_as_the_server_sends_it_again() {
    crate::test_support::log_everything();
    let agenda = |body: &str, revision: u64| {
        Event::Message(ServerMessage::Projection {
            projection: Box::new(
                tomori_protocol::Projection::new(SPACE, "sky").with_panels(vec![
                    tomori_protocol::Panel::new(
                        "panel-1",
                        "Agenda",
                        body,
                        tomori_protocol::Place::Wall {
                            wall: tomori_protocol::TOMORI_WALL.into(),
                            slot: 0,
                        },
                    )
                    .with_version(revision),
                ]),
            ),
        })
    };
    let tick = |line: u32, checked: bool, revision: u64| ClientMessage::CheckPanelItem {
        panel: "panel-1".into(),
        line,
        checked,
        revision: Some(revision),
    };
    let shown = |conversation: &Conversation| {
        let panel = &conversation.projection().unwrap().panels[0];
        (panel.body.clone(), panel.version)
    };
    let mut conversation = joined(|space| {
        space.projection = true;
        space.checks = true;
    });
    conversation.apply(role(ConnectionRole::Collaborator));
    conversation.apply(agenda("- [ ] One\n- [ ] Two", 3));
    conversation.take_actions();
    // It says the revision the server last sent, showing its tick at once but never counting it: had it guessed 4,
    // the revision the agent's rewrite racing the tick made, a second tick would land on another item.
    conversation.point_item(Some(("panel-1".to_owned(), 0)));
    assert!(!conversation.press_a(None));
    assert_eq!(sent(&mut conversation), [tick(0, true, 3)]);
    assert_eq!(shown(&conversation), ("- [x] One\n- [ ] Two".into(), 3));
    conversation.point_item(Some(("panel-1".to_owned(), 1)));
    assert!(!conversation.press_a(None));
    assert_eq!(sent(&mut conversation), [tick(1, true, 3)]);
    // The agent rewrote the body meanwhile, so the server refused both ticks, naming the panel, and sends the
    // projection again: the items show as they are, and the refusal is no problem to show, only a cue.
    conversation.apply(Event::Message(ServerMessage::Error {
        error: "That panel changed as you ticked it; look again.".into(),
        space: None,
        panel: Some("panel-1".into()),
        wall: None,
    }));
    conversation.apply(agenda("- [ ] Zero\n- [x] One\n- [ ] Two", 6));
    assert_eq!(
        shown(&conversation),
        ("- [ ] Zero\n- [x] One\n- [ ] Two".into(), 6)
    );
    assert_eq!(conversation.problem(), None);
    // So is a change to a wall refused as another changed it meanwhile.
    conversation.apply(Event::Message(ServerMessage::Error {
        error: "That wall changed as you changed it; look again.".into(),
        space: None,
        panel: None,
        wall: Some("tomori".into()),
    }));
    assert_eq!(conversation.problem(), None);
    // The frame step takes it, to draw the wall as the server has it at once, once.
    assert_eq!(conversation.take_refused_wall().as_deref(), Some("tomori"));
    assert_eq!(conversation.take_refused_wall(), None);
    // Looking again, the participant ticks the item where it is now, at the revision shown.
    conversation.point_item(Some(("panel-1".to_owned(), 2)));
    assert!(!conversation.press_a(None));
    assert_eq!(sent(&mut conversation), [tick(2, true, 6)]);
    // Another error is still a problem to show.
    conversation.apply(Event::Message(ServerMessage::Error {
        error: "Something went wrong.".into(),
        space: None,
        panel: None,
        wall: None,
    }));
    assert!(conversation.problem().is_some());
}

#[test]
fn the_images_of_the_panels_shown_are_fetched_a_few_at_a_time_held_and_asked_for_again_after_reconnecting()
 {
    let mut conversation = joined(|space| space.projection = true);
    let image =
        |digit: char| tomori_protocol::Image::new(digit.to_string().repeat(64), 64, 48, 100);
    let on = |slot| tomori_protocol::Place::Wall {
        wall: tomori_protocol::TOMORI_WALL.into(),
        slot,
    };
    let shown = |images: Vec<tomori_protocol::Image>| {
        Event::Message(ServerMessage::Projection {
            projection: Box::new(
                tomori_protocol::Projection::new(SPACE, "sky").with_panels(
                    images
                        .into_iter()
                        .enumerate()
                        .map(|(index, image)| {
                            tomori_protocol::Panel::new(
                                format!("panel-{index}"),
                                "",
                                "",
                                on(index as u32),
                            )
                            .with_image(image)
                        })
                        .collect(),
                ),
            ),
        })
    };
    let fetch = |digit: char| ClientMessage::FetchImage {
        image: digit.to_string().repeat(64),
    };
    let now = Instant::now();
    // Nothing is fetched before anything is shown.
    conversation.tick_space(now);
    assert!(sent(&mut conversation).is_empty());
    conversation.apply(shown(vec![image('a'), image('b'), image('a'), image('c')]));
    conversation.tick_space(now);
    assert_eq!(sent(&mut conversation), [fetch('a'), fetch('b')]);
    conversation.tick_space(now);
    assert!(sent(&mut conversation).is_empty());
    // Once one arrives, the next is asked for, and the one arrived is held to draw.
    conversation.apply(Event::Image {
        hash: "a".repeat(64),
        png: vec![1, 2, 3].into(),
    });
    assert_eq!(
        conversation.images().png(&"a".repeat(64)).unwrap()[..],
        [1, 2, 3]
    );
    conversation.tick_space(now);
    assert_eq!(sent(&mut conversation), [fetch('c')]);
    // Reconnected, what had not arrived is asked for again, and what had is held still.
    conversation.apply(Event::State(ConnectionState::Disconnected {
        reason: tomori_client::Disconnect::Closed,
        retry_in: Duration::from_secs(1),
    }));
    conversation.apply(shown(vec![image('a'), image('b'), image('c')]));
    conversation.tick_space(now);
    assert_eq!(sent(&mut conversation), [fetch('b'), fetch('c')]);
    assert!(conversation.images().png(&"a".repeat(64)).is_some());
}

#[test]
fn b_pointing_at_a_panel_dismisses_nothing_and_says_how_to_throw_it_away() {
    let mut conversation = joined(|space| space.projection = true);
    conversation.apply(projected(SPACE));
    conversation.point(Some(tomori_protocol::Resource::panel("panel-1")));
    // A problem shown is dismissed first.
    conversation.error = Some("Something failed.".into());
    conversation.back();
    conversation.hold_back(None);
    assert!(sent(&mut conversation).is_empty());
    assert!(conversation.error.is_none());
    // Pressed and let go, B alone dismisses nothing, and says that carrying the panel with the grip does.
    conversation.back();
    conversation.hold_back(Some(Duration::ZERO));
    conversation.hold_back(None);
    assert!(sent(&mut conversation).is_empty());
    assert_eq!(conversation.notice.as_deref(), Some(THROW_HOW));
    assert!(crate::ui_text::width(THROW_HOW) <= crate::pill::COLUMNS);
    // Held, it leaves the space, and still dismisses nothing.
    conversation.back();
    conversation.hold_back(Some(Duration::ZERO));
    assert!(conversation.hold_back(Some(LEAVE_HOLD)));
    conversation.hold_back(None);
    assert!(
        !sent(&mut conversation)
            .iter()
            .any(|message| matches!(message, ClientMessage::DismissPanel { .. }))
    );
    // A visitor, who dismisses nothing, is told to hold B to leave, as with nothing pointed at.
    let mut conversation = joined(|space| space.projection = true);
    conversation.apply(projected(SPACE));
    conversation.point(Some(tomori_protocol::Resource::panel("panel-1")));
    conversation.apply(role(ConnectionRole::Visitor));
    conversation.back();
    conversation.hold_back(None);
    assert!(sent(&mut conversation).is_empty());
    assert_eq!(
        conversation.notice.as_deref(),
        Some("Hold B to leave the space.")
    );
}

#[test]
fn a_panel_pointed_at_as_x_is_pressed_is_what_the_words_begin_attending_to() {
    let mut conversation = joined(|space| space.projection = true);
    conversation.apply(projected(SPACE));
    for (panel, named) in [("panel-1", "Build status"), ("panel-2", "Steps")] {
        conversation.point(Some(tomori_protocol::Resource::panel(panel)));
        conversation.talk(true);
        let actions = conversation.take_actions();
        let [Action::BeginUtterance(utterance)] = actions.as_slice() else {
            unreachable!("recording begins")
        };
        assert_eq!(
            utterance.attention,
            [tomori_protocol::Resource::panel(panel)]
        );
        assert!(
            conversation
                .text()
                .contains(&format!("MIC AGENT about {named}"))
        );
        conversation.talk(false);
        conversation.take_actions();
    }
}

#[test]
fn an_owner_adds_a_wall_ahead_of_them_and_removes_the_nearest_added_one() {
    let mut conversation = joined(|space| space.projection = true);
    let model = |conversation: &Conversation| conversation.menu_model().space.unwrap().walls;
    // Without a projection, nobody manages walls, and choosing changes nothing.
    assert_eq!(model(&conversation), None);
    conversation.choose_menu(crate::hand_menu::Choice::AddWall);
    conversation.choose_menu(crate::hand_menu::Choice::RemoveWall);
    assert!(sent(&mut conversation).is_empty());
    conversation.apply(projected(SPACE));
    // Nor without knowing where the participant stands.
    conversation.choose_menu(crate::hand_menu::Choice::AddWall);
    assert_eq!(model(&conversation), Some(false));
    assert!(sent(&mut conversation).is_empty());
    let head = tomori_protocol::Pose {
        position: [-1.5, 1.7, 0.0],
        orientation: [0.0, 0.0, 0.0, 1.0],
    };
    conversation.observe_head(Some(head));
    assert_eq!(model(&conversation), Some(true));
    conversation.choose_menu(crate::hand_menu::Choice::AddWall);
    conversation.choose_menu(crate::hand_menu::Choice::RemoveWall);
    assert_eq!(
        sent(&mut conversation),
        [
            ClientMessage::AddWall {
                pose: crate::projection::added_wall(head)
            },
            ClientMessage::RemoveWall {
                wall: "wall-1".into()
            },
        ]
    );
    // Anyone else does not manage them.
    conversation.apply(role(ConnectionRole::Collaborator));
    assert_eq!(model(&conversation), None);
    conversation.choose_menu(crate::hand_menu::Choice::AddWall);
    conversation.choose_menu(crate::hand_menu::Choice::RemoveWall);
    assert!(sent(&mut conversation).is_empty());
}

#[test]
fn a_on_a_teleport_pad_is_the_last_of_a_s_meanings_and_never_answers_a_request() {
    let approval = || tomori_protocol::Approval {
        id: "request-1".into(),
        title: "Bash".into(),
        text: "cargo test".into(),
        options: Vec::new(),
    };
    let waiting = || {
        let mut conversation = joined(|state| state.approval = Some(approval()));
        conversation.point_pad(true);
        conversation
    };
    // Where A decides, as in the desktop window, a request pending is decided, and no pad is chosen.
    let mut conversation = waiting();
    assert!(!conversation.accept());
    assert!(!conversation.take_pad_choice());
    assert!(matches!(
        sent(&mut conversation)[..],
        [ClientMessage::Decide { .. }]
    ));
    // Where it is decided on the right wrist, A says so, and chooses no pad.
    let mut conversation = waiting();
    conversation.decide_on_wrist();
    assert!(!conversation.accept());
    assert!(conversation.a_refused() && !conversation.take_pad_choice());
    assert!(sent(&mut conversation).is_empty());
    // Just after the request appeared, it is refused, and chooses no pad.
    let mut conversation = waiting();
    conversation.request_at = Some(Instant::now());
    assert!(!conversation.accept());
    assert!(conversation.a_refused() && !conversation.take_pad_choice());
    // A selected queue entry is cancelled before a pad is chosen.
    let mut conversation = joined(|_| {});
    conversation.point_pad(true);
    conversation.queue_selected = Some("entry".into());
    assert!(!conversation.accept());
    assert!(conversation.cancelling.is_some() && !conversation.take_pad_choice());
    // Otherwise, pointing at a pad, it is chosen, once, wherever the request is decided; and pointing at none, A
    // has nothing to do.
    for wrist in [false, true] {
        let mut conversation = joined(|_| {});
        if wrist {
            conversation.decide_on_wrist();
        }
        assert!(conversation.accept(), "{wrist}");
        assert!(!conversation.take_pad_choice());
        conversation.point_pad(true);
        assert!(!conversation.accept(), "{wrist}");
        assert!(!conversation.a_refused());
        assert!(conversation.take_pad_choice());
        assert!(!conversation.take_pad_choice());
    }
}

#[test]
fn the_participant_arrives_once_where_the_server_says() {
    let spawned = |space: &str, environment: &str, x: f32| {
        Event::Message(ServerMessage::Spawn {
            space: space.into(),
            environment: environment.into(),
            at: tomori_protocol::Pose::at([x, 0.0, 0.0]),
        })
    };
    let mut conversation = joined(|_| {});
    // A headset or desktop window asks where it arrives.
    assert!(Session::spawns(&conversation));
    assert_eq!(conversation.spawn(), None);
    // One for another space, or for another environment than the space is in, is not where it arrives.
    conversation.apply(spawned("elsewhere", "sky", 1.0));
    conversation.apply(spawned(SPACE, "farmyard", 1.0));
    assert_eq!(conversation.spawn(), None);
    conversation.apply(spawned(SPACE, "sky", 2.0));
    assert_eq!(conversation.spawn().map(|at| at.position[0]), Some(2.0));
    // Told again, as on reconnecting, it stays where it arrived.
    conversation.apply(spawned(SPACE, "sky", 3.0));
    assert_eq!(conversation.spawn().map(|at| at.position[0]), Some(2.0));
    // As the space changes environment, it arrives anew there, and not where it arrived before.
    conversation.apply(space(|state| state.environment = "farmyard".into()));
    assert_eq!(conversation.spawn(), None);
    conversation.apply(spawned(SPACE, "farmyard", 4.0));
    assert_eq!(conversation.spawn().map(|at| at.position[0]), Some(4.0));
    // Back in the first, it arrives anew there, not where it first arrived.
    conversation.apply(space(|state| state.environment = "sky".into()));
    assert_eq!(conversation.spawn(), None);
    conversation.apply(spawned(SPACE, "sky", 5.0));
    assert_eq!(conversation.spawn().map(|at| at.position[0]), Some(5.0));
    // Leaving forgets it.
    conversation.apply(Event::Message(ServerMessage::LEFT));
    assert_eq!(conversation.spawn(), None);
    assert_eq!(conversation.spectator_spawn(), None);
}

#[test]
fn the_spectator_chooses_its_own_spawn_point() {
    use tomori_protocol::spawn::{self, Spawn};
    let at = |x: f32| tomori_protocol::Pose::at([x, 0.0, 0.0]);
    let mut conversation = joined(|state| {
        state.anchors = tomori_protocol::Anchors::new()
            .with_spawn(Spawn::new(at(1.0)).with_tag(spawn::VISITOR))
            .with_spawn(Spawn::new(at(2.0)))
            .with_spawn(Spawn::new(at(3.0)).with_tag(spawn::SPECTATOR));
    });
    assert_eq!(conversation.spectator_spawn(), Some(at(3.0)));
    conversation.choose_own_arrival();
    assert!(!Session::spawns(&conversation));
}

#[test]
fn a_presses_a_copy_s_time_chip_freezing_or_unfreezing_it_as_seen_and_never_for_a_visitor() {
    use tomori_protocol::context::{ChipPress, ContextChip, ContextSetting, ContextValue};
    let chip = |press: Option<ChipPress>| {
        let chip = ContextChip::new(
            "time",
            "Time",
            ContextValue::Live { minutes: 15 },
            ContextSetting::Follows,
        );
        match press {
            Some(press) => chip.with_press(press),
            None => chip,
        }
    };
    let projected = |press: Option<ChipPress>, version: u64| {
        Event::Message(ServerMessage::Projection {
            projection: Box::new(
                tomori_protocol::Projection::new(SPACE, "sky").with_panels(vec![
                    tomori_protocol::Panel::new(
                        "panel-3",
                        "Latency",
                        "",
                        tomori_protocol::Place::Free {
                            pose: tomori_protocol::Pose::default(),
                        },
                    )
                    .with_context(vec![chip(press)])
                    .with_version(version),
                ]),
            ),
        })
    };
    let freeze = |frozen: bool, revision: Option<u64>| ClientMessage::FreezePanel {
        panel: "panel-3".into(),
        frozen,
        revision,
    };
    let mut conversation = joined(|space| space.projection = true);
    conversation.apply(role(ConnectionRole::Collaborator));
    let pointed = ("panel-3".to_owned(), "time".to_owned());
    // Nothing projected, or a chip that says nothing of pressing, presses nothing.
    conversation.point_chip(Some(pointed.clone()));
    assert_eq!(conversation.pointed_press(), None);
    conversation.apply(projected(None, 4));
    conversation.take_actions();
    assert_eq!(conversation.pointed_press(), None);
    assert!(conversation.press_a(None));
    assert!(sent(&mut conversation).is_empty());
    // A copy's following time chip freezes it, as it was seen; a frozen one unfreezes it.
    conversation.apply(projected(Some(ChipPress::Freeze), 5));
    assert_eq!(
        conversation.chip_press(&pointed),
        Some(("panel-3".to_owned(), ChipPress::Freeze))
    );
    assert_eq!(
        conversation.chip_press(&("panel-3".to_owned(), "region".to_owned())),
        None
    );
    assert_eq!(
        conversation.chip_press(&("panel-9".to_owned(), "time".to_owned())),
        None
    );
    assert!(!conversation.press_a(None));
    assert_eq!(sent(&mut conversation), [freeze(true, Some(5))]);
    conversation.apply(projected(Some(ChipPress::Unfreeze), 0));
    assert!(!conversation.press_a(None));
    assert_eq!(sent(&mut conversation), [freeze(false, None)]);
    // What a newer server says pressing does, it offers nothing for.
    conversation.apply(projected(Some(ChipPress::Unknown), 6));
    assert_eq!(conversation.pointed_press(), None);
    // Pointed at nothing, A presses nothing; a visitor is refused, and told they only watch.
    conversation.apply(projected(Some(ChipPress::Freeze), 7));
    conversation.point_chip(None);
    assert_eq!(conversation.pointed_press(), None);
    conversation.point_chip(Some(pointed));
    conversation.apply(role(ConnectionRole::Visitor));
    assert!(!conversation.press_a(None));
    assert!(conversation.a_refused());
    assert!(sent(&mut conversation).is_empty());
}
