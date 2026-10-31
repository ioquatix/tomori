// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

//! Incremental decoding with local-agreement commit, for one utterance.
//!
//! The uncommitted audio is decoded periodically. Words at the start of two consecutive hypotheses that agree are committed and their audio is dropped, so the final decode only covers the uncommitted tail.

use super::{RATE, Recognizer, RecognizerConfig, Word};
use crate::Error;

/// The transcript so far: committed words that will not change, and the latest tentative words after them.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Transcript {
    pub committed: String,
    pub tentative: String,
}

pub(crate) struct Policy {
    config: RecognizerConfig,
    /// Uncommitted 16 kHz audio.
    window: Vec<i16>,
    /// Samples received since the last decode.
    pending: usize,
    committed: Vec<String>,
    /// The previous hypothesis for the current window, with times relative to its start.
    previous: Vec<Word>,
    tentative: Vec<Word>,
    /// Total samples dropped from the front of the window, for diagnostics and tests.
    dropped: usize,
}

impl Policy {
    pub fn new(config: RecognizerConfig) -> Self {
        Self {
            config,
            window: Vec::new(),
            pending: 0,
            committed: Vec::new(),
            previous: Vec::new(),
            tentative: Vec::new(),
            dropped: 0,
        }
    }

    pub fn push(&mut self, samples: &[i16]) {
        self.window.extend_from_slice(samples);
        self.pending += samples.len();
    }

    /// Whether enough new audio has arrived for another partial decode.
    pub fn due(&self) -> bool {
        self.pending > 0
            && (self.pending >= samples(self.config.refresh)
                || self.window.len() >= samples(self.config.maximum_window))
    }

    pub fn transcript(&self) -> Transcript {
        Transcript {
            committed: join(self.committed.iter().map(String::as_str)),
            tentative: join(self.tentative.iter().map(|word| word.text.as_str())),
        }
    }

    /// The offset in samples of the uncommitted window from the start of the utterance.
    #[cfg(test)]
    pub fn window_start(&self) -> usize {
        self.dropped
    }

    /// Decode the uncommitted window and commit the words that agree with the previous hypothesis.
    pub fn decode(&mut self, recognizer: &mut dyn Recognizer) -> Result<(), Error> {
        self.pending = 0;
        if self.window.len() < samples(self.config.minimum_audio) {
            return Ok(());
        }
        if self.silent() {
            self.previous.clear();
            self.tentative.clear();
            // Silence is not speech, so a long silent window can be shortened.
            let maximum = samples(self.config.maximum_window);
            if self.window.len() >= maximum {
                self.drop_front(self.window.len() - RATE as usize);
            }
            return Ok(());
        }
        let words = recognizer.decode(&self.window, self.prompt().as_deref())?;
        let agreed = agreement(&self.previous, &words);
        let remaining = self.commit(&words, agreed);
        self.previous = remaining.clone();
        self.tentative = remaining;

        if self.window.len() >= samples(self.config.maximum_window) {
            // No agreement within the window: commit what is safely before its end rather than letting it grow without bound.
            let before =
                self.config.maximum_window.as_secs_f32() - self.config.force_margin.as_secs_f32();
            let words = self.tentative.clone();
            let count = words.iter().take_while(|word| word.end <= before).count();
            if count == 0 {
                return Err(Error::Overflow);
            }
            let remaining = self.commit(&words, count);
            self.previous = remaining.clone();
            self.tentative = remaining;
        }
        Ok(())
    }

    /// Decode only the uncommitted tail and return the complete transcript.
    pub fn finish(&mut self, recognizer: &mut dyn Recognizer) -> Result<String, Error> {
        while self.window.len() > samples(self.config.maximum_window) {
            self.decode(recognizer)?;
        }
        self.tentative.clear();
        if !self.window.is_empty() && !self.silent() {
            let minimum = samples(self.config.minimum_audio);
            if self.window.len() < minimum {
                // Pad a short tail rather than discard it.
                self.window.resize(minimum, 0);
            }
            let words = recognizer.decode(&self.window, self.prompt().as_deref())?;
            self.committed
                .extend(words.into_iter().map(|word| word.text.trim().to_owned()));
            self.drop_front(self.window.len());
        }
        self.committed.retain(|word| !word.is_empty());
        Ok(join(self.committed.iter().map(String::as_str)))
    }

