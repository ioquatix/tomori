// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

//! Portable CPU-only recognition; Android capture/lifecycle lives in voice.rs.
use anyhow::{Result, ensure};
#[cfg(test)]
use std::sync::{
    Arc, Once,
    atomic::{AtomicBool, Ordering},
};
#[cfg(test)]
use whisper_rs::{FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters};

pub const SAMPLE_RATE: usize = 16_000;
#[cfg(test)]
pub const MAX_SECONDS: usize = 60;

#[cfg(test)]
pub struct Decoder {
    context: WhisperContext,
}

#[cfg(test)]
impl Decoder {
    pub fn new(model: &[u8]) -> Result<Self> {
        // Native debug builds can log decoded tokens despite print_realtime=false.
        // Discard native token/progress logs; retain only technical errors.
        static LOGGING: Once = Once::new();
        LOGGING.call_once(|| {
            whisper_rs::install_logging_hooks();
            // SAFETY: callback is a static function, ignores user_data, and
            // catches Rust panics so they cannot unwind across the C ABI.
            unsafe {
                whisper_rs::set_log_callback(Some(native_error_log), std::ptr::null_mut());
            }
        });
        let mut parameters = WhisperContextParameters::default();
        parameters.use_gpu(false);
        Ok(Self {
            context: WhisperContext::new_from_buffer_with_params(model, parameters)?,
        })
    }

    pub fn transcribe(&self, samples: &[f32], cancelled: Arc<AtomicBool>) -> Result<String> {
        ensure!(
            samples.len() <= SAMPLE_RATE * MAX_SECONDS,
            "Recording too long"
        );
        // Reject silence and tiny accidental button presses instead of letting
        // Whisper hallucinate a sentence from near-empty audio.
        if samples.len() < SAMPLE_RATE / 3 || rms(samples) < 0.003 {
            return Ok(String::new());
        }
        let mut audio = samples.to_vec();
        audio.resize(audio.len().max(SAMPLE_RATE), 0.0);
        let mut state = self.context.create_state()?;
        let mut parameters = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
        parameters.set_n_threads(3);
        parameters.set_language(Some("en"));
        parameters.set_translate(false);
        parameters.set_no_context(true);
        parameters.set_no_timestamps(true);
        parameters.set_print_special(false);
        parameters.set_print_progress(false);
        parameters.set_print_realtime(false);
        parameters.set_print_timestamps(false);
        parameters.set_suppress_blank(true);
        // Whisper processes long recordings in ~30-second windows. Give each
        // window enough text tokens for a full spoken paragraph.
        parameters.set_max_tokens(256);
        // whisper-rs 0.16's safe closure trampoline casts its boxed trait-object
        // userdata to the wrong type. Use an explicitly typed stable pointer.
        // SAFETY: this Arc retains the AtomicBool until synchronous full()
        // returns. The C callback only loads the atomic, never frees/mutates it.
        unsafe {
            parameters.set_abort_callback(Some(abort_callback));
            parameters.set_abort_callback_user_data(Arc::as_ptr(&cancelled).cast_mut().cast());
        }
        state.full(parameters, &audio)?;
        let mut text = String::new();
        for segment in state.as_iter() {
            text.push_str(&segment.to_str_lossy()?);
        }
        Ok(text.split_whitespace().collect::<Vec<_>>().join(" "))
    }
}

#[cfg(test)]
unsafe extern "C" fn abort_callback(data: *mut std::ffi::c_void) -> bool {
    // SAFETY: transcribe() passes a stable, live Arc<AtomicBool> for this call.
    unsafe { &*data.cast::<AtomicBool>() }.load(Ordering::Relaxed)
}

