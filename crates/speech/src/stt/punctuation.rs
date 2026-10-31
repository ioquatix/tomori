// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

//! Restoring capitals and punctuation in final transcripts.
//!
//! The streaming transducer's vocabulary has no capitals or punctuation, so its transcripts are lowercase and unpunctuated. [`Punctuated`] wraps a [`StreamingRecognizer`] and passes each utterance's final transcript through a [`Punctuator`] on the recognition thread. Partial transcripts are left as they are: punctuation and capitals depend on the words that follow, so punctuating partials would change words already shown as committed.
//!
//! Punctuation never fails an utterance. If the punctuator returns an error, or changes the words rather than only their case and punctuation, the transcript is delivered as recognized. A panic in the punctuator disables punctuation until the process restarts, logged once, and later transcripts are delivered as recognized.

use std::{
    panic::{self, AssertUnwindSafe},
    path::{Path, PathBuf},
    sync::{Arc, Mutex, PoisonError},
};

use sherpa_onnx::{OnlinePunctuation, OnlinePunctuationConfig, OnlinePunctuationModelConfig};

use super::{RecognitionStream, StreamingRecognizer, Transcript};
use crate::{Error, stop::panic_message};

/// The default punctuation model, fetched by `scripts/fetch-asr-model.sh`.
pub const DEFAULT_PUNCTUATION_MODEL: &str = "sherpa-onnx-online-punct-en-2024-08-06";

/// Adds capitals and punctuation to a lowercase, unpunctuated transcript.
pub trait Punctuator: Send {
    /// The same words as `text`, with capitals and punctuation restored.
    fn punctuate(&mut self, text: &str) -> Result<String, Error>;
}

impl<P: Punctuator + ?Sized> Punctuator for Box<P> {
    fn punctuate(&mut self, text: &str) -> Result<String, Error> {
        (**self).punctuate(text)
    }
}

/// The punctuator shared by an engine's streams, until a panic disables it.
type Shared = Arc<Mutex<Option<Box<dyn Punctuator>>>>;

/// A [`StreamingRecognizer`] whose final transcripts are punctuated by a [`Punctuator`].
pub struct Punctuated<R> {
    recognizer: R,
    punctuator: Shared,
}

impl<R: StreamingRecognizer> Punctuated<R> {
    /// Punctuate the final transcripts of `recognizer` with `punctuator`.
    pub fn new(recognizer: R, punctuator: impl Punctuator + 'static) -> Self {
        Self {
            recognizer,
            punctuator: Arc::new(Mutex::new(Some(Box::new(punctuator)))),
        }
    }
}

impl<R: StreamingRecognizer> StreamingRecognizer for Punctuated<R> {
    fn open(&mut self) -> Result<Box<dyn RecognitionStream>, Error> {
        Ok(Box::new(PunctuatedStream {
            stream: self.recognizer.open()?,
            punctuator: Arc::clone(&self.punctuator),
        }))
    }
}

struct PunctuatedStream {
    stream: Box<dyn RecognitionStream>,
    punctuator: Shared,
}

impl RecognitionStream for PunctuatedStream {
    fn accept(&mut self, samples: &[i16]) {
        self.stream.accept(samples);
    }

    fn due(&self) -> bool {
        self.stream.due()
    }

    fn decode(&mut self) -> Result<(), Error> {
        self.stream.decode()
    }

    fn transcript(&self) -> Transcript {
        self.stream.transcript()
    }

    fn finish(&mut self) -> Result<String, Error> {
        let text = self.stream.finish()?;
        Ok(punctuate(&self.punctuator, text))
    }
}

/// `text` punctuated, or as it is if punctuation is disabled or fails.
fn punctuate(shared: &Shared, text: String) -> String {
    if text.trim().is_empty() {
        return text;
    }
    let mut punctuator = shared.lock().unwrap_or_else(PoisonError::into_inner);
    let Some(active) = punctuator.as_mut() else {
        return text;
    };
    match panic::catch_unwind(AssertUnwindSafe(|| active.punctuate(&text))) {
        Ok(Ok(punctuated)) if words(&punctuated) == words(&text) => end_sentence(&punctuated),
        Ok(Ok(punctuated)) => {
            log::warn!(
                "Transcript punctuation changed the words ({} to {}), so the transcript is sent as recognized.",
                words(&text).len(),
                words(&punctuated).len()
            );
            text
        }
        Ok(Err(error)) => {
            log::warn!(
                "Transcript punctuation failed ({error}), so the transcript is sent as recognized."
            );
            text
        }
        Err(panic) => {
            log::error!(
                "Transcript punctuation stopped after a panic ({}); transcripts are lowercase and unpunctuated until the server restarts.",
                panic_message(&*panic)
            );
            let disabled = punctuator.take();
            // Unloading the punctuator may panic again; that must not fail the utterance.
            let _ = panic::catch_unwind(AssertUnwindSafe(move || drop(disabled)));
            text
        }
    }
}

