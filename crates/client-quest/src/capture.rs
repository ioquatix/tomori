// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

//! The platform microphone, opened only while push-to-talk is held. See `design/live-audio.md`.
//!
//! Capture hands mono samples to a callback on its own thread; [`crate::connection::Connection`] passes them to the client's [`client::Microphone`], which frames and sends them.

/// Receives captured mono samples in the range -1 to 1, on the capture thread.
pub type Samples = Box<dyn FnMut(&[f32]) + Send>;

/// Told, from any thread, why capture stopped early.
pub type Failed = Box<dyn Fn(String) + Send + Sync>;

/// Capture in progress, stopped when dropped. Samples already delivered are not lost.
pub struct Recording {
    pub sample_rate: u32,
    _stream: Box<dyn std::any::Any>,
}

impl Recording {
    pub fn new(sample_rate: u32, stream: impl std::any::Any) -> Self {
        Self {
            sample_rate,
            _stream: Box::new(stream),
        }
    }
}

/// A source of microphone audio.
pub trait Capture {
    /// Whether the app may record, asking the participant if it has not yet. A request is answered later, so `Ok(false)` means ask again after they respond.
    fn permit(&mut self) -> Result<bool, String> {
        Ok(true)
    }

    /// Start capturing at a rate between 8 and 48 kHz, which the protocol carries.
    fn open(&mut self, samples: Samples, failed: Failed) -> Result<Recording, String>;
}

/// Average interleaved channels into mono.
#[cfg_attr(target_os = "android", allow(dead_code))]
fn downmix(samples: &[f32], channels: usize, output: &mut Vec<f32>) {
    output.clear();
    if channels <= 1 {
        output.extend_from_slice(samples);
        return;
    }
    output.extend(
        samples
            .chunks_exact(channels)
            .map(|frame| frame.iter().sum::<f32>() / channels as f32),
    );
}

#[cfg(not(target_os = "android"))]
pub use desktop::Desktop;

/// The default input device, through cpal.
#[cfg(not(target_os = "android"))]
mod desktop {
    use super::{Capture, Failed, Recording, Samples, downmix};
    use cpal::{
        ErrorKind, FromSample, SampleFormat, SizedSample, StreamConfig,
        traits::{DeviceTrait, HostTrait, StreamTrait},
    };

    pub struct Desktop;

    /// The protocol's range of sample rates.
    const RATES: std::ops::RangeInclusive<u32> = 8_000..=48_000;

    impl Capture for Desktop {
        fn open(&mut self, samples: Samples, failed: Failed) -> Result<Recording, String> {
            let started = std::time::Instant::now();
            let device = cpal::default_host()
                .default_input_device()
                .ok_or("No microphone is available.")?;
            let supported = device
                .default_input_config()
                .map_err(|error| format!("Cannot use the microphone: {error}"))?;
            let mut config: StreamConfig = supported.config();
            if !RATES.contains(&config.sample_rate) {
                // Ask for the nearest rate the protocol carries; the device refuses it if it cannot.
                config.sample_rate = config.sample_rate.clamp(*RATES.start(), *RATES.end());
            }
            let stream = match supported.sample_format() {
                SampleFormat::F32 => build::<f32>(&device, &config, samples, failed),
                SampleFormat::I16 => build::<i16>(&device, &config, samples, failed),
                SampleFormat::I32 => build::<i32>(&device, &config, samples, failed),
                SampleFormat::U16 => build::<u16>(&device, &config, samples, failed),
                format => {
                    return Err(format!(
                        "The microphone's {format} samples are not supported."
                    ));
                }
            }?;
            stream
                .play()
                .map_err(|error| format!("Cannot start the microphone: {error}"))?;
            log::info!(
                "Microphone opened at {} Hz, {} channels, in {:?}",
                config.sample_rate,
                config.channels,
                started.elapsed()
            );
            Ok(Recording::new(config.sample_rate, stream))
        }
    }

    fn build<T>(
        device: &cpal::Device,
        config: &StreamConfig,
        mut samples: Samples,
        failed: Failed,
    ) -> Result<cpal::Stream, String>
    where
        T: SizedSample,
        f32: FromSample<T>,
    {
        let channels = usize::from(config.channels);
        let (mut converted, mut mono) = (Vec::new(), Vec::new());
        device
            .build_input_stream::<T, _, _>(
                *config,
                move |data: &[T], _| {
                    converted.clear();
                    converted.extend(data.iter().map(|sample| sample.to_sample::<f32>()));
                    downmix(&converted, channels, &mut mono);
                    samples(&mono);
                },
                move |error: cpal::Error| match error.kind() {
                    // The stream keeps capturing without a gap.
                    ErrorKind::DeviceChanged | ErrorKind::RealtimeDenied => {
                        log::info!("Microphone: {error}");
                    }
                    // Anything else, including an overrun, loses audio, which must not be sent as if whole.
                    _ => failed(format!("The microphone failed: {error}")),
                },
                None,
            )
            .map_err(|error| format!("Cannot open the microphone: {error}"))
    }
}

#[cfg(target_os = "android")]
pub use android::Android;

