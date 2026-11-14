// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

use super::*;
use crate::stt::MAX_SEGMENT;
use std::sync::mpsc;

/// A fake offline recognizer that returns `text` for every clip.
struct Fake(String);

impl OfflineRecognizer for Fake {
    fn transcribe(&mut self, _samples: &[i16]) -> Result<String, Error> {
        Ok(self.0.clone())
    }
}

#[test]
fn decodes_the_buffered_clip_when_it_ends() {
    let (sender, receiver) = mpsc::channel();
    let mut offline = OfflineRecognition::spawn(Fake("the final words".to_owned()));
    let mut handle = offline.begin(RATE, move |event| {
        let _ = sender.send(event);
    });
    // No event is delivered while the participant is speaking.
    handle.push(&vec![0; 1600]);
    assert!(receiver.try_recv().is_err());
    handle.end();
    let event = receiver.recv().unwrap();
    assert!(
        matches!(&event, TranscriptEvent::Final(text) if text == "the final words"),
        "expected Final, got {event:?}"
    );
}

#[test]
fn a_cancelled_utterance_reports_nothing() {
    let mut offline = OfflineRecognition::spawn(Fake("hello".to_owned()));
    let handle = offline.begin(RATE, |event| unreachable!("reported {event:?}"));
    handle.cancel();
    // Reporting it would have disabled recognition.
    let (sender, receiver) = mpsc::channel();
    let handle = offline.begin(RATE, move |event| {
        let _ = sender.send(event);
    });
    handle.end();
    assert_eq!(
        receiver.recv().unwrap(),
        TranscriptEvent::Final(String::new())
    );
}

#[test]
fn an_empty_clip_is_an_empty_transcript() {
    let (sender, receiver) = mpsc::channel();
    let mut offline = OfflineRecognition::spawn(Fake("hello".to_owned()));
    let handle = offline.begin(RATE, move |event| {
        let _ = sender.send(event);
    });
    handle.end();
    let event = receiver.recv().unwrap();
    assert!(
        matches!(&event, TranscriptEvent::Final(text) if text.is_empty()),
        "expected an empty Final, got {event:?}"
    );
}

#[test]
fn stopping_fails_open_utterances() {
    let (sender, receiver) = mpsc::channel();
    let mut offline = OfflineRecognition::spawn(Fake("hello".to_owned()));
    let handle = offline.begin(RATE, move |event| {
        let _ = sender.send(event);
    });
    let stopper = offline.stopper();
    stopper.stop();
    assert!(stopper.wait(std::time::Instant::now() + std::time::Duration::from_secs(5)));
    drop(handle);
    let event = receiver.recv().unwrap();
    let TranscriptEvent::Failed(Error::Stopped(_)) = &event else {
        unreachable!("expected Failed(Stopped), got {event:?}");
    };
}

use std::{
    ops::Range,
    sync::{Arc, Mutex},
    time::Duration,
};

/// How long a test waits for an event, and a gated decode for the test to release it, so that a test whose expectation fails while a decode is held fails rather than hangs.
const WAIT: Duration = Duration::from_secs(10);

/// A fake offline recognizer that records the length of each clip it is given and answers with `answer`, called with the clip and its index.
struct Recorder {
    calls: Arc<Mutex<Vec<usize>>>,
    answer: Box<Answer>,
}

/// Answers the clip of the given index.
type Answer = dyn FnMut(usize, &[i16]) -> Result<String, Error> + Send;

impl Recorder {
    fn new(
        answer: impl FnMut(usize, &[i16]) -> Result<String, Error> + Send + 'static,
    ) -> (Self, Arc<Mutex<Vec<usize>>>) {
        let calls = Arc::new(Mutex::new(Vec::new()));
        (
            Self {
                calls: Arc::clone(&calls),
                answer: Box::new(answer),
            },
            calls,
        )
    }
}

impl OfflineRecognizer for Recorder {
    fn transcribe(&mut self, samples: &[i16]) -> Result<String, Error> {
        let length = samples.len();
        assert!(length <= MAX_SEGMENT, "{length} samples");
        let index = {
            let mut calls = self.calls.lock().unwrap();
            calls.push(samples.len());
            calls.len() - 1
        };
        (self.answer)(index, samples)
    }
}

/// A steady signal with no pause in it, `seconds` long.
fn steady(seconds: usize) -> Vec<i16> {
    (0..RATE as usize * seconds)
        .map(|index| if index % 40 < 20 { 6000 } else { -6000 })
        .collect()
}

/// The transcript so far, `text`, as an utterance is told it between segments.
fn so_far(text: &str) -> TranscriptEvent {
    TranscriptEvent::Partial(crate::stt::Transcript {
        committed: text.to_owned(),
        tentative: String::new(),
    })
}

/// The utterance's outcome, past any progress and transcripts so far it was told.
fn collect(receiver: &mpsc::Receiver<TranscriptEvent>) -> TranscriptEvent {
    loop {
        match receiver
            .recv_timeout(WAIT)
            .expect("the utterance's outcome is reported in time")
        {
            TranscriptEvent::Progress(_) | TranscriptEvent::Partial(_) => {}
            event => return event,
        }
    }
}

#[test]
fn a_short_utterance_is_decoded_whole_once() {
    for length in [RATE as usize * 10, MAX_SEGMENT] {
        let (recorder, calls) = Recorder::new(|_, _| Ok("  Short and sweet. ".to_owned()));
        let mut offline = OfflineRecognition::spawn(recorder);
        let (sender, receiver) = mpsc::channel();
        let mut handle = offline.begin(RATE, move |event| {
            let _ = sender.send(event);
        });
        handle.push(&vec![1000; length]);
        handle.end();
        assert_eq!(
            collect(&receiver),
            TranscriptEvent::Final("Short and sweet.".to_owned())
        );
        assert_eq!(*calls.lock().unwrap(), vec![length]);
    }
}

#[test]
fn a_long_utterance_is_decoded_in_segments_joined_in_order() {
    let (recorder, calls) = Recorder::new(|index, _| {
        Ok(["  The first part, ", "", "and the third.\n"][index].to_owned())
    });
    let mut offline = OfflineRecognition::spawn(recorder);
    let (sender, receiver) = mpsc::channel();
    let mut handle = offline.begin(RATE, move |event| {
        let _ = sender.send(event);
    });
    // In 100 ms frames, as the headset sends them.
    let audio = steady(70);
    for frame in audio.chunks(RATE as usize / 10) {
        handle.push(frame);
    }
    handle.end();
    assert_eq!(
        collect(&receiver),
        TranscriptEvent::Final("The first part, and the third.".to_owned())
    );
    // Without a pause, each segment but the last is as long as it can be, and together they are the whole utterance.
    assert_eq!(
        *calls.lock().unwrap(),
        vec![MAX_SEGMENT, MAX_SEGMENT, audio.len() - 2 * MAX_SEGMENT]
    );
}

