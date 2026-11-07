// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

//! A client's connection to a server, kept for as long as the client runs: it carries out what its [`Session`] asks for and reports back what happens. It owns the client, the microphone and the speaker, keeps each server's certificate and token, and browses for servers while the server list shows. See `design/clients.md`.
//!
//! The session is the client's conversation with the server, as `tomori-scene`'s `Conversation` is: state that asks for [`Action`]s and applies [`Event`]s and [`Report`]s, so that nothing in it touches the network or a device. The application calls [`Connection::poll`] once a frame.
use crate::{
    Client, ConnectionState, Disconnect, Event, Fingerprint, Microphone, Options, Transport,
    capture::{Capture, Recording},
    discovery,
    feedback::{self, Event as Feedback, Hand},
    identity::{Identity, Store},
    playback::{MixerOptions, Player, PlayerControl, spatial::Scene},
    server_choice::{self, Choice, Source},
    speaker::{Playback, Route, Shared, Speaking},
};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex, PoisonError},
    time::{Duration, Instant},
};
use tomori_protocol::{ClientMessage, Gate, Pose, ServerMessage};

/// What a [`Session`] asks its [`Connection`] to do, in order. The connection carries out every action in every state: one that needs a connection it does not have fails as [`Report`]s say, or, where nothing failed that the participant should see, is ignored and logged.
///
/// More actions may be added; a session that matches on them, as a test of one does, needs a wildcard arm.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub enum Action {
    /// Connect to this server, closing any current connection.
    Connect(Choice),
    /// Close the connection.
    Disconnect,
    /// Forget the servers found so far and browse again.
    Rescan,
    /// Send this message to the server. A session never asks for the messages the client sends itself, such as `Hello`. Without a connection, it fails with [`Report::Failed`].
    Send(ClientMessage),
    /// Open the microphone and send what it hears to the agent as this utterance. Without a connection, or for a connection generation that has ended, it fails with [`Report::MicrophoneFailed`].
    BeginUtterance(Utterance),
    /// Close the microphone and finish the utterance, for the server to transcribe.
    EndUtterance,
    /// Close the microphone and abandon the utterance.
    CancelUtterance,
    /// Close the microphone and forget the utterance, which the server has already ended.
    AbandonUtterance,
    /// Open the microphone for other participants, without addressing the agent, for the connection generation given (see [`Report::Generation`]). Without a connection, it fails with [`Report::MicrophoneFailed`].
    StartLive(u64),
    /// Close the live microphone.
    StopLive,
    /// Pairing gave this device an owner `token` for the server, as [`ServerMessage::Paired`] does: keep it with the certificate this connection presented, and connect with it. Without a connection over TLS, such as once the participant has left the server, the token is ignored. See `design/security.md`.
    Paired {
        /// The owner token the server issued.
        token: String,
    },
    /// A knock admitted this device to its space as a collaborator, as [`ServerMessage::Admitted`] does: keep the `token` for the server, with the certificate this connection presented. The connection is already a collaborator's. Without a connection over TLS, or in place of a token already kept, it is ignored.
    Admitted {
        /// The collaborator token the server issued.
        token: String,
    },
    /// Mute or unmute agent speech locally, live or played again.
    SetAgentMuted(bool),
    /// Change and keep this device's Feedback setting. See `design/feedback.md`.
    SetFeedback(feedback::Settings),
}

/// An utterance for [`Action::BeginUtterance`] to record: what the microphone hears until [`Action::EndUtterance`], sent to the agent, or as the answer to the presented question's field. Made with [`Utterance::new`], so that what an utterance may say can grow.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub struct Utterance {
    /// The utterance's identifier, unique on the connection, which the server's transcript of it names.
    pub id: String,
    /// The connection generation it is recorded for, as [`Report::Generation`] gave.
    pub generation: u64,
    /// The presented question's field it answers, if it answers one, rather than being queued for the agent.
    pub answer: Option<tomori_protocol::AnswerTarget>,
    /// Whether the words say the participant's new display name, which the server takes as a rename rather than queueing for the agent; see `design/security.md`.
    pub naming: bool,
    /// What the participant attends to as they begin speaking, such as the worker they point at, by identity only,
    /// which the server describes to the agent with the words. See `design/spaces.md`.
    pub attention: Vec<tomori_protocol::Resource>,
}

impl Utterance {
    /// The utterance `id`, for the agent, recorded for the connection `generation`.
    pub fn new(id: impl Into<String>, generation: u64) -> Self {
        Self {
            id: id.into(),
            generation,
            answer: None,
            naming: false,
            attention: Vec::new(),
        }
    }

    /// The utterance as the answer to the presented question's field `answer`, when given.
    pub fn answering(mut self, answer: Option<tomori_protocol::AnswerTarget>) -> Self {
        self.answer = answer;
        self
    }

    /// The utterance as the participant's new display name, which the server takes as a rename.
    pub fn naming(mut self) -> Self {
        self.naming = true;
        self
    }

    /// The utterance attending to `attention` as it begins, such as the worker the participant points at.
    pub fn attending(mut self, attention: Vec<tomori_protocol::Resource>) -> Self {
        self.attention = attention;
        self
    }
}

/// What a [`Connection`] tells its [`Session`], besides the client's [`Event`]s, in order with them.
///
/// More reports may be added; a session ignores those it does not know, with a wildcard arm.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub enum Report {
    /// Connecting to the server given at startup, as though it had been chosen. Reported by [`Connection::new`].
    Connecting(Choice),
    /// The participant's Feedback setting, as this device keeps it. Reported by [`Connection::new`].
    Feedback(feedback::Settings),
    /// The application lost focus: stop the live microphone and abandon a recording. Reported at the start of each frame without focus, before the connection closes the microphone.
    Unfocused,
    /// The servers to list: those found on the network, and those given at startup or remembered. Reported when they change, while browsing.
    Servers(Vec<Choice>),
    /// A frame at `now`, after the servers and before the client's events. `browsing` says whether the server list is focused and kept current, so that the session may connect to a server by itself.
    Tick { now: Instant, browsing: bool },
    /// The client connected: the microphone actions asked for from now on are for this connection generation. Reported just before the [`Event`] that says so.
    Generation(u64),
    /// Something failed that the participant should see, such as a request that could not be sent or a speaker that cannot open.
    Failed(String),
    /// The microphone could not record, so what it was recording is lost.
    MicrophoneFailed(String),
    /// Ending an utterance failed, so its words were not sent.
    NotSent(String),
    /// Something about the connection's security, such as a token the server refused or a certificate that changed. See `design/security.md`.
    SecurityNotice(String),
    /// Whether speech played again for this participant is playing. Reported once a frame.
    Replaying(bool),
}

/// The client's side of its conversation with a server, which a [`Connection`] carries out and reports to. `tomori-scene`'s `Conversation` is Tomori's.
///
/// The connection handles the security of the connection itself: pinning, tokens, and asking a server that refuses visitors to pair. Pairing and knocking are the session's to carry out, as the participant asks: it sends `PairBegin` with its commitment, answers [`ServerMessage::PairNonce`] with `PairReveal`, and passes on the token of [`ServerMessage::Paired`] as [`Action::Paired`], or that of [`ServerMessage::Admitted`] for its own knock as [`Action::Admitted`]. See `design/security.md`.
pub trait Session {
    /// The next action to carry out, or `None` when there is none. The connection takes them one at a time, reporting as it goes, so that a report such as [`Report::MicrophoneFailed`] can withdraw the actions queued behind the one that failed.
    fn next_action(&mut self) -> Option<Action>;

    /// What the client received: a change in the connection's state, a message from the server, or push-to-talk audio it could not deliver.
    fn apply(&mut self, event: Event);

    /// What the connection itself has to say.
    fn report(&mut self, report: Report);

