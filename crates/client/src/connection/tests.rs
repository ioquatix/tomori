// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

//! The connection's tests. Its parts, the voice, the speaker and the echo test, are tested in their own modules, and the replay here, on its own; the connection as a whole is driven through [`Session`] by [`Script`], a session that asks for what each test says and records what it is told, against a scripted [`Peer`] or a real [`Server`].
use super::{echo::ECHO_HEARD, test_support::*, *};
use crate::{
    capture::{Active, Failed, Recording, Samples},
    feedback::{Haptic, Level, Settings},
    playback::{Earcon, Layout, MixerOptions, Player},
    speaker::{Output, Shared},
};
use std::{
    cell::RefCell,
    collections::VecDeque,
    io::{Read, Write},
    net::TcpListener,
    path::PathBuf,
    sync::{
        Mutex,
        atomic::{AtomicBool, AtomicU32, Ordering},
        mpsc,
    },
};
use tomori_protocol::{ConnectionRole, Frame, Gate};

#[test]
fn the_kept_certificate_is_pinned_before_the_advertised_one() {
    let kept = Fingerprint::of(b"kept").to_string();
    let advertised = Fingerprint::of(b"advertised").to_string();
    assert_eq!(
        transport(Some(&kept), Some(&advertised)),
        (
            Transport::Tls {
                pin: kept.parse().ok()
            },
            true
        )
    );
    assert_eq!(
        transport(None, Some(&advertised)),
        (
            Transport::Tls {
                pin: advertised.parse().ok()
            },
            false
        )
    );
    // Nothing known, or nothing valid: try TLS, then plain HTTP.
    assert_eq!(
        transport(None, None),
        (Transport::Automatic { pin: None }, false)
    );
    assert_eq!(
        transport(Some("not a fingerprint"), None),
        (Transport::Automatic { pin: None }, false)
    );
}

#[test]
fn the_speaker_keeps_the_ambience_and_its_level_for_this_player_and_the_next() {
    use crate::playback::ambience::{Ambience, Bed, Slot};
    let mut speaker = Speaker::new(Box::new(FakeSpeaker::default()));
    let farm = Arc::new(Ambience::new().with_bed(Bed::new(Slot::missing("wind"))));
    // Without a player, kept for the next.
    speaker.set_ambience(Some(farm.clone()));
    speaker.set_ambience_gain(0.5);
    let (_sender, audio) = mpsc::sync_channel(8);
    let (player, control) = Player::new(audio, MixerOptions::default());
    speaker.player = Some((Arc::new(Mutex::new(player)), control));
    // With one, told it now.
    speaker.set_ambience(None);
    speaker.set_ambience_gain(0.25);
    assert!(speaker.ambience.is_none());
    assert_eq!(speaker.ambience_gain, 0.25);
    // A connection's player is given what was kept, and a change of the ambience its feedback hands on.
    let mut session = Script::default();
    let mut connection = kept(
        &mut session,
        Store::new(None),
        None,
        None,
        Scripted::default(),
        Nowhere,
    );
    assert_eq!(
        connection.speaker.ambience_gain,
        crate::feedback::Ambience::Low.gain()
    );
    connection.feedback().set_ambience(Some(farm.clone()));
    connection.poll(true, &mut session);
    assert!(
        connection
            .speaker
            .ambience
            .as_ref()
            .is_some_and(|kept| Arc::ptr_eq(kept, &farm))
    );
}

#[test]
fn a_poll_keeps_its_slowest_part_by_what_it_was() {
    assert_eq!(Kind::of(&Action::EndUtterance).name(), "EndUtterance");
    assert_eq!(Kind::of(&Action::Send(ClientMessage::Leave)).name(), "Send");
    assert_eq!(
        Kind::event(&Event::Message(ServerMessage::Ping { nonce: 1 })).name(),
        "Ping",
        "a message by its own kind"
    );
    let receiving = Event::Receiving {
        received: 1,
        length: 2,
    };
    assert_eq!(Kind::event(&receiving).name(), "Receiving");
    assert_eq!(Kind::named("tick").name(), "tick");
    assert_eq!(
        Kind::named(&"a".repeat(40)).name(),
        "a".repeat(32),
        "a name is cut"
    );
    assert_eq!(format!("{:?}", Kind::named("cues")), "cues");
    let mut slowest = None;
    Slowest::keep(
        &mut slowest,
        Kind::named("tick"),
        Instant::now() - Duration::from_millis(5),
    );
    Slowest::keep(&mut slowest, Kind::named("cues"), Instant::now());
    let longest = slowest.expect("a part was kept");
    assert_eq!(longest.what, Kind::named("tick"), "the slowest is kept");
    assert!(longest.took >= Duration::from_millis(5));
    let said = Slowest {
        what: Kind::named("ProjectionPatch"),
        took: Duration::from_micros(4210),
    };
    assert_eq!(said.to_string(), "ProjectionPatch 4.21 ms");
    // A connection keeps its polls' slowest part until it is asked, each action timed by its kind.
    let mut session = Script::default();
    let mut connection = kept(
        &mut session,
        Store::new(None),
        None,
        None,
        Scripted::default(),
        Nowhere,
    );
    assert_eq!(connection.slowest(), None);
    session.push(Action::SetAgentMuted(true));
    connection.poll(true, &mut session);
    assert!(connection.slowest().is_some());
    assert_eq!(connection.slowest(), None);
}

#[test]
fn a_catch_up_is_cued_once_unless_asked_for_and_ends_some_time_after_its_last_segment() {
    let start = Instant::now();
    let at = |seconds: u64| start + Duration::from_secs(seconds);
    let mut replay = Replay::default();
    assert!(!replay.over(start));
    // Nobody asked: the first segment is cued, and the next is part of the same catch-up.
    assert!(replay.played(start));
    assert!(!replay.played(at(1)));
    assert!(!replay.over(at(1) + REPLAY_OVER - Duration::from_millis(1)));
    assert!(replay.over(at(1) + REPLAY_OVER));
    // Asked for: never cued, and over once it has played, or if it never starts.
    let mut replay = Replay {
        asked: Some(start),
        since: None,
    };
    assert!(!replay.over(start + REPLAY_OVER));
    assert!(replay.over(start + REPLAY_START));
    assert!(!replay.played(at(1)));
    assert!(replay.over(at(1) + REPLAY_OVER));
}

/// A session that asks for the actions a test queues and records what it is told, as a client's conversation would apply it. It answers a pairing and an admission as a conversation does, and takes the cues due each frame as heard when `listening`, rather than leaving them to be played.
#[derive(Default)]
struct Script {
    actions: VecDeque<Action>,
    events: Vec<Event>,
    reports: Vec<Report>,
    browsing: bool,
    /// The approvals and questions, and the knocks, waiting for the participant.
    requests: Vec<String>,
    knocks: Vec<String>,
    listening: bool,
    heard: RefCell<Vec<Earcon>>,
    /// The value this device committed to for a pairing, to reveal when the server sends its own.
    pairing: Option<tomori_security::pairing::Nonce>,
    /// Leave the server as pairing completes, before the token is kept.
    leaving: bool,
}

impl Session for Script {
    fn next_action(&mut self) -> Option<Action> {
        self.actions.pop_front()
    }

    fn apply(&mut self, event: Event) {
        match &event {
            Event::Message(ServerMessage::PairNonce { .. }) => {
                let device = self.pairing.take().expect("this device began pairing");
                self.push(Action::Send(ClientMessage::PairReveal {
                    nonce: device.to_hex(),
                }));
            }
            Event::Message(ServerMessage::Paired { token }) => {
                // As a participant who leaves the server in the frame that pairing completes.
                if self.leaving {
                    self.push(Action::Disconnect);
                }
                self.push(Action::Paired {
                    token: token.clone(),
                });
            }
            Event::Message(ServerMessage::Admitted { token }) => {
                self.push(Action::Admitted {
                    token: token.clone(),
                });
            }
            _ => {}
        }
        self.events.push(event);
    }

    fn report(&mut self, report: Report) {
        self.reports.push(report);
    }

    fn is_browsing(&self) -> bool {
        self.browsing
    }

    fn observe(&self, feedback: &mut feedback::Feedback, now: Instant) {
        feedback.observe_requests(self.requests.iter().map(String::as_str), now);
        feedback.observe_knocks(self.knocks.iter().map(String::as_str), now);
        if self.listening {
            let earcons = feedback.take_earcons().map(|(earcon, _)| earcon);
            self.heard.borrow_mut().extend(earcons);
        }
    }
}

impl Script {
    fn listening() -> Self {
        Self {
            listening: true,
            ..Self::default()
        }
    }

    fn push(&mut self, action: Action) {
        self.actions.push_back(action);
    }

    /// The cues heard since last asked.
    fn heard(&self) -> Vec<Earcon> {
        std::mem::take(&mut self.heard.borrow_mut())
    }

    /// How many of the events applied match.
    fn count(&self, matches: impl Fn(&Event) -> bool) -> usize {
        self.events.iter().filter(|event| matches(event)).count()
    }

    fn reported(&self, report: &Report) -> usize {
        self.reports.iter().filter(|seen| *seen == report).count()
    }

    /// The connection generation the microphone records for, as reported.
    fn generation(&self) -> u64 {
        self.reports
            .iter()
            .rev()
            .find_map(|report| match report {
                Report::Generation(generation) => Some(*generation),
                _ => None,
            })
            .expect("connected")
    }

    /// Whether the connection's latest state is connected.
    fn connected(&self) -> bool {
        self.events.iter().rev().find_map(|event| match event {
            Event::State(state) => Some(state.is_connected()),
            _ => None,
        }) == Some(true)
    }

    fn certificate(&self) -> Option<Fingerprint> {
        self.events.iter().rev().find_map(|event| match event {
            Event::State(ConnectionState::Connected { certificate, .. }) => Some(*certificate),
            _ => None,
        })?
    }