#[cfg(test)]
unsafe extern "C" fn native_error_log(
    level: std::ffi::c_uint,
    text: *const std::ffi::c_char,
    _: *mut std::ffi::c_void,
) {
    if text.is_null()
        || !matches!(
            whisper_rs::GGMLLogLevel::from(level),
            whisper_rs::GGMLLogLevel::Error
        )
    {
        return;
    }
    let _ = std::panic::catch_unwind(|| {
        // SAFETY: whisper.cpp supplies a valid null-terminated string during
        // the callback. Debug/info/continuation logs (token text) are discarded.
        let text = unsafe { std::ffi::CStr::from_ptr(text) }.to_string_lossy();
        log::error!("Whisper backend: {}", text.trim());
    });
}

#[cfg(test)]
pub fn rms(samples: &[f32]) -> f32 {
    (samples.iter().map(|x| x * x).sum::<f32>() / samples.len().max(1) as f32).sqrt()
}

/// ndk 0.9's AAudio read wrapper incorrectly treats positive frame counts as
/// AudioError::__Unknown(count). Normalize both representations, retaining
/// genuine negative AAudio errors and rejecting counts outside our buffer.
pub fn audio_frame_count(result: std::result::Result<u32, i32>, capacity: usize) -> Result<usize> {
    let count = match result {
        Ok(count) => i64::from(count),
        Err(code) => i64::from(code),
    };
    ensure!(count >= 0, "AAudio read failed (code {count})");
    ensure!(
        count as usize <= capacity,
        "AAudio returned more frames than the input buffer"
    );
    Ok(count as usize)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn silence_detection_is_finite_and_measures_signal() {
        assert_eq!(rms(&[]), 0.0);
        assert_eq!(rms(&[0.0; 200]), 0.0);
        assert!((rms(&[-0.1, 0.1]) - 0.1).abs() < 0.0001);
        assert_eq!(MAX_SECONDS * SAMPLE_RATE, 960_000);
    }

    #[test]
    fn successful_aaudio_reads_are_not_mistaken_for_errors() {
        assert_eq!(audio_frame_count(Err(800), 800).unwrap(), 800);
        assert_eq!(audio_frame_count(Err(17), 800).unwrap(), 17);
        assert_eq!(audio_frame_count(Ok(800), 800).unwrap(), 800);
        assert_eq!(audio_frame_count(Ok(0), 800).unwrap(), 0);
        assert!(audio_frame_count(Err(-899), 800).is_err());
        assert!(audio_frame_count(Err(801), 800).is_err());
    }

    #[test]
    fn abort_callback_reads_the_actual_cancellation_flag() {
        let cancelled = Arc::new(AtomicBool::new(false));
        let ptr = Arc::as_ptr(&cancelled).cast_mut().cast();
        // SAFETY: Arc is held for the duration of these direct callback calls.
        assert!(!unsafe { abort_callback(ptr) });
        cancelled.store(true, Ordering::Relaxed);
        assert!(unsafe { abort_callback(ptr) });
        cancelled.store(false, Ordering::Relaxed);
        assert!(!unsafe { abort_callback(ptr) });
    }

    #[test]
    #[ignore = "requires downloaded voice model and target/downloads/jfk.wav"]
    fn transcribes_real_speech_offline() {
        let model = std::fs::read("android/voice-assets/ggml-base.en-q5_1.bin").unwrap();
        let decoder = Decoder::new(&model).unwrap();
        let reader = hound::WavReader::open("target/downloads/jfk.wav").unwrap();
        assert_eq!(reader.spec().sample_rate, SAMPLE_RATE as u32);
        assert_eq!(reader.spec().channels, 1);
        let samples: Vec<_> = reader
            .into_samples::<i16>()
            .map(|s| s.unwrap() as f32 / 32768.0)
            .collect();
        let text = decoder
            .transcribe(&samples, Arc::new(AtomicBool::new(false)))
            .unwrap();
        assert!(text.to_lowercase().contains("country"), "{text}");
        assert!(
            decoder
                .transcribe(&samples, Arc::new(AtomicBool::new(true)))
                .is_err()
        );
        assert_eq!(
            decoder
                .transcribe(
                    &vec![0.0; SAMPLE_RATE * MAX_SECONDS],
                    Arc::new(AtomicBool::new(false))
                )
                .unwrap(),
            ""
        );
        assert!(
            decoder
                .transcribe(
                    &vec![0.0; SAMPLE_RATE * MAX_SECONDS + 1],
                    Arc::new(AtomicBool::new(false))
                )
                .is_err()
        );
    }

    #[test]
    #[ignore = "requires downloaded voice model and target/downloads/jfk.wav"]
    fn transcribes_recordings_longer_than_one_whisper_window() {
        let model = std::fs::read("android/voice-assets/ggml-base.en-q5_1.bin").unwrap();
        let decoder = Decoder::new(&model).unwrap();
        let reader = hound::WavReader::open("target/downloads/jfk.wav").unwrap();
        let phrase: Vec<_> = reader
            .into_samples::<i16>()
            .map(|s| s.unwrap() as f32 / 32768.0)
            .collect();
        let mut samples = Vec::new();
        for _ in 0..3 {
            samples.extend_from_slice(&phrase);
            samples.resize(samples.len() + SAMPLE_RATE, 0.0);
        }
        assert!(samples.len() > SAMPLE_RATE * 30);
        assert!(samples.len() < SAMPLE_RATE * MAX_SECONDS);
        let text = decoder
            .transcribe(&samples, Arc::new(AtomicBool::new(false)))
            .unwrap();
        assert!(
            text.to_lowercase().matches("country").count() >= 6,
            "Expected speech across multiple windows: {text}"
        );
    }

    #[test]
    #[ignore = "requires both voice models and target/downloads/jfk.wav"]
    fn compare_tiny_and_base_on_same_speech() {
        let reader = hound::WavReader::open("target/downloads/jfk.wav").unwrap();
        assert_eq!(reader.spec().sample_rate, SAMPLE_RATE as u32);
        assert_eq!(reader.spec().channels, 1);
        let samples: Vec<_> = reader
            .into_samples::<i16>()
            .map(|s| s.unwrap() as f32 / 32768.0)
            .collect();
        let reference = "and so my fellow americans ask not what your country can do for you ask what you can do for your country";
        let mut results = Vec::new();
        for (name, path) in [
            ("tiny.en-q5_1", "android/assets/ggml-tiny.en-q5_1.bin"),
            ("base.en-q5_1", "android/voice-assets/ggml-base.en-q5_1.bin"),
        ] {
            let model = std::fs::read(path).unwrap();
            let decoder = Decoder::new(&model).unwrap();
            let start = std::time::Instant::now();
            let text = decoder
                .transcribe(&samples, Arc::new(AtomicBool::new(false)))
                .unwrap();
            let elapsed = start.elapsed();
            let normalized: String = text
                .chars()
                .map(|c| {
                    if c.is_ascii_alphanumeric() {
                        c.to_ascii_lowercase()
                    } else {
                        ' '
                    }
                })
                .collect();
            let expected: Vec<_> = reference.split_whitespace().collect();
            let actual: Vec<_> = normalized.split_whitespace().collect();
            let mut distance: Vec<usize> = (0..=actual.len()).collect();
            for (i, word) in expected.iter().enumerate() {
                let mut diagonal = distance[0];
                distance[0] = i + 1;
                for (j, actual_word) in actual.iter().enumerate() {
                    let old = distance[j + 1];
                    distance[j + 1] = (old + 1)
                        .min(distance[j] + 1)
                        .min(diagonal + usize::from(word != actual_word));
                    diagonal = old;
                }
            }
            println!(
                "{name}: {} word errors out of {}, decode {:.2}s on host",
                distance[actual.len()],
                expected.len(),
                elapsed.as_secs_f32()
            );
            assert!(normalized.contains("country"));
            results.push(distance[actual.len()]);
        }
        // A single example is a smoke test, not proof the bigger model always wins.
        assert_eq!(results.len(), 2);
    }
}
