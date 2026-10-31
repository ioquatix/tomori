// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

use std::sync::mpsc::SyncSender;

use protocol::{AGENT_SOURCE, Gate};

use super::*;
use crate::playback::JitterOptions;

const RATE: u32 = 1_000;
/// Samples per frame: 4 ms at [`RATE`].
const FRAME: usize = 4;

fn player() -> (SyncSender<AudioFrame>, Player, PlayerControl) {
    let (sender, audio) = mpsc::sync_channel(64);
    let (player, control) = Player::new(
        audio,
        MixerOptions {
            sample_rate: 48_000,
            jitter: JitterOptions {
                target_latency: Duration::from_millis(4),
                latency_limit: Duration::from_millis(200),
            },
        },
    );
    (sender, player, control)
}

fn frame(source: u32, sequence: u32, value: i16) -> AudioFrame {
    AudioFrame {
        gate: Gate::Live,
        source,
        sequence,
        timestamp: 0,
        sample_rate: RATE,
        samples: vec![value; FRAME],
    }
}

fn render(player: &mut Player, length: usize) -> Vec<i16> {
    let mut output = vec![1; length];
    player.render(RATE, &mut output);
    output
}

#[test]
fn plays_received_participants_and_agent_speech_together() {
    let (sender, mut player, control) = player();
    assert_eq!(render(&mut player, FRAME), [0; FRAME]);
    sender.send(frame(1, 0, 100)).unwrap();
    sender.send(frame(AGENT_SOURCE, 0, 20)).unwrap();
    assert_eq!(render(&mut player, FRAME), [120; FRAME]);
    assert_eq!(control.statistics(), PlaybackStatistics::default());
}

#[test]
fn never_plays_the_local_participant() {
    let (sender, mut player, control) = player();
    control.set_local_source(Some(5));
    sender.send(frame(5, 0, 1000)).unwrap();
    sender.send(frame(AGENT_SOURCE, 0, 7)).unwrap();
    assert_eq!(render(&mut player, FRAME), [7; FRAME]);

    // The local source survives a change of device rate.
    sender
        .send(AudioFrame {
            sample_rate: 2 * RATE,
            ..frame(5, 1, 1000)
        })
        .unwrap();
    let mut output = [1; FRAME];
    player.render(2 * RATE, &mut output);
    assert_eq!(output, [0; FRAME]);
}

#[test]
fn clearing_discards_buffered_audio() {
    let (sender, mut player, control) = player();
    for sequence in 0..4 {
        sender.send(frame(1, sequence, 50)).unwrap();
    }
    assert_eq!(render(&mut player, FRAME), [50; FRAME]);
    control.clear();
    assert_eq!(render(&mut player, FRAME), [0; FRAME]);
}

#[test]
fn silencing_fades_out_while_consuming_audio_in_real_time_then_fades_back_in() {
    let (sender, mut player, control) = player();
    for sequence in 0..20 {
        sender.send(frame(AGENT_SOURCE, sequence, 1000)).unwrap();
    }
    assert_eq!(render(&mut player, FRAME), [1000; FRAME]);

    control.set_silenced(true);
    assert!(control.is_silenced());
    // At 1 kHz the 10 ms fade takes ten samples, getting quieter each one.
    let faded = render(&mut player, 12);
    assert_eq!(
        faded,
        [900, 800, 700, 600, 500, 400, 300, 200, 100, 0, 0, 0]
    );
    assert_eq!(render(&mut player, 24), [0; 24]);

    // The 36 silenced samples were consumed rather than paused, so ten of the 21 frames have gone.
    sender.send(frame(AGENT_SOURCE, 20, 1000)).unwrap();
    control.set_silenced(false);
    let resumed = render(&mut player, 12);
    assert_eq!(
        resumed,
        [
            100, 200, 300, 400, 500, 600, 700, 800, 900, 1000, 1000, 1000
        ]
    );
    // Three more frames have gone, leaving eight.
    assert_eq!(render(&mut player, 32), [1000; 32]);
    assert_eq!(render(&mut player, FRAME), [0; FRAME]);
}

#[test]
fn an_unknown_device_rate_renders_silence() {
    let (sender, mut player, _control) = player();
    sender.send(frame(1, 0, 100)).unwrap();
    let mut output = [1; FRAME];
    player.render(0, &mut output);
    assert_eq!(output, [0; FRAME]);
}
