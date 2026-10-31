// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

//! Carries out the [`Conversation`]'s actions: owns the server connection and browses for servers while the server list shows.
use crate::{
    capture::{Capture, Recording},
    conversation::{Action, Conversation},
    identity::{Identity, Store},
    server_choice::{self, Choice, Source},
};
use client::{Client, ConnectionState, Event, Microphone, Options, discovery};
use std::sync::{Arc, Mutex, PoisonError};

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

impl Connection {
    /// Connects straight away to a server given at startup; otherwise the conversation starts at the server list.
    pub fn new(
        identity: Identity,
        store: Store,
        multicast: Box<dyn Multicast>,
        capture: Box<dyn Capture>,
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
        }
    }

    /// Run once per frame. Browsing, and the multicast lock it needs, stop while unfocused.
    pub fn poll(&mut self, focused: bool, conversation: &mut Conversation) {
        self.perform(conversation);
        self.browse(focused && conversation.is_browsing(), conversation);
        while let Some(event) = self.client.as_ref().and_then(Client::try_event) {
            if let Event::State(ConnectionState::Connected { .. }) = &event
                && let Some(instance) = conversation.server().and_then(|c| c.instance.as_deref())
            {
                self.store.remember_server(instance);
                self.remembered = Some(instance.to_owned());
            }
            conversation.apply(event);
        }
        if let Some(error) = self.voice.failure() {
            conversation.microphone_failed(error);
        }
        self.perform(conversation);
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
                        Ok(client) => self.client = Some(client),
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
}
