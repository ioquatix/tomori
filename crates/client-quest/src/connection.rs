// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

//! Carries out the [`Conversation`]'s actions: owns the server connection, the microphone and the speaker, and browses for servers while the server list shows.
use crate::{
    capture::{Capture, Recording},
    conversation::{Action, Conversation},
    identity::{Identity, Store},
    playback::{Playback, Speaking},
    server_choice::{self, Choice, Source},
};
use client::{
    Client, ConnectionState, Event, Microphone, Options, discovery,
    playback::{MixerOptions, Player, PlayerControl},
};
use protocol::ServerMessage;
use std::{
    sync::{Arc, Mutex, PoisonError},
    time::{Duration, Instant},
};

/// Permission to receive multicast while browsing.
pub trait Multicast {
    fn set_held(&mut self, held: bool);
}

/// Desktop systems deliver multicast without a lock.
impl Multicast for () {
    fn set_held(&mut self, _held: bool) {}
}

pub struct Connection {
    identity: Identity,
    store: Store,
    multicast: Box<dyn Multicast>,
    client: Option<Client>,
    browser: Option<discovery::Browser>,
    servers: discovery::Servers,
    /// Whether the server list needs rebuilding.
    changed: bool,
    /// Browsing failed; it is tried again on request rather than every frame.
    failed: bool,
    remembered: Option<String>,
    voice: Voice,
    speaker: Speaker,
}

/// Push-to-talk audio, from the platform microphone through the client's [`Microphone`]. The microphone is open only while recording.
struct Voice {
    capture: Box<dyn Capture>,
    /// Shared with the capture thread.
    shared: Arc<Mutex<Captured>>,
    recording: Option<Recording>,
}

#[derive(Default)]
struct Captured {
    /// For the current client, at the rate of the last recording.
    microphone: Option<Microphone>,
    /// Why capture stopped early.
    failure: Option<String>,
}

