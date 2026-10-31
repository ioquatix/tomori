// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

use super::*;
use crate::playback::CEILING;

const RATE: u32 = 1_000;

fn mixer() -> Mixer {
    Mixer::new(MixerOptions {
        sample_rate: RATE,
        jitter: JitterOptions {
            target_latency: Duration::from_millis(4),
            latency_limit: Duration::from_millis(40),
        },
    })
}

fn frame(source: u32, sequence: u32, value: i16) -> AudioFrame {
    AudioFrame {
        gate: Gate::Live,
        source,
        sequence,
        timestamp: 0,
        sample_rate: RATE,
        samples: vec![value; 4],
    }
}

fn mix(mixer: &mut Mixer, length: usize) -> Vec<i16> {
    let mut output = vec![0; length];
    mixer.mix(&mut output);
    output
}

#[test]
fn defaults_suit_a_headset() {
    let options = MixerOptions::default();
    assert_eq!(options.sample_rate, 48_000);
    assert!(options.jitter.target_latency < options.jitter.latency_limit);
    let mut mixer = Mixer::new(options);
    assert_eq!(mixer.agent_volume(), 1.0);
    assert!(!mixer.is_agent_muted());
    assert_eq!(mix(&mut mixer, 480), vec![0; 480]);
}

#[test]
fn sums_participants_and_agent_speech() {
    let mut mixer = mixer();
    mixer.push(frame(1, 0, 100));
    mixer.push(frame(2, 0, 20));
    mixer.push(frame(AGENT_SOURCE, 0, 3));
    assert_eq!(mix(&mut mixer, 4), [123; 4]);
    assert_eq!(mixer.sample_rate(), RATE);
}

#[test]
fn excludes_the_local_microphone_and_gated_off_frames() {
    let mut mixer = mixer();
    mixer.push(frame(5, 0, 1));
    mixer.set_local_source(Some(5));
    assert_eq!(mixer.sources().count(), 0);
    mixer.push(frame(5, 1, 1000));
    let mut off = frame(6, 0, 1000);
    off.gate = Gate::Off;
    mixer.push(off);
    mixer.push(frame(7, 0, 2));
    assert_eq!(mix(&mut mixer, 4), [2; 4]);
    mixer.set_local_source(None);
    mixer.push(frame(5, 2, 9));
    assert_eq!(mixer.sources().count(), 2);
}

#[test]
fn agent_volume_and_mute_are_local_and_only_affect_agent_speech() {
    let mut mixer = mixer();
    mixer.set_agent_volume(0.5);
    assert_eq!(mixer.agent_volume(), 0.5);
    mixer.push(frame(AGENT_SOURCE, 0, 100));
    mixer.push(frame(1, 0, 10));
    assert_eq!(mix(&mut mixer, 4), [60; 4]);

    mixer.set_agent_muted(true);
    assert!(mixer.is_agent_muted());
    mixer.push(frame(AGENT_SOURCE, 1, 100));
    mixer.push(frame(AGENT_SOURCE, 2, 200));
    mixer.push(frame(1, 1, 10));
    assert_eq!(mix(&mut mixer, 4), [10; 4]);

    // Muted speech was consumed in real time, so unmuting resumes with the current frame rather than replaying.
    mixer.set_agent_muted(false);
    mixer.push(frame(1, 2, 10));
    assert_eq!(mix(&mut mixer, 4), [110; 4]);

    mixer.set_agent_volume(-1.0);
    assert_eq!(mixer.agent_volume(), 0.0);
}

#[test]
fn limits_loud_mixes_instead_of_wrapping() {
    let mut mixer = mixer();
    for source in 0..4 {
        mixer.push(frame(source, 0, i16::MAX));
    }
    let output = mix(&mut mixer, 4);
    for sample in output {
        assert!(sample > 0 && f32::from(sample) <= CEILING);
    }
}

#[test]
fn one_late_source_does_not_delay_another() {
    let mut mixer = mixer();
    mixer.push(frame(1, 0, 1));
    // Source 2 has nothing buffered yet; source 1 plays regardless.
    assert_eq!(mix(&mut mixer, 4), [1; 4]);
    mixer.push(frame(2, 0, 2));
    mixer.push(frame(1, 1, 1));
    assert_eq!(mix(&mut mixer, 4), [3; 4]);
}

