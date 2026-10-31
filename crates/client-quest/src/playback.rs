// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

//! The platform speaker, which plays other participants and the agent's speech through the client's [`client::playback::Player`]. See `design/live-audio.md`.
//!
//! The output stream stays open while connected. It opens and closes on its own thread, so that the frame does not wait for the device. Its callback renders the player at the device rate; [`crate::connection::Connection`] controls the player and silences it while push-to-talk records.

use client::playback::Player;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
    mpsc,
};

/// Told, from any thread, why the output stopped.
pub type Failed = Box<dyn Fn(String) + Send + Sync>;

/// The player, shared with the output callback. Only the callback of one open stream uses it at a time; the application keeps it so that a new stream can take over after a failure.
pub type Shared = Arc<Mutex<Player>>;

/// An open output stream, stopped when dropped. Created and dropped on the speaker thread.
pub struct Output {
    _stream: Box<dyn std::any::Any>,
}

impl Output {
    pub fn new(stream: impl std::any::Any) -> Self {
        Self {
            _stream: Box::new(stream),
        }
    }
}

/// A destination for played audio.
pub trait Playback: Send + Sync {
    /// Start playing `player` until the returned output is dropped. Called on the speaker thread.
    fn open(&self, player: Shared, failed: Failed) -> Result<Output, String>;
}

/// The speaker thread, which opens an output and keeps it until this is dropped, then closes it. Dropping does not wait for the device to close.
pub struct Speaking {
    _stop: mpsc::Sender<()>,
    opened: Arc<AtomicBool>,
}

impl Speaking {
    /// Whether the output has opened. It may have failed since.
    pub fn is_open(&self) -> bool {
        self.opened.load(Ordering::Relaxed)
    }
}

/// Open `playback` on its own thread. Failing to open, and failures once open, are reported through `failed`.
pub fn start(
    playback: Arc<dyn Playback>,
    player: Shared,
    failed: Failed,
) -> Result<Speaking, String> {
    let (stop, stopped) = mpsc::channel::<()>();
    let failed: Arc<dyn Fn(String) + Send + Sync> = Arc::from(failed);
    let opened = Arc::new(AtomicBool::new(false));
    let thread_opened = opened.clone();
    std::thread::Builder::new()
        .name("tomori-speaker".into())
        .spawn(move || {
            let report = failed.clone();
            match playback.open(player, Box::new(move |error| report(error))) {
                // Kept until the speaker is dropped, which disconnects the channel.
                Ok(output) => {
                    thread_opened.store(true, Ordering::Relaxed);
                    let _ = stopped.recv();
                    drop(output);
                }
                Err(error) => failed(error),
            }
        })
        .map_err(|error| format!("Cannot start the speaker thread: {error}"))?;
    Ok(Speaking {
        _stop: stop,
        opened,
    })
}

/// Render the player as mono at `sample_rate` into `mono`, or silence if another stream holds it.
fn render(player: &Shared, sample_rate: u32, mono: &mut [i16]) {
    match player.try_lock() {
        Ok(mut player) => player.render(sample_rate, mono),
        // A stream that is being replaced, or a poisoned player, plays nothing rather than waiting.
        Err(_) => mono.fill(0),
    }
}

#[cfg(not(target_os = "android"))]
pub use desktop::Desktop;

/// The default output device, through cpal.
#[cfg(not(target_os = "android"))]
mod desktop {
    use super::{Failed, Output, Playback, Shared, render};
    use cpal::{
        ErrorKind, FromSample, SampleFormat, SizedSample, StreamConfig,
        traits::{DeviceTrait, HostTrait, StreamTrait},
    };

    pub struct Desktop;

    impl Playback for Desktop {
        fn open(&self, player: Shared, failed: Failed) -> Result<Output, String> {
            let device = cpal::default_host()
                .default_output_device()
                .ok_or("No speaker is available.")?;
            let supported = device
                .default_output_config()
                .map_err(|error| format!("Cannot use the speaker: {error}"))?;
            let config: StreamConfig = supported.config();
            let stream = match supported.sample_format() {
                SampleFormat::F32 => build::<f32>(&device, &config, player, failed),
                SampleFormat::I16 => build::<i16>(&device, &config, player, failed),
                SampleFormat::I32 => build::<i32>(&device, &config, player, failed),
                SampleFormat::U16 => build::<u16>(&device, &config, player, failed),
                format => {
                    return Err(format!("The speaker's {format} samples are not supported."));
                }
            }?;
            stream
                .play()
                .map_err(|error| format!("Cannot start the speaker: {error}"))?;
            log::info!(
                "Speaker opened at {} Hz, {} channels",
                config.sample_rate,
                config.channels
            );
            Ok(Output::new(stream))
        }
    }

