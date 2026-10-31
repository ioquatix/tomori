// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

//! Shared AAudio capture helpers; Android capture and lifecycle live in voice.rs.
use anyhow::{Result, ensure};

#[cfg_attr(not(target_os = "android"), allow(dead_code))]
pub const SAMPLE_RATE: usize = 16_000;

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
    fn successful_aaudio_reads_are_not_mistaken_for_errors() {
        assert_eq!(audio_frame_count(Err(800), 800).unwrap(), 800);
        assert_eq!(audio_frame_count(Err(17), 800).unwrap(), 17);
        assert_eq!(audio_frame_count(Ok(800), 800).unwrap(), 800);
        assert_eq!(audio_frame_count(Ok(0), 800).unwrap(), 0);
        assert!(audio_frame_count(Err(-899), 800).is_err());
        assert!(audio_frame_count(Err(801), 800).is_err());
    }
}
