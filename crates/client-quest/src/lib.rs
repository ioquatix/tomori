// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

#[cfg(target_os = "android")]
mod android;
#[cfg(target_os = "android")]
mod android_audio;
#[cfg(feature = "app")]
pub mod app;
mod builder;
#[cfg(not(target_os = "android"))]
mod cli;
mod vulkan;
mod xr;
mod xr_types;

// The modules this crate's own modules use, from `tomori-client`, `tomori-scene` and `tomori-render`, under their paths.
#[cfg(target_os = "android")]
use tomori_client::identity;
use tomori_client::{connection, feedback};
use tomori_render::{controller, graphics, scene_graphics};
use tomori_scene::{conversation, hands, haptics, log_cadence, scene};

/// The Android activity a downstream build's `android_main` is given, at the version Tomori uses.
#[cfg(target_os = "android")]
pub use android_activity::AndroidApp;
pub use builder::Builder;
#[cfg(not(target_os = "android"))]
pub use cli::{Command, Mode};
pub use tomori_client::identity::Overrides;
/// The desktop's modes, from `tomori-client-desktop`.
#[cfg(not(target_os = "android"))]
pub use tomori_client_desktop::{
    DESKTOP_CONTROLS, audio_application_name, pipewire_alsa_properties, run_discover,
};
/// What shows the desktop's windows: [`Desktop`], the window system, or a test's, such as `tomori_client_desktop::window::test::Headless`.
#[cfg(not(target_os = "android"))]
pub use tomori_client_desktop::{desktop_window::Desktop, window::Shell};
/// The spectator's modes, from `tomori-client-spectator`.
#[cfg(not(target_os = "android"))]
pub use tomori_client_spectator::{
    SpectatorOptions, run_pipewire as run_spectator_pipewire,
    run_pipewire_preview as run_spectator_pipewire_preview, run_window as run_spectator,
};
/// How participants travel in an environment, for [`Builder::locomotion`]; see `design/locomotion.md`.
pub use tomori_scene::locomotion::{Locomotion, Surface};

/// The `tomori` executable's command line, running `builder`'s headset: `tomori --xr`, `--desktop`, the server-less window and the spectator modes draw its sceneries, and `--console` names its environments. Tomori's own `main` is `tomori::main(tomori::app::builder())`, and a downstream build's `main` is `tomori::main(builder)`.
#[cfg(not(target_os = "android"))]
pub fn main(builder: Builder) -> anyhow::Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    let command = Command::parse(
        std::env::args().skip(1),
        std::env::var("TOMORI_SERVER").ok(),
        std::env::var("TOMORI_TOKEN").ok(),
    )?;
    for (name, value) in command.environment(std::env::var_os("PIPEWIRE_ALSA").is_some()) {
        // SAFETY: the process has started no other thread yet, so nothing reads the environment concurrently.
        unsafe { std::env::set_var(name, value) };
    }
    command.run_with(builder, &Desktop)
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

/// Tomori's own headset, [`app::builder`], with the default `app` feature, which a build of its own turns off to write its own `android_main`; see [`Builder::run_android`].
#[cfg(all(target_os = "android", feature = "app"))]
#[unsafe(no_mangle)]
fn android_main(app: AndroidApp) {
    crate::app::builder().run_android(app)
}

/// The headset on Android, drawing `sceneries`, until the activity is destroyed; see [`Builder::run_android`].
#[cfg(target_os = "android")]
fn run_android(app: AndroidApp, sceneries: &tomori_render::scenery::Sceneries) {
    use std::time::Duration;

    android_logger::init_once(
        android_logger::Config::default()
            .with_tag("tomori")
            .with_max_level(log::LevelFilter::Info),
    );
    std::panic::set_hook(Box::new(|info| log::error!("{info}")));
    // Environments' assets are bundled in the APK's `assets` folder; see design/assets.md.
    tomori_assets::Library::shared().add(android::ApkAssets::new(&app));
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
            let samples = android_samples(&android);
            let (mut conversation, connection) = android_connection(android);
            conversation.set_drawable(sceneries.environments());
            conversation.set_over_grid(sceneries.over_grid());
            conversation.set_locomotion(sceneries.locomotion());
            conversation.set_client_sides(sceneries.client_sides());
            xr::run(
                &platform,
                samples,
                || pump.poll(Duration::ZERO),
                conversation,
                connection,
                sceneries,
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

/// The samples per pixel the headset draws with: the intent extra `tomori.msaa`, as with `am start -e tomori.msaa off`, or 4 for multisample anti-aliasing.
#[cfg(target_os = "android")]
fn android_samples(android: &android::Android) -> u32 {
    let value = android.intent_extra("tomori.msaa").unwrap_or_else(|error| {
        log::warn!("Cannot read the tomori.msaa intent extra: {error:#}");
        None
    });
    graphics::requested_samples(value.as_deref())
}

/// The intent extras `tomori.server`, `tomori.name` and `tomori.token` override the defaults, as with `am start -e tomori.server 10.0.0.2`.
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
    let mut overrides = Overrides::default();
    overrides.server = extra("tomori.server");
    overrides.name = extra("tomori.name");
    overrides.token = extra("tomori.token");
    let device = android.device_name().unwrap_or_else(|error| {
        log::warn!("Cannot read the device name: {error:#}");
        None
    });
    let store = identity::Store::new(android.data());
    let identity = identity::Identity::new(&store, device.as_deref(), overrides);
    let mut conversation = conversation::Conversation::new(&identity.name);
    // `am start -e tomori.echo_test on` measures the echo each time the live microphone opens, with the platform's echo canceller; `canceller-off` with it disabled, all else equal; and `uncancelled` with the recognition preset and media output, as push-to-talk captures. See `design/spatial-audio.md`.
    let echo_test = extra("tomori.echo_test");
    let (label, uncancelled, canceller) = match echo_test.as_deref() {
        None => (None, false, true),
        Some("on") => (Some("communication preset, echo canceller on"), false, true),
        Some("canceller-off") => (
            Some("communication preset, echo canceller off"),
            false,
            false,
        ),
        Some("uncancelled") => (
            Some("recognition preset, media output, no echo canceller"),
            true,
            true,
        ),
        Some(other) => {
            log::warn!("Ignored tomori.echo_test {other:?}: use on, canceller-off or uncancelled");
            (None, false, true)
        }
    };
    android.log_audio_effects();
    let capture = Box::new(android_audio::capture::Android::new(
        android.clone(),
        uncancelled,
        canceller,
    ));
    let multicast = Box::new(android::MulticastLock::new(android));
    let playback = Box::new(android_audio::playback::Android);
    let mut connection = connection::Connection::new(
        identity,
        store,
        multicast,
        capture,
        playback,
        &mut conversation,
    );
    if let Some(label) = label {
        connection.set_echo_test(label, !uncancelled);
    }
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
