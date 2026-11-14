// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

use std::sync::mpsc::SyncSender;

use tomori_protocol::{AGENT_SOURCE, Gate, REPLAY_SOURCE};

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
    player.render(RATE, Layout::Mono, &mut output);
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
fn a_replay_stops_being_reported_once_it_has_been_quiet_for_a_pause() {
    let (sender, mut player, control) = player();
    sender.send(frame(REPLAY_SOURCE, 0, 7)).unwrap();
    render(&mut player, FRAME);
    assert!(control.is_replaying());
    // A breath between its segments is still the replay.
    render(&mut player, 400);
    assert!(control.is_replaying());
    // Past the pause it has ended, so the pill's `Replaying` clears.
    render(&mut player, 400);
    assert!(!control.is_replaying());
}

/// Render `samples` at twice the speech's rate, as 24 kHz speech plays on a 48 kHz headset, so that a stream that runs dry ends part way between two of its samples.
fn render_resampled(player: &mut Player, samples: usize) {
    let mut output = vec![0; samples];
    player.render(RATE * 2, Layout::Mono, &mut output);
}

/// More than [`REPLAY_PAUSE`] at the resampled rate.
const PAST_THE_PAUSE: usize = 2 * 800;

#[test]
fn a_live_reply_heard_for_the_first_time_is_not_replaying() {
    let (sender, mut player, control) = player();
    sender.send(frame(AGENT_SOURCE, 0, 7)).unwrap();
    for sequence in 1..5 {
        sender.send(frame(AGENT_SOURCE, sequence, 7)).unwrap();
        render_resampled(&mut player, FRAME * 2);
        assert!(!control.is_replaying());
        assert!(control.agent_speech().0);
    }
    render_resampled(&mut player, PAST_THE_PAUSE);
    assert!(!control.is_replaying());
}

#[test]
fn a_catch_up_is_replaying_until_it_ends_and_the_live_reply_after_it_is_not() {
    let (sender, mut player, control) = player();
    // Joining or returning, the latest reply is played again from its start; the pill says so throughout, breaths included.
    for sequence in 0..4 {
        sender.send(frame(REPLAY_SOURCE, sequence, 7)).unwrap();
        render_resampled(&mut player, FRAME * 2);
        assert!(control.is_replaying());
    }
    render_resampled(&mut player, 400);
    assert!(control.is_replaying());
    // Caught up, the next reply is heard live, for the first time.
    render_resampled(&mut player, PAST_THE_PAUSE);
    assert!(!control.is_replaying());
    for sequence in 0..4 {
        sender.send(frame(AGENT_SOURCE, sequence, 7)).unwrap();
        render_resampled(&mut player, FRAME * 2);
        assert!(!control.is_replaying());
    }
}

#[test]
fn a_replay_followed_by_a_live_reply_stops_replaying_once_it_has_played_out() {
    let (sender, mut player, control) = player();
    for sequence in 0..4 {
        sender.send(frame(REPLAY_SOURCE, sequence, 7)).unwrap();
    }
    render_resampled(&mut player, FRAME * 2);
    assert!(control.is_replaying());
    // A later reply starts live, which stops the replay at the server: what was sent of it plays out, and then it has ended.
    for sequence in 0..200 {
        sender.send(frame(AGENT_SOURCE, sequence, 20)).unwrap();
        render_resampled(&mut player, FRAME * 2);
    }
    assert!(control.agent_speech().0);
    assert!(!control.is_replaying());
}

#[test]
fn stopping_a_replay_stops_replaying_once_what_was_sent_has_played() {
    let (sender, mut player, control) = player();
    for sequence in 0..2 {
        sender.send(frame(REPLAY_SOURCE, sequence, 7)).unwrap();
    }
    render_resampled(&mut player, FRAME * 2);
    assert!(control.is_replaying());
    // Stopped, the server sends no more of it, and the participant follows live speech, or silence; what was sent fades out here at once.
    control.stop_replay();
    let mut output = vec![0; FRAME * 2];
    player.render(RATE * 2, Layout::Mono, &mut output);
    assert!(
        output[0] > 0 && output[FRAME * 2 - 1] < output[0],
        "{output:?}"
    );
    render_resampled(&mut player, PAST_THE_PAUSE);
    assert!(!control.is_replaying());
    // So the right stick asks for a replay again, and the next one is reported in turn.
    sender.send(frame(REPLAY_SOURCE, 2, 7)).unwrap();
    render_resampled(&mut player, FRAME * 2);
    assert!(control.is_replaying());
}

