// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

use super::super::{Bed, Call, Loop, Run};
use super::*;
use crate::playback::Layout;
use crate::playback::spatial::{Listener, Scene};

/// Half a second of a 440 Hz tone at half of full scale, 48 kHz mono, synthesised for these tests.
pub(crate) const MONO: &[u8] = include_bytes!("../fixtures/tone-mono.ogg");
/// The same in the left channel of two, the right silent.
pub(crate) const LEFT: &[u8] = include_bytes!("../fixtures/tone-left.ogg");
/// The tone's frames.
const FRAMES: usize = 24_000;

fn decode(bytes: &[u8], looping: bool, frames: usize, channels: usize) -> Vec<f32> {
    let mut decoder = Decoder::new(Arc::from(bytes), looping).unwrap();
    assert_eq!(decoder.rate(), 48_000);
    let mut out = Vec::new();
    decoder.read(&mut out, frames, channels);
    out
}

fn rms(samples: &[f32]) -> f32 {
    (samples.iter().map(|sample| sample * sample).sum::<f32>() / samples.len().max(1) as f32).sqrt()
}

#[test]
fn a_sound_decodes_as_long_as_it_is_into_the_channels_a_voice_wants() {
    // As long as it is, not padded to the end of its last packet.
    let mono = decode(MONO, false, FRAMES * 2, 1);
    assert_eq!(mono.len(), FRAMES);
    // A sine at half of full scale.
    let level = rms(&mono) / f32::from(i16::MAX);
    assert!((level - 0.5 / 2.0_f32.sqrt()).abs() < 0.02, "{level}");
    // Mono in both of two.
    let both = decode(MONO, false, 100, 2);
    assert_eq!(both.len(), 200);
    assert!(both.chunks(2).all(|pair| pair[0] == pair[1]));
    // Stereo as itself, and mixed down for one.
    let left = decode(LEFT, false, FRAMES, 2);
    let (l, r): (Vec<f32>, Vec<f32>) = left.chunks(2).map(|pair| (pair[0], pair[1])).unzip();
    assert!(rms(&l) > 100.0 * rms(&r).max(1.0));
    let down = decode(LEFT, false, FRAMES, 1);
    assert!((rms(&down) / rms(&l) - 0.5).abs() < 0.02);
}

#[test]
fn a_loop_begins_again_without_a_gap_and_a_sound_that_ends_ends() {
    let mut decoder = Decoder::new(Arc::from(MONO), true).unwrap();
    let mut out = Vec::new();
    assert_eq!(decoder.read(&mut out, FRAMES * 3 + 10, 1), FRAMES * 3 + 10);
    // The second pass is the first again.
    assert_eq!(out[FRAMES..FRAMES * 2], out[..FRAMES]);
    let mut once = Decoder::new(Arc::from(MONO), false).unwrap();
    let mut out = Vec::new();
    assert_eq!(once.read(&mut out, FRAMES * 2, 1), FRAMES);
    assert_eq!(once.read(&mut out, 10, 1), 0);
    // What is not Ogg Vorbis is refused, and one cut short in its audio ends where it can no longer be read: this one,
    // whose audio is all on its last page, at once.
    assert!(Decoder::new(Arc::from(&b"not a sound"[..]), false).is_err());
    let cut = &MONO[..MONO.len() - 300];
    let mut cut = Decoder::new(Arc::from(cut), true).unwrap();
    let mut out = Vec::new();
    assert_eq!(cut.read(&mut out, FRAMES, 1), 0);
}

/// The CRC of an Ogg page, over it with its own CRC as zero.
fn ogg_crc(page: &[u8]) -> u32 {
    page.iter().fold(0u32, |crc, &byte| {
        (0..8).fold(crc ^ (u32::from(byte) << 24), |crc, _| {
            if crc & 0x8000_0000 != 0 {
                (crc << 1) ^ 0x04c1_1db7
            } else {
                crc << 1
            }
        })
    })
}

