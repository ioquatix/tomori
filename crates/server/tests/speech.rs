// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

//! Speech recognition and agent speech end to end, with a fake recognizer and synthesizer in place of the real engines.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use protocol::{AGENT_SOURCE, AudioFrame, ClientMessage, Gate, QueueState, ServerMessage};
use server::fake::FakeBackend;
use server::test_support::{TestClient, TestServer, TextTranscriber};
use server::{RecognitionTranscriber, Server};
use speech::stt::{RATE, Word};
use speech::{AudioChunk, Error, Recognizer, RecognizerConfig, Synthesizer};

/// Words the fake recognizer knows. Each is "spoken" as [`WORD_SAMPLES`] samples of `BASE + index`.
const VOCABULARY: &[&str] = &["run", "the", "tests", "please", "fail", "crash"];
const BASE: i16 = 1000;
const WORD_SAMPLES: usize = RATE as usize / 5;
/// Samples per uploaded frame: 20 ms.
const FRAME_SAMPLES: usize = RATE as usize / 50;

/// The fake synthesizer's rate, and its audio per word: two 20 ms frames.
const SYNTHESIS_RATE: u32 = 24_000;
const SYNTHESIS_FRAME: usize = SYNTHESIS_RATE as usize / 50;
const FRAMES_PER_WORD: usize = 2;

/// Recognizes [`VOCABULARY`] words from their sample values. "fail" makes the decode fail, as a crashed worker process would; "crash" panics the recognizer thread.
struct FakeRecognizer;

impl Recognizer for FakeRecognizer {
    fn decode(&mut self, audio: &[i16], _prompt: Option<&str>) -> Result<Vec<Word>, Error> {
        let mut words = Vec::new();
        let mut start = 0;
        while start < audio.len() {
            let value = audio[start];
            let length = audio[start..]
                .iter()
                .take_while(|&&sample| sample == value)
                .count();
            // Commit cuts can leave a stray sample of the previous word; ignore runs that are not whole words.
            let count = (length + WORD_SAMPLES / 2) / WORD_SAMPLES;
            let word = usize::try_from(value - BASE)
                .ok()
                .and_then(|index| VOCABULARY.get(index));
            if let Some(&word) = word {
                for index in 0..count {
                    match word {
                        "fail" => return Err(Error::Worker("the worker exited".into())),
                        "crash" => panic!("the recognizer crashed"),
                        _ => {}
                    }
                    let begin = start + index * WORD_SAMPLES;
                    words.push(Word {
                        text: format!(" {word}"),
                        start: begin as f32 / RATE as f32,
                        end: (begin + WORD_SAMPLES) as f32 / RATE as f32,
                    });
                }
            }
            start += length;
        }
        Ok(words)
    }
}

/// Audio that [`FakeRecognizer`] hears as `words`.
fn spoken(words: &[&str]) -> Vec<i16> {
    words
        .iter()
        .flat_map(|word| {
            let index = VOCABULARY.iter().position(|known| known == word).unwrap();
            std::iter::repeat_n(BASE + index as i16, WORD_SAMPLES)
        })
        .collect()
}

fn speak(client: &mut TestClient, words: &[&str]) {
    for frame in spoken(words).chunks(FRAME_SAMPLES) {
        client.send_audio(Gate::Agent, frame.to_vec());
    }
}

/// Produces one chunk of [`FRAMES_PER_WORD`] frames per word, and records what it was asked to say.
#[derive(Clone, Default)]
struct FakeSynthesizer {
    spoken: Arc<Mutex<Vec<String>>>,
}

impl Synthesizer for FakeSynthesizer {
    fn synthesize(&mut self, text: &str, output: &mut dyn FnMut(AudioChunk)) -> Result<(), Error> {
        self.spoken.lock().unwrap().push(text.to_owned());
        for _ in text.split_whitespace() {
            output(AudioChunk {
                sample_rate: SYNTHESIS_RATE,
                samples: vec![100; SYNTHESIS_FRAME * FRAMES_PER_WORD],
            });
        }
        Ok(())
    }
}

