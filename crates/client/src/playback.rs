// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

//! Real-time playback of received audio: per-source jitter buffers, a mixer, and a [`Player`] that feeds the mixer from a [`crate::Client`] for an output device. Pure logic with no I/O, so it can run inside an audio device callback. See `design/live-audio.md`.

mod jitter_buffer;
mod limiter;
mod mixer;
mod player;

pub use jitter_buffer::{JitterBuffer, JitterOptions};
pub use limiter::{CEILING, Limiter};
pub use mixer::{Mixer, MixerOptions};
pub use player::{Player, PlayerControl};

/// Counters describing how playback handled imperfect input, summed across sources.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PlaybackStatistics {
    /// Frames that arrived after their slot was played, or duplicates. Dropped.
    pub late_frames: u64,
    /// Missing frames replaced with silence.
    pub concealed_frames: u64,
    /// Missing frames skipped over because the gap was too long to conceal.
    pub skipped_frames: u64,
    /// Buffered frames discarded because the latency limit was exceeded.
    pub discarded_frames: u64,
    /// Times a playing source ran out of audio and returned to buffering.
    pub underruns: u64,
}