#[test]
fn reconnecting_during_a_replay_stops_replaying_at_once() {
    let (sender, mut player, control) = player();
    for sequence in 0..4 {
        sender.send(frame(REPLAY_SOURCE, sequence, 7)).unwrap();
    }
    render_resampled(&mut player, FRAME * 2);
    assert!(control.is_replaying());
    // A new connection clears what was buffered; the reply it resumes is live, on the agent's own source.
    control.clear();
    render_resampled(&mut player, FRAME * 2);
    assert!(!control.is_replaying());
    sender.send(frame(AGENT_SOURCE, 0, 7)).unwrap();
    sender.send(frame(AGENT_SOURCE, 1, 7)).unwrap();
    render_resampled(&mut player, FRAME * 2);
    assert!(!control.is_replaying());
    assert!(control.agent_speech().0);
}

#[test]
fn agent_speech_is_reported_while_it_plays_live_or_again_even_muted() {
    let (sender, mut player, control) = player();
    assert_eq!(control.agent_speech(), (false, 0.0));
    // A participant is not the agent.
    sender.send(frame(1, 0, 1000)).unwrap();
    render(&mut player, FRAME);
    assert_eq!(control.agent_speech(), (false, 0.0));
    // Muted here, the agent still speaks to the others, so it plays, with its level.
    control.set_agent_muted(true);
    sender.send(frame(AGENT_SOURCE, 0, 1000)).unwrap();
    assert_eq!(render(&mut player, FRAME), [0; FRAME]);
    let (playing, level) = control.agent_speech();
    assert!(playing);
    assert!(level > 0.0);
    // Once its buffer runs dry, it stops at once, its level falling.
    render(&mut player, FRAME);
    let (playing, falling) = control.agent_speech();
    assert!(!playing);
    assert!(falling < level);
    // Catch-up, played again, is the agent speaking too.
    sender.send(frame(REPLAY_SOURCE, 0, 1000)).unwrap();
    render(&mut player, FRAME);
    assert!(control.agent_speech().0);
}

#[test]
fn the_others_voices_are_reported_while_they_play_but_never_the_agent_s() {
    let (sender, mut player, control) = player();
    assert_eq!(control.voices().as_slice(), [] as [u32; 0]);
    sender.send(frame(2, 0, 100)).unwrap();
    sender.send(frame(5, 0, 100)).unwrap();
    sender.send(frame(AGENT_SOURCE, 0, 100)).unwrap();
    sender.send(frame(REPLAY_SOURCE, 0, 100)).unwrap();
    render(&mut player, FRAME);
    let mut voices = control.voices().as_slice().to_vec();
    voices.sort_unstable();
    assert_eq!(voices, [2, 5]);
    // Once their buffers run dry, nobody's voice plays.
    render(&mut player, FRAME);
    assert_eq!(control.voices(), Voices::default());
    // No more than so many are told of.
    let many: Voices = (0..VOICES as u32 + 4).collect();
    assert_eq!(many.as_slice().len(), VOICES);
    assert_eq!(many.as_slice()[VOICES - 1], VOICES as u32 - 1);
}