    /// Whether the participant is looking at the server list, which is the only time the connection browses for servers. A session without a server list never browses.
    fn is_browsing(&self) -> bool {
        false
    }

    /// The environments the session presents, each at its protocol version, which each connection lists in its Hello so that the server offers only those; see [`crate::Options::environments`]. A session that presents no environments of its own lists none, and is offered every one, as a client from before versions is.
    fn environments(&self) -> Option<Vec<tomori_protocol::EnvironmentVersion>> {
        None
    }

    /// Tell `feedback`, once a frame at `now`, what is waiting for the participant and how long they have been recording, so that it cues them: see [`feedback::Feedback::observe_requests`], [`feedback::Feedback::observe_knocks`] and [`feedback::Feedback::observe_recording`]. A session that shows the participant nothing observes nothing.
    fn observe(&self, feedback: &mut feedback::Feedback, now: Instant) {
        let _ = (feedback, now);
    }
}

/// The other participants whose voices to place, as [`Connection::listen`] gives them.
#[derive(Clone, Copy, Debug)]
#[non_exhaustive]
pub struct Voices<'a> {
    /// The other participants in the space, by number, in order: neither this client's own nor the one it listens as.
    pub others: &'a [u32],
    /// Where each participant's head last was, from their presence.
    pub heads: &'a HashMap<u32, Pose>,
    /// Whether the participant hears in mono, as their Feedback setting says.
    pub mono: bool,
}

/// Permission to receive multicast while browsing.
pub trait Multicast {
    /// Hold the permission while `held`, as the connection browses, and release it otherwise.
    fn set_held(&mut self, held: bool);
}

/// Desktop systems deliver multicast without a lock.
impl Multicast for () {
    fn set_held(&mut self, _held: bool) {}
}

/// What a search for servers has found, as [`discovery::Browser`] reports it.
trait Browsing {
    /// The next event if one is ready.
    fn try_recv(&self) -> Result<discovery::Event, discovery::TryRecvError>;
}

impl Browsing for discovery::Browser {
    fn try_recv(&self) -> Result<discovery::Event, discovery::TryRecvError> {
        discovery::Browser::try_recv(self)
    }
}

/// Starts a search for servers.
type BrowseServers = fn() -> Result<Box<dyn Browsing>, Box<dyn std::error::Error>>;

/// A client's connection to a server, driven once a frame by [`Self::poll`] for its [`Session`]. See the [module documentation](self).
pub struct Connection {
    identity: Identity,
    store: Store,
    multicast: Box<dyn Multicast>,
    client: Option<Client>,
    browser: Option<Box<dyn Browsing>>,
    /// Starts browsing for servers, as [`discovery::browse`] does.
    browse_servers: BrowseServers,
    servers: discovery::Servers,
    /// Whether the server list needs rebuilding.
    changed: bool,
    /// Browsing failed; it is tried again on request rather than every frame.
    failed: bool,
    remembered: Option<String>,
    voice: Voice,
    speaker: Speaker,
    /// Receives other participants' presence reports, which the session need not apply. Set by a spectator that renders them.
    presence: Option<PresenceHook>,
    /// The server connected to, and how, for keeping its certificate and token.
    link: Option<Link>,
    /// What the participant hears and feels as things change; see `design/interaction.md`.
    feedback: feedback::Feedback,
    /// This participant's utterances recorded and not yet transcribed, so that their outcome is cued.
    utterances: Vec<String>,
    /// The utterance being recorded now, until X is released or it ends otherwise.
    recording: Option<String>,
    /// Speech played again to this participant, so that a catch-up nobody asked for is cued once.
    replay: Replay,
    /// Where each voice comes from: the other participants' latest heads, from relayed presence. See `design/spatial-audio.md`.
    heads: HashMap<u32, Pose>,
    /// This client's own participant number on the current connection, which is never placed.
    local_source: Option<u32>,
    /// The participant whose head the spectator hears from, who is the listener rather than a source.
    listening_as: Option<u32>,
    /// The other participants in the space, kept between frames to avoid allocating.
    others: Vec<u32>,
    /// Where [`Self::listen`] last placed the voices, kept so that it can be read back.
    placed: Scene,
    /// Measures how much of the speaker the live microphone hears, when started with it.
    echo_test: Option<EchoTest>,
    /// The name each server, by its link's key, last called this participant, which a new connection to it suggests in place of the device's for the rest of this run: so that a name it does not keep, such as a visitor's, survives connecting again. Never written down. See `design/security.md`.
    names: HashMap<String, String>,
}

/// Speech played again to this participant: a replay they asked for, or a catch-up on joining or returning.
#[derive(Debug, Default)]
struct Replay {
    /// When a replay was asked for, until it ends.
    asked: Option<Instant>,
    /// Speech played again is under way, as of its last segment.
    since: Option<Instant>,
}

impl Replay {
    /// Speech played again arrived at `now`: returns whether it is a catch-up nobody asked for that has just started, to cue.
    fn played(&mut self, now: Instant) -> bool {
        self.since.replace(now).is_none() && self.asked.is_none()
    }

    /// Whether, at `now`, the replay is over, if the player is quiet: some time after its last segment, or long after it was asked for without starting.
    fn over(&self, now: Instant) -> bool {
        match self.since {
            Some(since) => now.saturating_duration_since(since) >= REPLAY_OVER,
            None => self
                .asked
                .is_some_and(|asked| now.saturating_duration_since(asked) >= REPLAY_START),
        }
    }
}

/// How long after the last segment played again, with the player quiet, a replay counts as over.
const REPLAY_OVER: Duration = Duration::from_secs(3);
/// How long a replay asked for may take to start, as the server synthesizes it again, before it counts as never coming.
const REPLAY_START: Duration = Duration::from_secs(15);

/// How this device is connected to a server, so that what it learns can be kept for that server. See `design/security.md`.
struct Link {
    address: String,
    /// The server's discovered instance name, remembered as the server to connect to again once connected.
    instance: Option<String>,
    /// The server's discovered instance name, or its address, under which its certificate and token are kept.
    key: String,
    transport: Transport,
    token: Option<String>,
    /// Whether the token is the one given at startup, kept for this server once a connection over TLS succeeds.
    pending_token: bool,
    /// Whether the server's certificate is already kept for it.
    pinned: bool,
    /// The server presented a different certificate from the one kept: it is not trusted, so nothing it presents is kept.
    distrusted: bool,
    /// Asked to be admitted only to pair, since the server refuses visitors.
    pairing: bool,
    /// The certificate the server presented to this link's client, over TLS, once it connected: the one a token from pairing or a knock is kept with. A client never accepts another certificate once it has connected.
    certificate: Option<Fingerprint>,
}

/// What to do once the client reported why it disconnected.
enum Reopen {
    /// The server refused the token: connect again without it.
    WithoutToken,
    /// The server's certificate changed: connect again as a visitor, sending no token, and keep nothing.
    Distrusted,
    /// The server refuses visitors: connect again asking to pair.
    ToPair,
}

/// How to connect to `choice`, given what is kept for it: pin the certificate kept, or else the one it advertises; with neither, try TLS and fall back to plain HTTP.
fn transport(kept: Option<&str>, advertised: Option<&str>) -> (Transport, bool) {
    let parse =
        |fingerprint: Option<&str>| fingerprint.and_then(|text| text.parse::<Fingerprint>().ok());
    match (parse(kept), parse(advertised)) {
        (Some(pin), _) => (Transport::Tls { pin: Some(pin) }, true),
        (None, Some(pin)) => (Transport::Tls { pin: Some(pin) }, false),
        (None, None) => (Transport::Automatic { pin: None }, false),
    }
}

