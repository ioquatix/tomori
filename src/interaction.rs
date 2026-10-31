// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

//! Observable conversational activity, independent of the XR/audio backends.
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[allow(dead_code)] // Produced on Quest; also exercised by desktop tests.
pub enum SessionActivity {
    #[default]
    Unselected,
    Unknown,
    Working,
    Waiting,
    Ready,
    Unavailable,
}

#[derive(Clone, Copy, Debug, Default)]
#[allow(dead_code)]
pub struct SpeechActivity {
    pub pending: bool,
    pub playing: bool,
    pub level: f32,
    pub failed: bool,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    #[default]
    Neutral,
    Unknown,
    Working,
    Preparing,
    Speaking,
    Ready,
    Waiting,
    Listening,
    Transcribing,
    Unavailable,
}

/// All fields are observed status, never an estimate of hidden reasoning.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Visual {
    pub mode: Mode,
    pub level: f32,
    pub working: bool,
    pub time: f32,
}

impl Visual {
    #[cfg(any(not(target_os = "android"), test))]
    pub fn valid(&self) -> bool {
        self.level.is_finite()
            && (0.0..=1.0).contains(&self.level)
            && self.time.is_finite()
            && self.time >= 0.0
    }

    pub fn color(self) -> [f32; 3] {
        match self.mode {
            Mode::Neutral => [0.3, 0.75, 1.0],
            Mode::Unknown => [0.3, 0.35, 0.48],
            Mode::Working => [0.38, 0.38, 1.0],
            Mode::Preparing => [0.65, 0.45, 0.95],
            Mode::Speaking => [0.15, 0.8, 1.0],
            Mode::Ready => [0.15, 0.95, 0.65],
            Mode::Waiting => [1.0, 0.65, 0.16],
            Mode::Listening => [0.25, 0.95, 1.0],
            Mode::Transcribing => [0.4, 0.65, 1.0],
            Mode::Unavailable => [1.0, 0.25, 0.15],
        }
    }

    /// Perceptual gain makes ordinary speech visible without changing telemetry.
    pub fn amplitude(self) -> f32 {
        (self.level * 6.0).clamp(0.0, 1.0).sqrt()
    }
}

#[derive(Default)]
#[allow(dead_code)]
pub struct Inputs {
    pub session: SessionActivity,
    pub speech: SpeechActivity,
    pub recording: bool,
    pub voice_busy: bool,
    pub voice_error: bool,
    pub pending_text: bool,
}

/// A small settling interval avoids a one-frame ready flash at hand-offs.
#[derive(Default)]
#[allow(dead_code)]
pub struct Signals {
    ready_for: f32,
}

impl Signals {
    #[allow(dead_code)]
    pub fn update(&mut self, input: Inputs, dt: f32) -> Visual {
        let mode = if input.recording {
            Mode::Listening
        } else if input.speech.playing {
            Mode::Speaking
        } else if input.voice_busy {
            Mode::Transcribing
        } else if input.voice_error
            || input.speech.failed
            || input.session == SessionActivity::Unavailable
        {
            Mode::Unavailable
        } else if input.pending_text || input.speech.pending {
            Mode::Preparing
        } else {
            match input.session {
                SessionActivity::Unselected => Mode::Neutral,
                SessionActivity::Unknown => Mode::Unknown,
                SessionActivity::Working => Mode::Working,
                SessionActivity::Waiting => Mode::Waiting,
                SessionActivity::Ready => Mode::Ready,
                SessionActivity::Unavailable => Mode::Unavailable,
            }
        };
        self.ready_for = if mode == Mode::Ready {
            self.ready_for + dt.clamp(0.0, 0.1)
        } else {
            0.0
        };
        Visual {
            mode: if mode == Mode::Ready && self.ready_for < 0.8 {
                Mode::Preparing
            } else {
                mode
            },
            level: if input.speech.playing && input.speech.level.is_finite() {
                input.speech.level.clamp(0.0, 1.0)
            } else {
                0.0
            },
            working: input.session == SessionActivity::Working,
            time: 0.0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn queued_speech_and_chunk_gaps_never_look_ready() {
        let mut signals = Signals::default();
        for playing in [false, true, false, true, false] {
            let visual = signals.update(
                Inputs {
                    session: SessionActivity::Ready,
                    speech: SpeechActivity {
                        pending: true,
                        playing,
                        level: 0.5,
                        failed: false,
                    },
                    ..Default::default()
                },
                0.02,
            );
            assert_eq!(
                visual.mode,
                if playing {
                    Mode::Speaking
                } else {
                    Mode::Preparing
                }
            );
        }
        for _ in 0..7 {
            assert_eq!(
                signals
                    .update(
                        Inputs {
                            session: SessionActivity::Ready,
                            ..Default::default()
                        },
                        0.1
                    )
                    .mode,
                Mode::Preparing
            );
        }
        assert_eq!(
            signals
                .update(
                    Inputs {
                        session: SessionActivity::Ready,
                        ..Default::default()
                    },
                    0.1
                )
                .mode,
            Mode::Ready
        );
    }

    #[test]
    fn waits_errors_and_uncertainty_are_not_ready() {
        for (session, expected) in [
            (SessionActivity::Waiting, Mode::Waiting),
            (SessionActivity::Unknown, Mode::Unknown),
            (SessionActivity::Unavailable, Mode::Unavailable),
        ] {
            assert_eq!(
                Signals::default()
                    .update(
                        Inputs {
                            session,
                            ..Default::default()
                        },
                        5.0
                    )
                    .mode,
                expected
            );
        }
    }

    #[test]
    fn recording_and_speech_are_distinct_from_work() {
        let mut signals = Signals::default();
        let visual = signals.update(
            Inputs {
                session: SessionActivity::Working,
                recording: true,
                ..Default::default()
            },
            0.01,
        );
        assert_eq!(visual.mode, Mode::Listening);
        assert!(visual.working);
        let visual = signals.update(
            Inputs {
                session: SessionActivity::Working,
                speech: SpeechActivity {
                    playing: true,
                    level: 0.7,
                    ..Default::default()
                },
                ..Default::default()
            },
            0.01,
        );
        assert_eq!(visual.mode, Mode::Speaking);
        assert!(visual.working);
        assert_eq!(visual.level, 0.7);
    }

    #[test]
    fn visual_rejects_invalid_audio_or_time() {
        assert!(Visual::default().valid());
        assert!(
            !Visual {
                level: f32::NAN,
                ..Default::default()
            }
            .valid()
        );
        assert!(
            !Visual {
                level: 1.1,
                ..Default::default()
            }
            .valid()
        );
        assert!(
            !Visual {
                time: -1.0,
                ..Default::default()
            }
            .valid()
        );
    }
}