    /// Commit the first `count` words of `words`, drop their audio, and return the rest relative to the new window start.
    fn commit(&mut self, words: &[Word], count: usize) -> Vec<Word> {
        if count == 0 {
            return words.to_vec();
        }
        self.committed.extend(
            words[..count]
                .iter()
                .map(|word| word.text.trim().to_owned())
                .filter(|word| !word.is_empty()),
        );
        let mut cut = words[count - 1].end;
        if let Some(next) = words.get(count) {
            cut = cut.min(next.start);
        }
        let cut = ((cut.max(0.0) * RATE as f32) as usize).min(self.window.len());
        self.drop_front(cut);
        let shift = cut as f32 / RATE as f32;
        words[count..]
            .iter()
            .map(|word| Word {
                text: word.text.clone(),
                start: (word.start - shift).max(0.0),
                end: (word.end - shift).max(0.0),
            })
            .collect()
    }

    fn drop_front(&mut self, count: usize) {
        self.window.drain(..count);
        self.dropped += count;
    }

    fn prompt(&self) -> Option<String> {
        let start = self
            .committed
            .len()
            .saturating_sub(self.config.prompt_words);
        let prompt = join(self.committed[start..].iter().map(String::as_str));
        (!prompt.is_empty()).then_some(prompt)
    }

    /// Whether every 100 ms frame of the window is below the silence threshold.
    ///
    /// Short frames are checked instead of the whole window's average, so that a quiet phrase followed by a long pause is not discarded.
    fn silent(&self) -> bool {
        let threshold = self.config.silence_threshold;
        self.window.chunks(RATE as usize / 10).all(|frame| {
            let energy: f64 = frame
                .iter()
                .map(|&sample| {
                    let value = f64::from(sample) / 32768.0;
                    value * value
                })
                .sum();
            ((energy / frame.len() as f64).sqrt() as f32) < threshold
        })
    }
}

fn samples(duration: std::time::Duration) -> usize {
    (duration.as_secs_f64() * f64::from(RATE)) as usize
}

fn join<'a>(words: impl Iterator<Item = &'a str>) -> String {
    words
        .map(str::trim)
        .filter(|word| !word.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

/// Compare words ignoring case and punctuation, which Whisper varies between hypotheses.
fn normalized(word: &str) -> String {
    word.chars()
        .filter(|character| character.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

/// The number of leading words on which two hypotheses agree.
fn agreement(previous: &[Word], current: &[Word]) -> usize {
    previous
        .iter()
        .zip(current)
        .take_while(|(previous, current)| normalized(&previous.text) == normalized(&current.text))
        .count()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn agreement_ignores_case_and_punctuation() {
        let words = |texts: &[&str]| -> Vec<Word> {
            texts
                .iter()
                .map(|text| Word {
                    text: (*text).into(),
                    start: 0.0,
                    end: 0.0,
                })
                .collect()
        };
        assert_eq!(
            agreement(
                &words(&[" Hello,", " world"]),
                &words(&[" hello", " world.", " x"])
            ),
            2
        );
        assert_eq!(agreement(&words(&[" a", " b"]), &words(&[" a", " c"])), 1);
        assert_eq!(agreement(&[], &words(&[" a"])), 0);
    }

    #[test]
    fn silence_is_detected_per_frame() {
        let mut policy = Policy::new(RecognizerConfig::default());
        policy.push(&vec![0; RATE as usize * 2]);
        assert!(policy.silent());
        let mut loud = vec![0; RATE as usize * 2];
        loud[100..1700].fill(3000);
        policy.push(&loud);
        assert!(!policy.silent());
    }

    #[test]
    fn samples_converts_durations() {
        assert_eq!(samples(Duration::from_millis(500)), 8000);
    }
}