impl Voice {
    fn lock(&self) -> std::sync::MutexGuard<'_, Captured> {
        self.shared.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn begin(&mut self, client: &Client, utterance: String) -> Result<(), String> {
        self.recording = None;
        match self.capture.permit()? {
            true => {}
            false => return Err("Allow the microphone, then hold X again.".into()),
        }
        self.lock().failure = None;
        let shared = self.shared.clone();
        let samples = Box::new(move |samples: &[f32]| {
            let mut captured = shared.lock().unwrap_or_else(PoisonError::into_inner);
            if let Some(microphone) = captured.microphone.as_mut() {
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
        let recording = self.capture.open(samples, failed)?;
        let mut captured = self.lock();
        let microphone = match captured.microphone.take() {
            Some(microphone) if microphone.sample_rate() == recording.sample_rate => microphone,
            _ => Microphone::new(client.sender(), recording.sample_rate),
        };
        let microphone = captured.microphone.insert(microphone);
        microphone
            .begin(utterance)
            .map_err(|error| format!("Cannot send: {error}"))?;
        drop(captured);
        self.recording = Some(recording);
        Ok(())
    }

    /// Close the microphone, after which every captured sample has been passed on, then end or cancel the utterance.
    fn finish(&mut self, send: bool) -> Result<(), String> {
        self.recording = None;
        let mut captured = self.lock();
        let Some(microphone) = captured.microphone.as_mut() else {
            return Ok(());
        };
        let result = if send {
            microphone.end()
        } else {
            microphone.cancel()
        };
        result.map_err(|error| format!("Cannot send: {error}"))
    }

    /// Close the microphone and forget the utterance the server has already ended.
    fn abandon(&mut self) -> Result<(), String> {
        self.recording = None;
        let mut captured = self.lock();
        let Some(microphone) = captured.microphone.as_mut() else {
            return Ok(());
        };
        microphone
            .abandon()
            .map_err(|error| format!("Cannot send: {error}"))
    }

    /// The client is going away: close the microphone and forget it.
    fn reset(&mut self) {
        self.recording = None;
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
    player: Option<(crate::playback::Shared, PlayerControl)>,
    /// The speaker thread, while it is opening or playing.
    output: Option<Speaking>,
    /// Why the output failed to open or stopped, set from the speaker thread.
    failure: Arc<Mutex<Option<String>>>,
    /// When to try opening the speaker again.
    retry_at: Option<Instant>,
    /// Whether a failure has been shown since the speaker last opened, so that retries do not repeat it.
    reported: bool,
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
        }
    }

    /// Take the new client's received audio; nothing plays until [`Self::open`].
    fn attach(&mut self, client: &mut Client) {
        self.detach();
        if let Some(audio) = client.take_audio() {
            let (player, control) = Player::new(audio, MixerOptions::default());
            self.player = Some((Arc::new(Mutex::new(player)), control));
        }
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

    /// The server welcomed the client: a speaker that failed is tried again now, and shows its failure again if it fails, because the welcome clears what the conversation shows.
    fn connected(&mut self) {
        if self.output.is_none() {
            self.retry_at = None;
            self.reported = false;
        }
    }

    /// Start opening the speaker on its own thread if it is not open and is due. Failing to open is reported later by [`Self::poll`]. Returns a failure to show, once until the speaker opens again.
    fn open(&mut self) -> Option<String> {
        let (player, control) = self.player.as_ref()?;
        if self.output.is_some() || self.retry_at.is_some_and(|at| Instant::now() < at) {
            return None;
        }
        // Audio queued while nothing played is stale.
        control.clear();
        *self.failure.lock().unwrap_or_else(PoisonError::into_inner) = None;
        let failure = self.failure.clone();
        let failed = Box::new(move |error: String| {
            failure
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .get_or_insert(error);
        });
        match crate::playback::start(self.playback.clone(), player.clone(), failed) {
            Ok(output) => {
                self.output = Some(output);
                self.retry_at = None;
                None
            }
            Err(error) => self.failed(error),
        }
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
    /// Connects straight away to a server given at startup; otherwise the conversation starts at the server list.
    pub fn new(
        identity: Identity,
        store: Store,
        multicast: Box<dyn Multicast>,
        capture: Box<dyn Capture>,
        playback: Box<dyn Playback>,
        conversation: &mut Conversation,
    ) -> Self {
        log::info!(
            "Participant {} as {:?}",
            identity.participant,
            identity.name
        );
        if let Some(address) = &identity.server {
            conversation.connect(Choice::new(address, address, Source::Override));
        }
        let remembered = store.server();
        Self {
            identity,
            store,
            multicast,
            client: None,
            browser: None,
            servers: discovery::Servers::new(),
            changed: true,
            failed: false,
            remembered,
            voice: Voice {
                capture,
                shared: Arc::default(),
                recording: None,
            },
            speaker: Speaker::new(playback),
        }
    }

    /// Run once per frame. Browsing, and the multicast lock it needs, stop while unfocused.
    pub fn poll(&mut self, focused: bool, conversation: &mut Conversation) {
        self.perform(conversation);
        self.browse(focused && conversation.is_browsing(), conversation);
        while let Some(event) = self.client.as_ref().and_then(Client::try_event) {
            let connected = matches!(event, Event::State(ConnectionState::Connected { .. }));
            if connected
                && let Some(instance) = conversation.server().and_then(|c| c.instance.as_deref())
            {
                self.store.remember_server(instance);
                self.remembered = Some(instance.to_owned());
            }
            if let Some(control) = self.speaker.control() {
                match &event {
                    // Audio from before reconnecting, or from a space that was left, is stale. Moving to another space leaves the current one first.
                    Event::State(ConnectionState::Connected { source, .. }) => {
                        control.set_local_source(Some(*source));
                        control.clear();
                    }
                    Event::Message(ServerMessage::Left) => control.clear(),
                    _ => {}
                }
            }
            conversation.apply(event);
            // After the conversation applies the welcome, which clears earlier errors, so that a failure is shown again.
            if connected {
                self.speaker.connected();
                if let Some(error) = self.speaker.open() {
                    conversation.fail(error);
                }
            }
        }
        if let Some(error) = self.voice.failure() {
            conversation.microphone_failed(error);
        }
        self.perform(conversation);
        if self
            .client
            .as_ref()
            .is_some_and(|client| client.state().is_connected())
            && let Some(error) = self.speaker.poll().or_else(|| self.speaker.open())
        {
            conversation.fail(error);
        }
        // Nothing plays while the microphone records, so that playback cannot be transcribed. See `design/live-audio.md`.
        if let Some(control) = self.speaker.control() {
            control.set_silenced(self.voice.recording.is_some());
        }
    }

    fn perform(&mut self, conversation: &mut Conversation) {
        for action in conversation.take_actions() {
            match action {
                Action::Connect(choice) => {
                    self.disconnect();
                    let options = Options::new(
                        choice.address,
                        &self.identity.participant,
                        &self.identity.name,
                    );
                    match Client::connect(options) {
                        Ok(mut client) => {
                            self.speaker.attach(&mut client);
                            self.client = Some(client);
                        }
                        Err(error) => {
                            conversation.fail(format!("Cannot start a connection: {error}"))
                        }
                    }
                }
                Action::Disconnect => self.disconnect(),
                Action::Rescan => {
                    self.browser = None;
                    self.servers = discovery::Servers::new();
                    self.changed = true;
                    self.failed = false;
                }
                Action::Send(message) => match &self.client {
                    Some(client) => {
                        if let Err(error) = client.send(message) {
                            conversation.fail(format!("Cannot send: {error}"));
                        }
                    }
                    None => conversation.fail("Not connected."),
                },
                Action::BeginUtterance(utterance) => {
                    // Silence the speaker before the microphone opens.
                    if let Some(control) = self.speaker.control() {
                        control.set_silenced(true);
                    }
                    let begun = match &self.client {
                        Some(client) => self.voice.begin(client, utterance),
                        None => Err("Not connected.".into()),
                    };
                    if let Err(error) = begun {
                        log::warn!("Cannot record: {error}");
                        self.voice.recording = None;
                        conversation.microphone_failed(error);
                    }
                }
                Action::EndUtterance => {
                    if let Err(error) = self.voice.finish(true) {
                        conversation.fail(error);
                    }
                }
                Action::CancelUtterance => {
                    if let Err(error) = self.voice.finish(false) {
                        conversation.fail(error);
                    }
                }
                Action::AbandonUtterance => {
                    if let Err(error) = self.voice.abandon() {
                        conversation.fail(error);
                    }
                }
            }
        }
    }

    /// Close the connection without stalling the frame: closing waits briefly for the server.
    fn disconnect(&mut self) {
        self.voice.reset();
        self.speaker.detach();
        if let Some(client) = self.client.take() {
            let closed = std::thread::Builder::new()
                .name("tomori-close".into())
                .spawn(move || client.close());
            if let Err(error) = closed {
                log::warn!("Closing the connection in the background failed: {error}");
            }
        }
    }

    fn browse(&mut self, browsing: bool, conversation: &mut Conversation) {
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
            match discovery::browse() {
                Ok(browser) => self.browser = Some(browser),
                Err(error) => {
                    self.multicast.set_held(false);
                    self.failed = true;
                    log::warn!("Cannot browse for servers: {error}");
                    conversation.fail(format!("Cannot search the network: {error}"));
                }
            }
        }
        if let Some(browser) = &self.browser {
            while let Ok(event) = browser.try_recv() {
                self.changed |= self.servers.apply(event);
            }
        }
        self.publish(conversation);
    }

    fn publish(&mut self, conversation: &mut Conversation) {
        if std::mem::take(&mut self.changed) {
            conversation.set_servers(server_choice::choices(
                self.identity.server.as_deref(),
                self.remembered.as_deref(),
                &self.servers,
            ));
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
mod tests {
    use super::*;
    use crate::capture::{Failed, Samples};
    use std::{
        sync::atomic::{AtomicBool, AtomicU32, Ordering},
        thread::JoinHandle,
        time::{Duration, Instant},
    };

    /// Captures from its own thread until stopped, joining it when the recording is dropped, as AAudio does.
    struct Fake {
        rate: Arc<AtomicU32>,
        permitted: bool,
        fail: Option<String>,
    }

    struct Worker {
        stop: Arc<AtomicBool>,
        thread: Option<JoinHandle<()>>,
    }

    impl Drop for Worker {
        fn drop(&mut self) {
            self.stop.store(true, Ordering::Relaxed);
            self.thread.take().unwrap().join().unwrap();
        }
    }

    impl Capture for Fake {
        fn permit(&mut self) -> Result<bool, String> {
            Ok(self.permitted)
        }

        fn open(&mut self, mut samples: Samples, failed: Failed) -> Result<Recording, String> {
            let stop = Arc::new(AtomicBool::new(false));
            let fail = self.fail.clone();
            let thread = {
                let stop = stop.clone();
                std::thread::spawn(move || {
                    if let Some(error) = fail {
                        failed(error);
                    }
                    while !stop.load(Ordering::Relaxed) {
                        samples(&[0.1; 160]);
                        std::thread::sleep(Duration::from_millis(1));
                    }
                })
            };
            Ok(Recording::new(
                self.rate.load(Ordering::Relaxed),
                Worker {
                    stop,
                    thread: Some(thread),
                },
            ))
        }
    }

    fn voice(rate: &Arc<AtomicU32>, permitted: bool, fail: Option<&str>) -> Voice {
        Voice {
            capture: Box::new(Fake {
                rate: rate.clone(),
                permitted,
                fail: fail.map(str::to_owned),
            }),
            shared: Arc::default(),
            recording: None,
        }
    }

    /// A client that never connects, which is enough to queue messages.
    fn client() -> Client {
        Client::connect(Options::new("127.0.0.1:9", "ada", "Ada")).unwrap()
    }

    fn utterance(voice: &Voice) -> Option<String> {
        voice
            .lock()
            .microphone
            .as_ref()
            .and_then(|microphone| microphone.utterance().map(str::to_owned))
    }

    #[test]
    fn closing_a_recording_waits_for_its_thread_without_deadlock() {
        let client = client();
        let rate = Arc::new(AtomicU32::new(16_000));
        let mut voice = voice(&rate, true, None);
        voice.begin(&client, "u1".into()).unwrap();
        std::thread::sleep(Duration::from_millis(20));
        assert_eq!(utterance(&voice).as_deref(), Some("u1"));
        voice.finish(true).unwrap();
        assert!(voice.recording.is_none());
        assert_eq!(utterance(&voice), None);

        voice.begin(&client, "u2".into()).unwrap();
        std::thread::sleep(Duration::from_millis(5));
        voice.abandon().unwrap();
        assert_eq!(utterance(&voice), None);

        voice.begin(&client, "u3".into()).unwrap();
        voice.reset();
        assert!(voice.lock().microphone.is_none());
        client.close();
    }

    #[test]
    fn a_new_rate_gets_a_new_microphone() {
        let client = client();
        let rate = Arc::new(AtomicU32::new(16_000));
        let mut voice = voice(&rate, true, None);
        voice.begin(&client, "u1".into()).unwrap();
        voice.finish(false).unwrap();
        rate.store(48_000, Ordering::Relaxed);
        voice.begin(&client, "u2".into()).unwrap();
        let sample_rate = voice.lock().microphone.as_ref().unwrap().sample_rate();
        assert_eq!(sample_rate, 48_000);
        voice.finish(false).unwrap();
        client.close();
    }

    #[test]
    fn nothing_records_without_permission() {
        let client = client();
        let rate = Arc::new(AtomicU32::new(16_000));
        let mut voice = voice(&rate, false, None);
        let error = voice.begin(&client, "u1".into()).unwrap_err();
        assert!(error.contains("Allow the microphone"), "{error}");
        assert!(voice.recording.is_none());
        assert_eq!(utterance(&voice), None);
        client.close();
    }

    #[test]
    fn a_capture_failure_is_reported_only_while_recording() {
        let client = client();
        let rate = Arc::new(AtomicU32::new(16_000));
        let mut voice = voice(&rate, true, Some("The microphone failed."));
        voice.begin(&client, "u1".into()).unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        let failure = loop {
            if let Some(failure) = voice.failure() {
                break failure;
            }
            assert!(Instant::now() < deadline, "no failure reported");
            std::thread::sleep(Duration::from_millis(1));
        };
        assert_eq!(failure, "The microphone failed.");
        voice.finish(false).unwrap();

        // Closed before it is noticed: nothing is reported once the recording has ended.
        voice.begin(&client, "u2".into()).unwrap();
        voice.finish(false).unwrap();
        assert_eq!(voice.failure(), None);
        client.close();
    }

    /// Opens outputs that render the player once, or fails to open while `refuse` is set. Keeps each open output's failure callback.
    #[derive(Clone, Default)]
    struct FakeSpeaker {
        opened: Arc<AtomicU32>,
        closed: Arc<AtomicU32>,
        refuse: Arc<AtomicBool>,
        failed: Arc<Mutex<Option<crate::playback::Failed>>>,
    }

    /// Closes by counting, so that a test can see the speaker thread close its output.
    struct FakeOutput(Arc<AtomicU32>);

    impl Drop for FakeOutput {
        fn drop(&mut self) {
            self.0.fetch_add(1, Ordering::Relaxed);
        }
    }

    impl Playback for FakeSpeaker {
        fn open(
            &self,
            player: crate::playback::Shared,
            failed: crate::playback::Failed,
        ) -> Result<crate::playback::Output, String> {
            if self.refuse.load(Ordering::Relaxed) {
                return Err("No speaker is available.".into());
            }
            let mut output = [1; 48];
            player.lock().unwrap().render(48_000, &mut output);
            assert_eq!(output, [0; 48]);
            *self.failed.lock().unwrap() = Some(failed);
            self.opened.fetch_add(1, Ordering::Relaxed);
            Ok(crate::playback::Output::new(FakeOutput(
                self.closed.clone(),
            )))
        }
    }

    /// Wait for the speaker thread.
    fn until(condition: impl Fn() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !condition() {
            assert!(Instant::now() < deadline, "timed out");
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    /// Poll the speaker until it reports a failure.
    fn failure(speaker: &mut Speaker) -> String {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let Some(error) = speaker.poll() {
                return error;
            }
            assert!(Instant::now() < deadline, "no failure reported");
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    #[test]
    fn the_speaker_opens_on_its_own_thread_and_closes_when_the_client_goes_away() {
        let mut client = client();
        let fake = FakeSpeaker::default();
        let mut speaker = Speaker::new(Box::new(fake.clone()));
        // Nothing to play before a client is attached.
        assert_eq!(speaker.open(), None);
        assert!(speaker.output.is_none());

        speaker.attach(&mut client);
        assert!(client.take_audio().is_none(), "the speaker took the audio");
        assert_eq!(speaker.open(), None);
        assert_eq!(speaker.open(), None);
        until(|| fake.opened.load(Ordering::Relaxed) == 1);
        assert!(speaker.output.as_ref().unwrap().is_open());
        assert_eq!(speaker.poll(), None);

        speaker.detach();
        assert!(speaker.output.is_none() && speaker.control().is_none());
        until(|| fake.closed.load(Ordering::Relaxed) == 1);
        assert_eq!(fake.opened.load(Ordering::Relaxed), 1);
        client.close();
    }

    #[test]
    fn a_failed_speaker_is_reported_once_and_opened_again_later() {
        let mut client = client();
        let fake = FakeSpeaker::default();
        let mut speaker = Speaker::new(Box::new(fake.clone()));
        speaker.attach(&mut client);

        fake.refuse.store(true, Ordering::Relaxed);
        assert_eq!(speaker.open(), None);
        let error = failure(&mut speaker);
        assert!(error.contains("No speaker"), "{error}");
        assert!(speaker.output.is_none());
        // Not tried again until the retry is due, and not shown again when it fails again.
        assert_eq!(speaker.open(), None);
        assert!(speaker.output.is_none());
        speaker.retry_at = Some(Instant::now());
        assert_eq!(speaker.open(), None);
        until(|| speaker.failure.lock().unwrap().is_some());
        assert_eq!(speaker.poll(), None);
        assert!(speaker.output.is_none());

        // A new welcome tries again at once, and shows the failure again.
        speaker.connected();
        assert_eq!(speaker.open(), None);
        let error = failure(&mut speaker);
        assert!(error.contains("No speaker"), "{error}");

        fake.refuse.store(false, Ordering::Relaxed);
        speaker.retry_at = Some(Instant::now());
        assert_eq!(speaker.open(), None);
        until(|| fake.opened.load(Ordering::Relaxed) == 1);
        assert_eq!(speaker.poll(), None);

        // The open output fails from another thread: it is closed, shown, and reopened once due.
        let failed = fake.failed.lock().unwrap().take().unwrap();
        std::thread::spawn(move || failed("The speaker failed: unplugged".into()))
            .join()
            .unwrap();
        let error = speaker.poll().expect("the failure is shown");
        assert!(error.contains("unplugged"), "{error}");
        assert!(speaker.output.is_none());
        until(|| fake.closed.load(Ordering::Relaxed) == 1);
        assert_eq!(speaker.open(), None);
        speaker.retry_at = Some(Instant::now());
        assert_eq!(speaker.open(), None);
        until(|| fake.opened.load(Ordering::Relaxed) == 2);
        client.close();
    }

    #[test]
    fn opening_the_speaker_discards_audio_queued_while_nothing_played() {
        let fake = FakeSpeaker::default();
        let mut speaker = Speaker::new(Box::new(fake.clone()));
        let (sender, audio) = std::sync::mpsc::sync_channel(8);
        let (player, control) = Player::new(audio, MixerOptions::default());
        speaker.player = Some((Arc::new(Mutex::new(player)), control));
        for sequence in 0..8 {
            sender
                .send(protocol::AudioFrame {
                    gate: protocol::Gate::Live,
                    source: 1,
                    sequence,
                    timestamp: 0,
                    sample_rate: 48_000,
                    samples: vec![1000; 960],
                })
                .unwrap();
        }
        // The fake renders as it opens, and asserts that it hears nothing.
        assert_eq!(speaker.open(), None);
        until(|| fake.opened.load(Ordering::Relaxed) == 1);
    }
}