#[test]
fn segments_are_the_utterance_in_order() {
    // Each sample is its own index, scaled, so a segment shows where it came from.
    let audio: Vec<i16> = (0..RATE as usize * 80)
        .map(|index| (index / 1000) as i16)
        .collect();
    let starts = Arc::new(Mutex::new(Vec::new()));
    let seen = Arc::clone(&starts);
    let (recorder, calls) = Recorder::new(move |index, samples| {
        seen.lock()
            .unwrap()
            .push((samples[0], samples[samples.len() - 1]));
        Ok(format!("part {index}"))
    });
    let mut offline = OfflineRecognition::spawn(recorder);
    let (sender, receiver) = mpsc::channel();
    let mut handle = offline.begin(RATE, move |event| {
        let _ = sender.send(event);
    });
    handle.push(&audio);
    handle.end();
    let TranscriptEvent::Final(text) = collect(&receiver) else {
        unreachable!("expected a transcript");
    };
    let calls = calls.lock().unwrap().clone();
    assert_eq!(calls.iter().sum::<usize>(), audio.len());
    let parts: Vec<String> = (0..calls.len())
        .map(|index| format!("part {index}"))
        .collect();
    assert_eq!(text, parts.join(" "));
    // Each segment starts where the one before it ended.
    let mut next = 0;
    for (length, (first, last)) in calls.iter().zip(starts.lock().unwrap().iter()) {
        assert_eq!(*first, audio[next]);
        next += length;
        assert_eq!(*last, audio[next - 1]);
    }
}

/// A fake detector that hears speech everywhere but in `gap`, in clip indices, given audio that starts at `offset` in the clip.
struct Gap {
    offset: usize,
    gap: Range<usize>,
}

impl VoiceActivity for Gap {
    fn speech(&mut self, samples: &[i16]) -> Result<Vec<Range<usize>>, Error> {
        let gap = self.gap.start - self.offset..self.gap.end - self.offset;
        Ok(vec![0..gap.start, gap.end..samples.len()])
    }
}

#[test]
fn a_long_utterance_is_split_where_the_detector_hears_a_pause() {
    // Loudness alone shows no pause.
    let audio = steady(40);
    let gap = RATE as usize * 21..RATE as usize * 22;
    // The detector hears only the first segment's search; the rest fits in one segment.
    let detector = Gap {
        offset: segment::listen(&audio, 0).unwrap().start,
        gap: gap.clone(),
    };
    let (recorder, calls) = Recorder::new(|_, _| Ok("words".to_owned()));
    let mut offline = OfflineRecognition::with_voice_activity(recorder, detector);
    let (sender, receiver) = mpsc::channel();
    let mut handle = offline.begin(RATE, move |event| {
        let _ = sender.send(event);
    });
    handle.push(&audio);
    handle.end();
    assert_eq!(
        collect(&receiver),
        TranscriptEvent::Final("words words".to_owned())
    );
    let first = calls.lock().unwrap()[0];
    assert!(gap.contains(&first), "cut at {first}");
}

struct Panicking;

impl VoiceActivity for Panicking {
    fn speech(&mut self, _samples: &[i16]) -> Result<Vec<Range<usize>>, Error> {
        panic!("the detector broke");
    }
}

#[test]
fn a_panic_in_the_detector_falls_back_to_loudness() {
    log::set_max_level(log::LevelFilter::Trace);
    let (recorder, calls) = Recorder::new(|_, _| Ok("words".to_owned()));
    let mut offline = OfflineRecognition::with_voice_activity(recorder, Panicking);
    for _ in 0..2 {
        let (sender, receiver) = mpsc::channel();
        let mut handle = offline.begin(RATE, move |event| {
            let _ = sender.send(event);
        });
        handle.push(&steady(40));
        handle.end();
        assert_eq!(
            collect(&receiver),
            TranscriptEvent::Final("words words".to_owned())
        );
    }
    assert_eq!(calls.lock().unwrap().len(), 4);
}

#[test]
fn an_utterance_longer_than_the_limit_fails_as_it_passes_it() {
    log::set_max_level(log::LevelFilter::Trace);
    let (recorder, calls) = Recorder::new(|_, _| unreachable!("nothing is decoded"));
    let mut offline = OfflineRecognition::start(recorder, None, RATE as usize * 2, true);
    let (sender, receiver) = mpsc::channel();
    let mut handle = offline.begin(RATE, move |event| {
        let _ = sender.send(event);
    });
    // Exactly the limit is accepted.
    handle.push(&vec![1000; RATE as usize * 2]);
    assert!(receiver.recv_timeout(Duration::from_millis(200)).is_err());
    // One more frame fails it at once, before it ends.
    handle.push(&vec![1000; RATE as usize / 10]);
    assert_eq!(
        collect(&receiver),
        TranscriptEvent::Failed(Error::TooLong(2))
    );
    handle.push(&vec![1000; RATE as usize / 10]);
    handle.end();
    assert!(receiver.recv_timeout(Duration::from_millis(200)).is_err());
    assert!(calls.lock().unwrap().is_empty());
    assert_eq!(
        Error::TooLong(MAX_UTTERANCE / RATE as usize).to_string(),
        "the speech was longer than 5 minutes, the most that is transcribed at once"
    );
}

#[test]
fn a_failed_segment_fails_the_utterance() {
    log::set_max_level(log::LevelFilter::Trace);
    let (recorder, calls) = Recorder::new(|index, _| match index {
        1 => Err(Error::Engine("the decoder broke".to_owned())),
        _ => Ok("words".to_owned()),
    });
    let mut offline = OfflineRecognition::spawn(recorder);
    let (sender, receiver) = mpsc::channel();
    let mut handle = offline.begin(RATE, move |event| {
        let _ = sender.send(event);
    });
    handle.push(&steady(70));
    handle.end();
    assert_eq!(
        collect(&receiver),
        TranscriptEvent::Failed(Error::Engine("the decoder broke".to_owned()))
    );
    assert!(receiver.recv_timeout(Duration::from_millis(200)).is_err());
    // The rest of the utterance is not decoded.
    assert_eq!(calls.lock().unwrap().len(), 2);
}

#[test]
fn an_utterance_cancelled_or_abandoned_while_a_segment_is_decoded_ahead_reports_nothing() {
    for abandoned in [false, true] {
        let (started, decoding) = mpsc::channel();
        let (release, gate) = mpsc::channel::<()>();
        let gate = Mutex::new(gate);
        let (recorder, calls) = Recorder::new(move |_, _| {
            let _ = started.send(());
            let _ = gate.lock().unwrap().recv_timeout(WAIT);
            Ok("words".to_owned())
        });
        let mut offline = OfflineRecognition::spawn(recorder);
        // Declared after `offline`, so a failed assertion's unwinding drops it first, letting a held decode go on rather than hanging as `offline` joins the engine's thread.
        let release = release;
        // Reporting anything, even the words of the segment being decoded, would panic on the engine's thread, which disables recognition, as the next utterance would show.
        let mut handle = offline.begin(RATE, |event| unreachable!("reported {event:?}"));
        // Enough for two segments to be decided while it is spoken.
        handle.push(&steady(60));
        decoding
            .recv_timeout(WAIT)
            .expect("the first segment is decoded while the participant speaks");
        if abandoned {
            drop(handle);
        } else {
            handle.cancel();
        }
        release.send(()).unwrap();
        drop(release);
        let (sender, receiver) = mpsc::channel();
        let handle = offline.begin(RATE, move |event| {
            let _ = sender.send(event);
        });
        handle.end();
        assert_eq!(collect(&receiver), TranscriptEvent::Final(String::new()));
        // Its second segment, already decided, is not decoded.
        assert_eq!(calls.lock().unwrap().len(), 1);
    }
}