#[test]
fn agent_speech_is_muted_locally_and_a_replay_is_reported_while_it_plays() {
    let (sender, mut player, control) = player();
    control.set_agent_muted(true);
    sender.send(frame(1, 0, 100)).unwrap();
    sender.send(frame(AGENT_SOURCE, 0, 20)).unwrap();
    sender.send(frame(REPLAY_SOURCE, 0, 7)).unwrap();
    assert!(!control.is_replaying());
    assert_eq!(render(&mut player, FRAME), [100; FRAME]);
    assert!(control.is_replaying());
    // Unmuting resumes agent speech, live or played again, at once.
    control.set_agent_muted(false);
    sender.send(frame(1, 1, 100)).unwrap();
    sender.send(frame(AGENT_SOURCE, 1, 20)).unwrap();
    sender.send(frame(REPLAY_SOURCE, 1, 7)).unwrap();
    // Agent speech is one voice: the live speech that came first plays, and then what was played again.
    assert_eq!(render(&mut player, FRAME), [120; FRAME]);
    assert_eq!(render(&mut player, FRAME), [7; FRAME]);
    // The mute survives the mixer being made again for a new device rate.
    control.set_agent_muted(true);
    render(&mut player, FRAME);
    let mut output = vec![1; FRAME * 2];
    sender.send(frame(AGENT_SOURCE, 2, 20)).unwrap();
    player.render(RATE * 2, Layout::Mono, &mut output);
    assert!(output.iter().all(|&sample| sample == 0));
}

#[test]
fn skipped_agent_speech_is_silent_but_still_plays_and_the_mute_is_kept_apart() {
    let (sender, mut player, control) = player();
    control.set_agent_skipped(true);
    sender.send(frame(1, 0, 100)).unwrap();
    sender.send(frame(AGENT_SOURCE, 0, 20)).unwrap();
    sender.send(frame(REPLAY_SOURCE, 0, 7)).unwrap();
    // Other participants still play; the agent is silent here, live or played again, but still plays, with its level.
    assert_eq!(render(&mut player, FRAME), [100; FRAME]);
    let (playing, level) = control.agent_speech();
    assert!(playing);
    assert!(level > 0.0);
    // Muting and then no longer skipping keeps it silent, as it is still muted.
    control.set_agent_muted(true);
    control.set_agent_skipped(false);
    sender.send(frame(AGENT_SOURCE, 1, 20)).unwrap();
    assert_eq!(render(&mut player, FRAME), [0; FRAME]);
    // Unmuting then lets it be heard again.
    control.set_agent_muted(false);
    sender.send(frame(AGENT_SOURCE, 2, 20)).unwrap();
    assert_eq!(render(&mut player, FRAME), [20; FRAME]);
    // Skipping, and then unmuting, keeps it skipped; the skip survives the mixer being made again for a new rate.
    control.set_agent_skipped(true);
    control.set_agent_muted(false);
    render(&mut player, FRAME);
    let mut output = vec![1; FRAME * 2];
    sender
        .send(AudioFrame {
            sample_rate: RATE * 2,
            ..frame(AGENT_SOURCE, 3, 20)
        })
        .unwrap();
    player.render(RATE * 2, Layout::Mono, &mut output);
    assert!(output.iter().all(|&sample| sample == 0));
}

#[test]
fn never_plays_the_local_participant() {
    let (sender, mut player, control) = player();
    control.set_local_source(Some(5));
    sender.send(frame(5, 0, 1000)).unwrap();
    sender.send(frame(AGENT_SOURCE, 0, 7)).unwrap();
    assert_eq!(render(&mut player, FRAME), [7; FRAME]);

    // The local source survives a change of device rate: with enough buffered to play, only the agent is heard.
    for sequence in 1..5 {
        for (source, value) in [(5, 1000), (AGENT_SOURCE, 7)] {
            sender
                .send(AudioFrame {
                    sample_rate: 2 * RATE,
                    ..frame(source, sequence, value)
                })
                .unwrap();
        }
    }
    let mut output = [1; 2 * FRAME];
    player.render(2 * RATE, Layout::Mono, &mut output);
    assert_eq!(output, [7; 2 * FRAME]);
}

#[test]
fn clearing_discards_audio_received_but_not_yet_rendered() {
    let (sender, mut player, control) = player();
    for sequence in 0..4 {
        sender.send(frame(1, sequence, 50)).unwrap();
    }
    control.clear();
    assert_eq!(render(&mut player, FRAME), [0; FRAME]);
    for sequence in 10..14 {
        sender.send(frame(1, sequence, 60)).unwrap();
    }
    assert_eq!(render(&mut player, FRAME), [60; FRAME]);
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
    player.render(0, Layout::Mono, &mut output);
    assert_eq!(output, [0; FRAME]);
}