#[test]
fn restarts_a_source_that_changes_sample_rate() {
    let mut mixer = mixer();
    mixer.push(frame(AGENT_SOURCE, 0, 1));
    let mut faster = frame(AGENT_SOURCE, 1, 4);
    faster.sample_rate = RATE * 2;
    faster.samples = vec![4; 8];
    mixer.push(faster);
    assert_eq!(mix(&mut mixer, 4), [4; 4]);
}

#[test]
fn releases_idle_sources_and_clears() {
    let mut mixer = mixer();
    mixer.push(frame(1, 0, 1));
    mix(&mut mixer, 4);
    assert_eq!(mixer.sources().count(), 1);
    // Ten seconds at 1 kHz.
    mix(&mut mixer, 10_000);
    assert_eq!(mixer.sources().count(), 0);
    assert_eq!(mixer.statistics().underruns, 1);

    mixer.push(frame(1, 1, 1));
    mixer.clear();
    assert_eq!(mixer.sources().count(), 0);

    mixer.push(frame(1, 1, 1));
    mixer.push(frame(2, 1, 1));
    mixer.clear_source(1);
    assert_eq!(mixer.sources().collect::<Vec<_>>(), [2]);
}

#[test]
fn plays_a_source_that_restarts_its_sequence() {
    let mut mixer = mixer();
    for sequence in 500..510 {
        mixer.push(frame(1, sequence, 1));
        mix(&mut mixer, 4);
    }
    for sequence in 0..3 {
        mixer.push(frame(1, sequence, 2));
    }
    assert_eq!(mix(&mut mixer, 12), [2; 12]);
    assert_eq!(mixer.statistics().late_frames, 0);
}

#[test]
fn reports_whether_each_source_is_playing() {
    let mut mixer = mixer();
    assert!(!mixer.is_source_playing(1));
    mixer.push(frame(1, 0, 100));
    mixer.push(frame(1, 1, 100));
    assert!(!mixer.is_source_playing(1), "buffering is not playing");
    mix(&mut mixer, 4);
    assert!(mixer.is_source_playing(1));
    assert!(!mixer.is_source_playing(2));
    // Running out of audio returns the source to buffering.
    mix(&mut mixer, 8);
    assert!(!mixer.is_source_playing(1));
}

#[test]
fn measures_the_level_each_source_played() {
    let mut mixer = mixer();
    assert_eq!(mixer.source_level(1), 0.0);
    let loud = i16::MAX / 2;
    for sequence in 0..4 {
        mixer.push(frame(1, sequence, loud));
        mixer.push(frame(2, sequence, loud / 10));
    }
    mix(&mut mixer, 4);
    let first = mixer.source_level(1);
    let second = mixer.source_level(2);
    assert!((first - 0.5).abs() < 0.01, "{first}");
    assert!((second - 0.05).abs() < 0.01, "{second}");
    assert_eq!(mixer.source_level(3), 0.0);
}

#[test]
fn levels_fall_gradually_when_a_source_goes_quiet() {
    let mut mixer = mixer();
    mixer.push(frame(1, 0, i16::MAX / 2));
    mixer.push(frame(1, 1, i16::MAX / 2));
    mix(&mut mixer, 8);
    let playing = mixer.source_level(1);
    // 30 ms of silence at 1 kHz, less than the release time.
    mix(&mut mixer, 30);
    let falling = mixer.source_level(1);
    assert!(
        falling > 0.0 && falling < playing,
        "{falling} after {playing}"
    );
    // Long after, the level is close to zero.
    mix(&mut mixer, 1_000);
    assert!(mixer.source_level(1) < 0.001);
}

#[test]
fn muted_agent_speech_plays_silently() {
    let mut mixer = mixer();
    mixer.set_agent_muted(true);
    mixer.push(frame(AGENT_SOURCE, 0, 1_000));
    mixer.push(frame(AGENT_SOURCE, 1, 1_000));
    mix(&mut mixer, 4);
    assert!(mixer.is_source_playing(AGENT_SOURCE));
    assert_eq!(mixer.source_level(AGENT_SOURCE), 0.0);
}

#[test]
fn clearing_forgets_levels() {
    let mut mixer = mixer();
    mixer.push(frame(1, 0, 1_000));
    mixer.push(frame(2, 0, 1_000));
    mix(&mut mixer, 4);
    mixer.clear_source(1);
    assert_eq!(mixer.source_level(1), 0.0);
    assert!(mixer.source_level(2) > 0.0);
    mixer.clear();
    assert_eq!(mixer.source_level(2), 0.0);
    assert!(!mixer.is_source_playing(2));
}