#[test]
fn an_utterance_that_ends_goes_before_the_next_segment_of_a_long_one() {
    let (started, decoding) = mpsc::channel();
    let (release, gate) = mpsc::channel::<()>();
    let gate = Mutex::new(gate);
    let (recorder, calls) = Recorder::new(move |_, samples| {
        let _ = started.send(());
        let _ = gate.lock().unwrap().recv_timeout(WAIT);
        Ok(format!("{}", samples.len()))
    });
    // Decoding only once utterances end, so that the long one's first part is decoded after its end.
    let mut offline = OfflineRecognition::start(recorder, None, MAX_UTTERANCE, false);
    // Declared after `offline`, so a failed assertion's unwinding drops it first, letting a held decode go on rather than hanging as `offline` joins the engine's thread.
    let release = release;
    let (sender, receiver) = mpsc::channel();
    let long_sender = sender.clone();
    let mut long = offline.begin(RATE, move |event| {
        let _ = long_sender.send(("long", event));
    });
    long.push(&steady(70));
    long.end();
    // While the long utterance's first segment is decoded, a short one ends.
    decoding
        .recv_timeout(WAIT)
        .expect("a decode starts in time");
    let mut short = offline.begin(RATE, move |event| {
        let _ = sender.send(("short", event));
    });
    short.push(&vec![1000; RATE as usize]);
    short.end();
    for _ in 0..4 {
        release.send(()).unwrap();
    }
    // The short one, told it waits as it ends, is transcribed next, whole; the long one waits for it, and is then transcribed by parts, with its words so far after each.
    let waiting = TranscriptEvent::Progress(Progress::Waiting);
    let part = |part, parts| TranscriptEvent::Progress(Progress::Transcribing { part, parts });
    let events: Vec<_> = (0..10)
        .map(|_| {
            receiver
                .recv_timeout(WAIT)
                .expect("an event is reported in time")
        })
        .collect();
    assert_eq!(
        events,
        [
            ("long", part(1, 3)),
            ("short", waiting.clone()),
            ("long", so_far(&format!("{MAX_SEGMENT}"))),
            ("long", waiting),
            ("short", part(1, 1)),
            ("short", TranscriptEvent::Final(format!("{}", RATE))),
            ("long", part(2, 3)),
            ("long", so_far(&format!("{MAX_SEGMENT} {MAX_SEGMENT}"))),
            ("long", part(3, 3)),
            (
                "long",
                TranscriptEvent::Final(format!(
                    "{MAX_SEGMENT} {MAX_SEGMENT} {}",
                    RATE as usize * 70 - 2 * MAX_SEGMENT
                ))
            ),
        ]
    );
    assert_eq!(
        *calls.lock().unwrap(),
        vec![
            MAX_SEGMENT,
            RATE as usize,
            MAX_SEGMENT,
            RATE as usize * 70 - 2 * MAX_SEGMENT
        ]
    );
}

#[test]
fn an_utterance_is_told_each_of_its_parts_and_nothing_for_one() {
    let part = |part, parts| TranscriptEvent::Progress(Progress::Transcribing { part, parts });
    for (seconds, told) in [(10, vec![]), (70, vec![part(1, 3), part(2, 3), part(3, 3)])] {
        // Decoding only once utterances end, so that every part is decoded after the end.
        let mut offline =
            OfflineRecognition::start(Fake("words".to_owned()), None, MAX_UTTERANCE, false);
        let (sender, receiver) = mpsc::channel();
        let mut handle = offline.begin(RATE, move |event| {
            let _ = sender.send(event);
        });
        handle.push(&steady(seconds));
        handle.end();
        let mut events: Vec<_> = receiver
            .iter()
            .filter(|event| !matches!(event, TranscriptEvent::Partial(_)))
            .take(told.len() + 1)
            .collect();
        let Some(TranscriptEvent::Final(_)) = events.pop() else {
            unreachable!("the outcome comes last");
        };
        assert_eq!(events, told);
    }
}

#[test]
fn an_utterance_waits_only_behind_one_still_being_transcribed() {
    let (started, decoding) = mpsc::channel();
    let (release, gate) = mpsc::channel::<()>();
    let gate = Mutex::new(gate);
    let (recorder, _) = Recorder::new(move |_, _| {
        let _ = started.send(());
        let _ = gate.lock().unwrap().recv_timeout(WAIT);
        Ok("words".to_owned())
    });
    let mut offline = OfflineRecognition::start(recorder, None, RATE as usize * 2, true);
    // Declared after `offline`, so a failed assertion's unwinding drops it first, letting a held decode go on rather than hanging as `offline` joins the engine's thread.
    let release = release;
    let listen = |offline: &mut OfflineRecognition| {
        let (sender, receiver) = mpsc::channel();
        let handle = offline.begin(RATE, move |event| {
            let _ = sender.send(event);
        });
        (handle, receiver)
    };
    let (mut first, _first) = listen(&mut offline);
    first.push(&[1000; 1600]);
    first.end();
    decoding
        .recv_timeout(WAIT)
        .expect("the first utterance's decode starts");
    // One that ends while another is transcribed is told at once, before its end returns, that it waits.
    let (mut second, waiting) = listen(&mut offline);
    second.push(&[1000; 1600]);
    second.end();
    assert_eq!(
        waiting.try_recv().ok(),
        Some(TranscriptEvent::Progress(Progress::Waiting)),
        "the second utterance is told it waits as it ends"
    );
    release.send(()).unwrap();
    decoding
        .recv_timeout(WAIT)
        .expect("the second utterance's decode starts once the first's finishes");
    // Its turn comes, and it says so, after the one it waited for is told its outcome.
    assert_eq!(
        waiting.recv_timeout(WAIT).ok(),
        Some(TranscriptEvent::Progress(Progress::Transcribing {
            part: 1,
            parts: 1
        })),
        "the second utterance is told it is transcribed once its turn comes"
    );
    release.send(()).unwrap();
    assert_eq!(
        collect(&waiting),
        TranscriptEvent::Final("words".to_owned())
    );
    // One that failed as too long, ending after its failure, makes nothing wait.
    let (mut long, failed) = listen(&mut offline);
    long.push(&[1000; RATE as usize * 3]);
    assert_eq!(collect(&failed), TranscriptEvent::Failed(Error::TooLong(2)));
    long.end();
    let (mut last, alone) = listen(&mut offline);
    last.push(&[1000; 1600]);
    last.end();
    decoding
        .recv_timeout(WAIT)
        .expect("the last utterance's decode starts");
    release.send(()).unwrap();
    assert_eq!(
        alone.recv_timeout(WAIT).ok(),
        Some(TranscriptEvent::Final("words".to_owned())),
        "the last utterance, behind only one that failed, is told nothing before its outcome"
    );
}

#[test]
fn long_utterances_take_turns_a_segment_at_a_time() {
    let (started, decoding) = mpsc::channel();
    let (release, gate) = mpsc::channel::<()>();
    let gate = Mutex::new(gate);
    let (recorder, _) = Recorder::new(move |_, samples| {
        let _ = started.send(samples[0]);
        let _ = gate.lock().unwrap().recv_timeout(WAIT);
        Ok(String::new())
    });
    let mut offline = OfflineRecognition::spawn(recorder);
    // Declared after `offline`, so a failed assertion's unwinding drops it first, letting a held decode go on rather than hanging as `offline` joins the engine's thread.
    let release = release;
    // Each utterance's audio is its own value, so a segment shows whose it is.
    let mut first = offline.begin(RATE, |_| {});
    first.push(&vec![1000; RATE as usize * 70]);
    first.end();
    assert_eq!(
        decoding
            .recv_timeout(WAIT)
            .expect("a decode starts in time"),
        1000
    );
    let mut second = offline.begin(RATE, |_| {});
    second.push(&vec![2000; RATE as usize * 70]);
    second.end();
    let mut order = vec![1000];
    for _ in 0..5 {
        release.send(()).unwrap();
        order.push(
            decoding
                .recv_timeout(WAIT)
                .expect("a decode starts in time"),
        );
    }
    release.send(()).unwrap();
    assert_eq!(order, vec![1000, 2000, 1000, 2000, 1000, 2000]);
}