fn recognizing_server(backend: FakeBackend) -> TestServer {
    TestServer::with_server(backend, |backends| {
        let config = RecognizerConfig {
            refresh: Duration::from_millis(100),
            minimum_audio: Duration::from_millis(100),
            ..RecognizerConfig::default()
        };
        Server::new(
            backends,
            Arc::new(RecognitionTranscriber::new(FakeRecognizer, config)),
        )
    })
}

fn speaking_server(backend: FakeBackend, synthesizer: FakeSynthesizer) -> TestServer {
    TestServer::with_server(backend, move |backends| {
        Server::new(backends, Arc::new(TextTranscriber::default())).with_synthesizer(synthesizer)
    })
}

/// Two participants in one space, created by Ada.
fn pair(server: &TestServer) -> (TestClient, TestClient) {
    let mut ada = server.connect("ada", "Ada");
    let mut bob = server.connect("bob", "Bob");
    let space = ada.create_space("Speech").id;
    bob.send(&ClientMessage::Join {
        space: space.clone(),
    });
    bob.space(|state| state.id == space && state.participants.len() == 2);
    ada.space(|state| state.participants.len() == 2);
    (ada, bob)
}

/// Agent speech received by one client: announcements as `(segment, message, text)` and the frames.
#[derive(Default)]
struct Heard {
    announcements: Vec<(u32, String, String)>,
    frames: Vec<AudioFrame>,
}

/// Collect `frames` agent frames, and the announcements that arrived meanwhile.
fn hear(client: &TestClient, frames: usize) -> Heard {
    let mut heard = Heard::default();
    let deadline = Instant::now() + Duration::from_secs(10);
    while heard.frames.len() < frames {
        let remaining = deadline.saturating_duration_since(Instant::now());
        assert!(
            !remaining.is_zero(),
            "heard {} of {frames} frames",
            heard.frames.len()
        );
        // Participants' push-to-talk audio is relayed too; only agent speech counts here.
        if let Some(frame) = client.next_audio(remaining.min(Duration::from_millis(50)))
            && frame.source == AGENT_SOURCE
        {
            heard.frames.push(frame);
        }
    }
    // Announcements are sent before their frames, so all of them have arrived by now.
    while let Some(message) = client.next_message(Duration::from_millis(100)) {
        if let ServerMessage::Speech {
            segment,
            message,
            text,
            sample_rate,
        } = message
        {
            assert_eq!(sample_rate, SYNTHESIS_RATE);
            heard.announcements.push((segment, message, text));
        }
    }
    heard
}

/// Check that the frames form one contiguous, live, 20 ms stream.
fn assert_stream(frames: &[AudioFrame]) {
    let first = frames[0].sequence;
    for (index, frame) in frames.iter().enumerate() {
        assert_eq!(frame.sequence, first.wrapping_add(index as u32));
        assert_eq!(frame.gate, Gate::Live);
        assert_eq!(frame.sample_rate, SYNTHESIS_RATE);
        assert_eq!(frame.samples.len(), SYNTHESIS_FRAME);
    }
}

fn words(texts: &[&str]) -> usize {
    texts
        .iter()
        .map(|text| text.split_whitespace().count())
        .sum()
}

#[test]
fn push_to_talk_shows_partials_then_queues_the_final_transcript() {
    let server = recognizing_server(FakeBackend::manual());
    let mut ada = server.connect("ada", "Ada");
    let mut bob = server.connect("bob", "Bob");
    let space = ada.create_space("Recognition").id;
    bob.send(&ClientMessage::Join {
        space: space.clone(),
    });
    bob.space(|state| state.participants.len() == 2);

    ada.send(&ClientMessage::UtteranceBegin {
        utterance: "u1".into(),
    });
    speak(&mut ada, &["run", "the"]);
    let partial = ada.wait_for(|message| match message {
        ServerMessage::Transcript {
            utterance,
            text,
            finished: false,
        } if utterance == "u1" => Some(text.clone()),
        ServerMessage::Transcript { finished: true, .. } => {
            panic!("the final transcript arrived before any partial")
        }
        _ => None,
    });
    assert!("run the".starts_with(&partial), "partial {partial:?}");

    speak(&mut ada, &["tests"]);
    ada.send(&ClientMessage::UtteranceEnd {
        utterance: "u1".into(),
    });
    let text = ada.wait_for(|message| match message {
        ServerMessage::Transcript {
            utterance,
            text,
            finished: true,
        } if utterance == "u1" => Some(text.clone()),
        _ => None,
    });
    assert_eq!(text, "run the tests");

    let state = bob.space(|state| {
        state
            .queue
            .iter()
            .any(|entry| entry.state == QueueState::Delivered)
    });
    assert_eq!(state.queue[0].text, "run the tests");
    assert_eq!(state.queue[0].speaker, "Ada");
    assert_eq!(state.queue[0].participant, "ada");
    let session = server.backend.started()[0].clone();
    assert_eq!(session.log().prompts[0].text, "Ada: run the tests");

    // Partial transcripts are the speaker's own in-progress recording, so Bob saw none.
    while let Some(message) = bob.next_message(Duration::from_millis(100)) {
        assert!(
            !matches!(message, ServerMessage::Transcript { .. }),
            "{message:?}"
        );
    }
}

