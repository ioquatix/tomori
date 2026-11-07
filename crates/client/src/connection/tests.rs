// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

//! The connection's tests. Its parts, the voice, the speaker, the echo test and the replay, are tested on their own; the connection as a whole is driven through [`Session`] by [`Script`], a session that asks for what each test says and records what it is told, against a scripted [`Peer`] or a real [`Server`].
use super::*;
use crate::{
    capture::{Active, Failed, Samples},
    feedback::{Haptic, Level, Settings},
    playback::{Earcon, Layout},
    speaker::Output,
};
use std::{
    cell::RefCell,
    collections::VecDeque,
    io::{Read, Write},
    net::TcpListener,
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, AtomicU32, Ordering},
        mpsc,
    },
    thread::JoinHandle,
};
use tomori_protocol::{ConnectionRole, Frame};

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

/// A capture thread waiting for its recording to be dropped, then joined.
struct Waiting {
    stop: Option<mpsc::Sender<()>>,
    thread: Option<JoinHandle<()>>,
}

impl Drop for Waiting {
    fn drop(&mut self) {
        self.stop.take();
        self.thread.take().unwrap().join().unwrap();
    }
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

    fn request(&mut self) -> Result<bool, String> {
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

    fn open_live(
        &mut self,
        samples: Samples,
        failed: Failed,
        _active: Active,
    ) -> Result<Recording, String> {
        self.open(samples, failed)
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
        live: false,
        generation: None,
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

// Also built here, as well as by the scene: a builder this small is inlined into other crates, where its coverage is not counted as its own.
#[test]
fn an_utterance_is_for_the_agent_unless_it_answers_or_names() {
    let utterance = Utterance::new("u-1", 3);
    assert!(utterance.answer.is_none() && !utterance.naming && utterance.attention.is_empty());
    let naming = Utterance::new("u-1", 3).naming();
    assert!(naming.naming && naming.answer.is_none() && naming.generation == 3);
    // What it attends to, as it begins, goes with it.
    let worker = tomori_protocol::Resource::worker("ses_1");
    let attending = Utterance::new("u-1", 3).attending(vec![worker.clone()]);
    assert_eq!(attending.attention, [worker]);
}

#[test]
fn closing_a_recording_waits_for_its_thread_without_deadlock() {
    let client = client();
    let rate = Arc::new(AtomicU32::new(16_000));
    let mut voice = voice(&rate, true, None);
    voice
        .begin(&client, "u1".into(), 0, None, false, Vec::new())
        .unwrap();
    std::thread::sleep(Duration::from_millis(20));
    assert_eq!(utterance(&voice).as_deref(), Some("u1"));
    voice.finish(Some(&client), true).unwrap();
    assert!(voice.recording.is_none());
    assert_eq!(utterance(&voice), None);

    voice
        .begin(&client, "u2".into(), 0, None, false, Vec::new())
        .unwrap();
    std::thread::sleep(Duration::from_millis(5));
    voice.abandon();
    assert_eq!(utterance(&voice), None);

    // Words that say a new name are recorded the same way.
    voice
        .begin(&client, "u3".into(), 0, None, true, Vec::new())
        .unwrap();
    voice.reset();
    assert!(voice.lock().microphone.is_none());
    client.close();
}

#[test]
fn a_new_rate_gets_a_new_microphone() {
    let client = client();
    let rate = Arc::new(AtomicU32::new(16_000));
    let mut voice = voice(&rate, true, None);
    voice
        .begin(&client, "u1".into(), 0, None, false, Vec::new())
        .unwrap();
    voice.finish(Some(&client), false).unwrap();
    rate.store(48_000, Ordering::Relaxed);
    voice
        .begin(&client, "u2".into(), 0, None, false, Vec::new())
        .unwrap();
    let sample_rate = voice.lock().microphone.as_ref().unwrap().sample_rate();
    assert_eq!(sample_rate, 48_000);
    voice.finish(Some(&client), false).unwrap();
    client.close();
}

#[test]
fn nothing_records_without_permission() {
    let client = client();
    let rate = Arc::new(AtomicU32::new(16_000));
    let mut voice = voice(&rate, false, None);
    assert_eq!(voice.request_permission(), Ok(false));
    let error = voice
        .begin(&client, "u1".into(), 0, None, false, Vec::new())
        .unwrap_err();
    assert!(error.contains("Allow the microphone"), "{error}");
    assert!(voice.recording.is_none());
    assert_eq!(utterance(&voice), None);
    client.close();
}

#[test]
fn live_microphone_resumes_after_agent_and_can_be_muted_during_it() {
    let client = client();
    let rate = Arc::new(AtomicU32::new(16_000));
    let mut voice = voice(&rate, true, None);
    voice.start_live(&client, 0).unwrap();
    assert_eq!(voice.lock().microphone.as_ref().unwrap().gate(), Gate::Live);
    voice
        .begin(&client, "u1".into(), 0, None, false, Vec::new())
        .unwrap();
    assert!(voice.talking());
    assert_eq!(
        voice.lock().microphone.as_ref().unwrap().gate(),
        Gate::Agent
    );
    voice.finish(Some(&client), true).unwrap();
    assert!(voice.recording.is_some());
    assert_eq!(voice.lock().microphone.as_ref().unwrap().gate(), Gate::Live);
    voice
        .begin(&client, "u2".into(), 0, None, false, Vec::new())
        .unwrap();
    voice.stop_live();
    assert_eq!(
        voice.lock().microphone.as_ref().unwrap().gate(),
        Gate::Agent
    );
    voice.finish(Some(&client), true).unwrap();
    assert!(voice.recording.is_none());
    assert_eq!(voice.lock().microphone.as_ref().unwrap().gate(), Gate::Off);
    client.close();
}

#[test]
fn live_microphone_stops_and_permission_is_required() {
    let client = client();
    let rate = Arc::new(AtomicU32::new(16_000));
    let mut denied = voice(&rate, false, None);
    assert!(
        denied
            .start_live(&client, 0)
            .unwrap_err()
            .contains("left grip")
    );
    assert!(denied.recording.is_none());
    let mut voice = voice(&rate, true, None);
    voice.start_live(&client, 0).unwrap();
    voice.stop_live();
    assert!(voice.recording.is_none());
    assert_eq!(voice.lock().microphone.as_ref().unwrap().gate(), Gate::Off);
    client.close();
}

#[test]
fn microphone_access_is_requested_early_without_using_up_the_recording_request() {
    /// Records which permission calls were made, granting only once asked at recording.
    struct Asking(Arc<Mutex<Vec<&'static str>>>);
    impl Capture for Asking {
        fn permit(&mut self) -> Result<bool, String> {
            let mut calls = self.0.lock().unwrap();
            calls.push("permit");
            // Allowed once the participant answers the first request.
            Ok(calls.len() > 2)
        }
        fn request(&mut self) -> Result<bool, String> {
            self.0.lock().unwrap().push("request");
            Ok(false)
        }
        fn open(&mut self, _samples: Samples, _failed: Failed) -> Result<Recording, String> {
            self.0.lock().unwrap().push("open");
            Ok(Recording::new(16_000, ()))
        }
    }
    let client = client();
    let calls = Arc::new(Mutex::new(Vec::new()));
    let mut voice = Voice {
        capture: Box::new(Asking(calls.clone())),
        shared: Arc::new(Mutex::new(Captured::default())),
        recording: None,
        live: false,
        generation: None,
    };
    assert_eq!(voice.request_permission(), Ok(false));
    // Asking opens nothing.
    assert!(voice.recording.is_none());
    assert!(voice.lock().microphone.is_none());
    // The first recording still makes its own request.
    assert!(
        voice
            .begin(&client, "u1".into(), 0, None, false, Vec::new())
            .unwrap_err()
            .contains("hold X again")
    );
    assert_eq!(*calls.lock().unwrap(), ["request", "permit"]);
    // Allowed, the next recording opens the microphone.
    voice
        .begin(&client, "u2".into(), 0, None, false, Vec::new())
        .unwrap();
    assert_eq!(
        *calls.lock().unwrap(),
        ["request", "permit", "permit", "open"]
    );
    client.close();
}

#[test]
fn a_capture_failure_is_reported_only_while_recording() {
    let client = client();
    let rate = Arc::new(AtomicU32::new(16_000));
    let mut voice = voice(&rate, true, Some("The microphone failed."));
    voice
        .begin(&client, "u1".into(), 0, None, false, Vec::new())
        .unwrap();
    // Waits at least once, however soon the capture fails, so that how the race went changes no coverage.
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut failure = None;
    while failure.is_none() {
        assert!(Instant::now() < deadline, "no failure reported");
        std::thread::sleep(Duration::from_millis(1));
        failure = voice.failure();
    }
    assert_eq!(failure.as_deref(), Some("The microphone failed."));
    voice.finish(Some(&client), false).unwrap();

    // Closed before it is noticed: nothing is reported once the recording has ended.
    voice
        .begin(&client, "u2".into(), 0, None, false, Vec::new())
        .unwrap();
    voice.finish(Some(&client), false).unwrap();
    assert_eq!(voice.failure(), None);
    client.close();
}

#[test]
fn failure_before_end_cancels_the_utterance_and_keeps_the_microphone() {
    let client = client();
    let rate = Arc::new(AtomicU32::new(16_000));
    let mut voice = voice(&rate, true, None);
    voice.start_live(&client, 0).unwrap();
    voice
        .begin(&client, "u1".into(), 0, None, false, Vec::new())
        .unwrap();
    voice.lock().failure = Some("capture failed".into());
    assert_eq!(
        voice.finish(Some(&client), true).unwrap_err(),
        Finish::Utterance("capture failed".into())
    );
    assert_eq!(utterance(&voice), None);
    assert!(voice.lock().microphone.is_some());
    voice.stop_live();
    client.close();
}

/// A capture that records which kind of capture each step opened.
struct KindCapture(Arc<Mutex<Vec<&'static str>>>);

impl Capture for KindCapture {
    fn open(&mut self, _samples: Samples, _failed: Failed) -> Result<Recording, String> {
        self.0.lock().unwrap().push("agent");
        Ok(Recording::new(16_000, ()))
    }
    fn open_live(
        &mut self,
        _samples: Samples,
        _failed: Failed,
        _active: Active,
    ) -> Result<Recording, String> {
        self.0.lock().unwrap().push("live");
        Ok(Recording::new(16_000, ()))
    }
}

fn kind_voice() -> (Voice, Arc<Mutex<Vec<&'static str>>>) {
    let opened = Arc::new(Mutex::new(Vec::new()));
    let voice = Voice {
        capture: Box::new(KindCapture(opened.clone())),
        shared: Arc::new(Mutex::new(Captured::default())),
        recording: None,
        live: false,
        generation: None,
    };
    (voice, opened)
}

#[test]
fn push_to_talk_during_live_audio_captures_for_recognition() {
    let client = client();
    let (mut voice, opened) = kind_voice();
    voice.start_live(&client, 0).unwrap();
    voice
        .begin(&client, "u1".into(), 0, None, false, Vec::new())
        .unwrap();
    voice.finish(Some(&client), true).unwrap();
    assert_eq!(*opened.lock().unwrap(), ["live", "agent", "live"]);
    client.close();
}

#[test]
fn a_failed_live_reopen_after_a_sent_utterance_is_reported_separately() {
    struct ReopenFails(u32);
    impl Capture for ReopenFails {
        fn open(&mut self, _samples: Samples, _failed: Failed) -> Result<Recording, String> {
            Ok(Recording::new(16_000, ()))
        }
        fn open_live(
            &mut self,
            _samples: Samples,
            _failed: Failed,
            _active: Active,
        ) -> Result<Recording, String> {
            self.0 += 1;
            if self.0 > 1 {
                return Err("Cannot open the microphone.".into());
            }
            Ok(Recording::new(16_000, ()))
        }
    }
    let client = client();
    let mut voice = Voice {
        capture: Box::new(ReopenFails(0)),
        shared: Arc::new(Mutex::new(Captured::default())),
        recording: None,
        live: false,
        generation: None,
    };
    voice.start_live(&client, 0).unwrap();
    voice
        .begin(&client, "u1".into(), 0, None, false, Vec::new())
        .unwrap();
    // The utterance was ended; only the live microphone failed afterwards.
    assert_eq!(
        voice.finish(Some(&client), true).unwrap_err(),
        Finish::Live("Cannot open the microphone.".into())
    );
    assert_eq!(utterance(&voice), None);
    client.close();
}

#[test]
fn a_capture_for_one_connection_is_never_reused_for_another() {
    let client = client();
    let (mut voice, opened) = kind_voice();
    voice.start_live(&client, 0).unwrap();
    // Neither live audio nor push-to-talk for a later connection may use
    // the earlier connection's open capture.
    assert!(voice.start_live(&client, 1).is_err());
    assert!(
        voice
            .begin(&client, "u1".into(), 1, None, false, Vec::new())
            .is_err()
    );
    assert_eq!(*opened.lock().unwrap(), ["live"]);
    // The capture feeds only a microphone bound to its own connection.
    assert_eq!(
        voice.lock().microphone.as_ref().unwrap().connection(),
        Some(0)
    );
    voice.stop_live();
    client.close();
}

#[test]
fn live_and_agent_capture_threads_stop_before_the_gate_changes() {
    struct BoundaryCapture {
        shared: Arc<Mutex<Captured>>,
        closed_gates: Arc<Mutex<Vec<Gate>>>,
    }
    impl Capture for BoundaryCapture {
        fn open(&mut self, _samples: Samples, _failed: Failed) -> Result<Recording, String> {
            let (stop, stopped) = mpsc::channel::<()>();
            let shared = self.shared.clone();
            let closed_gates = self.closed_gates.clone();
            let thread = std::thread::spawn(move || {
                // Until the recording is dropped.
                let _ = stopped.recv();
                let gate = shared.lock().unwrap().microphone.as_ref().unwrap().gate();
                closed_gates.lock().unwrap().push(gate);
            });
            Ok(Recording::new(
                16_000,
                Waiting {
                    stop: Some(stop),
                    thread: Some(thread),
                },
            ))
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
    let client = client();
    let shared = Arc::new(Mutex::new(Captured::default()));
    let closed_gates = Arc::new(Mutex::new(Vec::new()));
    let mut voice = Voice {
        capture: Box::new(BoundaryCapture {
            shared: shared.clone(),
            closed_gates: closed_gates.clone(),
        }),
        shared,
        recording: None,
        live: false,
        generation: None,
    };
    voice.start_live(&client, 0).unwrap();
    voice
        .begin(&client, "u1".into(), 0, None, false, Vec::new())
        .unwrap();
    voice.finish(Some(&client), true).unwrap();
    voice.stop_live();
    assert_eq!(
        *closed_gates.lock().unwrap(),
        [Gate::Live, Gate::Agent, Gate::Off]
    );
    client.close();
}

#[test]
fn failure_reported_during_capture_shutdown_cancels_the_agent_utterance() {
    struct FailureOnStop(Arc<AtomicU32>);
    impl Capture for FailureOnStop {
        fn open(&mut self, _samples: Samples, failed: Failed) -> Result<Recording, String> {
            let attempt = self.0.fetch_add(1, Ordering::Relaxed);
            let (stop, stopped) = mpsc::channel::<()>();
            let thread = std::thread::spawn(move || {
                // Until the recording is dropped.
                let _ = stopped.recv();
                if attempt == 1 {
                    failed("read failed while stopping".into());
                }
            });
            Ok(Recording::new(
                16_000,
                Waiting {
                    stop: Some(stop),
                    thread: Some(thread),
                },
            ))
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
    let client = client();
    let attempts = Arc::new(AtomicU32::new(0));
    let mut voice = Voice {
        capture: Box::new(FailureOnStop(attempts.clone())),
        shared: Arc::default(),
        recording: None,
        live: false,
        generation: None,
    };
    voice.start_live(&client, 0).unwrap();
    voice
        .begin(&client, "u1".into(), 0, None, false, Vec::new())
        .unwrap();
    assert_eq!(
        voice.finish(Some(&client), true).unwrap_err(),
        Finish::Utterance("read failed while stopping".into())
    );
    assert_eq!(utterance(&voice), None);
    assert_eq!(attempts.load(Ordering::Relaxed), 2);
    voice.stop_live();
    client.close();
}

/// A capture that permits and opens as it is told, keeping the callbacks it is given.
#[derive(Default)]
struct Scripted {
    /// Why permission is refused, if it is.
    refuse: Option<String>,
    /// Why opening fails, if it does, after reporting it through the failure callback; from the opening numbered `fail_from` on.
    fail: Option<String>,
    fail_from: usize,
    opened: usize,
    samples: Arc<Mutex<Vec<Samples>>>,
    /// The failure callback of each capture opened.
    failed: Arc<Mutex<Vec<Failed>>>,
    /// What live capture was told about its connection when it opened.
    current: Arc<Mutex<Vec<bool>>>,
    /// Whether asking ahead of time is granted at once.
    granted: bool,
    /// How many times it was asked ahead of time.
    asked: Arc<AtomicU32>,
    /// Why capture fails as it stops, if it does.
    fail_on_stop: Option<String>,
    /// How many captures it opened have stopped.
    stopped: Arc<AtomicU32>,
    /// The audio session live capture plays in.
    session: Option<i32>,
}

/// Reports a failure when its recording is dropped, as a device whose last read fails, and counts it as stopped.
struct FailsOnStop(Option<(Failed, String)>, Arc<AtomicU32>);

impl Drop for FailsOnStop {
    fn drop(&mut self) {
        self.1.fetch_add(1, Ordering::Relaxed);
        if let Some((failed, error)) = self.0.take() {
            failed(error);
        }
    }
}

impl Capture for Scripted {
    fn permit(&mut self) -> Result<bool, String> {
        self.refuse.clone().map_or(Ok(true), Err)
    }

    fn request(&mut self) -> Result<bool, String> {
        self.asked.fetch_add(1, Ordering::Relaxed);
        self.refuse.clone().map_or(Ok(self.granted), Err)
    }

    fn open(&mut self, samples: Samples, failed: Failed) -> Result<Recording, String> {
        self.opened += 1;
        if let Some(error) = self.fail.clone().filter(|_| self.opened > self.fail_from) {
            failed(error.clone());
            return Err(error);
        }
        self.samples.lock().unwrap().push(samples);
        let stopping = match self.fail_on_stop.clone() {
            Some(error) => FailsOnStop(Some((failed, error)), self.stopped.clone()),
            None => {
                self.failed.lock().unwrap().push(failed);
                FailsOnStop(None, self.stopped.clone())
            }
        };
        Ok(Recording::new(16_000, stopping))
    }

    fn live_session(&self) -> Option<i32> {
        self.session
    }

    fn open_live(
        &mut self,
        samples: Samples,
        failed: Failed,
        active: Active,
    ) -> Result<Recording, String> {
        self.current.lock().unwrap().push(active());
        self.open(samples, failed)
    }
}

fn scripted(capture: Scripted) -> Voice {
    Voice {
        capture: Box::new(capture),
        shared: Arc::default(),
        recording: None,
        live: false,
        generation: None,
    }
}

#[test]
fn a_recording_for_one_connection_is_not_reopened_for_another() {
    let client = client();
    let mut voice = scripted(Scripted::default());
    voice
        .begin(&client, "u1".into(), 0, None, false, Vec::new())
        .unwrap();
    assert_eq!(
        voice.open(&client, false, 1),
        Err("Connection lost; microphone stopped.".into())
    );
    // Opening again for the same connection keeps the recording.
    assert_eq!(voice.open(&client, false, 0), Ok(()));
    client.close();
}

#[test]
fn a_refused_or_failed_microphone_is_reported() {
    let client = client();
    let mut refused = scripted(Scripted {
        refuse: Some("No microphone permission.".into()),
        ..Default::default()
    });
    assert_eq!(
        refused.begin(&client, "u1".into(), 0, None, false, Vec::new()),
        Err("No microphone permission.".into())
    );
    let mut failing = scripted(Scripted {
        fail: Some("The microphone is busy.".into()),
        ..Default::default()
    });
    assert_eq!(
        failing.begin(&client, "u1".into(), 0, None, false, Vec::new()),
        Err("The microphone is busy.".into())
    );
    // What failed is the utterance's failure, though no microphone was made.
    assert_eq!(
        failing.finish(Some(&client), true),
        Err(Finish::Utterance("The microphone is busy.".into()))
    );
    client.close();
}

#[test]
fn live_capture_can_tell_whether_its_connection_is_still_current() {
    let client = client();
    let current = Arc::new(Mutex::new(Vec::new()));
    let mut voice = scripted(Scripted {
        current: current.clone(),
        ..Default::default()
    });
    voice.start_live(&client, 0).unwrap();
    // This client never connects, so no connection of it is current.
    assert_eq!(*current.lock().unwrap(), [false]);
    // Muted while live: the gate closes, and the capture stops.
    voice.abandon();
    assert!(voice.recording.is_some());
    voice.stop_live();
    assert!(voice.recording.is_none());
    client.close();
}

#[test]
fn samples_from_a_capture_for_an_earlier_connection_are_not_sent() {
    let client = client();
    let samples = Arc::new(Mutex::new(Vec::new()));
    let mut voice = scripted(Scripted {
        samples: samples.clone(),
        ..Default::default()
    });
    voice
        .begin(&client, "u1".into(), 0, None, false, Vec::new())
        .unwrap();
    voice.finish(Some(&client), false).unwrap();
    voice
        .begin(&client, "u2".into(), 1, None, false, Vec::new())
        .unwrap();
    let mut samples = samples.lock().unwrap();
    // The first capture's callback still runs, as a device may call it late: nothing reaches the second's microphone.
    samples[0](&[0.5; 320]);
    samples[1](&[0.5; 320]);
    assert_eq!(voice.lock().heard.samples, 640);
    client.close();
}

#[test]
fn what_cannot_be_sent_once_the_client_is_closed_is_reported() {
    let closed = client();
    let mut voice = scripted(Scripted::default());
    // The microphone is made for one client, which is closed while it is kept.
    voice
        .begin(&closed, "u1".into(), 0, None, false, Vec::new())
        .unwrap();
    voice.finish(Some(&closed), false).unwrap();
    closed.close();
    let open = client();
    let closed = Err("Cannot send: the client is closed".to_owned());
    assert_eq!(voice.start_live(&open, 0), closed);
    assert!(voice.recording.is_none());
    assert_eq!(
        voice.begin(&open, "u2".into(), 0, None, false, Vec::new()),
        closed
    );
    open.close();
}

#[test]
fn an_utterance_or_the_live_gate_that_cannot_be_sent_is_reported() {
    let first = client();
    let mut voice = scripted(Scripted::default());
    voice.start_live(&first, 0).unwrap();
    voice
        .begin(&first, "u1".into(), 0, None, false, Vec::new())
        .unwrap();
    let sender = first.sender();
    first.close();
    let _ = sender;
    assert_eq!(
        voice.finish(None, true),
        Err(Finish::Utterance(
            "Cannot send: the client is closed".into()
        ))
    );
    // The live gate need not close on a closed client: nothing more is sent.
    voice.stop_live();
    assert!(!voice.live);
}

#[test]
fn a_failure_while_live_stops_the_next_utterance_and_its_reopening() {
    let client = client();
    let mut voice = scripted(Scripted {
        fail: Some("The microphone stopped.".into()),
        fail_from: 1,
        ..Default::default()
    });
    voice.start_live(&client, 0).unwrap();
    voice.lock().failure = Some("Overrun.".into());
    assert_eq!(
        voice.begin(&client, "u1".into(), 0, None, false, Vec::new()),
        Err("Overrun.".into())
    );
    voice.recording = Some(Recording::new(16_000, ()));
    assert_eq!(
        voice.begin(&client, "u2".into(), 0, None, false, Vec::new()),
        Err("The microphone stopped.".into())
    );
    assert!(!voice.talking());
    client.close();
}

#[test]
fn a_live_microphone_without_its_connection_is_not_reopened_after_an_utterance() {
    let client = client();
    let mut voice = scripted(Scripted::default());
    voice.start_live(&client, 0).unwrap();
    voice
        .begin(&client, "u1".into(), 0, None, false, Vec::new())
        .unwrap();
    assert_eq!(
        voice.finish(None, true),
        Err(Finish::Live(
            "Connection lost; live microphone stopped.".into()
        ))
    );
    client.close();
}

#[test]
fn a_cancelled_utterance_that_cannot_be_cancelled_or_abandoned_says_why() {
    let first = client();
    let mut voice = scripted(Scripted::default());
    voice
        .begin(&first, "u1".into(), 0, None, false, Vec::new())
        .unwrap();
    voice.lock().failure = Some("Overrun.".into());
    first.close();
    assert_eq!(
        voice.finish(None, true),
        Err(Finish::Utterance(
            "Cannot cancel: the client is closed".into()
        ))
    );
    assert_eq!(
        String::from(Finish::Live("live".into())),
        String::from(Finish::Utterance("live".into()))
    );
}

#[test]
fn a_closed_connection_refuses_the_microphone() {
    let client = client();
    client.sender().close();
    let mut voice = scripted(Scripted::default());
    assert_eq!(
        voice.open(&client, false, 0),
        Err("Connection lost; microphone stopped.".into())
    );
}

/// Opens outputs that render the player once, or fails to open while `refuse` is set. Keeps each open output's failure callback.
#[derive(Clone, Default)]
struct FakeSpeaker {
    opened: Arc<AtomicU32>,
    closed: Arc<AtomicU32>,
    refuse: Arc<AtomicBool>,
    failed: Arc<Mutex<Option<crate::speaker::Failed>>>,
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
        player: Shared,
        failed: crate::speaker::Failed,
        _route: Route,
    ) -> Result<Output, String> {
        if self.refuse.load(Ordering::Relaxed) {
            return Err("No speaker is available.".into());
        }
        let mut output = [1; 48];
        player
            .lock()
            .unwrap()
            .render(48_000, Layout::Mono, &mut output);
        assert_eq!(output, [0; 48]);
        *self.failed.lock().unwrap() = Some(failed);
        self.opened.fetch_add(1, Ordering::Relaxed);
        Ok(Output::new(FakeOutput(self.closed.clone())))
    }
}

/// Wait for another thread, such as the speaker's.
fn wait(condition: impl Fn() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !condition() {
        assert!(Instant::now() < deadline, "timed out");
        std::thread::sleep(Duration::from_millis(1));
    }
}

/// Poll the speaker until it reports a failure.
fn failure(speaker: &mut Speaker) -> String {
    // Waits at least once, however soon the speaker fails, so that how the race went changes no coverage.
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut error = None;
    while error.is_none() {
        assert!(Instant::now() < deadline, "no failure reported");
        std::thread::sleep(Duration::from_millis(1));
        error = speaker.poll();
    }
    error.unwrap()
}

/// A player of nothing, as a client's would be before anything arrives.
fn player() -> (Shared, PlayerControl) {
    let (_audio, received) = mpsc::sync_channel(1);
    let (player, control) = Player::new(received, MixerOptions::default());
    (Arc::new(Mutex::new(player)), control)
}

#[test]
fn rerouting_the_speaker_reopens_it_only_when_the_route_changes() {
    let mut speaker = Speaker::new(Box::new(FakeSpeaker::default()));
    let route = Route {
        communication: Some(42),
    };
    // Nothing open: only remembered for the next open.
    speaker.reroute(route);
    assert_eq!(speaker.route, route);
    speaker.reroute(route);
    assert!(speaker.output.is_none());
}

#[test]
fn rerouting_an_open_speaker_reopens_it_once() {
    let fake = FakeSpeaker::default();
    let opened = fake.opened.clone();
    let mut speaker = Speaker::new(Box::new(fake));
    speaker.player = Some(player());
    speaker.open();
    wait(|| opened.load(Ordering::Relaxed) == 1);
    let route = Route {
        communication: Some(42),
    };
    speaker.reroute(route);
    wait(|| opened.load(Ordering::Relaxed) == 2);
    wait(|| speaker.output.as_ref().is_some_and(Speaking::is_open));
    // The fake opens as media, as a speaker that fell back would.
    assert_eq!(speaker.output.as_ref().unwrap().communication(), None);
    speaker.reroute(route);
    std::thread::sleep(Duration::from_millis(20));
    assert_eq!(opened.load(Ordering::Relaxed), 2);
}

/// Opens every output as voice communication in audio session 42, as Android's speaker does for the live microphone.
struct Communicating;

impl Playback for Communicating {
    fn open(
        &self,
        _player: Shared,
        _failed: crate::speaker::Failed,
        _route: Route,
    ) -> Result<Output, String> {
        Ok(Output::communicating((), 42))
    }
}

#[test]
fn a_speaker_playing_as_voice_communication_says_in_which_session() {
    let mut speaker = Speaker::new(Box::new(Communicating));
    speaker.player = Some(player());
    speaker.open();
    wait(|| speaker.output.as_ref().is_some_and(Speaking::is_open));
    assert_eq!(speaker.output.as_ref().unwrap().communication(), Some(42));
}

#[test]
fn the_speaker_opens_on_its_own_thread_and_closes_when_the_client_goes_away() {
    let mut client = client();
    let fake = FakeSpeaker::default();
    let mut speaker = Speaker::new(Box::new(fake.clone()));
    // Nothing to play before a client is attached.
    speaker.open();
    assert!(speaker.output.is_none());

    speaker.attach(&mut client);
    assert!(client.take_audio().is_none(), "the speaker took the audio");
    speaker.open();
    speaker.open();
    wait(|| fake.opened.load(Ordering::Relaxed) == 1);
    assert!(speaker.output.as_ref().unwrap().is_open());
    assert_eq!(speaker.poll(), None);

    speaker.detach();
    assert!(speaker.output.is_none() && speaker.control().is_none());
    wait(|| fake.closed.load(Ordering::Relaxed) == 1);
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
    speaker.open();
    let error = failure(&mut speaker);
    assert!(error.contains("No speaker"), "{error}");
    assert!(speaker.output.is_none());
    // Not tried again until the retry is due, and not shown again when it fails again.
    speaker.open();
    assert!(speaker.output.is_none());
    speaker.retry_at = Some(Instant::now());
    speaker.open();
    wait(|| speaker.failure.lock().unwrap().is_some());
    assert_eq!(speaker.poll(), None);
    assert!(speaker.output.is_none());

    // A new welcome tries again at once, and shows the failure again.
    speaker.connected();
    speaker.open();
    let error = failure(&mut speaker);
    assert!(error.contains("No speaker"), "{error}");

    fake.refuse.store(false, Ordering::Relaxed);
    speaker.retry_at = Some(Instant::now());
    speaker.open();
    wait(|| fake.opened.load(Ordering::Relaxed) == 1);
    assert_eq!(speaker.poll(), None);

    // The open output fails from another thread: it is closed, shown, and reopened once due.
    let failed = fake.failed.lock().unwrap().take().unwrap();
    std::thread::spawn(move || failed("The speaker failed: unplugged".into()))
        .join()
        .unwrap();
    let error = speaker.poll().expect("the failure is shown");
    assert!(error.contains("unplugged"), "{error}");
    assert!(speaker.output.is_none());
    wait(|| fake.closed.load(Ordering::Relaxed) == 1);
    speaker.open();
    speaker.retry_at = Some(Instant::now());
    speaker.open();
    wait(|| fake.opened.load(Ordering::Relaxed) == 2);
    client.close();
}

#[test]
fn a_speaker_thread_that_cannot_start_is_reported_once_and_started_again_later() {
    let mut client = client();
    let fake = FakeSpeaker::default();
    let mut speaker = Speaker::new(Box::new(fake.clone()));
    speaker.attach(&mut client);

    crate::system::refuse("tomori-speaker");
    speaker.open();
    let error = speaker.poll().expect("the failure is shown");
    assert_eq!(
        error,
        "Cannot start the speaker thread: the system refused to start tomori-speaker Replies cannot be heard."
    );
    assert!(speaker.output.is_none());
    // Not shown again when it fails again before the speaker opens.
    crate::system::refuse("tomori-speaker");
    speaker.retry_at = Some(Instant::now());
    speaker.open();
    assert_eq!(speaker.poll(), None);
    assert!(speaker.output.is_none());

    // Started again once the retry is due.
    speaker.retry_at = Some(Instant::now());
    speaker.open();
    wait(|| fake.opened.load(Ordering::Relaxed) == 1);
    assert_eq!(speaker.poll(), None);
    assert!(speaker.output.is_some());
    client.close();
}

#[test]
fn opening_the_speaker_discards_audio_queued_while_nothing_played() {
    let fake = FakeSpeaker::default();
    let mut speaker = Speaker::new(Box::new(fake.clone()));
    let (sender, audio) = mpsc::sync_channel(8);
    let (player, control) = Player::new(audio, MixerOptions::default());
    speaker.player = Some((Arc::new(Mutex::new(player)), control));
    for sequence in 0..8 {
        sender
            .send(tomori_protocol::AudioFrame {
                gate: Gate::Live,
                source: 1,
                sequence,
                timestamp: 0,
                sample_rate: 48_000,
                samples: vec![1000; 960],
            })
            .unwrap();
    }
    // The fake renders as it opens, and asserts that it hears nothing.
    speaker.open();
    wait(|| fake.opened.load(Ordering::Relaxed) == 1);
}

/// An echo test for a live microphone opened at `opened`, as it starts.
fn echo_test(routed: bool) -> EchoTest {
    EchoTest {
        label: "test",
        routed,
        armed: None,
        speaker: None,
        opened: None,
        probed: None,
        floor: Heard::default(),
        echo: Heard::default(),
    }
}

#[test]
fn the_echo_test_hears_the_room_then_the_probe_and_reports_once() {
    let mut test = echo_test(false);
    let start = Instant::now();
    let at = |milliseconds: u64| start + Duration::from_millis(milliseconds);
    let heard = |level: f64| Heard {
        energy: level * level * 320.0,
        samples: 320,
    };
    // Before the live microphone opens, nothing.
    assert_eq!(test.update(heard(1.0), at(0)), (None, false));
    test.opened = Some(start);
    // The first 300 ms are ignored, then the room is heard until the probe plays at one second.
    assert_eq!(test.update(heard(1.0), at(100)), (None, false));
    assert_eq!(test.update(heard(0.001), at(500)), (None, false));
    assert_eq!(test.update(heard(0.001), at(900)), (None, false));
    assert_eq!(test.update(Heard::default(), at(1_000)), (None, true));
    assert_eq!(test.update(heard(1.0), at(1_100)), (None, false));
    assert_eq!(test.update(heard(0.01), at(1_400)), (None, false));
    assert_eq!(test.update(heard(0.01), at(2_200)), (None, false));
    let (result, probe) = test.update(Heard::default(), at(2_400));
    assert!(!probe);
    assert_eq!(
        result.as_deref(),
        Some(
            "Echo test (test, the speaker as media): the speaker played a probe at -20.0 dBFS; the microphone heard -40.0 dBFS during it, 20.0 dB below the probe, 20.0 dB above the room; the room was -60.0 dBFS"
        )
    );
    // Once only, until the microphone opens again.
    assert_eq!(test.update(heard(1.0), at(3_000)), (None, false));
    assert_eq!(Heard::default().level(), None);
}

#[test]
fn the_echo_test_waits_for_the_speaker_to_play_as_it_should() {
    crate::test_support::log_everything();
    let start = Instant::now();
    // Not armed, as before the live microphone opens: nothing starts.
    let mut test = echo_test(true);
    test.arm(Some(Some(7)), start);
    assert!(test.opened.is_none());
    // Armed, needing the microphone's session: not while the speaker is closed or plays as media.
    test.armed = Some(start);
    test.arm(None, start);
    test.arm(Some(None), start + Duration::from_millis(100));
    assert!(test.opened.is_none() && test.armed.is_some());
    // It gives up after a while, measuring nothing.
    test.arm(Some(None), start + ECHO_WAIT);
    assert!(test.opened.is_none() && test.armed.is_none());
    // As voice communication, it starts, saying in which session.
    test.armed = Some(start);
    test.arm(Some(Some(7)), start);
    assert_eq!((test.opened, test.speaker), (Some(start), Some(7)));
    // A test that needs no session starts once the speaker is open, as media, and gives up too if it never opens.
    let mut test = echo_test(false);
    test.armed = Some(start);
    test.arm(Some(None), start);
    assert_eq!((test.opened, test.speaker), (Some(start), None));
    let mut test = echo_test(false);
    test.armed = Some(start);
    test.arm(None, start + ECHO_WAIT);
    assert!(test.opened.is_none() && test.armed.is_none());
}

/// The result of an echo test that heard `floor` from the room and `echo` from the probe, played by a speaker in session `speaker`.
fn echo_result(speaker: Option<i32>, floor: Heard, echo: Heard) -> String {
    let opened = Instant::now();
    let mut test = EchoTest {
        speaker,
        opened: Some(opened),
        probed: Some(opened),
        floor,
        echo,
        ..echo_test(false)
    };
    test.update(Heard::default(), opened + ECHO_HEARD.end)
        .0
        .expect("a result")
}

#[test]
fn the_echo_test_says_what_it_could_not_hear_and_when_the_echo_is_lost_in_the_room() {
    let heard = |level: f64| Heard {
        energy: 10_f64.powf(level / 10.0) * 100.0,
        samples: 100,
    };
    let nothing = echo_result(None, Heard::default(), Heard::default());
    assert!(nothing.contains("the speaker as media"));
    assert!(nothing.contains("the microphone heard nothing during it; the room was nothing"));
    let lost = echo_result(Some(7), heard(-50.0), heard(-49.0));
    assert!(lost.contains("as voice communication in session 7"));
    assert!(lost.contains("within 1.0 dB of the room, so at most this"));
    assert!(lost.contains("dB below the probe"));
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
                .filter(|report| !matches!(report, Report::Tick { .. } | Report::Replaying(_)))
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
    let mut session = Script::listening();
    let mut connection = connection(&mut session, Scripted::default(), Nowhere);
    connection.feedback().set_haptics(true);
    // Presence goes nowhere, and no voice is placed.
    connection.send_presence(pose(0.0), None, None, [None, None]);
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
    assert!(
        session
            .reports
            .iter()
            .all(|report| matches!(report, Report::Tick { .. } | Report::Replaying(false)))
    );
    // Words never begun end as X is released, felt, and nothing is sent or reported.
    session.push(Action::EndUtterance);
    connection.poll(true, &mut session);
    assert!(session.heard().is_empty());
    assert_eq!(felt(&mut connection), [Hand::Left]);
    assert!(
        session
            .reports
            .iter()
            .all(|report| matches!(report, Report::Tick { .. } | Report::Replaying(false)))
    );
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
        for sequence in (0..10).filter(|sequence| *sequence != 4) {
            let frame = tomori_protocol::AudioFrame {
                gate: Gate::Live,
                source: tomori_protocol::AGENT_SOURCE,
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
    assert!(heard(&peer, ServerMessage::Left, connection, session).is_empty());
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
    peer.say(ServerMessage::Left, &mut connection, &mut session);
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
            Report::Feedback(_) | Report::Tick { .. } | Report::Replaying(false)
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
    assert!(
        session
            .reports
            .iter()
            .all(|report| matches!(report, Report::Tick { .. } | Report::Replaying(_)))
    );
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

    /// A microphone that hears `samples`, once, each time it opens, then nothing until it is closed; live or for push-to-talk alike.
    struct Talker {
        samples: Vec<i16>,
    }

    /// A capture thread, stopped and joined when dropped.
    struct Talking {
        stop: Arc<AtomicBool>,
        thread: Option<JoinHandle<()>>,
    }

    impl Drop for Talking {
        fn drop(&mut self) {
            self.stop.store(true, Ordering::Relaxed);
            let _ = self.thread.take().map(JoinHandle::join);
        }
    }

    impl Capture for Talker {
        fn open(&mut self, mut samples: Samples, _failed: Failed) -> Result<Recording, String> {
            let stop = Arc::new(AtomicBool::new(false));
            // Halfway between samples, so that converting back to 16 bits gives each sample exactly.
            let speech: Vec<f32> = self
                .samples
                .iter()
                .map(|sample| (f32::from(*sample) + 0.5) / f32::from(i16::MAX))
                .collect();
            let stopped = stop.clone();
            let thread = std::thread::spawn(move || {
                for chunk in speech.chunks(320) {
                    samples(chunk);
                }
                while !stopped.load(Ordering::Relaxed) {
                    std::thread::sleep(Duration::from_millis(5));
                }
            });
            Ok(Recording::new(
                16_000,
                Talking {
                    stop,
                    thread: Some(thread),
                },
            ))
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
        connection.send_presence(pose(0.0), Some(pose(-0.2)), Some(pose(0.2)), [None, None]);
        let (left, right) = bea.wait_for(
            || connection.poll(true, &mut session),
            |message| match message {
                ServerMessage::Presence { left, right, .. } => Some((*left, *right)),
                _ => None,
            },
        );
        assert_eq!(left.map(|pose| pose.position[0]), Some(-0.2));
        assert_eq!(right.map(|pose| pose.position[0]), Some(0.2));
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

    /// Pair as the participant does, the operator confirming the code on the server's console, and wait for `done`.
    fn pair(
        server: &Server,
        connection: &mut Connection,
        session: &mut Script,
        done: impl FnMut(&mut Connection, &mut Script) -> bool,
    ) {
        session.pair();
        let pairing = server.pairing.as_ref().unwrap();
        let mut code = None;
        until(connection, session, |_, _| {
            code = pairing.pending().and_then(|pending| pending.code);
            code.is_some()
        });
        let digits: String = code.unwrap().chars().filter(char::is_ascii_digit).collect();
        assert_eq!(pairing.confirm(&digits), Ok("Ada".to_owned()));
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
