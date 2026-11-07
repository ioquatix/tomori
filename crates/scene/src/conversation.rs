// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

//! The panel's conversation with a Tomori server: choosing a server, the lobby, then the joined space.
//!
//! This is pure state. It applies [`tomori_client::Event`]s and controller input, and queues [`Action`]s for a [`tomori_client::connection::Connection`] to carry out, as its [`Session`], so nothing here touches the network.
use crate::{
    arrival,
    caption::{COLUMNS, ROWS, wrapped_lines},
    environment::Environment,
    interaction::SessionActivity,
    locomotion::Locomotion,
};
#[cfg(any(not(target_os = "android"), test))]
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tomori_client::{
    ConnectionState, Event, UtteranceFailure,
    connection::{Action, Report, Session, Utterance},
    feedback,
    server_choice::{self, Choice},
};
use tomori_protocol::{
    Activity, BackendInfo, ClientMessage, ConnectionRole, Decision, Engagement, EnvironmentInfo,
    QueueState, Role, ServerMessage, SessionSummary, SpaceState, SpaceSummary, SpeechSetting,
    WorkspaceRoot,
};

const BODY_ROWS: usize = ROWS - 4;

/// The longest instructions an owner gives a space, as the server limits them.
const INSTRUCTIONS_BYTES: usize = 4096;

/// Said when a visitor tries to talk or act in the space they are watching. See `design/security.md`.
const VISITING: &str = "Visitors can watch and listen, but not talk or act here.";
/// Shown while the platform's request for the microphone is unanswered. See `design/arrival.md`.
pub const MICROPHONE_REQUEST: &str = "Waiting for microphone permission...";

/// How long B must be held to leave a space, so that a press meant for something else never leaves. See `design/interaction.md`.
pub const LEAVE_HOLD: Duration = Duration::from_secs(1);

/// How long a queued entry's cancellation can be undone before it is sent.
pub const CANCEL_UNDO: Duration = Duration::from_secs(3);

/// How long A does nothing after an approval or question appears, so that a press meant for what was shown before it, such as undoing a cancellation, never decides it.
pub const REQUEST_GRACE: Duration = Duration::from_millis(600);

/// The most panel rows the problem line and the queue strip each take.
const PROBLEM_ROWS: usize = 2;
const QUEUE_ROWS: usize = 2;

const LOST: &str = "Connection lost; your words were not sent.";

/// How long after the remembered server is found the headset waits before connecting to it automatically, so that other servers browsing finds at about the same time are known, and one that needs attention can hold the connection back. See `design/arrival.md`.
pub const AUTOMATIC_SETTLE: Duration = Duration::from_millis(1500);

/// Pairing this device as an owner, from choosing the pairing object until the server confirms or refuses it. See `design/security.md`.
#[derive(Clone, Debug, PartialEq)]
enum Pairing {
    /// Committed to this value, waiting for the server's.
    Committed(tomori_security::pairing::Nonce, Instant),
    /// Both sides have the code, written for reading, which waits to be confirmed on the server.
    Waiting(String, Instant),
}

/// The problem shown when words that said a new name did not change it, for `reason`.
fn name_not_changed(reason: &str) -> String {
    format!("Name not changed: {reason}")
}

/// How long the device waits for a pairing to be confirmed before it gives up itself: the server's timeout and a margin. A pairing the device gives up on is cancelled, so one choice of the pairing object allows the server one try. See `design/security.md`.
const PAIRING_PATIENCE: Duration = Duration::from_secs(135);

/// Push-to-talk, from pressing X until the server's transcript or failure. See `design/speech.md`.
#[derive(Clone, Debug, PartialEq)]
enum Talk {
    /// X is held and the microphone is open, sending `agent` audio.
    Recording {
        utterance: String,
        started: Instant,
        /// The partial transcript so far.
        heard: String,
        /// Whether the words answer the presented question rather than being queued.
        answering: bool,
        /// Whether the words say the participant's new display name rather than being queued.
        naming: bool,
        /// The worker or card pointed at as X was pressed, which the utterance began attending to, unless it answers
        /// the presented question. See `design/interaction.md`.
        pointed: Option<tomori_protocol::Resource>,
    },
    /// X was released; waiting for the server's final transcript.
    Sending {
        utterance: String,
        heard: String,
        answering: bool,
        /// What the server last said it is doing with the words, once it has them all; `None` until then, and
        /// throughout from a server that does not say. See `design/speech.md`.
        received: Option<crate::recording::Received>,
        naming: bool,
    },
}

/// What the transcript was laid out from, besides the space's revision.
#[derive(Clone, Debug, PartialEq)]
struct TranscriptKey {
    revision: u64,
    wrist: crate::transcript::State,
    role: ConnectionRole,
    source: Option<u32>,
    deciding: bool,
    /// The entry being removed, and the undo time left in sixtieths.
    cancelling: Option<(String, u32)>,
    problem: Option<String>,
    answerable: bool,
    hands_only: bool,
    grace: bool,
    columns: usize,
}

/// What became of the last words sent, which the recording indicator shows for a moment, or until dismissed for a failure. See `design/hand-menu.md`.
#[derive(Clone, Debug, PartialEq)]
enum Outcome {
    /// Transcribed as `text`, and queued, or applied as an answer.
    Sent { answered: bool, text: String },
    /// Taken as the participant's new display name.
    Named(String),
    /// Not sent, with why, which is also the problem shown.
    Failed(String),
    /// Not sent, as no speech was heard in it: nothing was lost, so the recording card shows it briefly, and it is no problem. See `design/speech.md`.
    Unheard,
    /// Words that said a new name did not change it, with why; the problem shown says so, and X says the name again until B cancels. See `design/hand-menu.md`.
    NameFailed(String),
}

impl Talk {
    fn utterance(&self) -> &str {
        match self {
            Talk::Recording { utterance, .. } | Talk::Sending { utterance, .. } => utterance,
        }
    }

    fn answering(&self) -> bool {
        match self {
            Talk::Recording { answering, .. } | Talk::Sending { answering, .. } => *answering,
        }
    }

    fn naming(&self) -> bool {
        match self {
            Talk::Recording { naming, .. } | Talk::Sending { naming, .. } => *naming,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Servers,
    Lobby,
    Space,
}

/// The lobby: the spaces and the create object, then the creation rings opened from it, one at a time. See `design/arrival.md`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LobbyStep {
    /// The spaces, followed by the create object.
    Spaces,
    /// Creating a space: choose a backend.
    Backend,
    /// Creating a space: choose a directory.
    Directory,
    /// Creating a space with a backend that can attach: a new session, or one of the backend's existing sessions in the directory.
    Session,
    /// Creating a space: choose the environment it is bound to. Only when the server offers this client more than one.
    Environment,
}

impl LobbyStep {
    fn ring(self) -> Option<arrival::Step> {
        match self {
            LobbyStep::Spaces => None,
            LobbyStep::Backend => Some(arrival::Step::Backend),
            LobbyStep::Directory => Some(arrival::Step::Directory),
            LobbyStep::Session => Some(arrival::Step::Session),
            LobbyStep::Environment => Some(arrival::Step::Environment),
        }
    }
}

pub struct Conversation {
    /// This participant's number on the connected server, from its Welcome, which the server records on their queued input. Kept while the connection is down, since a reconnect keeps it; see `design/security.md`.
    source: Option<u32>,
    /// What this connection may do where it is now, from its Welcome and the role messages that follow. See `design/security.md`.
    role: ConnectionRole,
    /// The fingerprint of the server's certificate on this connection, over TLS, which the pairing code is derived from.
    certificate: Option<tomori_client::Fingerprint>,
    pairing: Option<Pairing>,
    /// The knocks waiting on the space, while this participant is in it.
    knocks: Vec<tomori_protocol::KnockRequest>,
    /// What became of this visitor's knock on the space, if they knocked.
    knock: Option<tomori_protocol::KnockState>,
    /// The display name this device suggests to servers.
    name: String,
    /// The display name the connected server knows this participant by, from its Welcome and the names that follow, or `None` from a server that does not rename, which takes the suggested one. See `design/security.md`.
    server_name: Option<String>,
    /// Whether the next press of X says a new display name, as chosen in the hand menu.
    naming: bool,
    mode: Mode,
    servers: Vec<Choice>,
    server: usize,
    /// Whether the stick moved the server selection; until then the best choice, listed first, stays selected as servers appear.
    chosen: bool,
    /// The address of the chosen server while it is missing from the list, as while browsing restarts, so that it is selected again when it reappears.
    missing: Option<String>,
    /// Whether the remembered server may still be connected to when it appears: only once, before the participant moves the selection or connects.
    automatic: bool,
    /// When the remembered server, found, is due to be connected automatically; see [`AUTOMATIC_SETTLE`].
    automatic_at: Option<Instant>,
    /// The server chosen in the server list, while in the lobby or a space.
    connected: Option<Choice>,
    state: Option<ConnectionState>,
    spaces: Vec<SpaceSummary>,
    listed: bool,
    /// The selected space; one past the last is the create object.
    selected: usize,
    joining: Option<String>,
    /// A jump is under way: once the server lists its spaces, join the one that most needs attention. See `design/arrival.md`.
    jumping: bool,
    /// Space updates sent before the server handled our leave are ignored until it confirms.
    leaving: bool,
    space: Option<SpaceState>,
    /// The selected message; one past the last is the queue, when it has entries.
    message: usize,
    page: usize,
    follow: bool,
    /// The approval or question being shown, so paging and the chosen option reset when it changes.
    request: Option<String>,
    /// The chosen approval option: 0 allows once, `n` grants the `n`th standing option.
    option: usize,
    /// The approval a decision was sent for, so another press cannot send a second one.
    decided: Option<String>,
    notice: Option<String>,
    /// Whether the platform's request for the microphone is unanswered, which the server list says, since it shows no other notice. See `design/arrival.md`.
    microphone_waiting: bool,
    /// The space's own instructions to the agent that this owner gave for the next space they create, if any. See `design/spaces.md`.
    next_instructions: Option<String>,
    /// Shown in the lobby after a space you were in or joining has gone, until dismissed with B or another choice.
    ended: Option<String>,
    /// Which list the lobby shows: the spaces, or one of the creation rings.
    step: LobbyStep,
    /// The selection within the open creation ring.
    menu: usize,
    /// The server's backends, so the session ring is offered only for those that attach.
    backends: Vec<BackendInfo>,
    /// The directories a space may run in, offered so the client can choose without typing a path.
    roots: Vec<WorkspaceRoot>,
    /// The answer to [`ClientMessage::ListSessions`], shown on the session ring after "New session".
    sessions: Vec<SessionSummary>,
    /// The environments a space may be bound to that the server offers this client, offered on the environment ring when there are several.
    environments: Vec<EnvironmentInfo>,
    /// The environments this build draws; a space bound to another shows the grid, and says so.
    drawable: Vec<Environment>,
    /// The environments this build presents over the grid, without a scenery of their own, such as the GitHub prototype, whose cards the scene presents.
    over_grid: Vec<Environment>,
    /// The locomotion profiles this build declares, by environment; see `design/locomotion.md`.
    locomotion: Vec<(Environment, Locomotion)>,
    /// The client sides this build registers, by environment. See `design/environments.md`.
    client_sides: Vec<(Environment, crate::client_side::Factory)>,
    /// The backend chosen on the backend ring, kept until the space is created.
    menu_backend: Option<String>,
    /// The directory chosen on the directory ring, kept until the space is created.
    menu_directory: Option<String>,
    /// The space waiting at the environment step for its environment.
    menu_create: Option<Creation>,
    error: Option<String>,
    actions: Vec<Action>,
    /// The last body paginated, since the console's panel is drawn every frame.
    #[cfg(any(not(target_os = "android"), test))]
    paged: Option<Paged>,
    talk: Option<Talk>,
    /// What the participant points at now, a worker or a card, as the frame loop or console last reported.
    pointing_at: Option<tomori_protocol::Resource>,
    /// What each object of an environment's that the participant pointed at is called, by its identity, to name it as
    /// a worker or card is named. See `design/environments.md`.
    object_labels: std::collections::HashMap<String, String>,
    /// The cards of the space, in a space whose environment shows them. See `design/github.md`.
    cards: Option<tomori_protocol::Cards>,
    /// The state and events of the joined space's environment, as its server side sent them, until its client side takes them. See `design/environments.md`.
    channel: crate::channel::Replica,
    /// The space whose cards this connection asked to watch, so that it asks once for each space and connection.
    watching: Option<String>,
    live: bool,
    /// Whether the participant is wearing the device and attending. See `design/spaces.md`.
    engagement: Engagement,
    /// The engagement last reported on this connection, so that each change is reported once.
    reported: Option<Engagement>,
    /// Whether this client reports engagement at all; the spectator does not, so it follows live speech.
    reports_engagement: bool,
    /// Whether agent speech is muted locally.
    muted: bool,
    /// Whether a replay or catch-up was playing at the last frame.
    replaying: bool,
    /// This device's Feedback setting, which the hand menu and console change.
    feedback: feedback::Settings,
    /// Whether the last [`Self::accept`] refused, saying why, rather than acting or having nothing to do.
    refused: bool,
    /// The problem shown and when it first appeared, so that its age can be shown.
    problem_seen: Option<(String, Instant)>,
    /// The space's ongoing problem, as dismissed on this headset; it shows again if it changes.
    dismissed: Option<String>,
    /// The problem saying that this participant's words were not sent, or their name not changed, which what becomes of their next words replaces. See `design/interaction.md`.
    unsent: Option<String>,
    /// The problem last seen: shown on the right wrist's transcript, or on the recording card for the participant's own words, so that it no longer cues the right wrist. See `design/wrist-transcript.md`.
    problem_noticed: Option<String>,
    /// The open list of the space's modes and speech setting, and the selected item.
    space_menu: Option<usize>,
    /// The queued entry selected with the right stick, by id.
    queue_selected: Option<String>,
    /// A queued entry whose cancellation can still be undone, and when it is sent.
    cancelling: Option<(String, Instant)>,
    /// How long B has been held in a space, while it is, and whether that hold has already left.
    leave_hold: Option<(Duration, bool)>,
    /// When the approval or question shown appeared, until [`REQUEST_GRACE`] has passed.
    request_at: Option<Instant>,
    capture_generation: u64,
    /// The longest utterance the server's recognition takes, from its Welcome, when it has a limit.
    utterance_limit: Option<Duration>,
    /// What became of the last words sent, and when.
    outcome: Option<(Outcome, Instant)>,
    /// What the participant has done on the right wrist's transcript. See `design/wrist-transcript.md`.
    wrist: crate::transcript::State,
    /// The names seen in the space with their handles, for speakers' colours.
    known: crate::speakers::Known,
    /// Counts changes to the joined space, so that the transcript is laid out again only when something it shows changes.
    revision: u64,
    /// The transcript last laid out, and what it was laid out from.
    laid_out: Option<(TranscriptKey, Vec<crate::ui_text::Row>)>,
    /// Whether the status pill shows, a setting of this headset's. See `design/interaction.md`.
    pill: bool,
    /// Whether the participant travels where an environment allows it, a setting of this headset's. See `design/locomotion.md`.
    travel: bool,
    /// Approvals are decided and queued entries removed on the right wrist, so A elsewhere does neither, as on the headset and desktop window; the console decides with its own commands.
    decide_on_wrist: bool,
    /// Makes utterance identifiers unique across launches, since the server keys queued text by participant and utterance, and the participant persists.
    launch: String,
    utterances: u64,
}

/// A body's text, the rows it was paginated into, and its pages.
#[cfg(any(not(target_os = "android"), test))]
struct Paged {
    text: String,
    rows: usize,
    pages: Arc<[Vec<String>]>,
}

impl Conversation {
    /// `name` is this participant's display name.
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            source: None,
            role: ConnectionRole::Unknown,
            certificate: None,
            pairing: None,
            knocks: Vec::new(),
            knock: None,

