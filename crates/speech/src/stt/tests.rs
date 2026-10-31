// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

use std::{
    sync::{
        Arc, Condvar, Mutex,
        atomic::{AtomicBool, AtomicUsize},
    },
    time::{Duration, Instant},
};

use super::*;

/// Words the scripted recognizer knows. Each is "spoken" as [`WORD`] samples of `BASE + index`; any other sample is silence.
const VOCABULARY: &[&str] = &["run", "the", "tests", "please", "fail", "crash"];
const BASE: i16 = 1000;
/// 100 ms per word.
const WORD: usize = RATE as usize / 10;

/// Audio that [`Scripted`] hears as `words`.
fn spoken(words: &[&str]) -> Vec<i16> {
    words
        .iter()
        .flat_map(|word| {
            let index = VOCABULARY.iter().position(|known| known == word).unwrap();
            std::iter::repeat_n(BASE + index as i16, WORD)
        })
        .collect()
}

/// A gate that holds decoding until it is opened, and reports when a decode is waiting at it.
#[derive(Default)]
struct Gate {
    state: Mutex<(bool, bool)>,
    changed: Condvar,
}

impl Gate {
    fn closed() -> Arc<Self> {
        Arc::default()
    }

    fn pass(&self) {
        let mut state = self.state.lock().unwrap();
        state.1 = true;
        self.changed.notify_all();
        while !state.0 {
            state = self.changed.wait(state).unwrap();
        }
    }

    /// Wait until a decode is held at the gate.
    fn wait_for_decode(&self) {
        let mut state = self.state.lock().unwrap();
        while !state.1 {
            state = self.changed.wait(state).unwrap();
        }
    }

    fn open(&self) {
        self.state.lock().unwrap().0 = true;
        self.changed.notify_all();
    }
}

/// How the scripted recognizer behaves, shared by its streams.
#[derive(Default)]
struct Script {
    /// Opening a stream fails.
    refuse: bool,
    /// Streams are never due, so audio is only decoded when the utterance finishes.
    lazy: bool,
    /// Decoding waits at this gate until it opens.
    gate: Option<Arc<Gate>>,
    /// Samples accepted by every stream.
    accepted: AtomicUsize,
    /// Set when the recognizer is dropped.
    unloaded: AtomicBool,
}

/// A [`StreamingRecognizer`] that hears [`VOCABULARY`] words from their sample values, a whole word at a time. "fail" fails decoding; "crash" panics.
#[derive(Clone, Default)]
struct Scripted(Arc<Script>);

impl Scripted {
    fn new(script: Script) -> Self {
        Self(Arc::new(script))
    }
}

impl Drop for Scripted {
    fn drop(&mut self) {
        self.0.unloaded.store(true, Ordering::SeqCst);
    }
}

impl StreamingRecognizer for Scripted {
    fn open(&mut self) -> Result<Box<dyn RecognitionStream>, Error> {
        if self.0.refuse {
            return Err(Error::Unavailable("the model is missing".into()));
        }
        Ok(Box::new(Stream {
            script: Arc::clone(&self.0),
            audio: Vec::new(),
            decoded: 0,
            words: Vec::new(),
        }))
    }
}

struct Stream {
    script: Arc<Script>,
    audio: Vec<i16>,
    decoded: usize,
    words: Vec<&'static str>,
}

impl Stream {
    fn decode_words(&mut self) -> Result<(), Error> {
        if let Some(gate) = &self.script.gate {
            gate.pass();
        }
        while self.audio.len() - self.decoded >= WORD {
            let value = self.audio[self.decoded];
            self.decoded += WORD;
            let word = usize::try_from(value - BASE)
                .ok()
                .and_then(|index| VOCABULARY.get(index));
            match word {
                Some(&"fail") => return Err(Error::Engine("engine failed".into())),
                Some(&"crash") => panic!("the recognizer crashed"),
                Some(&word) => self.words.push(word),
                None => {}
            }
        }
        Ok(())
    }
}