    /// What the server last said this connection may do.
    fn role(&self) -> Option<ConnectionRole> {
        self.events.iter().rev().find_map(|event| match event {
            Event::State(ConnectionState::Connected { role, .. })
            | Event::Message(ServerMessage::Role { role }) => Some(*role),
            _ => None,
        })
    }

    /// Begin pairing, as choosing the pairing object does.
    fn pair(&mut self) {
        let device = tomori_security::pairing::Nonce::generate();
        self.push(Action::Send(ClientMessage::PairBegin {
            commitment: device.commitment(),
        }));
        self.pairing = Some(device);
    }
}

/// Poll as the frame loop does, focused, until `done`, at least once.
fn until(
    connection: &mut Connection,
    session: &mut Script,
    mut done: impl FnMut(&mut Connection, &mut Script) -> bool,
) {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        connection.poll(true, session);
        if done(connection, session) {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "timed out: {:?}\n{:?}",
            session.events,
            session
                .reports
                .iter()
                .filter(|report| !matches!(
                    report,
                    Report::Tick { .. }
                        | Report::Replaying(_)
                        | Report::AgentSpeech { .. }
                        | Report::Voices(_)
                ))
                .collect::<Vec<_>>()
        );
        std::thread::sleep(Duration::from_millis(2));
    }
}

/// Poll as the frame loop does, focused, for `duration`.
fn run_for(connection: &mut Connection, session: &mut Script, duration: Duration) {
    let end = Instant::now() + duration;
    until(connection, session, |_, _| Instant::now() >= end);
}

/// A speaker that opens and plays nowhere.
struct Nowhere;

impl Playback for Nowhere {
    fn open(
        &self,
        _player: Shared,
        _failed: crate::speaker::Failed,
        _route: Route,
    ) -> Result<Output, String> {
        Ok(Output::new(()))
    }
}

/// A speaker whose player the test renders itself, keeping how each output was asked to play.
#[derive(Clone, Default)]
struct Held {
    player: Arc<Mutex<Option<Shared>>>,
    routes: Arc<Mutex<Vec<Route>>>,
}

impl Playback for Held {
    fn open(
        &self,
        player: Shared,
        _failed: crate::speaker::Failed,
        route: Route,
    ) -> Result<Output, String> {
        self.routes.lock().unwrap().push(route);
        *self.player.lock().unwrap() = Some(player);
        Ok(Output::new(()))
    }
}

impl Held {
    fn opened(&self) -> bool {
        self.player.lock().unwrap().is_some()
    }

    /// Whether the next tenth of a second played is heard.
    fn heard(&self) -> bool {
        let player = self.player.lock().unwrap().clone().expect("opened");
        let mut output = vec![0; 4_800];
        player
            .lock()
            .unwrap()
            .render(48_000, Layout::Mono, &mut output);
        output.iter().any(|sample| *sample != 0)
    }

    /// Play two seconds, past any cue.
    fn flush(&self) {
        for _ in 0..20 {
            self.heard();
        }
    }

    fn route(&self) -> Option<Route> {
        self.routes.lock().unwrap().last().copied()
    }
}

/// Fails as it opens, as a device that disappears.
struct Vanishes;

impl Playback for Vanishes {
    fn open(
        &self,
        _player: Shared,
        failed: crate::speaker::Failed,
        _route: Route,
    ) -> Result<Output, String> {
        failed("The speaker went away.".into());
        Ok(Output::new(()))
    }
}

/// Permission to receive multicast, which keeps whether it was held each time it changed.
#[derive(Clone, Default)]
struct Lock(Arc<Mutex<Vec<bool>>>);

impl Multicast for Lock {
    fn set_held(&mut self, held: bool) {
        self.0.lock().unwrap().push(held);
    }
}

impl Lock {
    fn held(&self) -> Option<bool> {
        self.0.lock().unwrap().last().copied()
    }
}

/// A search that finds only what the test sent it.
impl Browsing for mpsc::Receiver<discovery::Event> {
    fn try_recv(&self) -> Result<discovery::Event, discovery::TryRecvError> {
        mpsc::Receiver::try_recv(self)
    }
}

/// A search that finds the server named Studio, advertised as `studio`.
fn finds_studio() -> Result<Box<dyn Browsing>, Box<dyn std::error::Error>> {
    let (found, search) = mpsc::channel();
    found
        .send(discovery::Event::Found(discovery::Server {
            instance: "studio".into(),
            name: "Studio".into(),
            host: "studio.local.".into(),
            addresses: vec!["192.0.2.1:8765".parse().unwrap()],
            port: 8765,
            protocol: Some(tomori_protocol::PROTOCOL_VERSION),
            spaces: None,
            participants: None,
            waiting: None,
            problems: None,
            replies: None,
            fingerprint: None,
        }))
        .unwrap();
    Ok(Box::new(search))
}

