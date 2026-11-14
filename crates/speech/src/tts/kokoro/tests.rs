// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

use super::*;

#[test]
fn converts_samples_to_pcm() {
    assert_eq!(
        to_pcm(&[0.0, 1.0, -1.0, 2.0, 0.5]),
        [0, 32767, -32767, 32767, 16384]
    );
}

#[test]
fn prepares_text_for_the_engine() {
    assert_eq!(prepare(" Hello\0there. "), Ok("Hello there.".to_owned()));
    let Err(Error::Engine(_)) = prepare("  ") else {
        unreachable!("expected Err(Error::Engine(_))");
    };
    let Err(Error::Engine(_)) = prepare(&"a".repeat(TEXT_LIMIT + 1)) else {
        unreachable!("expected Err(Error::Engine(_))");
    };
}

#[test]
fn reports_a_missing_installation() {
    let config = KokoroConfig::from_root(Path::new("/nonexistent"));
    let Err(Error::Unavailable(_)) = KokoroSynthesizer::new(&config) else {
        unreachable!("expected Err(Error::Unavailable(_))");
    };
}

#[test]
fn defaults_to_the_heart_voice() {
    // Speaker 3 of the Kokoro v1.0 voice table is `af_heart`.
    assert_eq!(KokoroConfig::from_root(Path::new("/")).voice, 3);
}

#[test]
fn rejects_an_unknown_voice() {
    let mut config = KokoroConfig::from_root(&crate::test_models::root());
    config.voice = 54;
    let Err(Error::Unavailable(message)) = KokoroSynthesizer::new(&config) else {
        unreachable!("an unknown voice is refused");
    };
    assert!(message.contains("voice"), "{message}");
}

/// Skipped without the Kokoro model; see [`crate::test_models`].
#[test]
fn synthesizes_sentences_as_they_are_generated() {
    let config = KokoroConfig::from_root(&crate::test_models::root());
    crate::test_models::with(
        "the Kokoro model",
        "bash scripts/fetch-tts-model.sh",
        &[&config.model],
        || {
            let mut synthesizer = KokoroSynthesizer::new(&config).unwrap();
            assert_eq!(synthesizer.speakers(), VOICES);
            let mut chunks = Vec::new();
            synthesizer
                .synthesize(
                    "Hello from Tomori. This is a second sentence.",
                    &mut |chunk| chunks.push(chunk),
                )
                .unwrap();
            // Each sentence arrives as its own chunk.
            let count = chunks.len();
            assert!(count >= 2, "{count} chunks");
            assert!(chunks.iter().all(|chunk| chunk.sample_rate == 24_000));
            let samples: usize = chunks.iter().map(|chunk| chunk.samples.len()).sum();
            assert!(samples > 24_000, "{samples}");
            // The synthesizer serves further segments.
            let mut more = 0;
            synthesizer
                .synthesize("Again.", &mut |chunk| more += chunk.samples.len())
                .unwrap();
            assert!(more > 0);
            // Stopping unloads the model and ends the thread; later segments fail.
            let stopper = synthesizer.stopper();
            stopper.stop();
            assert!(stopper.wait(std::time::Instant::now() + std::time::Duration::from_secs(5)));
            let mut ignore = drop::<AudioChunk>;
            let Err(Error::Stopped(_)) = synthesizer.synthesize("After.", &mut ignore) else {
                unreachable!("expected Err(Error::Stopped(_))");
            };
            // The model itself, as the synthesis thread asks it, given phonemes.
            let tts = OfflineTts::create(&config.sherpa()).unwrap();
            assert_eq!(Engine::sample_rate(&tts), 24_000);
            assert_eq!(Engine::num_speakers(&tts), VOICES as i32);
            let generation = GenerationConfig {
                phoneme_codepoints: Some(vec!["hˈI.".chars().map(|c| c as i32).collect()]),
                ..Default::default()
            };
            let audio = Engine::generate(&tts, &generation, |_, _| true).unwrap();
            assert!(!OfflineTts::samples(&audio).is_empty());
        },
    );
}

/// Skipped without the Kokoro model; see [`crate::test_models`].
#[test]
fn speaks_words_the_lexicon_lacks_and_nothing_for_an_emoji() {
    let config = KokoroConfig::from_root(&crate::test_models::root());
    crate::test_models::with(
        "the Kokoro model",
        "bash scripts/fetch-tts-model.sh",
        &[&config.model],
        || {
            let mut synthesizer = KokoroSynthesizer::new(&config).unwrap();
            let mut length = |text: &str| {
                let mut samples = 0;
                synthesizer
                    .synthesize(text, &mut |chunk| samples += chunk.samples.len())
                    .map(|()| samples)
            };
            // A name in no lexicon is pronounced, rather than skipped as sherpa-onnx's own lexicon would: it takes as long to say as a word of its length.
            let tomori = length("Tomori.").unwrap();
            assert!(tomori > length("Hi.").unwrap() + 2_400, "{tomori} samples");
            // An emoji alone is silent, and not an error.
            assert_eq!(length("🎉"), Ok(0));
        },
    );
}