#[test]
fn plays_a_sound_cue_once_and_clearing_keeps_it() {
    let (_sender, mut player, control) = player();
    let cue = super::super::Bank::new(RATE).samples(Earcon::Choose).len();
    control.play(Earcon::Choose, Direction::Centre);
    assert!(render(&mut player, cue).iter().any(|sample| *sample != 0));
    assert_eq!(render(&mut player, FRAME), [0; FRAME]);
    control.play(Earcon::Choose, Direction::Centre);
    control.clear();
    assert!(render(&mut player, cue).iter().any(|sample| *sample != 0));
    // A mixer prepared while silenced starts silent.
    control.set_silenced(true);
    player.prepare(RATE * 3);
    player.prepare(RATE * 3);
    player.prepare(0);
    let mut output = vec![1; FRAME];
    player.render(RATE * 3, Layout::Mono, &mut output);
    assert_eq!(output, [0; FRAME]);
}

#[test]
fn renders_stereo_from_the_latest_scene() {
    let (sender, mut player, control) = player();
    control.set_scene(|scene| {
        scene.listener = Some(crate::playback::spatial::Listener {
            position: [0.0, 1.6, 0.0],
            orientation: [0.0, 0.0, 0.0, 1.0],
        });
        scene.sources = vec![(1, [-2.0, 1.6, 0.0])];
    });
    // Replaced before the callback takes it: only the latest counts.
    control.set_scene(|scene| scene.sources[0].1 = [2.0, 1.6, 0.0]);
    for sequence in 0..4 {
        sender.send(frame(1, sequence, 1_000)).unwrap();
    }
    let mut output = vec![0; FRAME * 2 * 3];
    player.render(RATE, Layout::Stereo, &mut output);
    let (left, right): (Vec<_>, Vec<_>) = output.chunks(2).map(|pair| (pair[0], pair[1])).unzip();
    assert!(
        right.iter().map(|s| i32::from(*s)).sum::<i32>()
            > 2 * left.iter().map(|s| i32::from(*s)).sum::<i32>()
    );
}

#[test]
fn plays_the_echo_probe() {
    let (_sender, mut player, control) = player();
    control.play_probe();
    assert!(render(&mut player, 200).iter().all(|sample| *sample == 0));
    // Loaded into the mixer there is, and into those made later.
    player.load_probe();
    control.play_probe();
    assert!(render(&mut player, 200).iter().any(|sample| *sample != 0));
    control.play_probe();
    let mut output = vec![0; 200];
    player.render(RATE * 2, Layout::Mono, &mut output);
    assert!(output.iter().any(|sample| *sample != 0));
}

#[test]
fn a_scene_set_after_a_render_is_written_whole() {
    let (_sender, mut player, control) = player();
    control.set_scene(|scene| scene.sources = vec![(1, [1.0, 0.0, 0.0])]);
    render(&mut player, FRAME);
    // The slot now holds the callback's older scene, which the change replaces whole.
    control.set_scene(|scene| {
        assert!(scene.sources.is_empty());
        scene.sources = vec![(2, [2.0, 0.0, 0.0])];
    });
    render(&mut player, FRAME);
    control.set_scene(|scene| assert_eq!(scene.sources, [(1, [1.0, 0.0, 0.0])]));
}

#[test]
fn the_callback_never_waits_for_the_scene_or_the_statistics() {
    let (sender, mut player, control) = player();
    // A scene still being written when the callback renders is taken at its next render.
    control.set_scene(|scene| {
        scene.sources = vec![(1, [1.0, 0.0, 0.0])];
        render(&mut player, FRAME);
    });
    control.set_scene(|scene| assert_eq!(scene.sources, [(1, [1.0, 0.0, 0.0])]));
    render(&mut player, FRAME);
    control.set_scene(|scene| assert!(scene.sources.is_empty()));

    // Statistics being read when the callback renders are published by a later render.
    let held = control.shared.statistics.lock().unwrap();
    sender.send(frame(2, 0, 1_000)).unwrap();
    sender.send(frame(2, 0, 1_000)).unwrap();
    render(&mut player, FRAME);
    assert_eq!(held.late_frames, 0);
    drop(held);
    render(&mut player, FRAME);
    assert_eq!(control.statistics().late_frames, 1);
}

