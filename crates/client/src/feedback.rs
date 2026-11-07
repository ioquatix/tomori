// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

//! One vocabulary of feedback, heard and felt: each [`Event`] pairs a short sound cue with a controller vibration on the hand that acted, with the same rhythm, filtered by the participant's [`Settings`]. Pure, so that the frame loop and the connection only report what happened and apply what comes out. See `design/feedback.md`.
use std::hash::{DefaultHasher, Hash, Hasher};
use std::time::{Duration, Instant};

use crate::playback::{Direction, Earcon};
use serde::{Deserialize, Serialize};

/// At most one hover tick in this time, so that sweeping across several targets does not chatter.
pub const HOVER_INTERVAL: Duration = Duration::from_millis(80);
/// A request still waiting this long, not yet looked at, is cued once more.
pub const REMINDER: Duration = Duration::from_secs(60);
/// How far from the server's limit on an utterance a recording turns amber, and its nearing the limit is felt.
pub const WARNING: Duration = Duration::from_secs(30);
/// How far from the limit a recording turns red, and is felt again.
pub const CRITICAL: Duration = Duration::from_secs(10);
/// At most one ambient sound in this time, however many an environment makes, so that they stay rare. See
/// `design/feedback.md`.
pub const AMBIENT_INTERVAL: Duration = Duration::from_secs(2);
/// How long each vibration of the hold-to-leave ramp lasts; it is renewed every frame while B is held, so it never outlasts the hold by more than this.
const RAMP_PULSE: Duration = Duration::from_millis(40);

/// A controller.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Hand {
    Left,
    Right,
}

/// How much of the vocabulary plays, for sound and vibration separately.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Level {
    /// Every event.
    #[default]
    Full,
    /// Only what needs the participant: requests, failures and knocks.
    Reduced,
    Off,
}

impl Level {
    /// The next level, as choosing the setting steps through them.
    pub fn next(self) -> Self {
        match self {
            Self::Full => Self::Reduced,
            Self::Reduced => Self::Off,
            Self::Off => Self::Full,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Full => "full",
            Self::Reduced => "reduced",
            Self::Off => "off",
        }
    }

    /// The level named `text`, as the console's `feedback` command takes it.
    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "full" => Some(Self::Full),
            "reduced" => Some(Self::Reduced),
            "off" => Some(Self::Off),
            _ => None,
        }
    }

    fn allows(self, event: Event) -> bool {
        match self {
            Self::Full => true,
            Self::Reduced => event.essential(),
            Self::Off => false,
        }
    }
}

/// The participant's Feedback setting, kept on the device rather than in the space.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Settings {
    #[serde(default)]
    pub sounds: Level,
    #[serde(default)]
    pub vibration: Level,
    /// Play every source in both ears alike, for people who hear with one ear; distance still changes loudness. See `design/spatial-audio.md`.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub mono: bool,
}

/// Something that changed, which the participant hears and feels. See `design/feedback.md` for exactly when each happens.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum Event {
    /// The ray entered a new target. Felt very lightly, and silent.
    Hover,
    /// The trigger or A chose something.
    Choose,
    /// X opened the microphone for push-to-talk.
    RecordingStarts,
    /// X was released and the words are on their way.
    RecordingStops,
    /// The server queued or answered the words.
    Sent,
    /// Words were not sent, or a request failed to send, by the hand that acted.
    NotSent(Hand),
    /// An approval or question arrived for the space.
    RequestWaiting,
    /// The same request still waits after [`REMINDER`], not yet looked at.
    RequestReminder,
    /// Speech played again to catch up, which was not asked for, started.
    CatchingUp,
    /// Someone knocks to take part.
    Knock,
    /// The left grip turned the live microphone on.
    LiveOn,
    /// The live microphone turned off.
    LiveOff,
    /// Holding B left the space.
    Left,
    /// The right wrist's transcript appeared on the hand, or Y pinned the hand menu open: felt very lightly on that hand, and silent.
    Shown(Hand),
    /// The recording reached 30 seconds, and then 10, before the server's limit, as the recording card turns amber and then red. Felt, and silent.
    LimitWarning,
    LimitCritical,
}

/// One vibration of a pattern: when it starts after the event, how long it lasts (`None` is the shortest the controller can), and how strongly, from 0 to 1.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Pulse {
    pub start: Duration,
    pub length: Option<Duration>,
    pub amplitude: f32,
}

const fn pulse(start: u64, length: u64, amplitude: f32) -> Pulse {
    Pulse {
        start: Duration::from_millis(start),
        length: Some(Duration::from_millis(length)),
        amplitude,
    }
}

