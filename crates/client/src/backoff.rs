// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

use std::time::Duration;

/// Reconnection delays: doubling from the initial delay up to the limit, reset after a successful connection.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Backoff {
    pub initial_delay: Duration,
    pub delay_limit: Duration,
}

impl Default for Backoff {
    fn default() -> Self {
        Self {
            initial_delay: Duration::from_millis(250),
            delay_limit: Duration::from_secs(5),
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
}