/// `bytes`' Ogg pages, each as where it starts, where its body starts and where it ends.
fn ogg_pages(bytes: &[u8]) -> Vec<(usize, usize, usize)> {
    let mut pages = Vec::new();
    let mut at = 0;
    while at + 27 <= bytes.len() && &bytes[at..at + 4] == b"OggS" {
        let segments = usize::from(bytes[at + 26]);
        let body = at + 27 + segments;
        let length: usize = bytes[at + 27..body]
            .iter()
            .map(|&size| usize::from(size))
            .sum();
        pages.push((at, body, body + length));
        at = body + length;
    }
    pages
}

/// `bytes` with every page's CRC made right again, as a file damaged before it was written would be.
fn ogg_resealed(mut bytes: Vec<u8>) -> Vec<u8> {
    for (start, _, end) in ogg_pages(&bytes) {
        bytes[start + 22..start + 26].fill(0);
        let crc = ogg_crc(&bytes[start..end]);
        bytes[start + 22..start + 26].copy_from_slice(&crc.to_le_bytes());
    }
    bytes
}

#[test]
fn a_malformed_sound_ends_or_is_refused_and_never_takes_the_thread_with_it() {
    crate::test_support::log_everything();
    let pages = ogg_pages(MONO);
    assert!(pages.len() >= 3, "{pages:?}");
    // At a rate no sound is at, refused: the rate is 12 bytes into the identification header, the first page's body.
    let mut slow = MONO.to_vec();
    let rate = pages[0].1 + 12;
    slow[rate..rate + 4].copy_from_slice(&1u32.to_le_bytes());
    let error = Decoder::new(Arc::from(ogg_resealed(slow)), false)
        .err()
        .unwrap();
    assert!(error.contains("1 Hz"), "{error}");
    // Its audio damaged throughout, but its pages whole, so that the decoder itself meets the damage: every sound ends,
    // a loop too, rather than beginning again for ever, or decodes, and none panics out of the decoder.
    let (_, audio, end) = *pages.last().unwrap();
    let mut failed = 0;
    for (stride, mask) in [(7, 0xff), (13, 0x55), (31, 0x80), (97, 0x0f)] {
        let mut damaged = MONO.to_vec();
        for byte in damaged[audio..end].iter_mut().step_by(stride) {
            *byte ^= mask;
        }
        let damaged: Arc<[u8]> = Arc::from(ogg_resealed(damaged));
        // Its headers are whole.
        let mut decoder = Decoder::new(damaged, true).unwrap();
        let mut out = Vec::new();
        let read = decoder.read(&mut out, FRAMES * 3, 1);
        assert!(read <= FRAMES * 3);
        assert_eq!(out.len(), read);
        assert!(out.iter().all(|sample| sample.is_finite()));
        failed += usize::from(decoder.failed());
    }
    assert!(failed > 0);
    // A decoder that panics fails as one that refuses.
    let panicked = super::super::decoder::guard::<(), String>(|| panic!("a malformed file"));
    assert_eq!(panicked, Err("its decoder failed".to_owned()));
}

fn ambience(ambience: Ambience) -> Command {
    Command::Ambience(Some(Arc::new(ambience)))
}

fn mono() -> Slot {
    Slot::embedded("tone", MONO)
}

fn listener() -> Scene {
    Scene {
        listener: Some(Listener {
            position: [0.0, 1.6, 0.0],
            orientation: [0.0, 0.0, 0.0, 1.0],
        }),
        sources: Vec::new(),
        mono: false,
    }
}

/// Mix `frames` of `bus` in stereo, left and right.
fn mix(bus: &mut Bus, scene: &Scene, frames: usize) -> (Vec<f32>, Vec<f32>) {
    let mut out = vec![0.0; frames * 2];
    bus.mix(scene, Layout::Stereo, &mut out, &mut Vec::new());
    out.chunks(2).map(|pair| (pair[0], pair[1])).unzip()
}