impl Event {
    /// The hand the event is felt on: the right for pointing, choosing and requests, the left for the voice.
    pub fn hand(self) -> Hand {
        match self {
            Self::RecordingStarts
            | Self::RecordingStops
            | Self::Knock
            | Self::LiveOn
            | Self::LiveOff
            | Self::LimitWarning
            | Self::LimitCritical => Hand::Left,
            Self::NotSent(hand) | Self::Shown(hand) => hand,
            Self::Hover
            | Self::Choose
            | Self::Sent
            | Self::RequestWaiting
            | Self::RequestReminder
            | Self::CatchingUp
            | Self::Left => Hand::Right,
        }
    }

    /// Whether the event plays at the reduced level: what needs the participant, and failures.
    pub fn essential(self) -> bool {
        matches!(
            self,
            Self::NotSent(_) | Self::RequestWaiting | Self::RequestReminder | Self::Knock
        )
    }

    /// Where the cue sounds from: towards the hand that acted, from the agent for what the agent needs, or in the middle.
    pub fn direction(self) -> Direction {
        match self {
            Self::RequestWaiting | Self::RequestReminder | Self::CatchingUp => Direction::Agent,
            Self::Sent => Direction::Centre,
            event => match event.hand() {
                Hand::Left => Direction::Left,
                Hand::Right => Direction::Right,
            },
        }
    }

    pub fn earcon(self) -> Option<Earcon> {
        match self {
            Self::Hover
            | Self::RecordingStops
            | Self::Shown(_)
            | Self::LimitWarning
            | Self::LimitCritical => None,
            Self::Choose => Some(Earcon::Choose),
            Self::RecordingStarts | Self::LiveOn => Some(Earcon::On),
            Self::LiveOff => Some(Earcon::Off),
            Self::Sent => Some(Earcon::Sent),
            Self::NotSent(_) => Some(Earcon::Failed),
            Self::RequestWaiting => Some(Earcon::Request),
            Self::RequestReminder => Some(Earcon::Reminder),
            Self::CatchingUp => Some(Earcon::CatchingUp),
            Self::Knock => Some(Earcon::Knock),
            Self::Left => Some(Earcon::Leave),
        }
    }

    /// The vibration, with the same rhythm as the sound: one tap, two or three.
    pub fn pattern(self) -> &'static [Pulse] {
        match self {
            // Very light, and as short as the controller allows.
            Self::Hover | Self::Shown(_) => &[Pulse {
                start: Duration::ZERO,
                length: None,
                amplitude: 0.15,
            }],
            Self::Choose => const { &[pulse(0, 20, 0.5)] },
            Self::RecordingStarts => const { &[pulse(0, 25, 0.6)] },
            Self::RecordingStops => const { &[pulse(0, 15, 0.35)] },
            // The stop tap was just felt.
            Self::Sent | Self::CatchingUp => &[],
            Self::NotSent(_) => const { &[pulse(0, 30, 0.7), pulse(110, 30, 0.7)] },
            Self::RequestWaiting => {
                const { &[pulse(0, 25, 0.6), pulse(105, 25, 0.6), pulse(210, 25, 0.6)] }
            }
            Self::RequestReminder => const { &[pulse(0, 25, 0.5)] },
            Self::Knock => const { &[pulse(0, 25, 0.5), pulse(120, 25, 0.5)] },
            Self::LiveOn => const { &[pulse(0, 25, 0.5)] },
            Self::LiveOff => const { &[pulse(0, 15, 0.3), pulse(90, 15, 0.3)] },
            Self::Left => const { &[pulse(0, 30, 0.7)] },
            Self::LimitWarning => const { &[pulse(0, 25, 0.5)] },
            Self::LimitCritical => const { &[pulse(0, 25, 0.6), pulse(110, 25, 0.6)] },
        }
    }
}

/// What to do with a controller's vibration now.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Haptic {
    Vibrate {
        length: Option<Duration>,
        amplitude: f32,
    },
    Stop,
}

/// A request that waits, as [`Feedback::observe_requests`] last saw it.
#[derive(Clone, Debug)]
struct Waiting {
    id: String,
    since: Instant,
    /// Reminded already, or looked at, so it is not cued again.
    settled: bool,
}

