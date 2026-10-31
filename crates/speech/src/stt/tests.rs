// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

use std::{
    sync::{Arc, Mutex},
    time::Duration,
};

use super::{policy::Policy, *};

const BASE: i16 = 1000;
const TICK: usize = RATE as usize / 100;

/// Audio whose samples encode their own position in 10 ms ticks, so that the fake recognizer knows which part of the utterance it was given.
fn audio(seconds: f32) -> Vec<i16> {
    (0..(seconds * RATE as f32) as usize)
        .map(|index| BASE + (index / TICK) as i16)
        .collect()
}

/// A scripted recognizer: returns the script's words that lie within the window. Words ending within `unstable` seconds of the end of the window are misheard differently on each call, as Whisper does with cut-off speech, unless the window reaches the end of the script's audio.
#[derive(Clone)]
struct Scripted {
    words: Vec<(&'static str, f32, f32)>,
    unstable: f32,
    calls: Arc<Mutex<Vec<Call>>>,
    fail_on: Option<usize>,
}

#[derive(Clone, Debug)]
struct Call {
    start: f32,
    length: f32,
    prompt: Option<String>,
}

impl Scripted {
    fn new(words: Vec<(&'static str, f32, f32)>) -> Self {
        Self {
            words,
            unstable: 0.5,
            calls: Arc::default(),
            fail_on: None,
        }
    }

    fn calls(&self) -> Vec<Call> {
        self.calls.lock().unwrap().clone()
    }
}

impl Recognizer for Scripted {
    fn decode(&mut self, window: &[i16], prompt: Option<&str>) -> Result<Vec<Word>, Error> {
        let mut calls = self.calls.lock().unwrap();
        let start = window
            .iter()
            .find(|&&sample| sample >= BASE)
            .map_or(0.0, |&sample| f32::from(sample - BASE) / 100.0);
        let length = window.len() as f32 / RATE as f32;
        calls.push(Call {
            start,
            length,
            prompt: prompt.map(str::to_owned),
        });
        if self.fail_on == Some(calls.len()) {
            return Err(Error::Engine("engine failed".into()));
        }
        let end =
            start + window.iter().filter(|&&sample| sample >= BASE).count() as f32 / RATE as f32;
        Ok(self
            .words
            .iter()
            .filter(|(_, word_start, word_end)| {
                *word_start >= start - 0.005 && *word_end <= end + 0.005
            })
            .map(|&(text, word_start, word_end)| {
                let total = self.words.last().map_or(0.0, |word| word.2 + 0.05);
                let text = if word_end > end - self.unstable && end < total - 0.01 {
                    format!(" {text}{}", "x".repeat(calls.len()))
                } else {
                    format!(" {text}")
                };
                Word {
                    text,
                    start: word_start - start,
                    end: word_end - start,
                }
            })
            .collect())
    }
}

/// Ten words, 0.4 s each with 0.1 s gaps, over five seconds.
fn sentence() -> Vec<(&'static str, f32, f32)> {
    [
        "the", "quick", "brown", "fox", "jumps", "over", "the", "lazy", "dog", "today",
    ]
    .into_iter()
    .enumerate()
    .map(|(index, word)| (word, index as f32 * 0.5 + 0.05, index as f32 * 0.5 + 0.45))
    .collect()
}

#[test]
fn commits_agreed_words_and_finishes_with_only_the_tail() {
    let mut recognizer = Scripted::new(sentence());
    let mut policy = Policy::new(RecognizerConfig::default());
    let samples = audio(5.0);
    let mut partials = Vec::new();
    for chunk in samples.chunks(320) {
        policy.push(chunk);
        if policy.due() {
            policy.decode(&mut recognizer).unwrap();
            partials.push(policy.transcript());
        }
    }
    // Partial transcripts arrive while speaking, and commit grows monotonically.
    assert!(partials.len() >= 4);
    assert!(partials.iter().any(|partial| !partial.committed.is_empty()));
    for pair in partials.windows(2) {
        assert!(pair[1].committed.starts_with(&pair[0].committed));
    }
    let committed_before_end = policy.transcript().committed;
    let tail_start = policy.window_start() as f32 / RATE as f32;
    assert!(tail_start > 2.0, "{tail_start}");

    let text = policy.finish(&mut recognizer).unwrap();
    assert_eq!(text, "the quick brown fox jumps over the lazy dog today");

    // The final decode only covered the uncommitted tail, with the committed text as its prompt.
    let last = recognizer.calls().pop().unwrap();
    assert!((last.start - tail_start).abs() < 0.02, "{last:?}");
    assert!(last.length < 5.0 - 2.0);
    assert_eq!(last.prompt.as_deref(), Some(committed_before_end.as_str()));
}

#[test]
fn unstable_words_are_tentative_until_two_hypotheses_agree() {
    let mut recognizer = Scripted::new(sentence());
    recognizer.unstable = 0.0;
    let mut policy = Policy::new(RecognizerConfig::default());
    let samples = audio(5.0);
    policy.push(&samples[..RATE as usize]);
    policy.decode(&mut recognizer).unwrap();
    // The first hypothesis has nothing to agree with.
    assert_eq!(
        policy.transcript(),
        Transcript {
            committed: String::new(),
            tentative: "the quick".into()
        }
    );
    policy.push(&samples[RATE as usize..RATE as usize * 2]);
    policy.decode(&mut recognizer).unwrap();
    assert_eq!(
        policy.transcript(),
        Transcript {
            committed: "the quick".into(),
            tentative: "brown fox".into()
        }
    );
}

#[test]
fn forces_a_commit_when_a_full_window_never_agrees() {
    let mut recognizer = Scripted::new(sentence());
    // Every word is misheard differently on each call.
    recognizer.unstable = 100.0;
    let config = RecognizerConfig {
        maximum_window: Duration::from_secs(3),
        force_margin: Duration::from_secs(1),
        ..RecognizerConfig::default()
    };
    let mut policy = Policy::new(config);
    for chunk in audio(5.0).chunks(320) {
        policy.push(chunk);
        if policy.due() {
            policy.decode(&mut recognizer).unwrap();
        }
    }
    assert!(!policy.transcript().committed.is_empty());
    assert!(policy.window_start() > 0);
}

#[test]
fn fails_rather_than_discarding_speech_without_a_safe_boundary() {
    // One long word fills the window, so no word ends before the margin.
    let mut recognizer = Scripted::new(vec![("aaaah", 0.0, 4.0)]);
    recognizer.unstable = 100.0;
    let config = RecognizerConfig {
        maximum_window: Duration::from_secs(3),
        force_margin: Duration::from_secs(1),
        ..RecognizerConfig::default()
    };
    let mut policy = Policy::new(config);
    let mut result = Ok(());
    for chunk in audio(4.0).chunks(320) {
        policy.push(chunk);
        if policy.due() {
            result = policy.decode(&mut recognizer);
            if result.is_err() {
                break;
            }
        }
    }
    assert_eq!(result, Err(Error::Overflow));
}

#[test]
fn silence_is_not_sent_to_the_recognizer() {
    let mut recognizer = Scripted::new(sentence());
    let mut policy = Policy::new(RecognizerConfig::default());
    policy.push(&vec![0; RATE as usize * 3]);
    while policy.due() {
        policy.decode(&mut recognizer).unwrap();
    }
    assert_eq!(policy.finish(&mut recognizer).unwrap(), "");
    assert!(recognizer.calls().is_empty());
}

fn collect(recognition: &mut Recognition, id: u64) -> (Utterance, mpsc::Receiver<TranscriptEvent>) {
    let (sender, receiver) = mpsc::channel();
    let utterance = recognition.begin(id, RATE, move |event| {
        let _ = sender.send(event);
    });
    (utterance, receiver)
}

fn final_event(receiver: &mpsc::Receiver<TranscriptEvent>) -> (Vec<Transcript>, TranscriptEvent) {
    let mut partials = Vec::new();
    loop {
        match receiver
            .recv_timeout(Duration::from_secs(5))
            .expect("an event")
        {
            TranscriptEvent::Partial(transcript) => partials.push(transcript),
            other => return (partials, other),
        }
    }
}

#[test]
fn service_sends_partials_during_the_utterance_and_a_final_after_it() {
    let recognizer = Scripted::new(sentence());
    let mut recognition = Recognition::spawn(recognizer.clone(), RecognizerConfig::default());
    let (mut utterance, receiver) = collect(&mut recognition, 1);
    let samples = audio(5.0);
    let (first, rest) = samples.split_at(RATE as usize * 3);
    for chunk in first.chunks(320) {
        utterance.push(chunk);
    }
    // A partial arrives before the utterance ends.
    match receiver.recv_timeout(Duration::from_secs(5)).unwrap() {
        TranscriptEvent::Partial(transcript) => assert!(!transcript.tentative.is_empty()),
        other => panic!("unexpected {other:?}"),
    }
    for chunk in rest.chunks(320) {
        utterance.push(chunk);
    }
    utterance.end();
    let (_, last) = final_event(&receiver);
    assert_eq!(
        last,
        TranscriptEvent::Final("the quick brown fox jumps over the lazy dog today".into())
    );
    assert!(receiver.recv_timeout(Duration::from_millis(100)).is_err());
}

#[test]
fn service_reports_recognizer_failure_for_the_utterance() {
    let mut recognizer = Scripted::new(sentence());
    // Audio pushed before the engine runs is coalesced, so the first decode may be the final one.
    recognizer.fail_on = Some(1);
    // Utterances shorter than the script end mid-script; their last words are not cut off.
    recognizer.unstable = 0.0;
    let mut recognition = Recognition::spawn(recognizer, RecognizerConfig::default());
    let (mut utterance, receiver) = collect(&mut recognition, 1);
    for chunk in audio(5.0).chunks(320) {
        utterance.push(chunk);
    }
    utterance.end();
    let (_, last) = final_event(&receiver);
    assert_eq!(
        last,
        TranscriptEvent::Failed(Error::Engine("engine failed".into()))
    );
    // The utterance is finished: nothing else is delivered.
    assert!(receiver.recv_timeout(Duration::from_millis(100)).is_err());

    // The next utterance uses the restarted recognizer.
    let (mut utterance, receiver) = collect(&mut recognition, 2);
    for chunk in audio(2.0).chunks(320) {
        utterance.push(chunk);
    }
    utterance.end();
    let (_, last) = final_event(&receiver);
    assert_eq!(last, TranscriptEvent::Final("the quick brown fox".into()));
}

#[test]
fn service_handles_concurrent_and_cancelled_utterances() {
    let mut recognizer = Scripted::new(sentence());
    recognizer.unstable = 0.0;
    let mut recognition = Recognition::spawn(recognizer, RecognizerConfig::default());
    let (mut first, first_events) = collect(&mut recognition, 1);
    let (mut second, second_events) = collect(&mut recognition, 2);
    let (mut third, third_events) = collect(&mut recognition, 3);
    let long = audio(5.0);
    let short = audio(1.5);
    for (index, chunk) in long.chunks(320).enumerate() {
        first.push(chunk);
        third.push(chunk);
        if let Some(chunk) = short.chunks(320).nth(index) {
            second.push(chunk);
        }
    }
    third.cancel();
    second.end();
    first.end();
    assert_eq!(
        final_event(&second_events).1,
        TranscriptEvent::Final("the quick brown".into())
    );
    assert_eq!(
        final_event(&first_events).1,
        TranscriptEvent::Final("the quick brown fox jumps over the lazy dog today".into())
    );
    // A cancelled utterance may have produced partials, but never a result.
    while let Ok(event) = third_events.recv_timeout(Duration::from_millis(100)) {
        assert!(matches!(event, TranscriptEvent::Partial(_)), "{event:?}");
    }
}

#[test]
fn service_resamples_to_sixteen_kilohertz() {
    let recognizer = Scripted::new(sentence());
    let mut recognition = Recognition::spawn(recognizer.clone(), RecognizerConfig::default());
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
    drop(recognition);
    assert!(recognizer.calls().is_empty());
}