impl RecognitionStream for Stream {
    fn accept(&mut self, samples: &[i16]) {
        self.script
            .accepted
            .fetch_add(samples.len(), Ordering::SeqCst);
        self.audio.extend_from_slice(samples);
    }

    fn due(&self) -> bool {
        !self.script.lazy && self.audio.len() - self.decoded >= WORD
    }

    fn decode(&mut self) -> Result<(), Error> {
        self.decode_words()
    }

    fn transcript(&self) -> Transcript {
        match self.words.split_last() {
            Some((last, rest)) => Transcript {
                committed: rest.join(" "),
                tentative: (*last).to_owned(),
            },
            None => Transcript::default(),
        }
    }

    fn finish(&mut self) -> Result<String, Error> {
        self.decode_words()?;
        Ok(self.words.join(" "))
    }
}

fn collect(recognition: &mut Recognition, id: u64) -> (Utterance, mpsc::Receiver<TranscriptEvent>) {
    let (sender, receiver) = mpsc::channel();
    let utterance = recognition.begin(id, RATE, move |event| {
        let _ = sender.send(event);
    });
    (utterance, receiver)
}

fn speak(utterance: &mut Utterance, words: &[&str]) {
    for frame in spoken(words).chunks(320) {
        utterance.push(frame);
    }
}

fn next(receiver: &mpsc::Receiver<TranscriptEvent>) -> TranscriptEvent {
    receiver
        .recv_timeout(Duration::from_secs(5))
        .expect("an event")
}

/// The partials up to the last event, and the last event.
fn final_event(receiver: &mpsc::Receiver<TranscriptEvent>) -> (Vec<Transcript>, TranscriptEvent) {
    let mut partials = Vec::new();
    loop {
        match next(receiver) {
            TranscriptEvent::Partial(transcript) => partials.push(transcript),
            other => return (partials, other),
        }
    }
}

fn assert_finished(receiver: &mpsc::Receiver<TranscriptEvent>) {
    assert!(receiver.recv_timeout(Duration::from_millis(100)).is_err());
}

#[test]
fn sends_partials_during_the_utterance_and_a_final_after_it() {
    let mut recognition = Recognition::spawn(Scripted::default());
    let (mut utterance, receiver) = collect(&mut recognition, 1);
    speak(&mut utterance, &["run", "the"]);
    // A partial arrives before the utterance ends.
    match next(&receiver) {
        TranscriptEvent::Partial(transcript) => assert!(!transcript.tentative.is_empty()),
        other => panic!("unexpected {other:?}"),
    }
    speak(&mut utterance, &["tests"]);
    utterance.end();
    let (_, last) = final_event(&receiver);
    assert_eq!(last, TranscriptEvent::Final("run the tests".into()));
    assert_finished(&receiver);
}

#[test]
fn an_utterance_that_cannot_start_fails() {
    let mut recognition = Recognition::spawn(Scripted::new(Script {
        refuse: true,
        ..Script::default()
    }));
    let (mut utterance, receiver) = collect(&mut recognition, 1);
    speak(&mut utterance, &["run"]);
    utterance.end();
    assert_eq!(
        next(&receiver),
        TranscriptEvent::Failed(Error::Unavailable("the model is missing".into()))
    );
    assert_finished(&receiver);
}

#[test]
fn a_decode_failure_fails_only_its_own_utterance() {
    let mut recognition = Recognition::spawn(Scripted::default());
    let (mut failing, failing_events) = collect(&mut recognition, 1);
    let (mut other, other_events) = collect(&mut recognition, 2);
    speak(&mut other, &["run", "the"]);
    speak(&mut failing, &["please", "fail", "please"]);
    assert_eq!(
        final_event(&failing_events).1,
        TranscriptEvent::Failed(Error::Engine("engine failed".into()))
    );
    // The failed utterance is finished: nothing else is delivered, even after it ends.
    failing.end();
    assert_finished(&failing_events);
    speak(&mut other, &["tests"]);
    other.end();
    assert_eq!(
        final_event(&other_events).1,
        TranscriptEvent::Final("run the tests".into())
    );
}