/// The words of `text`, ignoring case and punctuation, to check that punctuation changed nothing else.
fn words(text: &str) -> Vec<String> {
    text.split_whitespace()
        .map(|word| {
            word.chars()
                .filter(|character| character.is_alphanumeric() || *character == '\'')
                .flat_map(char::to_lowercase)
                .collect::<String>()
        })
        .filter(|word| !word.is_empty())
        .collect()
}

/// End `text` with a full stop, question mark or exclamation mark: a whole utterance is at least one sentence. A trailing comma, colon or semicolon becomes a full stop.
fn end_sentence(text: &str) -> String {
    let mut text = text.trim_end().to_owned();
    match text.chars().last() {
        Some('.' | '?' | '!') | None => {}
        Some(',' | ';' | ':') => {
            text.pop();
            text.push('.');
        }
        Some(_) => text.push('.'),
    }
    text
}

/// Where to find the English punctuation and casing model, a CNN-BiLSTM from sherpa-onnx's punctuation models.
#[derive(Clone, Debug)]
pub struct PunctuationConfig {
    /// A directory holding `model.int8.onnx` and `bpe.vocab`.
    pub model: PathBuf,
}

impl PunctuationConfig {
    /// The default model under the repository `root`: `.tools/asr/` followed by [`DEFAULT_PUNCTUATION_MODEL`].
    pub fn from_root(root: &Path) -> Self {
        Self {
            model: root.join(".tools/asr").join(DEFAULT_PUNCTUATION_MODEL),
        }
    }
}

/// A word appended to the text before punctuating it, then removed.
///
/// The model decides the punctuation after a word from the words around it, so it rarely punctuates the last word of its input: "run the tests" comes back as "Run the tests", and "can you explain why the build failed" without its question mark. Followed by "i", which usually begins a new sentence, the last word gets the punctuation it would have mid-text.
const PROBE: &str = "i";

/// A [`Punctuator`] backed by sherpa-onnx's English CNN-BiLSTM punctuation and casing model, on the CPU, in this process.
pub struct CnnBilstmPunctuator {
    punctuation: OnlinePunctuation,
}

impl CnnBilstmPunctuator {
    /// Load the model, once, when the server starts.
    pub fn new(config: &PunctuationConfig) -> Result<Self, Error> {
        let model = config.model.join("model.int8.onnx");
        let vocabulary = config.model.join("bpe.vocab");
        if !model.exists() || !vocabulary.exists() {
            return Err(Error::Unavailable(format!(
                "the punctuation model is not installed in {}; run bash scripts/fetch-asr-model.sh",
                config.model.display()
            )));
        }
        let punctuation = OnlinePunctuation::create(&OnlinePunctuationConfig {
            model: OnlinePunctuationModelConfig {
                cnn_bilstm: Some(model.to_string_lossy().into_owned()),
                bpe_vocab: Some(vocabulary.to_string_lossy().into_owned()),
                num_threads: 1,
                provider: Some("cpu".into()),
                ..Default::default()
            },
        })
        .ok_or_else(|| {
            Error::Unavailable(format!(
                "sherpa-onnx could not load the punctuation model in {}",
                config.model.display()
            ))
        })?;
        Ok(Self { punctuation })
    }

    fn add(&self, text: &str) -> Result<String, Error> {
        self.punctuation
            .add_punctuation(text)
            .ok_or_else(|| Error::Engine("the punctuation model returned no result".into()))
    }
}

impl Punctuator for CnnBilstmPunctuator {
    fn punctuate(&mut self, text: &str) -> Result<String, Error> {
        let probed = self.add(&format!("{} {PROBE}", text.trim_end()))?;
        match without_probe(&probed) {
            Some(punctuated) => Ok(punctuated.to_owned()),
            None => self.add(text),
        }
    }
}

