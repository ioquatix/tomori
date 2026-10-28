//! Platform-independent speech lifecycle; guards outlive queueing and synthesis.
use std::{
    collections::BTreeMap,
    sync::{
        Arc, Mutex, MutexGuard,
        atomic::{AtomicU64, Ordering},
    },
};

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct Snapshot {
    pub pending: bool,
    pub playing: bool,
    pub level: f32,
    pub failed: bool,
}

#[derive(Default)]
struct State {
    pending: usize,
    failed: bool,
    next_playback: u64,
    // Playback is deliberately independent of the job epoch: cancellation does
    // not mean the hardware has actually stopped yet.
    playbacks: BTreeMap<u64, f32>,
}

#[derive(Default)]
struct Shared {
    epoch: AtomicU64,
    state: Mutex<State>,
}

impl Shared {
    fn state(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|error| error.into_inner())
    }
}

#[derive(Clone, Default)]
pub(crate) struct SpeechTracker(Arc<Shared>);

impl SpeechTracker {
    /// Register before enqueueing so a fast worker cannot finish uncounted work.
    /// Keep this guard across every synthesized chunk and the gaps between them.
    pub fn begin_job(&self) -> SpeechJob {
        let mut state = self.0.state();
        state.failed = false;
        state.pending += 1;
        SpeechJob {
            shared: self.0.clone(),
            epoch: self.0.epoch.load(Ordering::Relaxed),
        }
    }

    pub fn cancel(&self) {
        let mut state = self.0.state();
        self.0.epoch.fetch_add(1, Ordering::AcqRel);
        state.pending = 0;
    }

    pub fn clear_failure(&self) {
        self.0.state().failed = false;
    }

    pub fn fail(&self) {
        self.0.state().failed = true;
    }

    pub fn snapshot(&self) -> Snapshot {
        let state = self.0.state();
        Snapshot {
            pending: state.pending != 0,
            playing: !state.playbacks.is_empty(),
            level: state.playbacks.values().copied().fold(0.0, f32::max),
            failed: state.failed,
        }
    }
}

pub(crate) struct SpeechJob {
    shared: Arc<Shared>,
    epoch: u64,
}

impl SpeechJob {
    pub fn is_current(&self) -> bool {
        self.shared.epoch.load(Ordering::Acquire) == self.epoch
    }

    /// Returns false for cancellation, which must not become a speech error.
    pub fn fail(&self) -> bool {
        let mut state = self.shared.state();
        if !self.is_current() {
            return false;
        }
        state.failed = true;
        true
    }

    /// Call only once the output stream has actually started. The guard must
    /// outlive that stream, including its drain and cancellation/close paths.
    pub fn start_playback(&self) -> Playback {
        let mut state = self.shared.state();
        let id = state.next_playback;
        state.next_playback = state.next_playback.wrapping_add(1);
        state.playbacks.insert(id, 0.0);
        Playback {
            shared: self.shared.clone(),
            id,
        }
    }
}

impl Drop for SpeechJob {
    fn drop(&mut self) {
        let mut state = self.shared.state();
        if self.is_current() {
            state.pending -= 1;
        }
    }
}

pub(crate) struct Playback {
    shared: Arc<Shared>,
    id: u64,
}

impl Playback {
    /// Meter only the slice accepted by the output stream, not requested frames.
    pub fn written(&self, samples: &[f32]) {
        let level = if samples.is_empty() {
            0.0
        } else {
            let squares: f64 = samples
                .iter()
                .map(|&sample| {
                    if sample.is_finite() {
                        f64::from(sample).powi(2)
                    } else {
                        0.0
                    }
                })
                .sum();
            (squares / samples.len() as f64).sqrt().clamp(0.0, 1.0) as f32
        };
        if let Some(current) = self.shared.state().playbacks.get_mut(&self.id) {
            *current = level;
        }
    }
}