            name: name.into(),
            server_name: None,
            naming: false,
            mode: Mode::Servers,
            servers: Vec::new(),
            server: 0,
            chosen: false,
            missing: None,
            automatic: true,
            automatic_at: None,
            connected: None,
            state: None,
            spaces: Vec::new(),
            listed: false,
            selected: 0,
            joining: None,
            jumping: false,
            leaving: false,
            space: None,
            message: 0,
            page: 0,
            follow: true,
            request: None,
            option: 0,
            decided: None,
            notice: None,
            microphone_waiting: false,
            next_instructions: None,
            ended: None,
            step: LobbyStep::Spaces,
            menu: 0,
            backends: Vec::new(),
            roots: Vec::new(),
            environments: Vec::new(),
            drawable: Vec::new(),
            over_grid: Vec::new(),
            locomotion: Vec::new(),
            client_sides: Vec::new(),
            sessions: Vec::new(),
            menu_backend: None,
            menu_directory: None,
            menu_create: None,
            error: None,
            actions: Vec::new(),
            #[cfg(any(not(target_os = "android"), test))]
            paged: None,
            talk: None,
            pointing_at: None,
            object_labels: std::collections::HashMap::new(),
            cards: None,
            channel: crate::channel::Replica::default(),
            watching: None,
            live: false,
            engagement: Engagement::Engaged,
            reported: None,
            reports_engagement: true,
            muted: false,
            replaying: false,
            feedback: feedback::Settings::default(),
            refused: false,
            problem_seen: None,
            dismissed: None,
            unsent: None,
            problem_noticed: None,
            space_menu: None,
            queue_selected: None,
            cancelling: None,
            leave_hold: None,
            request_at: None,
            capture_generation: 0,
            utterance_limit: None,
            outcome: None,
            wrist: Default::default(),
            known: Default::default(),
            revision: 0,
            laid_out: None,
            decide_on_wrist: false,
            pill: true,
            travel: true,
            launch: format!(
                "{:x}",
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_nanos()
            ),
            utterances: 0,
        }
    }

    /// The current panel: the server list, the lobby, or a space.
    #[cfg(any(not(target_os = "android"), test))]
    pub fn mode(&self) -> Mode {
        self.mode
    }

    /// Whether the server list is showing, which is the only time it is worth browsing.
    pub fn is_browsing(&self) -> bool {
        self.mode == Mode::Servers
    }

    /// The server chosen in the server list, while connected or connecting to it.
    pub fn server(&self) -> Option<&Choice> {
        self.connected.as_ref()
    }

    /// Connection generation at which new microphone actions are requested.
    pub fn set_capture_generation(&mut self, generation: u64) {
        self.capture_generation = generation;
    }

    #[cfg(any(test, feature = "testing"))]
    pub fn take_actions(&mut self) -> Vec<Action> {
        std::mem::take(&mut self.actions)
    }

    /// Take one action so a failure can cancel later queued microphone starts.
    pub fn next_action(&mut self) -> Option<Action> {
        if self.actions.is_empty() {
            None
        } else {
            Some(self.actions.remove(0))
        }
    }

    /// Replace the server list, keeping the selection on the same server.
    pub fn set_servers(&mut self, servers: Vec<Choice>) {
        let selected = self.missing.take().or_else(|| {
            self.servers
                .get(self.server)
                .filter(|_| self.chosen)
                .map(|choice| choice.address.clone())
        });
        match selected {
            Some(address) => match servers.iter().position(|choice| choice.address == address) {
                Some(index) => self.server = index,
                None => {
                    self.server = 0;
                    self.missing = Some(address);
                }
            },
            None => self.server = 0,
        }
        self.servers = servers;
    }

    /// Connect to the remembered server once it has been listed for [`AUTOMATIC_SETTLE`] at `now`, unless the participant chose first. If another server then needs attention and it doesn't, give up connecting automatically, so that the participant can see the other on the server row and nothing connects later while they reach for it. Call every frame.
    pub fn tick(&mut self, now: Instant) {
        let remembered = self.servers.iter().any(|choice| {
            choice.source == server_choice::Source::Remembered && choice.is_available()
        });
        if !self.automatic || self.chosen || self.mode != Mode::Servers || !remembered {
            self.automatic_at = None;
            return;
        }
        if now < *self.automatic_at.get_or_insert(now + AUTOMATIC_SETTLE) {
            return;
        }
        match server_choice::automatic(&self.servers).cloned() {
            Some(choice) => self.connect(choice),
            None => {
                log::info!("Not connecting automatically while another server needs attention.");
                self.automatic = false;
                self.automatic_at = None;
            }
        }
    }

    /// Connect to a server without choosing it from the list, as for an address given at startup.
    pub fn connect(&mut self, choice: Choice) {
        log::info!("Connecting to {} at {}", choice.label, choice.address);
        self.automatic = false;
        // The server being connected is the one to select on return, not one that went missing earlier.
        self.missing = None;
        self.actions.push(Action::Connect(choice.clone()));
        self.connected = Some(choice);
        self.state = None;
        self.source = None;
        self.role = ConnectionRole::Unknown;
        self.certificate = None;
        self.pairing = None;
        self.knocks.clear();
        self.knock = None;
        self.jumping = false;
        // Each server keeps its own name for this participant.
        self.server_name = None;
        self.naming = false;

        self.spaces.clear();
        self.listed = false;
        self.selected = 0;
        self.close_space();
        // A transcript still awaited cannot arrive on the new connection.
        self.talk = None;
        // The menu state belongs to the previous server; it will be re-offered by the new Welcome.
        self.close_menu();
        self.backends.clear();
        self.roots.clear();
        self.environments.clear();
        self.mode = Mode::Lobby;
        self.error = None;
        self.notice = None;
        self.ended = None;
    }

    /// Join `space` by id, as the lobby's accept does, for a client that takes the space as an argument rather than choosing it in the list.
    #[cfg(not(target_os = "android"))]
    pub fn join_space(&mut self, space: impl Into<String>) {
        let space = space.into();
        self.joining = Some(space.clone());
        self.leaving = false;
        self.error = None;
        self.ended = None;
        self.send(ClientMessage::Join { space });
    }

    /// Show, while `waiting`, that the platform's request for the microphone is unanswered: on the server list, and in the notice unless something else is already noted; and stop showing it once answered. See `design/arrival.md`.
    pub fn microphone_request(&mut self, waiting: bool) {
        self.microphone_waiting = waiting;
        if waiting {
            self.notice.get_or_insert_with(|| MICROPHONE_REQUEST.into());
        } else if self.notice.as_deref() == Some(MICROPHONE_REQUEST) {
            self.notice = None;
        }
    }

    /// Report a problem the connection found, such as a client that could not start.
    /// Tell the participant something about the connection's security, such as that the server's identity changed. See `design/security.md`.
    pub fn security_notice(&mut self, notice: impl Into<String>) {
        let notice = notice.into();
        log::warn!("{notice}");
        self.notice = Some(notice);
    }

    /// What this connection may do where it is now.
    pub fn role(&self) -> ConnectionRole {
        self.role
    }

    /// Whether this connection may create spaces, so that the create object is offered. Only owners create spaces; see `design/security.md`.
    fn creates(&self) -> bool {
        self.role == ConnectionRole::Owner
    }

    pub fn fail(&mut self, error: impl Into<String>) {
        self.error = Some(error.into());
    }

    pub fn apply(&mut self, event: Event) {
        match event {
            Event::State(state) => self.connection_changed(state),
            Event::Message(message) => self.receive(message),
            // Push-to-talk audio for the words being recorded or sent that could not be delivered. Anything else a later client observes is not this conversation's to apply.
            event => {
                if let Event::UtteranceFailed(failed) = event
                    && self.talk.as_ref().map(Talk::utterance) == Some(failed.utterance.as_str())
                {
                    let naming = self.talk.as_ref().is_some_and(Talk::naming);
                    self.stop_talking();
                    self.talk = None;
                    let error: String = match failed.failure {
                        UtteranceFailure::Disconnected => LOST.into(),
                        UtteranceFailure::Congested => {
                            "Network too slow; your words were not sent.".into()
                        }
                    };
                    self.failed(error, naming);
                }
            }
        }
    }

    fn connection_changed(&mut self, state: ConnectionState) {
        if !matches!(state, ConnectionState::Connected { .. }) {
            // A pairing belongs to its connection; the server withdraws it.
            self.pairing = None;
            self.stop_live();
            let naming = self.talk.as_ref().is_some_and(Talk::naming);
            match &self.talk {
                Some(Talk::Recording { .. }) => {
                    self.stop_talking();
                    self.failed(LOST.into(), naming);
                }
                // The server may still queue it, but its transcript cannot reach us. A name may still be taken, and says so if it is; until then, X says it again.
                Some(Talk::Sending { .. }) => {
                    self.talk = None;
                    self.naming = naming && self.renames();
                    self.notice = Some("Connection lost while sending your words.".into());
                }
                None => {}
            }
        }
        if let ConnectionState::Connected {
            server,
            source,
            role,
            certificate,
            backends,
            roots,
            environments,
            utterance_limit,
            name,
        } = &state
        {
            log::info!("Connected to {server} as {role:?}");
            self.server_name = name.clone();
            self.source = Some(*source);
            self.utterance_limit = *utterance_limit;
            self.role = *role;
            self.certificate = *certificate;
            // In a space a problem, such as words lost as the connection dropped, stays until it is dismissed.
            if self.mode != Mode::Space {
                self.error = None;
            }
            // A new connection starts with nothing reported, and watching nothing.
            self.reported = None;
            self.watching = None;
            self.backends = backends.clone();
            self.roots = roots.clone();
            self.environments = environments.clone();
            match self.mode {
                // A space joined meanwhile is restored by the client, and the server sends its state.
                Mode::Lobby => self.list(),
                // The client rejoins the space; the server may have lost it while away.
                Mode::Space => self.joining = self.space.as_ref().map(|space| space.id.clone()),
                Mode::Servers => {}
            }
        }
        self.state = Some(state);
    }

    fn receive(&mut self, message: ServerMessage) {
        match message {
            // A connection admitted only to pair is not shown spaces, even if a server sends them.
            ServerMessage::Spaces { .. } if self.role == ConnectionRole::Pairing => {}
            ServerMessage::PairNonce { nonce } => self.pair_with(&nonce),
            ServerMessage::PairWaiting => {}
            ServerMessage::Knocks { knocks } => self.knocks = knocks,
            ServerMessage::KnockState { state } => {
                self.notice = Some(
                    match state {
                        tomori_protocol::KnockState::Pending => {
                            "You knocked; waiting for someone to let you in."
                        }
                        tomori_protocol::KnockState::Declined => "Your knock was declined.",
                        tomori_protocol::KnockState::Expired => {
                            "Nobody answered your knock in time."
                        }
                        tomori_protocol::KnockState::Withdrawn
                        | tomori_protocol::KnockState::Unknown => "Your knock was withdrawn.",
                    }
                    .into(),
                );
                self.knock = Some(state);
            }
            // Only an answer to this device's own knock, so that a server cannot hand it a token unasked.
            ServerMessage::Admitted { token }
                if self.knock == Some(tomori_protocol::KnockState::Pending) =>
            {
                self.knock = None;
                // Only over TLS; the connection keeps the token with the certificate it presented.
                if self.certificate.is_some() {
                    self.actions.push(Action::Admitted { token });
                }
                self.notice = Some("You were let in: you can now take part in this space.".into());
            }
            ServerMessage::Admitted { .. } => {
                log::warn!("Ignored an admission this device did not ask for.")
            }
            ServerMessage::Paired { token } => {
                self.pairing = None;
                if self.certificate.is_some() {
                    self.actions.push(Action::Paired { token });
                    self.notice =
                        Some("Paired: this device is now an owner of this server.".into());
                }
            }
            ServerMessage::PairFailed { error } => {
                self.pairing = None;
                self.notice = Some(error);
            }
            ServerMessage::Role { role } => {
                log::info!("Now {role:?}");
                self.role = role;
                // A visitor sends no audio, so the microphone closes as the role changes.
                if !role.participates() {
                    self.stop_live();
                    self.stop_talking();
                    // The server ignores a visitor's engagement, so it is reported again on taking part.
                    self.reported = None;
                }
                // Admitted to act here, the participant's engagement now counts, and the space's cards can be watched.
                self.report();
                self.watch_cards();
            }
            ServerMessage::Spaces { spaces } => {
                let selected = self.spaces.get(self.selected).map(|space| &space.id);
                let urgent = arrival::most_urgent(&spaces).filter(|_| !self.listed);
                self.selected = if self.listed && self.selected == self.spaces.len() {
                    // The create object stays selected, after however many spaces there now are.
                    spaces.len()
                } else if let Some(index) = urgent {
                    // A fresh listing starts on the space that most needs attention, so A goes there.
                    index
                } else {
                    selected
                        .and_then(|id| spaces.iter().position(|space| &space.id == id))
                        .unwrap_or(0)
                };
                self.spaces = spaces;
                self.listed = true;
                if std::mem::take(&mut self.jumping) {
                    self.land();
                }
            }
            ServerMessage::Space { state } => self.update_space(*state),
            ServerMessage::Name { name } => {
                log::info!("Now called {name:?}");
                self.server_name = Some(name);
            }
            // For the joined space's environment only, as it is now: a late one from a space left, or from before the
            // space changed environment, is ignored.
            ServerMessage::EnvironmentState {
                environment,
                version,
                state,
            } => {
                self.receive_environment(environment, version, crate::channel::Update::State(state))
            }
            ServerMessage::EnvironmentEvent {
                environment,
                version,
                event,
            } => {
                self.receive_environment(environment, version, crate::channel::Update::Event(event))
            }
            // Only the joined space's cards are kept; a late update from a space left is ignored.
            ServerMessage::Cards { cards } => {
                if self
                    .space
                    .as_ref()
                    .is_some_and(|space| space.id == cards.space)
                {
                    self.cards = Some(*cards);
                }
            }
            // The answer to a session list, shown on the session ring after "New session".
            ServerMessage::Sessions { sessions } => {
                if self.step == LobbyStep::Session {
                    self.sessions = sessions;
                    self.menu = self.menu.min(self.sessions.len());
                } else {
                    self.error = Some("Too late to show those sessions.".into());
                }
            }
            ServerMessage::Left => {
                let requested = std::mem::take(&mut self.leaving);
                if self.mode != Mode::Servers {
                    self.close_space();
                    self.mode = Mode::Lobby;
                    // Leaving on request is followed by a listing; otherwise ask for one.
                    if !requested {
                        self.list();
                    }
                }
            }
            ServerMessage::Error { error, space } => {
                log::warn!("Server error: {error}");
                if let Some(gone) = space.filter(|space| Some(space) == self.joining.as_ref()) {
                    // Our join was refused because the space does not exist, as after the server restarts. The client
                    // sends the membership it last recorded again after every reconnect, which would be this join, so
                    // record a leave in its place.
                    let title = self
                        .space
                        .as_ref()
                        .filter(|space| space.id == gone)
                        .map(|space| space.title.clone())
                        .or_else(|| {
                            self.spaces
                                .iter()
                                .find(|summary| summary.id == gone)
                                .map(|summary| summary.title.clone())
                        })
                        .unwrap_or_else(|| "The space".into());
                    self.ended = Some(format!("{title} has ended."));
                    self.spaces.retain(|summary| summary.id != gone);
                    self.selected = self.selected.min(self.spaces.len().saturating_sub(1));
                    self.listed = false;
                    self.close_space();
                    self.mode = Mode::Lobby;
                    self.leaving = true;
                    self.send(ClientMessage::Leave);
                } else {
                    if self.mode == Mode::Lobby {
                        // A join refused without naming the space is not taken as the space being gone, but it is no longer awaited.
                        self.joining = None;
                    }
                    self.error = Some(error);
                }
                // A refused decision can be tried again.
                self.decided = None;
            }
            ServerMessage::Transcript {
                utterance,
                text,
                finished,
            } => {
                let ours = self.talk.as_ref().map(Talk::utterance) == Some(utterance.as_str());
                if ours && finished {
                    // The queue entry now shows the words, and replaces an earlier utterance's failure.
                    let talk = self.talk.take();
                    let outcome = if talk.as_ref().is_some_and(Talk::naming) {
                        // The words were the name the server now uses.
                        self.server_name = Some(text.clone());
                        Outcome::Named(text)
                    } else {
                        let answered = talk.is_some_and(|talk| talk.answering());
                        Outcome::Sent { answered, text }
                    };
                    self.outcome = Some((outcome, Instant::now()));
                    self.replace_unsent();
                } else if ours
                    && let Some(Talk::Recording { heard, .. } | Talk::Sending { heard, .. }) =
                        self.talk.as_mut()
                {
                    *heard = text;
                }
            }
            ServerMessage::TranscriptProgress {
                utterance,
                stage,
                part,
            } => {
                // Only words on their way are being transcribed; an earlier utterance's progress is no longer shown.
                if let Some(Talk::Sending {
                    utterance: ours,
                    received,
                    ..
                }) = self.talk.as_mut()
                    && *ours == utterance
                {
                    *received = Some(crate::recording::Received { stage, part });
                }
            }
            ServerMessage::TranscriptFailed { utterance, error } => {
                log::warn!("Utterance {utterance} failed: {error}");
                let mut naming = false;
                if self.talk.as_ref().map(Talk::utterance) == Some(utterance.as_str()) {
                    let talk = self.talk.take();
                    naming = talk.as_ref().is_some_and(Talk::naming);
                    // Cancelling would be refused, since the server has already ended it.
                    if matches!(talk, Some(Talk::Recording { .. })) {
                        self.actions.push(Action::AbandonUtterance);
                    }
                }
                if error == tomori_protocol::NO_SPEECH && !naming {
                    // Nothing said was lost: the card says so for a moment, and the problem line, the pill and the right wrist are not troubled.
                    self.outcome = Some((Outcome::Unheard, Instant::now()));
                } else {
                    // Also shown for an earlier utterance, whose words were not sent either.
                    self.failed(error, naming);
                }
            }
            // Agent speech is heard rather than shown: its audio plays through the speaker.
            ServerMessage::Speech { segment, text, .. } => {
                log::info!("Agent speech {segment}: {text}");
            }
            // Presence comes with the presence work.
            ServerMessage::Welcome { .. }
            | ServerMessage::Presence { .. }
            | ServerMessage::Ping { .. }
            | ServerMessage::Pong { .. } => {}
        }
    }

    /// Show a space's state. The server sends it after a join, on every change, and unasked when it rejoins this participant after a reconnect.
    fn update_space(&mut self, state: SpaceState) {
        if self.mode == Mode::Servers
            || (self.leaving && self.joining.as_deref() != Some(state.id.as_str()))
        {
            return;
        }
        let entered = self.mode != Mode::Space
            || self.space.as_ref().is_none_or(|space| space.id != state.id);
        // The server put this participant in a space they did not ask for, as their last space when they connect: join it
        // as their own choice, which clears what it needed while nobody was there, now that they see it. A jump moves on
        // from it instead.
        let unasked = entered && self.joining.is_none() && !self.jumping;
        if entered {
            log::info!("Joined space {} ({})", state.title, state.id);
            self.close_space();
            self.mode = Mode::Space;
        }
        // A late update from a space being left does not end a join of another.
        if entered || self.joining.as_deref() == Some(state.id.as_str()) {
            self.joining = None;
        }
        self.leaving = false;
        self.known.learn(&state);
        self.revision += 1;
        let request = state
            .approval
            .as_ref()
            .map(|approval| approval.id.clone())
            .or_else(|| state.question.as_ref().map(|question| question.id.clone()));
        let arrived = request.is_some() && request != self.request;
        if request != self.request {
            self.page = 0;
            self.option = 0;
            self.decided = None;
            self.request = request;
        }
        if let Some(approval) = &state.approval {
            // Allow once, each standing option, then deny.
            self.option = self.option.min(approval.options.len() + 1);
        }
        if unasked {
            self.join(state.id.clone());
        }
        let previous = self
            .space
            .as_ref()
            .and_then(|space| space.messages.get(self.message))
            .map(|message| message.id.clone());
        if self.follow {
            self.message = state.messages.len().saturating_sub(1);
        } else if let Some(index) =
            previous.and_then(|id| state.messages.iter().position(|message| message.id == id))
        {
            self.message = index;
        } else {
            self.message = self.message.min(items(&state).saturating_sub(1));
        }
        // What an environment's side sent belongs to the binding it ran for: once the space moves to another environment or version, it is forgotten, so that coming back waits for the new binding's state rather than presenting the old one.
        if self.space.as_ref().is_some_and(|space| {
            (space.environment.as_str(), space.environment_version)
                != (state.environment.as_str(), state.environment_version)
        }) {
            self.channel.clear();
        }
        self.space = Some(state);
        self.watch_cards();
        if arrived {
            self.request_at = Some(Instant::now());
            // The transcript's focus moves to the request's title, so that an A meant for something else there decides nothing.
            self.wrist.focus_card(
                self.space
                    .as_ref()
                    .and_then(|space| space.messages.last())
                    .map(|message| message.id.as_str()),
            );
            // A request takes A: the list closes, a selected entry is let go, and a cancellation still in its undo time is sent now, so that an A meant for them cannot decide it.
            self.space_menu = None;
            self.queue_selected = None;
            if let Some((entry, _)) = self.cancelling.take() {
                self.cancel_now(entry);
            }
        }
        if let Some(selected) = self.space_menu {
            let count = self.menu_items().len();
            self.space_menu = (count > 0).then(|| selected.min(count - 1));
        }
        self.report();
    }

    /// Send the cancellation of `entry` if it is still waiting, or say that it was not cancelled.
    fn cancel_now(&mut self, entry: String) {
        if self.waiting().iter().any(|waiting| waiting.id == entry) {
            self.send(ClientMessage::CancelQueued { entry });
        } else {
            self.notice = Some("Not cancelled: already sent or gone.".into());
        }
    }

    /// Report the engagement to a server that accepts it, once per change on each connection. Sent without clearing a shown error, as it is not something the participant did.
    fn report(&mut self) {
        // A visitor sends no presence, so no engagement either; see `design/security.md`.
        let supported = self.reports_engagement
            && self.role.participates()
            && self.mode == Mode::Space
            && matches!(self.state, Some(ConnectionState::Connected { .. }))
            && self
                .space
                .as_ref()
                .is_some_and(|space| space.speech != SpeechSetting::Unknown);
        if supported && self.reported != Some(self.engagement) {
            self.reported = Some(self.engagement);
            self.actions.push(Action::Send(ClientMessage::Engagement {
                state: self.engagement,
            }));
        }
    }

    /// Never report engagement, so that the server keeps this client at the live edge of speech rather than playing it what it missed, as for the spectator. See `design/spectator.md`.
    #[cfg(not(target_os = "android"))]
    pub fn follow_live_speech_only(&mut self) {
        self.reports_engagement = false;
    }

    /// The participant put the device on or took it off, or something else took or returned its input. Taking it off stops the live microphone and any recording, as losing focus does; see `design/live-audio.md`.
    pub fn set_engagement(&mut self, engagement: Engagement) {
        self.engagement = engagement;
        if !engagement.is_listening() {
            self.stop_live();
            self.stop_talking();
        }
        self.report();
    }

    /// Left stick click: mute or unmute spoken replies, for this participant only. Other participants still play.
    pub fn toggle_mute(&mut self) {
        self.muted = !self.muted;
        log::info!(
            "Spoken replies {}",
            if self.muted { "muted" } else { "unmuted" }
        );
        self.actions.push(Action::SetAgentMuted(self.muted));
    }

    /// Right stick click: play the latest reply again from its start, for this participant only, or stop what is playing again and follow live speech. Asking to hear it again unmutes spoken replies.
    pub fn replay(&mut self) {
        let Some(space) = self.space.as_ref().filter(|_| self.mode == Mode::Space) else {
            self.notice = Some("Join a space to hear a reply again.".into());
            return;
        };
        if space.speech == SpeechSetting::Unknown {
            self.notice = Some("This server cannot play replies again.".into());
            return;
        }
        if self.replaying {
            self.notice = None;
            self.send(ClientMessage::StopReplay);
            return;
        }
        if self.muted {
            self.toggle_mute();
        }
        self.notice = None;
        self.send(ClientMessage::Replay { message: None });
    }

    /// This device's Feedback setting, as kept on it.
    pub fn set_feedback(&mut self, settings: feedback::Settings) {
        self.feedback = settings;
    }

    /// Change one half of the Feedback setting, sounds or vibration, and keep it.
    pub fn change_feedback(
        &mut self,
        sounds: Option<feedback::Level>,
        vibration: Option<feedback::Level>,
    ) {
        let settings = feedback::Settings {
            sounds: sounds.unwrap_or(self.feedback.sounds),
            vibration: vibration.unwrap_or(self.feedback.vibration),
            ..self.feedback
        };
        self.keep_feedback(settings);
    }

    /// Play every source in both ears alike, or place each where it is, and keep it. See `design/spatial-audio.md`.
    pub fn set_mono(&mut self, mono: bool) {
        self.keep_feedback(feedback::Settings {
            mono,
            ..self.feedback
        });
    }

    fn keep_feedback(&mut self, settings: feedback::Settings) {
        log::info!(
            "Feedback: sounds {}, vibration {}, {}",
            settings.sounds.label(),
            settings.vibration.label(),
            if settings.mono { "mono" } else { "stereo" }
        );
        self.feedback = settings;
        self.actions.push(Action::SetFeedback(settings));
    }

    /// Whether a replay or catch-up is playing, from the player, once per frame.
    pub fn set_replaying(&mut self, replaying: bool) {
        self.replaying = replaying;
    }

    /// Finish a jump with the first listing: join the space that most needs attention, even one the server already put this participant in, since joining is what says it has been seen. If someone dealt with it meanwhile, stay in the lobby and say so.
    fn land(&mut self) {
        match arrival::most_urgent(&self.spaces) {
            Some(index) => {
                let space = self.spaces[index].id.clone();
                log::info!("Jumping to space {space}");
                self.selected = index;
                self.join(space);
            }
            None if self.mode == Mode::Lobby => {
                let server = self
                    .connected
                    .as_ref()
                    .map_or("the server", |choice| choice.label.as_str());
                self.notice = Some(format!("Nothing on {server} needs you now."));
            }
            None => {}
        }
    }

    /// Ask to join `space`, from the lobby or from another space.
    fn join(&mut self, space: String) {
        self.joining = Some(space.clone());
        self.leaving = false;
        self.error = None;
        self.ended = None;
        self.send(ClientMessage::Join { space });
    }

    /// What the most urgent space needs, said in a few words for the lobby's status, such as "Deploy needs you".
    fn attention_status(&self) -> Option<String> {
        let space = &self.spaces[arrival::most_urgent(&self.spaces)?];
        let title = &space.title;
        // The most urgent space is one that needs attention.
        space.attention.map(|attention| match attention {
            tomori_protocol::Attention::Input => format!("{title} needs you"),
            tomori_protocol::Attention::Problem => format!("{title} has a problem"),
            tomori_protocol::Attention::Reply => format!("{title} has a new reply"),
            tomori_protocol::Attention::Unknown => format!("{title} needs attention"),
        })
    }

    fn close_space(&mut self) {
        self.stop_live();
        self.stop_talking();
        // A new name is said in a space; leaving it stops waiting for one.
        self.naming = false;
        // Knocks belong to the space.
        self.knocks.clear();
        self.knock = None;
        self.space = None;
        self.cards = None;
        self.channel.clear();
        self.watching = None;
        self.joining = None;
        self.message = 0;
        self.page = 0;
        self.follow = true;
        self.request = None;
        self.option = 0;
        self.decided = None;
        self.notice = None;
        self.dismissed = None;
        self.space_menu = None;
        self.queue_selected = None;
        self.cancelling = None;
        self.wrist = Default::default();
        self.known.clear();
        self.revision += 1;
    }

    fn list(&mut self) {
        // A connection admitted only to pair sees no spaces.
        if self.role == ConnectionRole::Pairing {
            return;
        }
        self.actions.push(Action::Send(ClientMessage::ListSpaces));
    }

    /// The pairing object: its code while pairing, which is compared with the server's. See `design/arrival.md`.
    fn pairing_item(&self) -> arrival::Item {
        let (label, detail, pending) = match &self.pairing {
            Some(Pairing::Waiting(code, _)) => (code.clone(), "Confirm on the server", true),
            Some(Pairing::Committed(..)) => ("Pairing...".to_owned(), "Choose to cancel", true),
            None => ("Pair".to_owned(), "Become an owner", false),
        };
        arrival::Item {
            key: arrival::Key::Pair,
            kind: arrival::Kind::Pair { pending },
            label,
            detail: Some(detail.into()),
            selected: false,
        }
    }

    /// What the hand menu shows now. See `design/hand-menu.md`.
    pub fn menu_model(&self) -> crate::hand_menu::Model {
        let space = self.space.as_ref().filter(|_| self.mode == Mode::Space);
        crate::hand_menu::Model {
            name: self.display_name().to_owned(),
            renames: self.renames(),
            naming: self.naming,
            role: self.role,
            in_space: space.is_some(),
            participants: space
                .map(|space| {
                    space
                        .participants
                        .iter()
                        .map(|participant| participant.name.clone())
                        .collect()
                })
                .unwrap_or_default(),
            visitors: space.map_or(0, |space| space.visitors),
            knocks: self.knocks().to_vec(),
            can_knock: self.can_knock(),
            knock: self.knock_state(),
            pairing: self.pairable().then(|| self.pairing_item().label),
            notice: self.notice.clone(),
            instructions: space.and_then(|space| space.instructions.clone()),
            space: space.map(|space| crate::hand_menu::SpaceInfo {
                title: space.title.clone(),
                running: format!(
                    "{} in {}",
                    space.backend,
                    space
                        .directory
                        .trim_end_matches('/')
                        .rsplit('/')
                        .next()
                        .unwrap_or_default()
                ),
                activity: activity_label(space.activity).into(),
                modes: space
                    .modes
                    .iter()
                    .map(|mode| {
                        (
                            mode.id.clone(),
                            mode.name.clone(),
                            mode.description.clone(),
                            Some(&mode.id) == space.mode.as_ref(),
                        )
                    })
                    .collect(),
                mode: space
                    .current_mode()
                    .map(|mode| mode.name.clone())
                    .or_else(|| space.mode.clone().filter(|_| !space.modes.is_empty())),
                speech: space.speech,
                environment: (!space.environment.is_empty()).then(|| self.shown_environment(space)),
                environments: self.changeable_environments(space),
                here: here(space),
            }),
            people: space
                .map(|space| {
                    space
                        .participants
                        .iter()
                        .map(|participant| {
                            let colour = crate::speakers::colour(participant.source);
                            crate::hand_menu::Person {
                                name: participant.name.clone(),
                                colour: [colour[0], colour[1], colour[2]],
                                you: Some(participant.source) == self.source,
                                away: participant.engagement == Some(Engagement::Away),
                            }
                        })
                        .collect()
                })
                .unwrap_or_default(),
            muted: self.muted,
            pill: self.pill,
            travel: self.declared_locomotion().travels().then_some(self.travel),
            pointed: None,
            feedback: self.feedback,
        }
    }

    /// The display name this participant goes by: the one the server knows them by, or else the one this device suggests.
    pub fn display_name(&self) -> &str {
        self.server_name.as_deref().unwrap_or(&self.name)
    }

    /// Whether this participant can say a new name here: the server renames, and they take part in the space they are in, since the words are recorded as push-to-talk. See `design/security.md`.
    fn renames(&self) -> bool {
        self.server_name.is_some()
            && self.mode == Mode::Space
            && self.space.is_some()
            && self.role.participates()
    }

    /// Change this participant's display name to `name`, as typed on the console, on a server that renames. The server keeps it with their token, or for a visitor while it keeps them. Says what happened in the notice.
    pub fn rename(&mut self, name: &str) {
        if self.server_name.is_none()
            || !matches!(self.state, Some(ConnectionState::Connected { .. }))
        {
            self.notice = Some("This server does not change names.".into());
            return;
        }
        // Cleaned and checked as the server does, so that what is refused says why at once; the server checks again, and whether someone else has the name.
        match tomori_protocol::text::display_name(name) {
            Ok(name) => {
                self.notice = Some(format!("Changing your name to {name}."));
                self.send(ClientMessage::SetName { name });
            }
            Err(error) => self.notice = Some(format!("Name not changed: {error}")),
        }
    }

    /// The name the server gives the environment `id`, or the identifier itself when it offers none by that name.
    fn environment_name(&self, id: &str) -> String {
        self.environments
            .iter()
            .find(|offered| offered.id == id)
            .map_or_else(|| id.to_owned(), |offered| offered.label.clone())
    }

    /// The environments this participant can change `space` to, as the hand menu offers them: id, name and whether it is the current one, starting with the current one when the server no longer offers it. Only owners can, and only on a server that changes environments. See `design/environments.md`.
    fn changeable_environments(&self, space: &SpaceState) -> Vec<(String, String, bool)> {
        if self.role != ConnectionRole::Owner || !space.environment_changes {
            return Vec::new();
        }
        let mut offered: Vec<_> = self
            .environments
            .iter()
            .map(|offered| {
                let current = offered.id == space.environment;
                let name = if current {
                    self.shown_environment(space)
                } else {
                    offered.label.clone()
                };
                (offered.id.clone(), name, current)
            })
            .collect();
        if !offered.iter().any(|(_, _, current)| *current) {
            offered.insert(
                0,
                (
                    space.environment.clone(),
                    self.shown_environment(space),
                    true,
                ),
            );
        }
        offered
    }

    /// Change the joined space's environment to `id`, for everyone in it. Only owners can, and only on a server that changes environments; the server refuses one it does not offer. Says what happened in the notice. See `design/environments.md`.
    pub fn change_environment(&mut self, id: &str) {
        if self.role != ConnectionRole::Owner {
            self.notice = Some("Only owners can change the space's environment.".into());
            return;
        }
        let in_space = self.mode == Mode::Space;
        let Some(space) = self.space.as_ref().filter(|_| in_space) else {
            self.notice = Some("Join a space first.".into());
            return;
        };
        if !space.environment_changes {
            self.notice = Some("This server does not change a space's environment.".into());
            return;
        }
        let name = self.environment_name(id);
        // Choosing the environment it is in, as the menu lists it even when this client cannot present it, changes nothing, and the server would refuse it as one not offered to this client.
        if space.environment == id {
            self.notice = Some(format!("The space is already in {name}."));
            return;
        }
        self.send(ClientMessage::SetEnvironment {
            environment: id.to_owned(),
        });
        self.notice = Some(format!("Changing the environment to {name} for everyone."));
    }

    /// Act on a choice made in the hand menu.
    pub fn choose_menu(&mut self, choice: crate::hand_menu::Choice) {
        match choice {
            crate::hand_menu::Choice::Knock => self.knock(),
            crate::hand_menu::Choice::Admit(id) => self.decide_knock(&id, true),
            crate::hand_menu::Choice::Decline(id) => self.decide_knock(&id, false),
            crate::hand_menu::Choice::Pair => self.pair(),
            crate::hand_menu::Choice::ClearInstructions => self.instruct(""),
            crate::hand_menu::Choice::Mode(mode) => {
                self.send(ClientMessage::SetMode { mode });
            }
            crate::hand_menu::Choice::Speech => {
                if let Some(space) = self.space.as_ref() {
                    let speech = match space.speech {
                        SpeechSetting::Brief => SpeechSetting::Full,
                        _ => SpeechSetting::Brief,
                    };
                    self.send(ClientMessage::SetSpeech { speech });
                }
            }
            crate::hand_menu::Choice::Environment(id) => self.change_environment(&id),
            crate::hand_menu::Choice::Mute => self.toggle_mute(),
            crate::hand_menu::Choice::Pill => self.toggle_pill(),
            crate::hand_menu::Choice::Travel => self.toggle_travel(),
            crate::hand_menu::Choice::Leave if self.mode == Mode::Space => self.leave(),
            crate::hand_menu::Choice::Leave => {}
            crate::hand_menu::Choice::Sounds => {
                self.change_feedback(Some(self.feedback.sounds.next()), None)
            }
            crate::hand_menu::Choice::Vibration => {
                self.change_feedback(None, Some(self.feedback.vibration.next()))
            }
            crate::hand_menu::Choice::Mono => self.set_mono(!self.feedback.mono),
            // The next press of X says the new name; choosing again changes nothing after all.
            crate::hand_menu::Choice::Rename => self.naming = !self.naming && self.renames(),
        }
    }

    /// Give the joined space's agent `text` as the space's own instructions, or, outside a space, the agent of the next space this owner creates; empty text removes them. Only owners can, and only where the server shows a space's instructions. Says what happened in the notice. See `design/spaces.md`.
    pub fn instruct(&mut self, text: &str) {
        let text = text.trim();
        if self.role != ConnectionRole::Owner {
            self.notice = Some("Only owners can give a space instructions.".into());
            return;
        }
        // The server's limit, checked here so that a space is not refused for them.
        if text.len() > INSTRUCTIONS_BYTES {
            self.notice = Some(format!(
                "Instructions can be at most {INSTRUCTIONS_BYTES} bytes."
            ));
            return;
        }
        let in_space = self.mode == Mode::Space;
        match self.space.as_ref().filter(|_| in_space) {
            Some(space) if space.instructions.is_none() => {
                self.notice = Some("This agent takes no instructions from Tomori.".into());
            }
            Some(_) => {
                // An owner takes part, so this is always sent.
                self.send(ClientMessage::SetInstructions {
                    text: text.to_owned(),
                });
                self.notice = Some(if text.is_empty() {
                    "Removing this space's instructions.".into()
                } else {
                    "Giving the agent this space's instructions.".into()
                });
            }
            None => {
                self.next_instructions = (!text.is_empty()).then(|| text.to_owned());
                self.notice = Some(if text.is_empty() {
                    "The next space you create has no instructions of its own.".into()
                } else {
                    "The next space you create is given these instructions, if its agent takes them."
                        .into()
                });
            }
        }
    }

    /// The knocks waiting on the space, for a participant to admit or decline.
    pub fn knocks(&self) -> &[tomori_protocol::KnockRequest] {
        if self.role.participates() {
            &self.knocks
        } else {
            &[]
        }
    }

    /// What became of this visitor's knock, if they knocked.
    pub fn knock_state(&self) -> Option<tomori_protocol::KnockState> {
        self.knock
    }

    /// Whether this visitor can knock on the space they are watching: watching one, and not already waiting.
    pub fn can_knock(&self) -> bool {
        self.mode == Mode::Space
            && self.space.is_some()
            && self.role == ConnectionRole::Visitor
            && self.knock != Some(tomori_protocol::KnockState::Pending)
    }

    /// Knock on the space this visitor is watching, to ask to take part. See `design/security.md`.
    pub fn knock(&mut self) {
        if self.can_knock() {
            self.actions.push(Action::Send(ClientMessage::Knock));
        }
    }

    /// Admit or decline the knock `id`, as a participant.
    pub fn decide_knock(&mut self, id: &str, admit: bool) {
        self.send(ClientMessage::DecideKnock {
            knock: id.to_owned(),
            admit,
        });
    }

    /// Whether this device can pair with the server: connected over TLS, and not already an owner. See `design/security.md`.
    pub fn pairable(&self) -> bool {
        self.certificate.is_some()
            && self.role != ConnectionRole::Owner
            && matches!(self.state, Some(ConnectionState::Connected { .. }))
    }

    /// Start pairing, committing to a random value, or cancel the pairing in progress.
    pub fn pair(&mut self) {
        if self.pairing.take().is_some() {
            self.actions.push(Action::Send(ClientMessage::PairCancel));
            self.notice = None;
            return;
        }
        if !self.pairable() {
            return;
        }
        let device = tomori_security::pairing::Nonce::generate();
        self.actions.push(Action::Send(ClientMessage::PairBegin {
            commitment: device.commitment(),
        }));
        self.pairing = Some(Pairing::Committed(device, Instant::now()));
        // In a space a problem stays until it is dismissed or replaced by a newer one, so that it is not wiped before it is read; elsewhere the next attempt replaces it.
        if self.mode != Mode::Space {
            self.error = None;
        }
        self.notice = Some("Pairing...".into());
    }

    /// The server sent its value: reveal ours, and derive the code from both and the certificate this connection presented.
    fn pair_with(&mut self, nonce: &str) {
        let (Some(Pairing::Committed(device, began)), Some(certificate), Some(server)) = (
            self.pairing.clone(),
            self.certificate,
            tomori_security::pairing::Nonce::parse(nonce),
        ) else {
            return;
        };
        self.actions.push(Action::Send(ClientMessage::PairReveal {
            nonce: device.to_hex(),
        }));
        let code = tomori_security::pairing::display(&tomori_security::pairing::code(
            certificate,
            device,
            server,
        ));
        self.notice = Some(format!("Pairing code {code}: confirm it on the server."));
        self.pairing = Some(Pairing::Waiting(code, began));
    }

    /// Give up on a pairing that has waited longer than the device waits for one, the server's timeout and a margin, at `now`, cancelling it on the server. Called every frame.
    pub fn expire_pairing(&mut self, now: Instant) {
        let began = match &self.pairing {
            Some(Pairing::Committed(_, began) | Pairing::Waiting(_, began)) => *began,
            None => return,
        };
        if now.saturating_duration_since(began) >= PAIRING_PATIENCE {
            self.pairing = None;
            self.actions.push(Action::Send(ClientMessage::PairCancel));
            self.notice = Some("The pairing was not confirmed in time.".into());
        }
    }

    /// Send `message`, unless it acts in the space and this connection only watches. Returns whether it was sent.
    fn send(&mut self, message: ClientMessage) -> bool {
        // A visitor only watches: acting in the space, such as deciding an approval, is not sent. See `design/security.md`.
        let acts = matches!(
            message,
            ClientMessage::CancelQueued { .. }
                | ClientMessage::Decide { .. }
                | ClientMessage::Answer { .. }
                | ClientMessage::SetAttribution { .. }
                | ClientMessage::SetMode { .. }
                | ClientMessage::DecideKnock { .. }
                | ClientMessage::SetSpeech { .. }
                | ClientMessage::SetInstructions { .. }
                | ClientMessage::SetEnvironment { .. }
                | ClientMessage::Replay { .. }
                | ClientMessage::StopReplay
        );
        if acts && !self.role.participates() {
            self.notice = Some(VISITING.into());
            return false;
        }
        // In a space a problem stays until it is dismissed or replaced by a newer one, so that it is not wiped before it is read; elsewhere the next attempt replaces it.
        if self.mode != Mode::Space {
            self.error = None;
        }
        self.actions.push(Action::Send(message));
        true
    }

    /// The number of entries in the list shown at `step`, so the stick stays on one of them.
    fn step_list(&self, step: LobbyStep) -> usize {
        match step {
            // The spaces, then the create object.
            LobbyStep::Spaces => self.spaces.len() + usize::from(self.creates()),
            LobbyStep::Backend => self.backends.len(),
            LobbyStep::Directory => self.roots.len(),
            // "New session", then the existing sessions.
            LobbyStep::Session => self.sessions.len() + 1,
            LobbyStep::Environment => self.environments.len(),
        }
    }

    /// Whether the chosen backend can attach to an existing session, so that the session ring is offered.
    fn attaches(&self) -> bool {
        self.backends
            .iter()
            .any(|backend| Some(&backend.name) == self.menu_backend.as_ref() && backend.attach)
    }

    /// Close the creation rings, back to the spaces with the create object selected, forgetting their choices.
    fn close_menu(&mut self) {
        if self.step != LobbyStep::Spaces {
            self.selected = self.spaces.len();
        }
        self.step = LobbyStep::Spaces;
        self.menu = 0;
        self.sessions.clear();
        self.menu_backend = None;
        self.menu_directory = None;
        self.menu_create = None;
    }

    /// Send `create`, bound to the only environment the server offers, or ask which one when it offers several. A server that offers none predates environments, and binds its own.
    fn submit(&mut self, create: Creation) {
        self.joining = None;
        if self.environments.len() > 1 {
            self.menu = 0;
            self.step = LobbyStep::Environment;
            self.menu_create = Some(create);
            return;
        }
        let only = self.environments.first().map(|offered| offered.id.clone());
        self.send(create.message(only));
        self.close_menu();
    }

    /// The environments this build has sceneries for, as [`Self::set_drawable`] gave them.
    #[cfg(any(test, feature = "testing"))]
    pub fn drawable(&self) -> &[Environment] {
        &self.drawable
    }

    /// Draw `drawable`, the environments this build has sceneries for, beside the built-in grid, which is all a conversation draws until it is given them.
    pub fn set_drawable(&mut self, drawable: Vec<Environment>) {
        self.drawable = drawable;
    }

    /// Present `over_grid` over the grid, without sceneries of their own, as the GitHub prototype's cards are: listed in `Hello` and shown without a notice. See `tomori-render`'s `Sceneries::present_over_grid`.
    pub fn set_over_grid(&mut self, over_grid: Vec<Environment>) {
        self.over_grid = over_grid;
    }

    /// Present each environment `client_sides` names with its client side, in a space bound to it whose scenery this
    /// build draws. See `design/environments.md`.
    pub fn set_client_sides(
        &mut self,
        client_sides: Vec<(Environment, crate::client_side::Factory)>,
    ) {
        self.client_sides = client_sides;
    }

    /// The client side of the joined space's environment, as this build draws it, if the build registers one: the
    /// environment, at the version drawn, and what makes its client side.
    pub fn client_side(&self) -> Option<(Environment, crate::client_side::Factory)> {
        let environment = self.environment();
        self.joined_space()?;
        self.client_sides
            .iter()
            .find(|(registered, _)| registered.id() == environment.id())
            .map(|(_, factory)| (environment, factory.clone()))
    }

    /// Let the participant travel as `locomotion` declares, for each environment it names, in a space bound to it whose scenery this build draws. An environment it does not name has none, as Tomori's own do.
    pub fn set_locomotion(&mut self, locomotion: Vec<(Environment, Locomotion)>) {
        self.locomotion = locomotion;
    }

    /// How the participant may travel: as declared for the joined space's environment, when this build draws it and travel is not turned off on this headset, and otherwise not at all. See `design/locomotion.md`.
    pub fn locomotion(&self) -> &Locomotion {
        const NONE: &Locomotion = &Locomotion::None;
        if self.travel {
            self.declared_locomotion()
        } else {
            NONE
        }
    }

    /// Turn travel off on this headset, staying room scale wherever an environment allows it, or on again. See `design/locomotion.md`.
    pub fn toggle_travel(&mut self) {
        self.travel = !self.travel;
    }

    /// How the joined space's environment lets the participant travel, as declared, whether or not it is turned off here.
    fn declared_locomotion(&self) -> &Locomotion {
        const NONE: &Locomotion = &Locomotion::None;
        // The environment the space is shown in: one this build cannot draw at the space's version is the grid, which declares none.
        self.joined_space()
            .map(|(space, _)| self.environment_of(space))
            .and_then(|environment| {
                self.locomotion
                    .iter()
                    .find(|(declared, _)| *declared == environment)
            })
            .map_or(NONE, |(_, locomotion)| locomotion)
    }

    /// The environments this build presents, each at its version, as its Hello lists them, each once: the grid first, always at its built-in version, then those it draws, at the versions their sceneries are registered at, and those it presents over the grid, at the versions they are presented at unless a scenery is registered for them. Each environment's version is in its own crate, which its server half registers it with too. See `design/environments.md`.
    pub fn presented(&self) -> Vec<tomori_protocol::EnvironmentVersion> {
        let mut presented: Vec<Environment> = Vec::new();
        for environment in std::iter::once(Environment::MINIMAL)
            .chain(self.drawable.iter().copied())
            .chain(self.over_grid.iter().copied())
        {
            if !presented.contains(&environment) {
                presented.push(environment);
            }
        }
        presented
            .into_iter()
            .map(|environment| {
                tomori_protocol::EnvironmentVersion::new(environment.id(), environment.version())
            })
            .collect()
    }

    /// What this build presents of the environment `id`: the environment it draws, or one it presents over the grid, or `None` when it cannot present it at all.
    fn presents(&self, id: &str) -> Option<Environment> {
        Environment::for_space(id, &self.drawable).or_else(|| {
            self.over_grid
                .iter()
                .copied()
                .find(|environment| environment.id() == id)
        })
    }

    /// The scenery to draw: the Minimal grid outside a space, and inside one the environment it is bound to, or the grid when this build cannot draw that environment, or draws it at another version than the space's. See `design/environments.md`.
    pub fn environment(&self) -> Environment {
        match &self.space {
            Some(space) if self.mode == Mode::Space => self.environment_of(space),
            _ => Environment::MINIMAL,
        }
    }

    /// The scenery to draw for `space`: its environment, when this build draws it at the version the space's state names, or names none, as from an older server; otherwise the grid. See `design/environments.md`.
    pub fn environment_of(&self, space: &SpaceState) -> Environment {
        Environment::for_space(&space.environment, &self.drawable)
            .filter(|environment| {
                space
                    .environment_version
                    .is_none_or(|version| version == environment.version())
            })
            .unwrap_or(Environment::MINIMAL)
    }

    /// What `space`'s environment needs that this build lacks, when it shows the grid instead: the environment, or the version it needs, as the space's state names it. `None` when it presents the environment at that version, or names none to compare. See `design/environments.md`.
    fn environment_notice(&self, space: &SpaceState) -> Option<String> {
        let id = space.environment.as_str();
        if id.is_empty() {
            return Some("The server named no environment.".into());
        }
        match (self.presents(id), space.environment_version) {
            // Says which side has which version. A server that agrees on versions does not offer this client the environment, so it is named by its label only when known, and otherwise by its identifier.
            (Some(presented), Some(version)) if presented.version() != version => Some(format!(
                "{} needs version {version}; this app has {}",
                self.environment_name(id),
                presented.version()
            )),
            (Some(_), _) => None,
            (None, Some(version)) => Some(format!("Unknown environment: {id} version {version}")),
            (None, None) => Some(format!("Unknown environment: {id}")),
        }
    }

    /// How the hand menu names the joined space's environment: what this build lacks of it, or else its name.
    fn shown_environment(&self, space: &SpaceState) -> String {
        self.environment_notice(space)
            .unwrap_or_else(|| self.environment_name(&space.environment))
    }

    /// Open the first creation ring.
    fn open_menu(&mut self) {
        self.close_menu();
        self.step = LobbyStep::Backend;
    }

    /// Step back to the previous creation ring, selecting what was chosen on it, or close the rings from the first.
    fn back_ring(&mut self) {
        match self.step {
            LobbyStep::Spaces => {}
            LobbyStep::Backend => self.close_menu(),
            LobbyStep::Directory => {
                self.step = LobbyStep::Backend;
                self.menu = self
                    .backends
                    .iter()
                    .position(|backend| Some(&backend.name) == self.menu_backend.as_ref())
                    .unwrap_or(0);
                self.menu_backend = None;
            }
            LobbyStep::Session => {
                self.step = LobbyStep::Directory;
                self.menu = self
                    .roots
                    .iter()
                    .position(|root| Some(&root.path) == self.menu_directory.as_ref())
                    .unwrap_or(0);
                self.menu_directory = None;
                self.sessions.clear();
            }
            LobbyStep::Environment => {
                self.menu_create = None;
                if self.attaches() {
                    self.step = LobbyStep::Session;
                    self.menu = 0;
                } else {
                    self.step = LobbyStep::Directory;
                    self.menu = self
                        .roots
                        .iter()
                        .position(|root| Some(&root.path) == self.menu_directory.as_ref())
                        .unwrap_or(0);
                }
            }
        }
    }

    /// A title for a space the client creates, from the backend and the chosen root, since the headset cannot type one.
    fn lobby_title(&self) -> String {
        let backend = self.menu_backend.as_deref().unwrap_or("agent");
        let label = self
            .roots
            .iter()
            .find(|root| Some(&root.path) == self.menu_directory.as_ref())
            .map(|root| root.label.as_str())
            .unwrap_or("");
        format!("{backend} in {label}")
    }

    /// Create the space chosen on the rings, bound to `session` if one was chosen, then choose its environment if the server offers this client more than one.
    fn create(&mut self, session: Option<SessionSummary>) {
        let (Some(backend), Some(directory)) =
            (self.menu_backend.clone(), self.menu_directory.clone())
        else {
            return;
        };
        let title = match &session {
            Some(session) if !session.title.trim().is_empty() => session.title.clone(),
            _ => self.lobby_title(),
        };
        let create = Creation {
            title,
            backend,
            directory,
            session: session.map(|session| session.id),
            instructions: self.next_instructions.take(),
        };
        self.submit(create);
    }

    /// Return to the server list, closing the connection.
    fn leave_server(&mut self) {
        self.actions.push(Action::Disconnect);
        self.connected = None;
        self.jumping = false;
        self.state = None;
        // What belonged to the connection goes with it, so that a token the server sends as the participant leaves, in the same frame, is not passed on.
        self.certificate = None;
        self.pairing = None;
        self.knock = None;
        self.spaces.clear();
        self.listed = false;
        self.close_space();
        self.close_menu();
        self.talk = None;
        self.leaving = false;
        self.error = None;
        self.ended = None;
        self.mode = Mode::Servers;
    }

    /// The pending approval that a press would decide, if any.
    fn approval(&self) -> Option<&tomori_protocol::Approval> {
        self.space
            .as_ref()
            .and_then(|space| space.approval.as_ref())
            .filter(|approval| self.decided.as_ref() != Some(&approval.id))
    }

    /// Whether the right stick chooses among an approval's choices, rather than selecting queued entries.
    #[cfg(any(not(target_os = "android"), test))]
    pub fn is_choosing(&self) -> bool {
        self.mode == Mode::Space && self.approval().is_some()
    }

    /// Left stick: choose a server or space, or page through messages.
    pub fn navigate(&mut self, horizontal: i32, vertical: i32) {
        match self.mode {
            Mode::Servers => {
                self.server = shift(self.server, vertical, self.servers.len());
                if vertical != 0 {
                    self.chosen = true;
                    self.missing = None;
                }
            }
            Mode::Lobby => match self.step {
                LobbyStep::Spaces => {
                    self.selected = shift(self.selected, vertical, self.step_list(self.step))
                }
                step => self.menu = shift(self.menu, vertical, self.step_list(step)),
            },
            Mode::Space => {
                let Some(space) = &self.space else {
                    return;
                };
                if let Some(selected) = self.space_menu {
                    self.space_menu = Some(shift(selected, vertical, self.menu_items().len()));
                    return;
                }
                let (count, messages) = (items(space), space.messages.len());
                if self.request.is_none() && horizontal != 0 {
                    self.message = shift(self.message, horizontal, count);
                    self.page = 0;
                }
                if vertical != 0 {
                    self.page = self.page.saturating_add_signed(vertical as isize);
                }
                let last_page = paginate(&self.body().1, self.body_rows()).len() - 1;
                self.page = self.page.min(last_page);
                // Back at the newest page, follow new messages again.
                self.follow = self.request.is_none()
                    && self.message + 1 == messages.max(1)
                    && self.page == last_page;
            }
        }
    }

    /// Right stick left or right: choose among an approval's choices: allow once, each standing option, then deny.
    #[cfg(any(not(target_os = "android"), test))]
    pub fn choose(&mut self, direction: i32) {
        if let Some(count) = self.approval().map(|approval| approval.options.len() + 2) {
            self.option = shift(self.option, direction, count);
        }
    }

    /// The entries still waiting in the queue, oldest first, which any participant may cancel.
    fn waiting(&self) -> Vec<&tomori_protocol::QueueEntry> {
        self.space
            .as_ref()
            .map(|space| {
                space
                    .queue
                    .iter()
                    .filter(|entry| entry.state == QueueState::Queued)
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Right stick up or down, while no approval is presented: select a waiting entry in the queue strip, which A then cancels. Moving past either end clears the selection.
    #[cfg(any(not(target_os = "android"), test))]
    pub fn select_queued(&mut self, direction: i32) {
        if self.mode != Mode::Space || self.is_choosing() || self.space_menu.is_some() {
            return;
        }
        if !self.role.participates() {
            self.notice = Some(VISITING.into());
            return;
        }
        let waiting: Vec<String> = self
            .waiting()
            .iter()
            .map(|entry| entry.id.clone())
            .collect();
        let current = self
            .queue_selected
            .as_ref()
            .and_then(|id| waiting.iter().position(|entry| entry == id));
        let next = match current {
            None if direction > 0 => (!waiting.is_empty()).then_some(0),
            None => waiting.len().checked_sub(1),
            Some(index) => index
                .checked_add_signed(direction as isize)
                .filter(|index| *index < waiting.len()),
        };
        self.queue_selected = next.map(|index| waiting[index].clone());
    }

    /// Advance what waits on time in a space at `now`: a queued entry's cancellation whose undo time has run out is sent. Call every frame.
    pub fn tick_space(&mut self, now: Instant) {
        if let Some((entry, due)) = &self.cancelling
            && now >= *due
        {
            let entry = entry.clone();
            self.cancelling = None;
            self.cancel_now(entry);
        }
        // A selected entry that has been sent or cancelled is no longer selected.
        if let Some(selected) = &self.queue_selected
            && !self.waiting().iter().any(|entry| &entry.id == selected)
        {
            self.queue_selected = None;
        }
    }

    /// B held in a space for `held`, or released (`None`). Holding it for [`LEAVE_HOLD`] leaves the space, once per hold. Returns whether this call left.
    pub fn hold_back(&mut self, held: Option<Duration>) -> bool {
        let Some(held) = held.filter(|_| self.mode == Mode::Space) else {
            self.leave_hold = None;
            return false;
        };
        let left = self.leave_hold.is_some_and(|(_, left)| left);
        self.leave_hold = Some((held, left));
        if held >= LEAVE_HOLD && !left {
            self.leave_hold = Some((held, true));
            self.leave();
            return true;
        }
        false
    }

    /// Whether the right stick up and down selects queued entries now, rather than doing anything else.
    #[cfg(test)]
    pub fn selects_queue(&self) -> bool {
        self.mode == Mode::Space
            && self.role.participates()
            && !self.is_choosing()
            && self.space_menu.is_none()
            && !self.waiting().is_empty()
    }

    /// Leave the space for the lobby. A cancellation still in its undo time is sent first, as it was asked for.
    fn leave(&mut self) {
        if let Some((entry, _)) = self.cancelling.take() {
            self.cancel_now(entry);
        }
        self.leaving = true;
        self.close_space();
        self.listed = false;
        self.mode = Mode::Lobby;
        self.send(ClientMessage::Leave);
    }

    /// The entries of the space's list: its modes, then its speech setting where the server has one.
    fn menu_items(&self) -> Vec<MenuItem> {
        let Some(space) = &self.space else {
            return Vec::new();
        };
        let mut items: Vec<MenuItem> = space
            .modes
            .iter()
            .map(|mode| MenuItem::Mode(mode.clone()))
            .collect();
        if space.speech != SpeechSetting::Unknown {
            items.push(MenuItem::Speech(space.speech));
        }
        items
    }

    /// A while the ray highlights an arrival object chooses that object, as
    /// the trigger does, so that A never acts on a different item from the one
    /// pointed at. Otherwise A accepts as [`Self::accept`] does. Returns true
    /// when A has nothing to do here.
    pub fn press_a(&mut self, highlighted: Option<&arrival::Key>) -> bool {
        self.refused = false;
        match highlighted {
            Some(key) if self.arrival().is_some() => {
                self.choose_object(key);
                false
            }
            _ => self.accept(),
        }
    }

    /// Whether the last press of A was refused, saying why, as just after a request appears, so that it is not felt as a choice. See `design/feedback.md`.
    pub fn a_refused(&self) -> bool {
        self.refused
    }

    /// A: connect, join, confirm the chosen approval choice, choose from the space's list, or cancel the selected queued entry or undo that. Returns true when A has nothing to do here. A never resets the swarm in a space, where it would hide what A does; see `design/interaction.md`.
    pub fn accept(&mut self) -> bool {
        // Choosing anything takes over from a jump still waiting for the listing.
        self.jumping = false;
        self.refused = false;
        match self.mode {
            Mode::Servers => {
                match self.servers.get(self.server).cloned() {
                    Some(choice) if choice.is_available() => self.connect(choice),
                    Some(choice) => {
                        self.refused = true;
                        self.error = Some(format!(
                            "Cannot join {}: {}.",
                            choice.label,
                            choice.unavailable.unwrap_or_default()
                        ));
                    }
                    None => {}
                }
                false
            }
            Mode::Lobby => {
                match self.step {
                    LobbyStep::Spaces => match self.spaces.get(self.selected) {
                        Some(space) => {
                            let space = space.id.clone();
                            self.join(space);
                        }
                        // The create object opens its first ring.
                        None if self.creates() => {
                            self.ended = None;
                            self.open_menu();
                        }
                        None => {}
                    },
                    LobbyStep::Backend => {
                        if let Some(backend) = self.backends.get(self.menu) {
                            self.menu_backend = Some(backend.name.clone());
                            self.menu = 0;
                            self.step = LobbyStep::Directory;
                        }
                    }
                    LobbyStep::Directory => {
                        if let Some(root) = self.roots.get(self.menu) {
                            self.menu_directory = Some(root.path.clone());
                            if self.attaches() {
                                self.menu = 0;
                                self.step = LobbyStep::Session;
                                self.list_sessions();
                            } else {
                                self.create(None);
                            }
                        }
                    }
                    LobbyStep::Session => {
                        let session = self
                            .menu
                            .checked_sub(1)
                            .and_then(|index| self.sessions.get(index))
                            .cloned();
                        self.create(session);
                    }
                    LobbyStep::Environment => {
                        let environment = self
                            .environments
                            .get(self.menu)
                            .map(|offered| offered.id.clone());
                        if let (Some(environment), Some(create)) =
                            (environment, self.menu_create.take())
                        {
                            self.send(create.message(Some(environment)));
                            self.close_menu();
                        }
                    }
                }
                false
            }
            Mode::Space => {
                if let Some(selected) = self.space_menu {
                    match self.menu_items().get(selected) {
                        Some(MenuItem::Mode(mode)) => {
                            let mode = mode.id.clone();
                            self.send(ClientMessage::SetMode { mode });
                        }
                        Some(MenuItem::Speech(speech)) => {
                            let speech = match speech {
                                SpeechSetting::Brief => SpeechSetting::Full,
                                _ => SpeechSetting::Brief,
                            };
                            self.send(ClientMessage::SetSpeech { speech });
                        }
                        None => {}
                    }
                    self.space_menu = None;
                    return false;
                }
                if self
                    .space
                    .as_ref()
                    .is_some_and(|space| space.approval.is_some() || space.question.is_some())
                    && self
                        .request_at
                        .is_some_and(|at| at.elapsed() < REQUEST_GRACE)
                {
                    self.refused = true;
                    self.notice = Some("Read the request before choosing.".into());
                    return false;
                }
                if self.decide_on_wrist
                    && self
                        .space
                        .as_ref()
                        .is_some_and(|space| space.approval.is_some() || space.question.is_some())
                {
                    self.refused = true;
                    self.notice = Some("Turn your right wrist to decide.".into());
                    return false;
                }
                if self.approval().is_some() {
                    self.decide();
                    return false;
                }
                // Removing queued entries is on the right wrist.
                if self.decide_on_wrist {
                    return true;
                }
                // A decision on its way still owns the button.
                if self
                    .space
                    .as_ref()
                    .is_some_and(|space| space.approval.is_some())
                {
                    return false;
                }
                // A undoes a cancellation still in its undo time, or cancels the selected entry after it.
                if (self.cancelling.is_some() || self.queue_selected.is_some())
                    && !self.role.participates()
                {
                    self.refused = true;
                    self.notice = Some(VISITING.into());
                    return false;
                }
                if let Some((entry, _)) = self.cancelling.take() {
                    self.queue_selected = Some(entry);
                    self.notice = Some("Not cancelled.".into());
                    false
                } else if let Some(entry) = self.queue_selected.take() {
                    self.cancelling = Some((entry, Instant::now() + CANCEL_UNDO));
                    self.notice = None;
                    false
                } else {
                    true
                }
            }
        }
    }

    /// Send the decision for the chosen choice of the pending approval.
    fn decide(&mut self) {
        let Some(approval) = self.approval() else {
            return;
        };
        let request = approval.id.clone();
        let (decision, option) = match self.option.checked_sub(1) {
            None => (Decision::AllowOnce, None),
            Some(index) if index < approval.options.len() => {
                (Decision::Grant, Some(approval.options[index].id.clone()))
            }
            Some(_) => (Decision::Deny, None),
        };
        if self.send(ClientMessage::Decide {
            request: request.clone(),
            decision,
            option,
        }) {
            self.decided = Some(request);
        }
    }

    /// Decide approvals and remove queued entries only on the right wrist, as the headset and desktop window do.
    pub fn decide_on_wrist(&mut self) {
        self.decide_on_wrist = true;
    }

    /// The right wrist's transcript, `columns` wide, read-only when shown from hand tracking alone. See `design/wrist-transcript.md`.
    pub fn transcript(&mut self, columns: usize, hands_only: bool) -> Vec<crate::ui_text::Row> {
        let now = Instant::now();
        self.observe_problem(now);
        // The problem line shows it, so it has been seen and no longer cues the wrist.
        if let Some(problem) = self.problem() {
            self.problem_noticed = Some(problem);
        }
        let problem = self.problem_text(now);
        let answerable = self.answerable().is_some();
        let Some(space) = self.space.as_ref().filter(|_| self.mode == Mode::Space) else {
            return Vec::new();
        };
        let cancelling = self.cancelling.as_ref().map(|(id, due)| {
            let left = due.saturating_duration_since(now).as_secs_f32() / CANCEL_UNDO.as_secs_f32();
            (id.as_str(), left)
        });
        let grace = self
            .request_at
            .is_some_and(|at| at.elapsed() < REQUEST_GRACE);
        // A decision is only awaited for the approval shown: another request forgets it.
        let deciding = self.decided.is_some();
        // Laid out again only when something it shows has changed, as it is drawn every frame.
        let key = TranscriptKey {
            revision: self.revision,
            wrist: self.wrist.clone(),
            role: self.role,
            source: self.source,
            deciding,
            cancelling: cancelling.map(|(id, left)| (id.to_owned(), (left * 60.0) as u32)),
            problem: problem.clone(),
            answerable,
            hands_only,
            grace,
            columns,
        };
        if let Some((laid_out, rows)) = &self.laid_out
            && *laid_out == key
        {
            return rows.clone();
        }
        let input = crate::transcript::Input {
            you: self.source,
            participates: self.role.participates(),
            known: Some(&self.known),
            deciding,
            cancelling,
            problem,
            answerable,
            hands_only,
            grace,
        };
        let rows = crate::transcript::layout(space, &input, &mut self.wrist, columns);
        // Laying out settles the focus, which is what the next frame compares.
        self.laid_out = Some((
            TranscriptKey {
                wrist: self.wrist.clone(),
                ..key
            },
            rows.clone(),
        ));
        rows
    }

    /// What the transcript's stick moves through.
    fn wrist_items(&self) -> Vec<crate::transcript::Item> {
        let Some(space) = self.space.as_ref().filter(|_| self.mode == Mode::Space) else {
            return Vec::new();
        };
        let input = crate::transcript::Input {
            you: self.source,
            participates: self.role.participates(),
            known: Some(&self.known),
            deciding: self.decided.is_some(),
            ..Default::default()
        };
        // The width it was last laid out at, on the wrist or detached, so that moving through it matches what is shown.
        let columns = self
            .laid_out
            .as_ref()
            .map_or(crate::transcript::WRIST_COLUMNS, |(key, _)| key.columns);
        crate::transcript::items(space, &input, &self.wrist, columns)
    }

    /// The newest message's id, which the transcript counts newer messages from.
    fn newest(&self) -> Option<String> {
        self.space
            .as_ref()
            .and_then(|space| space.messages.last())
            .map(|message| message.id.clone())
    }

    /// The right wrist's transcript opened: while a request waits, focus its title and give A a moment of grace again, so that a reflex press decides nothing.
    pub fn wrist_opened(&mut self) {
        let pending = self
            .space
            .as_ref()
            .is_some_and(|space| space.approval.is_some() || space.question.is_some());
        if pending && self.mode == Mode::Space {
            let newest = self.newest();
            self.wrist.focus_card(newest.as_deref());
            self.request_at = Some(Instant::now());
        }
    }

    /// The right stick up (`-1`) or down (`1`) on the transcript.
    pub fn wrist_step(&mut self, direction: i32) {
        let items = self.wrist_items();
        let newest = self.newest();
        self.wrist.step(&items, direction, newest.as_deref());
    }

    /// The right stick left (`-1`) or right (`1`) on the transcript: the previous or next turn.
    pub fn wrist_jump(&mut self, direction: i32) {
        let items = self.wrist_items();
        let newest = self.newest();
        self.wrist.jump(&items, direction, newest.as_deref());
    }

    /// The right wrist's focus: a turn, a request's button or a queued entry.
    pub fn wrist_focus(&self) -> Option<&crate::transcript::Focus> {
        self.wrist.focus.as_ref()
    }

    /// A on the transcript: expand or collapse a turn, confirm a choice, or remove a queued entry or undo that. Returns whether it did any of them, rather than nothing or refusing.
    pub fn wrist_accept(&mut self) -> bool {
        use crate::transcript::Focus;
        let items = self.wrist_items();
        self.wrist.settle(&items);
        match self.wrist.focus.clone() {
            None | Some(Focus::Card(0)) => false,
            Some(Focus::Group(..)) => {
                self.wrist.toggle(&items);
                true
            }
            // Settled, the focus is only on what this participant may act on: a visitor is offered no choices or entries to remove.
            Some(Focus::Card(choice)) => {
                if self
                    .request_at
                    .is_some_and(|at| at.elapsed() < REQUEST_GRACE)
                {
                    self.notice = Some("Read the request before choosing.".into());
                    false
                } else {
                    // Settled, a choice is focused only while an approval waits for this participant's decision.
                    self.option = choice - 1;
                    self.decide();
                    true
                }
            }
            Some(Focus::Queue(entry)) => {
                match self.cancelling.take() {
                    // A again within the undo time keeps it.
                    Some((cancelling, _)) if cancelling == entry => {
                        self.notice = Some("Not removed.".into());
                        true
                    }
                    other => {
                        if let Some((other, _)) = other {
                            self.cancel_now(other);
                        }
                        // Settled, the focus is only on an entry still waiting.
                        self.cancelling = Some((entry, Instant::now() + CANCEL_UNDO));
                        self.notice = None;
                        true
                    }
                }
            }
        }
    }

    /// B on the transcript: collapse, stop focusing and follow live, or dismiss the problem. Returns false when there was nothing to back out of, and never decides, cancels or leaves.
    pub fn wrist_back(&mut self) -> bool {
        if self.wrist.back() {
            return true;
        }
        if self.error.take().is_some() {
            return true;
        }
        if let Some(error) = self.space.as_ref().and_then(|space| space.error.clone())
            && self.dismissed.as_ref() != Some(&error)
        {
            self.dismissed = Some(error);
            return true;
        }
        false
    }

    /// What waits for this participant on the right wrist, for its cue at each frame: `Some(false)` for a request, until it is decided or answered, and `Some(true)` for a problem not yet seen. See `design/wrist-transcript.md`.
    pub fn wrist_waiting(&mut self) -> Option<bool> {
        let problem = self.problem();
        // A problem seen stays seen only while it lasts; shown again later, it is news again.
        if self.problem_noticed != problem {
            self.problem_noticed = None;
        }
        let space = self.space.as_ref().filter(|_| self.mode == Mode::Space)?;
        if self.approval().is_some() || space.question.is_some() {
            Some(false)
        } else {
            problem
                .filter(|problem| self.problem_noticed.as_ref() != Some(problem))
                .map(|_| true)
        }
    }

    /// What the status pill shows at `now`, while in a space and it is not turned off. See `design/interaction.md`.
    pub fn pill_status(&mut self, now: Instant) -> Option<crate::pill::Status> {
        use crate::pill::{Need, Status};
        if !self.pill {
            return None;
        }
        self.observe_problem(now);
        let problem = self.problem_text(now);
        let space = self.space.as_ref().filter(|_| self.mode == Mode::Space)?;
        let connection = match &self.state {
            None | Some(ConnectionState::Connecting { attempt: 1 }) => Some("Connecting...".into()),
            Some(ConnectionState::Connecting { attempt }) => {
                Some(format!("Connecting (attempt {attempt})..."))
            }
            Some(ConnectionState::Disconnected { retry_in, .. }) => Some(format!(
                "Offline, retrying in {} s",
                retry_in.as_secs_f32().ceil()
            )),
            Some(ConnectionState::Closed) => Some("Disconnected.".into()),
            // Connected, or in a state a later client knows of, which has nothing to say.
            Some(_) => None,
        };
        let leaving = self
            .leave_hold
            .is_some_and(|(held, left)| !left && held >= Duration::from_millis(150));
        let need = if self.approval().is_some() {
            Some(Need::Approval)
        } else if space.question.is_some() {
            Some(Need::Question)
        } else if let Some(problem) = problem {
            Some(Need::Problem(problem))
        } else if let Some(connection) = connection {
            Some(Need::Connection(connection))
        } else if leaving {
            Some(Need::Leaving)
        } else {
            self.notice.clone().map(Need::Notice)
        };
        Some(Status {
            activity: space.activity,
            mode: space
                .current_mode()
                .map(|mode| mode.name.clone())
                .or_else(|| space.mode.clone().filter(|_| !space.modes.is_empty())),
            title: space.title.clone(),
            replaying: self.replaying,
            need,
        })
    }

    /// Turn the status pill on or off, on this headset.
    pub fn toggle_pill(&mut self) {
        self.pill = !self.pill;
    }

    /// The problem with how long it has been shown, on one line, for the transcript.
    fn problem_text(&self, now: Instant) -> Option<String> {
        let problem = self.problem()?;
        Some(format!(
            "! {} {}",
            age(self.problem_age(&problem, now)),
            problem.replace('\n', " ")
        ))
    }

    /// How long `problem` has been shown at `now`: no time at all if it was not shown before.
    fn problem_age(&self, problem: &str, now: Instant) -> Duration {
        match &self.problem_seen {
            Some((seen, since)) if seen == problem => now.saturating_duration_since(*since),
            _ => Duration::ZERO,
        }
    }

    /// B: in a space, close what is open, or dismiss the problem shown; it never decides an approval, cancels input or leaves, which takes holding it (see [`Self::hold_back`]). In the lobby, dismiss what is shown or return to the server list.
    pub fn back(&mut self) {
        self.jumping = false;
        match self.mode {
            Mode::Servers => {}
            Mode::Lobby if self.dismissable() => {
                self.error = None;
                self.ended = None;
            }
            // At the spaces there is nothing to return from, so go to the server list.
            Mode::Lobby if self.step == LobbyStep::Spaces => self.leave_server(),
            Mode::Lobby => self.back_ring(),
            Mode::Space => {
                if self.space_menu.take().is_some() || self.queue_selected.take().is_some() {
                    return;
                }
                // Stop waiting for a new name, and dismiss why the last was not changed.
                if std::mem::take(&mut self.naming) {
                    if matches!(self.outcome, Some((Outcome::NameFailed(_), _))) {
                        self.error = None;
                    }
                    return;
                }
                // The card saying that no speech was heard, which is no problem.
                if let Some((Outcome::Unheard, at)) = self.outcome
                    && self.unheard_shown(Instant::now(), at)
                {
                    self.outcome = None;
                    return;
                }
                if self.error.take().is_some() {
                    return;
                }
                if let Some(error) = self.space.as_ref().and_then(|space| space.error.clone())
                    && self.dismissed.as_ref() != Some(&error)
                {
                    self.dismissed = Some(error);
                    return;
                }
                self.notice = Some("Hold B to leave the space.".into());
            }
        }
    }

    /// Ask the chosen backend for its sessions in the chosen directory.
    fn list_sessions(&mut self) {
        if let (Some(backend), Some(directory)) =
            (self.menu_backend.clone(), self.menu_directory.clone())
        {
            self.send(ClientMessage::ListSessions { backend, directory });
        }
    }

    /// Y: browse again, refresh the spaces or the sessions being shown, or in a space open or close the list of the agent's modes, with their descriptions, and the speech setting. A space with neither fetches its state again instead.
    pub fn refresh(&mut self) {
        // In a space a problem stays until it is dismissed; see `design/interaction.md`.
        if self.mode != Mode::Space {
            self.error = None;
        }
        match self.mode {
            Mode::Servers => self.actions.push(Action::Rescan),
            Mode::Lobby => match self.step {
                LobbyStep::Session => self.list_sessions(),
                LobbyStep::Spaces
                | LobbyStep::Backend
                | LobbyStep::Directory
                | LobbyStep::Environment => self.list(),
            },
            Mode::Space => {
                if self.space_menu.take().is_some() {
                    return;
                }
                let Some(space) = &self.space else {
                    return;
                };
                let items = self.menu_items();
                if items.is_empty() {
                    let space = space.id.clone();
                    self.joining = Some(space.clone());
                    self.send(ClientMessage::Join { space });
                    return;
                }
                // The list opens on the current mode, so A alone keeps it.
                let current = items
                    .iter()
                    .position(|item| matches!(item, MenuItem::Mode(mode) if Some(&mode.id) == space.mode.as_ref()))
                    .unwrap_or(0);
                self.space_menu = Some(current);
            }
        }
    }

    /// X pressed or released: push-to-talk. While held, the microphone sends `agent` audio, heard by the other participants and transcribed for the agent; releasing sends it to be queued, or, while a question Tomori can answer is presented, as the answer to its current field.
    pub fn talk(&mut self, pressed: bool) {
        if !pressed {
            if let Some(Talk::Recording {
                utterance,
                heard,
                answering,
                naming,
                ..
            }) = self.talk.take()
            {
                self.actions.push(Action::EndUtterance);
                self.talk = Some(Talk::Sending {
                    utterance,
                    heard,
                    answering,
                    received: None,
                    naming,
                });
            }
            return;
        }
        if matches!(self.talk, Some(Talk::Recording { .. })) {
            return;
        }
        if self.mode != Mode::Space || self.space.is_none() {
            self.notice = Some("Join a space to talk to the agent.".into());
            return;
        }
        if !self.role.participates() {
            self.notice = Some(VISITING.into());
            return;
        }
        if !matches!(self.state, Some(ConnectionState::Connected { .. })) {
            self.error = Some("Offline; cannot record.".into());
            return;
        }
        self.utterances += 1;
        let utterance = format!("{}-{}", self.launch, self.utterances);
        // A new recording replaces what the indicator said of the last.
        self.outcome = None;
        // A problem shown stays until it is dismissed or replaced; see `design/interaction.md`.
        self.notice = None;
        // Words chosen to say a new name answer nothing; see `design/security.md`.
        let naming = std::mem::take(&mut self.naming) && self.renames();
        let answer =
            self.answerable()
                .filter(|_| !naming)
                .map(|question| tomori_protocol::AnswerTarget {
                    question: question.id.clone(),
                    field: question.field.clone(),
                });
        let answering = answer.is_some();
        // What is pointed at as X is pressed is what the words are about, captured now, as the server captures it;
        // pointing elsewhere while talking does not change it. An answer goes to the question presented, and a name to
        // the participant, so either is about nothing pointed at.
        let pointed = self
            .pointing_at
            .clone()
            .filter(|target| !naming && self.knows(target));
        let attention = pointed.iter().filter(|_| !answering).cloned().collect();
        self.actions.push(Action::BeginUtterance(if naming {
            Utterance::new(utterance.clone(), self.capture_generation).naming()
        } else {
            Utterance::new(utterance.clone(), self.capture_generation)
                .answering(answer)
                .attending(attention)
        }));
        self.talk = Some(Talk::Recording {
            utterance,
            started: Instant::now(),
            heard: String::new(),
            answering,
            naming,
            pointed,
        });
    }

    /// The worker the participant points at now, by identifier, or `None`: from the headset's ray each frame, or a
    /// console command. The worker pointed at as X is pressed is sent as the words begin, and pointing elsewhere
    /// while X is held does not change it. See `design/interaction.md`.
    pub fn point_at(&mut self, worker: Option<&str>) {
        self.point(worker.map(tomori_protocol::Resource::worker));
    }

    /// What the participant points at now, a worker or a card, as [`Self::point_at`] says of workers.
    pub fn point(&mut self, target: Option<tomori_protocol::Resource>) {
        if self.pointing_at != target {
            self.pointing_at = target;
        }
    }

    /// The participant points at `object`, something an environment shows, called `label`: attended to as
    /// `Resource::object` while they talk, as a worker or card is, and named `label` while they do.
    pub fn point_at_object(&mut self, object: &str, label: &str) {
        self.object_labels
            .insert(object.to_owned(), label.to_owned());
        self.point(Some(tomori_protocol::Resource::object(object)));
    }

    /// The worker the participant points at now, as the console shows it.
    #[cfg(not(target_os = "android"))]
    pub fn pointing_at(&self) -> Option<&str> {
        match &self.pointing_at {
            Some(tomori_protocol::Resource::Worker { worker, .. }) => Some(worker),
            _ => None,
        }
    }

    /// Whether the joined space has `target`, a worker or a card.
    fn knows(&self, target: &tomori_protocol::Resource) -> bool {
        self.about(target).is_some()
    }

    /// What `target` is called, as the recording indicator and status line name it: a worker's title, or a card's
    /// number and title. `None` when the joined space does not have it, or for a kind this client does not show.
    fn about(&self, target: &tomori_protocol::Resource) -> Option<String> {
        use tomori_protocol::Resource;
        match target {
            Resource::Worker { worker, .. } => self
                .joined_space()
                .and_then(|(space, _)| space.workers.iter().find(|known| &known.id == worker))
                .map(|worker| worker.title.clone()),
            Resource::Card { card, .. } => self
                .cards()
                .and_then(|cards| cards.cards.iter().find(|known| &known.id == card))
                .map(|card| format!("#{} {}", card.number, card.title)),
            Resource::Object { object, .. } => self
                .joined_space()
                .and_then(|_| self.object_labels.get(object).cloned()),
            _ => None,
        }
    }

    /// The cards of the joined space, while its environment shows them. See `design/github.md`.
    pub fn cards(&self) -> Option<&tomori_protocol::Cards> {
        self.cards.as_ref().filter(|cards| {
            self.joined_space()
                .is_some_and(|(space, _)| space.id == cards.space)
        })
    }

    /// Keep `update` of `environment` at `version`, if that is the joined space's environment as the space says.
    fn receive_environment(
        &mut self,
        environment: String,
        version: u32,
        update: crate::channel::Update,
    ) {
        if let Some(space) = self.space.as_ref().filter(|space| {
            space.environment == environment
                && space
                    .environment_version
                    .is_none_or(|named| named == version)
        }) {
            self.channel
                .receive(&space.id, &environment, version, update);
        }
    }

    /// What the server side of `environment` at `version` sent for the joined space since it was last taken, in order:
    /// its state, which replaces whatever was sent before, and its events. Nothing for another environment or version,
    /// or outside a space. See `design/environments.md`.
    pub fn environment_updates(
        &mut self,
        environment: &str,
        version: u32,
    ) -> Vec<crate::channel::Update> {
        match self.joined_space() {
            Some((space, _)) => {
                let space = space.id.clone();
                self.channel.take(&space, environment, version)
            }
            None => Vec::new(),
        }
    }

    /// Ask the server side of `environment` at `version`, the joined space's, for `intent`. Only a participant asks, of
    /// a server that has sent the environment's state in this space, so that it runs the side and takes intents; says
    /// whether it asked.
    pub fn environment_intent(
        &mut self,
        environment: &str,
        version: u32,
        intent: tomori_protocol::Payload,
    ) -> bool {
        let asking = self.role.participates()
            && self.joined_space().is_some_and(|(space, connected)| {
                connected && self.channel.has(&space.id, environment, version)
            });
        if asking {
            self.actions
                .push(Action::Send(ClientMessage::EnvironmentIntent {
                    environment: environment.to_owned(),
                    version,
                    intent,
                }));
        }
        asking
    }

    /// Put the card `card` at `placement`, or back in the inbox, for everyone in the space. Only a participant can.
    pub fn place_card(&mut self, card: String, placement: Option<tomori_protocol::Pose>) {
        if !self.role.participates() || self.cards().is_none() {
            return;
        }
        self.actions
            .push(Action::Send(ClientMessage::PlaceCard { card, placement }));
    }

    /// Ask to watch the joined space's cards, once for each space and connection, if it shows them and this
    /// participant takes part; forget them if it does not show them.
    fn watch_cards(&mut self) {
        let Some(space) = self.space.as_ref().filter(|_| self.mode == Mode::Space) else {
            return;
        };
        if !space.cards {
            self.cards = None;
            self.watching = None;
            return;
        }
        if self.role.participates() && self.watching.as_ref() != Some(&space.id) {
            self.watching = Some(space.id.clone());
            self.actions.push(Action::Send(ClientMessage::WatchCards));
        }
    }

    /// The presented question, when this participant can answer it by voice: Tomori supports it, and the server says it routes spoken answers. See `design/spaces.md`.
    fn answerable(&self) -> Option<&tomori_protocol::Question> {
        let space = self.space.as_ref().filter(|_| self.role.participates())?;
        space
            .question
            .as_ref()
            .filter(|question| question.supported && space.voice_answers)
    }

    /// Toggle an explicitly enabled microphone for the space, not the agent.
    pub fn toggle_live(&mut self) {
        if self.live {
            self.stop_live();
            return;
        }
        if matches!(self.talk, Some(Talk::Recording { .. })) {
            self.notice = Some("Release X before enabling live audio.".into());
            return;
        }
        if self.mode != Mode::Space || self.space.is_none() {
            self.notice = Some("Join a space to use live audio.".into());
            return;
        }
        if !self.role.participates() {
            self.notice = Some(VISITING.into());
            return;
        }
        if !matches!(self.state, Some(ConnectionState::Connected { .. })) {
            self.error = Some("Offline; cannot use live audio.".into());
            return;
        }
        self.notice = None;
        self.live = true;
        self.actions
            .push(Action::StartLive(self.capture_generation));
    }

    /// Stop live audio on focus loss, departure, failure, or explicit mute.
    pub fn stop_live(&mut self) {
        if self.live {
            self.live = false;
            let pending = self
                .actions
                .iter()
                .any(|action| matches!(action, Action::StartLive(_)));
            self.actions
                .retain(|action| !matches!(action, Action::StartLive(_)));
            if !pending {
                // Mute the live gate before a previously queued X release can
                // restore it, for example when focus is lost just afterward.
                let before_end = self
                    .actions
                    .iter()
                    .position(|action| matches!(action, Action::EndUtterance))
                    .unwrap_or(self.actions.len());
                self.actions.insert(before_end, Action::StopLive);
            }
        }
    }

    /// Abandon a recording without sending it, as when the session loses focus. Words already released are still transcribed.
    pub fn stop_talking(&mut self) {
        if let Some(Talk::Recording {
            utterance, naming, ..
        }) = &self.talk
        {
            // A name not said yet is still to be said: X says it again.
            self.naming = *naming && self.renames();
            let pending = self.actions.iter().any(
                |action| matches!(action, Action::BeginUtterance(begun) if begun.id == *utterance),
            );
            self.actions.retain(
                |action| !matches!(action, Action::BeginUtterance(begun) if begun.id == *utterance),
            );
            if !pending {
                self.actions.push(Action::CancelUtterance);
            }
            self.talk = None;
        }
    }

    /// The microphone could not record, so the recording is abandoned.
    pub fn microphone_failed(&mut self, error: impl Into<String>) {
        let naming = self.talk.as_ref().is_some_and(Talk::naming);
        // Words being recorded are lost with the capture.
        let recording = matches!(self.talk, Some(Talk::Recording { .. }));
        self.stop_live();
        // X may have been released while capture failed, before the queued end
        // was sent. Cancel instead of finalizing an incomplete utterance.
        // Only an utterance whose end has not been performed yet is lost; a
        // failure after that comes from the live capture reopened after it,
        // and the sent utterance is still transcribed.
        let mut cancelled = false;
        if let Some(Talk::Sending { utterance, .. }) = &self.talk
            && self
                .actions
                .iter()
                .any(|action| matches!(action, Action::EndUtterance))
        {
            cancelled = true;
            let pending = self.actions.iter().any(
                |action| matches!(action, Action::BeginUtterance(begun) if begun.id == *utterance),
            );
            self.actions.retain(|action| {
                !matches!(action, Action::EndUtterance)
                    && !matches!(action, Action::BeginUtterance(begun) if begun.id == *utterance)
            });
            if !pending {
                self.actions.push(Action::CancelUtterance);
            }
        }
        self.stop_talking();
        if cancelled {
            self.talk = None;
        }
        self.failed(error.into(), naming);
        // Words already with the server are still transcribed, and the live microphone failing loses none, so what becomes of the next words does not answer it.
        if !recording && !cancelled {
            self.unsent = None;
        }
    }

    /// Show `error` as the problem, and on the recording indicator, as words that were not sent, or, if they were `naming` the participant, as a name not changed, which X then says again. See `design/hand-menu.md`.
    fn failed(&mut self, error: String, naming: bool) {
        let problem = if naming {
            self.naming = self.renames();
            let problem = name_not_changed(&error);
            self.outcome = Some((Outcome::NameFailed(error), Instant::now()));
            problem
        } else {
            self.outcome = Some((Outcome::Failed(error.clone()), Instant::now()));
            error
        };
        // Unless later words are on their way, the recording card shows it where the words were spoken, with the not-sent cue, so the right wrist does not cue it too.
        if self.talk.is_none() {
            self.problem_noticed = Some(problem.clone());
        }
        // What becomes of the next words replaces it.
        self.unsent = Some(problem.clone());
        self.error = Some(problem);
    }

    /// What became of this participant's newest words replaces the problem saying that earlier words were not sent, which is no longer news.
    fn replace_unsent(&mut self) {
        if let Some(unsent) = self.unsent.take()
            && self.error.as_ref() == Some(&unsent)
        {
            self.error = None;
        }
    }

    /// What the recording indicator on the left controller says at `now`, if anything. See `design/hand-menu.md`.
    pub fn recording_phase(&self, now: Instant) -> Option<crate::recording::Phase> {
        use crate::recording::Phase;
        match &self.talk {
            Some(Talk::Recording {
                started,
                heard,
                naming: true,
                ..
            }) => {
                return Some(Phase::Naming {
                    elapsed: now.saturating_duration_since(*started),
                    heard: heard.clone(),
                });
            }
            Some(Talk::Sending {
                heard,
                naming: true,
                ..
            }) => {
                return Some(Phase::Renaming {
                    heard: heard.clone(),
                });
            }
            Some(Talk::Recording {
                started,
                heard,
                answering,
                pointed,
                ..
            }) => {
                // The worker or card the words will be about, which an answer never is.
                let about = pointed
                    .as_ref()
                    .filter(|_| !answering)
                    .and_then(|pointed| self.about(pointed));
                return Some(Phase::Recording {
                    about,
                    elapsed: now.saturating_duration_since(*started),
                    heard: heard.clone(),
                    answering: answering
                        .then(|| {
                            self.space
                                .as_ref()
                                .and_then(|space| space.question.as_ref())
                                .map(|question| question.title.clone())
                        })
                        .flatten(),
                });
            }
            Some(Talk::Sending {
                heard,
                answering,
                received,
                ..
            }) => {
                return Some(Phase::Sending {
                    heard: heard.clone(),
                    answering: *answering,
                    received: *received,
                });
            }
            None => {}
        }
        match &self.outcome {
            Some((Outcome::Sent { answered, text }, at))
                if now.saturating_duration_since(*at) < crate::recording::OUTCOME =>
            {
                // Where the words are in the queue, by this participant's newest entry with them.
                let position = (!answered)
                    .then(|| {
                        let waiting: Vec<_> = self
                            .space
                            .as_ref()?
                            .queue
                            .iter()
                            .filter(|entry| {
                                matches!(entry.state, QueueState::Queued | QueueState::Sending)
                            })
                            .collect();
                        waiting
                            .iter()
                            .rposition(|entry| {
                                Some(entry.source) == self.source
                                    && entry.text.trim() == text.trim()
                            })
                            .map(|index| index + 1)
                    })
                    .flatten();
                Some(Phase::Sent {
                    answered: *answered,
                    position,
                })
            }
            Some((Outcome::Named(name), at))
                if now.saturating_duration_since(*at) < crate::recording::OUTCOME =>
            {
                Some(Phase::Named { name: name.clone() })
            }
            // For a few seconds, unless B dismisses the problem or another recording starts first.
            Some((Outcome::Failed(reason), at))
                if self.error.as_ref() == Some(reason)
                    && now.saturating_duration_since(*at) < crate::recording::NOT_SENT =>
            {
                Some(Phase::Failed {
                    reason: reason.clone(),
                })
            }
            Some((Outcome::Unheard, at)) if self.unheard_shown(now, *at) => Some(Phase::Failed {
                reason: tomori_protocol::NO_SPEECH.into(),
            }),
            Some((Outcome::NameFailed(reason), _))
                if self.error.as_deref() == Some(name_not_changed(reason).as_str()) =>
            {
                Some(Phase::NameFailed {
                    reason: reason.clone(),
                })
            }
            _ => None,
        }
    }

    /// Whether the card still says, at `now`, that no speech was heard in words that ended `at`.
    fn unheard_shown(&self, now: Instant, at: Instant) -> bool {
        self.talk.is_none() && now.saturating_duration_since(at) < crate::recording::NOT_SENT
    }

    /// The longest utterance the server's recognition takes, when it says.
    pub fn utterance_limit(&self) -> Option<Duration> {
        self.utterance_limit
    }

    /// Whether the live microphone is on, which the `LIVE` badge on the left controller shows.
    pub fn live(&self) -> bool {
        self.live
    }

    /// Ending the utterance failed, so its words were not sent: stop showing
    /// them as on their way, and report why.
    pub fn utterance_not_sent(&mut self, error: impl Into<String>) {
        // A later utterance may already be queued behind this end: cancel it
        // as any capture failure does, before clearing what is on its way.
        self.microphone_failed(error);
        if matches!(self.talk, Some(Talk::Sending { .. })) {
            self.talk = None;
        }
    }

    /// How long X has been held, while recording.
    pub fn recording(&self) -> Option<Duration> {
        match &self.talk {
            Some(Talk::Recording { started, .. }) => Some(started.elapsed()),
            _ => None,
        }
    }

    /// The joined space, and whether its state is current: not while the connection is down, when the server may have moved on without us.
    pub fn joined_space(&self) -> Option<(&SpaceState, bool)> {
        let space = self.space.as_ref().filter(|_| self.mode == Mode::Space)?;
        Some((
            space,
            matches!(self.state, Some(ConnectionState::Connected { .. })),
        ))
    }

    /// What the swarm should show. `Unknown` and a lost connection are never shown as ready.
    pub fn activity(&self) -> SessionActivity {
        let Some(space) = self.space.as_ref().filter(|_| self.mode == Mode::Space) else {
            return SessionActivity::Unselected;
        };
        if !matches!(self.state, Some(ConnectionState::Connected { .. })) {
            return SessionActivity::Unavailable;
        }
        if self.decided.is_some() {
            return SessionActivity::Working;
        }
        if space.approval.is_some() || space.question.is_some() {
            return SessionActivity::Waiting;
        }
        match space.activity {
            Activity::Idle
                if space.queue.iter().any(|entry| {
                    matches!(entry.state, QueueState::Queued | QueueState::Sending)
                }) =>
            {
                SessionActivity::Working
            }
            Activity::Idle => SessionActivity::Ready,
            Activity::Running => SessionActivity::Working,
            Activity::Waiting => SessionActivity::Waiting,
            Activity::Closed => SessionActivity::Unavailable,
            Activity::Unknown => SessionActivity::Unknown,
        }
    }

    /// The panel text, at most [`ROWS`] lines of at most [`COLUMNS`] characters.
    #[cfg(any(not(target_os = "android"), test))]
    pub fn text(&mut self) -> String {
        let lines = match self.mode {
            Mode::Servers => self.servers_text(),
            Mode::Lobby => self.lobby_text(),
            Mode::Space => self.space_text(),
        };
        debug_assert!(lines.len() <= ROWS);
        lines.join("\n")
    }

    /// Whether the lobby shows an error or an ended space, which B dismisses before it returns to the server list.
    fn dismissable(&self) -> bool {
        self.error.is_some() || (self.ended.is_some() && self.status().is_none())
    }

    fn status(&self) -> Option<String> {
        if let Some(error) = &self.error {
            return Some(format!("ERROR: {error}"));
        }
        match &self.state {
            None | Some(ConnectionState::Connecting { attempt: 1 }) => {
                return Some("Connecting...".into());
            }
            Some(ConnectionState::Connecting { attempt }) => {
                return Some(format!("Connecting (attempt {attempt})..."));
            }
            Some(ConnectionState::Disconnected { reason, retry_in }) => {
                return Some(format!(
                    "Offline, retry in {}s: {reason}",
                    retry_in.as_secs_f32().ceil()
                ));
            }
            Some(ConnectionState::Closed) => return Some("Disconnected.".into()),
            // Connected, or in a state a later client knows of, which has nothing to say.
            Some(_) => {}
        }
        self.notice.clone()
    }

    /// The server list's status: an error, then a microphone request still unanswered, then that it is searching. See `design/arrival.md`.
    fn servers_status(&self) -> &str {
        match &self.error {
            Some(error) => error,
            None if self.microphone_waiting => MICROPHONE_REQUEST,
            None => "Searching the network...",
        }
    }

    #[cfg(any(not(target_os = "android"), test))]
    fn servers_text(&self) -> Vec<String> {
        let mut lines = vec![
            short(&format!(
                "SERVERS {}/{}",
                (self.server + 1).min(self.servers.len()),
                self.servers.len()
            )),
            "Stick up/down: choose. A: connect.".into(),
        ];
        let rows = ROWS - 4;
        let start = window(self.server, self.servers.len(), rows);
        for (index, choice) in self.servers.iter().enumerate().skip(start).take(rows) {
            let marker = if index == self.server { '>' } else { ' ' };
            lines.push(short(&match &choice.unavailable {
                // A row has no room for both, and what needs attention, then the counts, say more about a discovered server than its address.
                None => match choice.needs().or_else(|| choice.counts()) {
                    Some(counts) => format!("{marker} {} ({counts})", choice.label),
                    None => format!("{marker} {} {}", choice.label, choice.address),
                },
                Some(reason) => format!("{marker} ({} - {reason})", choice.label),
            }));
        }
        while lines.len() < ROWS - 2 {
            lines.push(String::new());
        }
        lines.push(short(self.servers_status()));
        lines.push("A connect | Y search again".into());
        lines
    }

    #[cfg(any(not(target_os = "android"), test))]
    fn lobby_text(&self) -> Vec<String> {
        match self.step {
            LobbyStep::Spaces => self.spaces_text(),
            step => self.menu_text(step),
        }
    }

    /// The lobby at its spaces list.
    #[cfg(any(not(target_os = "android"), test))]
    fn spaces_text(&self) -> Vec<String> {
        let server = self
            .connected
            .as_ref()
            .map_or("server", |choice| choice.label.as_str());
        let mut lines = vec![short(&format!(
            "{server} | SPACES {}/{}",
            (self.selected + 1).min(self.spaces.len()),
            self.spaces.len()
        ))];
        lines.push(short(
            &self
                .status()
                .or_else(|| self.ended.clone())
                .unwrap_or_else(|| {
                    if self.joining.is_some() {
                        "Joining...".into()
                    } else if self.role == ConnectionRole::Pairing {
                        "This server admits only paired devices: pair to join.".into()
                    } else if !self.listed {
                        "Listing spaces...".into()
                    } else if self.spaces.is_empty() && self.creates() {
                        "No spaces yet; create one.".into()
                    } else if self.spaces.is_empty() {
                        "No spaces yet.".into()
                    } else {
                        self.attention_status()
                            .unwrap_or_else(|| format!("You are {}.", self.name))
                    }
                }),
        ));
        let rows = ROWS - 3;
        // The spaces, then the create object.
        let entries = self
            .spaces
            .iter()
            .map(|space| {
                format!(
                    "{} [{}{}]",
                    space.title,
                    space
                        .attention
                        .map_or(activity_label(space.activity), arrival::attention_label),
                    match space.participants {
                        0 => String::new(),
                        count => format!(", {count} here"),
                    }
                )
            })
            .chain(self.creates().then(|| "+ New space".to_owned()));
        let start = window(self.selected, self.step_list(LobbyStep::Spaces), rows);
        for (index, entry) in entries.enumerate().skip(start).take(rows) {
            let marker = if index == self.selected { '>' } else { ' ' };
            lines.push(short(&format!("{marker} {entry}")));
        }
        while lines.len() < ROWS - 1 {
            lines.push(String::new());
        }
        lines.push(if self.dismissable() {
            "A choose | B dismiss | Y refresh".into()
        } else {
            "A choose | B servers | Y refresh".into()
        });
        lines
    }

    /// What the open creation ring needs said: what is missing when it is empty, since the headset cannot type, or what it is for.
    fn ring_hint(&self, step: LobbyStep) -> &'static str {
        match step {
            LobbyStep::Backend if self.backends.is_empty() => {
                "The server has no backends; ask on the PC."
            }
            LobbyStep::Directory if self.roots.is_empty() => {
                "The server offers no folders; ask on the PC."
            }
            LobbyStep::Backend => "The agent that runs in the space.",
            LobbyStep::Directory => "The folder the agent works in.",
            LobbyStep::Session if self.sessions.is_empty() => {
                "Start a new session; none listed yet."
            }
            LobbyStep::Session => "A new session, or attach to an existing one.",
            LobbyStep::Environment => "Where the space takes place, for everyone in it.",
            LobbyStep::Spaces => unreachable!("the spaces are not a ring"),
        }
    }

    /// The creation rings, as a list, at one of their steps.
    #[cfg(any(not(target_os = "android"), test))]
    fn menu_text(&self, step: LobbyStep) -> Vec<String> {
        let label = match step {
            LobbyStep::Backend => "NEW SPACE: BACKEND",
            LobbyStep::Directory => "NEW SPACE: FOLDER",
            LobbyStep::Session => "NEW SPACE: SESSION",
            LobbyStep::Environment => "NEW SPACE: ENVIRONMENT",
            LobbyStep::Spaces => unreachable!(),
        };
        let count = self.step_list(step);
        let mut lines = vec![short(&format!(
            "{label} {}/{}",
            (self.menu + 1).min(count),
            count
        ))];
        lines.push(short(
            &self
                .status()
                .unwrap_or_else(|| self.ring_hint(step).to_owned()),
        ));
        let rows = ROWS - 3;
        let start = window(self.menu, count, rows);
        for item in self.ring_items(step).into_iter().skip(start).take(rows) {
            let marker = if item.selected { '>' } else { ' ' };
            lines.push(short(&format!("{marker} {}", item.label)));
        }
        while lines.len() < ROWS - 1 {
            lines.push(String::new());
        }
        lines.push(if self.dismissable() {
            "A choose | B dismiss | Y refresh".into()
        } else {
            "A choose | B back | Y refresh".into()
        });
        lines
    }

    /// The choices on the ring at `step`, with the stick's selection marked.
    fn ring_items(&self, step: LobbyStep) -> Vec<arrival::Item> {
        let choice = |key, label: &str, detail: Option<&str>| arrival::Item {
            key,
            kind: arrival::Kind::Choice,
            label: label.to_owned(),
            detail: detail.map(str::to_owned),
            selected: false,
        };
        let mut items: Vec<_> = match step {
            LobbyStep::Spaces => Vec::new(),
            LobbyStep::Backend => self
                .backends
                .iter()
                .map(|backend| {
                    choice(
                        arrival::Key::Backend(backend.name.clone()),
                        &backend.name,
                        backend.attach.then_some("can attach"),
                    )
                })
                .collect(),
            LobbyStep::Directory => self
                .roots
                .iter()
                .map(|root| {
                    choice(
                        arrival::Key::Directory(root.path.clone()),
                        &root.label,
                        None,
                    )
                })
                .collect(),
            LobbyStep::Session => {
                std::iter::once(choice(arrival::Key::Session(None), "New session", None))
                    .chain(self.sessions.iter().map(|session| {
                        let title = if session.title.trim().is_empty() {
                            &session.id
                        } else {
                            &session.title
                        };
                        choice(arrival::Key::Session(Some(session.id.clone())), title, None)
                    }))
                    .collect()
            }
            LobbyStep::Environment => self
                .environments
                .iter()
                .map(|offered| {
                    choice(
                        arrival::Key::Environment(offered.id.clone()),
                        &offered.label,
                        None,
                    )
                })
                .collect(),
        };
        if let Some(item) = items.get_mut(self.menu) {
            item.selected = true;
        }
        items
    }

    /// What the headset shows as objects in the world, outside a space. See `design/arrival.md`.
    pub fn arrival(&self) -> Option<arrival::View> {
        match self.mode {
            Mode::Space => None,
            Mode::Servers => Some(arrival::View {
                heading: "Choose a server".into(),
                status: Some(self.servers_status().into()),
                connected: None,
                items: self
                    .servers
                    .iter()
                    .enumerate()
                    .flat_map(|(index, choice)| {
                        let attention = choice.attention();
                        let server = arrival::Item {
                            key: arrival::Key::Server(choice.address.clone()),
                            kind: arrival::Kind::Server {
                                source: choice.source,
                                available: choice.is_available(),
                                attention,
                            },
                            label: choice.label.clone(),
                            detail: Some(choice.detail()),
                            selected: index == self.server,
                        };
                        // A server whose spaces need attention has a jump object beside it.
                        let jump = attention.map(|attention| arrival::Item {
                            key: arrival::Key::Jump(choice.address.clone()),
                            kind: arrival::Kind::Jump { attention },
                            label: "Jump".into(),
                            detail: Some(format!("join what needs you on {}", choice.label)),
                            selected: false,
                        });
                        std::iter::once(server).chain(jump)
                    })
                    .collect(),
                ring: None,
                joining: None,
            }),
            Mode::Lobby => {
                let server = self
                    .connected
                    .as_ref()
                    .map_or("server", |choice| choice.label.as_str());
                let ring = self.step.ring();
                let spaces = self.step == LobbyStep::Spaces;
                let status = self.status().or_else(|| self.ended.clone()).or_else(|| {
                    if self.joining.is_some() {
                        Some("Joining...".into())
                    } else if self.role == ConnectionRole::Pairing {
                        Some("This server admits only paired devices: pair to join.".into())
                    } else if !self.listed {
                        Some("Listing spaces...".into())
                    } else if !spaces {
                        Some(self.ring_hint(self.step).into())
                    } else if self.spaces.is_empty() && self.creates() {
                        Some("No spaces yet; create one.".into())
                    } else if self.spaces.is_empty() {
                        Some("No spaces yet.".into())
                    } else {
                        self.attention_status()
                    }
                });
                let items = self
                    .spaces
                    .iter()
                    .enumerate()
                    .map(|(index, space)| arrival::Item {
                        key: arrival::Key::Space(space.id.clone()),
                        kind: arrival::Kind::Space {
                            activity: space.activity,
                            participants: space.participants,
                            hue: arrival::hue(&space.id),
                            attention: space.attention,
                        },
                        label: space.title.clone(),
                        detail: Some(
                            space
                                .attention
                                .map_or(activity_label(space.activity), arrival::attention_label)
                                .into(),
                        ),
                        selected: spaces && index == self.selected,
                    })
                    .chain(self.creates().then(|| arrival::Item {
                        key: arrival::Key::Create,
                        kind: arrival::Kind::Create { open: !spaces },
                        label: if spaces { "New space" } else { "Back" }.into(),
                        detail: None,
                        selected: spaces && self.selected >= self.spaces.len(),
                    }))
                    .chain(self.pairable().then(|| self.pairing_item()))
                    .collect();
                Some(arrival::View {
                    heading: match ring {
                        Some(step) => step.title().into(),
                        None => format!("Spaces on {server}"),
                    },
                    status,
                    connected: self.connected.as_ref().map(|choice| arrival::Item {
                        key: arrival::Key::Connected(choice.address.clone()),
                        kind: arrival::Kind::Connected,
                        label: choice.label.clone(),
                        detail: Some("Choose to change".into()),
                        selected: false,
                    }),
                    items,
                    ring: ring.map(|step| arrival::Ring {
                        step,
                        items: self.ring_items(self.step),
                    }),
                    joining: self
                        .joining
                        .as_ref()
                        .map(|space| arrival::Key::Space(space.clone())),
                })
            }
        }
    }

    /// The space joined, as the arrival object it was chosen from, so that its environment grows out of that object.
    pub fn entered(&self) -> Option<arrival::Key> {
        self.space
            .as_ref()
            .filter(|_| self.mode == Mode::Space)
            .map(|space| arrival::Key::Space(space.id.clone()))
    }

    /// Choose an arrival object, as by pointing at it and pulling the trigger.
    pub fn choose_object(&mut self, key: &arrival::Key) {
        match (self.mode, key) {
            // Connect, then join the space that most needs attention once the server lists them.
            (Mode::Servers, arrival::Key::Jump(address)) => {
                if let Some(index) = self
                    .servers
                    .iter()
                    .position(|choice| &choice.address == address && choice.is_available())
                {
                    self.server = index;
                    self.chosen = true;
                    let choice = self.servers[index].clone();
                    let label = choice.label.clone();
                    self.connect(choice);
                    self.jumping = true;
                    self.notice = Some(format!("Jumping to what needs you on {label}..."));
                }
            }
            (Mode::Servers, arrival::Key::Server(address)) => {
                if let Some(index) = self
                    .servers
                    .iter()
                    .position(|choice| &choice.address == address)
                {
                    self.server = index;
                    self.chosen = true;
                    self.missing = None;
                    self.accept();
                }
            }
            (Mode::Lobby, arrival::Key::Connected(_)) => self.leave_server(),
            (Mode::Lobby, arrival::Key::Space(id)) => {
                if let Some(index) = self.spaces.iter().position(|space| &space.id == id) {
                    self.close_menu();
                    self.selected = index;
                    self.accept();
                }
            }
            (Mode::Lobby, arrival::Key::Pair) => self.pair(),
            (Mode::Lobby, arrival::Key::Create) if !self.creates() => {}
            (Mode::Lobby, arrival::Key::Create) => {
                self.error = None;
                if self.step == LobbyStep::Spaces {
                    self.selected = self.spaces.len();
                    self.accept();
                } else {
                    self.back_ring();
                }
            }
            (Mode::Lobby, key) => {
                let position = self
                    .ring_items(self.step)
                    .iter()
                    .position(|item| &item.key == key);
                if let Some(index) = position {
                    self.error = None;
                    self.menu = index;
                    self.accept();
                }
            }
            _ => {}
        }
    }

    /// The space's list on `rows` rows: a window of its entries that keeps the selected one in view, then the selected entry's description, so that both always show.
    #[cfg(any(not(target_os = "android"), test))]
    fn menu_body(&self, selected: usize, rows: usize) -> (String, String) {
        let items = self.menu_items();
        let current = self.space.as_ref().and_then(|space| space.mode.as_ref());
        let description = match items.get(selected) {
            Some(MenuItem::Mode(mode)) => mode
                .description
                .clone()
                .unwrap_or_else(|| format!("Switch the agent to {}.", mode.name)),
            Some(MenuItem::Speech(SpeechSetting::Brief)) => {
                "Replies speak their first paragraphs, then the rest is on the panel. A switches everyone to full replies.".into()
            }
            Some(MenuItem::Speech(_)) => {
                "Replies are spoken in full. A switches everyone to brief replies.".into()
            }
            None => String::new(),
        };
        // The description gets the rows the entries leave, but at most half, and the entries at least one.
        let room = rows.saturating_sub(items.len().min(rows) + 1);
        let room = room.max(rows.saturating_sub(1) / 2);
        // The body has at least four rows, so the description at least one.
        let description = crate::ui_text::lines(&description, COLUMNS, room);
        let shown = rows
            .saturating_sub(description.len() + usize::from(!description.is_empty()))
            .max(1);
        let first = window(selected, items.len(), shown);
        let mut lines: Vec<String> = items
            .iter()
            .enumerate()
            .skip(first)
            .take(shown)
            .map(|(index, item)| {
                let marker = if index == selected { ">" } else { " " };
                let now = match item {
                    MenuItem::Mode(mode) if Some(&mode.id) == current => " (now)",
                    _ => "",
                };
                short(&format!("{marker} {}{now}", menu_label(item)))
            })
            .collect();
        if !description.is_empty() && lines.len() < rows {
            lines.push(String::new());
            lines.extend(description);
        }
        lines.truncate(rows);
        (
            format!("MODE AND SPEECH {}/{}", selected + 1, items.len()),
            lines.join("\n"),
        )
    }

    /// The label and text of what the body shows when the space's list is closed: the pending request, or the selected message.
    fn body(&self) -> (String, String) {
        let Some(space) = &self.space else {
            return ("Loading".into(), String::new());
        };
        if let Some(approval) = &space.approval {
            return (
                format!("APPROVAL: {}", approval.title),
                approval.text.clone(),
            );
        }
        if let Some(question) = &space.question {
            let mut text = question.text.clone();
            if self.answerable().is_none() {
                text.push_str("\n\nAnswer this question on the PC.");
            }
            return (format!("QUESTION: {}", question.title), text);
        }
        if let Some(message) = space.messages.get(self.message) {
            let role = match message.role {
                Role::User => "USER",
                Role::Assistant => "ASSISTANT",
            };
            return (
                format!("{role} {}/{}", self.message + 1, space.messages.len()),
                message.text.clone(),
            );
        }
        ("No messages yet".into(), String::new())
    }

    /// The problem to show: this participant's newest error, else the space's ongoing problem unless dismissed here.
    pub(crate) fn problem(&self) -> Option<String> {
        self.error.clone().or_else(|| {
            self.space
                .as_ref()
                .and_then(|space| space.error.clone())
                .filter(|error| self.dismissed.as_ref() != Some(error))
        })
    }

    /// The problem line: the problem and how long it has been shown, on up to [`PROBLEM_ROWS`] rows. Neither the status nor anything else displaces it.
    fn problem_lines(&self, now: Instant) -> Vec<String> {
        let Some(problem) = self.problem() else {
            return Vec::new();
        };
        crate::ui_text::lines(
            &format!(
                "! {} {}",
                age(self.problem_age(&problem, now)),
                problem.replace('\n', " ")
            ),
            COLUMNS,
            PROBLEM_ROWS,
        )
    }

    /// The queue strip: entries waiting or being sent, oldest first, on up to [`QUEUE_ROWS`] rows, keeping the selected entry in view.
    fn queue_lines(&self) -> Vec<String> {
        let Some(space) = &self.space else {
            return Vec::new();
        };
        let entries: Vec<&tomori_protocol::QueueEntry> = space
            .queue
            .iter()
            .filter(|entry| matches!(entry.state, QueueState::Queued | QueueState::Sending))
            .collect();
        let line = |entry: &tomori_protocol::QueueEntry| {
            let cancelling = self
                .cancelling
                .as_ref()
                .is_some_and(|(id, _)| *id == entry.id);
            let selected = self.queue_selected.as_ref() == Some(&entry.id);
            let (marker, state) = match (cancelling, selected) {
                (true, _) => ("x", "cancelling, A undoes"),
                (false, true) => (">", queue_label(entry.state)),
                (false, false) => ("-", queue_label(entry.state)),
            };
            // Entries are matched by participant number, since display names need not be unique.
            let speaker = if Some(entry.source) == self.source {
                "You"
            } else {
                entry.speaker.as_str()
            };
            short(&format!("{marker} {speaker} {state}: {}", entry.text))
        };
        if entries.len() <= QUEUE_ROWS {
            return entries.into_iter().map(line).collect();
        }
        let selected = entries
            .iter()
            .position(|entry| {
                self.queue_selected.as_ref() == Some(&entry.id)
                    || self
                        .cancelling
                        .as_ref()
                        .is_some_and(|(id, _)| *id == entry.id)
            })
            .unwrap_or(0);
        let shown = &entries[window(selected, entries.len(), QUEUE_ROWS - 1)..][..QUEUE_ROWS - 1];
        let mut lines: Vec<String> = shown.iter().map(|entry| line(entry)).collect();
        lines.push(format!("  {} in the queue; R-stick selects", entries.len()));
        lines
    }

    /// The status line in a space: what the microphone is doing, the connection, a notice, a replay, or the counts. Problems have their own line.
    #[cfg(any(not(target_os = "android"), test))]
    fn space_status(&self, space: &SpaceState) -> String {
        let talking = match &self.talk {
            // Until words are heard, the line says which worker or card they will be about.
            Some(Talk::Recording {
                heard,
                answering: false,
                naming: false,
                pointed: Some(pointed),
                ..
            }) if heard.is_empty() && self.knows(pointed) => Some(short(&format!(
                "MIC AGENT about {}",
                self.about(pointed).unwrap_or_default()
            ))),
            Some(
                talk @ Talk::Recording {
                    heard, answering, ..
                },
            ) => {
                let label = if talk.naming() {
                    "MIC NAME"
                } else if *answering {
                    "MIC ANSWER"
                } else {
                    "MIC AGENT"
                };
                Some(if heard.is_empty() {
                    format!("{label}: listening...")
                } else {
                    format!("{label}: {}", tail(heard, COLUMNS - 12))
                })
            }
            Some(Talk::Sending {
                heard, received, ..
            }) => {
                let label = match received {
                    None => "Sending",
                    Some(received)
                        if received.stage == tomori_protocol::TranscriptStage::Waiting =>
                    {
                        "Waiting"
                    }
                    Some(_) => "Transcribing",
                };
                Some(format!(
                    "{label}: {}...",
                    tail(heard, COLUMNS - 5 - label.len())
                ))
            }
            None => None,
        };
        let connection = match &self.state {
            None | Some(ConnectionState::Connecting { attempt: 1 }) => Some("Connecting...".into()),
            Some(ConnectionState::Connecting { attempt }) => {
                Some(format!("Connecting (attempt {attempt})..."))
            }
            Some(ConnectionState::Disconnected { reason, retry_in }) => Some(format!(
                "Offline, retry in {}s: {reason}",
                retry_in.as_secs_f32().ceil()
            )),
            Some(ConnectionState::Closed) => Some("Disconnected.".into()),
            // Connected, or in a state a later client knows of, which has nothing to say.
            Some(_) => None,
        };
        let leaving = self
            .leave_hold
            .filter(|(held, left)| !left && *held >= Duration::from_millis(150))
            .map(|_| "Leaving: keep holding B...".to_owned());
        let status = talking
            .or(leaving)
            .or(connection)
            .or_else(|| self.notice.clone())
            .unwrap_or_else(|| {
                if self.replaying {
                    "Replaying... R-stick click stops".into()
                } else if let Some(notice) = self.environment_notice(space) {
                    // The grid is shown instead, never another environment.
                    notice
                } else {
                    let mut parts = Vec::new();
                    if !self.live {
                        parts.push("Mic off".to_owned());
                    }
                    if self.muted {
                        parts.push("Muted".to_owned());
                    }
                    if space.speech == SpeechSetting::Brief {
                        parts.push("Brief".to_owned());
                    }
                    parts.push(here(space));
                    parts.join(" | ")
                }
            });
        if self.live && !matches!(self.talk, Some(Talk::Recording { .. })) {
            format!("MIC LIVE | {status}")
        } else {
            status
        }
    }

    /// Note when the problem shown first appeared, so that its age can be shown.
    fn observe_problem(&mut self, now: Instant) {
        match self.problem() {
            None => self.problem_seen = None,
            Some(problem) => {
                if self
                    .problem_seen
                    .as_ref()
                    .is_none_or(|(seen, _)| *seen != problem)
                {
                    self.problem_seen = Some((problem, now));
                }
            }
        }
    }

    /// The queue strip's lines, unless an approval's choices need the room.
    fn strip_lines(&self) -> Vec<String> {
        let choosing = self
            .space
            .as_ref()
            .and_then(|space| space.approval.as_ref())
            .is_some_and(|_| self.decided.is_none());
        if choosing {
            Vec::new()
        } else {
            self.queue_lines()
        }
    }

    /// The rows the body has, after the problem line, the queue strip and an approval's choice; paging, following and drawing all use it.
    fn body_rows(&self) -> usize {
        let problem = self.problem_lines(Instant::now()).len();
        let queue = self.strip_lines().len();
        let choice = self
            .choice_lines(BODY_ROWS - problem - queue - 1)
            .map_or(0, |lines| lines.len());
        BODY_ROWS - problem - queue - choice
    }

    /// The approval's chosen choice, on at most `budget` rows.
    fn choice_lines(&self, budget: usize) -> Option<Vec<String>> {
        self.space
            .as_ref()
            .and_then(|space| space.approval.as_ref())
            .filter(|_| {
                self.decided.is_none() && self.space_menu.is_none() && !self.decide_on_wrist
            })
            .map(|approval| {
                let option = match self.option.checked_sub(1) {
                    None => "Allow once",
                    Some(index) if index < approval.options.len() => {
                        approval.options[index].label.as_str()
                    }
                    Some(_) => "Deny",
                };
                let mut lines = wrapped_lines(&format!(
                    "{}/{}: {option}",
                    self.option + 1,
                    approval.options.len() + 2
                ));
                if lines.len() > budget {
                    lines.truncate(budget);
                    // The queue strip gives way to a choice, which so has the body's rows but the problem's and one.
                    let last = lines.last_mut().expect("a choice keeps a row");
                    *last = short(&format!("{last} more"));
                }
                lines
            })
    }

    #[cfg(any(not(target_os = "android"), test))]
    fn space_text(&mut self) -> Vec<String> {
        let now = Instant::now();
        self.observe_problem(now);
        let problem = self.problem_lines(now);
        let queue = self.strip_lines();
        // The chosen choice is shown in full where it fits, since a standing option's label is the whole of what it grants.
        let choice = self.choice_lines(BODY_ROWS - problem.len() - queue.len() - 1);
        let rows = BODY_ROWS - problem.len() - queue.len() - choice.as_ref().map_or(0, Vec::len);
        let (label, body) = match self.space_menu {
            Some(selected) => self.menu_body(selected, rows),
            None => self.body(),
        };
        let pages = match &self.paged {
            Some(paged) if paged.text == body && paged.rows == rows => paged.pages.clone(),
            _ => {
                let pages: Arc<[Vec<String>]> = paginate(&body, rows).into();
                self.paged = Some(Paged {
                    text: body,
                    rows,
                    pages: pages.clone(),
                });
                pages
            }
        };
        // The list fits its rows, and leaves the reader's page as it was for when it closes.
        let page = if self.space_menu.is_some() {
            0
        } else {
            // Following, the newest page is shown at the rows there are now, which a problem or the strip can change without the space changing.
            if self.follow && self.request.is_none() {
                self.page = pages.len() - 1;
            }
            self.page = self.page.min(pages.len() - 1);
            self.page
        };
        let Some(space) = &self.space else {
            return vec![
                "SPACE".into(),
                "Loading...".into(),
                String::new(),
                "Hold B to leave".into(),
            ];
        };
        let status = self.space_status(space);
        // The current mode is shown by the name the agent's interface gives it, even when it is not one Tomori can step to.
        let mode = space
            .current_mode()
            .map(|mode| mode.name.as_str())
            .or(space.mode.as_deref().filter(|_| !space.modes.is_empty()));
        let mut lines = vec![
            short(&match mode {
                Some(mode) => format!(
                    "[{} | {mode}] {}",
                    activity_label(space.activity),
                    space.title
                ),
                None => format!("[{}] {}", activity_label(space.activity), space.title),
            }),
            short(&status),
        ];
        lines.extend(problem.iter().cloned());
        lines.push(short(&format!("p{}/{} {label}", page + 1, pages.len())));
        lines.extend(pages[page].iter().cloned());
        let footer = if matches!(self.talk, Some(Talk::Recording { .. })) {
            "Release X to send"
        } else if self.space_menu.is_some() {
            "Stick choose | A select | B close"
        } else if self.decide_on_wrist && (space.approval.is_some() || space.question.is_some()) {
            "Turn your right wrist to decide"
        } else if let Some(choice) = choice {
            while lines.len() + choice.len() + queue.len() < ROWS - 1 {
                lines.push(String::new());
            }
            lines.extend(choice);
            "R-stick choose | A confirm"
        } else if space.approval.is_some() {
            "Sending decision..."
        } else if self.naming && self.renames() {
            if matches!(self.outcome, Some((Outcome::NameFailed(_), _))) && !problem.is_empty() {
                "Hold X to try again | B cancel"
            } else {
                "Hold X to say your name | B cancel"
            }
        } else if self.answerable().is_some() {
            "Hold X to answer | hold B leave"
        } else if self.cancelling.is_some() {
            "A undo cancel"
        } else if self.queue_selected.is_some() {
            "A cancel entry | B deselect"
        } else if !problem.is_empty() {
            "B dismiss | X talk | hold B leave"
        } else if self.menu_items().is_empty() {
            "X talk | Y refresh | hold B leave"
        } else {
            "X talk | Y modes | hold B leave"
        };
        while lines.len() + queue.len() < ROWS - 1 {
            lines.push(String::new());
        }
        lines.extend(queue);
        lines.push(footer.into());
        lines
    }
}

/// What an entry of the space's list is called.
#[cfg(any(not(target_os = "android"), test))]
fn menu_label(item: &MenuItem) -> String {
    match item {
        MenuItem::Mode(mode) => format!("Mode: {}", mode.name),
        MenuItem::Speech(SpeechSetting::Brief) => "Speech: brief".into(),
        MenuItem::Speech(_) => "Speech: full".into(),
    }
}

/// How long ago something happened, briefly.
fn age(elapsed: Duration) -> String {
    let seconds = elapsed.as_secs();
    match seconds {
        0..60 => format!("{seconds}s"),
        60..3600 => format!("{}m", seconds / 60),
        _ => format!("{}h", seconds / 3600),
    }
}

/// A space chosen on the creation rings, not yet bound to an environment.
#[derive(Debug)]
struct Creation {
    title: String,
    backend: String,
    directory: String,
    session: Option<String>,
    instructions: Option<String>,
}

impl Creation {
    /// The `CreateSpace` for this space in `environment`, or in the server's own when `None`.
    fn message(self, environment: Option<String>) -> ClientMessage {
        ClientMessage::CreateSpace {
            title: self.title,
            backend: self.backend,
            directory: self.directory,
            session: self.session,
            environment,
            instructions: self.instructions,
        }
    }
}

/// An entry of the space's list, opened with Y.
#[derive(Clone, Debug, PartialEq)]
enum MenuItem {
    Mode(tomori_protocol::Mode),
    Speech(SpeechSetting),
}

/// The messages, each of which the stick can select.
fn items(space: &SpaceState) -> usize {
    space.messages.len()
}

/// How many participants are in the space, and how many of them are away.
fn here(space: &SpaceState) -> String {
    let away = space
        .participants
        .iter()
        .filter(|participant| participant.engagement == Some(Engagement::Away))
        .count();
    match away {
        0 => format!("{} here", space.participants.len()),
        away => format!("{} here, {away} away", space.participants.len() - away),
    }
}

fn activity_label(activity: Activity) -> &'static str {
    match activity {
        Activity::Idle => "idle",
        Activity::Running => "running",
        Activity::Waiting => "waiting",
        Activity::Closed => "closed",
        Activity::Unknown => "unknown",
    }
}

