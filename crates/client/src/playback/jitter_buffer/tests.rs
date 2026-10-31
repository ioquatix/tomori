// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

use super::*;
use protocol::Gate;

const RATE: u32 = 1_000;

// Four-sample frames at 1 kHz: the target is two frames and the limit five.
fn options() -> JitterOptions {
    JitterOptions {
        target_latency: Duration::from_millis(8),
        latency_limit: Duration::from_millis(20),
    }
}

fn frame(sequence: u32, value: i16) -> AudioFrame {
    AudioFrame {
        gate: Gate::Live,
        source: 1,
        sequence,
        timestamp: 0,
        sample_rate: RATE,
        samples: vec![value; 4],
    }
}

fn pull(buffer: &mut JitterBuffer, length: usize, statistics: &mut PlaybackStatistics) -> Vec<f32> {
    let mut output = vec![0.0; length];
    buffer.mix_into(&mut output, RATE, 1.0, statistics);
    output
}

#[test]
fn buffers_to_the_target_before_playing() {
    let mut statistics = PlaybackStatistics::default();
    let mut buffer = JitterBuffer::new(RATE, options());
    buffer.push(frame(0, 1), &mut statistics);
    assert_eq!(pull(&mut buffer, 2, &mut statistics), [0.0; 2]);
    assert!(!buffer.is_playing());

    buffer.push(frame(1, 2), &mut statistics);
    assert_eq!(
        pull(&mut buffer, 8, &mut statistics),
        [1.0, 1.0, 1.0, 1.0, 2.0, 2.0, 2.0, 2.0]
    );
    assert!(buffer.is_playing());
    assert_eq!(statistics, PlaybackStatistics::default());
}

#[test]
fn reorders_frames_by_sequence() {
    let mut statistics = PlaybackStatistics::default();
    let mut buffer = JitterBuffer::new(RATE, options());
    buffer.push(frame(11, 2), &mut statistics);
    buffer.push(frame(10, 1), &mut statistics);
    buffer.push(frame(12, 3), &mut statistics);
    let output = pull(&mut buffer, 12, &mut statistics);
    assert_eq!(&output[0..4], [1.0; 4]);
    assert_eq!(&output[4..8], [2.0; 4]);
    assert_eq!(&output[8..12], [3.0; 4]);
}

#[test]
fn conceals_a_missing_frame_and_drops_it_when_late() {
    let mut statistics = PlaybackStatistics::default();
    let mut buffer = JitterBuffer::new(RATE, options());
    buffer.push(frame(0, 1), &mut statistics);
    buffer.push(frame(2, 3), &mut statistics);
    let output = pull(&mut buffer, 12, &mut statistics);
    assert_eq!(&output[0..4], [1.0; 4]);
    assert_eq!(&output[4..8], [0.0; 4]);
    assert_eq!(&output[8..12], [3.0; 4]);
    assert_eq!(statistics.concealed_frames, 1);

    buffer.push(frame(1, 2), &mut statistics);
    assert_eq!(statistics.late_frames, 1);
    assert_eq!(buffer.buffered_samples(), 0);
}

#[test]
fn skips_gaps_too_long_to_conceal() {
    let mut statistics = PlaybackStatistics::default();
    let mut buffer = JitterBuffer::new(RATE, options());
    buffer.push(frame(0, 1), &mut statistics);
    buffer.push(frame(9, 2), &mut statistics);
    let output = pull(&mut buffer, 8, &mut statistics);
    assert_eq!(&output[0..4], [1.0; 4]);
    assert_eq!(&output[4..8], [2.0; 4]);
    assert_eq!(statistics.skipped_frames, 8);
    assert_eq!(statistics.concealed_frames, 0);
}

#[test]
fn underruns_play_silence_and_rebuffer() {
    let mut statistics = PlaybackStatistics::default();
    let mut buffer = JitterBuffer::new(RATE, options());
    buffer.push(frame(0, 1), &mut statistics);
    buffer.push(frame(1, 2), &mut statistics);
    let output = pull(&mut buffer, 10, &mut statistics);
    assert_eq!(&output[8..10], [0.0; 2]);
    assert_eq!(statistics.underruns, 1);
    assert!(!buffer.is_playing());

    // One frame is below the target, so the source keeps buffering rather than stuttering.
    buffer.push(frame(2, 3), &mut statistics);
    assert_eq!(pull(&mut buffer, 4, &mut statistics), [0.0; 4]);
    buffer.push(frame(3, 4), &mut statistics);
    assert_eq!(&pull(&mut buffer, 4, &mut statistics), &[3.0; 4]);
}

#[test]
fn resumes_from_the_oldest_buffered_frame_after_silence() {
    let mut statistics = PlaybackStatistics::default();
    let mut buffer = JitterBuffer::new(RATE, options());
    buffer.push(frame(0, 1), &mut statistics);
    buffer.push(frame(1, 1), &mut statistics);
    pull(&mut buffer, 12, &mut statistics);
    // Frames 2 and 3 were dropped by the sender while the source was silent.
    buffer.push(frame(4, 5), &mut statistics);
    buffer.push(frame(5, 6), &mut statistics);
    let output = pull(&mut buffer, 8, &mut statistics);
    assert_eq!(&output[0..4], [5.0; 4]);
    assert_eq!(statistics.concealed_frames, 0);
}