/// Turns events into sound cues and vibrations, as the [`Settings`] allow.
///
/// Feed it events with [`Self::emit`] and what it observes each frame, then drain [`Self::take_earcons`] into the player and [`Self::haptics`] into the controllers. A multi-pulse pattern is sequenced across frames: each pulse is handed out at the first frame at or after its start.
#[derive(Debug, Default)]
pub struct Feedback {
    settings: Settings,
    /// Whether this client has controllers to vibrate; vibrations are not queued otherwise.
    haptics: bool,
    /// Whether this client plays cues: not the spectator, whose audience sees no interface.
    muted: bool,
    last_hover: Option<Instant>,
    /// Each pulse still to come, with its hand and whether its event is essential, so that a change to the setting can drop the rest.
    pulses: Vec<(Instant, Hand, Pulse, bool)>,
    earcons: Vec<(Earcon, Direction)>,
    /// The hold-to-leave ramp, from 0 to 1, while B is held in a space, and whether it has left.
    ramp: Option<f32>,
    left: bool,
    /// How close to the server's limit the recording in progress has been cued: 0, 1 at [`WARNING`], 2 at [`CRITICAL`].
    limit_cued: u8,
    /// Whether the ramp vibrated last frame, so that it is stopped once when the hold ends.
    ramping: bool,
    requests: Vec<Waiting>,
    knocks: Vec<String>,
    ambient: Vec<(crate::playback::Sound, crate::playback::spatial::Position)>,
    last_ambient: Option<Instant>,
}

impl Feedback {
    pub fn new(settings: Settings) -> Self {
        Self {
            settings,
            ..Self::default()
        }
    }

    pub fn settings(&self) -> Settings {
        self.settings
    }

    pub fn set_settings(&mut self, settings: Settings) {
        self.settings = settings;
        let vibration = self.settings.vibration;
        self.pulses.retain(|(.., essential)| match vibration {
            Level::Full => true,
            Level::Reduced => *essential,
            Level::Off => false,
        });
    }

    /// Whether this client's controllers vibrate. Off by default, for clients without controllers.
    pub fn set_haptics(&mut self, haptics: bool) {
        self.haptics = haptics;
        if !haptics {
            self.pulses.clear();
        }
    }

    /// Never play cues, as for the spectator. Vibrations are unaffected.
    pub fn mute_sounds(&mut self) {
        self.muted = true;
        self.earcons.clear();
    }

    /// Something happened at `now`, heard only if `heard`: recording starting while the live microphone is already open is felt and not heard, since the microphone would pick up its cue.
    pub fn emit_heard(&mut self, event: Event, heard: bool, now: Instant) {
        if heard {
            self.emit(event, now);
        } else {
            let earcons = self.earcons.len();
            self.emit(event, now);
            self.earcons.truncate(earcons);
        }
    }

    /// Something happened at `now`.
    pub fn emit(&mut self, event: Event, now: Instant) {
        if event == Event::Hover {
            if self
                .last_hover
                .is_some_and(|last| now.saturating_duration_since(last) < HOVER_INTERVAL)
            {
                return;
            }
            self.last_hover = Some(now);
        }
        log::debug!("Feedback: {event:?}");
        if !self.muted
            && self.settings.sounds.allows(event)
            && let Some(earcon) = event.earcon()
        {
            self.earcons.push((earcon, event.direction()));
        }
        if event == Event::Left {
            self.left = true;
            self.ramp = None;
        }
        if self.haptics && self.settings.vibration.allows(event) {
            let hand = event.hand();
            let essential = event.essential();
            self.pulses.extend(
                event
                    .pattern()
                    .iter()
                    .map(|pulse| (now + pulse.start, hand, *pulse, essential)),
            );
        }
    }

