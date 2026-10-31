// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

//! Real-time mixing of participant and agent speech streams, ported from `bridge/audio.mjs`. Not yet wired to a spectator feed.
//!
//! Each source keeps at most [`BUFFER_LIMIT`] samples: audio that arrives faster than it is mixed drops its oldest samples instead of building latency, and a source with nothing buffered contributes silence. See `design/live-audio.md`.

use std::collections::VecDeque;

/// The mixer's sample rate. Sources must be resampled to this rate before pushing.
pub const SAMPLE_RATE: u32 = 16_000;

/// Samples produced per tick: 50 ms.
pub const SAMPLES_PER_TICK: usize = 800;

/// Samples buffered per source: 150 ms.
pub const BUFFER_LIMIT: usize = SAMPLES_PER_TICK * 3;

/// Add `inputs` sample by sample into `output`, saturating at the limits of `i16`. Shorter inputs contribute silence after their end.
pub fn mix(inputs: &[&[i16]], output: &mut [i16]) {
    for (index, sample) in output.iter_mut().enumerate() {
        let sum: i32 = inputs
            .iter()
            .filter_map(|input| input.get(index))
            .map(|&value| i32::from(value))
            .sum();
        *sample = sum.clamp(i32::from(i16::MIN), i32::from(i16::MAX)) as i16;
    }
}

/// Buffers several sources and mixes them one tick at a time.
#[derive(Debug)]
pub struct Mixer<Source> {
    sources: Vec<(Source, VecDeque<i16>)>,
}

impl<Source> Default for Mixer<Source> {
    fn default() -> Self {
        Self {
            sources: Vec::new(),
        }
    }
}

impl<Source: PartialEq> Mixer<Source> {
    pub fn new() -> Self {
        Self::default()
    }

    /// Append samples from a source, dropping its oldest samples beyond [`BUFFER_LIMIT`].
    pub fn push(&mut self, source: Source, samples: &[i16]) {
        let index = match self.sources.iter().position(|(key, _)| *key == source) {
            Some(index) => index,
            None => {
                self.sources.push((source, VecDeque::new()));
                self.sources.len() - 1
            }
        };
        let buffer = &mut self.sources[index].1;
        buffer.extend(samples);
        let excess = buffer.len().saturating_sub(BUFFER_LIMIT);
        buffer.drain(..excess);
    }

    pub fn remove(&mut self, source: &Source) {
        self.sources.retain(|(key, _)| key != source);
    }

    /// Mix and consume the next tick of every source.
    pub fn tick(&mut self) -> Vec<i16> {
        let chunks: Vec<Vec<i16>> = self
            .sources
            .iter_mut()
            .map(|(_, buffer)| {
                let count = buffer.len().min(SAMPLES_PER_TICK);
                buffer.drain(..count).collect()
            })
            .collect();
        let inputs: Vec<&[i16]> = chunks.iter().map(Vec::as_slice).collect();
        let mut output = vec![0; SAMPLES_PER_TICK];
        mix(&inputs, &mut output);
        output
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mixing_saturates_and_pads_with_silence() {
        let mut output = [0; 3];
        mix(&[&[30_000, -30_000, 5], &[10_000, -10_000]], &mut output);
        assert_eq!(output, [i16::MAX, i16::MIN, 5]);
    }

    #[test]
    fn sources_keep_only_recent_audio() {
        let mut mixer = Mixer::new();
        mixer.push("mic", &vec![1; BUFFER_LIMIT + 10]);
        mixer.push("agent", &[2; 5]);
        let first = mixer.tick();
        assert_eq!(first[..5], [3; 5]);
        assert_eq!(first[5..], [1; SAMPLES_PER_TICK - 5]);
        mixer.tick();
        mixer.tick();
        assert_eq!(mixer.tick(), vec![0; SAMPLES_PER_TICK]);
    }

    #[test]
    fn removed_sources_are_silent() {
        let mut mixer = Mixer::new();
        mixer.push(1, &[7; 10]);
        mixer.remove(&1);
        assert_eq!(mixer.tick(), vec![0; SAMPLES_PER_TICK]);
    }
}
