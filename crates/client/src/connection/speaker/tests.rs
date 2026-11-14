// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

//! Tests of the connection's speaker.
use super::*;
use crate::{connection::test_support::*, speaker::Output};
use std::sync::{atomic::Ordering, mpsc};

/// Poll the speaker until it reports a failure.
fn failure(speaker: &mut Speaker) -> String {
    // Waits at least once, however soon the speaker fails, so that how the race went changes no coverage.
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut error = None;
    while error.is_none() {
        assert!(Instant::now() < deadline, "no failure reported");
        std::thread::sleep(Duration::from_millis(1));
        error = speaker.poll();
    }
    error.unwrap()
}

/// A player of nothing, as a client's would be before anything arrives.
fn player() -> (Shared, PlayerControl) {
    let (_audio, received) = mpsc::sync_channel(1);
    let (player, control) = Player::new(received, MixerOptions::default());
    (Arc::new(Mutex::new(player)), control)
}

#[test]
fn rerouting_the_speaker_reopens_it_only_when_the_route_changes() {
    let mut speaker = Speaker::new(Box::new(FakeSpeaker::default()));
    let route = Route {
        communication: Some(42),
    };
    // Nothing open: only remembered for the next open.
    speaker.reroute(route);
    assert_eq!(speaker.route, route);
    speaker.reroute(route);
    assert!(speaker.output.is_none());
}

#[test]
fn rerouting_an_open_speaker_reopens_it_once() {
    let fake = FakeSpeaker::default();
    let opened = fake.opened.clone();
    let mut speaker = Speaker::new(Box::new(fake));
    speaker.player = Some(player());
    speaker.open();
    wait(|| opened.load(Ordering::Relaxed) == 1);
    let route = Route {
        communication: Some(42),
    };
    speaker.reroute(route);
    wait(|| opened.load(Ordering::Relaxed) == 2);
    wait(|| speaker.output.as_ref().is_some_and(Speaking::is_open));
    // The fake opens as media, as a speaker that fell back would.
    assert_eq!(speaker.output.as_ref().unwrap().communication(), None);
    speaker.reroute(route);
    std::thread::sleep(Duration::from_millis(20));
    assert_eq!(opened.load(Ordering::Relaxed), 2);
}

/// Opens every output as voice communication in audio session 42, as Android's speaker does for the live microphone.
struct Communicating;

impl Playback for Communicating {
    fn open(
        &self,
        _player: Shared,
        _failed: crate::speaker::Failed,
        _route: Route,
    ) -> Result<Output, String> {
        Ok(Output::communicating((), 42))
    }
}

#[test]
fn an_open_speaker_is_opened_again_on_gaining_focus_where_the_platform_asks() {
    crate::test_support::log_everything();
    let fake = FakeSpeaker {
        reopens: true,
        ..FakeSpeaker::default()
    };
    let opened = fake.opened.clone();
    let mut speaker = Speaker::new(Box::new(fake));
    speaker.player = Some(player());
    // Nothing open: gaining focus opens nothing.
    speaker.focus(false);
    speaker.focus(true);
    assert!(speaker.output.is_none());
    speaker.focus(false);
    speaker.open();
    wait(|| opened.load(Ordering::Relaxed) == 1);
    speaker.focus(true);
    wait(|| opened.load(Ordering::Relaxed) == 2);
    // Keeping focus, or losing it, keeps the output.
    speaker.focus(true);
    speaker.focus(false);
    std::thread::sleep(Duration::from_millis(20));
    assert_eq!(opened.load(Ordering::Relaxed), 2);
}

#[test]
fn an_open_speaker_is_kept_on_gaining_focus_where_the_platform_does_not_ask() {
    let mut speaker = Speaker::new(Box::new(Communicating));
    speaker.player = Some(player());
    speaker.open();
    wait(|| speaker.output.as_ref().is_some_and(Speaking::is_open));
    speaker.focus(false);
    speaker.focus(true);
    assert!(speaker.output.as_ref().is_some_and(Speaking::is_open));
}

#[test]
fn a_speaker_playing_as_voice_communication_says_in_which_session() {
    let mut speaker = Speaker::new(Box::new(Communicating));
    speaker.player = Some(player());
    speaker.open();
    wait(|| speaker.output.as_ref().is_some_and(Speaking::is_open));
    assert_eq!(speaker.output.as_ref().unwrap().communication(), Some(42));
}