/// A directory of its own for a store, under the temporary directory, and empty.
fn directory(name: &str) -> PathBuf {
    let directory =
        std::env::temp_dir().join(format!("tomori-connection-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&directory);
    directory
}

fn identity(store: &Store, server: Option<String>, token: Option<String>) -> Identity {
    Identity::new(
        store,
        None,
        crate::identity::Overrides {
            server,
            name: Some("Ada".into()),
            token,
        },
    )
}

/// Ada's connection, keeping what she learns in `store`, connected at once to `server` if given, with `token`.
fn kept(
    session: &mut Script,
    store: Store,
    server: Option<String>,
    token: Option<String>,
    capture: impl Capture + 'static,
    playback: impl Playback + 'static,
) -> Connection {
    let identity = identity(&store, server, token);
    Connection::new(
        identity,
        store,
        Box::new(()),
        Box::new(capture),
        Box::new(playback),
        session,
    )
}

/// Ada's connection, keeping nothing, not yet connected.
fn connection(
    session: &mut Script,
    capture: impl Capture + 'static,
    playback: impl Playback + 'static,
) -> Connection {
    kept(session, Store::new(None), None, None, capture, playback)
}

/// What the participant feels by now, by hand.
fn felt(connection: &mut Connection) -> Vec<Hand> {
    connection
        .feedback()
        .haptics(Instant::now() + Duration::from_secs(1))
        .into_iter()
        .filter(|(_, haptic)| matches!(haptic, Haptic::Vibrate { .. }))
        .map(|(hand, _)| hand)
        .collect()
}

fn pose(x: f32) -> Pose {
    Pose {
        position: [x, 1.6, 0.0],
        orientation: [0.0, 0.0, 0.0, 1.0],
    }
}

#[test]
fn a_server_given_at_startup_is_connected_to_at_once_and_the_feedback_setting_kept_is_reported() {
    crate::test_support::log_everything();
    let mut session = Script::default();
    let _connection = kept(
        &mut session,
        Store::new(None),
        Some(" 192.0.2.1:8765 ".into()),
        None,
        Scripted::default(),
        Nowhere,
    );
    assert_eq!(
        session.reports,
        [
            Report::Connecting(Choice::new(
                "192.0.2.1:8765",
                "192.0.2.1:8765",
                Source::Override
            )),
            Report::Feedback(Settings::default())
        ]
    );
    // A token given without a server waits for one; nothing connects.
    let mut session = Script::default();
    let _connection = kept(
        &mut session,
        Store::new(None),
        None,
        Some("tomori_token".into()),
        Scripted::default(),
        Nowhere,
    );
    assert_eq!(session.reports, [Report::Feedback(Settings::default())]);
}

#[test]
fn asking_for_the_microphone_ahead_of_time_asks_the_platform_once() {
    crate::test_support::log_everything();
    for capture in [
        Scripted::default(),
        Scripted {
            granted: true,
            ..Default::default()
        },
        Scripted {
            refuse: Some("No permission.".into()),
            ..Default::default()
        },
    ] {
        let asked = capture.asked.clone();
        let mut session = Script::default();
        let mut connection = connection(&mut session, capture, Nowhere);
        connection.request_microphone();
        assert_eq!(asked.load(Ordering::Relaxed), 1);
    }
}

#[test]
fn without_a_connection_nothing_is_sent_and_the_participant_is_told() {
    crate::test_support::log_everything();
    let mut session = Script::listening();
    let mut connection = connection(&mut session, Scripted::default(), Nowhere);
    connection.feedback().set_haptics(true);
    // Presence goes nowhere, and no voice is placed.
    connection.send_presence(
        pose(0.0),
        None,
        None,
        [None, None],
        &[],
        [false, true],
        [None, None],
    );
    let mut placed = false;
    connection.listen([2], |_, _| placed = true);
    assert!(!placed);
    // A request fails, felt on the right hand, which asked.
    session.push(Action::Send(ClientMessage::ListSpaces));
    connection.poll(true, &mut session);
    assert_eq!(
        session.reported(&Report::Failed("Not connected.".into())),
        1
    );
    assert_eq!(session.heard(), [Earcon::Failed]);
    assert_eq!(felt(&mut connection), [Hand::Right, Hand::Right]);
    // Neither recording nor the live microphone starts, felt on the left hand, the voice's.
    for action in [
        Action::BeginUtterance(Utterance::new("u-1", 0)),
        Action::StartLive(0),
    ] {
        session.push(action);
        connection.poll(true, &mut session);
        assert_eq!(
            session.reported(&Report::MicrophoneFailed("Not connected.".into())),
            1
        );
        session.reports.clear();
        assert!(session.heard().contains(&Earcon::Failed));
        assert!(felt(&mut connection).contains(&Hand::Left));
    }
    // What never started stops silently, and there is nothing to cancel, abandon or close, nothing to search again while the server list is not seen, and no connection to keep a token for.
    for action in [
        Action::StopLive,
        Action::CancelUtterance,
        Action::AbandonUtterance,
        Action::Disconnect,
        Action::SetAgentMuted(true),
        Action::Rescan,
        Action::Paired {
            token: "tomori_owner".into(),
        },
        Action::Admitted {
            token: "tomori_collaborator".into(),
        },
    ] {
        session.push(action);
    }
    connection.poll(true, &mut session);
    assert!(session.heard().is_empty());
    assert!(felt(&mut connection).is_empty());
    assert!(session.reports.iter().all(|report| matches!(
        report,
        Report::Tick { .. }
            | Report::Replaying(false)
            | Report::AgentSpeech { playing: false, .. }
            | Report::Voices(_)
    )));
    // Words never begun end as X is released, felt, and nothing is sent or reported.
    session.push(Action::EndUtterance);
    connection.poll(true, &mut session);
    assert!(session.heard().is_empty());
    assert_eq!(felt(&mut connection), [Hand::Left]);
    assert!(session.reports.iter().all(|report| matches!(
        report,
        Report::Tick { .. }
            | Report::Replaying(false)
            | Report::AgentSpeech { playing: false, .. }
            | Report::Voices(_)
    )));
    // Without a speaker, cues and ambient sounds have nowhere to play, and are dropped.
    session.listening = false;
    connection.feedback().emit(Feedback::Choose, Instant::now());
    let sound = crate::playback::Sound::new(vec![1_000.0; 480], 48_000);
    connection
        .feedback()
        .ambient(sound, [0.0; 3], Instant::now());
    connection.poll(true, &mut session);
    assert_eq!(connection.feedback().take_earcons().count(), 0);
    assert_eq!(connection.feedback().take_ambient().count(), 0);
}

#[test]
fn the_feedback_setting_is_applied_and_kept_on_the_device() {
    let directory = directory("feedback");
    let mut session = Script::default();
    let mut connection = kept(
        &mut session,
        Store::new(Some(directory.clone())),
        None,
        None,
        Scripted::default(),
        Nowhere,
    );
    let quiet = Settings {
        sounds: Level::Off,
        vibration: Level::Reduced,
        mono: true,
        ambience: crate::feedback::Ambience::Off,
    };
    session.push(Action::SetFeedback(quiet));
    connection.poll(true, &mut session);
    assert_eq!(connection.feedback().settings(), quiet);
    // The next run starts with it.
    let mut next = Script::default();
    let _next = kept(
        &mut next,
        Store::new(Some(directory.clone())),
        None,
        None,
        Scripted::default(),
        Nowhere,
    );
    assert_eq!(next.reported(&Report::Feedback(quiet)), 1);
    let _ = std::fs::remove_dir_all(directory);
}

#[test]
fn the_network_is_searched_only_while_the_server_list_is_seen() {
    let lock = Lock::default();
    let mut session = Script {
        browsing: true,
        ..Script::default()
    };
    let store = Store::new(None);
    let identity = identity(&store, None, None);
    let mut connection = Connection::new(
        identity,
        store,
        Box::new(lock.clone()),
        Box::new(Scripted::default()),
        Box::new(Nowhere),
        &mut session,
    );
    connection.poll(true, &mut session);
    connection.poll(true, &mut session);
    assert_eq!(lock.held(), Some(true));
    assert!(
        session
            .reports
            .iter()
            .any(|report| matches!(report, Report::Tick { browsing: true, .. }))
    );
    // Unfocused, it stops, as does the microphone.
    connection.poll(false, &mut session);
    assert_eq!(lock.held(), Some(false));
    assert_eq!(session.reported(&Report::Unfocused), 1);
    assert!(matches!(
        session
            .reports
            .iter()
            .rev()
            .find(|report| matches!(report, Report::Tick { .. })),
        Some(Report::Tick {
            browsing: false,
            ..
        })
    ));
    // Dropping the connection releases the permission, once more.
    connection.poll(true, &mut session);
    drop(connection);
    assert_eq!(lock.held(), Some(false));
}

#[test]
fn a_server_the_search_finds_is_listed_and_a_failure_to_search_is_shown_once() {
    crate::test_support::log_everything();
    let lock = Lock::default();
    let mut session = Script {
        browsing: true,
        ..Script::default()
    };
    let store = Store::new(None);
    let identity = identity(&store, None, None);
    let mut connection = Connection::new(
        identity,
        store,
        Box::new(lock.clone()),
        Box::new(Scripted::default()),
        Box::new(Nowhere),
        &mut session,
    );
    connection.browse_servers = finds_studio;
    connection.poll(true, &mut session);
    let listed = session
        .reports
        .iter()
        .rev()
        .find_map(|report| match report {
            Report::Servers(servers) => Some(servers.clone()),
            _ => None,
        });
    assert!(
        listed
            .unwrap()
            .iter()
            .any(|choice| choice.label == "Studio" && choice.source == Source::Discovered)
    );
    // Searching again after it stopped fails: shown once, and the permission is released.
    connection.poll(false, &mut session);
    connection.browse_servers = || Err("no multicast".into());
    let failed = Report::Failed("Cannot search the network: no multicast".into());
    connection.poll(true, &mut session);
    connection.poll(true, &mut session);
    assert_eq!(session.reported(&failed), 1);
    assert_eq!(lock.held(), Some(false));
    // Searching again tries again.
    session.push(Action::Rescan);
    connection.poll(true, &mut session);
    connection.poll(true, &mut session);
    assert_eq!(session.reported(&failed), 2);
}

/// What a [`Peer`] does next.
enum Say {
    Message(ServerMessage),
    Audio(tomori_protocol::AudioFrame),
    Close,
}

/// A server on a loopback port that speaks plain HTTP and says only what the test tells it, for what a real server cannot be made to say at will. It welcomes each connection, as participant 1 and an owner, or refuses it if `refusing`.
struct Peer {
    address: String,
    say: mpsc::Sender<Say>,
    /// How many connections it has seen end.
    ended: Arc<AtomicU32>,
}

/// `data` as one HTTP/1.1 chunk.
fn chunk(data: &[u8]) -> Vec<u8> {
    let mut output = format!("{:x}\r\n", data.len()).into_bytes();
    output.extend_from_slice(data);
    output.extend_from_slice(b"\r\n");
    output
}

impl Peer {
    fn start() -> Self {
        Self::serving(false)
    }

    fn serving(refusing: bool) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap().to_string();
        let (say, said) = mpsc::channel();
        let ended = Arc::new(AtomicU32::new(0));
        let counted = ended.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { break };
                // An automatic connection tries TLS first: refuse it, as a server in `--auth open` mode does, so that it tries plain HTTP.
                let mut first = [0; 1];
                if stream.peek(&mut first).unwrap_or(0) == 0 || first[0] == 0x16 {
                    continue;
                }
                let mut head = Vec::new();
                let mut byte = [0; 1];
                while !head.ends_with(b"\r\n\r\n") && stream.read(&mut byte).unwrap_or(0) == 1 {
                    head.push(byte[0]);
                }
                if refusing {
                    let _ =
                        stream.write_all(b"HTTP/1.1 401 Unauthorized\r\ncontent-length: 0\r\n\r\n");
                    continue;
                }
                let welcome = ServerMessage::Welcome {
                    protocol: tomori_protocol::PROTOCOL_VERSION,
                    server: "peer".into(),
                    source: 1,
                    role: ConnectionRole::Owner,
                    backends: vec![],
                    roots: vec![],
                    environments: vec![],
                    utterance_limit: None,
                    name: None,
                    limits: None,
                };
                let mut response =
                    b"HTTP/1.1 200 OK\r\ntransfer-encoding: chunked\r\n\r\n".to_vec();
                response.extend(chunk(&Frame::server(&welcome).encode()));
                let _ = stream.write_all(&response);
                // Read what the client sends until it closes.
                let gone = Arc::new(AtomicBool::new(false));
                let reader = {
                    let mut stream = stream.try_clone().unwrap();
                    let gone = gone.clone();
                    std::thread::spawn(move || {
                        let mut sink = [0; 4096];
                        while stream.read(&mut sink).unwrap_or(0) > 0 {}
                        gone.store(true, Ordering::Relaxed);
                    })
                };
                loop {
                    match said.recv_timeout(Duration::from_millis(5)) {
                        Ok(Say::Message(message)) => {
                            let _ = stream.write_all(&chunk(&Frame::server(&message).encode()));
                        }
                        Ok(Say::Audio(frame)) => {
                            let _ = stream.write_all(&chunk(&Frame::audio(&frame).encode()));
                        }
                        Ok(Say::Close) => break,
                        Err(mpsc::RecvTimeoutError::Timeout) if !gone.load(Ordering::Relaxed) => {}
                        Err(_) => break,
                    }
                }
                let _ = stream.shutdown(std::net::Shutdown::Both);
                let _ = reader.join();
                counted.fetch_add(1, Ordering::Relaxed);
            }
        });
        Self {
            address,
            say,
            ended,
        }
    }

    fn choice(&self) -> Choice {
        Choice::new("Peer", &self.address, Source::Discovered)
    }

    /// Say `message` to the connection, and poll until it is applied.
    fn say(&self, message: ServerMessage, connection: &mut Connection, session: &mut Script) {
        let event = Event::Message(message.clone());
        let before = session.count(|applied| *applied == event);
        self.say.send(Say::Message(message)).unwrap();
        until(connection, session, |_, session| {
            session.count(|applied| *applied == event) > before
        });
    }

    /// Speak as the agent for a fifth of a second, with one frame lost on the way, and poll until it has all arrived.
    fn speak(&self, connection: &mut Connection, session: &mut Script) {
        self.speak_as(tomori_protocol::AGENT_SOURCE, 0, connection, session);
    }

    /// Speak as `source` for a fifth of a second, as [`Self::speak`] does, numbering its frames from `from`.
    fn speak_as(&self, source: u32, from: u32, connection: &mut Connection, session: &mut Script) {
        for sequence in (from..from + 10).filter(|sequence| *sequence != from + 4) {
            let frame = tomori_protocol::AudioFrame {
                gate: Gate::Live,
                source,
                sequence,
                timestamp: u64::from(sequence) * 20_000,
                sample_rate: 48_000,
                samples: vec![8_000; 960],
            };
            self.say.send(Say::Audio(frame)).unwrap();
        }
        // What follows it on the connection arrives after it.
        self.say(
            ServerMessage::Spaces { spaces: vec![] },
            connection,
            session,
        );
    }

    /// Close the connection, and poll until the client has noticed.
    fn close(&self, connection: &mut Connection, session: &mut Script) {
        let dropped =
            |event: &Event| matches!(event, Event::State(ConnectionState::Disconnected { .. }));
        let before = session.count(dropped);
        self.say.send(Say::Close).unwrap();
        until(connection, session, |_, session| {
            session.count(dropped) > before
        });
    }
}