fn queue_label(state: QueueState) -> &'static str {
    match state {
        QueueState::Queued => "queued",
        QueueState::Sending => "sending",
        QueueState::Delivered => "sent",
        QueueState::Failed => "failed",
    }
}

/// The first of `rows` visible entries that keeps `selected` in view.
fn window(selected: usize, count: usize, rows: usize) -> usize {
    selected
        .saturating_sub(rows - 1)
        .min(count.saturating_sub(rows))
}

/// The end of `text` within `width` columns, so that words still being heard stay in view.
#[cfg(any(not(target_os = "android"), test))]
fn tail(text: &str, width: usize) -> String {
    let line = crate::ui_text::clip(text, usize::MAX);
    if crate::ui_text::width(&line) <= width {
        return line;
    }
    // As many characters from the end as fit beside the "…", counted in cells as drawn.
    let mut kept = Vec::new();
    let mut used = 1;
    for ch in line.chars().rev() {
        used += crate::ui_text::width(ch.encode_utf8(&mut [0; 4]));
        if used > width {
            break;
        }
        kept.push(ch);
    }
    std::iter::once('…').chain(kept.into_iter().rev()).collect()
}

/// `text` on one row of the panel, as [`crate::ui_text::clip`] cuts it.
fn short(text: &str) -> String {
    crate::ui_text::clip(text, COLUMNS)
}