/// Wait for a message from the server through a real client.
fn receive<T>(client: &client::Client, mut select: impl FnMut(ServerMessage) -> Option<T>) -> T {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let remaining = deadline
            .checked_duration_since(Instant::now())
            .expect("the message arrives in time");
        if let client::Event::Message(message) = client.events().recv_timeout(remaining).unwrap()
            && let Some(value) = select(message)
        {
            return value;
        }
    }
}

#[test]
fn the_client_microphone_sends_push_to_talk_that_is_transcribed_and_queued() {
    let server = recognizing_server(FakeBackend::manual());
    let ada = client::Client::connect(client::Options::new(
        server.address.to_string(),
        "ada",
        "Ada",
    ))
    .unwrap();
    ada.sender()
        .send(ClientMessage::CreateSpace {
            title: "Microphone".into(),
            backend: server::fake::FAKE_BACKEND_NAME.into(),
            directory: "/tmp".into(),
            session: None,
        })
        .unwrap();
    let space = receive(&ada, |message| match message {
        ServerMessage::Space { state } => Some(state.id),
        _ => None,
    });
    let mut bob = server.connect("bob", "Bob");
    bob.send(&ClientMessage::Join { space });
    bob.space(|state| state.participants.len() == 2);

    let mut microphone = client::Microphone::new(ada.sender(), RATE);
    // Audio before pressing is not sent, and capture delivers buffers that are not whole frames.
    microphone.push(&spoken(&["fail"])).unwrap();
    microphone.begin("launch-1").unwrap();
    for buffer in spoken(&["run", "the", "tests"]).chunks(441) {
        microphone.push(buffer).unwrap();
    }
    microphone.end().unwrap();

    let text = receive(&ada, |message| match message {
        ServerMessage::Transcript {
            utterance,
            text,
            finished: true,
        } if utterance == "launch-1" => Some(text),
        ServerMessage::TranscriptFailed { error, .. } => panic!("{error}"),
        _ => None,
    });
    assert_eq!(text, "run the tests");
    // Bob heard it too, as push-to-talk audio.
    let heard = bob.next_audio(Duration::from_secs(5)).unwrap();
    assert_eq!(heard.gate, Gate::Agent);
    let state = bob.space(|state| !state.queue.is_empty());
    assert_eq!(state.queue[0].text, "run the tests");
    assert_eq!(state.queue[0].participant, "ada");
    ada.close();
}

#[test]
fn a_recognizer_failure_fails_the_utterance() {
    let server = recognizing_server(FakeBackend::manual());
    let mut ada = server.connect("ada", "Ada");
    ada.create_space("Failure");
    ada.send(&ClientMessage::UtteranceBegin {
        utterance: "u1".into(),
    });
    speak(&mut ada, &["please", "fail"]);
    ada.send(&ClientMessage::UtteranceEnd {
        utterance: "u1".into(),
    });
    let error = ada.wait_for(|message| match message {
        ServerMessage::TranscriptFailed { utterance, error } if utterance == "u1" => {
            Some(error.clone())
        }
        ServerMessage::Transcript { finished: true, .. } => panic!("a transcript was queued"),
        _ => None,
    });
    assert!(error.contains("the worker exited"), "{error}");
    assert!(error.contains("nothing was sent"), "{error}");
    // Nothing was queued: no snapshot with a queue entry follows.
    // Nor is the end refused, if the failure came first.
    while let Some(message) = ada.next_message(Duration::from_millis(200)) {
        match message {
            ServerMessage::Space { state } => assert!(state.queue.is_empty()),
            ServerMessage::Error { error, .. } => panic!("{error}"),
            _ => {}
        }
    }
    assert!(server.backend.started()[0].log().prompts.is_empty());
}

