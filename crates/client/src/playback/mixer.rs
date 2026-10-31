// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

use std::collections::HashMap;
use std::time::Duration;

use protocol::{AGENT_SOURCE, AudioFrame, Gate};

use super::{JitterBuffer, JitterOptions, Limiter, PlaybackStatistics};

/// How long a source may go without frames before its buffer is released.
const IDLE_LIMIT: Duration = Duration::from_secs(10);

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
            self.sources.remove(&source);
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

    /// Discard all buffered audio, for example after a reconnect.
    pub fn clear(&mut self) {
        self.sources.clear();
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
        for (&source, buffer) in self.sources.iter_mut() {
            let gain = if source == AGENT_SOURCE {
                agent_gain
            } else {
                1.0
            };
            buffer.mix_into(&mut self.accumulator, rate, gain, &mut self.statistics);
        }
        let idle_limit = (IDLE_LIMIT.as_secs_f64() * f64::from(rate)) as u64;
        self.sources.retain(|_, buffer| {
            buffer.is_playing()
                || buffer.buffered_samples() > 0
                || buffer.idle_samples() < idle_limit
        });
        self.limiter.process(&self.accumulator, output);
    }
}

#[cfg(test)]
mod tests;