    fn build<T>(
        device: &cpal::Device,
        config: &StreamConfig,
        player: Shared,
        failed: Failed,
    ) -> Result<cpal::Stream, String>
    where
        T: SizedSample + FromSample<i16>,
    {
        let channels = usize::from(config.channels).max(1);
        let sample_rate = config.sample_rate;
        let mut mono = Vec::new();
        device
            .build_output_stream::<T, _, _>(
                *config,
                move |data: &mut [T], _| {
                    mono.resize(data.len() / channels, 0);
                    render(&player, sample_rate, &mut mono);
                    for (frame, &sample) in data.chunks_exact_mut(channels).zip(&mono) {
                        frame.fill(T::from_sample(sample));
                    }
                },
                move |error: cpal::Error| match error.kind() {
                    // The stream keeps playing; a glitch in real-time audio is not worth reopening for.
                    ErrorKind::DeviceChanged | ErrorKind::RealtimeDenied | ErrorKind::Xrun => {
                        log::info!("Speaker: {error}");
                    }
                    _ => failed(format!("The speaker failed: {error}")),
                },
                None,
            )
            .map_err(|error| format!("Cannot open the speaker: {error}"))
    }
}

#[cfg(target_os = "android")]
pub use android::Android;

/// AAudio, at the device's own rate, with a low-latency data callback.
#[cfg(target_os = "android")]
mod android {
    use super::{Failed, Output, Playback, Shared, render};
    use ndk::audio::{
        AudioCallbackResult, AudioContentType, AudioDirection, AudioFormat, AudioPerformanceMode,
        AudioSharingMode, AudioStreamBuilder, AudioUsage,
    };

    pub struct Android;

    impl Playback for Android {
        fn open(&self, player: Shared, failed: Failed) -> Result<Output, String> {
            // Mono 16-bit at the device's rate, rendered straight into AAudio's buffer when it accepts mono.
            let mut spread = Vec::new();
            let stream = AudioStreamBuilder::new()
                .map_err(|error| format!("Cannot open the speaker: {error}"))?
                .direction(AudioDirection::Output)
                .sharing_mode(AudioSharingMode::Shared)
                .performance_mode(AudioPerformanceMode::LowLatency)
                .usage(AudioUsage::Media)
                .content_type(AudioContentType::Speech)
                .format(AudioFormat::PCM_I16)
                .channel_count(1)
                .data_callback(Box::new(move |stream, data, frames| {
                    let channels = stream.channel_count().max(1) as usize;
                    let frames = frames.max(0) as usize;
                    let sample_rate = stream.sample_rate().max(0) as u32;
                    // SAFETY: AAudio hands the callback a buffer of `frames` frames of the stream's format, which was checked to be 16-bit when the stream opened, with `channels` samples each.
                    let output = unsafe {
                        std::slice::from_raw_parts_mut(data.cast::<i16>(), frames * channels)
                    };
                    if channels == 1 {
                        render(&player, sample_rate, output);
                    } else {
                        spread.resize(frames, 0);
                        render(&player, sample_rate, &mut spread);
                        for (frame, &sample) in output.chunks_exact_mut(channels).zip(&spread) {
                            frame.fill(sample);
                        }
                    }
                    AudioCallbackResult::Continue
                }))
                .error_callback(Box::new(move |_, error| {
                    failed(format!("The speaker failed: {error}"))
                }))
                .open_stream()
                .map_err(|error| format!("Cannot open the speaker: {error}"))?;
            if stream.format() != AudioFormat::PCM_I16 {
                return Err("The speaker opened in an unexpected format.".into());
            }
            stream
                .request_start()
                .map_err(|error| format!("Cannot start the speaker: {error}"))?;
            log::info!(
                "Speaker opened at {} Hz, {} channels",
                stream.sample_rate(),
                stream.channel_count()
            );
            Ok(Output::new(Stream(stream)))
        }
    }

    /// Stops the stream before closing it.
    struct Stream(ndk::audio::AudioStream);

    impl Drop for Stream {
        fn drop(&mut self) {
            let _ = self.0.request_stop();
        }
    }
}