/// AAudio, at the 16 kHz the recognizer uses, after asking for `RECORD_AUDIO`.
#[cfg(target_os = "android")]
mod android {
    use super::{Capture, Failed, Recording, Samples};
    use ndk::audio::{
        AudioDirection, AudioFormat, AudioInputPreset, AudioSharingMode, AudioStreamBuilder,
    };
    use std::{
        sync::{
            Arc,
            atomic::{AtomicBool, Ordering},
        },
        thread::JoinHandle,
    };

    const SAMPLE_RATE: u32 = 16_000;

    pub struct Android {
        android: Arc<crate::android::Android>,
        requested: bool,
    }

    impl Android {
        pub fn new(android: Arc<crate::android::Android>) -> Self {
            Self {
                android,
                requested: false,
            }
        }
    }

    impl Capture for Android {
        /// Asks once per launch; after that, the participant must allow it in Settings, since Android may not ask again.
        fn permit(&mut self) -> Result<bool, String> {
            let asked = self.requested;
            let granted = self
                .android
                .check_or_request("android.permission.RECORD_AUDIO", &mut self.requested)
                .map_err(|error| format!("Cannot ask for the microphone: {error:#}"))?;
            if !granted && asked {
                return Err("Allow the microphone for Tomori in Settings.".into());
            }
            Ok(granted)
        }

        /// The stream opens on the capture thread, so that the frame does not wait for it; failing to open is reported through `failed`.
        fn open(&mut self, samples: Samples, failed: Failed) -> Result<Recording, String> {
            let stop = Arc::new(AtomicBool::new(false));
            let thread_stop = stop.clone();
            let thread = std::thread::Builder::new()
                .name("tomori-microphone".into())
                .spawn(move || {
                    if let Err(error) = capture(samples, &thread_stop) {
                        failed(error);
                    }
                })
                .map_err(|error| format!("Cannot start the microphone thread: {error}"))?;
            Ok(Recording::new(
                SAMPLE_RATE,
                Worker {
                    stop,
                    thread: Some(thread),
                },
            ))
        }
    }

    /// Stops and joins the capture thread when dropped, so that every captured sample is delivered first.
    struct Worker {
        stop: Arc<AtomicBool>,
        thread: Option<JoinHandle<()>>,
    }

    impl Drop for Worker {
        fn drop(&mut self) {
            self.stop.store(true, Ordering::Relaxed);
            if let Some(thread) = self.thread.take() {
                let _ = thread.join();
            }
        }
    }

    fn capture(mut samples: Samples, stop: &AtomicBool) -> Result<(), String> {
        // Shared mode supports Android's resampler, so ask for the recognizer's 16 kHz mono directly.
        let stream = AudioStreamBuilder::new()
            .and_then(|builder| {
                builder
                    .direction(AudioDirection::Input)
                    .sharing_mode(AudioSharingMode::Shared)
                    .channel_count(1)
                    .sample_rate(SAMPLE_RATE as i32)
                    .format(AudioFormat::PCM_Float)
                    .input_preset(AudioInputPreset::VoiceRecognition)
                    .open_stream()
            })
            .map_err(|error| format!("Cannot open the microphone: {error}"))?;
        if stream.sample_rate() != SAMPLE_RATE as i32
            || stream.channel_count() != 1
            || stream.format() != AudioFormat::PCM_Float
        {
            return Err("The microphone opened in an unexpected format.".into());
        }
        stream
            .request_start()
            .map_err(|error| format!("Cannot start the microphone: {error}"))?;
        log::info!("Microphone opened (16 kHz mono)");
        // 20 ms reads, one outgoing frame each.
        let mut buffer = [0.0_f32; (SAMPLE_RATE / 50) as usize];
        let mut result = Ok(());
        while result.is_ok() && !stop.load(Ordering::Relaxed) {
            // SAFETY: `buffer` holds `buffer.len()` samples of the negotiated mono float format, and only this thread uses the stream.
            let read = unsafe {
                stream.read(buffer.as_mut_ptr().cast(), buffer.len() as i32, 100_000_000)
            };
            result = frame_count(read.map_err(i32::from), buffer.len())
                .map(|count| samples(&buffer[..count]));
        }
        let _ = stream.request_stop();
        log::info!("Microphone closed");
        result
    }

    /// The samples read. ndk 0.9 returns a positive count from `AAudioStream_read` as an error code, so only negative values are errors.
    fn frame_count(result: Result<u32, i32>, capacity: usize) -> Result<usize, String> {
        let count = match result {
            Ok(count) => i64::from(count),
            Err(code) => i64::from(code),
        };
        if count < 0 {
            return Err(format!("The microphone failed (AAudio error {count})."));
        }
        if count as usize > capacity {
            return Err("The microphone returned more samples than asked for.".into());
        }
        Ok(count as usize)
    }
}

#[cfg(test)]
mod tests {
    use super::downmix;

    #[test]
    fn downmixes_interleaved_channels() {
        let mut mono = Vec::new();
        downmix(&[1.0, 0.0, 0.5, 0.5, -1.0, -0.5], 2, &mut mono);
        assert_eq!(mono, [0.5, 0.5, -0.75]);
        downmix(&[0.25, 0.75], 1, &mut mono);
        assert_eq!(mono, [0.25, 0.75]);
    }
}