/// Ada's connection to `peer`, connected.
fn with_peer(
    peer: &Peer,
    session: &mut Script,
    capture: impl Capture + 'static,
    playback: impl Playback + 'static,
) -> Connection {
    let mut connection = connection(session, capture, playback);
    session.push(Action::Connect(peer.choice()));
    until(&mut connection, session, |_, session| session.connected());
    connection
}

fn transcript(utterance: &str, finished: bool) -> ServerMessage {
    ServerMessage::Transcript {
        utterance: utterance.into(),
        text: "hello".into(),
        finished,
    }
}

fn not_transcribed(utterance: &str) -> ServerMessage {
    ServerMessage::TranscriptFailed {
        utterance: utterance.into(),
        error: "Too long.".into(),
    }
}

fn speech(replay: bool) -> ServerMessage {
    ServerMessage::Speech {
        segment: 0,
        message: "m".into(),
        text: "Hello.".into(),
        sample_rate: 24_000,
        replay,
    }
}

/// Say `message` through `peer`, and return what was heard as it arrived.
fn heard(
    peer: &Peer,
    message: ServerMessage,
    connection: &mut Connection,
    session: &mut Script,
) -> Vec<Earcon> {
    peer.say(message, connection, session);
    session.heard()
}

/// Record `utterance`, ending it unless `recording`.
fn record(connection: &mut Connection, session: &mut Script, utterance: &str, recording: bool) {
    let generation = session.generation();
    session.push(Action::BeginUtterance(Utterance::new(
        utterance, generation,
    )));
    if !recording {
        session.push(Action::EndUtterance);
    }
    connection.poll(true, session);
    session.heard();
}

#[test]
fn this_participants_words_are_cued_when_the_server_transcribes_them_or_cannot() {
    let peer = Peer::start();
    let mut session = Script::listening();
    let mut connection = with_peer(&peer, &mut session, Scripted::default(), Nowhere);
    record(&mut connection, &mut session, "u-1", false);
    record(&mut connection, &mut session, "u-2", false);
    let mut heard = |message| heard(&peer, message, &mut connection, &mut session);
    // Partial words, and another participant's, are not cued.
    assert!(heard(transcript("u-1", false)).is_empty());
    assert!(heard(transcript("someone-else", true)).is_empty());
    assert_eq!(heard(transcript("u-1", true)), [Earcon::Sent]);
    assert!(heard(transcript("u-1", true)).is_empty());
    assert_eq!(heard(not_transcribed("u-2")), [Earcon::Failed]);
    // Words already forgotten are not cued.
    assert!(heard(not_transcribed("u-9")).is_empty());
}

#[test]
fn a_tick_or_a_wall_s_change_the_server_refuses_is_cued_as_not_done_and_other_errors_are_not() {
    let peer = Peer::start();
    let mut session = Script::listening();
    let mut connection = with_peer(&peer, &mut session, Scripted::default(), Nowhere);
    let mut heard = |message| heard(&peer, message, &mut connection, &mut session);
    let error = |panel: Option<&str>| ServerMessage::Error {
        error: "That panel changed as you ticked it; look again.".into(),
        space: None,
        panel: panel.map(Into::into),
        wall: None,
    };
    assert_eq!(heard(error(Some("panel-1"))), [Earcon::Failed]);
    assert!(heard(error(None)).is_empty());
    // So is a change to a wall refused as the wall changed since it was taken hold of.
    assert_eq!(
        heard(ServerMessage::Error {
            error: "That wall changed as you changed it; look again.".into(),
            space: None,
            panel: None,
            wall: Some("tomori".into()),
        }),
        [Earcon::Failed]
    );
}

#[test]
fn words_on_their_way_are_lost_when_the_connection_drops() {
    let peer = Peer::start();
    let mut session = Script::listening();
    let mut connection = with_peer(&peer, &mut session, Scripted::default(), Nowhere);
    // Words being recorded are lost, and cued.
    record(&mut connection, &mut session, "u-1", true);
    peer.close(&mut connection, &mut session);
    assert_eq!(session.heard(), [Earcon::Failed]);
    until(&mut connection, &mut session, |_, session| {
        session.connected()
    });
    // Words released may still be queued by the server, but their transcript cannot reach this client: forgotten without a cue.
    record(&mut connection, &mut session, "u-2", false);
    peer.close(&mut connection, &mut session);
    assert!(session.heard().is_empty());
    until(&mut connection, &mut session, |_, session| {
        session.connected()
    });
    assert!(
        heard(
            &peer,
            transcript("u-2", true),
            &mut connection,
            &mut session
        )
        .is_empty()
    );
    // Cancelling or abandoning forgets only the words being recorded, not those still being transcribed.
    record(&mut connection, &mut session, "u-3", false);
    record(&mut connection, &mut session, "u-4", true);
    session.push(Action::CancelUtterance);
    connection.poll(true, &mut session);
    assert!(
        heard(
            &peer,
            transcript("u-4", true),
            &mut connection,
            &mut session
        )
        .is_empty()
    );
    record(&mut connection, &mut session, "u-5", true);
    assert_eq!(
        heard(&peer, not_transcribed("u-5"), &mut connection, &mut session),
        [Earcon::Failed]
    );
    session.push(Action::AbandonUtterance);
    connection.poll(true, &mut session);
    assert_eq!(
        heard(
            &peer,
            transcript("u-3", true),
            &mut connection,
            &mut session
        ),
        [Earcon::Sent]
    );
}

#[test]
fn a_catch_up_nobody_asked_for_is_cued_once() {
    let peer = Peer::start();
    let mut session = Script::listening();
    let mut connection = with_peer(&peer, &mut session, Scripted::default(), Nowhere);
    let (connection, session) = (&mut connection, &mut session);
    assert_eq!(
        heard(&peer, speech(true), connection, session),
        [Earcon::CatchingUp]
    );
    assert!(heard(&peer, speech(true), connection, session).is_empty());
    // Live speech ends it, as does leaving the space.
    assert!(heard(&peer, speech(false), connection, session).is_empty());
    assert_eq!(
        heard(&peer, speech(true), connection, session),
        [Earcon::CatchingUp]
    );
    assert!(heard(&peer, ServerMessage::LEFT, connection, session).is_empty());
    // A replay asked for is not cued, until it is stopped. Each is asked for before the server answers.
    session.push(Action::Send(ClientMessage::Replay { message: None }));
    connection.poll(true, session);
    assert!(heard(&peer, speech(true), connection, session).is_empty());
    session.push(Action::Send(ClientMessage::StopReplay));
    connection.poll(true, session);
    assert_eq!(
        heard(&peer, speech(true), connection, session),
        [Earcon::CatchingUp]
    );
    // A while after its last segment, with nothing playing, it is over, and the next is a catch-up of its own.
    run_for(
        connection,
        session,
        REPLAY_OVER + Duration::from_millis(100),
    );
    assert_eq!(
        heard(&peer, speech(true), connection, session),
        [Earcon::CatchingUp]
    );
    assert!(
        session
            .reports
            .iter()
            .all(|report| *report != Report::Replaying(true))
    );
}

#[test]
fn an_environment_s_ambient_sound_plays_through_the_speaker() {
    let peer = Peer::start();
    let held = Held::default();
    let mut session = Script::default();
    let mut connection = with_peer(&peer, &mut session, Scripted::default(), held.clone());
    until(&mut connection, &mut session, |_, _| held.opened());
    held.flush();
    assert!(!held.heard());
    let sound = crate::playback::Sound::new(vec![1_000.0; 4_800], 48_000);
    connection
        .feedback()
        .ambient(sound, [0.0, 1.6, -1.0], Instant::now());
    connection.poll(true, &mut session);
    assert!((0..10).any(|_| held.heard()));
}

#[test]
fn the_recording_tick_plays_through_the_silencing_only_while_the_live_microphone_is_off() {
    let peer = Peer::start();
    let held = Held::default();
    let mut session = Script::listening();
    let mut connection = with_peer(&peer, &mut session, Scripted::default(), held.clone());
    until(&mut connection, &mut session, |_, _| held.opened());
    held.flush();
    record(&mut connection, &mut session, "u-1", true);
    assert!(held.heard());
    session.push(Action::EndUtterance);
    connection.poll(true, &mut session);
    held.flush();
    // The live microphone is cued when it turns on and off.
    session.push(Action::StartLive(session.generation()));
    connection.poll(true, &mut session);
    assert_eq!(session.heard(), [Earcon::On]);
    // A live microphone would hear the tick, so it is only felt.
    record(&mut connection, &mut session, "u-2", true);
    assert!(!held.heard());
    session.push(Action::EndUtterance);
    session.push(Action::StopLive);
    connection.poll(true, &mut session);
    assert_eq!(session.heard(), [Earcon::Off]);
    // A microphone that is not live does not turn off.
    session.push(Action::StopLive);
    connection.poll(true, &mut session);
    assert!(session.heard().is_empty());
}