#[test]
fn a_scene_left_poisoned_by_the_application_is_still_taken() {
    let (sender, mut player, control) = player();
    let failed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        control.set_scene(|scene| {
            scene.sources = vec![(1, [1.0, 0.0, 0.0])];
            panic!("the application failed while writing the scene");
        })
    }));
    assert!(failed.is_err());
    // The interrupted change was not marked fresh, so the callback keeps its own scene; later changes still reach it.
    render(&mut player, FRAME);
    control.set_scene(|scene| scene.sources = vec![(2, [2.0, 0.0, 0.0])]);
    render(&mut player, FRAME);
    control.set_scene(|scene| assert!(scene.sources.is_empty()));
    for sequence in 0..4 {
        sender.send(frame(2, sequence, 1_000)).unwrap();
    }
    assert!(
        render(&mut player, FRAME * 3)
            .iter()
            .any(|sample| *sample != 0)
    );
}

mod ambience {
    use super::*;
    use crate::playback::ambience::engine::tests::MONO;
    use crate::playback::ambience::{Ambience, Bed, Slot};

    fn tone() -> Option<Arc<Ambience>> {
        Some(Arc::new(
            Ambience::new().with_bed(Bed::new(Slot::embedded("tone", MONO))),
        ))
    }

    /// Render `frames` at `rate` in stereo, a few milliseconds' worth at a time and about as often, and whether the
    /// last was heard.
    fn play(player: &mut Player, rate: u32, frames: usize) -> bool {
        let mut heard = false;
        for _ in 0..frames / 96 {
            let mut output = vec![0; 192];
            player.render(rate, Layout::Stereo, &mut output);
            heard = output.iter().any(|sample| sample.unsigned_abs() > 500);
            std::thread::sleep(Duration::from_millis(2));
        }
        heard
    }

    /// Render until the ambience is heard, as its thread begins it, for at most ten seconds.
    fn until_heard(player: &mut Player, rate: u32) {
        assert!(
            (0..1_000).any(|_| play(player, rate, 96)),
            "the ambience was never heard"
        );
    }

    #[test]
    fn an_ambience_plays_from_its_own_thread_and_on_in_a_mixer_at_another_rate() {
        let (_sender, mut player, control) = player();
        // Without an ambience, no thread starts.
        control.set_ambience(None);
        assert!(!control.end_ambience());
        control.set_ambience(tone());
        until_heard(&mut player, 48_000);
        // Its thread decodes more as it plays: more than its channels hold at first.
        assert!(play(&mut player, 48_000, 48_000 / 2));
        // A mixer made at another rate takes the bus, at the level set.
        control.set_ambience_gain(0.5);
        until_heard(&mut player, 44_100);
        assert_eq!(player.mixer.as_ref().map(Mixer::ambience_gain), Some(0.5));
        let (playing, _) = player.mixer.as_ref().unwrap().playing_ambience();
        assert_eq!(playing, 1);
        // Another ambience, or none, reaches the thread it has.
        control.set_ambience(None);
        assert!(control.end_ambience());
    }

    #[test]
    fn off_from_the_start_it_decodes_nothing_and_its_thread_waits() {
        let (_sender, mut player, control) = player();
        control.set_ambience_gain(-1.0);
        control.set_ambience(tone());
        assert!(!play(&mut player, 48_000, 960));
        assert_eq!(player.mixer.as_ref().map(Mixer::ambience_gain), Some(0.0));
        assert!(control.end_ambience());
    }

    #[test]
    fn an_ambience_whose_thread_cannot_start_is_silent() {
        crate::test_support::log_everything();
        let (_sender, mut player, control) = player();
        crate::system::refuse("tomori-ambience");
        control.set_ambience(tone());
        assert!(!play(&mut player, 48_000, 960));
        assert!(!control.end_ambience());
        // Asked again, it starts.
        control.set_ambience(tone());
        until_heard(&mut player, 48_000);
        assert!(control.end_ambience());
    }
}