#[test]
fn a_voice_whose_phonemes_cannot_be_read_is_said() {
    let installed = KokoroConfig::from_root(&crate::test_models::root());
    crate::test_models::with(
        "the Kokoro model",
        "bash scripts/fetch-tts-model.sh",
        &[&installed.model],
        || {
            // `tokens.txt` is there, but cannot be read as text.
            let partial =
                std::env::temp_dir().join(format!("tomori-kokoro-partial-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&partial);
            std::fs::create_dir_all(partial.join("tokens.txt")).unwrap();
            for file in FILES.iter().filter(|file| **file != "tokens.txt") {
                std::os::unix::fs::symlink(installed.model.join(file), partial.join(file)).unwrap();
            }
            let config = KokoroConfig {
                model: partial.clone(),
                ..installed.clone()
            };
            let Err(Error::Unavailable(error)) = KokoroSynthesizer::new(&config) else {
                unreachable!("the model is refused");
            };
            assert!(error.contains("could not be read"), "{error}");
            std::fs::remove_dir_all(partial).unwrap();
        },
    );
}

/// What the stand-in model does with a segment, given the callback for its sentences.
type Act = dyn Fn(&mut dyn FnMut(&[f32], f32) -> bool) -> Option<Vec<f32>> + Send + Sync;

/// A model that does what `act` says, at `rate` with `speakers` voices, keeping the phonemes it is given.
struct Fake {
    rate: i32,
    speakers: i32,
    act: Arc<Act>,
    heard: Arc<std::sync::Mutex<Vec<Vec<i32>>>>,
}

/// A phonemizer that reads each letter as itself, and the text as one sentence.
struct Letters;

impl Phonemizer for Letters {
    fn sentences(&self, text: &str) -> Vec<String> {
        vec![text.to_owned()]
    }
}

/// The model and how it reads, as [`KokoroSynthesizer::start`] loads them.
fn loaded(fake: Fake) -> Option<(Fake, Reading<Letters>)> {
    let symbols = ('a'..='z').chain('A'..='Z').chain([' ', '.']).collect();
    Some((
        fake,
        Reading {
            phonemizer: Letters,
            symbols,
        },
    ))
}

impl Engine for Fake {
    type Audio = Vec<f32>;

    fn sample_rate(&self) -> i32 {
        self.rate
    }

    fn num_speakers(&self) -> i32 {
        self.speakers
    }

    fn generate(
        &self,
        generation: &GenerationConfig,
        mut callback: impl FnMut(&[f32], f32) -> bool + 'static,
    ) -> Option<Vec<f32>> {
        *self.heard.lock().unwrap() = generation.phoneme_codepoints.clone().unwrap_or_default();
        (self.act)(&mut callback)
    }

    fn samples(audio: &Vec<f32>) -> &[f32] {
        audio
    }
}

fn faked(
    act: impl Fn(&mut dyn FnMut(&[f32], f32) -> bool) -> Option<Vec<f32>> + Send + Sync + 'static,
) -> KokoroSynthesizer {
    faked_hearing(act, Arc::default())
}

/// As [`faked`], keeping the phonemes the model is given in `heard`.
fn faked_hearing(
    act: impl Fn(&mut dyn FnMut(&[f32], f32) -> bool) -> Option<Vec<f32>> + Send + Sync + 'static,
    heard: Arc<std::sync::Mutex<Vec<Vec<i32>>>>,
) -> KokoroSynthesizer {
    let act: Arc<Act> = Arc::new(act);
    KokoroSynthesizer::start(
        move || {
            loaded(Fake {
                rate: 24_000,
                speakers: 4,
                act,
                heard,
            })
        },
        3,
        1.0,
    )
    .unwrap()
}

/// The samples of a segment, or why it failed.
fn spoken(synthesizer: &mut KokoroSynthesizer, text: &str) -> Result<Vec<i16>, Error> {
    let mut samples = Vec::new();
    synthesizer
        .synthesize(text, &mut |chunk| samples.extend(chunk.samples))
        .map(|()| samples)
}

#[test]
fn a_model_that_cannot_start_is_said() {
    let fake = |rate, speakers| {
        move || {
            loaded(Fake {
                rate,
                speakers,
                act: Arc::new(|_: &mut dyn FnMut(&[f32], f32) -> bool| None),
                heard: Arc::default(),
            })
        }
    };
    type Load = Box<dyn FnOnce() -> Option<(Fake, Reading<Letters>)> + Send>;
    for (load, said) in [
        (Box::new(|| None) as Load, "could not load the Kokoro model"),
        (
            Box::new(|| panic!("the model is broken")),
            "stopped while loading",
        ),
        (Box::new(fake(0, 4)), "no sample rate"),
        (Box::new(fake(24_000, 2)), "out of range"),
    ] {
        let Err(Error::Unavailable(error)) = KokoroSynthesizer::start(load, 3, 1.0) else {
            unreachable!("the model does not start");
        };
        assert!(error.contains(said), "{error}");
    }
}

#[test]
fn a_segment_streamed_whole_or_not_at_all_is_said_as_it_came() {
    // Streamed sentence by sentence, with an empty callback between them.
    let mut synthesizer = faked(|callback| {
        assert!(callback(&[], 0.0));
        assert!(callback(&[0.5; 3], 0.5));
        Some(vec![0.5; 3])
    });
    assert_eq!(synthesizer.sample_rate(), 24_000);
    assert_eq!(spoken(&mut synthesizer, "Hello.").unwrap(), [16384; 3]);
    let Err(Error::Engine(_)) = spoken(&mut synthesizer, "  ") else {
        unreachable!("an empty segment is refused");
    };
    // Given whole, without calling back.
    let mut whole = faked(|_| Some(vec![0.25; 2]));
    assert_eq!(spoken(&mut whole, "Hello.").unwrap(), [8192; 2]);
    // Nothing at all.
    for act in [
        Box::new(|_: &mut dyn FnMut(&[f32], f32) -> bool| Some(Vec::new())) as Box<Act>,
        Box::new(|_: &mut dyn FnMut(&[f32], f32) -> bool| None),
    ] {
        let mut silent = faked(act);
        let Err(Error::Engine(_)) = spoken(&mut silent, "Hello.") else {
            unreachable!("no audio is an error");
        };
    }
}

#[test]
fn the_model_is_given_the_phonemes_it_knows() {
    let heard = Arc::default();
    let mut synthesizer = faked_hearing(|_| Some(vec![0.5; 3]), Arc::clone(&heard));
    assert_eq!(spoken(&mut synthesizer, "Hi❓ there.").unwrap(), [16384; 3]);
    let expected: Vec<i32> = "Hi there.".chars().map(|c| c as i32).collect();
    assert_eq!(*heard.lock().unwrap(), [expected]);
    // Nothing it can pronounce is silent, without asking the model.
    let mut silent = faked(|_| unreachable!("nothing is synthesized"));
    assert_eq!(spoken(&mut silent, "🎉"), Ok(Vec::new()));
}

#[test]
fn a_model_that_panics_disables_synthesis() {
    log::set_max_level(log::LevelFilter::Trace);
    let mut synthesizer = faked(|_| panic!("the model broke"));
    let Err(Error::Stopped(_)) = spoken(&mut synthesizer, "Hello.") else {
        unreachable!("a panic stops synthesis");
    };
    // Once its thread has gone, later segments find nobody to ask.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    assert!(synthesizer.stopper().wait(deadline));
    let Err(Error::Stopped(_)) = spoken(&mut synthesizer, "Hello.") else {
        unreachable!("synthesis stays stopped");
    };
}

#[test]
fn stopping_ends_a_segment_being_synthesized_and_refuses_those_waiting() {
    let stopper: Arc<std::sync::Mutex<Option<Stopper>>> = Arc::default();
    let stopping = stopper.clone();
    let mut synthesizer = faked(move |callback| {
        stopping.lock().unwrap().as_ref().unwrap().stop();
        // The next sentence is not wanted.
        assert!(!callback(&[0.5; 3], 0.5));
        Some(vec![0.5; 3])
    });
    *stopper.lock().unwrap() = Some(synthesizer.stopper());
    let Err(Error::Stopped(_)) = spoken(&mut synthesizer, "Hello.") else {
        unreachable!("stopping ends the segment");
    };
    // A segment waiting when synthesis stops is refused.
    let mut waiting = faked(|_| unreachable!("nothing is synthesized while stopping"));
    waiting.stopping.store(true, Ordering::SeqCst);
    let Err(Error::Stopped(_)) = spoken(&mut waiting, "Hello.") else {
        unreachable!("a stopping synthesizer refuses segments");
    };
}