#[test]
fn a_recognizer_failure_while_recording_closes_the_utterance() {
    let server = recognizing_server(FakeBackend::manual());
    let mut ada = server.connect("ada", "Ada");
    ada.create_space("Failure");
    ada.send(&ClientMessage::UtteranceBegin {
        utterance: "u1".into(),
    });
    speak(&mut ada, &["please", "fail", "please", "please"]);
    // Still recording: the failure arrives before any end.
    ada.wait_for(|message| match message {
        ServerMessage::TranscriptFailed { utterance, .. } if utterance == "u1" => Some(()),
        _ => None,
    });
    // An end already on its way when the failure was reported is ignored, not refused.
    ada.send(&ClientMessage::UtteranceEnd {
        utterance: "u1".into(),
    });
    // The client forgets it without cancelling, as the server has closed it; the next utterance neither fails it again nor is refused.
    ada.send(&ClientMessage::UtteranceBegin {
        utterance: "u2".into(),
    });
    speak(&mut ada, &["run", "the", "tests"]);
    ada.send(&ClientMessage::UtteranceEnd {
        utterance: "u2".into(),
    });
    let text = ada.wait_for(|message| match message {
        ServerMessage::TranscriptFailed { utterance, error } => {
            panic!("{utterance} failed: {error}")
        }
        ServerMessage::Error { error, .. } => panic!("{error}"),
        ServerMessage::Transcript {
            utterance,
            text,
            finished: true,
        } if utterance == "u2" => Some(text.clone()),
        _ => None,
    });
    assert!(text.contains("run the tests"), "{text}");
}

#[test]
fn a_crashed_recognizer_fails_the_utterance_instead_of_dropping_it() {
    let server = recognizing_server(FakeBackend::manual());
    let mut ada = server.connect("ada", "Ada");
    ada.create_space("Crash");
    ada.send(&ClientMessage::UtteranceBegin {
        utterance: "u1".into(),
    });
    speak(&mut ada, &["crash"]);
    ada.send(&ClientMessage::UtteranceEnd {
        utterance: "u1".into(),
    });
    let error = ada.wait_for(|message| match message {
        ServerMessage::TranscriptFailed { utterance, error } if utterance == "u1" => {
            Some(error.clone())
        }
        _ => None,
    });
    assert!(error.contains("stopped"), "{error}");

    // Later utterances fail visibly too, rather than waiting forever.
    ada.send(&ClientMessage::UtteranceBegin {
        utterance: "u2".into(),
    });
    speak(&mut ada, &["run"]);
    ada.send(&ClientMessage::UtteranceEnd {
        utterance: "u2".into(),
    });
    ada.wait_for(|message| match message {
        ServerMessage::TranscriptFailed { utterance, .. } if utterance == "u2" => Some(()),
        _ => None,
    });
}

#[test]
fn a_streamed_markdown_reply_is_spoken_to_every_participant_without_code() {
    let synthesizer = FakeSynthesizer::default();
    let server = speaking_server(FakeBackend::manual(), synthesizer.clone());
    let (mut ada, bob) = pair(&server);

    ada.say("u1", "how do I run it?");
    ada.space(|state| state.activity == protocol::Activity::Running);
    let session = server.backend.started()[0].clone();
    let started = Instant::now();
    let message = session.stream(&[
        "Here is the **plan**.\n\n```sh\ncargo run --",
        "release\n```\n\nRun it with `cargo`. Then ",
        "check the output.\n",
    ]);

    let expected = [
        "Here is the plan.",
        "Run it with cargo.",
        "Then check the output.",
    ];
    let frames = words(&expected) * FRAMES_PER_WORD;
    for client in [&ada, &bob] {
        let heard = hear(client, frames);
        // Synthesis is instant, but frames are sent at the rate they play, only slightly ahead.
        let duration = Duration::from_millis(20) * frames as u32;
        assert!(started.elapsed() + Duration::from_millis(150) >= duration);
        assert_stream(&heard.frames);
        let texts: Vec<&str> = heard
            .announcements
            .iter()
            .map(|(_, _, text)| text.as_str())
            .collect();
        assert_eq!(texts, expected);
        let segments: Vec<u32> = heard
            .announcements
            .iter()
            .map(|(segment, _, _)| *segment)
            .collect();
        assert_eq!(segments, [1, 2, 3]);
        assert!(heard.announcements.iter().all(|(_, id, _)| *id == message));
        // Nothing more follows.
        assert!(client.next_audio(Duration::from_millis(200)).is_none());
    }
    assert_eq!(*synthesizer.spoken.lock().unwrap(), expected);
}

