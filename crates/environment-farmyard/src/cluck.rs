// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

//! A chicken's cluck, synthesized rather than loaded from a file, as Tomori's sound cues are: a voice of harmonics
//! gliding in pitch, through two resonances as a throat shapes a vowel, with a breath of noise as it starts. Rendered
//! once, and played where the chicken is, as an ambient sound of the farmyard's. See `design/farmyard.md`.
use std::f64::consts::{PI, TAU};

/// A kind of cluck.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Cluck {
    /// One low "bok".
    Single,
    /// "Bok-bok".
    Double,
    /// A short "bok" and a rising "bwaak?": the chicken noticed someone pointing at it.
    Noticed,
    /// A sharp "b-kawk!", high and falling: the chicken was startled, and sprints away.
    Startled,
}

/// The rate clucks are rendered at, which the mixer plays at its own.
pub const RATE: u32 = 48_000;

/// A cluck's peak, as a fraction of full scale: quieter than any of Tomori's cues.
pub const LEVEL: f32 = 0.06;

/// One syllable of a cluck: when it starts and how long it lasts, in milliseconds, its pitch from and to, and the
/// resonances of its vowel.
struct Syllable {
    start: u32,
    length: u32,
    pitch: (f64, f64),
    formants: [(f64, f64); 2],
}

impl Cluck {
    /// Every kind, in the order a mixer holds them.
    pub const ALL: [Self; 4] = [Self::Single, Self::Double, Self::Noticed, Self::Startled];

    fn syllables(self) -> Vec<Syllable> {
        // "o", as in "bok", and "aa", as in "bwaak".
        const O: [(f64, f64); 2] = [(700.0, 1.0), (1_350.0, 0.5)];
        const AA: [(f64, f64); 2] = [(950.0, 1.0), (1_900.0, 0.6)];
        let syllable = |start, length, pitch, formants| Syllable {
            start,
            length,
            pitch,
            formants,
        };
        match self {
            Self::Single => vec![syllable(0, 90, (420.0, 310.0), O)],
            Self::Double => vec![
                syllable(0, 80, (430.0, 320.0), O),
                syllable(140, 75, (390.0, 290.0), O),
            ],
            Self::Noticed => vec![
                syllable(0, 55, (400.0, 340.0), O),
                syllable(95, 190, (330.0, 560.0), AA),
            ],
            Self::Startled => vec![
                syllable(0, 40, (480.0, 560.0), O),
                syllable(60, 170, (740.0, 500.0), AA),
            ],
        }
    }

    /// How long it lasts, in milliseconds.
    pub fn length(self) -> u32 {
        self.syllables()
            .iter()
            .map(|syllable| syllable.start + syllable.length)
            .max()
            .unwrap_or(0)
    }

    /// Its samples at `sample_rate`, in `i16` units, peaking at [`LEVEL`] of full scale. Deterministic.
    pub fn render(self, sample_rate: u32) -> Vec<f32> {
        let rate = f64::from(sample_rate.max(1));
        let samples =
            |milliseconds: u32| (f64::from(milliseconds) * rate / 1000.0).round() as usize;
        let mut output = vec![0.0_f64; samples(self.length())];
        // A fixed noise, the same every time.
        let mut noise_state = 0x2545_f491_u32;
        let mut noise = move || {
            noise_state = noise_state
                .wrapping_mul(1_664_525)
                .wrapping_add(1_013_904_223);
            f64::from(noise_state >> 8) / f64::from(1u32 << 23) - 1.0
        };
        for syllable in self.syllables() {
            let start = samples(syllable.start);
            let length = samples(syllable.length).max(1);
            let attack = samples(6).max(1);
            let decay = (300.0_f64).ln() / (length - attack.min(length)).max(1) as f64;
            let mut resonators = syllable
                .formants
                .map(|(frequency, gain)| (Resonator::new(frequency, frequency * 0.22, rate), gain));
            let mut phase = 0.0_f64;
            for index in 0..length {
                let progress = index as f64 / length as f64;
                let (from, to) = syllable.pitch;
                let pitch = from + (to - from) * progress;
                phase = (phase + pitch / rate).fract();
                // A voice rich in harmonics, falling away as a throat's does, up to about 4 kHz.
                let harmonics = (4_000.0 / pitch) as usize;
                let voice: f64 = (1..=harmonics)
                    .map(|harmonic| {
                        let harmonic = harmonic as f64;
                        (TAU * phase * harmonic).sin() / harmonic
                    })
                    .sum();
                // A breath of noise as it starts, the "b" of "bok".
                let breath =
                    noise() * (1.0 - index as f64 / samples(12).max(1) as f64).max(0.0) * 1.5;
                let source = voice + breath;
                let shaped: f64 = resonators
                    .iter_mut()
                    .map(|(resonator, gain)| resonator.filter(source) * *gain)
                    .sum();
                let envelope = if index < attack {
                    0.5 - 0.5 * (PI * index as f64 / attack as f64).cos()
                } else {
                    (-decay * (index - attack) as f64).exp()
                };
                // The last couple of milliseconds fade to nothing, so that it never ends on a step.
                let tail = ((length - index) as f64 / (rate * 0.002)).min(1.0);
                output[start + index] += shaped * envelope * tail;
            }
        }
        let peak = output
            .iter()
            .fold(0.0_f64, |peak, sample| peak.max(sample.abs()));
        let scale = f64::from(LEVEL) * f64::from(i16::MAX) / peak.max(f64::MIN_POSITIVE);
        output
            .into_iter()
            .map(|sample| (sample * scale) as f32)
            .collect()
    }
}

