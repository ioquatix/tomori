// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

//! A streaming resampler for microphone audio, adequate for speech recognition.
//!
//! When downsampling, a moving average over one output period suppresses most aliasing before linear interpolation.

/// Converts a stream of samples from one rate to another, keeping state between chunks.
#[derive(Clone, Debug)]
pub struct Resampler {
    from: u32,
    to: u32,
    /// Input samples per output sample.
    step: f64,
    /// The position of the next output sample, in input samples after `previous`.
    phase: f64,
    previous: Option<f32>,
    taps: usize,
    history: Vec<f32>,
    sum: f32,
}

impl Resampler {
    pub fn new(from: u32, to: u32) -> Self {
        let taps = if from > to {
            (f64::from(from) / f64::from(to)).round().max(1.0) as usize
        } else {
            1
        };
        Self {
            from,
            to,
            step: f64::from(from) / f64::from(to),
            phase: 0.0,
            previous: None,
            taps,
            history: Vec::with_capacity(taps),
            sum: 0.0,
        }
    }

    pub fn input_rate(&self) -> u32 {
        self.from
    }

    /// Resample `input`, appending to `output`.
    pub fn process(&mut self, input: &[i16], output: &mut Vec<i16>) {
        if self.from == self.to {
            output.extend_from_slice(input);
            return;
        }
        output.reserve((input.len() as f64 / self.step) as usize + 1);
        for &sample in input {
            let current = self.filter(f32::from(sample));
            let Some(previous) = self.previous else {
                self.previous = Some(current);
                continue;
            };
            while self.phase < 1.0 {
                let value = previous + (current - previous) * self.phase as f32;
                output.push(value.round().clamp(-32768.0, 32767.0) as i16);
                self.phase += self.step;
            }
            self.phase -= 1.0;
            self.previous = Some(current);
        }
    }

    fn filter(&mut self, sample: f32) -> f32 {
        if self.taps == 1 {
            return sample;
        }
        if self.history.len() == self.taps {
            self.sum -= self.history.remove(0);
        }
        self.history.push(sample);
        self.sum += sample;
        self.sum / self.history.len() as f32
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn resample(from: u32, to: u32, input: &[i16], chunk: usize) -> Vec<i16> {
        let mut resampler = Resampler::new(from, to);
        let mut output = Vec::new();
        for part in input.chunks(chunk) {
            resampler.process(part, &mut output);
        }
        output
    }

    #[test]
    fn passes_through_at_the_same_rate() {
        assert_eq!(resample(16_000, 16_000, &[1, 2, 3], 2), [1, 2, 3]);
    }

    #[test]
    fn converts_lengths_by_the_rate_ratio() {
        let input = vec![1000i16; 48_000];
        let output = resample(48_000, 16_000, &input, 480);
        assert!(
            (15_990..=16_000).contains(&output.len()),
            "{}",
            output.len()
        );
        assert!(output[10..].iter().all(|&sample| sample == 1000));
        let output = resample(8_000, 16_000, &input[..8_000], 160);
        assert!(
            (15_990..=16_000).contains(&output.len()),
            "{}",
            output.len()
        );
    }

    #[test]
    fn chunking_does_not_change_the_output() {
        let input: Vec<i16> = (0..4_410)
            .map(|index| ((index * 37) % 2000) as i16)
            .collect();
        assert_eq!(
            resample(44_100, 16_000, &input, 1),
            resample(44_100, 16_000, &input, 441)
        );
    }

    #[test]
    fn preserves_a_low_frequency_tone() {
        // 200 Hz at 48 kHz downsampled to 16 kHz keeps its amplitude.
        let input: Vec<i16> = (0..48_000)
            .map(|index| {
                (10_000.0 * (2.0 * std::f32::consts::PI * 200.0 * index as f32 / 48_000.0).sin())
                    as i16
            })
            .collect();
        let output = resample(48_000, 16_000, &input, 960);
        let peak = output
            .iter()
            .map(|sample| sample.unsigned_abs())
            .max()
            .unwrap();
        assert!((9_500..=10_000).contains(&peak), "{peak}");
    }
}