#[test]
fn plays_a_short_tail_once_the_source_stops_sending() {
    let mut statistics = PlaybackStatistics::default();
    let mut buffer = JitterBuffer::new(RATE, options());
    buffer.push(frame(0, 7), &mut statistics);
    // Below the target: silence until the source has been idle for the target latency.
    assert_eq!(pull(&mut buffer, 4, &mut statistics), [0.0; 4]);
    assert_eq!(pull(&mut buffer, 4, &mut statistics), [7.0; 4]);
}

#[test]
fn discards_the_oldest_audio_beyond_the_latency_limit() {
    let mut statistics = PlaybackStatistics::default();
    let mut buffer = JitterBuffer::new(RATE, options());
    for sequence in 0..6 {
        buffer.push(frame(sequence, sequence as i16), &mut statistics);
    }
    assert_eq!(statistics.discarded_frames, 4);
    assert_eq!(buffer.buffered(), Duration::from_millis(8));
    let output = pull(&mut buffer, 8, &mut statistics);
    assert_eq!(&output[0..4], [4.0; 4]);

    // Frames older than the discarded ones are late.
    buffer.push(frame(1, 1), &mut statistics);
    assert_eq!(statistics.late_frames, 1);
}

#[test]
fn trimming_drops_partially_played_audio() {
    let mut statistics = PlaybackStatistics::default();
    let mut buffer = JitterBuffer::new(RATE, options());
    buffer.push(frame(0, 1), &mut statistics);
    buffer.push(frame(1, 2), &mut statistics);
    pull(&mut buffer, 2, &mut statistics);
    for sequence in 2..6 {
        buffer.push(frame(sequence, sequence as i16), &mut statistics);
    }
    assert_eq!(statistics.discarded_frames, 3);
    assert_eq!(buffer.buffered(), Duration::from_millis(8));
    assert_eq!(&pull(&mut buffer, 4, &mut statistics), &[4.0; 4]);
}

#[test]
fn rejects_duplicates() {
    let mut statistics = PlaybackStatistics::default();
    let mut buffer = JitterBuffer::new(RATE, options());
    buffer.push(frame(0, 1), &mut statistics);
    buffer.push(frame(0, 1), &mut statistics);
    assert_eq!(statistics.late_frames, 1);
    assert_eq!(buffer.buffered_samples(), 4);
}

#[test]
fn orders_across_sequence_wraparound() {
    let mut statistics = PlaybackStatistics::default();
    let mut buffer = JitterBuffer::new(RATE, options());
    buffer.push(frame(0, 3), &mut statistics);
    buffer.push(frame(u32::MAX, 2), &mut statistics);
    buffer.push(frame(u32::MAX - 1, 1), &mut statistics);
    let output = pull(&mut buffer, 12, &mut statistics);
    assert_eq!(&output[0..4], [1.0; 4]);
    assert_eq!(&output[4..8], [2.0; 4]);
    assert_eq!(&output[8..12], [3.0; 4]);
    assert_eq!(statistics, PlaybackStatistics::default());
}

#[test]
fn resamples_to_the_device_rate() {
    let mut statistics = PlaybackStatistics::default();
    let options = JitterOptions {
        target_latency: Duration::from_millis(20),
        latency_limit: Duration::from_millis(100),
    };
    let mut buffer = JitterBuffer::new(16_000, options);
    for sequence in 0..3 {
        let start = sequence as i16 * 320;
        buffer.push(
            AudioFrame {
                gate: Gate::Live,
                source: 1,
                sequence,
                timestamp: 0,
                sample_rate: 16_000,
                samples: (start..start + 320).collect(),
            },
            &mut statistics,
        );
    }
    // 20 ms at 48 kHz is 960 samples, interpolating three output samples per input sample.
    let mut output = vec![0.0; 960];
    buffer.mix_into(&mut output, 48_000, 1.0, &mut statistics);
    for (index, sample) in output.iter().enumerate() {
        let expected = index as f32 / 3.0;
        assert!(
            (sample - expected).abs() < 0.01,
            "sample {index}: {sample} != {expected}"
        );
    }
    assert_eq!(statistics, PlaybackStatistics::default());
}

#[test]
fn applies_gain() {
    let mut statistics = PlaybackStatistics::default();
    let mut buffer = JitterBuffer::new(RATE, options());
    buffer.push(frame(0, 100), &mut statistics);
    buffer.push(frame(1, 100), &mut statistics);
    let mut output = vec![1.0; 4];
    buffer.mix_into(&mut output, RATE, 0.5, &mut statistics);
    assert_eq!(output, [51.0; 4]);
}