#[test]
fn short_utterances_do_not_hold_up_a_long_one() {
    let (started, decoding) = mpsc::channel();
    let (release, gate) = mpsc::channel::<()>();
    let gate = Mutex::new(gate);
    let (recorder, _) = Recorder::new(move |_, samples| {
        let _ = started.send(samples[0]);
        let _ = gate.lock().unwrap().recv_timeout(WAIT);
        Ok(String::new())
    });
    let mut offline = OfflineRecognition::spawn(recorder);
    // Declared after `offline`, so a failed assertion's unwinding drops it first, letting a held decode go on rather than hanging as `offline` joins the engine's thread.
    let release = release;
    let mut long = offline.begin(RATE, |_| {});
    long.push(&vec![1000; RATE as usize * 40]);
    long.end();
    let mut order = vec![
        decoding
            .recv_timeout(WAIT)
            .expect("a decode starts in time"),
    ];
    // Each short utterance ends while the previous segment is being decoded.
    for value in [2000, 3000] {
        let mut short = offline.begin(RATE, |_| {});
        short.push(&vec![value; RATE as usize]);
        short.end();
        release.send(()).unwrap();
        order.push(
            decoding
                .recv_timeout(WAIT)
                .expect("a decode starts in time"),
        );
    }
    release.send(()).unwrap();
    order.push(
        decoding
            .recv_timeout(WAIT)
            .expect("a decode starts in time"),
    );
    release.send(()).unwrap();
    // The first short one ended before the long one's first segment was decoded, so it goes first; the second ended after, so it waits.
    assert_eq!(order, vec![1000, 2000, 1000, 3000]);
}

/// A fake detector that hears speech in any audio that is not silent.
struct Loud;

impl VoiceActivity for Loud {
    fn speech(&mut self, samples: &[i16]) -> Result<Vec<Range<usize>>, Error> {
        let mut speech = Vec::new();
        if samples.iter().any(|&sample| sample != 0) {
            speech.push(0..samples.len());
        }
        Ok(speech)
    }
}

#[test]
fn a_word_or_two_for_a_segment_without_speech_in_a_long_utterance_is_left_out() {
    for (heard, expected) in [
        ("you", "Speech. Speech."),
        ("Thank you.", "Speech. Speech."),
        (
            "Something said very quietly.",
            "Speech. Something said very quietly. Speech.",
        ),
    ] {
        let (recorder, calls) = Recorder::new(move |_, samples| {
            Ok(if samples.iter().all(|&sample| sample == 0) {
                heard.to_owned()
            } else {
                "Speech.".to_owned()
            })
        });
        let mut offline = OfflineRecognition::with_voice_activity(recorder, Loud);
        let (sender, receiver) = mpsc::channel();
        let mut handle = offline.begin(RATE, move |event| {
            let _ = sender.send(event);
        });
        // Speech, a pause longer than a segment, and speech again.
        handle.push(&steady(20));
        handle.push(&vec![0; RATE as usize * 30]);
        handle.push(&steady(20));
        handle.end();
        assert_eq!(
            collect(&receiver),
            TranscriptEvent::Final(expected.to_owned())
        );
        // Every segment is decoded, and together they are the whole utterance.
        let calls = calls.lock().unwrap().clone();
        assert_eq!(calls.len(), 3, "{calls:?}");
        assert_eq!(calls.iter().sum::<usize>(), RATE as usize * 70);
    }
}

#[test]
fn without_a_detector_a_word_for_a_pause_is_kept() {
    let (recorder, _) = Recorder::new(|_, samples| {
        Ok(if samples.iter().all(|&sample| sample == 0) {
            "you".to_owned()
        } else {
            "Speech.".to_owned()
        })
    });
    let mut offline = OfflineRecognition::spawn(recorder);
    let (sender, receiver) = mpsc::channel();
    let mut handle = offline.begin(RATE, move |event| {
        let _ = sender.send(event);
    });
    handle.push(&steady(20));
    handle.push(&vec![0; RATE as usize * 30]);
    handle.push(&steady(20));
    handle.end();
    assert_eq!(
        collect(&receiver),
        TranscriptEvent::Final("Speech. you Speech.".to_owned())
    );
}

/// The transcript of a short utterance of `audio` that `recognizer` hears as `heard`, with `voice` to hear speech in it, if any, and how many times it was decoded.
fn short(
    heard: &'static str,
    voice: Option<Box<dyn VoiceActivity>>,
    audio: &[i16],
) -> (TranscriptEvent, usize) {
    let (recorder, calls) = Recorder::new(move |_, _| Ok(heard.to_owned()));
    let mut offline = OfflineRecognition::start(recorder, voice, MAX_UTTERANCE, true);
    let (sender, receiver) = mpsc::channel();
    let mut handle = offline.begin(RATE, move |event| {
        let _ = sender.send(event);
    });
    handle.push(audio);
    handle.end();
    let event = collect(&receiver);
    let calls = calls.lock().unwrap().len();
    (event, calls)
}

#[test]
fn a_short_utterance_with_no_speech_in_it_is_empty() {
    log::set_max_level(log::LevelFilter::Trace);
    let silence = vec![0; RATE as usize * 2];
    let empty = (TranscriptEvent::Final(String::new()), 1);
    // A note of noise is left out, with a detector or without one.
    for heard in [
        "[BLANK_AUDIO]",
        " (static) ",
        "[ Silence ]",
        "[Music] (static)",
    ] {
        assert_eq!(short(heard, None, &silence), empty, "{heard}");
        assert_eq!(short(heard, Some(Box::new(Loud)), &silence), empty);
    }
    // A word or two is left out where the detector hears no speech.
    for heard in ["you", "Thank you."] {
        assert_eq!(short(heard, Some(Box::new(Loud)), &silence), empty);
    }
}

#[test]
fn a_short_utterance_keeps_words_that_may_be_speech() {
    let kept = |text: &str| (TranscriptEvent::Final(text.to_owned()), 1);
    // Words the detector heard speech for.
    assert_eq!(
        short("Yes.", Some(Box::new(Loud)), &steady(1)),
        kept("Yes.")
    );
    // A word or two without a detector to say there was no speech.
    assert_eq!(short("Yes.", None, &vec![0; RATE as usize]), kept("Yes."));
    // Anything longer, in case the detector missed quiet speech.
    assert_eq!(
        short(
            "Something said very quietly.",
            Some(Box::new(Loud)),
            &vec![0; RATE as usize]
        ),
        kept("Something said very quietly.")
    );
    // A note among words.
    assert_eq!(
        short(
            "(laughs) Yes, do it.",
            Some(Box::new(Loud)),
            &vec![0; RATE as usize]
        ),
        kept("(laughs) Yes, do it.")
    );
}

#[test]
fn a_note_of_noise_in_a_long_utterance_is_left_out() {
    let (recorder, _) = Recorder::new(|index, _| {
        Ok(["Hello.", "[BLANK_AUDIO]", " (water running) "][index].to_owned())
    });
    let mut offline = OfflineRecognition::spawn(recorder);
    let (sender, receiver) = mpsc::channel();
    let mut handle = offline.begin(RATE, move |event| {
        let _ = sender.send(event);
    });
    handle.push(&steady(70));
    handle.end();
    assert_eq!(
        collect(&receiver),
        TranscriptEvent::Final("Hello.".to_owned())
    );
    assert!(annotation("[BLANK_AUDIO]") && annotation("(buzzing)"));
    assert!(!annotation("(a) and (b)") && !annotation("[1] is cited") && !annotation("()x"));
    assert!(!annotation("") && !annotation("(") && !annotation("words"));
    assert!(annotation("[Music] (static)") && annotation("()") && !annotation("(a]"));
    assert!(!annotation("[BLANK_AUDIO] yes") && !annotation("[a [b]]"));
}