#[test]
fn the_others_voices_are_placed_from_their_presence_and_forgotten_on_leaving() {
    let peer = Peer::start();
    let mut session = Script::default();
    let mut connection = with_peer(&peer, &mut session, Scripted::default(), Nowhere);
    let watched = Arc::new(Mutex::new(Vec::new()));
    let watching = watched.clone();
    connection.watch_presence(move |message| {
        if let ServerMessage::Presence { source, .. } = message {
            watching.lock().unwrap().push(*source);
        }
    });
    // Listening as participant 4, as the spectator does, who is then no source.
    connection.listen_as(Some(4));
    let presence = ServerMessage::Presence {
        source: 2,
        head: pose(1.0),
        left: None,
        right: None,
        hands: None,
        pointing: None,
    };
    peer.say(presence, &mut connection, &mut session);
    assert_eq!(*watched.lock().unwrap(), [2]);
    // Nothing is placed until the first listen.
    assert_eq!(*connection.placed(), Scene::default());
    let placed = |connection: &mut Connection| {
        let mut voices = None;
        // This client is participant 1, as the peer welcomed it.
        connection.listen([4, 3, 2, 1, 3], |scene, placed| {
            scene.sources.clear();
            scene
                .sources
                .extend(placed.others.iter().map(|source| (*source, [1.0; 3])));
            scene.mono = true;
            let mut heads: Vec<_> = placed.heads.keys().copied().collect();
            heads.sort_unstable();
            voices = Some((placed.others.to_vec(), heads, placed.mono));
        });
        voices.expect("placed")
    };
    assert_eq!(placed(&mut connection), (vec![2, 3], vec![2], false));
    // What was placed can be read back, as the player was given it.
    assert_eq!(connection.placed().sources, [(2, [1.0; 3]), (3, [1.0; 3])]);
    assert!(connection.placed().mono && connection.placed().listener.is_none());
    peer.say(ServerMessage::LEFT, &mut connection, &mut session);
    assert_eq!(placed(&mut connection), (vec![2, 3], vec![], false));
}

#[test]
fn a_microphone_failure_is_reported_whenever_it_happens() {
    let peer = Peer::start();
    let failed = Arc::new(Mutex::new(Vec::new()));
    let mut session = Script::default();
    let mut connection = with_peer(
        &peer,
        &mut session,
        Scripted {
            failed: failed.clone(),
            ..Default::default()
        },
        Nowhere,
    );
    connection.feedback().set_haptics(true);
    // Between frames.
    record(&mut connection, &mut session, "u-1", true);
    failed.lock().unwrap()[0]("Overrun.".into());
    felt(&mut connection);
    connection.poll(true, &mut session);
    assert_eq!(
        session.reported(&Report::MicrophoneFailed("Overrun.".into())),
        1
    );
    assert_eq!(felt(&mut connection), [Hand::Left, Hand::Left]);
    // The conversation abandons the recording, as it does.
    session.push(Action::CancelUtterance);
    connection.poll(true, &mut session);
    // While the frame takes what the server sent: here, as presence arrives.
    record(&mut connection, &mut session, "u-2", true);
    let failure = failed.clone();
    connection.watch_presence(move |_| failure.lock().unwrap()[1]("Overrun again.".into()));
    let presence = ServerMessage::Presence {
        source: 2,
        head: pose(1.0),
        left: None,
        right: None,
        hands: None,
        pointing: None,
    };
    peer.say(presence, &mut connection, &mut session);
    assert_eq!(
        session.reported(&Report::MicrophoneFailed("Overrun again.".into())),
        1
    );
}

#[test]
fn losing_focus_closes_the_microphone() {
    let peer = Peer::start();
    let capture = Scripted::default();
    let stopped = capture.stopped.clone();
    let mut session = Script::default();
    let mut connection = with_peer(&peer, &mut session, capture, Nowhere);
    record(&mut connection, &mut session, "u-1", true);
    assert_eq!(stopped.load(Ordering::Relaxed), 0);
    connection.poll(false, &mut session);
    assert_eq!(stopped.load(Ordering::Relaxed), 1);
    assert_eq!(session.reported(&Report::Unfocused), 1);
}

#[test]
fn words_whose_capture_fails_as_they_end_are_not_sent() {
    let peer = Peer::start();
    let mut session = Script::listening();
    let capture = Scripted {
        fail_on_stop: Some("The last read failed.".into()),
        ..Default::default()
    };
    let mut connection = with_peer(&peer, &mut session, capture, Nowhere);
    record(&mut connection, &mut session, "u-1", false);
    assert_eq!(
        session.reported(&Report::NotSent("The last read failed.".into())),
        1
    );
    // The cue played at once, as the words were not sent; nothing is cued when their transcript cannot come.
    assert!(heard(&peer, not_transcribed("u-1"), &mut connection, &mut session).is_empty());
}

#[test]
fn a_live_microphone_that_cannot_reopen_after_words_is_reported() {
    let busy = "The microphone is busy.".to_owned();
    let reopening_fails = || Scripted {
        fail: Some("The microphone is busy.".into()),
        fail_from: 2,
        ..Default::default()
    };
    // After words that were sent.
    let peer = Peer::start();
    let mut session = Script::default();
    let mut connection = with_peer(&peer, &mut session, reopening_fails(), Nowhere);
    session.push(Action::StartLive(session.generation()));
    record(&mut connection, &mut session, "u-1", false);
    assert!(session.reported(&Report::MicrophoneFailed(busy.clone())) > 0);
    // After words that were cancelled.
    let peer = Peer::start();
    let mut session = Script::default();
    let mut connection = with_peer(&peer, &mut session, reopening_fails(), Nowhere);
    session.push(Action::StartLive(session.generation()));
    record(&mut connection, &mut session, "u-1", true);
    session.push(Action::CancelUtterance);
    connection.poll(true, &mut session);
    assert_eq!(session.reported(&Report::Failed(busy)), 1);
}

#[test]
fn the_live_microphone_plays_the_speaker_in_its_session_and_the_echo_test_measures_it() {
    crate::test_support::log_everything();
    let peer = Peer::start();
    let held = Held::default();
    let mut session = Script::listening();
    let capture = Scripted {
        session: Some(5),
        ..Default::default()
    };
    let mut connection = connection(&mut session, capture, held.clone());
    // Set from the start, as the headset does, and without needing the session, so that the speaker this test opens, as media, will do.
    connection.set_echo_test("test", false);
    session.push(Action::Connect(peer.choice()));
    until(&mut connection, &mut session, |_, _| held.opened());
    held.flush();
    assert_eq!(held.route(), Some(Route::default()));
    session.push(Action::StartLive(session.generation()));
    until(&mut connection, &mut session, |_, _| {
        held.route()
            == Some(Route {
                communication: Some(5),
            })
    });
    // A second after the speaker plays, the probe plays; it is measured for a while longer.
    until(&mut connection, &mut session, |_, _| held.heard());
    run_for(
        &mut connection,
        &mut session,
        ECHO_HEARD.end + Duration::from_millis(100),
    );
    session.push(Action::StopLive);
    until(&mut connection, &mut session, |_, _| {
        held.route() == Some(Route::default())
    });
}

#[test]
fn an_echo_test_set_while_connected_plays_its_probe_on_the_open_speaker() {
    let peer = Peer::start();
    let held = Held::default();
    let mut session = Script::listening();
    let mut connection = with_peer(&peer, &mut session, Scripted::default(), held.clone());
    until(&mut connection, &mut session, |_, _| held.opened());
    held.flush();
    connection.set_echo_test("test", false);
    session.push(Action::StartLive(session.generation()));
    until(&mut connection, &mut session, |_, _| held.heard());
}

#[test]
fn agent_speech_muted_while_connected_is_not_heard() {
    crate::test_support::log_everything();
    let peer = Peer::start();
    let held = Held::default();
    let mut session = Script::default();
    let mut connection = with_peer(&peer, &mut session, Scripted::default(), held.clone());
    until(&mut connection, &mut session, |_, _| held.opened());
    let heard = |held: &Held| (0..10).any(|_| held.heard());
    session.push(Action::SetAgentMuted(true));
    connection.poll(true, &mut session);
    peer.speak(&mut connection, &mut session);
    assert!(!heard(&held));
    session.push(Action::SetAgentMuted(false));
    connection.poll(true, &mut session);
    peer.speak(&mut connection, &mut session);
    assert!(heard(&held));
    // Leaving the server closes the speaker, logging what it played.
    session.push(Action::Disconnect);
    connection.poll(true, &mut session);
}

#[test]
fn the_voices_chosen_are_played_from_the_first_frame_and_as_they_change() {
    const PERSON: u32 = 7;
    let peer = Peer::start();
    let held = Held::default();
    let mut session = Script::default();
    let mut connection = connection(&mut session, Scripted::default(), held.clone());
    // Chosen before connecting, it holds from the connection's first frame.
    connection.hear(crate::Hearing::Agent);
    assert_eq!(connection.hearing(), crate::Hearing::Agent);
    session.push(Action::Connect(peer.choice()));
    until(&mut connection, &mut session, |_, session| {
        session.connected()
    });
    until(&mut connection, &mut session, |_, _| held.opened());
    // Whether the next second is heard, and whether agent speech plays meanwhile, heard or muted.
    let played = |held: &Held, connection: &Connection| {
        let (mut heard, mut speaking) = (false, false);
        for _ in 0..10 {
            heard |= held.heard();
            speaking |= connection.speaker.agent_speech().0;
        }
        held.flush();
        (heard, speaking)
    };
    let (connection, session) = (&mut connection, &mut session);
    // Each stream numbered on from the last, so that none is taken for late frames of the one before.
    for (from, (hearing, person, agent)) in (0..).step_by(10).zip([
        (None, false, true),
        (Some(crate::Hearing::Nobody), false, false),
        (Some(crate::Hearing::All), true, true),
    ]) {
        if let Some(hearing) = hearing {
            connection.hear(hearing);
        }
        peer.speak_as(PERSON, from, connection, session);
        assert_eq!(played(&held, connection), (person, false), "{hearing:?}");
        // Agent speech that is not played is muted rather than dropped, so it still plays, for the agent to be seen speaking.
        peer.speak_as(tomori_protocol::AGENT_SOURCE, from, connection, session);
        assert_eq!(played(&held, connection), (agent, true), "{hearing:?}");
    }
}

#[test]
fn agent_speech_skipped_while_connected_is_not_heard_until_no_longer_skipped() {
    let peer = Peer::start();
    let held = Held::default();
    let mut session = Script::default();
    let mut connection = with_peer(&peer, &mut session, Scripted::default(), held.clone());
    until(&mut connection, &mut session, |_, _| held.opened());
    let heard = |held: &Held| (0..10).any(|_| held.heard());
    session.push(Action::SetAgentSkipped(true));
    connection.poll(true, &mut session);
    peer.speak(&mut connection, &mut session);
    assert!(!heard(&held));
    session.push(Action::SetAgentSkipped(false));
    connection.poll(true, &mut session);
    peer.speak(&mut connection, &mut session);
    assert!(heard(&held));
}