#[test]
fn a_bed_plays_in_both_ears_over_and_over_once_heard() {
    let (mut feeder, mut bus) = pair(1);
    assert!(feeder.idle());
    feeder.apply(Command::Gain(1.0), Duration::ZERO);
    // Heard, or off again, without an ambience, there is nothing to decode.
    assert!(feeder.idle());
    feeder.apply(Command::Gain(0.0), Duration::ZERO);
    feeder.apply(Command::Gain(1.0), Duration::ZERO);
    assert!(feeder.idle());
    feeder.apply(
        ambience(Ambience::new().with_bed(Bed::new(Slot::embedded("left", LEFT)))),
        Duration::ZERO,
    );
    assert!(!feeder.idle());
    feeder.pump(Duration::ZERO);
    assert_eq!(feeder.playing(), 1);
    // About ten seconds, the feeder filling as the callback plays.
    let mut heard = (0.0, 0.0);
    for _ in 0..1_000 {
        let (left, right) = mix(&mut bus, &listener(), 480);
        heard.0 += rms(&left);
        heard.1 += rms(&right);
        feeder.pump(Duration::ZERO);
    }
    assert_eq!(bus.playing(), 1);
    assert_eq!(bus.late(), 0);
    assert!(
        heard.0 > 1_000.0 * 10_000.0 && heard.1 < heard.0 / 100.0,
        "{heard:?}"
    );
}

#[test]
fn a_silent_slot_plays_nothing_and_leaves_the_thread_idle() {
    crate::test_support::log_everything();
    for slot in [
        Slot::missing("wind"),
        Slot::new("wind", || None),
        Slot::new("wind", || Some(vec![0; MOST_BYTES + 1])),
        Slot::new("wind", || Some(b"not a sound".to_vec())),
    ] {
        let (mut feeder, mut bus) = pair(1);
        feeder.apply(Command::Gain(1.0), Duration::ZERO);
        feeder.apply(
            ambience(
                Ambience::new()
                    .with_bed(Bed::new(slot.clone()))
                    .with_loop(Loop::at(slot.clone(), [0.0; 3]))
                    .with_call(Call::new(vec![slot])),
            ),
            Duration::ZERO,
        );
        assert!(feeder.idle());
        feeder.pump(Duration::from_secs(3_600));
        assert_eq!(feeder.playing(), 0);
        let (left, right) = mix(&mut bus, &listener(), 480);
        assert!(left.iter().chain(&right).all(|sample| *sample == 0.0));
    }
}

#[test]
fn no_more_than_its_voices_play_and_calls_wait_for_one_to_be_free() {
    crate::test_support::log_everything();
    let (mut feeder, mut bus) = pair(1);
    feeder.apply(Command::Gain(1.0), Duration::ZERO);
    let mut many = Ambience::new();
    for index in 0..VOICES + 2 {
        many = many.with_loop(Loop::at(mono(), [index as f32, 1.6, -2.0]));
    }
    let many = many.with_call(Call::new(vec![mono()]).every(Duration::ZERO, Duration::ZERO));
    feeder.apply(ambience(many), Duration::ZERO);
    for millisecond in 0..200 {
        feeder.pump(Duration::from_millis(millisecond));
        mix(&mut bus, &listener(), 48);
        assert_eq!(feeder.playing(), VOICES);
    }
    assert_eq!(bus.playing(), VOICES);
}

#[test]
fn a_call_plays_once_from_where_it_is_and_frees_its_voice() {
    let (mut feeder, mut bus) = pair(5);
    feeder.apply(Command::Gain(1.0), Duration::ZERO);
    let call = Call::new(vec![mono(), Slot::missing("tui-2")])
        .every(Duration::from_secs(100), Duration::from_secs(100))
        .around(&[[-3.0, 1.6, 0.0]], 0.0);
    feeder.apply(ambience(Ambience::new().with_call(call)), Duration::ZERO);
    feeder.pump(Duration::ZERO);
    // Due within 100 seconds, its silent variant is skipped, and its other heard, from its left.
    let mut heard = None;
    for second in 1..=300 {
        feeder.pump(Duration::from_secs(second));
        if feeder.playing() == 1 {
            heard = Some(second);
            break;
        }
    }
    assert!(heard.is_some());
    let mut energy = (0.0, 0.0);
    for _ in 0..60 {
        let (left, right) = mix(&mut bus, &listener(), 480);
        energy.0 += rms(&left);
        energy.1 += rms(&right);
        feeder.pump(Duration::from_secs(400));
    }
    assert!(energy.0 > 2.0 * energy.1, "{energy:?}");
    // Half a second of it, then its end, and both sides free it.
    assert_eq!((feeder.playing(), bus.playing()), (0, 0));
}