/// The link to keep a token from `what`, pairing or a knock, for, with the certificate its connection presented, if there is one. A session may ask after the connection has closed, as when the participant leaves the server in the frame that pairing completes, or before its client has connected over TLS; the token is then ignored.
fn issued<'a>(link: &'a mut Option<Link>, what: &str) -> Option<(&'a mut Link, Fingerprint)> {
    let Some(link) = link.as_mut() else {
        log::debug!("Ignored the token from {what}, as there is no connection to keep it for.");
        return None;
    };
    let Some(certificate) = link.certificate else {
        log::debug!(
            "Ignored the token from {what}, as the connection presented no certificate to keep it with."
        );
        return None;
    };
    Some((link, certificate))
}

/// A hook that renders the other participants' presence reports.
type PresenceHook = Box<dyn FnMut(&ServerMessage) + Send>;

/// Why ending push-to-talk failed: the utterance itself, or only the live
/// microphone reopened after it was sent.
#[derive(Debug, PartialEq)]
enum Finish {
    Utterance(String),
    Live(String),
}

impl From<Finish> for String {
    fn from(finish: Finish) -> Self {
        match finish {
            Finish::Utterance(error) | Finish::Live(error) => error,
        }
    }
}

/// Live and push-to-talk audio through the client's [`Microphone`].
struct Voice {
    capture: Box<dyn Capture>,
    /// Shared with the capture thread.
    shared: Arc<Mutex<Captured>>,
    recording: Option<Recording>,
    live: bool,
    generation: Option<u64>,
}

#[derive(Default)]
struct Captured {
    /// For the current client, at the rate of the last recording.
    microphone: Option<Microphone>,
    /// Why capture stopped early.
    failure: Option<String>,
    /// What the microphone heard since last taken, for the echo test.
    heard: Heard,
}

/// The energy of captured samples, from −1 to 1, and how many there were.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct Heard {
    energy: f64,
    samples: u64,
}

impl Heard {
    fn add(&mut self, other: Heard) {
        self.energy += other.energy;
        self.samples += other.samples;
    }

    /// The level heard, in dBFS RMS, if anything was.
    fn level(&self) -> Option<f64> {
        (self.samples > 0).then(|| 10.0 * (self.energy / self.samples as f64).max(1e-12).log10())
    }
}

/// When the echo test listens, from when the live microphone opens: first the room, then the probe the speaker plays. See `design/spatial-audio.md`.
const ECHO_FLOOR: std::ops::Range<Duration> =
    Duration::from_millis(300)..Duration::from_millis(1_000);
const ECHO_PROBE: Duration = Duration::from_millis(1_000);
/// How long the echo test waits for the speaker to play as it needs before giving up, saying so.
const ECHO_WAIT: Duration = Duration::from_secs(5);
/// The part of the probe measured, after it starts: its steady middle, allowing for the speaker's and microphone's latency.
const ECHO_HEARD: std::ops::Range<Duration> =
    Duration::from_millis(300)..Duration::from_millis(1_300);

/// The echo test, for the live microphone opened at `opened`: what it heard of the room and of the probe. See `design/spatial-audio.md`.
#[derive(Debug)]
struct EchoTest {
    /// How the live microphone captures, for the log: with the platform's echo canceller or without.
    label: &'static str,
    /// Whether the speaker plays as voice communication in the microphone's session for this test, so that it waits for that.
    routed: bool,
    /// When the live microphone opened, while the test waits for the speaker to play as it should.
    armed: Option<Instant>,
    /// The audio session the speaker played in as voice communication during the test, or `None` as media, for the log.
    speaker: Option<i32>,
    opened: Option<Instant>,
    probed: Option<Instant>,
    floor: Heard,
    echo: Heard,
}

impl EchoTest {
    /// Start timing once the speaker plays as the test needs, after any reopening for the live microphone: as voice communication, not having fallen back to media, when the test needs that. `playing` is how the open speaker plays at `now`, `None` if it is not open, and `Some(None)` as media. Gives up, saying so, after [`ECHO_WAIT`].
    fn arm(&mut self, playing: Option<Option<i32>>, now: Instant) {
        let Some(armed) = self.armed else {
            return;
        };
        match playing {
            Some(communication) if !self.routed || communication.is_some() => {
                self.armed = None;
                self.speaker = communication;
                self.opened = Some(now);
            }
            _ if now.saturating_duration_since(armed) >= ECHO_WAIT => {
                self.armed = None;
                log::warn!(
                    "Echo test ({}): nothing measured, as the speaker did not play {} within {} seconds",
                    self.label,
                    if self.routed {
                        "as voice communication in the live microphone's session"
                    } else {
                        "at all"
                    },
                    ECHO_WAIT.as_secs()
                );
            }
            _ => {}
        }
    }

    /// Attribute what the microphone `heard`, up to `now`, to the room or the probe. Returns the result to log once both are measured, and whether to play the probe now.
    fn update(&mut self, heard: Heard, now: Instant) -> (Option<String>, bool) {
        let Some(opened) = self.opened else {
            return (None, false);
        };
        let since = now.saturating_duration_since(opened);
        let mut probe = false;
        match self.probed {
            None if since >= ECHO_PROBE => {
                self.probed = Some(now);
                probe = true;
            }
            None if ECHO_FLOOR.contains(&since) => self.floor.add(heard),
            None => {}
            Some(probed) => {
                let since = now.saturating_duration_since(probed);
                if ECHO_HEARD.contains(&since) {
                    self.echo.add(heard);
                } else if since >= ECHO_HEARD.end {
                    self.opened = None;
                    let level = |heard: Heard| {
                        heard
                            .level()
                            .map_or_else(|| "nothing".into(), |level| format!("{level:.1} dBFS"))
                    };
                    let probe = 20.0 * f64::from(crate::playback::PROBE_LEVEL).log10();
                    let below = self.echo.level().map_or_else(String::new, |echo| {
                        format!(", {:.1} dB below the probe", probe - echo)
                    });
                    // Within a few dB of the room, the echo is lost in it, and the measurement says only that it is at most this.
                    let room = match (self.echo.level(), self.floor.level()) {
                        (Some(echo), Some(floor)) if echo - floor < 3.0 => {
                            format!(
                                ", within {:.1} dB of the room, so at most this",
                                echo - floor
                            )
                        }
                        (Some(echo), Some(floor)) => {
                            format!(", {:.1} dB above the room", echo - floor)
                        }
                        _ => String::new(),
                    };
                    return (
                        Some(format!(
                            "Echo test ({}, the speaker {}): the speaker played a probe at {probe:.1} dBFS; the microphone heard {} during it{below}{room}; the room was {}",
                            self.label,
                            self.speaker.map_or_else(
                                || "as media".into(),
                                |session| format!("as voice communication in session {session}")
                            ),
                            level(self.echo),
                            level(self.floor),
                        )),
                        false,
                    );
                }
            }
        }
        (None, probe)
    }
}

