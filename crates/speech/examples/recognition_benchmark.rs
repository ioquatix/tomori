// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

//! Compares speech recognizers on English clips: accuracy, time to first partial, real-time factor and CPU use.
//!
//! ```sh
//! cargo run --release -p speech --example recognition_benchmark -- --root . [--model DIRECTORY]... [--whisper] [--punctuation]
//! ```
//!
//! Clips are the LibriSpeech test wavs shipped with the default streaming model, and sentences synthesized with Kokoro in four voices. Each streaming model is fed 100 ms frames in real time through [`Recognition`], as push-to-talk delivers them, and then again as fast as it can decode. Whisper, which cannot stream, decodes each whole clip; its first partial is estimated as a 1 second refresh interval, as a windowed re-decoding scheme would use, plus the time to decode that first second.
//!
//! With `--punctuation`, the streaming models' final transcripts are punctuated as the server punctuates them, and the time each punctuation takes is reported. Word errors ignore case and punctuation either way.

use std::{
    path::{Path, PathBuf},
    sync::{Arc, Mutex, mpsc},
    thread,
    time::{Duration, Instant},
};

use speech::{
    Error, Recognition, StreamingRecognizer, Synthesizer, TranscriptEvent,
    stt::{
        CnnBilstmPunctuator, Punctuated, PunctuationConfig, Punctuator, RATE, Resampler,
        TransducerConfig, TransducerRecognizer, WhisperConfig, WhisperRecognizer,
    },
    tts::{KokoroConfig, KokoroSynthesizer},
};

/// Kokoro v1.0 voices: `af_heart`, `am_michael`, `bf_emma` and `bm_george`.
const VOICES: [(u32, &str); 4] = [
    (3, "af_heart"),
    (16, "am_michael"),
    (21, "bf_emma"),
    (26, "bm_george"),
];

const SENTENCES: [&str; 4] = [
    "Please open the server crate and run the tests again.",
    "Can you explain why the build failed on the Linux machine yesterday?",
    "Move the speech settings into the configuration file and commit the change.",
    "What is the weather like in Tokyo this weekend?",
];

const FRAME: usize = RATE as usize / 10;

/// A [`Punctuator`] that records how long each call takes, in seconds.
struct Timed {
    punctuator: CnnBilstmPunctuator,
    times: Arc<Mutex<Vec<f64>>>,
}

impl Punctuator for Timed {
    fn punctuate(&mut self, text: &str) -> Result<String, Error> {
        let started = Instant::now();
        let result = self.punctuator.punctuate(text);
        self.times
            .lock()
            .unwrap()
            .push(started.elapsed().as_secs_f64());
        result
    }
}

struct Clip {
    name: String,
    reference: String,
    audio: Vec<i16>,
}

impl Clip {
    fn seconds(&self) -> f64 {
        self.audio.len() as f64 / f64::from(RATE)
    }
}

fn resample(rate: u32, samples: &[i16]) -> Vec<i16> {
    let mut output = Vec::new();
    Resampler::new(rate, RATE).process(samples, &mut output);
    output
}

fn librispeech(directory: &Path) -> Vec<Clip> {
    let Ok(transcripts) = std::fs::read_to_string(directory.join("test_wavs/trans.txt")) else {
        return Vec::new();
    };
    transcripts
        .lines()
        .filter_map(|line| {
            let (file, reference) = line.split_once(' ')?;
            let path = directory.join("test_wavs").join(file);
            let wave = sherpa_onnx::Wave::read(path.to_str()?)?;
            let samples: Vec<i16> = wave
                .samples()
                .iter()
                .map(|sample| (sample.clamp(-1.0, 1.0) * 32767.0) as i16)
                .collect();
            Some(Clip {
                name: format!("librispeech/{file}"),
                reference: reference.to_owned(),
                audio: resample(wave.sample_rate() as u32, &samples),
            })
        })
        .collect()
}