#[test]
fn a_finish_failure_fails_only_its_own_utterance() {
    let mut recognition = Recognition::spawn(Scripted::new(Script {
        lazy: true,
        ..Script::default()
    }));
    let (mut failing, failing_events) = collect(&mut recognition, 1);
    let (mut other, other_events) = collect(&mut recognition, 2);
    speak(&mut failing, &["please", "fail"]);
    speak(&mut other, &["run", "the", "tests"]);
    failing.end();
    other.end();
    assert_eq!(
        final_event(&failing_events).1,
        TranscriptEvent::Failed(Error::Engine("engine failed".into()))
    );
    assert_eq!(
        final_event(&other_events).1,
        TranscriptEvent::Final("run the tests".into())
    );
}

#[test]
fn a_cancelled_utterance_never_reports_a_result() {
    let mut recognition = Recognition::spawn(Scripted::default());
    let (mut cancelled, cancelled_events) = collect(&mut recognition, 1);
    let (mut other, other_events) = collect(&mut recognition, 2);
    speak(&mut cancelled, &["run", "the"]);
    speak(&mut other, &["run"]);
    cancelled.cancel();
    other.end();
    assert_eq!(
        final_event(&other_events).1,
        TranscriptEvent::Final("run".into())
    );
    // A cancelled utterance may have produced partials, but never a result.
    while let Ok(event) = cancelled_events.recv_timeout(Duration::from_millis(100)) {
        assert!(matches!(event, TranscriptEvent::Partial(_)), "{event:?}");
    }
}

#[test]
fn finishing_takes_priority_over_partials() {
    let gate = Gate::closed();
    let mut recognition = Recognition::spawn(Scripted::new(Script {
        gate: Some(Arc::clone(&gate)),
        ..Script::default()
    }));
    let order = Arc::new(Mutex::new(Vec::new()));
    let record = |name: &'static str| {
        let order = Arc::clone(&order);
        move |event: TranscriptEvent| order.lock().unwrap().push((name, event))
    };
    let mut long = recognition.begin(1, RATE, record("long"));
    speak(&mut long, &["run"]);
    // The long utterance's first decode is held while more of it arrives and a short utterance is spoken and ended.
    gate.wait_for_decode();
    speak(&mut long, &["the", "tests", "please"]);
    let mut short = recognition.begin(2, RATE, record("short"));
    speak(&mut short, &["run", "the"]);
    short.end();
    gate.open();
    let deadline = Instant::now() + Duration::from_secs(5);
    while order.lock().unwrap().len() < 3 {
        assert!(Instant::now() < deadline, "{:?}", order.lock().unwrap());
        thread::sleep(Duration::from_millis(10));
    }
    long.end();
    let order = order.lock().unwrap();
    // The decode that was held reports its partial; then the short utterance finishes before the long one decodes again.
    assert!(
        matches!(&order[0], ("long", TranscriptEvent::Partial(_))),
        "{order:?}"
    );
    assert_eq!(
        order[1],
        ("short", TranscriptEvent::Final("run the".into())),
        "{order:?}"
    );
}

#[test]
fn an_utterance_that_falls_behind_fails() {
    let gate = Gate::closed();
    let mut recognition = Recognition::spawn(Scripted::new(Script {
        gate: Some(Arc::clone(&gate)),
        ..Script::default()
    }));
    let (mut utterance, receiver) = collect(&mut recognition, 1);
    speak(&mut utterance, &["run"]);
    gate.wait_for_decode();
    // Recognition is stuck, and more than the backlog arrives meanwhile.
    let words = vec!["the"; BACKLOG / WORD + 10];
    speak(&mut utterance, &words);
    gate.open();
    assert_eq!(
        final_event(&receiver).1,
        TranscriptEvent::Failed(Error::Behind)
    );
    utterance.end();
    assert_finished(&receiver);
}