impl Voice {
    fn lock(&self) -> std::sync::MutexGuard<'_, Captured> {
        self.shared.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn open(&mut self, client: &Client, live: bool, generation: u64) -> Result<(), String> {
        // Never rearm a new connection while an old capture callback could
        // still be in flight. Its recording must be stopped and joined first.
        if self.recording.is_some() && self.generation != Some(generation) {
            return Err("Connection lost; microphone stopped.".into());
        }
        if !client.sender().arm_microphone(generation) {
            return Err("Connection lost; microphone stopped.".into());
        }
        if self.recording.is_some() {
            return Ok(());
        }
        match self.capture.permit()? {
            true => {}
            false => {
                return Err(if live {
                    "Allow the microphone, then squeeze the left grip again."
                } else {
                    "Allow the microphone, then hold X again."
                }
                .into());
            }
        }
        {
            let mut captured = self.lock();
            captured.failure = None;
            // Bind before the device opens, so the first samples are already
            // tied to this connection.
            if let Some(microphone) = captured.microphone.as_mut() {
                microphone.bind_connection(generation);
            }
        }
        let shared = self.shared.clone();
        let samples = Box::new(move |samples: &[f32]| {
            let mut captured = shared.lock().unwrap_or_else(PoisonError::into_inner);
            captured.heard.add(Heard {
                energy: samples
                    .iter()
                    .map(|sample| f64::from(*sample).powi(2))
                    .sum(),
                samples: samples.len() as u64,
            });
            // A capture only feeds a microphone bound to its own connection;
            // the outbox drops its frames once that connection has ended.
            if let Some(microphone) = captured.microphone.as_mut()
                && microphone.connection() == Some(generation)
            {
                // Only fails once the client is closed, when nothing more can be sent.
                let _ = microphone.push_float(samples);
            }
        });
        let shared = self.shared.clone();
        let failed = Box::new(move |error: String| {
            log::warn!("{error}");
            let mut captured = shared.lock().unwrap_or_else(PoisonError::into_inner);
            captured.failure.get_or_insert(error);
        });
        let sender = client.sender();
        let recording = if live {
            self.capture.open_live(
                samples,
                failed,
                Box::new(move || sender.connection_is_current(generation)),
            )?
        } else {
            self.capture.open(samples, failed)?
        };
        let mut captured = self.lock();
        let mut microphone = match captured.microphone.take() {
            Some(microphone) if microphone.sample_rate() == recording.sample_rate => microphone,
            _ => Microphone::new(client.sender(), recording.sample_rate),
        };
        microphone.bind_connection(generation);
        captured.microphone = Some(microphone);
        drop(captured);
        self.recording = Some(recording);
        self.generation = Some(generation);
        Ok(())
    }

    /// Ask for the microphone ahead of its first use, so that the platform's
    /// request does not interrupt the first recording. Opening the microphone
    /// still checks, and explains how to allow it if it was refused.
    fn request_permission(&mut self) -> Result<bool, String> {
        self.capture.request()
    }

    fn start_live(&mut self, client: &Client, generation: u64) -> Result<(), String> {
        self.open(client, true, generation)?;
        let result = self
            .lock()
            .microphone
            .as_mut()
            .unwrap()
            .set_gate(Gate::Live);
        if let Err(error) = result {
            self.recording = None;
            return Err(format!("Cannot send: {error}"));
        }
        self.live = true;
        Ok(())
    }

    fn stop_live(&mut self) {
        self.live = false;
        let talking = self.talking();
        // Only fails once the client is closed, when nothing more can be sent.
        let _ = self
            .lock()
            .microphone
            .as_mut()
            .map(|microphone| microphone.set_gate(Gate::Off));
        if !talking {
            self.recording = None;
        }
    }

    fn talking(&self) -> bool {
        self.lock()
            .microphone
            .as_ref()
            .is_some_and(|microphone| microphone.utterance().is_some())
    }

    fn begin(
        &mut self,
        client: &Client,
        utterance: String,
        generation: u64,
        answer: Option<tomori_protocol::AnswerTarget>,
        naming: bool,
        attention: Vec<tomori_protocol::Resource>,
    ) -> Result<(), String> {
        let resume_live = self.live && self.recording.is_some();
        if resume_live {
            if self.generation != Some(generation) || !client.sender().arm_microphone(generation) {
                return Err("Connection lost; microphone stopped.".into());
            }
            // Finish the in-flight live read before changing its gate: samples
            // captured before X must never be transcribed for the agent.
            self.recording = None;
            if let Some(error) = self.lock().failure.take() {
                return Err(error);
            }
        } else {
            self.open(client, false, generation)?;
        }
        let mut voice = self.lock();
        let microphone = voice.microphone.as_mut().unwrap();
        let result = microphone.start(utterance, answer, naming, attention);
        drop(voice);
        if let Err(error) = result {
            self.recording = None;
            return Err(format!("Cannot send: {error}"));
        }
        // Agent audio is captured as push-to-talk always is, for recognition;
        // `finish` reopens live capture afterwards.
        if resume_live && let Err(error) = self.open(client, false, generation) {
            // Only fails once the client is closed, when nothing more can be sent.
            let _ = self.lock().microphone.as_mut().map(Microphone::cancel);
            return Err(error);
        }
        Ok(())
    }

    /// Close the microphone, after which every captured sample has been passed on, then end or cancel the utterance.
    fn finish(&mut self, client: Option<&Client>, send: bool) -> Result<(), Finish> {
        let resume_live = self.live;
        let generation = self.generation;
        // Join the capture thread before changing the gate or checking for
        // failure; no in-flight read may become the next mode's audio.
        self.recording = None;
        let mut captured = self.lock();
        let failure = if send { captured.failure.take() } else { None };
        let Some(microphone) = captured.microphone.as_mut() else {
            return failure.map_or(Ok(()), |error| Err(Finish::Utterance(error)));
        };
        if let Some(error) = failure {
            microphone
                .cancel()
                .map_err(|error| Finish::Utterance(format!("Cannot cancel: {error}")))?;
            return Err(Finish::Utterance(error));
        }
        let result = if send {
            microphone.end()
        } else {
            microphone.cancel()
        };
        result.map_err(|error| Finish::Utterance(format!("Cannot send: {error}")))?;
        drop(captured);
        if resume_live {
            // A live microphone was opened for a connection, which must still be there to reopen it.
            let (client, generation) = client
                .zip(generation)
                .ok_or_else(|| Finish::Live("Connection lost; live microphone stopped.".into()))?;
            self.open(client, true, generation).map_err(Finish::Live)?;
        }
        Ok(())
    }

    /// Close the microphone and forget the utterance the server has already ended.
    fn abandon(&mut self) {
        if !self.live {
            self.recording = None;
        }
        // Only fails once the client is closed, when nothing more can be sent.
        let _ = self.lock().microphone.as_mut().map(Microphone::abandon);
    }

    /// The client is going away: close the microphone and forget it.
    fn reset(&mut self) {
        self.recording = None;
        self.live = false;
        self.generation = None;
        self.lock().microphone = None;
    }

    /// Why the open microphone stopped, if it did.
    fn failure(&mut self) -> Option<String> {
        self.recording.as_ref()?;
        self.lock().failure.take()
    }
}

/// How long to wait before opening the speaker again after it failed.
const SPEAKER_RETRY: Duration = Duration::from_secs(5);

/// Other participants and the agent's speech, played through the platform speaker by the client's [`Player`]. The speaker opens once the server welcomes the client and stays open until the client goes away.
struct Speaker {
    playback: Arc<dyn Playback>,
    /// The player of the current client, shared with the output callback, and its control.
    player: Option<(Shared, PlayerControl)>,
    /// The speaker thread, while it is opening or playing.
    output: Option<Speaking>,
    /// Why the output failed to open or stopped, set from the speaker thread.
    failure: Arc<Mutex<Option<String>>>,
    /// When to try opening the speaker again.
    retry_at: Option<Instant>,
    /// Whether a failure has been shown since the speaker last opened, so that retries do not repeat it.
    reported: bool,
    /// Whether agent speech is muted locally, kept across connections.
    muted: bool,
    /// How the open output plays, and how the next one should.
    route: Route,
    /// Whether the players made for later connections load the echo probe, for the echo test.
    probe: bool,
}

impl Speaker {
    fn new(playback: Box<dyn Playback>) -> Self {
        Self {
            playback: Arc::from(playback),
            player: None,
            output: None,
            failure: Arc::default(),
            retry_at: None,
            reported: false,
            muted: false,
            route: Route::default(),
            probe: false,
        }
    }