#[test]
fn the_speaker_opens_on_its_own_thread_and_closes_when_the_client_goes_away() {
    let mut client = client();
    let fake = FakeSpeaker::default();
    let mut speaker = Speaker::new(Box::new(fake.clone()));
    // Nothing to play before a client is attached.
    speaker.open();
    assert!(speaker.output.is_none());

    speaker.attach(&mut client);
    assert!(client.take_audio().is_none(), "the speaker took the audio");
    speaker.open();
    speaker.open();
    wait(|| fake.opened.load(Ordering::Relaxed) == 1);
    assert!(speaker.output.as_ref().unwrap().is_open());
    assert_eq!(speaker.poll(), None);

    speaker.detach();
    assert!(speaker.output.is_none() && speaker.control().is_none());
    wait(|| fake.closed.load(Ordering::Relaxed) == 1);
    assert_eq!(fake.opened.load(Ordering::Relaxed), 1);
    client.close();
}

#[test]
fn a_failed_speaker_is_reported_once_and_opened_again_later() {
    crate::test_support::log_everything();
    let mut client = client();
    let fake = FakeSpeaker::default();
    let mut speaker = Speaker::new(Box::new(fake.clone()));
    speaker.attach(&mut client);

    fake.refuse.store(true, Ordering::Relaxed);
    speaker.open();
    let error = failure(&mut speaker);
    assert!(error.contains("No speaker"), "{error}");
    assert!(speaker.output.is_none());
    // Not tried again until the retry is due, and not shown again when it fails again.
    speaker.open();
    assert!(speaker.output.is_none());
    speaker.retry_at = Some(Instant::now());
    speaker.open();
    wait(|| speaker.failure.lock().unwrap().is_some());
    assert_eq!(speaker.poll(), None);
    assert!(speaker.output.is_none());

    // A new welcome tries again at once, and shows the failure again.
    speaker.connected();
    speaker.open();
    let error = failure(&mut speaker);
    assert!(error.contains("No speaker"), "{error}");

    fake.refuse.store(false, Ordering::Relaxed);
    speaker.retry_at = Some(Instant::now());
    speaker.open();
    wait(|| fake.opened.load(Ordering::Relaxed) == 1);
    assert_eq!(speaker.poll(), None);

    // The open output fails from another thread: it is closed, shown, and reopened once due.
    let failed = fake.failed.lock().unwrap().take().unwrap();
    std::thread::spawn(move || failed("The speaker failed: unplugged".into()))
        .join()
        .unwrap();
    let error = speaker.poll().expect("the failure is shown");
    assert!(error.contains("unplugged"), "{error}");
    assert!(speaker.output.is_none());
    wait(|| fake.closed.load(Ordering::Relaxed) == 1);
    speaker.open();
    speaker.retry_at = Some(Instant::now());
    speaker.open();
    wait(|| fake.opened.load(Ordering::Relaxed) == 2);
    client.close();
}

#[test]
fn a_speaker_thread_that_cannot_start_is_reported_once_and_started_again_later() {
    let mut client = client();
    let fake = FakeSpeaker::default();
    let mut speaker = Speaker::new(Box::new(fake.clone()));
    speaker.attach(&mut client);

    crate::system::refuse("tomori-speaker");
    speaker.open();
    let error = speaker.poll().expect("the failure is shown");
    assert_eq!(
        error,
        "Cannot start the speaker thread: the system refused to start tomori-speaker Replies cannot be heard."
    );
    assert!(speaker.output.is_none());
    // Not shown again when it fails again before the speaker opens.
    crate::system::refuse("tomori-speaker");
    speaker.retry_at = Some(Instant::now());
    speaker.open();
    assert_eq!(speaker.poll(), None);
    assert!(speaker.output.is_none());

    // Started again once the retry is due.
    speaker.retry_at = Some(Instant::now());
    speaker.open();
    wait(|| fake.opened.load(Ordering::Relaxed) == 1);
    assert_eq!(speaker.poll(), None);
    assert!(speaker.output.is_some());
    client.close();
}

#[test]
fn opening_the_speaker_discards_audio_queued_while_nothing_played() {
    let fake = FakeSpeaker::default();
    let mut speaker = Speaker::new(Box::new(fake.clone()));
    let (sender, audio) = mpsc::sync_channel(8);
    let (player, control) = Player::new(audio, MixerOptions::default());
    speaker.player = Some((Arc::new(Mutex::new(player)), control));
    for sequence in 0..8 {
        sender
            .send(tomori_protocol::AudioFrame {
                gate: tomori_protocol::Gate::Live,
                source: 1,
                sequence,
                timestamp: 0,
                sample_rate: 48_000,
                samples: vec![1000; 960],
            })
            .unwrap();
    }
    // The fake renders as it opens, and asserts that it hears nothing.
    speaker.open();
    wait(|| fake.opened.load(Ordering::Relaxed) == 1);
}
