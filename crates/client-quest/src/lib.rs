// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

#[cfg(target_os = "android")]
mod audience;
mod caption;
#[cfg(any(target_os = "android", test))]
#[cfg_attr(not(target_os = "android"), allow(dead_code))]
mod chat;
mod controller;
mod controller_graphics;
#[cfg_attr(not(target_os = "android"), allow(dead_code))]
mod delegation;
#[cfg(not(target_os = "android"))]
mod desktop;
mod environment;
mod graphics;
mod hands;
#[cfg(not(target_os = "android"))]
mod headless_spectator;
mod interaction;
#[cfg(not(target_os = "android"))]
mod pipewire_video;
mod spectator;
#[cfg(not(target_os = "android"))]
mod spectator_view;
#[cfg(any(target_os = "android", test))]
mod speech;
#[cfg(any(target_os = "android", test))]
#[cfg_attr(not(target_os = "android"), allow(dead_code))]
mod speech_activity;
#[cfg(any(target_os = "android", test))]
mod spoken;
mod swarm;
#[cfg(target_os = "android")]
mod tts;
#[cfg(target_os = "android")]
mod voice;
mod vulkan;
mod workspace_graphics;
mod xr;

/// Run the windowed desktop demo without an XR runtime or headset.
#[cfg(not(target_os = "android"))]
pub fn run() -> anyhow::Result<()> {
    desktop::run(false, false)
}

/// Render a read-only, headset-following view from the PC bridge.
#[cfg(not(target_os = "android"))]
pub fn run_spectator() -> anyhow::Result<()> {
    desktop::run(true, false)
}

/// Prototype direct PipeWire output without a window.
#[cfg(not(target_os = "android"))]
pub fn run_spectator_pipewire() -> anyhow::Result<()> {
    anyhow::ensure!(
        cfg!(target_os = "linux"),
        "PipeWire video output requires Linux"
    );
    headless_spectator::run()
}

/// Prototype direct PipeWire output with a local preview window.
#[cfg(not(target_os = "android"))]
pub fn run_spectator_pipewire_preview() -> anyhow::Result<()> {
    anyhow::ensure!(
        cfg!(target_os = "linux"),
        "PipeWire video output requires Linux"
    );
    desktop::run(true, true)
}

/// Run against the host's active OpenXR runtime instead of a desktop window.
#[cfg(not(target_os = "android"))]
pub fn run_xr() -> anyhow::Result<()> {
    xr::run(&(), || false)
}

// Bound each input batch so that a continuous input stream cannot prevent the
// next lifecycle poll. A full batch may still leave input in the queue.
#[cfg(any(target_os = "android", test))]
fn drain_input_batch(mut next: impl FnMut() -> bool) -> bool {
    const BATCH_SIZE: usize = 64;
    for _ in 0..BATCH_SIZE {
        if !next() {
            return false;
        }
    }
    true
}

#[cfg(target_os = "android")]
struct AndroidEventPump<'a> {
    app: &'a android_activity::AndroidApp,
    destroyed: bool,
    input_pending: bool,
    input_retry_at: Option<std::time::Instant>,
    last_input_error_log: Option<std::time::Instant>,
    last_poll: std::time::Instant,
    last_gap_log: Option<std::time::Instant>,
}

#[cfg(target_os = "android")]
impl<'a> AndroidEventPump<'a> {
    fn new(app: &'a android_activity::AndroidApp) -> Self {
        Self {
            app,
            destroyed: false,
            input_pending: false,
            input_retry_at: None,
            last_input_error_log: None,
            last_poll: std::time::Instant::now(),
            last_gap_log: None,
        }
    }

