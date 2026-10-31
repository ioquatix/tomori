// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

use std::collections::HashMap;
use std::time::Duration;

use protocol::{AGENT_SOURCE, AudioFrame, Gate};

use super::{JitterBuffer, JitterOptions, Limiter, PlaybackStatistics};

/// How long a source may go without frames before its buffer is released.
const IDLE_LIMIT: Duration = Duration::from_secs(10);
/// How quickly a source's level falls once it plays more quietly: it drops to about a third over this time.
const LEVEL_RELEASE: Duration = Duration::from_millis(150);

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MixerOptions {
    /// The output device rate.
    pub sample_rate: u32,
    pub jitter: JitterOptions,
}

impl Default for MixerOptions {
    fn default() -> Self {
        Self {
            sample_rate: 48_000,
            jitter: JitterOptions::default(),
        }
    }
}

/// Mixes every remote participant and the agent speech stream into one mono `i16` stream at the device rate.
///
/// Feed it every received [`AudioFrame`] with [`Self::push`] and pull output from the audio callback with [`Self::mix`]. Each source has its own [`JitterBuffer`], so one late or bursty participant does not delay the others. Agent volume and mute are local to this mixer; muting still consumes agent audio in real time, so unmuting resumes live speech rather than replaying what was missed.
pub struct Mixer {
    options: MixerOptions,
    sources: HashMap<u32, JitterBuffer>,
    /// This client's own participant number, whose audio is never played back.
    local_source: Option<u32>,
    agent_volume: f32,
    agent_muted: bool,
    limiter: Limiter,
    accumulator: Vec<f32>,
    /// One source's contribution to the current [`Self::mix`] call, so its level can be measured.
    scratch: Vec<f32>,
    /// The level of each buffered source; see [`Self::source_level`].
    levels: HashMap<u32, f32>,
    statistics: PlaybackStatistics,
}

impl Mixer {
    pub fn new(options: MixerOptions) -> Self {
        assert!(
            options.sample_rate > 0,
            "device sample rate must be positive"
        );
        Self {
            options,
            sources: HashMap::new(),
            local_source: None,
            agent_volume: 1.0,
            agent_muted: false,
            limiter: Limiter::new(options.sample_rate),
            accumulator: Vec::new(),
            scratch: Vec::new(),
            levels: HashMap::new(),
            statistics: PlaybackStatistics::default(),
        }
    }

    pub fn sample_rate(&self) -> u32 {
        self.options.sample_rate
    }

    /// Ignore audio from this participant number: a client never hears its own microphone. Set from [`protocol::ServerMessage::Welcome`].
    pub fn set_local_source(&mut self, source: Option<u32>) {
        self.local_source = source;
        if let Some(source) = source {
            self.clear_source(source);
        }
    }

    pub fn agent_volume(&self) -> f32 {
        self.agent_volume
    }

    /// Set the local agent speech gain. Negative values are treated as zero.
    pub fn set_agent_volume(&mut self, volume: f32) {
        self.agent_volume = volume.max(0.0);
    }

    pub fn is_agent_muted(&self) -> bool {
        self.agent_muted
    }

    pub fn set_agent_muted(&mut self, muted: bool) {
        self.agent_muted = muted;
    }

    pub fn statistics(&self) -> PlaybackStatistics {
        self.statistics
    }

    /// The sources currently buffered, including those still filling.
    pub fn sources(&self) -> impl Iterator<Item = u32> + '_ {
        self.sources.keys().copied()
    }

    /// Discard all buffered audio. The application owns the mixer, so it calls this when the audio it hears changes context: on [`protocol::ServerMessage::Left`], before joining a different space, and after reconnecting.
    pub fn clear(&mut self) {
        self.sources.clear();
        self.levels.clear();
    }

    /// Discard one source's buffered audio, for example when a participant leaves the space. Its next frame starts a new buffer.
    pub fn clear_source(&mut self, source: u32) {
        self.sources.remove(&source);
        self.levels.remove(&source);
    }

    /// Whether `source` is contributing audio now, rather than buffering, stalled or absent. Agent speech that is muted still counts as playing, because it is consumed in real time.
    pub fn is_source_playing(&self, source: u32) -> bool {
        self.sources
            .get(&source)
            .is_some_and(JitterBuffer::is_playing)
    }

    /// The recent loudness of what [`Self::mix`] played from `source`, after its gain, from 0 (silent or absent) to 1 (full scale), for visuals such as a speaking indicator. It follows a louder signal at once and falls over about [`LEVEL_RELEASE`] when the source goes quiet.
    pub fn source_level(&self, source: u32) -> f32 {
        self.levels.get(&source).copied().unwrap_or(0.0)
    }

    /// Queue a received frame for playback.
    pub fn push(&mut self, frame: AudioFrame) {
        if frame.gate == Gate::Off || Some(frame.source) == self.local_source {
            return;
        }
        let source = self
            .sources
            .entry(frame.source)
            .or_insert_with(|| JitterBuffer::new(frame.sample_rate, self.options.jitter));
        if source.sample_rate() != frame.sample_rate {
            // A new speech segment can change rate; restart the source rather than play mismatched audio.
            *source = JitterBuffer::new(frame.sample_rate, self.options.jitter);
        }
        source.push(frame, &mut self.statistics);
    }

    /// Fill `output` with the next mixed samples. Never blocks: missing audio is silence.
    pub fn mix(&mut self, output: &mut [i16]) {
        self.accumulator.clear();
        self.accumulator.resize(output.len(), 0.0);
        let rate = self.options.sample_rate;
        let agent_gain = if self.agent_muted {
            0.0
        } else {
            self.agent_volume
        };
        let release =
            (-(output.len() as f64) / (LEVEL_RELEASE.as_secs_f64() * f64::from(rate))).exp() as f32;
        for (&source, buffer) in self.sources.iter_mut() {
            let gain = if source == AGENT_SOURCE {
                agent_gain
            } else {
                1.0
            };
            self.scratch.clear();
            self.scratch.resize(output.len(), 0.0);
            buffer.mix_into(&mut self.scratch, rate, gain, &mut self.statistics);
            let mut energy = 0.0;
            for (total, &sample) in self.accumulator.iter_mut().zip(&self.scratch) {
                *total += sample;
                energy += sample * sample;
            }
            let rms = (energy / output.len().max(1) as f32).sqrt() / f32::from(i16::MAX);
            let level = self.levels.entry(source).or_insert(0.0);
            *level = rms.min(1.0).max(*level * release);
        }
        let idle_limit = (IDLE_LIMIT.as_secs_f64() * f64::from(rate)) as u64;
        self.sources.retain(|_, buffer| {
            buffer.is_playing()
                || buffer.buffered_samples() > 0
                || buffer.idle_samples() < idle_limit
        });
        let sources = &self.sources;
        self.levels.retain(|source, _| sources.contains_key(source));
        self.limiter.process(&self.accumulator, output);
    }
}

#[cfg(test)]
mod tests;