/// `text` without its last word, if that word is [`PROBE`] with any capitals or punctuation.
fn without_probe(text: &str) -> Option<&str> {
    let (rest, last) = text.trim_end().rsplit_once(char::is_whitespace)?;
    last.trim_matches(|character: char| !character.is_alphanumeric())
        .eq_ignore_ascii_case(PROBE)
        .then(|| rest.trim_end())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Recognition, TranscriptEvent, stt::RATE};
    use std::{
        sync::{
            atomic::{AtomicUsize, Ordering},
            mpsc,
        },
        time::Duration,
    };

    /// A recognizer that hears nothing but returns `text` when each utterance finishes, with the transcript `text` throughout.
    struct Fixed(&'static str);

    impl StreamingRecognizer for Fixed {
        fn open(&mut self) -> Result<Box<dyn RecognitionStream>, Error> {
            Ok(Box::new(FixedStream(self.0)))
        }
    }

    struct FixedStream(&'static str);

    impl RecognitionStream for FixedStream {
        fn accept(&mut self, _samples: &[i16]) {}

        fn due(&self) -> bool {
            false
        }

        fn decode(&mut self) -> Result<(), Error> {
            Ok(())
        }

        fn transcript(&self) -> Transcript {
            Transcript {
                committed: String::new(),
                tentative: self.0.to_owned(),
            }
        }

        fn finish(&mut self) -> Result<String, Error> {
            Ok(self.0.to_owned())
        }
    }

    /// A punctuator that behaves as its function says, counting calls.
    struct Fake<F> {
        respond: F,
        calls: Arc<AtomicUsize>,
    }

    impl<F: FnMut(&str) -> Result<String, Error> + Send> Punctuator for Fake<F> {
        fn punctuate(&mut self, text: &str) -> Result<String, Error> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            (self.respond)(text)
        }
    }

    fn fake(
        respond: impl FnMut(&str) -> Result<String, Error> + Send + 'static,
    ) -> (impl Punctuator, Arc<AtomicUsize>) {
        let calls = Arc::new(AtomicUsize::new(0));
        (
            Fake {
                respond,
                calls: Arc::clone(&calls),
            },
            calls,
        )
    }

    /// Capitalizes the first letter, as a model would.
    fn capitalize(text: &str) -> Result<String, Error> {
        let mut characters = text.chars();
        Ok(characters
            .next()
            .map(|first| first.to_uppercase().chain(characters).collect())
            .unwrap_or_default())
    }

    /// The final transcript of one utterance of `recognizer`.
    fn transcribe(recognition: &mut Recognition) -> String {
        let (sender, receiver) = mpsc::channel();
        let utterance = recognition.begin(1, RATE, move |event| {
            let _ = sender.send(event);
        });
        utterance.end();
        loop {
            match receiver.recv_timeout(Duration::from_secs(5)).unwrap() {
                TranscriptEvent::Partial(_) => {}
                TranscriptEvent::Final(text) => return text,
                TranscriptEvent::Failed(error) => panic!("{error}"),
            }
        }
    }

    #[test]
    fn punctuates_the_final_transcript() {
        let (punctuator, _) = fake(|text| Ok(format!("{}?", capitalize(text)?)));
        let mut recognition =
            Recognition::spawn(Punctuated::new(Fixed("can you run the tests"), punctuator));
        assert_eq!(transcribe(&mut recognition), "Can you run the tests?");
    }

    #[test]
    fn leaves_partial_transcripts_alone() {
        let (punctuator, calls) = fake(capitalize);
        let mut recognizer = Punctuated::new(Fixed("run the tests"), punctuator);
        let mut stream = recognizer.open().unwrap();
        stream.decode().unwrap();
        assert_eq!(stream.transcript().tentative, "run the tests");
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        assert_eq!(stream.finish().unwrap(), "Run the tests.");
    }

    #[test]
    fn ends_every_transcript_with_a_sentence_mark() {
        assert_eq!(end_sentence("Run the tests"), "Run the tests.");
        assert_eq!(end_sentence("Yes, "), "Yes.");
        assert_eq!(end_sentence("Can I;"), "Can I.");
        assert_eq!(end_sentence("Really?"), "Really?");
        assert_eq!(end_sentence("Stop!"), "Stop!");
        assert_eq!(end_sentence("Done."), "Done.");
    }

    #[test]
    fn does_not_punctuate_an_empty_transcript() {
        let (punctuator, calls) = fake(capitalize);
        let mut recognition = Recognition::spawn(Punctuated::new(Fixed(""), punctuator));
        assert_eq!(transcribe(&mut recognition), "");
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn a_failure_sends_the_transcript_as_recognized() {
        let (punctuator, calls) = fake(|_| Err(Error::Engine("no result".into())));
        let mut recognition =
            Recognition::spawn(Punctuated::new(Fixed("run the tests"), punctuator));
        assert_eq!(transcribe(&mut recognition), "run the tests");
        // A failure affects only its own transcript.
        assert_eq!(transcribe(&mut recognition), "run the tests");
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn changed_words_send_the_transcript_as_recognized() {
        let (punctuator, _) = fake(|_| Ok("Run the test.".into()));
        let mut recognition =
            Recognition::spawn(Punctuated::new(Fixed("run the tests"), punctuator));
        assert_eq!(transcribe(&mut recognition), "run the tests");
    }

    #[test]
    fn a_panic_disables_punctuation_but_not_recognition() {
        let (punctuator, calls) = fake(|_| panic!("the punctuator crashed"));
        let mut recognition =
            Recognition::spawn(Punctuated::new(Fixed("run the tests"), punctuator));
        assert_eq!(transcribe(&mut recognition), "run the tests");
        // Later transcripts are sent as recognized without reaching the punctuator.
        assert_eq!(transcribe(&mut recognition), "run the tests");
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    /// A punctuator that records the thread it is dropped on.
    struct Unloaded(Arc<Mutex<Option<std::thread::ThreadId>>>);

    impl Punctuator for Unloaded {
        fn punctuate(&mut self, text: &str) -> Result<String, Error> {
            Ok(text.to_owned())
        }
    }

    impl Drop for Unloaded {
        fn drop(&mut self) {
            *self.0.lock().unwrap() = Some(std::thread::current().id());
        }
    }

    #[test]
    fn stopping_unloads_the_punctuator_on_the_recognition_thread() {
        let unloaded = Arc::new(Mutex::new(None));
        let recognition = Recognition::spawn(Punctuated::new(
            Fixed("run the tests"),
            Unloaded(Arc::clone(&unloaded)),
        ));
        let stopper = recognition.stopper();
        stopper.stop();
        assert!(stopper.wait(std::time::Instant::now() + Duration::from_secs(5)));
        let thread = unloaded
            .lock()
            .unwrap()
            .expect("the punctuator was unloaded");
        assert_ne!(thread, std::thread::current().id());
    }

    #[test]
    fn removes_the_probe_word() {
        assert_eq!(without_probe("Run the tests. I"), Some("Run the tests."));
        assert_eq!(without_probe("Can I, I,"), Some("Can I,"));
        assert_eq!(without_probe("Run the tests"), None);
        assert_eq!(without_probe("I"), None);
    }

    #[test]
    fn reports_a_missing_model() {
        let config = PunctuationConfig::from_root(Path::new("/nonexistent"));
        assert!(matches!(
            CnnBilstmPunctuator::new(&config),
            Err(Error::Unavailable(message)) if message.contains("fetch-asr-model")
        ));
    }

    fn repository() -> PathBuf {
        match std::env::var_os("TOMORI_SPEECH_ROOT") {
            Some(root) => PathBuf::from(root),
            None => Path::new(env!("CARGO_MANIFEST_DIR")).join("../.."),
        }
    }

    /// Requires the punctuation model; see `readme.md`. Set `TOMORI_SPEECH_ROOT` to use a model outside this checkout.
    #[test]
    #[ignore = "requires the punctuation model"]
    fn punctuates_with_the_real_model() {
        let punctuator =
            CnnBilstmPunctuator::new(&PunctuationConfig::from_root(&repository())).unwrap();
        let mut recognition = Recognition::spawn(Punctuated::new(
            Fixed("how are you i am fine thank you"),
            punctuator,
        ));
        assert_eq!(
            transcribe(&mut recognition),
            "How are you? I am fine. Thank you."
        );
        let punctuator =
            CnnBilstmPunctuator::new(&PunctuationConfig::from_root(&repository())).unwrap();
        let mut recognition = Recognition::spawn(Punctuated::new(
            Fixed("can you explain why the build failed on the linux machine yesterday"),
            punctuator,
        ));
        assert_eq!(
            transcribe(&mut recognition),
            "Can you explain why the build failed on the Linux machine yesterday?"
        );
    }
}