struct Crashing;

impl OfflineRecognizer for Crashing {
    fn transcribe(&mut self, samples: &[i16]) -> Result<String, Error> {
        if samples.len() < MAX_SEGMENT {
            panic!("the recognizer crashed");
        }
        Ok("words".to_owned())
    }
}

#[test]
fn a_panic_in_a_later_segment_disables_recognition() {
    let mut offline = OfflineRecognition::spawn(Crashing);
    let (sender, receiver) = mpsc::channel();
    let mut handle = offline.begin(RATE, move |event| {
        let _ = sender.send(event);
    });
    handle.push(&steady(40));
    handle.end();
    let TranscriptEvent::Failed(Error::Stopped(_)) = collect(&receiver) else {
        unreachable!("expected TranscriptEvent::Failed(Error::Stopped(_))");
    };
    let (sender, receiver) = mpsc::channel();
    let mut handle = offline.begin(RATE, move |event| {
        let _ = sender.send(event);
    });
    handle.push(&steady(1));
    handle.end();
    let TranscriptEvent::Failed(Error::Stopped(_)) = collect(&receiver) else {
        unreachable!("expected TranscriptEvent::Failed(Error::Stopped(_))");
    };
}

#[test]
fn segments_are_decoded_while_the_participant_speaks_leaving_only_the_tail() {
    let (done, decoded) = mpsc::channel();
    let (recorder, calls) = Recorder::new(move |index, _| {
        let _ = done.send(index);
        Ok(format!("part {index}"))
    });
    let mut offline = OfflineRecognition::spawn(recorder);
    let (sender, receiver) = mpsc::channel();
    let mut handle = offline.begin(RATE, move |event| {
        let _ = sender.send(event);
    });
    // In 100 ms frames, as the headset sends them. Without a pause, each segment ends at its limit, which is decided once the audio reaches a little past it.
    let audio = steady(70);
    let mut sent = 0;
    let mut heard = Vec::new();
    for frame in audio.chunks(RATE as usize / 10) {
        handle.push(frame);
        sent += frame.len();
        if segment::decided(sent, heard.len() * MAX_SEGMENT) {
            // The segment is decoded, and the words so far told, while the participant is still speaking.
            assert_eq!(decoded.recv_timeout(WAIT).ok(), Some(heard.len()));
            heard.push(format!("part {}", heard.len()));
            assert_eq!(
                receiver.recv_timeout(WAIT).ok(),
                Some(so_far(&heard.join(" ")))
            );
        }
    }
    assert_eq!(heard.len(), 2);
    handle.end();
    // Only the last segment is left, and the utterance is told it is the last of three.
    assert_eq!(
        receiver.recv_timeout(WAIT).ok(),
        Some(TranscriptEvent::Progress(Progress::Transcribing {
            part: 3,
            parts: 3
        }))
    );
    assert_eq!(
        receiver.recv_timeout(WAIT).ok(),
        Some(TranscriptEvent::Final("part 0 part 1 part 2".to_owned()))
    );
    assert_eq!(
        *calls.lock().unwrap(),
        vec![MAX_SEGMENT, MAX_SEGMENT, audio.len() - 2 * MAX_SEGMENT]
    );
}

/// A fake detector that hears speech in each 10 ms of audio louder than a whisper.
struct Energy;

impl VoiceActivity for Energy {
    fn speech(&mut self, samples: &[i16]) -> Result<Vec<Range<usize>>, Error> {
        let frame = RATE as usize / 100;
        let mut speech: Vec<Range<usize>> = Vec::new();
        for (index, chunk) in samples.chunks(frame).enumerate() {
            let loudness: i64 = chunk.iter().map(|&sample| i64::from(sample).abs()).sum();
            if loudness <= 500 * chunk.len() as i64 {
                continue;
            }
            let start = index * frame;
            match speech.last_mut() {
                Some(last) if last.end == start => last.end = start + chunk.len(),
                _ => speech.push(start..start + chunk.len()),
            }
        }
        Ok(speech)
    }
}

/// Two minutes and a quarter of loud, varied audio with breaths of 0.4 seconds at a few places, some close to a segment's limit, and 40 seconds of silence in the middle, long enough for a segment of nothing but silence.
fn talk() -> Vec<i16> {
    let second = |seconds: f64| (seconds * f64::from(RATE)) as usize;
    let breaths = [12.3, 21.7, 27.6, 45.2, 47.9, 52.0, 111.5, 120.0];
    (0..second(135.0))
        .map(|index| {
            let breath = breaths
                .iter()
                .any(|&at| (second(at)..second(at + 0.4)).contains(&index));
            if (second(60.0)..second(100.0)).contains(&index) {
                0
            } else if breath {
                (index % 41) as i16 - 20
            } else {
                ((index * 7919 % 2001) as i16 - 1000) * 6
            }
        })
        .collect()
}

/// A recognizer that hears each clip by where it starts and how long it is, or as "you" for silence, as Whisper does, and tells `done` of each clip it has decoded.
fn hearing(done: mpsc::Sender<()>) -> (Recorder, Arc<Mutex<Vec<usize>>>) {
    Recorder::new(move |_, samples| {
        let _ = done.send(());
        Ok(if samples.iter().all(|&sample| sample == 0) {
            "you".to_owned()
        } else {
            format!("{} from {}.", samples.len(), samples[0])
        })
    })
}

#[test]
fn segments_decoded_while_the_participant_speaks_are_those_decoded_after_the_end() {
    let audio = talk();
    // Decoded only once the utterance ends, as before.
    let (done, _) = mpsc::channel();
    let (recorder, after) = hearing(done);
    let mut offline =
        OfflineRecognition::start(recorder, Some(Box::new(Energy)), MAX_UTTERANCE, false);
    let (sender, receiver) = mpsc::channel();
    let mut handle = offline.begin(RATE, move |event| {
        let _ = sender.send(event);
    });
    handle.push(&audio);
    handle.end();
    let TranscriptEvent::Final(expected) = collect(&receiver) else {
        unreachable!("the utterance is transcribed");
    };
    // A segment of silence is heard as "you", which the detector says is no speech, so it is left out.
    let segments = after.lock().unwrap().len();
    assert!(segments >= 6, "{after:?}");
    assert!(!expected.contains("you"), "{expected}");
    assert!(expected.matches(" from ").count() < segments, "{expected}");

    // Decoded while it is spoken, in 100 ms frames, letting each segment that is decided be decoded before more is spoken.
    let (done, decoded) = mpsc::channel();
    let (recorder, ahead) = hearing(done);
    let mut offline = OfflineRecognition::with_voice_activity(recorder, Energy);
    let (sender, receiver) = mpsc::channel();
    let mut handle = offline.begin(RATE, move |event| {
        let _ = sender.send(event);
    });
    let mut sent = 0;
    for frame in audio.chunks(RATE as usize / 10) {
        handle.push(frame);
        sent += frame.len();
        while segment::decided(sent, ahead.lock().unwrap().iter().sum()) {
            decoded
                .recv_timeout(WAIT)
                .expect("a decided segment is decoded");
        }
    }
    let spoken = ahead.lock().unwrap().len();
    handle.end();
    assert_eq!(collect(&receiver), TranscriptEvent::Final(expected));
    // The same segments, of which at most the last two are left once the participant stops.
    assert_eq!(*ahead.lock().unwrap(), *after.lock().unwrap());
    assert!(segments - spoken <= 2, "{spoken} of {segments}");
}