fn shift(index: usize, delta: i32, count: usize) -> usize {
    index
        .saturating_add_signed(delta as isize)
        .min(count.saturating_sub(1))
}

/// `text` paged at the body's full height, for tests.
#[cfg(test)]
fn pages(text: &str) -> Vec<Vec<String>> {
    paginate(text, BODY_ROWS)
}

fn paginate(text: &str, rows: usize) -> Vec<Vec<String>> {
    let lines = wrapped_lines(text);
    if lines.is_empty() {
        vec![vec![String::new()]]
    } else {
        lines.chunks(rows).map(|page| page.to_vec()).collect()
    }
}

/// The conversation is the session its connection carries out: see `design/clients.md`.
impl Session for Conversation {
    fn next_action(&mut self) -> Option<Action> {
        Conversation::next_action(self)
    }

    fn apply(&mut self, event: Event) {
        Conversation::apply(self, event);
    }

    fn environments(&self) -> Option<Vec<tomori_protocol::EnvironmentVersion>> {
        Some(self.presented())
    }

    fn report(&mut self, report: Report) {
        // Reported every frame, and needing nothing more of the match below.
        if let Report::Replaying(replaying) = report {
            self.set_replaying(replaying);
        }
        match report {
            Report::Connecting(choice) => self.connect(choice),
            Report::Feedback(settings) => self.set_feedback(settings),
            Report::Unfocused => {
                self.stop_live();
                self.stop_talking();
            }
            Report::Servers(servers) => self.set_servers(servers),
            Report::Tick { now, browsing } => {
                if browsing {
                    self.tick(now);
                }
                self.expire_pairing(now);
            }
            Report::Generation(generation) => self.set_capture_generation(generation),
            Report::Failed(error) => self.fail(error),
            Report::MicrophoneFailed(error) => self.microphone_failed(error),
            Report::NotSent(error) => self.utterance_not_sent(error),
            Report::SecurityNotice(notice) => self.security_notice(notice),
            // Replaying, applied above, and anything a later connection reports that this conversation does not know, which needs nothing of it.
            _ => {}
        }
    }

