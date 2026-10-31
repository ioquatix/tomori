// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

#[cfg(target_os = "android")]
mod android;
mod caption;
mod capture;
mod connection;
#[cfg(not(target_os = "android"))]
mod console;
mod controller;
mod controller_graphics;
mod conversation;
#[cfg_attr(not(target_os = "android"), allow(dead_code))]
mod delegation;
#[cfg(not(target_os = "android"))]
mod desktop;
mod environment;
mod graphics;
mod hands;
#[cfg(not(target_os = "android"))]
mod headless_spectator;
mod identity;
mod interaction;
#[cfg(not(target_os = "android"))]
mod pipewire_video;
mod server_choice;
#[cfg(not(target_os = "android"))]
mod spectator;
#[cfg(not(target_os = "android"))]
mod spectator_view;
mod swarm;
mod vulkan;
mod workspace_graphics;
mod xr;

pub use identity::Overrides;

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
pub fn run_xr(overrides: Overrides) -> anyhow::Result<()> {
    let (conversation, connection) = desktop_connection(overrides);
    xr::run(&(), || false, conversation, connection)
}

/// Show the headset's panel in the terminal, to use a server without a headset or a GPU.
#[cfg(not(target_os = "android"))]
pub fn run_console(overrides: Overrides) -> anyhow::Result<()> {
    let (conversation, connection) = desktop_connection(overrides);
    console::run(conversation, connection)
}

/// Desktop runs keep nothing, so each is a new participant.
#[cfg(not(target_os = "android"))]
fn desktop_connection(
    overrides: Overrides,
) -> (conversation::Conversation, connection::Connection) {
    let store = identity::Store::new(None);
    let identity = identity::Identity::new(&store, identity::host_name().as_deref(), overrides);
    let mut conversation = conversation::Conversation::new(&identity.participant, &identity.name);
    let connection = connection::Connection::new(
        identity,
        store,
        Box::new(()),
        Box::new(capture::Desktop),
        &mut conversation,
    );
    (conversation, connection)
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
    let result = match android::Android::new(&app) {
        Ok(_) if pump.destroyed => Ok(()),
        // xr::run owns the connection, so it closes before Destroy is acknowledged.
        Ok(android) => {
            let (conversation, connection) = android_connection(android);
            xr::run(
                &platform,
                || pump.poll(Duration::ZERO),
                conversation,
                connection,
            )
        }
        Err(error) => Err(error.context("reaching the Android activity")),
    };
    if let Err(error) = result {
        log::error!("tomori stopped: {error:#}");
    }
    // NativeActivity must continue acknowledging lifecycle events until Destroy,
    // including when XR initialization failed or the runtime requested exit.
    while !pump.destroyed {
        pump.poll(Duration::from_millis(100));
    }
}

/// The intent extras `tomori.server` and `tomori.name` override the defaults, as with `am start -e tomori.server 10.0.0.2`.
#[cfg(target_os = "android")]
fn android_connection(
    android: std::sync::Arc<android::Android>,
) -> (conversation::Conversation, connection::Connection) {
    let extra = |name| {
        android.intent_extra(name).unwrap_or_else(|error| {
            log::warn!("Cannot read the {name} intent extra: {error:#}");
            None
        })
    };
    let overrides = Overrides {
        server: extra("tomori.server"),
        name: extra("tomori.name"),
    };
    let device = android.device_name().unwrap_or_else(|error| {
        log::warn!("Cannot read the device name: {error:#}");
        None
    });
    let store = identity::Store::new(android.data());
    let identity = identity::Identity::new(&store, device.as_deref(), overrides);
    let mut conversation = conversation::Conversation::new(&identity.participant, &identity.name);
    let capture = Box::new(capture::Android::new(android.clone()));
    let multicast = Box::new(android::MulticastLock::new(android));
    let connection =
        connection::Connection::new(identity, store, multicast, capture, &mut conversation);
    (conversation, connection)
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