#[test]
fn turned_off_every_voice_ends_and_nothing_decodes_until_it_is_heard_again() {
    let (mut feeder, mut bus) = pair(1);
    feeder.apply(Command::Gain(0.5), Duration::ZERO);
    feeder.apply(
        ambience(
            Ambience::new()
                .with_bed(Bed::new(Slot::embedded("left", LEFT)))
                .with_loop(Loop::at(mono(), [0.0, 1.6, -2.0])),
        ),
        Duration::ZERO,
    );
    feeder.pump(Duration::ZERO);
    mix(&mut bus, &listener(), 480);
    feeder.pump(Duration::ZERO);
    assert_eq!((feeder.playing(), bus.playing()), (2, 2));
    // Off, each voice's end waits behind what its channel holds, and the feeder is idle once every end is sent.
    feeder.apply(Command::Gain(0.0), Duration::ZERO);
    feeder.apply(Command::Gain(0.0), Duration::ZERO);
    assert!(!feeder.idle());
    // Ending again what is already ending sends one end each.
    let ambience_now = feeder
        .prepared
        .as_ref()
        .map(|prepared| prepared.ambience.clone());
    feeder.apply(Command::Ambience(ambience_now), Duration::ZERO);
    for _ in 0..20 {
        mix(&mut bus, &listener(), BLOCK);
        feeder.pump(Duration::ZERO);
    }
    assert!(feeder.idle());
    assert_eq!((feeder.playing(), bus.playing()), (0, 0));
    // Heard again, both begin again.
    feeder.apply(Command::Gain(1.0), Duration::ZERO);
    feeder.pump(Duration::ZERO);
    mix(&mut bus, &listener(), 48);
    assert_eq!((feeder.playing(), bus.playing()), (2, 2));
    // Another ambience ends the last's voices and begins its own; and none ends them all.
    feeder.apply(
        ambience(Ambience::new().with_loop(Loop::at(mono(), [0.0; 3]))),
        Duration::ZERO,
    );
    for _ in 0..20 {
        mix(&mut bus, &listener(), BLOCK);
        feeder.pump(Duration::ZERO);
    }
    assert_eq!((feeder.playing(), bus.playing()), (1, 1));
    feeder.apply(Command::Ambience(None), Duration::ZERO);
    for _ in 0..20 {
        mix(&mut bus, &listener(), BLOCK);
        feeder.pump(Duration::ZERO);
    }
    assert!(feeder.idle());
    assert_eq!(bus.playing(), 0);
}

