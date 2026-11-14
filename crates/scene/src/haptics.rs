// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

//! The controllers' vibration: what [`tomori_client::feedback::Feedback`] hands out each frame, applied through OpenXR's haptic output action, one action, `haptic`, for both hands, which `xr.rs` creates and calls. See `design/feedback.md`.
use std::time::Duration;
use tomori_client::feedback::{Hand, Haptic};

/// A controller vibration output, as OpenXR's haptic action provides it.
pub trait Vibrator {
    /// Why a call failed.
    type Error: Copy + PartialEq + std::fmt::Display;
    /// Vibrate `hand` at `amplitude`, between 0 and 1, for `length`, or as briefly as the runtime can when `None`.
    fn vibrate(
        &self,
        hand: Hand,
        length: Option<Duration>,
        amplitude: f32,
    ) -> Result<(), Self::Error>;
    /// Stop vibrating `hand`.
    fn stop(&self, hand: Hand) -> Result<(), Self::Error>;
}

/// Applies the vibrations, logging a failure when it changes, so that a repeated failure is logged once.
pub struct Haptics<E> {
    failure: Option<E>,
}

impl<E> Default for Haptics<E> {
    fn default() -> Self {
        Self { failure: None }
    }
}

impl<E: Copy + PartialEq + std::fmt::Display> Haptics<E> {
    /// Vibrate `hand` through `output`, or stop it. A failure never stops the frame.
    pub fn apply(&mut self, output: &impl Vibrator<Error = E>, hand: Hand, haptic: Haptic) {
        let result = match haptic {
            Haptic::Vibrate { length, amplitude } => output.vibrate(hand, length, amplitude),
            Haptic::Stop => output.stop(hand),
        };
        match result {
            Ok(()) => {
                if self.failure.take().is_some() {
                    log::info!("Controller vibration works again");
                }
            }
            Err(error) if self.failure != Some(error) => {
                log::warn!("Cannot vibrate the {hand:?} controller: {error}");
                self.failure = Some(error);
            }
            Err(_) => {}
        }
    }

    /// The failure logged last, until a vibration works again.
    #[cfg(test)]
    fn failure(&self) -> Option<E> {
        self.failure
    }
}

/// The index of `hand` in OpenXR's `[left, right]` subaction paths.
pub fn index(hand: Hand) -> usize {
    match hand {
        Hand::Left => 0,
        Hand::Right => 1,
    }
}

/// How long OpenXR vibrates for `length`, in nanoseconds; `None` asks for the runtime's shortest.
pub fn nanoseconds(length: Option<Duration>) -> Option<i64> {
    length.map(|length| i64::try_from(length.as_nanos()).unwrap_or(i64::MAX))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    /// Records each call, and fails while `fail` holds an error.
    #[derive(Default)]
    struct Recorder {
        calls: RefCell<Vec<String>>,
        fail: Option<&'static str>,
    }

    impl Vibrator for Recorder {
        type Error = &'static str;

        fn vibrate(
            &self,
            hand: Hand,
            length: Option<Duration>,
            amplitude: f32,
        ) -> Result<(), Self::Error> {
            self.calls
                .borrow_mut()
                .push(format!("vibrate {hand:?} {length:?} {amplitude}"));
            self.fail.map_or(Ok(()), Err)
        }

        fn stop(&self, hand: Hand) -> Result<(), Self::Error> {
            self.calls.borrow_mut().push(format!("stop {hand:?}"));
            self.fail.map_or(Ok(()), Err)
        }
    }

    #[test]
    fn vibrations_and_stops_reach_the_output() {
        let output = Recorder::default();
        let mut haptics = Haptics::default();
        haptics.apply(
            &output,
            Hand::Left,
            Haptic::Vibrate {
                length: Some(Duration::from_millis(20)),
                amplitude: 0.5,
            },
        );
        haptics.apply(&output, Hand::Right, Haptic::Stop);
        assert_eq!(
            *output.calls.borrow(),
            ["vibrate Left Some(20ms) 0.5", "stop Right"]
        );
        assert_eq!(haptics.failure(), None);
    }

    #[test]
    fn a_failure_is_kept_until_vibration_works_again() {
        crate::test_support::log_everything();
        let mut haptics = Haptics::default();
        let failing = Recorder {
            fail: Some("lost"),
            ..Default::default()
        };
        haptics.apply(&failing, Hand::Left, Haptic::Stop);
        assert_eq!(haptics.failure(), Some("lost"));
        // The same failure again is not logged again; another one is.
        haptics.apply(&failing, Hand::Left, Haptic::Stop);
        assert_eq!(haptics.failure(), Some("lost"));
        let other = Recorder {
            fail: Some("busy"),
            ..Default::default()
        };
        haptics.apply(&other, Hand::Right, Haptic::Stop);
        assert_eq!(haptics.failure(), Some("busy"));
        haptics.apply(&Recorder::default(), Hand::Right, Haptic::Stop);
        assert_eq!(haptics.failure(), None);
    }

    #[test]
    fn hands_and_lengths_are_given_in_openxr_terms() {
        assert_eq!(index(Hand::Left), 0);
        assert_eq!(index(Hand::Right), 1);
        assert_eq!(nanoseconds(None), None);
        assert_eq!(nanoseconds(Some(Duration::from_millis(3))), Some(3_000_000));
        assert_eq!(nanoseconds(Some(Duration::MAX)), Some(i64::MAX));
    }
}