#[test]
fn an_utterance_that_ends_goes_before_segments_of_those_still_spoken() {
    let (started, decoding) = mpsc::channel();
    let (release, gate) = mpsc::channel::<()>();
    let gate = Mutex::new(gate);
    let (recorder, _) = Recorder::new(move |_, samples| {
        let _ = started.send(samples[0]);
        let _ = gate.lock().unwrap().recv_timeout(WAIT);
        Ok("words".to_owned())
    });
    let mut offline = OfflineRecognition::spawn(recorder);
    // Declared after `offline`, so a failed assertion's unwinding drops it first, letting a held decode go on rather than hanging as `offline` joins the engine's thread.
    let release = release;
    let next = || decoding.recv_timeout(WAIT).ok();
    // Ada speaks for long enough that her first segment is decoded while she speaks.
    let (sender, told) = mpsc::channel();
    let mut ada = offline.begin(RATE, move |event| {
        let _ = sender.send(event);
    });
    ada.push(&vec![1000; RATE as usize * 30]);
    assert_eq!(next(), Some(1000));
    // Meanwhile, Bob's first segment is decided too, and Carol's short words end; she is told at once that they wait.
    let mut bob = offline.begin(RATE, |_| {});
    bob.push(&vec![2000; RATE as usize * 30]);
    let (sender, carol) = mpsc::channel();
    let mut words = offline.begin(RATE, move |event| {
        let _ = sender.send(event);
    });
    words.push(&vec![3000; RATE as usize]);
    words.end();
    assert_eq!(
        carol.try_recv().ok(),
        Some(TranscriptEvent::Progress(Progress::Waiting))
    );
    // Carol's words, which have ended, go before Bob's segment, which was due first.
    release.send(()).unwrap();
    assert_eq!(next(), Some(3000));
    release.send(()).unwrap();
    assert_eq!(next(), Some(2000));
    release.send(()).unwrap();
    assert_eq!(collect(&carol), TranscriptEvent::Final("words".to_owned()));
    // Ada ends while her own second segment is decoded ahead: she waits for no one else's words, so she is not told she waits, and only her last part is left.
    ada.push(&vec![1000; RATE as usize * 30]);
    assert_eq!(next(), Some(1000));
    ada.end();
    release.send(()).unwrap();
    assert_eq!(next(), Some(1000));
    release.send(()).unwrap();
    let mut events = Vec::new();
    while !matches!(events.last(), Some(TranscriptEvent::Final(_))) {
        events.push(told.recv_timeout(WAIT).expect("Ada is told in time"));
    }
    assert_eq!(
        events,
        [
            so_far("words"),
            so_far("words words"),
            TranscriptEvent::Progress(Progress::Transcribing { part: 3, parts: 3 }),
            TranscriptEvent::Final("words words words".to_owned()),
        ]
    );
    drop(bob);
}

#[test]
fn utterances_still_spoken_take_turns_a_segment_at_a_time() {
    let (started, decoding) = mpsc::channel();
    let (release, gate) = mpsc::channel::<()>();
    let gate = Mutex::new(gate);
    let (recorder, _) = Recorder::new(move |_, samples| {
        let _ = started.send(samples[0]);
        let _ = gate.lock().unwrap().recv_timeout(WAIT);
        Ok(String::new())
    });
    let mut offline = OfflineRecognition::spawn(recorder);
    // Declared after `offline`, so a failed assertion's unwinding drops it first, letting a held decode go on rather than hanging as `offline` joins the engine's thread.
    let release = release;
    // Each has two segments decided while it is spoken. Bob's arrive while Ada's first is decoded, so they wait for it, but not for her second.
    let mut ada = offline.begin(RATE, drop::<TranscriptEvent>);
    ada.push(&vec![1000; RATE as usize * 60]);
    let mut order = vec![
        decoding
            .recv_timeout(WAIT)
            .expect("a decode starts in time"),
    ];
    let mut bob = offline.begin(RATE, drop::<TranscriptEvent>);
    bob.push(&vec![2000; RATE as usize * 60]);
    for _ in 0..3 {
        release.send(()).unwrap();
        order.push(
            decoding
                .recv_timeout(WAIT)
                .expect("a decode starts in time"),
        );
    }
    assert_eq!(order, vec![1000, 2000, 1000, 2000]);
    // Nothing more is decided until they speak further.
    release.send(()).unwrap();
    assert!(decoding.recv_timeout(Duration::from_millis(200)).is_err());
    ada.cancel();
    bob.cancel();
}

#[test]
fn decoding_ahead_waits_for_an_end_on_its_way() {
    let (mut engine, sender) = engine_of(Fake("words".to_owned()));
    let ada = begin(&sender, 1);
    sender
        .send(Command::Audio {
            key: 1,
            samples: vec![1000; RATE as usize * 30],
            at: Instant::now(),
        })
        .unwrap();
    let bob = begin(&sender, 2);
    // Bob's handle has said that he has ended, and its end is on its way.
    pending(&engine.pending)
        .open
        .extend([(1, false), (2, true)]);
    assert!(engine.turn());
    assert!(ada.try_recv().is_err(), "a segment was decoded ahead");
    sender
        .send(Command::End {
            key: 2,
            at: Instant::now(),
            waiting: false,
        })
        .unwrap();
    assert!(engine.turn());
    assert_eq!(
        bob.try_recv().ok(),
        Some(TranscriptEvent::Final(String::new()))
    );
    assert!(engine.turn());
    assert_eq!(ada.try_recv().ok(), Some(so_far("words")));
}

#[test]
fn an_utterance_passing_the_limit_after_segments_were_decoded_ahead_fails() {
    let (recorder, calls) = Recorder::new(|_, _| Ok("words".to_owned()));
    let mut offline = OfflineRecognition::start(recorder, None, RATE as usize * 40, true);
    let (sender, receiver) = mpsc::channel();
    let mut handle = offline.begin(RATE, move |event| {
        let _ = sender.send(event);
    });
    handle.push(&steady(30));
    assert_eq!(receiver.recv_timeout(WAIT).ok(), Some(so_far("words")));
    handle.push(&steady(11));
    assert_eq!(
        receiver.recv_timeout(WAIT).ok(),
        Some(TranscriptEvent::Failed(Error::TooLong(40)))
    );
    handle.end();
    assert!(receiver.recv_timeout(Duration::from_millis(200)).is_err());
    assert_eq!(calls.lock().unwrap().len(), 1);
}