fn synthesized(root: &Path) -> Vec<Clip> {
    let mut clips = Vec::new();
    for (index, (voice, name)) in VOICES.iter().enumerate() {
        let mut config = KokoroConfig::from_root(root);
        config.voice = *voice;
        let mut kokoro = KokoroSynthesizer::new(&config).expect("load Kokoro");
        // Each voice speaks two sentences, so every sentence is heard in two voices.
        for sentence in [SENTENCES[index], SENTENCES[(index + 1) % SENTENCES.len()]] {
            let mut audio = Vec::new();
            kokoro
                .synthesize(sentence, &mut |chunk| {
                    audio.extend(resample(chunk.sample_rate, &chunk.samples))
                })
                .expect("synthesize");
            clips.push(Clip {
                name: format!("kokoro/{name}"),
                reference: sentence.to_owned(),
                audio,
            });
        }
    }
    clips
}

fn words(text: &str) -> Vec<String> {
    text.to_lowercase()
        .chars()
        .map(|character| {
            if character.is_alphanumeric() || character == '\'' {
                character
            } else {
                ' '
            }
        })
        .collect::<String>()
        .split_whitespace()
        .map(str::to_owned)
        .collect()
}

/// Word errors (substitutions, insertions and deletions) and reference words.
fn word_errors(reference: &str, hypothesis: &str) -> (usize, usize) {
    let reference = words(reference);
    let hypothesis = words(hypothesis);
    let mut previous: Vec<usize> = (0..=hypothesis.len()).collect();
    for (row, expected) in reference.iter().enumerate() {
        let mut current = vec![row + 1; hypothesis.len() + 1];
        for (column, heard) in hypothesis.iter().enumerate() {
            let substitution = previous[column] + usize::from(expected != heard);
            current[column + 1] = substitution
                .min(previous[column + 1] + 1)
                .min(current[column] + 1);
        }
        previous = current;
    }
    (previous[hypothesis.len()], reference.len())
}

/// Processor time used by this process so far.
fn processor_time() -> Duration {
    let mut usage = std::mem::MaybeUninit::<libc::rusage>::zeroed();
    // SAFETY: `getrusage` fills the structure it is given.
    let usage = unsafe {
        libc::getrusage(libc::RUSAGE_SELF, usage.as_mut_ptr());
        usage.assume_init()
    };
    let time = |value: libc::timeval| {
        Duration::from_secs(value.tv_sec as u64) + Duration::from_micros(value.tv_usec as u64)
    };
    time(usage.ru_utime) + time(usage.ru_stime)
}

fn load_average() -> f64 {
    let mut loads = [0.0f64; 3];
    // SAFETY: the array holds the one value requested.
    unsafe { libc::getloadavg(loads.as_mut_ptr(), 1) };
    loads[0]
}

#[derive(Default)]
struct Totals {
    errors: usize,
    reference_words: usize,
    first_partials: Vec<f64>,
    finals: Vec<f64>,
    audio: f64,
    processor: f64,
    fast_wall: f64,
    /// Seconds taken by each punctuation of a final transcript.
    punctuation: Vec<f64>,
    /// Synthesized clips whose final transcript matches the punctuated reference exactly.
    exact: usize,
    synthesized: usize,
}

impl Totals {
    fn row(&self, name: &str, threads: u32, load: f64) -> String {
        let mean = |values: &[f64]| values.iter().sum::<f64>() / values.len().max(1) as f64;
        let maximum = |values: &[f64]| values.iter().cloned().fold(0.0, f64::max);
        format!(
            "| {name} | {:.1}% ({}/{}) | {:.2} s (max {:.2}) | {:.2} s (max {:.2}) | {:.2} | {:.2} | {threads} | {load:.0} |",
            100.0 * self.errors as f64 / self.reference_words.max(1) as f64,
            self.errors,
            self.reference_words,
            mean(&self.first_partials),
            maximum(&self.first_partials),
            mean(&self.finals),
            maximum(&self.finals),
            self.fast_wall / self.audio,
            self.processor / self.audio,
        )
    }