    /// Play as `route` says: an open output that plays otherwise is closed and opened again at once. See `design/spatial-audio.md`.
    fn reroute(&mut self, route: Route) {
        if route == self.route {
            return;
        }
        log::info!(
            "Speaker: playing {}",
            match route.communication {
                Some(session) =>
                    format!("as voice communication in session {session}, for the live microphone"),
                None => "as media".into(),
            }
        );
        self.route = route;
        if self.output.take().is_none() {
            return;
        }
        self.retry_at = None;
        // What is buffered is still current: rerouting must not cut a reply short.
        self.start(false);
    }

    /// Take the new client's received audio; nothing plays until [`Self::open`].
    fn attach(&mut self, client: &mut Client) {
        self.detach();
        let audio = client
            .take_audio()
            .expect("a new client's audio is taken once, here");
        let (mut player, control) = Player::new(audio, MixerOptions::default());
        if self.probe {
            player.load_probe();
        }
        control.set_agent_muted(self.muted);
        self.player = Some((Arc::new(Mutex::new(player)), control));
    }

    /// Mute or unmute agent speech locally, on this player and those of later connections.
    fn set_muted(&mut self, muted: bool) {
        self.muted = muted;
        if let Some(control) = self.control() {
            control.set_agent_muted(muted);
        }
    }

    /// Whether agent speech played again for this participant is playing.
    fn is_replaying(&self) -> bool {
        self.control().is_some_and(PlayerControl::is_replaying)
    }

    /// The client is going away: stop playing and forget its audio.
    fn detach(&mut self) {
        self.output = None;
        if let Some((_, control)) = self.player.take() {
            let statistics = control.statistics();
            if statistics != Default::default() {
                log::info!("Playback: {statistics:?}");
            }
        }
        self.retry_at = None;
        self.reported = false;
    }

    fn control(&self) -> Option<&PlayerControl> {
        self.player.as_ref().map(|(_, control)| control)
    }

    /// The server welcomed the client: a speaker that failed is tried again now, and shows its failure again if it fails, because the welcome clears what the session shows.
    fn connected(&mut self) {
        if self.output.is_none() {
            self.retry_at = None;
            self.reported = false;
        }
    }

    /// Start opening the speaker on its own thread if it is not open and is due. Failing to open is reported later by [`Self::poll`].
    fn open(&mut self) {
        if self.output.is_some() || self.retry_at.is_some_and(|at| Instant::now() < at) {
            return;
        }
        // Audio queued while nothing played is stale.
        self.start(true);
    }

    /// Start the speaker thread, first discarding what was queued if `clear`.
    fn start(&mut self, clear: bool) {
        let Some((player, control)) = self.player.as_ref() else {
            return;
        };
        if clear {
            control.clear();
        }
        *self.failure.lock().unwrap_or_else(PoisonError::into_inner) = None;
        let failure = self.failure.clone();
        let failed = Box::new(move |error: String| {
            failure
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .get_or_insert(error);
        });
        self.output = Some(crate::speaker::start(
            self.playback.clone(),
            player.clone(),
            failed,
            self.route,
        ));
        self.retry_at = None;
    }

    /// Close an output that failed to open or stopped, so that it is opened again after a while. Returns a failure to show.
    fn poll(&mut self) -> Option<String> {
        let output = self.output.as_ref()?;
        // Opened since it last failed: a later failure is worth showing again.
        if output.is_open() {
            self.reported = false;
        }
        let error = self
            .failure
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take()?;
        self.output = None;
        self.failed(error)
    }

    fn failed(&mut self, error: String) -> Option<String> {
        self.retry_at = Some(Instant::now() + SPEAKER_RETRY);
        if std::mem::replace(&mut self.reported, true) {
            log::debug!("Speaker: {error}");
            return None;
        }
        log::warn!("{error}");
        Some(format!("{error} Replies cannot be heard."))
    }
}

impl Connection {
    /// A connection for the participant `identity`, keeping what it learns in `store`, with the platform's `multicast` permission, microphone and speaker. It connects straight away to a server given at startup, reporting [`Report::Connecting`] to `session`; otherwise the session starts at the server list. It reports the Feedback setting kept, as [`Report::Feedback`].
    pub fn new(
        identity: Identity,
        store: Store,
        multicast: Box<dyn Multicast>,
        capture: Box<dyn Capture>,
        playback: Box<dyn Playback>,
        session: &mut dyn Session,
    ) -> Self {
        // The participant identity is a secret of this device, so it is not logged.
        log::info!("Participant {:?}", identity.name);
        if let Some(address) = &identity.server {
            session.report(Report::Connecting(Choice::new(
                address,
                address,
                Source::Override,
            )));
        } else if identity.token.is_some() {
            log::info!(
                "The token given is presented only to a server given at startup, or over USB."
            );
        }
        let remembered = store.server();
        let feedback = store.feedback();
        session.report(Report::Feedback(feedback));
        Self {
            identity,
            store,
            multicast,
            client: None,
            browser: None,
            browse_servers: || {
                discovery::browse()
                    .map(|browser| Box::new(browser) as Box<dyn Browsing>)
                    .map_err(Into::into)
            },
            servers: discovery::Servers::new(),
            changed: true,
            failed: false,
            remembered,
            voice: Voice {
                capture,
                shared: Arc::default(),
                recording: None,
                live: false,
                generation: None,
            },
            speaker: Speaker::new(playback),
            presence: None,
            link: None,
            feedback: feedback::Feedback::new(feedback),
            utterances: Vec::new(),
            recording: None,
            replay: Replay::default(),
            heads: HashMap::new(),
            local_source: None,
            listening_as: None,
            others: Vec::new(),
            placed: Scene::default(),
            echo_test: None,
            names: HashMap::new(),
        }
    }

    /// Measure, each time the live microphone opens, how much of a probe the speaker plays it hears, and log it: `label` says how the microphone captures. Needs a quiet room. See `design/spatial-audio.md`.
    pub fn set_echo_test(&mut self, label: &'static str, routed: bool) {
        log::info!(
            "Echo test ({label}): each time the live microphone opens, a probe plays a second after the speaker is ready; keep the room quiet and the volume the same between runs"
        );
        if let Some((player, _)) = &self.speaker.player {
            player
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .load_probe();
        }
        self.speaker.probe = true;
        self.echo_test = Some(EchoTest {
            label,
            routed,
            armed: None,
            speaker: None,
            opened: None,
            probed: None,
            floor: Heard::default(),
            echo: Heard::default(),
        });
    }

    /// Route the speaker for the live microphone, and run the echo test.
    fn listen_live(&mut self) {
        // Push-to-talk while live closes the live stream for a while, so the session stays until live audio stops or the microphone opens in another.
        let route = Route {
            communication: self
                .voice
                .live
                .then(|| {
                    self.voice
                        .capture
                        .live_session()
                        .or(self.speaker.route.communication)
                })
                .flatten(),
        };
        self.speaker.reroute(route);
        let heard = std::mem::take(&mut self.voice.lock().heard);
        if let Some(test) = &mut self.echo_test {
            if !self.voice.live {
                test.opened = None;
                test.armed = None;
                return;
            }
            let playing = self
                .speaker
                .output
                .as_ref()
                .filter(|output| output.is_open())
                .map(Speaking::communication);
            test.arm(playing, Instant::now());
            let (result, probe) = test.update(heard, Instant::now());
            if probe && let Some(control) = self.speaker.control() {
                control.play_probe();
            }
            if let Some(result) = result {
                log::info!("{result}");
            }
        }
    }

    /// Hear as `participant` does, from their head, as the spectator does for the participant it follows: they are the listener, so their own voice is not placed.
    pub fn listen_as(&mut self, participant: Option<u32>) {
        self.listening_as = participant;
    }

