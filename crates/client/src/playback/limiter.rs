// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

/// The largest output magnitude the limiter aims for, leaving a little headroom below full scale.
pub const CEILING: f32 = 32_000.0;

/// How long the limiter takes to recover from full attenuation back to unity gain.
const RELEASE_SECONDS: f32 = 0.1;

/// A peak limiter that converts a mixed signal to `i16` without wrapping or harsh clipping.
///
/// Gain drops immediately when a sample would exceed [`CEILING`] and recovers linearly towards unity, so signals that fit are passed through unchanged.
#[derive(Clone, Debug)]
pub struct Limiter {
    gain: f32,
    release: f32,
}

impl Limiter {
    pub fn new(sample_rate: u32) -> Self {
        Self {
            gain: 1.0,
            release: 1.0 / (RELEASE_SECONDS * sample_rate as f32),
        }
    }

    pub fn gain(&self) -> f32 {
        self.gain
    }

    pub fn process(&mut self, input: &[f32], output: &mut [i16]) {
        for (sample, output) in input.iter().zip(output.iter_mut()) {
            let magnitude = sample.abs();
            if magnitude * self.gain > CEILING {
                self.gain = CEILING / magnitude;
            }
            *output = (sample * self.gain)
                .round()
                .clamp(f32::from(i16::MIN), f32::from(i16::MAX)) as i16;
            self.gain = (self.gain + self.release).min(1.0);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn passes_signals_below_the_ceiling_unchanged() {
        let mut limiter = Limiter::new(48_000);
        let input = [0.0, 1.0, -1.0, 12_345.0, -31_999.0];
        let mut output = [0; 5];
        limiter.process(&input, &mut output);
        assert_eq!(output, [0, 1, -1, 12_345, -31_999]);
        assert_eq!(limiter.gain(), 1.0);
    }

    #[test]
    fn attenuates_overloads_and_recovers() {
        let mut limiter = Limiter::new(1_000);
        let mut output = [0; 1];
        limiter.process(&[128_000.0], &mut output);
        assert_eq!(output[0], 32_000);
        assert!(limiter.gain() < 0.3);

        // A tenth of a second at 1 kHz is 100 samples to recover fully.
        let quiet = [100.0; 100];
        let mut output = [0; 100];
        limiter.process(&quiet, &mut output);
        assert!(output[0] < 100);
        assert_eq!(limiter.gain(), 1.0);
        assert_eq!(output[99], 100);
    }

    #[test]
    fn never_wraps_at_extreme_input() {
        let mut limiter = Limiter::new(48_000);
        let input = [f32::from(i16::MIN) * 8.0, f32::from(i16::MAX) * 8.0];
        let mut output = [0; 2];
        limiter.process(&input, &mut output);
        assert!(output[0] <= -31_990 && output[0] >= -32_000);
        assert!(output[1] > 0 && output[1] <= 32_000);
    }
}