    fn punctuation(&self, name: &str) -> Option<String> {
        if self.punctuation.is_empty() {
            return None;
        }
        let milliseconds: Vec<f64> = self.punctuation.iter().map(|time| time * 1000.0).collect();
        Some(format!(
            "{name}: punctuation took {:.1} ms on average (max {:.1}); {}/{} synthesized sentences match their reference exactly",
            milliseconds.iter().sum::<f64>() / milliseconds.len() as f64,
            milliseconds.iter().cloned().fold(0.0, f64::max),
            self.exact,
            self.synthesized,
        ))
    }
}

/// Feed `clip` to `recognition` in 100 ms frames, in real time, and return the times of the first non-empty partial and of the final transcript after the end, with the final text.
fn run(recognition: &mut Recognition, clip: &Clip) -> (Option<f64>, f64, String) {
    let (sender, events) = mpsc::channel();
    let mut utterance = recognition.begin(0, RATE, move |event| {
        let _ = sender.send((Instant::now(), event));
    });
    let start = Instant::now();
    for (index, frame) in clip.audio.chunks(FRAME).enumerate() {
        let due = start + Duration::from_millis(100 * index as u64);
        thread::sleep(due.saturating_duration_since(Instant::now()));
        utterance.push(frame);
    }
    thread::sleep(
        (start + Duration::from_secs_f64(clip.seconds())).saturating_duration_since(Instant::now()),
    );
    let ended = Instant::now();
    utterance.end();
    let mut first = None;
    loop {
        let (time, event) = events
            .recv_timeout(Duration::from_secs(300))
            .expect("recognition finishes");
        match event {
            TranscriptEvent::Partial(transcript) => {
                if first.is_none()
                    && !(transcript.committed.is_empty() && transcript.tentative.is_empty())
                {
                    first = Some((time - start).as_secs_f64());
                }
            }
            TranscriptEvent::Final(text) => {
                return (first, (time - ended).as_secs_f64(), text);
            }
            TranscriptEvent::Failed(error) => panic!("{} failed: {error}", clip.name),
        }
    }
}

/// Decode `clip` with `recognizer` directly, as fast as it can, and return the final text.
///
/// This bypasses [`Recognition`], which fails an utterance whose audio arrives more than [`speech::stt::BACKLOG`] ahead of decoding.
fn decode_at_once(recognizer: &mut dyn StreamingRecognizer, clip: &Clip) -> String {
    let mut stream = recognizer.open().expect("open a stream");
    for frame in clip.audio.chunks(FRAME) {
        stream.accept(frame);
        while stream.due() {
            stream.decode().expect("decode");
        }
    }
    stream.finish().expect("finish")
}

fn streaming(
    directory: &Path,
    clips: &[Clip],
    threads: u32,
    punctuation: Option<&PunctuationConfig>,
) -> Totals {
    let mut config = TransducerConfig::with_model(directory);
    config.threads = threads;
    let load = |times: &Arc<Mutex<Vec<f64>>>| -> Box<dyn StreamingRecognizer> {
        let loaded = Instant::now();
        let recognizer = TransducerRecognizer::new(&config).expect("load the streaming model");
        eprintln!("Loaded {} in {:?}", directory.display(), loaded.elapsed());
        match punctuation {
            Some(punctuation) => {
                let punctuator =
                    CnnBilstmPunctuator::new(punctuation).expect("load the punctuation model");
                Box::new(Punctuated::new(
                    recognizer,
                    Timed {
                        punctuator,
                        times: Arc::clone(times),
                    },
                ))
            }
            None => Box::new(recognizer),
        }
    };
    let times = Arc::new(Mutex::new(Vec::new()));
    let mut recognition = Recognition::spawn(load(&times));
    // A second instance for decoding as fast as possible, whose punctuation times are not reported.
    let mut fast_recognizer = load(&Arc::default());
    let mut totals = Totals::default();
    for clip in clips {
        let processor = processor_time();
        let (first, last, text) = run(&mut recognition, clip);
        totals.processor += (processor_time() - processor).as_secs_f64();
        let fast = Instant::now();
        let fast_text = decode_at_once(&mut *fast_recognizer, clip);
        totals.fast_wall += fast.elapsed().as_secs_f64();
        assert_eq!(
            text, fast_text,
            "streaming results depend only on the audio"
        );
        let (errors, reference_words) = word_errors(&clip.reference, &text);
        eprintln!(
            "  {:<22} first {:>5} final +{last:.2}s errors {errors}/{reference_words}: {text}",
            clip.name,
            first.map_or("-".into(), |first| format!("{first:.2}s"))
        );
        totals.errors += errors;
        totals.reference_words += reference_words;
        totals.first_partials.extend(first);
        totals.finals.push(last);
        totals.audio += clip.seconds();
        if clip.name.starts_with("kokoro/") {
            totals.synthesized += 1;
            totals.exact += usize::from(text == clip.reference);
        }
    }
    totals.punctuation = std::mem::take(&mut *times.lock().unwrap());
    totals
}