    /// The cues to play, oldest first, and where each sounds from.
    pub fn take_earcons(&mut self) -> std::vec::Drain<'_, (Earcon, Direction)> {
        self.earcons.drain(..)
    }

    /// An environment made `sound` at `position`, in the scene, at `now`, such as an animal calling. Ambient sounds are
    /// not part of the vocabulary: they mark nothing, so they are heard only with sounds at full, never on a client that
    /// plays no cues, and at most once in [`AMBIENT_INTERVAL`]. See `design/feedback.md`.
    pub fn ambient(
        &mut self,
        sound: crate::playback::Sound,
        position: crate::playback::spatial::Position,
        now: Instant,
    ) {
        let recent = self
            .last_ambient
            .is_some_and(|last| now.saturating_duration_since(last) < AMBIENT_INTERVAL);
        if self.muted || self.settings.sounds != Level::Full || recent {
            return;
        }
        self.last_ambient = Some(now);
        self.ambient.push((sound, position));
    }

    /// The ambient sounds to play, oldest first, and where each is.
    pub fn take_ambient(
        &mut self,
    ) -> std::vec::Drain<'_, (crate::playback::Sound, crate::playback::spatial::Position)> {
        self.ambient.drain(..)
    }

    /// B is held in a space, `progress` from 0 to 1 through the hold that leaves, or released (`None`). The right controller ramps from light to firm while it is held, until [`Event::Left`] says it left.
    pub fn hold_to_leave(&mut self, progress: Option<f32>) {
        match progress {
            Some(progress) if !self.left => self.ramp = Some(progress.clamp(0.0, 1.0)),
            Some(_) => {}
            None => {
                self.ramp = None;
                self.left = false;
            }
        }
    }

    /// The vibrations due by `now`, in order, for each hand.
    pub fn haptics(&mut self, now: Instant) -> Vec<(Hand, Haptic)> {
        let mut due = Vec::new();
        self.pulses.sort_by_key(|(at, ..)| *at);
        let ready = self.pulses.partition_point(|(at, ..)| *at <= now);
        due.extend(self.pulses.drain(..ready).map(|(_, hand, pulse, _)| {
            (
                hand,
                Haptic::Vibrate {
                    length: pulse.length,
                    amplitude: pulse.amplitude,
                },
            )
        }));
        let ramp = self
            .ramp
            .filter(|_| self.haptics && self.settings.vibration == Level::Full);
        match ramp {
            Some(progress) => {
                due.push((
                    Hand::Right,
                    Haptic::Vibrate {
                        length: Some(RAMP_PULSE),
                        amplitude: 0.1 + 0.5 * progress.min(1.0),
                    },
                ));
                self.ramping = true;
            }
            None if std::mem::take(&mut self.ramping)
                && !due.iter().any(|(hand, _)| *hand == Hand::Right) =>
            {
                due.push((Hand::Right, Haptic::Stop));
            }
            None => {}
        }
        due
    }

    /// The approvals and questions waiting in the space now, by id: a new one is cued, and one still waiting after [`REMINDER`] that was not looked at is cued once more. Several arriving together are cued once.
    pub fn observe_requests<'a>(&mut self, ids: impl IntoIterator<Item = &'a str>, now: Instant) {
        let mut arrived = false;
        let mut kept = Vec::with_capacity(self.requests.len());
        for id in ids {
            match self.requests.iter().position(|waiting| waiting.id == id) {
                Some(index) => kept.push(self.requests.swap_remove(index)),
                None => {
                    arrived = true;
                    kept.push(Waiting {
                        id: id.to_owned(),
                        since: now,
                        settled: false,
                    });
                }
            }
        }
        self.requests = kept;
        if arrived {
            self.emit(Event::RequestWaiting, now);
        }
        let mut remind = false;
        for waiting in &mut self.requests {
            if !waiting.settled && now.saturating_duration_since(waiting.since) >= REMINDER {
                waiting.settled = true;
                remind = true;
            }
        }
        if remind {
            self.emit(Event::RequestReminder, now);
        }
    }

    /// The participant looked at the requests waiting, by opening the right wrist's transcript or focusing a request there, so they are not reminded of them.
    pub fn request_opened(&mut self) {
        for waiting in &mut self.requests {
            waiting.settled = true;
        }
    }

    /// How long the participant has been recording, while they are, and the server's limit on an utterance, when it says: reaching [`WARNING`] and then [`CRITICAL`] before the limit is felt, as the recording card turns amber and then red, only the nearer when both are passed at once. See `design/feedback.md`.
    pub fn observe_recording(
        &mut self,
        elapsed: Option<Duration>,
        limit: Option<Duration>,
        now: Instant,
    ) {
        let Some(elapsed) = elapsed else {
            self.limit_cued = 0;
            return;
        };
        let Some(left) = limit.map(|limit| limit.saturating_sub(elapsed)) else {
            return;
        };
        let stage = if left <= CRITICAL {
            2
        } else if left <= WARNING {
            1
        } else {
            0
        };
        if stage > self.limit_cued {
            self.limit_cued = stage;
            self.emit(
                if stage == 2 {
                    Event::LimitCritical
                } else {
                    Event::LimitWarning
                },
                now,
            );
        }
    }

    /// The knocks waiting on the space now, by id: a new one is cued.
    pub fn observe_knocks<'a>(&mut self, ids: impl IntoIterator<Item = &'a str>, now: Instant) {
        let ids: Vec<&str> = ids.into_iter().collect();
        let arrived = ids
            .iter()
            .any(|id| !self.knocks.iter().any(|known| known == id));
        self.knocks = ids.into_iter().map(str::to_owned).collect();
        if arrived {
            self.emit(Event::Knock, now);
        }
    }
}

/// Which target the ray is on, so that entering a new one is noticed: `None` on nothing.
#[derive(Clone, Copy, Debug, Default)]
pub struct Hover {
    current: Option<u64>,
}

impl Hover {
    /// The ray is on `target`, or on nothing. Returns whether it just entered a new target, which is not while the trigger pulls the swarm.
    pub fn update(&mut self, target: Option<u64>, pulling: bool) -> bool {
        let entered = target.is_some() && target != self.current && !pulling;
        self.current = target;
        entered
    }
}