/// Transcribes a minute of LibriSpeech, from the clips that come with the Whisper model, with Whisper and the Silero detector, as the offline backend does. Skipped without both models; see [`crate::test_models`].
#[test]
fn transcribes_a_long_utterance_with_whisper() {
    use crate::{
        stt::{SileroVad, SileroVadConfig, WhisperConfig, WhisperRecognizer},
        test_models,
    };

    /// Whisper, recording the length of each segment once it has decoded it, and saying so.
    struct Whisper(WhisperRecognizer, Arc<Mutex<Vec<usize>>>, mpsc::Sender<()>);
    impl OfflineRecognizer for Whisper {
        fn transcribe(&mut self, samples: &[i16]) -> Result<String, Error> {
            let text = self.0.transcribe(samples);
            self.1.lock().unwrap().push(samples.len());
            let _ = self.2.send(());
            text
        }
    }

    let root = test_models::root();
    let config = WhisperConfig::named(&root, test_models::WHISPER_MODEL);
    let vad = SileroVadConfig::from_root(&root);
    test_models::with(
        "the Whisper and Silero voice activity models",
        &format!("bash scripts/fetch-asr-model.sh whisper {}", config.name),
        &[&config.model, &vad.model],
        || {
            let directory = config.model.join("test_wavs");
            let transcripts = std::fs::read_to_string(directory.join("trans.txt")).unwrap();
            let mut audio = Vec::new();
            let mut reference = Vec::new();
            // Each clip twice, with a short breath between clips, so the minute has pauses but none long.
            for _ in 0..2 {
                for line in transcripts.lines() {
                    let (file, text) = line.split_once(' ').unwrap();
                    let wave =
                        sherpa_onnx::Wave::read(directory.join(file).to_str().unwrap()).unwrap();
                    let samples: Vec<i16> = wave
                        .samples()
                        .iter()
                        .map(|sample| (sample.clamp(-1.0, 1.0) * 32767.0) as i16)
                        .collect();
                    let mut resampled = Vec::new();
                    Resampler::new(wave.sample_rate() as u32, RATE)
                        .process(&samples, &mut resampled);
                    audio.extend(resampled);
                    audio.extend(std::iter::repeat_n(0, RATE as usize * 4 / 10));
                    reference.extend(text.split_whitespace().map(str::to_lowercase));
                }
            }
            let length = audio.len();
            assert!(length > 2 * MAX_SEGMENT, "{length} samples");

            // With the detector, as the server runs, and by loudness alone, as when its model is not installed, decoding once the recording ends.
            let mut decoded_after_the_end = None;
            for detector in [true, false] {
                let whisper = WhisperRecognizer::new(&config).unwrap();
                let segments = Arc::new(Mutex::new(Vec::new()));
                let whisper = Whisper(whisper, Arc::clone(&segments), mpsc::channel().0);
                let voice: Option<Box<dyn VoiceActivity>> = if detector {
                    Some(Box::new(SileroVad::new(&vad).unwrap()))
                } else {
                    None
                };
                let mut offline = OfflineRecognition::start(whisper, voice, MAX_UTTERANCE, false);
                let (sender, receiver) = mpsc::channel();
                let mut handle = offline.begin(RATE, move |event| {
                    let _ = sender.send(event);
                });
                let started = std::time::Instant::now();
                handle.push(&audio);
                handle.end();
                let mut told = Vec::new();
                let event = loop {
                    match receiver.recv_timeout(Duration::from_secs(600)).unwrap() {
                        TranscriptEvent::Progress(progress) => told.push(progress),
                        TranscriptEvent::Partial(_) => {}
                        event => break event,
                    }
                };
                let TranscriptEvent::Final(text) = event else {
                    unreachable!("{event:?}");
                };
                eprintln!(
                    "Release to final transcript, decoding after the end: {:.2} seconds",
                    started.elapsed().as_secs_f64()
                );
                if detector {
                    decoded_after_the_end = Some((text.clone(), segments.lock().unwrap().clone()));
                }
                // Each part was told as it started, counted against all of them.
                let parts = segments.lock().unwrap().len();
                let expected: Vec<_> = (1..=parts)
                    .map(|part| Progress::Transcribing { part, parts })
                    .collect();
                assert_eq!(told, expected);
                let words: Vec<String> = text
                    .to_lowercase()
                    .split(|character: char| !(character.is_alphanumeric() || character == '\''))
                    .filter(|word| !word.is_empty())
                    .map(str::to_owned)
                    .collect();
                let errors = edit_distance(&reference, &words);
                let segments: Vec<String> = segments
                    .lock()
                    .unwrap()
                    .iter()
                    .map(|length| format!("{:.2}", *length as f64 / f64::from(RATE)))
                    .collect();
                eprintln!(
                    "{} {:.1} seconds transcribed in {:.1} seconds, in segments of {} seconds, {errors} word errors in {} words: {text}",
                    if detector { "Silero:" } else { "Loudness:" },
                    audio.len() as f64 / f64::from(RATE),
                    started.elapsed().as_secs_f64(),
                    segments.join(", "),
                    reference.len()
                );
                // A cut through a word would cost at least a word on each side of it; LibriSpeech spells "dishonoured" and "for ever" in British style, which Whisper does not, so a perfect transcript has 6 errors.
                assert!(errors * 20 <= reference.len(), "{errors} errors: {text}");
            }

            // Decoded while it is spoken, as the server runs, in 100 ms frames, each segment that is decided being decoded before more is spoken, as on a machine that keeps up: the same segments are decoded, the transcript is the same, and only the tail is left once the participant stops.
            let (expected, cuts) = decoded_after_the_end.expect("transcribed with the detector");
            let segments = Arc::new(Mutex::new(Vec::new()));
            let (done, decoded) = mpsc::channel();
            let whisper = Whisper(
                WhisperRecognizer::new(&config).unwrap(),
                Arc::clone(&segments),
                done,
            );
            let mut offline =
                OfflineRecognition::with_voice_activity(whisper, SileroVad::new(&vad).unwrap());
            let (sender, receiver) = mpsc::channel();
            let mut handle = offline.begin(RATE, move |event| {
                let _ = sender.send(event);
            });
            let mut sent = 0;
            for frame in audio.chunks(RATE as usize / 10) {
                handle.push(frame);
                sent += frame.len();
                // Segments are counted once decoded, so none is still being decoded when the participant stops.
                while segment::decided(sent, segments.lock().unwrap().iter().sum()) {
                    decoded
                        .recv_timeout(Duration::from_secs(600))
                        .expect("a decided segment is decoded");
                }
            }
            let spoken = segments.lock().unwrap().len();
            let released = std::time::Instant::now();
            handle.end();
            let mut heard = Vec::new();
            let event = loop {
                match receiver.recv_timeout(Duration::from_secs(600)).unwrap() {
                    TranscriptEvent::Partial(transcript) => heard.push(transcript.committed),
                    TranscriptEvent::Progress(_) => {}
                    event => break event,
                }
            };
            eprintln!(
                "Release to final transcript, decoding while spoken: {:.2} seconds, with {} of {} segments left: {heard:?}",
                released.elapsed().as_secs_f64(),
                cuts.len() - spoken,
                cuts.len()
            );
            assert_eq!(event, TranscriptEvent::Final(expected.clone()));
            assert_eq!(*segments.lock().unwrap(), cuts);
            assert!(
                spoken >= 1 && cuts.len() - spoken <= 2,
                "{spoken} of {cuts:?}"
            );
            // The words so far, told after each segment but the last, lead to the transcript.
            assert!(!heard.is_empty(), "no words were told while spoken");
            for words in &heard {
                assert!(expected.starts_with(words.as_str()), "{words:?}");
            }
        },
    );
}