#[test]
fn a_speaker_that_fails_once_open_is_reported_once_and_tried_again_later() {
    let peer = Peer::start();
    let mut session = Script::default();
    let mut connection = with_peer(&peer, &mut session, Scripted::default(), Vanishes);
    let failed = Report::Failed("The speaker went away. Replies cannot be heard.".into());
    until(&mut connection, &mut session, |_, session| {
        session.reported(&failed) > 0
    });
    run_for(&mut connection, &mut session, Duration::from_millis(50));
    assert_eq!(session.reported(&failed), 1);
}

#[test]
fn a_connection_that_cannot_start_is_reported_and_tried_again_when_chosen_again() {
    let peer = Peer::start();
    let mut session = Script::default();
    let mut connection = connection(&mut session, Scripted::default(), Nowhere);
    crate::system::refuse(crate::system::SCHEDULER);
    session.push(Action::Connect(peer.choice()));
    connection.poll(true, &mut session);
    assert_eq!(
        session.reported(&Report::Failed(
            "Cannot start a connection: the system refused to start scheduler".into()
        )),
        1
    );
    // Still usable: choosing the server again connects.
    session.push(Action::Connect(peer.choice()));
    until(&mut connection, &mut session, |_, session| {
        session.connected()
    });
}

#[test]
fn a_server_that_is_not_there_is_retried_without_a_report() {
    let mut session = Script::default();
    let mut connection = connection(&mut session, Scripted::default(), Nowhere);
    session.push(Action::Connect(Choice::new(
        "Nobody",
        "127.0.0.1:9",
        Source::Discovered,
    )));
    // Nothing listens there, so each attempt fails at once.
    until(&mut connection, &mut session, |_, session| {
        session.count(|event| matches!(event, Event::State(ConnectionState::Disconnected { .. })))
            > 0
    });
    assert!(
        session.reports.iter().all(|report| matches!(
            report,
            Report::Feedback(_)
                | Report::Tick { .. }
                | Report::Replaying(false)
                | Report::AgentSpeech { playing: false, .. }
                | Report::Voices(_)
        )),
        "{:?}",
        session.reports
    );
}

#[test]
fn a_discovered_server_is_remembered_once_connected_and_closed_when_left() {
    let peer = Peer::start();
    let directory = directory("remembered");
    let mut session = Script::default();
    let mut connection = kept(
        &mut session,
        Store::new(Some(directory.clone())),
        None,
        None,
        Scripted::default(),
        Nowhere,
    );
    let mut choice = peer.choice();
    choice.instance = Some("studio".into());
    session.push(Action::Connect(choice));
    until(&mut connection, &mut session, |_, session| {
        session.connected()
    });
    assert_eq!(
        Store::new(Some(directory.clone())).server().as_deref(),
        Some("studio")
    );
    // Found again, it is listed as the one remembered.
    session.browsing = true;
    connection.browse_servers = finds_studio;
    connection.poll(true, &mut session);
    let listed = session
        .reports
        .iter()
        .rev()
        .find_map(|report| match report {
            Report::Servers(servers) => Some(servers.clone()),
            _ => None,
        });
    assert!(
        listed
            .unwrap()
            .iter()
            .any(|choice| choice.label == "Studio" && choice.source == Source::Remembered)
    );
    // Leaving the server closes the connection.
    session.push(Action::Disconnect);
    connection.poll(true, &mut session);
    let deadline = Instant::now() + Duration::from_secs(10);
    while peer.ended.load(Ordering::Relaxed) == 0 {
        assert!(Instant::now() < deadline, "timed out");
        std::thread::sleep(Duration::from_millis(2));
    }
    let _ = std::fs::remove_dir_all(directory);
}

#[test]
fn what_waits_for_the_participant_is_cued_as_the_session_observes_it() {
    let mut session = Script {
        requests: vec!["p1".into()],
        knocks: vec!["k1".into()],
        ..Script::listening()
    };
    let mut connection = connection(&mut session, Scripted::default(), Nowhere);
    connection.poll(true, &mut session);
    assert_eq!(session.heard(), [Earcon::Request, Earcon::Knock]);
    connection.poll(true, &mut session);
    assert!(session.heard().is_empty());
}

#[test]
fn a_server_that_refuses_even_to_pair_admits_only_tokens() {
    crate::test_support::log_everything();
    // As a server from before pairing would: asked to pair, it refuses again.
    let peer = Peer::serving(true);
    let mut session = Script::default();
    let mut connection = connection(&mut session, Scripted::default(), Nowhere);
    session.push(Action::Connect(peer.choice()));
    let notice = Report::SecurityNotice("This server admits only devices with a token.".into());
    until(&mut connection, &mut session, |_, session| {
        session.reported(&notice) > 0
    });
}

/// A session that says only what to do, as a bot without a server list or anything to show might.
#[derive(Default)]
struct Quiet {
    actions: VecDeque<Action>,
    reports: Vec<Report>,
}

impl Session for Quiet {
    fn next_action(&mut self) -> Option<Action> {
        self.actions.pop_front()
    }

    fn apply(&mut self, _event: Event) {}

    fn report(&mut self, report: Report) {
        self.reports.push(report);
    }
}

#[test]
fn a_session_without_a_server_list_never_browses_and_observes_nothing() {
    let lock = Lock::default();
    let mut session = Quiet::default();
    let mut connection = Connection::new(
        identity(&Store::new(None), None, None),
        Store::new(None),
        Box::new(lock.clone()),
        Box::new(Scripted::default()),
        Box::new(Nowhere),
        &mut session,
    );
    connection.poll(true, &mut session);
    assert_eq!(lock.held(), None);
    assert!(session.reports.iter().any(|report| matches!(
        report,
        Report::Tick {
            browsing: false,
            ..
        }
    )));
    assert_eq!(connection.feedback().take_earcons().count(), 0);
}

/// Ada's connection to `peer`, keeping what she learns in `directory`, connected.
fn keeping_with_peer(peer: &Peer, session: &mut Script, directory: &std::path::Path) -> Connection {
    let mut connection = kept(
        session,
        Store::new(Some(directory.to_owned())),
        None,
        None,
        Scripted::default(),
        Nowhere,
    );
    session.push(Action::Connect(peer.choice()));
    until(&mut connection, session, |_, session| session.connected());
    connection
}

#[test]
fn a_token_is_ignored_without_a_connection_over_tls_to_keep_it_for() {
    crate::test_support::log_everything();
    let peer = Peer::start();
    let directory = directory("plain-token");
    let mut session = Script::default();
    let mut connection = keeping_with_peer(&peer, &mut session, &directory);
    let credential = || Store::new(Some(directory.clone())).credential(&peer.address);
    let tokens = || {
        [
            Action::Paired {
                token: "tomori_owner".into(),
            },
            Action::Admitted {
                token: "tomori_collaborator".into(),
            },
        ]
    };
    // Over plain HTTP the server presented no certificate, so neither token is kept, and the connection stays as it is.
    session.reports.clear();
    session.actions.extend(tokens());
    connection.poll(true, &mut session);
    assert_eq!(credential(), Default::default());
    assert!(connection.client.is_some());
    assert_eq!(peer.ended.load(Ordering::Relaxed), 0);
    assert!(session.reports.iter().all(|report| matches!(
        report,
        Report::Tick { .. } | Report::Replaying(_) | Report::AgentSpeech { .. } | Report::Voices(_)
    )));
    // Nor while connecting, before the server has presented anything.
    session.push(Action::Connect(peer.choice()));
    session.actions.extend(tokens());
    connection.poll(true, &mut session);
    assert!(connection.link.as_ref().unwrap().certificate.is_none());
    assert_eq!(credential(), Default::default());
    // Nor once the connection is closed, in the same frame as the session asks.
    until(&mut connection, &mut session, |_, session| {
        session.connected()
    });
    session.push(Action::Disconnect);
    session.actions.extend(tokens());
    connection.poll(true, &mut session);
    assert!(connection.client.is_none() && connection.link.is_none());
    assert_eq!(credential(), Default::default());
    let _ = std::fs::remove_dir_all(directory);
}

#[test]
fn an_admission_over_a_certificate_other_than_the_one_kept_is_ignored() {
    let peer = Peer::start();
    let directory = directory("admitted-elsewhere");
    let mut session = Script::default();
    let mut connection = keeping_with_peer(&peer, &mut session, &directory);
    let kept = Fingerprint::of(b"the server kept");
    connection
        .store
        .remember_certificate(&peer.address, &kept.to_string());
    // As though the connection had presented another certificate, which a client pinned to the one kept never accepts.
    connection.link.as_mut().unwrap().certificate = Some(Fingerprint::of(b"another server"));
    let admitted = Action::Admitted {
        token: "tomori_collaborator".into(),
    };
    session.push(admitted.clone());
    connection.poll(true, &mut session);
    assert_eq!(connection.store.credential(&peer.address).token, None);
    // Over the certificate kept, it is kept, and pinned from then on.
    connection.link.as_mut().unwrap().certificate = Some(kept);
    session.push(admitted);
    connection.poll(true, &mut session);
    assert_eq!(
        Store::new(Some(directory.clone()))
            .credential(&peer.address)
            .token
            .as_deref(),
        Some("tomori_collaborator")
    );
    let link = connection.link.as_ref().unwrap();
    assert!(link.pinned && link.token.is_some());
    assert_eq!(link.transport, Transport::Tls { pin: Some(kept) });
    let _ = std::fs::remove_dir_all(directory);
}

/// Against a server, as a headset takes part in its spaces.
mod served {
    use super::*;
    use tomori_server::test_support::speech;