fn whisper(root: &Path, clips: &[Clip], threads: u32) -> Totals {
    let mut config = WhisperConfig::from_root(root);
    config.threads = threads;
    let mut whisper = WhisperRecognizer::new(&config).expect("load Whisper");
    let mut totals = Totals::default();
    for clip in clips {
        let processor = processor_time();
        let started = Instant::now();
        let first_second = &clip.audio[..clip.audio.len().min(RATE as usize)];
        whisper.transcribe(first_second).expect("decode");
        let first = 1.0 + started.elapsed().as_secs_f64();
        let decode = Instant::now();
        let text = whisper.transcribe(&clip.audio).expect("decode");
        let wall = decode.elapsed().as_secs_f64();
        totals.processor += (processor_time() - processor).as_secs_f64();
        let (errors, reference_words) = word_errors(&clip.reference, &text);
        eprintln!(
            "  {:<22} first {first:.2}s whole clip {wall:.2}s errors {errors}/{reference_words}: {text}",
            clip.name
        );
        totals.errors += errors;
        totals.reference_words += reference_words;
        totals.first_partials.push(first);
        // Without word times, finishing decodes the whole utterance.
        totals.finals.push(wall);
        totals.fast_wall += wall;
        totals.audio += clip.seconds();
    }
    // Processor time covers the first-second decode as well as the whole clip.
    totals
}

fn main() {
    let mut root = PathBuf::from(".");
    let mut models = Vec::new();
    let mut with_whisper = false;
    let mut with_punctuation = false;
    let mut threads = 2;
    let mut arguments = std::env::args().skip(1);
    while let Some(argument) = arguments.next() {
        match argument.as_str() {
            "--root" => root = arguments.next().expect("--root needs a directory").into(),
            "--model" => models.push(PathBuf::from(
                arguments.next().expect("--model needs a directory"),
            )),
            "--whisper" => with_whisper = true,
            "--punctuation" => with_punctuation = true,
            "--threads" => {
                threads = arguments
                    .next()
                    .and_then(|value| value.parse().ok())
                    .expect("--threads needs a count")
            }
            other => panic!("unknown argument {other:?}"),
        }
    }
    if models.is_empty() {
        models.push(TransducerConfig::from_root(&root).model);
    }
    let mut clips = librispeech(&TransducerConfig::from_root(&root).model);
    clips.extend(synthesized(&root));
    let audio: f64 = clips.iter().map(Clip::seconds).sum();
    eprintln!("{} clips, {audio:.1} s of audio", clips.len());

    let punctuation = with_punctuation.then(|| PunctuationConfig::from_root(&root));
    let mut rows = Vec::new();
    let mut notes = Vec::new();
    for model in &models {
        let load = load_average();
        let totals = streaming(model, &clips, threads, punctuation.as_ref());
        let name = model.file_name().unwrap_or_default().to_string_lossy();
        rows.push(totals.row(&name, threads, load));
        notes.extend(totals.punctuation(&name));
    }
    if with_whisper {
        let load = load_average();
        let totals = whisper(&root, &clips, threads.max(4));
        rows.push(totals.row("whisper small.en (int8, whole clip)", threads.max(4), load));
    }
    println!(
        "| Model | Word error rate | First partial | Final after end | Real-time factor | Processor seconds per audio second | Threads | Load average |"
    );
    println!("|---|---|---|---|---|---|---|---|");
    for row in rows {
        println!("{row}");
    }
    for note in notes {
        println!("\n{note}");
    }
}
