// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

use std::collections::{BTreeMap, VecDeque};
use std::time::Duration;

use protocol::AudioFrame;

use super::PlaybackStatistics;

/// The most consecutive missing frames replaced with silence before playback skips to the next available frame.
const CONCEALMENT_LIMIT: u64 = 3;

/// Jitter buffer timing, shared by every source in a [`super::Mixer`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct JitterOptions {
    /// Audio buffered before a source starts (or restarts after an underrun) playing.
    pub target_latency: Duration,
    /// Buffered audio beyond this is discarded, oldest first, back down to the target.
    pub latency_limit: Duration,
}

impl Default for JitterOptions {
    fn default() -> Self {
        Self {
            target_latency: Duration::from_millis(60),
            latency_limit: Duration::from_millis(250),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum State {
    /// Waiting for the target latency to accumulate. Contributes silence.
    Buffering,
    Playing,
}

/// Reorders one source's frames by sequence and converts them to the device rate.
///
/// Playback never waits for a missing frame: when the next expected frame is absent but later frames are buffered, its slot is filled with silence (up to [`CONCEALMENT_LIMIT`] frames, then playback skips ahead). A frame arriving after its slot has been played or concealed is dropped. When the buffer runs dry the source contributes silence and buffers to the target latency again.
pub struct JitterBuffer {
    options: JitterOptions,
    sample_rate: u32,
    /// Frames not yet consumed, keyed by sequence extended to 64 bits so ordering survives `u32` wraparound.
    frames: BTreeMap<u64, Vec<i16>>,
    /// The last extended sequence received, used to extend the next one.
    latest: Option<u64>,
    /// The next sequence to play. Frames before it are late.
    next: Option<u64>,
    /// Source-rate samples ready for resampling.
    pending: VecDeque<i16>,
    /// Fractional read position into `pending`.
    position: f64,
    /// Samples per frame, used to size concealment.
    frame_length: usize,
    state: State,
    /// Device-rate samples produced since the last frame arrived.
    idle_samples: u64,
}

impl JitterBuffer {
    pub fn new(sample_rate: u32, options: JitterOptions) -> Self {
        Self {
            options,
            sample_rate,
            frames: BTreeMap::new(),
            latest: None,
            next: None,
            pending: VecDeque::new(),
            position: 0.0,
            frame_length: 0,
            state: State::Buffering,
            idle_samples: 0,
        }
    }

    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    /// Whether the source is currently contributing audio rather than buffering.
    pub fn is_playing(&self) -> bool {
        self.state == State::Playing
    }

    /// Device-rate samples produced since a frame last arrived.
    pub fn idle_samples(&self) -> u64 {
        self.idle_samples
    }

    /// Source-rate samples buffered and not yet played.
    pub fn buffered_samples(&self) -> usize {
        let pending = self.pending.len().saturating_sub(self.position as usize);
        pending + self.frames.values().map(Vec::len).sum::<usize>()
    }

    pub fn buffered(&self) -> Duration {
        Duration::from_secs_f64(self.buffered_samples() as f64 / self.sample_rate as f64)
    }

    fn samples(&self, duration: Duration) -> usize {
        (duration.as_secs_f64() * self.sample_rate as f64).round() as usize
    }

    fn extend(&self, sequence: u32) -> u64 {
        match self.latest {
            None => u64::from(sequence) + (1 << 32),
            Some(latest) => {
                let delta = sequence.wrapping_sub(latest as u32) as i32;
                latest.wrapping_add_signed(i64::from(delta))
            }
        }
    }

    /// Add a frame. Its sample rate must match [`Self::sample_rate`]; the mixer replaces the buffer when a source changes rate.
    pub fn push(&mut self, frame: AudioFrame, statistics: &mut PlaybackStatistics) {
        debug_assert_eq!(frame.sample_rate, self.sample_rate);
        self.idle_samples = 0;
        let sequence = self.extend(frame.sequence);
        if self.next.is_some_and(|next| sequence < next) || self.frames.contains_key(&sequence) {
            statistics.late_frames += 1;
            return;
        }
        if self.latest.is_none_or(|latest| sequence > latest) {
            self.latest = Some(sequence);
        }
        if !frame.samples.is_empty() {
            self.frame_length = frame.samples.len();
        }
        self.frames.insert(sequence, frame.samples);

        if self.buffered_samples() > self.samples(self.options.latency_limit) {
            self.trim(statistics);
        }
    }

    /// Discard the oldest audio until the buffer is back at the target latency.
    fn trim(&mut self, statistics: &mut PlaybackStatistics) {
        let target = self.samples(self.options.target_latency);
        if !self.pending.is_empty() {
            self.pending.clear();
            self.position = 0.0;
        }
        while self.buffered_samples() > target {
            let Some((sequence, _)) = self.frames.pop_first() else {
                break;
            };
            statistics.discarded_frames += 1;
            self.next = Some(sequence + 1);
        }
    }

    /// Make more source samples available. Returns false on underrun.
    fn refill(&mut self, statistics: &mut PlaybackStatistics) -> bool {
        let Some(entry) = self.frames.first_entry() else {
            return false;
        };
        let next = self.next.unwrap_or(*entry.key());
        let missing = *entry.key() - next;
        if missing == 0 {
            self.pending.extend(entry.remove());
            self.next = Some(next + 1);
        } else if missing <= CONCEALMENT_LIMIT {
            statistics.concealed_frames += 1;
            self.pending
                .extend(std::iter::repeat_n(0, self.frame_length));
            self.next = Some(next + 1);
        } else {
            statistics.skipped_frames += missing;
            self.next = Some(*entry.key());
        }
        true
    }

    /// Add this source's next `output.len()` samples at `device_rate`, scaled by `gain`, to `output`.
    pub fn mix_into(
        &mut self,
        output: &mut [f32],
        device_rate: u32,
        gain: f32,
        statistics: &mut PlaybackStatistics,
    ) {
        self.idle_samples += output.len() as u64;
        if self.state == State::Buffering {
            // Start once the target is buffered, or when the source has stopped sending so that the tail of an utterance is not held back.
            let target = self.options.target_latency.as_secs_f64();
            let stalled = self.idle_samples as f64 >= target * f64::from(device_rate);
            if self.frames.is_empty()
                || (self.buffered_samples() < self.samples(self.options.target_latency) && !stalled)
            {
                return;
            }
            self.state = State::Playing;
            // Resume from the oldest buffered frame rather than concealing a gap that preceded the silence.
            self.next = self.frames.keys().next().copied();
        }

        let step = self.sample_rate as f64 / device_rate as f64;
        for sample in output.iter_mut() {
            let index = self.position as usize;
            let fraction = (self.position - index as f64) as f32;
            let needed = if fraction == 0.0 {
                index + 1
            } else {
                index + 2
            };
            while self.pending.len() < needed {
                if !self.refill(statistics) {
                    statistics.underruns += 1;
                    self.state = State::Buffering;
                    return;
                }
            }
            let current = f32::from(self.pending[index]);
            let value = if fraction == 0.0 {
                current
            } else {
                current + (f32::from(self.pending[index + 1]) - current) * fraction
            };
            *sample += value * gain;
            self.position += step;
            let consumed = self.position as usize;
            if consumed > 0 {
                self.pending.drain(..consumed.min(self.pending.len()));
                self.position -= consumed as f64;
            }
        }
    }
}

#[cfg(test)]
mod tests;