impl Drop for Playback {
    fn drop(&mut self) {
        self.shared.state().playbacks.remove(&self.id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn queued_jobs_remain_pending_until_all_complete() {
        let tracker = SpeechTracker::default();
        let first = tracker.begin_job();
        let second = tracker.begin_job();
        assert!(tracker.snapshot().pending);
        drop(first);
        assert!(tracker.snapshot().pending);
        drop(second);
        assert_eq!(tracker.snapshot(), Snapshot::default());
    }

    #[test]
    fn synthesis_and_chunk_gaps_do_not_look_ready() {
        let tracker = SpeechTracker::default();
        let job = tracker.begin_job();
        assert!(tracker.snapshot().pending);
        assert!(!tracker.snapshot().playing);
        let first_chunk = job.start_playback();
        first_chunk.written(&[0.5]);
        drop(first_chunk);
        assert_eq!(
            tracker.snapshot(),
            Snapshot {
                pending: true,
                ..Snapshot::default()
            }
        );
        let second_chunk = job.start_playback();
        assert!(tracker.snapshot().playing);
        drop(second_chunk);
        drop(job);
        assert_eq!(tracker.snapshot(), Snapshot::default());
    }

    #[test]
    fn cancellation_preserves_playback_but_excludes_stale_jobs_and_errors() {
        let tracker = SpeechTracker::default();
        let old = tracker.begin_job();
        let queued = tracker.begin_job();
        let playback = old.start_playback();
        playback.written(&[0.5]);
        tracker.cancel();
        assert!(!old.is_current());
        assert!(!tracker.snapshot().pending);
        assert!(tracker.snapshot().playing);
        assert_eq!(tracker.snapshot().level, 0.5);
        let new = tracker.begin_job();
        assert!(!old.fail());
        drop(old);
        drop(queued);
        assert!(tracker.snapshot().pending);
        assert!(!tracker.snapshot().failed);
        drop(playback);
        assert!(!tracker.snapshot().playing);
        assert_eq!(tracker.snapshot().level, 0.0);
        assert!(tracker.snapshot().pending);
        drop(new);
        assert_eq!(tracker.snapshot(), Snapshot::default());
    }

    #[test]
    fn failure_is_sticky_until_next_job_or_explicit_clear_not_cancellation() {
        let tracker = SpeechTracker::default();
        let job = tracker.begin_job();
        assert!(job.fail());
        drop(job);
        tracker.cancel();
        assert!(tracker.snapshot().failed);
        let next = tracker.begin_job();
        assert!(!tracker.snapshot().failed);
        assert!(next.fail());
        tracker.clear_failure();
        assert!(!tracker.snapshot().failed);
        drop(next);
        tracker.fail();
        assert!(tracker.snapshot().failed);
    }

    #[test]
    fn level_is_written_sample_rms_and_resets_for_silence_drain_and_end() {
        let tracker = SpeechTracker::default();
        let job = tracker.begin_job();
        let playback = job.start_playback();
        playback.written(&[0.5, -0.5, 0.0, 0.0]);
        assert!((tracker.snapshot().level - 0.125_f32.sqrt()).abs() < 1e-6);
        playback.written(&[0.0, 0.0]);
        assert_eq!(tracker.snapshot().level, 0.0);
        playback.written(&[1.0]);
        playback.written(&[]); // Drain remains playing, without a frozen meter.
        assert!(tracker.snapshot().playing);
        assert_eq!(tracker.snapshot().level, 0.0);
        playback.written(&[0.8]);
        drop(playback);
        assert_eq!(tracker.snapshot().level, 0.0);
        assert!(!tracker.snapshot().playing);
    }

    #[test]
    fn level_stays_finite_and_bounded_and_old_guard_cannot_clear_new_playback() {
        let tracker = SpeechTracker::default();
        let old = tracker.begin_job();
        let old_playback = old.start_playback();
        old_playback.written(&[f32::MAX, f32::NAN]);
        assert_eq!(tracker.snapshot().level, 1.0);
        tracker.cancel();
        let new = tracker.begin_job();
        let new_playback = new.start_playback();
        new_playback.written(&[0.25]);
        drop(old_playback);
        drop(old);
        assert!(tracker.snapshot().playing);
        assert_eq!(tracker.snapshot().level, 0.25);
        drop(new_playback);
        drop(new);
        assert_eq!(tracker.snapshot(), Snapshot::default());
    }
}