#[test]
fn an_utterance_that_keeps_up_may_be_longer_than_the_backlog() {
    let mut recognition = Recognition::spawn(Scripted::default());
    let (mut utterance, receiver) = collect(&mut recognition, 1);
    let second = vec!["the"; RATE as usize / WORD];
    let seconds = BACKLOG / RATE as usize + 3;
    for count in 1..=seconds {
        speak(&mut utterance, &second);
        // Wait until this second has been decoded.
        loop {
            match next(&receiver) {
                TranscriptEvent::Partial(transcript)
                    if transcript.committed.split_whitespace().count() + 1
                        == count * second.len() =>
                {
                    break;
                }
                TranscriptEvent::Partial(_) => {}
                other => panic!("unexpected {other:?}"),
            }
        }
    }
    utterance.end();
    match final_event(&receiver).1 {
        TranscriptEvent::Final(text) => {
            assert_eq!(text.split_whitespace().count(), seconds * second.len())
        }
        other => panic!("unexpected {other:?}"),
    }
}

#[test]
fn resamples_to_sixteen_kilohertz() {
    let recognizer = Scripted::default();
    let script = Arc::clone(&recognizer.0);
    let mut recognition = Recognition::spawn(recognizer);
    let (sender, receiver) = mpsc::channel();
    let mut utterance = recognition.begin(1, 48_000, move |event| {
        let _ = sender.send(event);
    });
    utterance.push_frame(&AudioFrame {
        gate: protocol::Gate::Agent,
        source: 0,
        sequence: 0,
        timestamp: 0,
        sample_rate: 48_000,
        samples: vec![0; 48_000 * 2],
    });
    utterance.end();
    assert_eq!(
        final_event(&receiver).1,
        TranscriptEvent::Final(String::new())
    );
    let accepted = script.accepted.load(Ordering::SeqCst);
    assert!(
        accepted.abs_diff(RATE as usize * 2) < 100,
        "{accepted} samples"
    );
}

#[test]
fn a_panic_disables_recognition_and_fails_every_utterance() {
    let mut recognition = Recognition::spawn(Scripted::default());
    let (mut waiting, waiting_events) = collect(&mut recognition, 1);
    speak(&mut waiting, &["run"]);
    assert!(matches!(next(&waiting_events), TranscriptEvent::Partial(_)));
    let (mut crashing, crashing_events) = collect(&mut recognition, 2);
    speak(&mut crashing, &["crash"]);
    let stopped = TranscriptEvent::Failed(Error::Stopped(UNAVAILABLE.into()));
    assert_eq!(final_event(&crashing_events).1, stopped);
    // The utterance that was waiting fails too, rather than waiting forever.
    assert_eq!(final_event(&waiting_events).1, stopped);
    // Later utterances fail at once.
    let (mut later, later_events) = collect(&mut recognition, 3);
    speak(&mut later, &["run"]);
    later.end();
    assert_eq!(next(&later_events), stopped);
}

#[test]
fn stopping_fails_open_utterances_unloads_the_recognizer_and_exits() {
    let recognizer = Scripted::default();
    let script = Arc::clone(&recognizer.0);
    let mut recognition = Recognition::spawn(recognizer);
    let (mut utterance, receiver) = collect(&mut recognition, 1);
    speak(&mut utterance, &["run"]);
    assert!(matches!(next(&receiver), TranscriptEvent::Partial(_)));
    let stopper = recognition.stopper();
    stopper.stop();
    assert_eq!(
        next(&receiver),
        TranscriptEvent::Failed(Error::Stopped("the server is shutting down".into()))
    );
    assert!(stopper.wait(Instant::now() + Duration::from_secs(5)));
    assert!(script.unloaded.load(Ordering::SeqCst));
    // A later utterance's sink is dropped without an event, which the server reports as a failure.
    let (_later, later_events) = collect(&mut recognition, 2);
    assert_eq!(
        later_events.recv_timeout(Duration::from_secs(5)),
        Err(mpsc::RecvTimeoutError::Disconnected)
    );
}