    fn is_browsing(&self) -> bool {
        Conversation::is_browsing(self)
    }

    /// The approval and question waiting in the joined space, the knocks on it, and the recording, nearing the server's limit. Visitors cannot decide requests or let anyone in, so nothing waits for them.
    fn observe(&self, feedback: &mut feedback::Feedback, now: Instant) {
        feedback.observe_recording(self.recording(), self.utterance_limit(), now);
        let space = self
            .joined_space()
            .map(|(space, _)| space)
            .filter(|_| self.role().participates());
        let requests = space.into_iter().flat_map(|space| {
            let approval = space.approval.as_ref().map(|approval| approval.id.as_str());
            let question = space.question.as_ref().map(|question| question.id.as_str());
            approval.into_iter().chain(question)
        });
        feedback.observe_requests(requests, now);
        let knocks = space
            .into_iter()
            .flat_map(|_| self.knocks().iter().map(|knock| knock.id.as_str()));
        feedback.observe_knocks(knocks, now);
    }
}

#[cfg(test)]
mod tests {
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
            },
            BackendInfo {
                name: "plain".into(),
                attach: false,
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
        })
    }

    /// A conversation connected to a server and joined to [`SPACE`], with any request it shows there long enough to be chosen.
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
            environment: "sky".into(),
        }
    }

    /// A space listed as needing `attention` for `age` seconds.
    fn needing(
        id: &str,
        title: &str,
        attention: tomori_protocol::Attention,
        age: u32,
    ) -> SpaceSummary {
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
        conversation.apply(Event::Message(ServerMessage::Left));
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
            .map(|item| item.detail.clone().unwrap())
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
                },
                tomori_protocol::SessionSummary {
                    id: "ses_2".into(),
                    title: "Other work".into(),
                    directory: "/work/tomori".into(),
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
        conversation.apply(Event::Message(ServerMessage::Left));
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
        let status =
            |conversation: &Conversation| conversation.arrival().and_then(|view| view.status);
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
        conversation.apply(Event::Message(ServerMessage::Left));
        assert_eq!(conversation.environment(), Environment::MINIMAL);
        // Returning to the servers keeps the grid.
        conversation.apply(space(|_| {}));
        assert_eq!(conversation.environment(), crate::test_support::SKY);
        conversation.apply(Event::Message(ServerMessage::Left));
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
        assert_eq!(conversation.locomotion(), &Locomotion::None);
        conversation.set_locomotion(vec![(OBSERVE, teleport.clone())]);
        // Shown as the grid, it has none.
        assert_eq!(conversation.locomotion(), &Locomotion::None);
        assert_eq!(conversation.menu_model().travel, None);
        conversation.set_drawable(vec![Environment::MINIMAL, OBSERVE]);
        assert_eq!(conversation.locomotion(), &teleport);
        // This headset can turn travel off, staying room scale, and on again, from the hand menu.
        assert_eq!(conversation.menu_model().travel, Some(true));
        conversation.choose_menu(crate::hand_menu::Choice::Travel);
        assert_eq!(conversation.locomotion(), &Locomotion::None);
        assert_eq!(conversation.menu_model().travel, Some(false));
        conversation.choose_menu(crate::hand_menu::Choice::Travel);
        assert_eq!(conversation.locomotion(), &teleport);
        // Sky, which declares none, and the lobby have none.
        conversation.apply(space(|state| state.environment = "sky".into()));
        assert_eq!(conversation.locomotion(), &Locomotion::None);
        conversation.apply(space(|state| state.environment = "observe".into()));
        assert_eq!(conversation.locomotion(), &teleport);
        // At a version this build does not have, the space is shown as the grid, which declares none.
        conversation.apply(space(|state| {
            state.environment = "observe".into();
            state.environment_version = Some(2);
        }));
        assert_eq!(conversation.locomotion(), &Locomotion::None);
        conversation.apply(space(|state| state.environment = "observe".into()));
        assert_eq!(conversation.locomotion(), &teleport);
        hold_b(&mut conversation);
        assert_eq!(conversation.locomotion(), &Locomotion::None);
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
        let mut conversation =
            joined(|state| state.approval = Some(approval(&[("always", label)])));
        conversation.choose(1);
        let text = conversation.text();
        assert_eq!(text.lines().count(), ROWS);
        let shown = text.split_whitespace().collect::<Vec<_>>().join(" ");
        assert!(shown.contains(&format!("2/3: {label}")), "{text}");
        // Even with a problem shown above it.
        conversation.apply(Event::Message(ServerMessage::Error {
            error:
                "The mode was not changed: the agent refused it, for a reason long enough to wrap."
                    .into(),
            space: None,
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
            let Some(Phase::NameFailed { .. }) = conversation.recording_phase(Instant::now())
            else {
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
        conversation.apply(Event::Message(ServerMessage::Left));
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
        conversation.apply(Event::Message(ServerMessage::Left));
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
        conversation.apply(Event::Message(ServerMessage::Left));
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
        conversation.apply(Event::Message(ServerMessage::Left));
        // Leaving is followed by a listing, so none is requested.
        assert!(conversation.take_actions().is_empty());
        conversation.apply(space(|_| {}));
        assert_eq!(conversation.mode(), Mode::Space);
    }

    #[test]
    fn being_left_by_the_server_returns_to_the_lobby_and_lists() {
        let mut conversation = joined(|_| {});
        conversation.apply(Event::Message(ServerMessage::Left));
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
        conversation.apply(Event::Message(ServerMessage::Left));
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
        };
        conversation.set_feedback(kept);
        assert_eq!(conversation.menu_model().feedback, kept);
        assert!(conversation.take_actions().is_empty());
        conversation.choose_menu(crate::hand_menu::Choice::Sounds);
        conversation.choose_menu(crate::hand_menu::Choice::Vibration);
        let full = Settings {
            sounds: Level::Full,
            vibration: Level::Reduced,
            mono: false,
        };
        let stepped = Settings {
            sounds: Level::Full,
            vibration: Level::Off,
            mono: false,
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
        let Some(Action::BeginUtterance(Utterance { generation: 8, .. })) =
            conversation.next_action()
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
    fn the_next_words_replace_words_that_were_not_sent_on_the_card_the_problem_line_and_the_wrist()
    {
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
            conversation.apply(Event::Message(ServerMessage::Left));
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
            conversation.apply(Event::Message(ServerMessage::Left));
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
            conversation.apply(Event::Message(ServerMessage::Left));
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
        conversation.apply(Event::Message(ServerMessage::Left));
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
}