    // Must run on android_main's thread. No input iterator outlives this call.
    fn poll(&mut self, timeout: std::time::Duration) -> bool {
        use android_activity::{InputStatus, MainEvent, PollEvent};
        use std::time::{Duration, Instant};

        if self.destroyed {
            return true;
        }

        let now = Instant::now();
        let gap = now.duration_since(self.last_poll);
        if gap >= Duration::from_secs(2)
            && self
                .last_gap_log
                .is_none_or(|last| now.duration_since(last) >= Duration::from_secs(30))
        {
            log::warn!("Android event pump gap: {} ms", gap.as_millis());
            self.last_gap_log = Some(now);
        }
        self.last_poll = now;

        // Retry a failed iterator even if the detached queue cannot wake us.
        // A full batch also needs another turn, without waiting for a wakeup.
        let timeout = if self.input_pending {
            timeout.min(
                self.input_retry_at
                    .map_or(Duration::ZERO, |retry| retry.saturating_duration_since(now)),
            )
        } else {
            timeout
        };
        self.app.poll_events(Some(timeout), |event| match event {
            PollEvent::Main(MainEvent::Destroy) => self.destroyed = true,
            PollEvent::Main(MainEvent::InputAvailable) => self.input_pending = true,
            _ => {}
        });

        if !self.destroyed
            && self.input_pending
            && self
                .input_retry_at
                .is_none_or(|retry| Instant::now() >= retry)
        {
            // Acquiring the iterator reattaches the queue. Its next() performs
            // Android pre-dispatch and finishes each event; Unhandled preserves
            // the platform's default behavior, including for system/back keys.
            match self.app.input_events_iter() {
                Ok(mut input) => {
                    self.input_pending =
                        drain_input_batch(|| input.next(|_| InputStatus::Unhandled));
                    self.input_retry_at = None;
                    self.last_input_error_log = None;
                }
                Err(error) => {
                    let now = Instant::now();
                    if self
                        .last_input_error_log
                        .is_none_or(|last| now.duration_since(last) >= Duration::from_secs(10))
                    {
                        log::warn!("Failed to acquire Android input iterator: {error:?}");
                        self.last_input_error_log = Some(now);
                    }
                    self.input_retry_at = Some(now + Duration::from_millis(100));
                }
            }
        }
        self.destroyed
    }
}

#[cfg(target_os = "android")]
#[unsafe(no_mangle)]
fn android_main(app: android_activity::AndroidApp) {
    use std::time::Duration;

    android_logger::init_once(
        android_logger::Config::default()
            .with_tag("tomori")
            .with_max_level(log::LevelFilter::Info),
    );
    std::panic::set_hook(Box::new(|info| log::error!("{info}")));
    let mut pump = AndroidEventPump::new(&app);
    // Service already queued startup events without allowing an input flood to
    // delay initialization indefinitely.
    for _ in 0..8 {
        if pump.poll(Duration::ZERO) {
            return;
        }
    }
    // ndk-context exposes the Application in android-activity 0.6.1, NOT the
    // Activity. XR must receive the Activity so it can observe resume/focus.
    // AndroidApp keeps these JNI references alive until android_main returns.
    let platform =
        unsafe { openxr::AndroidPlatformInfo::new(app.vm_as_ptr(), app.activity_as_ptr()) };
    let mut voice = voice::Voice::new(&app);
    for _ in 0..8 {
        if pump.poll(Duration::ZERO) {
            break;
        }
    }
    let result = if pump.destroyed {
        Ok(())
    } else {
        xr::run(&platform, || pump.poll(Duration::ZERO), &mut voice)
    };
    // Poll before potentially blocking cleanup, unless Destroy already arrived.
    pump.poll(Duration::ZERO);
    drop(voice); // release mic/recognizer before waiting for Activity destruction
    if let Err(error) = result {
        log::error!("tomori stopped: {error:#}");
    }
    // NativeActivity must continue acknowledging lifecycle events until Destroy,
    // including when XR initialization failed or the runtime requested exit.
    while !pump.destroyed {
        pump.poll(Duration::from_millis(100));
    }
}

#[cfg(test)]
mod android_event_pump_tests {
    use super::drain_input_batch;

    #[test]
    fn stops_when_input_is_empty() {
        let mut calls = 0;
        let pending = drain_input_batch(|| {
            calls += 1;
            calls <= 3
        });
        assert!(!pending);
        assert_eq!(calls, 4);
    }

    #[test]
    fn leaves_a_flood_for_another_lifecycle_poll() {
        let mut remaining = 100;
        let mut next = || {
            if remaining == 0 {
                false
            } else {
                remaining -= 1;
                true
            }
        };
        assert!(drain_input_batch(&mut next));
        assert!(!drain_input_batch(&mut next));
        assert_eq!(remaining, 0);
    }

    #[test]
    fn exact_batch_gets_an_extra_empty_check_next_turn() {
        let mut remaining = 64;
        let mut next = || {
            if remaining == 0 {
                false
            } else {
                remaining -= 1;
                true
            }
        };
        assert!(drain_input_batch(&mut next));
        assert!(!drain_input_batch(&mut next));
    }
}