/// With Whisper and the Silero detector, as the offline backend runs: what Whisper writes for a recording with nothing said in it is left out, and a short answer the detector hears as speech is kept. Skipped without both models; see [`crate::test_models`].
#[test]
fn a_silent_recording_is_empty_with_whisper_and_silero() {
    use crate::{
        stt::{SileroVad, SileroVadConfig, WhisperConfig, WhisperRecognizer},
        test_models,
    };

    let root = test_models::root();
    let config = WhisperConfig::named(&root, test_models::WHISPER_MODEL);
    let vad = SileroVadConfig::from_root(&root);
    test_models::with(
        "the Whisper and Silero voice activity models",
        &format!("bash scripts/fetch-asr-model.sh whisper {}", config.name),
        &[&config.model, &vad.model],
        || {
            // Two seconds of silence and of faint noise, as from a press to talk by accident.
            let silence = vec![0; RATE as usize * 2];
            let noise: Vec<i16> = (0..RATE as usize * 2)
                .map(|index| ((index * 7919 % 401) as i16 - 200) / 4)
                .collect();
            for audio in [silence, noise] {
                let mut whisper = WhisperRecognizer::new(&config).unwrap();
                let heard = Arc::new(Mutex::new(Vec::new()));
                let shared = Arc::clone(&heard);
                let (recorder, _) = Recorder::new(move |_, samples| {
                    whisper
                        .transcribe(samples)
                        .inspect(|text| shared.lock().unwrap().push(text.clone()))
                });
                let vad = SileroVad::new(&vad).unwrap();
                let mut offline = OfflineRecognition::with_voice_activity(recorder, vad);
                let (sender, receiver) = mpsc::channel();
                let mut handle = offline.begin(RATE, move |event| {
                    let _ = sender.send(event);
                });
                handle.push(&audio);
                handle.end();
                let event = collect(&receiver);
                eprintln!("Whisper heard {:?}", heard.lock().unwrap());
                assert_eq!(event, TranscriptEvent::Final(String::new()));
            }
            // A word of speech the detector hears is kept, even as short as "yes": the start of a LibriSpeech clip, which Whisper would transcribe as more.
            let wave =
                sherpa_onnx::Wave::read(config.model.join("test_wavs/0.wav").to_str().unwrap())
                    .unwrap();
            let speech: Vec<i16> = wave.samples()[..RATE as usize * 2]
                .iter()
                .map(|sample| (sample.clamp(-1.0, 1.0) * 32767.0) as i16)
                .collect();
            let voice: Box<dyn VoiceActivity> = Box::new(SileroVad::new(&vad).unwrap());
            assert_eq!(
                short("Yes.", Some(voice), &speech),
                (TranscriptEvent::Final("Yes.".to_owned()), 1)
            );
        },
    );
}

/// The number of words inserted, deleted or replaced to turn `reference` into `words`.
fn edit_distance(reference: &[String], words: &[String]) -> usize {
    let mut previous: Vec<usize> = (0..=words.len()).collect();
    for (row, expected) in reference.iter().enumerate() {
        let mut current = vec![row + 1];
        for (column, word) in words.iter().enumerate() {
            let replace = previous[column] + usize::from(expected != word);
            current.push(
                replace
                    .min(previous[column + 1] + 1)
                    .min(current[column] + 1),
            );
        }
        previous = current;
    }
    previous[words.len()]
}

/// A detector that cannot hear.
struct Deaf;

impl VoiceActivity for Deaf {
    fn speech(&mut self, _samples: &[i16]) -> Result<Vec<Range<usize>>, Error> {
        Err(Error::Engine("the detector is deaf".into()))
    }
}

/// A recognizer that panics.
struct Broken;

impl OfflineRecognizer for Broken {
    fn transcribe(&mut self, _samples: &[i16]) -> Result<String, Error> {
        panic!("the recognizer broke")
    }
}

#[test]
fn a_detector_that_fails_falls_back_to_loudness_and_a_boxed_recognizer_transcribes() {
    log::set_max_level(log::LevelFilter::Trace);
    let mut boxed: Box<dyn OfflineRecognizer> = Box::new(Fake("hello".to_owned()));
    assert_eq!(boxed.transcribe(&[]).unwrap(), "hello");
    let (recorder, calls) = Recorder::new(|_, _| Ok("words".to_owned()));
    let mut offline = OfflineRecognition::with_voice_activity(recorder, Deaf);
    let (sender, receiver) = mpsc::channel();
    let mut handle = offline.begin(RATE, move |event| {
        let _ = sender.send(event);
    });
    handle.push(&steady(40));
    handle.end();
    assert_eq!(
        collect(&receiver),
        TranscriptEvent::Final("words words".to_owned())
    );
    assert_eq!(calls.lock().unwrap().len(), 2);
}

#[test]
fn a_recognizer_that_panics_disables_recognition_and_a_report_that_panics_is_survived() {
    log::set_max_level(log::LevelFilter::Trace);
    let mut offline = OfflineRecognition::spawn(Broken);
    let (sender, receiver) = mpsc::channel();
    let mut handle = offline.begin(RATE, move |event| {
        let _ = sender.send(event);
    });
    handle.push(&[1000; 160]);
    handle.end();
    let TranscriptEvent::Failed(Error::Stopped(_)) = collect(&receiver) else {
        unreachable!("a panic stops recognition");
    };
    // Told it has stopped, a sink that panics panics the engine again.
    let handle = offline.begin(RATE, |_| panic!("the sink broke"));
    handle.end();
    let (sender, receiver) = mpsc::channel();
    let handle = offline.begin(RATE, move |event| {
        let _ = sender.send(event);
    });
    handle.end();
    let TranscriptEvent::Failed(Error::Stopped(_)) = collect(&receiver) else {
        unreachable!("recognition stays stopped");
    };
}

/// An engine of `recognizer`, with what sends it commands.
fn engine_of(recognizer: Fake) -> (OfflineEngine<Fake>, Sender<Command>) {
    let (sender, receiver) = mpsc::channel();
    (
        OfflineEngine::new(
            receiver,
            recognizer,
            None,
            MAX_UTTERANCE,
            true,
            Pending::default(),
        ),
        sender,
    )
}

fn begin(sender: &Sender<Command>, key: u64) -> mpsc::Receiver<TranscriptEvent> {
    let (events, receiver) = mpsc::channel();
    sender
        .send(Command::Begin {
            key,
            sink: Arc::new(Mutex::new(Box::new(move |event| {
                let _ = events.send(event);
            }))),
        })
        .unwrap();
    receiver
}

#[test]
fn an_engine_whose_commands_end_fails_what_is_open() {
    // While it waits for the next.
    let (mut engine, sender) = engine_of(Fake("hello".to_owned()));
    let events = begin(&sender, 1);
    assert!(engine.turn());
    drop(sender);
    assert!(!engine.turn());
    let TranscriptEvent::Failed(Error::Stopped(_)) = events.recv().unwrap() else {
        unreachable!("the open utterance fails");
    };
    // While it applies those waiting.
    let (mut engine, sender) = engine_of(Fake("hello".to_owned()));
    let events = begin(&sender, 1);
    sender
        .send(Command::Audio {
            key: 1,
            samples: vec![1000; 160],
            at: Instant::now(),
        })
        .unwrap();
    drop(sender);
    assert!(!engine.turn());
    let TranscriptEvent::Failed(Error::Stopped(_)) = events.recv().unwrap() else {
        unreachable!("the open utterance fails");
    };
}

#[test]
fn audio_at_another_rate_is_resampled_and_too_little_to_resample_is_kept() {
    let mut offline = OfflineRecognition::spawn(Fake("hello".to_owned()));
    let mut handle = offline.begin(RATE, drop::<TranscriptEvent>);
    handle.push_frame(&AudioFrame {
        gate: tomori_protocol::Gate::Agent,
        source: 0,
        sequence: 0,
        timestamp: 0,
        sample_rate: 48_000,
        samples: vec![0; 1],
    });
    handle.cancel();
}