    /// Place the voices, once a frame: `participants` are those in the space this participant is in, by number, and `place` fills the player's scene from the [`Voices`] of the others, with where this participant hears from and where the agent is. See `design/spatial-audio.md`. Without a connection nothing plays, and `place` is not called.
    pub fn listen(
        &mut self,
        participants: impl IntoIterator<Item = u32>,
        place: impl FnOnce(&mut Scene, Voices<'_>),
    ) {
        let Some(control) = self.speaker.control() else {
            return;
        };
        self.others.clear();
        self.others
            .extend(participants.into_iter().filter(|source| {
                Some(*source) != self.local_source && Some(*source) != self.listening_as
            }));
        self.others.sort_unstable();
        self.others.dedup();
        let others = &self.others;
        self.heads.retain(|source, _| others.contains(source));
        let voices = Voices {
            others,
            heads: &self.heads,
            mono: self.feedback.settings().mono,
        };
        let placed = &mut self.placed;
        control.set_scene(|scene| {
            place(scene, voices);
            // Copied into storage kept between frames, so that this allocates only as the space grows.
            placed.listener = scene.listener;
            placed.sources.clone_from(&scene.sources);
            placed.mono = scene.mono;
        });
    }

    /// Where [`Self::listen`] last placed the listener and the voices, as the player was given them, or an empty scene before it first has; kept from one connection to the next, until it places them again.
    pub fn placed(&self) -> &Scene {
        &self.placed
    }

    /// What the participant hears and feels as things change, for the frame loop to report pointing and choosing to, and to take vibrations from.
    pub fn feedback(&mut self) -> &mut feedback::Feedback {
        &mut self.feedback
    }

    /// Ask for microphone access once the client can show the request.
    pub fn request_microphone(&mut self) {
        match self.microphone_permission() {
            Ok(true) => log::info!("Microphone access is allowed."),
            Ok(false) => log::info!("Asked for microphone access."),
            Err(error) => log::warn!("{error}"),
        }
    }

    /// Ask for microphone access ahead of the first recording or, once asked, whether it is allowed yet, without waiting: the platform answers the request later, so `Ok(false)` means not yet. `tomori-scene`'s `microphone_request` decides when the headset asks.
    pub fn microphone_permission(&mut self) -> Result<bool, String> {
        self.voice.request_permission()
    }

    /// Report this participant's head, controllers and hands to the server, so the others in the space can render them. The poses are in the space's scene coordinates. The client coalesces these to the latest, and the server relays them to the other participants. Without a connection, nothing is sent.
    pub fn send_presence(
        &mut self,
        head: Pose,
        left: Option<Pose>,
        right: Option<Pose>,
        hands: [Option<tomori_protocol::Hand>; 2],
    ) {
        let Some(client) = &self.client else {
            return;
        };
        let message = ClientMessage::Presence {
            head,
            left,
            right,
            hands: Some(Box::new(hands)),
        };
        // The client is open while it is kept, and the server relays only the latest presence, so nothing is lost by a send that fails.
        let _ = client.send(message);
    }

    /// Receive the other participants' presence reports, as a spectator that renders them does. The hook is called for every [`ServerMessage::Presence`], from the render loop's thread.
    pub fn watch_presence(&mut self, hook: impl FnMut(&ServerMessage) + Send + 'static) {
        self.presence = Some(Box::new(hook));
    }

    /// Run once per frame: apply what the client received to `session`, and carry out what it asks for. Browsing, and the multicast lock it needs, stop while not `focused`, as does the microphone.
    pub fn poll(&mut self, focused: bool, session: &mut dyn Session) {
        if !focused {
            session.report(Report::Unfocused);
            // Stop the capture thread before processing any queued end or
            // cancellation; focus loss must not deliver another live buffer.
            self.voice.recording = None;
        }
        if let Some(error) = self.voice.failure() {
            self.feedback
                .emit(Feedback::NotSent(Hand::Left), Instant::now());
            session.report(Report::MicrophoneFailed(error));
        }
        let browsing = focused && session.is_browsing();
        self.browse(browsing, session);
        // Browsing only while the server list is seen and kept current, so nothing connects automatically behind the system menu.
        session.report(Report::Tick {
            now: Instant::now(),
            browsing,
        });
        let mut reopen = None;
        while let Some(event) = self.client.as_ref().and_then(Client::try_event) {
            match &event {
                Event::State(ConnectionState::Connected { certificate, .. }) => {
                    self.keep(*certificate)
                }
                Event::State(ConnectionState::Disconnected { reason, .. }) => {
                    reopen = self.refused(reason, session).or(reopen);
                }
                _ => {}
            }
            let connected = matches!(event, Event::State(ConnectionState::Connected { .. }));
            if connected && let Some(client) = self.client.as_ref() {
                session.report(Report::Generation(client.sender().connection_generation()));
            }
            if connected
                && let Some(instance) = self.link.as_ref().and_then(|link| link.instance.clone())
            {
                self.store.remember_server(&instance);
                self.remembered = Some(instance);
            }
            // A client's player is made with it, so there is always one here.
            let _ = self.speaker.control().inspect(|control| match &event {
                // Audio from before reconnecting, or from a space that was left, is stale. Moving to another space leaves the current one first.
                Event::State(ConnectionState::Connected { source, .. }) => {
                    control.set_local_source(Some(*source));
                    control.clear();
                }
                Event::Message(ServerMessage::Left) => control.clear(),
                _ => {}
            });
            self.observe(&event);
            match &event {
                Event::Message(ServerMessage::Presence { source, head, .. }) => {
                    self.heads.insert(*source, *head);
                }
                Event::State(ConnectionState::Connected { source, .. }) => {
                    self.local_source = Some(*source);
                    self.heads.clear();
                }
                Event::Message(ServerMessage::Left) => self.heads.clear(),
                Event::Message(ServerMessage::Name { name }) => {
                    let link = self.link.as_ref().expect("a client is kept with its link");
                    self.names.insert(link.key.clone(), name.clone());
                }
                _ => {}
            }
            // The spectator renders presence.
            if let Event::Message(message @ ServerMessage::Presence { .. }) = &event
                && let Some(presence) = self.presence.as_deref_mut()
            {
                presence(message);
            }
            session.apply(event);
            // After the session applies the welcome, which clears earlier errors, so that a failure is shown again.
            if connected {
                self.speaker.connected();
                self.speaker.open();
            }
        }
        if let Some(error) = self.voice.failure() {
            self.feedback
                .emit(Feedback::NotSent(Hand::Left), Instant::now());
            session.report(Report::MicrophoneFailed(error));
        }
        if let Some(reopen) = reopen {
            self.reopen(reopen, session);
        }
        self.perform(session);
        if self
            .client
            .as_ref()
            .is_some_and(|client| client.state().is_connected())
        {
            if let Some(error) = self.speaker.poll() {
                session.report(Report::Failed(error));
            }
            self.speaker.open();
        }
        // Silence only during push-to-talk, not while talking to the space.
        if let Some(control) = self.speaker.control() {
            control.set_silenced(self.voice.talking());
        }
        session.report(Report::Replaying(self.speaker.is_replaying()));
        self.listen_live();
        self.cue(session);
    }

    /// Cue what the server said about this participant's words and speech played again.
    fn observe(&mut self, event: &Event) {
        let now = Instant::now();
        match event {
            Event::Message(ServerMessage::Transcript {
                utterance,
                finished: true,
                ..
            }) => {
                if self.forget_utterance(utterance) {
                    self.feedback.emit(Feedback::Sent, now);
                }
            }
            Event::Message(ServerMessage::TranscriptFailed { utterance, .. }) => {
                if self.forget_utterance(utterance) {
                    self.feedback.emit(Feedback::NotSent(Hand::Left), now);
                }
            }
            Event::Message(ServerMessage::Speech { replay: true, .. }) => {
                if self.replay.played(now) {
                    self.feedback.emit(Feedback::CatchingUp, now);
                }
            }
            // Words being recorded when the connection drops are lost. Words already released may still be queued by the server, but their transcript cannot reach this client, so they are forgotten without a cue, as the session shows no failure for them.
            Event::State(ConnectionState::Disconnected { .. }) => {
                self.replay = Replay::default();
                let lost = self.end_recording().is_some();
                self.utterances.clear();
                if lost {
                    self.feedback.emit(Feedback::NotSent(Hand::Left), now);
                }
            }
            // Live speech stops a replay.
            Event::Message(ServerMessage::Speech { replay: false, .. } | ServerMessage::Left)
            | Event::State(_) => self.replay = Replay::default(),
            _ => {}
        }
    }

    /// The utterance being recorded ended without being sent: forget it.
    fn end_recording(&mut self) -> Option<String> {
        let utterance = self.recording.take()?;
        self.utterances.retain(|id| *id != utterance);
        Some(utterance)
    }

    fn forget_utterance(&mut self, utterance: &str) -> bool {
        if self.recording.as_deref() == Some(utterance) {
            self.recording = None;
        }
        let known = self.utterances.iter().position(|id| id == utterance);
        known.map(|index| self.utterances.remove(index)).is_some()
    }

    /// Cue the requests and knocks waiting, and the recording nearing the limit, as `session` observes them, and play the cues due.
    fn cue(&mut self, session: &dyn Session) {
        let now = Instant::now();
        if self.replay.over(now) && !self.speaker.is_replaying() {
            self.replay = Replay::default();
        }
        session.observe(&mut self.feedback, now);
        self.play_cues();
    }

    /// Play the cues due. Nothing plays without a connection and an open speaker, and a cue played late would mislead, so they are dropped then.
    fn play_cues(&mut self) {
        match self
            .speaker
            .control()
            .filter(|_| self.speaker.output.is_some())
        {
            Some(control) => {
                for (earcon, direction) in self.feedback.take_earcons() {
                    control.play(earcon, direction);
                }
                for (sound, position) in self.feedback.take_ambient() {
                    control.ambient(sound, position);
                }
            }
            None => {
                self.feedback.take_earcons().for_each(drop);
                self.feedback.take_ambient().for_each(drop);
            }
        }
    }

    fn perform(&mut self, session: &mut dyn Session) {
        while let Some(action) = session.next_action() {
            match action {
                Action::Connect(choice) => {
                    let mut key = choice
                        .instance
                        .clone()
                        .unwrap_or_else(|| choice.address.clone());
                    let mut kept = self.store.credential(&key);
                    // A server reached another way, such as over USB and then over Wi-Fi, is recognized by its certificate: the one kept for this name, or else the one it advertises. That certificate is then pinned, so only the server holding its key receives the token kept for it.
                    if kept.token.is_none()
                        && let Some((server, credential)) = kept
                            .certificate
                            .as_deref()
                            .or(choice.fingerprint.as_deref())
                            .and_then(|fingerprint| {
                                self.store.credential_for_certificate(fingerprint)
                            })
                    {
                        key = server;
                        kept = credential;
                    }
                    let (transport, pinned) =
                        transport(kept.certificate.as_deref(), choice.fingerprint.as_deref());
                    // A token given at startup goes only to the server given at startup, or over USB to this computer: never to a discovered server, whose advertisement anyone on the network could make.
                    let startup = self
                        .identity
                        .token
                        .clone()
                        .filter(|_| matches!(choice.source, Source::Override | Source::Usb));
                    let pending_token = kept.token.is_none() && startup.is_some();
                    let link = Link {
                        address: choice.address,
                        instance: choice.instance,
                        key,
                        transport,
                        token: kept.token.or(startup),
                        pending_token,
                        pinned,
                        distrusted: false,
                        pairing: false,
                        certificate: None,
                    };
                    self.open(link, session);
                }
                Action::Paired { token } => self.paired(token, session),
                Action::Admitted { token } => self.admitted(token),
                Action::Disconnect => self.disconnect(),
                Action::Rescan => {
                    self.browser = None;
                    self.servers = discovery::Servers::new();
                    self.changed = true;
                    self.failed = false;
                }
                Action::Send(message) => {
                    match &message {
                        ClientMessage::Replay { .. } => self.replay.asked = Some(Instant::now()),
                        ClientMessage::StopReplay => self.replay = Replay::default(),
                        _ => {}
                    }
                    // The client is open while it is kept, and the session sends none of the messages the client reserves for itself.
                    let sent = self
                        .client
                        .as_ref()
                        .is_some_and(|client| client.send(message).is_ok());
                    if !sent {
                        self.feedback
                            .emit(Feedback::NotSent(Hand::Right), Instant::now());
                        session.report(Report::Failed("Not connected.".into()));
                    }
                }
                Action::BeginUtterance(Utterance {
                    id: utterance,
                    generation,
                    answer,
                    naming,
                    attention,
                }) => {
                    // The opening tick plays through the silencing, as X is pressed and before the microphone has opened; see `design/live-audio.md`. A live microphone is already open and would hear it, so then it is only felt.
                    self.feedback.emit_heard(
                        Feedback::RecordingStarts,
                        !self.voice.live,
                        Instant::now(),
                    );
                    self.play_cues();
                    // Silence the speaker before the microphone opens.
                    if let Some(control) = self.speaker.control() {
                        control.set_silenced(true);
                    }
                    self.utterances.retain(|id| *id != utterance);
                    self.utterances.push(utterance.clone());
                    self.recording = Some(utterance.clone());
                    let begun = match &self.client {
                        Some(client) => self
                            .voice
                            .begin(client, utterance, generation, answer, naming, attention),
                        None => Err("Not connected.".into()),
                    };
                    if let Err(error) = begun {
                        log::warn!("Cannot record: {error}");
                        self.end_recording();
                        self.feedback
                            .emit(Feedback::NotSent(Hand::Left), Instant::now());
                        self.voice.recording = None;
                        session.report(Report::MicrophoneFailed(error));
                    }
                }
                Action::EndUtterance => {
                    self.feedback.emit(Feedback::RecordingStops, Instant::now());
                    let released = self.recording.take();
                    match self.voice.finish(self.client.as_ref(), true) {
                        Ok(()) => {}
                        Err(Finish::Utterance(error)) => {
                            self.utterances.retain(|id| Some(id) != released.as_ref());
                            self.feedback
                                .emit(Feedback::NotSent(Hand::Left), Instant::now());
                            session.report(Report::NotSent(error))
                        }
                        Err(Finish::Live(error)) => session.report(Report::MicrophoneFailed(error)),
                    }
                }
                Action::CancelUtterance => {
                    self.end_recording();
                    if let Err(error) = self.voice.finish(self.client.as_ref(), false) {
                        session.report(Report::Failed(String::from(error)));
                    }
                }
                Action::AbandonUtterance => {
                    self.end_recording();
                    self.voice.abandon();
                }
                Action::StartLive(generation) => {
                    let result = match &self.client {
                        Some(client) => self.voice.start_live(client, generation),
                        None => Err("Not connected.".into()),
                    };
                    match result {
                        Ok(()) => {
                            self.feedback.emit(Feedback::LiveOn, Instant::now());
                            if let Some(test) = &mut self.echo_test {
                                *test = EchoTest {
                                    armed: Some(Instant::now()),
                                    speaker: None,
                                    opened: None,
                                    probed: None,
                                    floor: Heard::default(),
                                    echo: Heard::default(),
                                    ..*test
                                };
                            }
                        }
                        Err(error) => {
                            self.voice.recording = None;
                            self.feedback
                                .emit(Feedback::NotSent(Hand::Left), Instant::now());
                            session.report(Report::MicrophoneFailed(error));
                        }
                    }
                }
                Action::StopLive => {
                    // Only a microphone that was live turns off; one that failed to start never turned on.
                    if self.voice.live {
                        self.feedback.emit(Feedback::LiveOff, Instant::now());
                    }
                    self.voice.stop_live();
                }
                Action::SetAgentMuted(muted) => self.speaker.set_muted(muted),
                Action::SetFeedback(settings) => {
                    self.feedback.set_settings(settings);
                    self.store.remember_feedback(settings);
                }
            }
        }
    }

    /// Connect as `link` says, closing the current connection without stalling the frame: closing waits briefly for the server.
    fn open(&mut self, mut link: Link, session: &mut dyn Session) {
        self.disconnect();
        // The new client has presented no certificate yet.
        link.certificate = None;
        let name = self.names.get(&link.key).unwrap_or(&self.identity.name);
        let mut options = Options::new(link.address.clone(), &self.identity.participant, name);
        options.microphone_rearm_on_disconnect = true;
        options.transport = link.transport;
        options.token = link.token.clone();
        options.pairing = link.pairing;
        options.environments = session.environments();
        // Fails only if the system cannot start the client's thread or scheduler. The headset says so and stays usable: choosing the server again tries again.
        match Client::connect(options) {
            Ok(mut client) => {
                self.speaker.attach(&mut client);
                self.client = Some(client);
                self.link = Some(link);
            }
            Err(error) => session.report(Report::Failed(format!(
                "Cannot start a connection: {error}"
            ))),
        }
    }

    /// Keep what a connection over TLS established: the server's certificate, pinned from then on, and a token given at startup. The certificate is kept on the link even from a server that is distrusted, for pairing to replace the one kept.
    fn keep(&mut self, certificate: Option<Fingerprint>) {
        let link = self.link.as_mut().expect("a client is kept with its link");
        link.certificate = certificate;
        let Some(certificate) = certificate.filter(|_| !link.distrusted) else {
            return;
        };
        if !link.pinned {
            self.store
                .remember_certificate(&link.key, &certificate.to_string());
            link.pinned = true;
            // Pinned from now on, including when the client reconnects.
            link.transport = Transport::Tls {
                pin: Some(certificate),
            };
        }
        if link.pending_token
            && let Some(token) = &link.token
        {
            self.store.remember_token(&link.key, token);
            link.pending_token = false;
            self.identity.token = None;
        }
    }

    /// Respond to a refused connection: forget a refused token, or distrust a server whose certificate changed.
    fn refused(&mut self, reason: &Disconnect, session: &mut dyn Session) -> Option<Reopen> {
        let link = self.link.as_mut().expect("a client is kept with its link");
        match reason {
            Disconnect::Status(401) if link.token.is_some() => {
                if link.pending_token {
                    self.identity.token = None;
                } else {
                    self.store.forget_token(&link.key);
                }
                session.report(Report::SecurityNotice(
                    "This server no longer admits this device, so it watches as a visitor.".into(),
                ));
                Some(Reopen::WithoutToken)
            }
            // The server refuses visitors: ask to pair instead, which it admits.
            Disconnect::Status(401) if !link.pairing => Some(Reopen::ToPair),
            Disconnect::Status(401) => {
                session.report(Report::SecurityNotice(
                    "This server admits only devices with a token.".into(),
                ));
                None
            }
            Disconnect::Certificate(_) if !link.distrusted => {
                session.report(Report::SecurityNotice(
                    "This server's identity has changed, so this device watches as a visitor and sends it no token.".into(),
                ));
                Some(Reopen::Distrusted)
            }
            _ => None,
        }
    }

    fn reopen(&mut self, reopen: Reopen, session: &mut dyn Session) {
        let mut link = self.link.take().expect("a client is kept with its link");
        link.token = None;
        link.pending_token = false;
        match reopen {
            Reopen::Distrusted => {
                link.distrusted = true;
                link.transport = Transport::Tls { pin: None };
            }
            Reopen::ToPair => link.pairing = true,
            Reopen::WithoutToken => {}
        }
        self.open(link, session);
    }

    /// A knock admitted this device: keep its collaborator token for the server, with the certificate the connection presented, to present when it connects again. The connection is already a collaborator's, so it is not reopened.
    fn admitted(&mut self, token: String) {
        let Some((link, certificate)) = issued(&mut self.link, "an admission") else {
            return;
        };
        // Never from a server whose identity changed, over a certificate other than the one kept, or in place of a token already kept: an admission may not replace what this device trusts.
        let kept = self.store.credential(&link.key).certificate;
        if link.distrusted
            || link.token.is_some()
            || kept.is_some_and(|kept| kept != certificate.to_string())
        {
            log::warn!(
                "Ignored an admission that would replace what this device keeps for the server."
            );
            return;
        }
        self.store
            .remember_certificate(&link.key, &certificate.to_string());
        self.store.remember_token(&link.key, &token);
        link.token = Some(token);
        link.pending_token = false;
        link.pinned = true;
        link.distrusted = false;
        link.transport = Transport::Tls {
            pin: Some(certificate),
        };
    }

    /// Pairing gave this device an owner token: keep it with the certificate the pairing connection presented, which replaces one it distrusted, and connect with both. See `design/security.md`.
    fn paired(&mut self, token: String, session: &mut dyn Session) {
        let Some((_, certificate)) = issued(&mut self.link, "pairing") else {
            return;
        };
        let mut link = self.link.take().expect("a link was found");
        self.store
            .remember_certificate(&link.key, &certificate.to_string());
        self.store.remember_token(&link.key, &token);
        link.transport = Transport::Tls {
            pin: Some(certificate),
        };
        link.token = Some(token);
        link.pending_token = false;
        link.pinned = true;
        link.distrusted = false;
        link.pairing = false;
        self.open(link, session);
    }

    fn disconnect(&mut self) {
        self.link = None;
        self.utterances.clear();
        self.recording = None;
        self.voice.reset();
        self.speaker.detach();
        if let Some(client) = self.client.take() {
            // A thread that cannot start drops the client here instead, which closes it.
            let _ = std::thread::Builder::new()
                .name("tomori-close".into())
                .spawn(move || client.close());
        }
    }

    fn browse(&mut self, browsing: bool, session: &mut dyn Session) {
        if !browsing {
            if self.browser.take().is_some() {
                self.multicast.set_held(false);
                // A new browser reports only the servers it finds, not those that vanished while it was stopped.
                self.servers = discovery::Servers::new();
                self.changed = true;
            }
            return;
        }
        if self.browser.is_none() && !self.failed {
            self.multicast.set_held(true);
            match (self.browse_servers)() {
                Ok(browser) => self.browser = Some(browser),
                Err(error) => {
                    self.multicast.set_held(false);
                    self.failed = true;
                    log::warn!("Cannot browse for servers: {error}");
                    session.report(Report::Failed(format!(
                        "Cannot search the network: {error}"
                    )));
                }
            }
        }
        if let Some(browser) = &self.browser {
            while let Ok(event) = browser.try_recv() {
                self.changed |= self.servers.apply(event);
            }
        }
        self.publish(session);
    }

    fn publish(&mut self, session: &mut dyn Session) {
        if std::mem::take(&mut self.changed) {
            session.report(Report::Servers(server_choice::choices(
                self.identity.server.as_deref(),
                self.remembered.as_deref(),
                &self.servers,
            )));
        }
    }
}

impl Drop for Connection {
    fn drop(&mut self) {
        self.browser = None;
        self.multicast.set_held(false);
        self.speaker.detach();
        // Close in the foreground, so the server hears the goodbye before the process ends.
        if let Some(client) = self.client.take() {
            client.close();
        }
    }
}

#[cfg(test)]
mod tests;