/// A two-pole resonance, as a formant of a vowel: a band-pass at `frequency` of width `bandwidth`.
struct Resonator {
    a1: f64,
    a2: f64,
    gain: f64,
    y1: f64,
    y2: f64,
}

impl Resonator {
    fn new(frequency: f64, bandwidth: f64, rate: f64) -> Self {
        let radius = (-PI * bandwidth / rate).exp();
        Self {
            a1: 2.0 * radius * (TAU * frequency / rate).cos(),
            a2: -radius * radius,
            gain: 1.0 - radius,
            y1: 0.0,
            y2: 0.0,
        }
    }

    fn filter(&mut self, input: f64) -> f64 {
        let output = self.gain * input + self.a1 * self.y1 + self.a2 * self.y2;
        self.y2 = self.y1;
        self.y1 = output;
        output
    }
}

/// Each kind of cluck, rendered at [`RATE`] once, in the order of [`Cluck::ALL`].
pub fn rendered() -> &'static [std::sync::Arc<[f32]>; 4] {
    static RENDERED: std::sync::OnceLock<[std::sync::Arc<[f32]>; 4]> = std::sync::OnceLock::new();
    RENDERED.get_or_init(|| Cluck::ALL.map(|cluck| cluck.render(RATE).into()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn peak(samples: &[f32]) -> f32 {
        samples
            .iter()
            .fold(0.0, |peak, sample| peak.max(sample.abs()))
    }

    #[test]
    fn every_cluck_is_short_quiet_and_never_clips() {
        for cluck in Cluck::ALL {
            let samples = cluck.render(RATE);
            let length = cluck.length();
            assert!((50..=300).contains(&length), "{cluck:?} lasts {length} ms");
            assert_eq!(samples.len(), length as usize * RATE as usize / 1000);
            let level = peak(&samples) / f32::from(i16::MAX);
            assert!((level - LEVEL).abs() < 1e-4, "{cluck:?} peaks at {level}");
            // Starts and ends silently, so it never clicks.
            let (first, last) = (samples[0], samples[samples.len() - 1]);
            assert!(first.abs() < 1.0, "{cluck:?} starts at {first}");
            assert!(last.abs() < 30.0, "{cluck:?} ends at {last}");
        }
        // Rendered once, as rendering them again would.
        assert_eq!(&*rendered()[3], &Cluck::Startled.render(RATE)[..]);
    }

    #[test]
    fn clucks_are_deterministic_distinct_and_voiced() {
        for (index, cluck) in Cluck::ALL.iter().enumerate() {
            assert_eq!(cluck.render(RATE), cluck.render(RATE));
            for other in &Cluck::ALL[index + 1..] {
                assert_ne!(cluck.render(RATE), other.render(RATE));
            }
        }
        // A double cluck is two, with a quiet gap between.
        let double = Cluck::Double.render(RATE);
        let gap = &double[RATE as usize * 100 / 1000..RATE as usize * 135 / 1000];
        assert!(peak(gap) < peak(&double) * 0.1);
        // Voiced, by how often it crosses zero, not hiss.
        let single = Cluck::Single.render(RATE);
        let crossings = single
            .windows(2)
            .filter(|pair| (pair[0] < 0.0) != (pair[1] < 0.0))
            .count();
        let per_second = crossings as f32 / (single.len() as f32 / RATE as f32);
        assert!((400.0..6_000.0).contains(&per_second), "{per_second}");
        // At another rate, it lasts as long.
        assert_eq!(Cluck::Single.render(24_000).len(), single.len() / 2);
    }
}