#[test]
fn queueing_and_recording_during_a_reply_do_not_cut_off_speech() {
    let server = speaking_server(FakeBackend::manual(), FakeSynthesizer::default());
    let (mut ada, mut bob) = pair(&server);

    ada.say("u1", "tell me a story");
    ada.space(|state| state.activity == protocol::Activity::Running);
    let session = server.backend.started()[0].clone();
    let sentences = [
        "Once upon a time there was a server.",
        "It spoke every sentence it was given.",
        "Nobody could interrupt it by talking.",
        "And it lived happily ever after.",
    ];
    let message = session.start_message();
    session.delta(&message, &format!("{} {} ", sentences[0], sentences[1]));

    // Speech has started; now Bob queues input and Ada records while it plays.
    let started = hear(&bob, 1);
    bob.say("u2", "and then?");
    bob.space(|state| {
        state
            .queue
            .iter()
            .any(|entry| entry.text == "and then?" && entry.state == QueueState::Queued)
    });
    ada.send(&ClientMessage::UtteranceBegin {
        utterance: "u3".into(),
    });
    ada.send_audio(Gate::Agent, vec![0; 320]);
    ada.send(&ClientMessage::UtteranceCancel {
        utterance: "u3".into(),
    });

    session.delta(&message, &format!("{} {}", sentences[2], sentences[3]));
    session.emit(backend::Event::MessageCompleted {
        message: message.clone(),
    });
    session.emit(backend::Event::Idle);

    let frames = words(&sentences) * FRAMES_PER_WORD;
    let rest = hear(&bob, frames - 1);
    let stream: Vec<AudioFrame> = started.frames.into_iter().chain(rest.frames).collect();
    assert_stream(&stream);
    let texts: Vec<&str> = started
        .announcements
        .iter()
        .chain(&rest.announcements)
        .map(|(_, _, text)| text.as_str())
        .collect();
    assert_eq!(texts, sentences);

    let heard = hear(&ada, frames);
    assert_stream(&heard.frames);
    assert_eq!(heard.announcements.len(), sentences.len());

    // Bob's input was delivered once the turn ended.
    let prompts: Vec<String> = session
        .log()
        .prompts
        .into_iter()
        .map(|prompt| prompt.text)
        .collect();
    assert_eq!(prompts, ["Ada: tell me a story", "Bob: and then?"]);
}

#[test]
fn a_participant_who_joins_mid_reply_hears_the_rest() {
    let server = speaking_server(FakeBackend::manual(), FakeSynthesizer::default());
    let mut ada = server.connect("ada", "Ada");
    let space = ada.create_space("Late").id;
    ada.say("u1", "go on");
    ada.space(|state| state.activity == protocol::Activity::Running);
    let session = server.backend.started()[0].clone();
    let sentence =
        "This sentence is long enough that somebody can join while it is still being spoken aloud.";
    session.stream(&[sentence]);
    let frames = words(&[sentence]) * FRAMES_PER_WORD;

    let first = ada
        .next_audio(Duration::from_secs(5))
        .expect("speech starts");
    let mut bob = server.connect("bob", "Bob");
    bob.send(&ClientMessage::Join { space });
    let joined = bob
        .next_audio(Duration::from_secs(5))
        .expect("bob hears the rest");
    assert!(joined.sequence > first.sequence);
    let mut rest = vec![joined];
    while let Some(frame) = bob.next_audio(Duration::from_millis(300)) {
        rest.push(frame);
    }
    assert_stream(&rest);
    assert_eq!(rest.last().unwrap().sequence as usize, frames - 1);
    assert!(rest.len() < frames);
}