    /// A server on a loopback port that serves only TLS and admits by tokens, and anyone else as a visitor if `visitors`, pairing devices if `pairing`, with scripted agent sessions.
    struct Server {
        server: tomori_server::test_support::TestServer,
        pin: Fingerprint,
        /// Where the operator confirms a device's pairing code, on a server that pairs.
        pairing: Option<tomori_server::Pairing>,
    }

    impl Server {
        fn start() -> Self {
            Self::with(true, false)
        }

        fn with(visitors: bool, pairing: bool) -> Self {
            let certificate = tomori_security::ServerCertificate::generate();
            let pin = certificate.fingerprint();
            let config = certificate
                .server_config()
                .expect("the certificate is usable");
            let (sender, handle) = mpsc::channel();
            let server =
                tomori_server::test_support::TestServer::with_tokens(visitors, move |server| {
                    let server = server.with_tls(config);
                    let server = if pairing {
                        server.with_pairing(pin)
                    } else {
                        server
                    };
                    let _ = sender.send(pairing.then(|| server.pairing()));
                    server
                });
            let pairing = handle.recv().expect("the server starts");
            Self {
                server,
                pin,
                pairing,
            }
        }

        fn address(&self) -> String {
            self.server.address.to_string()
        }

        /// A token naming `name` as an owner.
        fn owner(&self, name: &str) -> String {
            self.server.issue(name, tomori_security::Grant::Owner).1
        }

        /// Ada's connection, keeping what she learns in `store`, connected to this server at once with `token`.
        fn participant(
            &self,
            session: &mut Script,
            token: Option<String>,
            capture: impl Capture + 'static,
            store: Store,
        ) -> Connection {
            kept(
                session,
                store,
                Some(self.address()),
                token,
                capture,
                Nowhere,
            )
        }

        /// Another client in this server's spaces, named `name`, with `token`, or as a visitor without one.
        fn remote(&self, name: &str, token: Option<String>) -> Remote {
            let mut options = Options::new(self.address(), name.to_lowercase(), name.to_owned());
            options.token = token;
            options.transport = Transport::Tls {
                pin: Some(self.pin),
            };
            Remote {
                client: Client::connect(options).expect("the client starts"),
            }
        }
    }

    /// The session applies the connection's own [`Report::Connecting`], as a conversation connects.
    fn connecting(session: &mut Script) {
        let choice = session.reports.iter().find_map(|report| match report {
            Report::Connecting(choice) => Some(choice.clone()),
            _ => None,
        });
        session.push(Action::Connect(choice.expect("a server was given")));
    }

    /// A client of the [`Server`], other than the one tested.
    struct Remote {
        client: Client,
    }

    impl Remote {
        fn send(&self, message: ClientMessage) {
            self.client.send(message).expect("the message is queued");
        }

        /// The first message `select` chooses, waiting for it, while `meanwhile` runs before each look at the events, such as the frames of the client tested. It runs at least once, however soon the message arrives.
        fn wait_for<T>(
            &self,
            mut meanwhile: impl FnMut(),
            mut select: impl FnMut(&ServerMessage) -> Option<T>,
        ) -> T {
            let deadline = Instant::now() + Duration::from_secs(10);
            loop {
                assert!(Instant::now() < deadline, "timed out");
                meanwhile();
                while let Some(event) = self.client.try_event() {
                    if let Event::Message(message) = event
                        && let Some(selected) = select(&message)
                    {
                        return selected;
                    }
                }
                std::thread::sleep(Duration::from_millis(2));
            }
        }

        /// Create a space with the scripted backend, and wait for its first snapshot.
        fn create_space(&self, title: &str) -> String {
            self.send(ClientMessage::CreateSpace {
                title: title.to_owned(),
                backend: tomori_server::fake::FAKE_BACKEND_NAME.to_owned(),
                directory: "/tmp".to_owned(),
                session: None,
                environment: None,
                instructions: None,
            });
            self.wait_for(
                || {},
                |message| match message {
                    ServerMessage::Space { state } if state.title == title => {
                        Some(state.id.clone())
                    }
                    _ => None,
                },
            )
        }
    }

    /// A microphone that hears `samples`, once, each time it opens, then nothing until it is closed; live or for
    /// push-to-talk alike. It hears them all before opening returns, as a platform may call back with the first samples
    /// at once, so that they are heard only if they are kept until the microphone is set to what it was opened for.
    struct Talker {
        samples: Vec<i16>,
    }

    impl Capture for Talker {
        fn open(&mut self, mut samples: Samples, _failed: Failed) -> Result<Recording, String> {
            // Halfway between samples, so that converting back to 16 bits gives each sample exactly.
            let speech: Vec<f32> = self
                .samples
                .iter()
                .map(|sample| (f32::from(*sample) + 0.5) / f32::from(i16::MAX))
                .collect();
            for chunk in speech.chunks(320) {
                samples(chunk);
            }
            Ok(Recording::new(16_000, ()))
        }

        fn open_live(
            &mut self,
            samples: Samples,
            failed: Failed,
            _active: Active,
        ) -> Result<Recording, String> {
            self.open(samples, failed)
        }
    }

    /// Join `space`, as the lobby's accept does.
    fn join(connection: &mut Connection, session: &mut Script, space: &str) {
        session.push(Action::Send(ClientMessage::Join {
            space: space.to_owned(),
        }));
        until(connection, session, |_, session| {
            session.count(|event| {
                matches!(event, Event::Message(ServerMessage::Space { state }) if state.id == space)
            }) > 0
        });
    }

    /// Ada, an owner who hears `capture`, in Bea's space on `server`.
    fn in_space(
        server: &Server,
        session: &mut Script,
        capture: impl Capture + 'static,
    ) -> (Remote, Connection) {
        let bea = server.remote("Bea", Some(server.owner("Bea")));
        let space = bea.create_space("Studio");
        let mut connection = server.participant(
            session,
            Some(server.owner("Ada")),
            capture,
            Store::new(None),
        );
        connecting(session);
        join(&mut connection, session, &space);
        (bea, connection)
    }

    fn role_becomes(connection: &mut Connection, session: &mut Script, role: ConnectionRole) {
        until(connection, session, |_, session| {
            session.role() == Some(role)
        });
    }

    #[test]
    fn a_name_the_server_does_not_keep_is_suggested_again_for_the_rest_of_the_run() {
        let server = Server::start();
        let mut session = Script::listening();
        // A visitor, whose name the server keeps only while it keeps them.
        let mut connection = server.participant(
            &mut session,
            None,
            Talker { samples: vec![] },
            Store::new(None),
        );
        connecting(&mut session);
        until(&mut connection, &mut session, |_, session| {
            session.connected()
        });
        session.push(Action::Send(ClientMessage::SetName { name: "Vic".into() }));
        until(&mut connection, &mut session, |_, session| {
            session.count(|event| {
                matches!(event, Event::Message(ServerMessage::Name { name }) if name == "Vic")
            }) > 0
        });
        // Leaving the server and choosing it again starts a new client, which suggests the name.
        session.push(Action::Disconnect);
        run_for(&mut connection, &mut session, Duration::from_millis(20));
        connecting(&mut session);
        until(&mut connection, &mut session, |_, session| {
            session.count(|event| {
                matches!(
                    event,
                    Event::State(ConnectionState::Connected { name: Some(name), .. }) if name == "Vic"
                )
            }) > 0
        });
    }

    #[test]
    fn words_said_while_x_is_held_reach_the_space_and_are_cued_as_sent() {
        let server = Server::start();
        let mut session = Script::listening();
        let talker = Talker {
            samples: speech("hello"),
        };
        let (bea, mut connection) = in_space(&server, &mut session, talker);
        record(&mut connection, &mut session, "u-1", true);
        run_for(&mut connection, &mut session, Duration::from_millis(50));
        session.push(Action::EndUtterance);
        let said = bea.wait_for(
            || connection.poll(true, &mut session),
            |message| match message {
                ServerMessage::Space { state } => state
                    .messages
                    .iter()
                    .find(|message| message.text.starts_with("Ada: "))
                    .map(|message| message.text.clone()),
                _ => None,
            },
        );
        assert_eq!(said, "Ada: hello");
        until(&mut connection, &mut session, |_, session| {
            session.heard().contains(&Earcon::Sent)
        });
    }

    #[test]
    fn the_live_microphone_is_heard_by_the_space_and_turned_off() {
        let server = Server::start();
        let mut session = Script::listening();
        let talker = Talker {
            samples: speech(&"a".repeat(1_000)),
        };
        let (bea, mut connection) = in_space(&server, &mut session, talker);
        session.push(Action::StartLive(session.generation()));
        until(&mut connection, &mut session, |_, _| {
            bea.client.try_audio().is_some()
        });
        session.push(Action::StopLive);
        until(&mut connection, &mut session, |_, session| {
            session.heard().contains(&Earcon::Off)
        });
    }

    #[test]
    fn presence_carries_the_controllers() {
        let server = Server::start();
        let mut session = Script::default();
        let (bea, mut connection) = in_space(&server, &mut session, Scripted::default());
        connection.send_presence(
            pose(0.0),
            Some(pose(-0.2)),
            Some(pose(0.2)),
            [None, None],
            &["panel-1".to_owned()],
            [true, true],
            [
                None,
                Some(tomori_protocol::Ray {
                    aim: pose(0.25),
                    length: 2.0,
                    meets: true,
                }),
            ],
        );
        let (left, right, pointing) = bea.wait_for(
            || connection.poll(true, &mut session),
            |message| match message {
                ServerMessage::Presence {
                    left,
                    right,
                    pointing,
                    ..
                } => Some((*left, *right, pointing.as_deref().copied())),
                _ => None,
            },
        );
        assert_eq!(left.map(|pose| pose.position[0]), Some(-0.2));
        assert_eq!(right.map(|pose| pose.position[0]), Some(0.2));
        // The right ray, drawn in full, is relayed from its aim.
        let [left_ray, right_ray] = pointing.expect("a ray drawn in full");
        assert_eq!(left_ray, None);
        assert_eq!(
            right_ray.map(|ray| (ray.aim.position[0], ray.length)),
            Some((0.25, 2.0))
        );
    }

