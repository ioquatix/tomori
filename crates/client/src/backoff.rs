// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

use std::time::Duration;

/// Reconnection delays: doubling from the initial delay up to the limit, reset after a connection that stayed up.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Backoff {
    pub initial_delay: Duration,
    pub delay_limit: Duration,
    /// How long a welcomed connection must last before it resets the delay. A server that welcomes clients and then drops them at once is retried with growing delays, like one that refuses them.
    pub stable_duration: Duration,
}

impl Default for Backoff {
    fn default() -> Self {
        Self {
            initial_delay: Duration::from_millis(250),
            delay_limit: Duration::from_secs(5),
            stable_duration: Duration::from_secs(10),
        }
    }
}

impl Backoff {
    /// The delay before reconnection attempt `failures` (counting from one).
    pub fn delay(&self, failures: u32) -> Duration {
        let factor = 1u32
            .checked_shl(failures.saturating_sub(1))
            .unwrap_or(u32::MAX);
        self.initial_delay
            .saturating_mul(factor)
            .min(self.delay_limit)
    }

    /// The consecutive failures after a connection ends, given those before it and how long it was welcomed for, if at all. A connection that lasted [`Self::stable_duration`] counts as the first failure of a new run.
    pub fn failures_after(&self, failures: u32, welcomed_for: Option<Duration>) -> u32 {
        match welcomed_for {
            Some(duration) if duration >= self.stable_duration => 1,
            _ => failures.saturating_add(1),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn doubles_up_to_the_limit() {
        let backoff = Backoff::default();
        let delays: Vec<_> = [1, 2, 3, 4, 5, 6, 40]
            .into_iter()
            .map(|failures| backoff.delay(failures).as_millis())
            .collect();
        assert_eq!(delays, [250, 500, 1000, 2000, 4000, 5000, 5000]);
        assert_eq!(backoff.delay(0), Duration::from_millis(250));
    }

    #[test]
    fn resets_only_after_a_stable_connection() {
        let backoff = Backoff::default();
        // Refused, then welcomed but dropped at once: the delay keeps growing.
        let mut failures = 0;
        for welcomed_for in [
            None,
            None,
            Some(Duration::from_millis(5)),
            Some(Duration::ZERO),
        ] {
            failures = backoff.failures_after(failures, welcomed_for);
        }
        assert_eq!(failures, 4);
        assert_eq!(backoff.delay(failures), Duration::from_secs(2));

        // A connection that stayed up starts over.
        failures = backoff.failures_after(failures, Some(Duration::from_secs(10)));
        assert_eq!(failures, 1);
        assert_eq!(backoff.failures_after(u32::MAX, None), u32::MAX);
    }
}