/// A stable key for a hover target, such as an arrival object's key or a worker's identifier.
pub fn key(target: &(impl Hash + ?Sized)) -> u64 {
    let mut hasher = DefaultHasher::new();
    target.hash(&mut hasher);
    hasher.finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    impl Haptic {
        /// How strongly it vibrates, or 0 for a stop.
        fn amplitude(self) -> f32 {
            match self {
                Haptic::Vibrate { amplitude, .. } => amplitude,
                Haptic::Stop => 0.0,
            }
        }
    }

    #[test]
    fn a_pulse_starts_and_lasts_as_it_says() {
        let made = pulse(110, 30, 0.7);
        assert_eq!(made.start, Duration::from_millis(110));
        assert_eq!(made.length, Some(Duration::from_millis(30)));
        assert_eq!(made.amplitude, 0.7);
    }

    fn feedback() -> (Feedback, Instant) {
        let mut feedback = Feedback::new(Settings::default());
        feedback.set_haptics(true);
        (feedback, Instant::now())
    }

    fn ms(milliseconds: u64) -> Duration {
        Duration::from_millis(milliseconds)
    }

    const EVENTS: [Event; 18] = [
        Event::Hover,
        Event::Choose,
        Event::RecordingStarts,
        Event::RecordingStops,
        Event::Sent,
        Event::NotSent(Hand::Left),
        Event::NotSent(Hand::Right),
        Event::RequestWaiting,
        Event::RequestReminder,
        Event::CatchingUp,
        Event::Knock,
        Event::LiveOn,
        Event::LiveOff,
        Event::Left,
        Event::Shown(Hand::Left),
        Event::Shown(Hand::Right),
        Event::LimitWarning,
        Event::LimitCritical,
    ];

    #[test]
    fn each_event_has_its_hand_sound_and_rhythm() {
        let table: Vec<_> = EVENTS
            .iter()
            .map(|event| (event.hand(), event.earcon(), event.pattern().len()))
            .collect();
        assert_eq!(
            table,
            [
                (Hand::Right, None, 1),
                (Hand::Right, Some(Earcon::Choose), 1),
                (Hand::Left, Some(Earcon::On), 1),
                (Hand::Left, None, 1),
                (Hand::Right, Some(Earcon::Sent), 0),
                (Hand::Left, Some(Earcon::Failed), 2),
                (Hand::Right, Some(Earcon::Failed), 2),
                (Hand::Right, Some(Earcon::Request), 3),
                (Hand::Right, Some(Earcon::Reminder), 1),
                (Hand::Right, Some(Earcon::CatchingUp), 0),
                (Hand::Left, Some(Earcon::Knock), 2),
                (Hand::Left, Some(Earcon::On), 1),
                (Hand::Left, Some(Earcon::Off), 2),
                (Hand::Right, Some(Earcon::Leave), 1),
                (Hand::Left, None, 1),
                (Hand::Right, None, 1),
                (Hand::Left, None, 1),
                (Hand::Left, None, 2),
            ]
        );
        for event in EVENTS {
            for pulse in event.pattern() {
                assert!((0.0..=1.0).contains(&pulse.amplitude), "{event:?}");
                assert!(
                    pulse.length.is_none_or(|length| length <= ms(30)),
                    "{event:?}"
                );
            }
        }
        // Hover is the lightest of all, and as short as the controller allows.
        let hover = Event::Hover.pattern()[0];
        assert_eq!(hover.length, None);
        // A panel appearing is felt as hover is; everything else more firmly.
        assert_eq!(Event::Shown(Hand::Right).pattern(), Event::Hover.pattern());
        assert!(
            EVENTS[1..]
                .iter()
                .filter(|event| !matches!(event, Event::Shown(_)))
                .all(|event| {
                    event
                        .pattern()
                        .iter()
                        .all(|pulse| pulse.amplitude > hover.amplitude)
                })
        );
    }

    #[test]
    fn cues_sound_towards_the_hand_or_from_the_agent() {
        let directions: Vec<_> = EVENTS.iter().map(|event| event.direction()).collect();
        assert_eq!(
            directions,
            [
                Direction::Right,
                Direction::Right,
                Direction::Left,
                Direction::Left,
                Direction::Centre,
                Direction::Left,
                Direction::Right,
                Direction::Agent,
                Direction::Agent,
                Direction::Agent,
                Direction::Left,
                Direction::Left,
                Direction::Left,
                Direction::Right,
                Direction::Left,
                Direction::Right,
                Direction::Left,
                Direction::Left,
            ]
        );
    }

    #[test]
    fn reduced_keeps_requests_failures_and_knocks() {
        let essential: Vec<_> = EVENTS
            .into_iter()
            .filter(|event| event.essential())
            .collect();
        assert_eq!(
            essential,
            [
                Event::NotSent(Hand::Left),
                Event::NotSent(Hand::Right),
                Event::RequestWaiting,
                Event::RequestReminder,
                Event::Knock,
            ]
        );
    }

    #[test]
    fn the_setting_filters_sounds_and_vibration_separately() {
        let (mut feedback, now) = feedback();
        for (sounds, vibration, event, heard, felt) in [
            (Level::Full, Level::Full, Event::Choose, true, true),
            (Level::Reduced, Level::Full, Event::Choose, false, true),
            (Level::Full, Level::Reduced, Event::Choose, true, false),
            (Level::Reduced, Level::Reduced, Event::Knock, true, true),
            (Level::Off, Level::Full, Event::Knock, false, true),
            (Level::Full, Level::Off, Event::Knock, true, false),
            (
                Level::Off,
                Level::Off,
                Event::NotSent(Hand::Left),
                false,
                false,
            ),
        ] {
            feedback.set_settings(Settings {
                sounds,
                vibration,
                mono: false,
            });
            feedback.emit(event, now);
            assert_eq!(
                feedback.take_earcons().count() > 0,
                heard,
                "{sounds:?} {event:?}"
            );
            assert_eq!(
                !feedback.haptics(now).is_empty(),
                felt,
                "{vibration:?} {event:?}"
            );
            assert_eq!(
                feedback.settings(),
                Settings {
                    sounds,
                    vibration,
                    mono: false
                }
            );
        }
    }

    #[test]
    fn ambient_sounds_are_heard_only_with_sounds_at_full_and_rarely() {
        let (mut feedback, now) = feedback();
        let sound = || crate::playback::Sound::new(vec![1.0; 4], 48_000);
        feedback.ambient(sound(), [1.0, 0.0, 0.0], now);
        // At most one in the interval.
        feedback.ambient(sound(), [2.0, 0.0, 0.0], now + AMBIENT_INTERVAL / 2);
        let heard: Vec<_> = feedback
            .take_ambient()
            .map(|(_, position)| position)
            .collect();
        assert_eq!(heard, [[1.0, 0.0, 0.0]]);
        feedback.ambient(sound(), [3.0, 0.0, 0.0], now + AMBIENT_INTERVAL);
        assert_eq!(feedback.take_ambient().count(), 1);
        // Not with sounds reduced, nor on a client that plays no cues.
        feedback.set_settings(Settings {
            sounds: Level::Reduced,
            ..Settings::default()
        });
        feedback.ambient(sound(), [0.0; 3], now + AMBIENT_INTERVAL * 4);
        assert_eq!(feedback.take_ambient().count(), 0);
        let (mut muted, now) = self::feedback();
        muted.mute_sounds();
        muted.ambient(sound(), [0.0; 3], now);
        assert_eq!(muted.take_ambient().count(), 0);
    }

    #[test]
    fn levels_step_parse_and_label() {
        assert_eq!(Level::Full.next(), Level::Reduced);
        assert_eq!(Level::Reduced.next(), Level::Off);
        assert_eq!(Level::Off.next(), Level::Full);
        for level in [Level::Full, Level::Reduced, Level::Off] {
            assert_eq!(Level::parse(level.label()), Some(level));
        }
        assert_eq!(Level::parse("loud"), None);
        assert_eq!(Settings::default().vibration, Level::Full);
        // Kept as JSON on the device; a missing field is full.
        let kept: Settings = serde_json::from_str(r#"{"sounds":"reduced"}"#).unwrap();
        assert_eq!(
            kept,
            Settings {
                sounds: Level::Reduced,
                vibration: Level::Full,
                mono: false,
            }
        );
        assert_eq!(
            serde_json::to_string(&kept).unwrap(),
            r#"{"sounds":"reduced","vibration":"full"}"#
        );
    }

    #[test]
    fn clients_without_controllers_queue_no_vibrations_and_the_spectator_plays_no_cues() {
        let mut feedback = Feedback::new(Settings::default());
        let now = Instant::now();
        feedback.emit(Event::RequestWaiting, now);
        assert!(feedback.haptics(now + ms(500)).is_empty());
        assert_eq!(
            feedback
                .take_earcons()
                .map(|(earcon, _)| earcon)
                .collect::<Vec<_>>(),
            [Earcon::Request]
        );
        feedback.set_haptics(true);
        feedback.emit(Event::RequestWaiting, now);
        feedback.set_haptics(false);
        assert!(feedback.haptics(now + ms(500)).is_empty());
        feedback.mute_sounds();
        feedback.emit(Event::Choose, now);
        assert_eq!(feedback.take_earcons().count(), 0);
    }

    #[test]
    fn a_cue_can_be_felt_without_being_heard() {
        let (mut feedback, now) = feedback();
        feedback.emit_heard(Event::RecordingStarts, false, now);
        assert_eq!(feedback.take_earcons().count(), 0);
        assert_eq!(feedback.haptics(now).len(), 1);
        // Only that event.
        feedback.emit_heard(Event::Choose, true, now);
        assert_eq!(feedback.take_earcons().count(), 1);
    }

    #[test]
    fn reducing_vibration_drops_the_rest_of_a_pattern_that_is_no_longer_felt() {
        let (mut feedback, now) = feedback();
        feedback.emit(Event::LiveOff, now);
        feedback.emit(Event::Knock, now);
        feedback.haptics(now);
        feedback.set_settings(Settings {
            sounds: Level::Full,
            vibration: Level::Reduced,
            mono: false,
        });
        // The knock's second tap, which is essential, remains; the live microphone's does not.
        let due = feedback.haptics(now + ms(500));
        assert_eq!(due.len(), 1);
        assert_eq!(due[0].0, Hand::Left);
    }

    #[test]
    fn multi_pulse_patterns_are_handed_out_at_frame_boundaries() {
        let (mut feedback, now) = feedback();
        feedback.emit(Event::RequestWaiting, now);
        let vibrate = |amplitude| Haptic::Vibrate {
            length: Some(ms(25)),
            amplitude,
        };
        assert_eq!(feedback.haptics(now), [(Hand::Right, vibrate(0.6))]);
        // Frames every 11 ms: nothing until the second tap is due, and then it comes at the first frame after.
        assert!(feedback.haptics(now + ms(99)).is_empty());
        assert_eq!(
            feedback.haptics(now + ms(110)),
            [(Hand::Right, vibrate(0.6))]
        );
        assert_eq!(
            feedback.haptics(now + ms(220)),
            [(Hand::Right, vibrate(0.6))]
        );
        assert!(feedback.haptics(now + ms(400)).is_empty());
        // A late frame hands out everything due, in order, on both hands.
        feedback.emit(Event::Knock, now);
        feedback.emit(Event::Choose, now + ms(5));
        let due = feedback.haptics(now + ms(500));
        assert_eq!(
            due.iter().map(|(hand, _)| *hand).collect::<Vec<_>>(),
            [Hand::Left, Hand::Right, Hand::Left]
        );
        // Turning vibration off drops what is still to come.
        feedback.emit(Event::NotSent(Hand::Left), now);
        feedback.haptics(now);
        feedback.set_settings(Settings {
            sounds: Level::Full,
            vibration: Level::Off,
            mono: false,
        });
        assert!(feedback.haptics(now + ms(500)).is_empty());
    }

    #[test]
    fn hover_ticks_at_most_once_per_interval() {
        let (mut feedback, now) = feedback();
        feedback.emit(Event::Hover, now);
        feedback.emit(Event::Hover, now + ms(40));
        feedback.emit(Event::Hover, now + ms(79));
        assert_eq!(feedback.haptics(now + ms(79)).len(), 1);
        feedback.emit(Event::Hover, now + ms(80));
        assert_eq!(
            feedback.haptics(now + ms(80)),
            [(
                Hand::Right,
                Haptic::Vibrate {
                    length: None,
                    amplitude: 0.15
                }
            )]
        );
        // Hover is felt, never heard.
        assert_eq!(feedback.take_earcons().count(), 0);
    }

    #[test]
    fn hover_is_noticed_on_entering_a_new_target_only() {
        let mut hover = Hover::default();
        let worker = key("worker-1");
        assert!(hover.update(Some(worker), false));
        assert!(!hover.update(Some(worker), false));
        assert!(!hover.update(None, false));
        assert!(hover.update(Some(worker), false));
        assert!(hover.update(Some(key("worker-2")), false));
        // Not while pulling the swarm, and the target is remembered so that releasing does not tick.
        assert!(!hover.update(Some(worker), true));
        assert!(!hover.update(Some(worker), false));
        assert_eq!(key("worker-1"), worker);
    }

    #[test]
    fn holding_to_leave_ramps_then_taps_once_and_stops_when_let_go() {
        let (mut feedback, now) = feedback();
        let amplitude = |due: &[(Hand, Haptic)]| {
            assert_eq!(due.len(), 1);
            assert_eq!(due[0].0, Hand::Right);
            due[0].1.amplitude()
        };
        assert_eq!(Haptic::Stop.amplitude(), 0.0);
        feedback.hold_to_leave(Some(0.0));
        assert!((amplitude(&feedback.haptics(now)) - 0.1).abs() < 1e-6);
        feedback.hold_to_leave(Some(0.5));
        assert!((amplitude(&feedback.haptics(now)) - 0.35).abs() < 1e-6);
        feedback.hold_to_leave(Some(1.5));
        assert!((amplitude(&feedback.haptics(now)) - 0.6).abs() < 1e-6);
        // Leaving taps once and clicks, and ends the ramp.
        feedback.emit(Event::Left, now);
        assert!((amplitude(&feedback.haptics(now)) - 0.7).abs() < 1e-6);
        assert_eq!(
            feedback
                .take_earcons()
                .map(|(earcon, _)| earcon)
                .collect::<Vec<_>>(),
            [Earcon::Leave]
        );
        // Still held after leaving: nothing more.
        feedback.hold_to_leave(Some(1.2));
        feedback.hold_to_leave(Some(0.2));
        assert!(feedback.haptics(now).is_empty());
        assert_eq!(feedback.take_earcons().count(), 0);
        feedback.hold_to_leave(None);
        // A short hold, let go, stops the ramp once.
        feedback.hold_to_leave(Some(0.3));
        feedback.haptics(now);
        feedback.hold_to_leave(None);
        assert_eq!(feedback.haptics(now), [(Hand::Right, Haptic::Stop)]);
        assert!(feedback.haptics(now).is_empty());
        // The ramp is not part of the reduced vocabulary.
        feedback.set_settings(Settings {
            sounds: Level::Full,
            vibration: Level::Reduced,
            mono: false,
        });
        feedback.hold_to_leave(Some(0.3));
        assert!(feedback.haptics(now).is_empty());
    }

    #[test]
    fn a_new_request_is_cued_once_and_reminded_once_unless_opened() {
        let (mut feedback, now) = feedback();
        let cues = |feedback: &mut Feedback| {
            feedback
                .take_earcons()
                .map(|(earcon, _)| earcon)
                .collect::<Vec<_>>()
        };
        feedback.observe_requests([], now);
        assert!(cues(&mut feedback).is_empty());
        feedback.observe_requests(["a", "b"], now);
        assert_eq!(cues(&mut feedback), [Earcon::Request]);
        feedback.observe_requests(["a", "b"], now + ms(1_000));
        assert!(cues(&mut feedback).is_empty());
        feedback.observe_requests(["b"], now + REMINDER);
        assert_eq!(cues(&mut feedback), [Earcon::Reminder]);
        feedback.observe_requests(["b"], now + REMINDER * 3);
        assert!(cues(&mut feedback).is_empty());
        // Looked at in time: no reminder.
        feedback.observe_requests(["b", "c"], now + REMINDER * 3);
        assert_eq!(cues(&mut feedback), [Earcon::Request]);
        feedback.request_opened();
        feedback.observe_requests(["b", "c"], now + REMINDER * 5);
        assert!(cues(&mut feedback).is_empty());
        // A request that left and came back is new again.
        feedback.observe_requests([], now + REMINDER * 5);
        feedback.observe_requests(["c"], now + REMINDER * 5);
        assert_eq!(cues(&mut feedback), [Earcon::Request]);
    }

    #[test]
    fn a_new_knock_is_cued() {
        let (mut feedback, now) = feedback();
        feedback.observe_knocks(["k1"], now);
        feedback.observe_knocks(["k1"], now);
        feedback.observe_knocks([], now);
        feedback.observe_knocks(["k2", "k3"], now);
        assert_eq!(
            feedback
                .take_earcons()
                .map(|(earcon, _)| earcon)
                .collect::<Vec<_>>(),
            [Earcon::Knock, Earcon::Knock]
        );
        assert_eq!(feedback.haptics(now).len(), 2);
    }

    #[test]
    fn a_recording_nearing_the_limit_is_felt_at_30_and_10_seconds_left() {
        let mut feedback = Feedback::new(Settings::default());
        feedback.set_haptics(true);
        let now = Instant::now();
        let limit = Some(Duration::from_secs(60));
        let mut felt = |elapsed: Option<u64>, limit: Option<Duration>| {
            feedback.observe_recording(elapsed.map(Duration::from_secs), limit, now);
            feedback.haptics(now + Duration::from_secs(1)).len()
        };
        assert_eq!(felt(Some(20), limit), 0);
        assert_eq!(felt(Some(30), limit), 1);
        assert_eq!(felt(Some(40), limit), 0);
        assert_eq!(felt(Some(50), limit), 2);
        assert_eq!(felt(Some(55), limit), 0);
        // A new recording starts again; without a limit, nothing is felt.
        assert_eq!(felt(None, limit), 0);
        assert_eq!(felt(Some(55), limit), 2);
        assert_eq!(felt(None, None), 0);
        assert_eq!(felt(Some(55), None), 0);
        // Felt, never heard.
        assert_eq!(feedback.take_earcons().count(), 0);
    }
}