#[test]
fn blocks_go_round_and_are_never_more_than_the_callback_has_room_to_give_back() {
    let (mut feeder, mut bus) = pair(1);
    feeder.apply(Command::Gain(1.0), Duration::ZERO);
    feeder.apply(
        ambience(Ambience::new().with_loop(Loop::at(mono(), [0.0; 3]))),
        Duration::ZERO,
    );
    assert_eq!(feeder.pool.len(), BLOCKS);
    for _ in 0..100 {
        feeder.pump(Duration::ZERO);
        mix(&mut bus, &listener(), 480);
    }
    // Each block came back: those in the channel, the one waiting and the one the callback plays, and the rest.
    feeder.pump(Duration::ZERO);
    assert_eq!(feeder.pool.len() + DEPTH + 1 + 1, BLOCKS);
    // Every voice playing and full holds every block there is, so none is made beyond them, and none is late.
    let loops = (0..VOICES).fold(Ambience::new(), |ambience, _| {
        ambience.with_loop(Loop::at(mono(), [0.0; 3]))
    });
    feeder.apply(ambience(loops), Duration::ZERO);
    for _ in 0..200 {
        feeder.pump(Duration::ZERO);
        assert!(feeder.pool.len() <= BLOCKS);
        mix(&mut bus, &listener(), 480);
    }
    assert_eq!(bus.playing(), VOICES);
    assert_eq!(bus.late(), 0);
    // Turned off, every one comes back, none lost for want of room to give it back.
    feeder.apply(Command::Gain(0.0), Duration::ZERO);
    for _ in 0..100 {
        feeder.pump(Duration::ZERO);
        mix(&mut bus, &listener(), BLOCK);
    }
    feeder.pump(Duration::ZERO);
    assert_eq!(bus.playing(), 0);
    assert_eq!(feeder.pool.len(), BLOCKS);
    // Should blocks be lost, none are made beyond them: its voices wait, late, rather than the callback having no room
    // to give one back.
    feeder.pool.clear();
    feeder.apply(Command::Gain(1.0), Duration::ZERO);
    feeder.pump(Duration::ZERO);
    mix(&mut bus, &listener(), 480);
    feeder.pump(Duration::ZERO);
    assert!(feeder.pool.is_empty());
    assert_eq!(bus.late(), VOICES as u64);
    // With its bus gone, nothing hears its voices, which go.
    let (mut feeder, bus) = pair(2);
    feeder.apply(Command::Gain(1.0), Duration::ZERO);
    feeder.apply(
        ambience(Ambience::new().with_loop(Loop::at(mono(), [0.0; 3]))),
        Duration::ZERO,
    );
    feeder.pump(Duration::ZERO);
    assert_eq!(feeder.playing(), 1);
    drop(bus);
    feeder.pump(Duration::ZERO);
    assert_eq!(feeder.playing(), 0);
}

#[test]
fn a_sound_lasts_as_its_last_page_says_and_a_run_waits_out_each_call() {
    // Half a second, by its last page; nothing for what has no pages, nor for a last page cut short.
    let decoder = Decoder::new(Arc::from(MONO), false).unwrap();
    assert_eq!(decoder.length(), Duration::from_millis(500));
    assert_eq!(super::super::decoder::frames(b"not a sound"), 0);
    let pages = ogg_pages(MONO);
    let cut = &MONO[..pages[pages.len() - 1].2 - 1];
    assert!(super::super::decoder::frames(cut) < FRAMES as u64);
    // A last page on which no packet ends leaves the length where the page before says.
    let mut unended = MONO.to_vec();
    let last = pages[pages.len() - 1].0;
    unended[last + 6..last + 14].fill(0xff);
    assert_eq!(
        super::super::decoder::frames(&unended),
        super::super::decoder::frames(cut)
    );
    let header = &MONO[..pages[pages.len() - 1].0 + 27];
    assert_eq!(
        super::super::decoder::frames(header),
        super::super::decoder::frames(cut)
    );
    // A run of three: each next call a second after the last's half second, at its pitch, then a long rest.
    let (mut feeder, _bus) = pair(3);
    feeder.apply(Command::Gain(1.0), Duration::ZERO);
    let call = Call::new(vec![mono(), Slot::missing("ruru-2")])
        .every(Duration::from_secs(600), Duration::from_secs(600))
        .run(Run::weighted(&[0.0, 0.0, 1.0]).gap(Duration::from_secs(1), Duration::from_secs(1)));
    feeder.apply(ambience(Ambience::new().with_call(call)), Duration::ZERO);
    let next = |feeder: &Feeder| feeder.prepared.as_ref().unwrap().scheduler.next()[0];
    let mut at = next(&feeder);
    let mut heard = 0;
    for _ in 0..3 {
        feeder.pump(at);
        heard = feeder.playing().max(heard);
        let then = next(&feeder);
        // Its silent variant lasts no time.
        assert!(
            [Duration::from_secs(1), Duration::from_millis(1_500)].contains(&(then - at))
                || then - at >= Duration::from_secs(600),
            "{:?}",
            then - at
        );
        at = then;
    }
    assert!(heard >= 1);
    assert!(at >= Duration::from_secs(600));
}
