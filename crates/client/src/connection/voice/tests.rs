// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

//! Tests of the connection's microphone.
use super::*;
use crate::{
    capture::{Active, Failed, Samples},
    connection::test_support::*,
};
use std::{
    sync::{
        atomic::{AtomicBool, AtomicU32, Ordering},
        mpsc,
    },
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
    voice.begin(&client, Utterance::new("u1", 0)).unwrap();
    std::thread::sleep(Duration::from_millis(20));
    assert_eq!(utterance(&voice).as_deref(), Some("u1"));
    voice.finish(Some(&client), true).unwrap();
    assert!(voice.recording.is_none());
    assert_eq!(utterance(&voice), None);

    voice.begin(&client, Utterance::new("u2", 0)).unwrap();
    std::thread::sleep(Duration::from_millis(5));
    voice.abandon();
    assert_eq!(utterance(&voice), None);

    // Words that say a new name, or the space's title, are recorded the same way.
    voice
        .begin(&client, Utterance::new("u3", 0).naming())
        .unwrap();
    voice.reset();
    voice
        .begin(&client, Utterance::new("u4", 0).titling())
        .unwrap();
    assert_eq!(utterance(&voice).as_deref(), Some("u4"));
    voice.reset();
    assert!(voice.lock().microphone.is_none());
    client.close();
}

#[test]
fn a_new_rate_gets_a_new_microphone() {
    let client = client();
    let rate = Arc::new(AtomicU32::new(16_000));
    let mut voice = voice(&rate, true, None);
    voice.begin(&client, Utterance::new("u1", 0)).unwrap();
    voice.finish(Some(&client), false).unwrap();
    rate.store(48_000, Ordering::Relaxed);
    voice.begin(&client, Utterance::new("u2", 0)).unwrap();
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
    let error = voice.begin(&client, Utterance::new("u1", 0)).unwrap_err();
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
    voice.begin(&client, Utterance::new("u1", 0)).unwrap();
    assert!(voice.talking());
    assert_eq!(
        voice.lock().microphone.as_ref().unwrap().gate(),
        Gate::Agent
    );
    voice.finish(Some(&client), true).unwrap();
    assert!(voice.recording.is_some());
    assert_eq!(voice.lock().microphone.as_ref().unwrap().gate(), Gate::Live);
    voice.begin(&client, Utterance::new("u2", 0)).unwrap();
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
            .contains("click the left stick")
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
            .begin(&client, Utterance::new("u1", 0))
            .unwrap_err()
            .contains("hold X again")
    );
    assert_eq!(*calls.lock().unwrap(), ["request", "permit"]);
    // Allowed, the next recording opens the microphone.
    voice.begin(&client, Utterance::new("u2", 0)).unwrap();
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
    voice.begin(&client, Utterance::new("u1", 0)).unwrap();
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
    voice.begin(&client, Utterance::new("u2", 0)).unwrap();
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
    voice.begin(&client, Utterance::new("u1", 0)).unwrap();
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
    voice.begin(&client, Utterance::new("u1", 0)).unwrap();
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
    voice.begin(&client, Utterance::new("u1", 0)).unwrap();
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
    assert!(voice.begin(&client, Utterance::new("u1", 1)).is_err());
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
    voice.begin(&client, Utterance::new("u1", 0)).unwrap();
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
    voice.begin(&client, Utterance::new("u1", 0)).unwrap();
    assert_eq!(
        voice.finish(Some(&client), true).unwrap_err(),
        Finish::Utterance("read failed while stopping".into())
    );
    assert_eq!(utterance(&voice), None);
    assert_eq!(attempts.load(Ordering::Relaxed), 2);
    voice.stop_live();
    client.close();
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
    voice.begin(&client, Utterance::new("u1", 0)).unwrap();
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
    crate::test_support::log_everything();
    let client = client();
    let mut refused = scripted(Scripted {
        refuse: Some("No microphone permission.".into()),
        ..Default::default()
    });
    assert_eq!(
        refused.begin(&client, Utterance::new("u1", 0)),
        Err("No microphone permission.".into())
    );
    let mut failing = scripted(Scripted {
        fail: Some("The microphone is busy.".into()),
        ..Default::default()
    });
    assert_eq!(
        failing.begin(&client, Utterance::new("u1", 0)),
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
    voice.begin(&client, Utterance::new("u1", 0)).unwrap();
    voice.finish(Some(&client), false).unwrap();
    voice.begin(&client, Utterance::new("u2", 1)).unwrap();
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
    voice.begin(&closed, Utterance::new("u1", 0)).unwrap();
    voice.finish(Some(&closed), false).unwrap();
    closed.close();
    let open = client();
    let closed = Err("Cannot send: the client is closed".to_owned());
    assert_eq!(voice.start_live(&open, 0), closed);
    assert!(voice.recording.is_none());
    assert_eq!(voice.begin(&open, Utterance::new("u2", 0)), closed);
    open.close();
}

#[test]
fn an_utterance_or_the_live_gate_that_cannot_be_sent_is_reported() {
    let first = client();
    let mut voice = scripted(Scripted::default());
    voice.start_live(&first, 0).unwrap();
    voice.begin(&first, Utterance::new("u1", 0)).unwrap();
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
        voice.begin(&client, Utterance::new("u1", 0)),
        Err("Overrun.".into())
    );
    voice.recording = Some(Recording::new(16_000, ()));
    assert_eq!(
        voice.begin(&client, Utterance::new("u2", 0)),
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
    voice.begin(&client, Utterance::new("u1", 0)).unwrap();
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
    voice.begin(&first, Utterance::new("u1", 0)).unwrap();
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