    #[test]
    fn a_token_given_at_startup_that_the_server_refuses_is_dropped() {
        let server = Server::start();
        let mut session = Script::default();
        let mut connection = server.participant(
            &mut session,
            Some("tomori_not_a_token".into()),
            Scripted::default(),
            Store::new(None),
        );
        connecting(&mut session);
        role_becomes(&mut connection, &mut session, ConnectionRole::Visitor);
        assert_eq!(
            session.reported(&Report::SecurityNotice(
                "This server no longer admits this device, so it watches as a visitor.".into()
            )),
            1
        );
    }

    #[test]
    fn a_kept_token_that_the_server_refuses_is_forgotten() {
        let server = Server::start();
        let directory = directory("revoked");
        Store::new(Some(directory.clone())).remember_token(&server.address(), "tomori_revoked");
        let mut session = Script::default();
        let mut connection = server.participant(
            &mut session,
            None,
            Scripted::default(),
            Store::new(Some(directory.clone())),
        );
        connecting(&mut session);
        role_becomes(&mut connection, &mut session, ConnectionRole::Visitor);
        let kept = Store::new(Some(directory.clone())).credential(&server.address());
        assert_eq!(kept.token, None);
        let _ = std::fs::remove_dir_all(directory);
    }

    #[test]
    fn a_server_that_refuses_visitors_is_asked_to_pair_and_pairing_makes_an_owner() {
        let server = Server::with(false, true);
        let directory = directory("paired");
        let mut session = Script::default();
        let mut connection = server.participant(
            &mut session,
            None,
            Scripted::default(),
            Store::new(Some(directory.clone())),
        );
        connecting(&mut session);
        role_becomes(&mut connection, &mut session, ConnectionRole::Pairing);
        pair(&server, &mut connection, &mut session, |_, session| {
            session.role() == Some(ConnectionRole::Owner)
        });
        let kept = Store::new(Some(directory.clone())).credential(&server.address());
        assert!(kept.token.is_some());
        assert_eq!(kept.certificate, Some(server.pin.to_string()));
        let _ = std::fs::remove_dir_all(directory);
    }

    /// Pair as the participant does, the operator opening pairing, which is closed by default, and confirming the code on the server's console, and wait for `done`.
    fn pair(
        server: &Server,
        connection: &mut Connection,
        session: &mut Script,
        done: impl FnMut(&mut Connection, &mut Script) -> bool,
    ) {
        let pairing = server.pairing.as_ref().unwrap();
        pairing.open().unwrap();
        session.pair();
        let mut code = None;
        until(connection, session, |_, _| {
            code = pairing.pending().and_then(|pending| pending.code);
            code.is_some()
        });
        let digits: String = code.unwrap().chars().filter(char::is_ascii_digit).collect();
        assert_eq!(
            pairing.confirm(&digits),
            Ok(tomori_security::pairing::ChosenName::new("Ada"))
        );
        until(connection, session, done);
    }

    #[test]
    fn leaving_the_server_as_pairing_completes_keeps_no_token() {
        let server = Server::with(false, true);
        let directory = directory("left-pairing");
        let mut session = Script::default();
        let mut connection = server.participant(
            &mut session,
            None,
            Scripted::default(),
            Store::new(Some(directory.clone())),
        );
        connecting(&mut session);
        role_becomes(&mut connection, &mut session, ConnectionRole::Pairing);
        session.leaving = true;
        pair(&server, &mut connection, &mut session, |_, session| {
            session.count(|event| matches!(event, Event::Message(ServerMessage::Paired { .. }))) > 0
        });
        // The participant left in the frame the token arrived: it is not kept, and nothing connects again.
        assert!(connection.client.is_none() && connection.link.is_none());
        let kept = Store::new(Some(directory.clone())).credential(&server.address());
        assert_eq!(kept.token, None);
        assert_eq!(kept.certificate, Some(server.pin.to_string()));
        let _ = std::fs::remove_dir_all(directory);
    }

    #[test]
    fn pairing_with_a_distrusted_server_replaces_its_certificate() {
        let server = Server::with(true, true);
        let directory = directory("distrusted-paired");
        let other = Fingerprint::of(b"another server");
        Store::new(Some(directory.clone()))
            .remember_certificate(&server.address(), &other.to_string());
        let mut session = Script::default();
        let mut connection = server.participant(
            &mut session,
            None,
            Scripted::default(),
            Store::new(Some(directory.clone())),
        );
        connecting(&mut session);
        role_becomes(&mut connection, &mut session, ConnectionRole::Visitor);
        assert_eq!(session.certificate(), Some(server.pin));
        pair(&server, &mut connection, &mut session, |_, session| {
            session.role() == Some(ConnectionRole::Owner)
        });
        let kept = Store::new(Some(directory.clone())).credential(&server.address());
        assert!(kept.token.is_some());
        assert_eq!(kept.certificate, Some(server.pin.to_string()));
        let _ = std::fs::remove_dir_all(directory);
    }

    #[test]
    fn a_server_that_does_not_present_the_certificate_it_advertises_is_pinned_to_neither() {
        let server = Server::start();
        let directory = directory("misadvertised");
        let mut session = Script::default();
        let mut connection = kept(
            &mut session,
            Store::new(Some(directory.clone())),
            None,
            None,
            Scripted::default(),
            Nowhere,
        );
        let mut choice = Choice::new("Studio", server.address(), Source::Discovered);
        choice.fingerprint = Some(Fingerprint::of(b"another server").to_string());
        session.push(Action::Connect(choice));
        role_becomes(&mut connection, &mut session, ConnectionRole::Visitor);
        assert_eq!(
            session.reported(&Report::SecurityNotice(
                "This server's identity has changed, so this device watches as a visitor and sends it no token.".into()
            )),
            1
        );
        let kept = Store::new(Some(directory.clone())).credential(&server.address());
        assert_eq!(kept, Default::default());
        let _ = std::fs::remove_dir_all(directory);
    }

    #[test]
    fn a_server_whose_certificate_changed_is_distrusted_and_sent_no_token() {
        let server = Server::start();
        let directory = directory("distrusted");
        let other = Fingerprint::of(b"another server");
        Store::new(Some(directory.clone()))
            .remember_certificate(&server.address(), &other.to_string());
        let mut session = Script::default();
        let mut connection = server.participant(
            &mut session,
            Some(server.owner("Ada")),
            Scripted::default(),
            Store::new(Some(directory.clone())),
        );
        connecting(&mut session);
        role_becomes(&mut connection, &mut session, ConnectionRole::Visitor);
        assert_eq!(
            session.reported(&Report::SecurityNotice(
                "This server's identity has changed, so this device watches as a visitor and sends it no token.".into()
            )),
            1
        );
        // What was kept is not replaced by a server that cannot be trusted.
        let kept = Store::new(Some(directory.clone())).credential(&server.address());
        assert_eq!(kept.certificate, Some(other.to_string()));
        let _ = std::fs::remove_dir_all(directory);
    }

    #[test]
    fn an_admitted_knock_keeps_the_collaborators_token_for_next_time() {
        let server = Server::start();
        let bea = server.remote("Bea", Some(server.owner("Bea")));
        let space = bea.create_space("Studio");
        let directory = directory("admitted");
        let mut session = Script::default();
        let mut connection = server.participant(
            &mut session,
            None,
            Scripted::default(),
            Store::new(Some(directory.clone())),
        );
        connecting(&mut session);
        join(&mut connection, &mut session, &space);
        session.push(Action::Send(ClientMessage::Knock));
        let knock = bea.wait_for(
            || connection.poll(true, &mut session),
            |message| match message {
                ServerMessage::Knocks { knocks } => knocks.first().map(|knock| knock.id.clone()),
                _ => None,
            },
        );
        bea.send(ClientMessage::DecideKnock { knock, admit: true });
        let token = || {
            Store::new(Some(directory.clone()))
                .credential(&server.address())
                .token
        };
        until(&mut connection, &mut session, |_, _| token().is_some());
        // An admission never replaces a token already kept.
        let kept = token();
        session.push(Action::Admitted {
            token: "tomori_another".into(),
        });
        connection.poll(true, &mut session);
        assert_eq!(token(), kept);
        let _ = std::fs::remove_dir_all(&directory);
    }

    #[test]
    fn a_token_kept_for_the_same_server_under_another_name_is_presented() {
        let server = Server::start();
        let directory = directory("renamed");
        let store = Store::new(Some(directory.clone()));
        let pin = server.pin.to_string();
        store.remember_certificate(&server.address(), &pin);
        store.remember_certificate("studio", &pin);
        store.remember_token("studio", &server.owner("Ada"));
        let mut session = Script::default();
        let mut connection = server.participant(&mut session, None, Scripted::default(), store);
        connecting(&mut session);
        role_becomes(&mut connection, &mut session, ConnectionRole::Owner);
        let _ = std::fs::remove_dir_all(directory);
    }

    #[test]
    fn a_token_given_without_a_server_is_presented_only_to_one_reached_over_usb() {
        let server = Server::start();
        let directory = directory("usb");
        let mut session = Script::default();
        let mut connection = kept(
            &mut session,
            Store::new(Some(directory.clone())),
            None,
            Some(server.owner("Ada")),
            Scripted::default(),
            Nowhere,
        );
        // Anyone on the network could advertise a server, so a discovered one is not given it.
        session.push(Action::Connect(Choice::new(
            "Studio",
            server.address(),
            Source::Discovered,
        )));
        role_becomes(&mut connection, &mut session, ConnectionRole::Visitor);
        session.push(Action::Connect(Choice::new(
            "USB",
            server.address(),
            Source::Usb,
        )));
        role_becomes(&mut connection, &mut session, ConnectionRole::Owner);
        // Kept for that server from then on.
        let kept = Store::new(Some(directory.clone())).credential(&server.address());
        assert!(kept.token.is_some());
        let _ = std::fs::remove_dir_all(directory);
    }
}
