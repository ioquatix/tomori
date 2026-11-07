// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

use std::time::Duration;

use tomori_environment_farmyard::behaviour::{Behaviour, Doing};
use tomori_environment_farmyard::flock::Flock;
use tomori_protocol::Payload;

use super::*;

fn chicken() -> Arc<Chicken> {
    Chicken::shared().unwrap()
}

/// The state the side of `space` shares at `clock`, with its flock as it began then.
fn state(space: &str, clock: u64) -> (Flock, Update) {
    let flock = Flock::new(flock::seed(space), chicken(), clock);
    let state = flock.state(clock);
    (
        flock,
        Update::State(Payload::new(serde_json::to_value(state).unwrap())),
    )
}

fn event(clock: u64, chicken: usize, behaviour: Behaviour) -> Update {
    let event = Event {
        clock,
        chicken,
        behaviour,
    };
    Update::Event(Payload::new(serde_json::to_value(event).unwrap()))
}

/// The client side of `space`, begun at `epoch`, having received its state at `clock`.
fn joined(space: &str, epoch: Instant, clock: u64) -> (FarmyardClient, Flock) {
    let mut client = FarmyardClient::new(space, Some(chicken()), epoch);
    let (flock, update) = state(space, clock);
    client.update(Input::new(vec![update], epoch));
    (client, flock)
}

fn drawn(output: &Output) -> &Drawn {
    output
        .drawn
        .as_ref()
        .unwrap()
        .downcast_ref::<Drawn>()
        .unwrap()
}

#[test]
fn nothing_is_shown_until_the_server_says_what_there_is() {
    let epoch = Instant::now();
    let mut client = FarmyardClient::new("space-1", Some(chicken()), epoch);
    let output = client.update(Input::new(Vec::new(), epoch));
    assert!(output.drawn.is_none() && output.intents.is_empty() && output.sounds.is_empty());
    assert_eq!(client.clock(epoch), None);
    // What cannot be read is passed over.
    let broken = Payload::new(serde_json::json!({"chickens": "none"}));
    client.update(Input::new(
        vec![Update::State(broken.clone()), Update::Event(broken)],
        epoch,
    ));
    assert_eq!(client.clock(epoch), None);
    // Without its chicken, which could not be loaded, it shows none.
    let mut bare = FarmyardClient::new("space-1", None, epoch);
    let (_, update) = state("space-1", 5_000);
    let output = bare.update(Input::new(vec![update], epoch));
    assert!(drawn(&output).chickens.is_empty());
    assert!(
        bare.pick(&Ray::new(Vec3::Y, Vec3::NEG_Z).unwrap())
            .is_none()
    );
}

#[test]
fn the_flock_is_shown_on_the_farmyard_s_clock_as_its_server_side_says() {
    let epoch = Instant::now();
    let (mut client, flock) = joined("space-1", epoch, 5_000);
    assert_eq!(client.clock(epoch), Some(5_000));
    assert_eq!(
        client.clock(epoch + Duration::from_millis(250)),
        Some(5_250)
    );
    let output = client.update(Input::new(Vec::new(), epoch + Duration::from_millis(100)));
    let shown = drawn(&output);
    assert_eq!(shown.chickens.len(), flock.hens().len());
    // A later event moves the clock on if it arrived later than the clock says, and changes what its chicken does.
    let walking = Behaviour {
        since: 6_000,
        at: flock.hens()[0].behaviour.at,
        heading: 0.0,
        doing: Doing::Walk {
            to: [0.0, 0.0, -2.0],
        },
    };
    client.update(Input::new(
        vec![event(6_000, 0, walking)],
        epoch + Duration::from_millis(200),
    ));
    assert_eq!(
        client.clock(epoch + Duration::from_millis(200)),
        Some(6_000)
    );
    // An event that arrived as the clock says leaves it be; one for a chicken it does not have changes nothing.
    client.update(Input::new(
        vec![event(5_000, 0, walking), event(5_000, 9, walking)],
        epoch + Duration::from_millis(300),
    ));
    assert_eq!(
        client.clock(epoch + Duration::from_millis(300)),
        Some(6_100)
    );
    let before = drawn(&client.update(Input::new(Vec::new(), epoch + Duration::from_millis(400))))
        .chickens[0]
        .transform;
    let after = drawn(&client.update(Input::new(Vec::new(), epoch + Duration::from_secs(4))))
        .chickens[0]
        .transform;
    assert_ne!(before, after);
}

#[test]
fn the_ray_points_at_the_nearest_chicken() {
    let epoch = Instant::now();
    let (mut client, flock) = joined("space-1", epoch, 5_000);
    client.update(Input::new(Vec::new(), epoch));
    let at = Vec3::from(flock.hens()[1].behaviour.at);
    let ray = Ray::new(at + Vec3::new(0.0, 0.18, 3.0), Vec3::NEG_Z).unwrap();
    let picked = client.pick(&ray).unwrap();
    assert_eq!(
        (picked.object.as_str(), picked.label.as_str()),
        ("chicken-1", flock.hens()[1].name.as_str())
    );
    assert!(picked.distance > 2.5 && picked.distance < 3.0);
    assert!(
        client
            .pick(&Ray::new(Vec3::new(0.0, 30.0, 0.0), Vec3::Y).unwrap())
            .is_none()
    );
    // Through two, the nearer.
    let other = Vec3::from(flock.hens()[0].behaviour.at);
    let through = (at - other).normalize();
    let ray = Ray::new(other + Vec3::Y * 0.18 - through * 2.0, through).unwrap();
    assert_eq!(client.pick(&ray).unwrap().object, "chicken-0");
}

#[test]
fn pointing_at_a_chicken_asks_it_to_notice_and_looking_away_ends_it() {
    let epoch = Instant::now();
    let (mut client, _) = joined("space-1", epoch, 5_000);
    let head = Some(Vec3::new(1.0, 1.6, 2.0));
    let intents = |client: &mut FarmyardClient,
                   pointed: Option<&str>,
                   head: Option<Vec3>,
                   after: u64|
     -> Vec<serde_json::Value> {
        let now = epoch + Duration::from_millis(after);
        client
            .update(Input::new(Vec::new(), now).pointing(pointed, head))
            .intents
            .into_iter()
            .map(|intent| intent.value().clone())
            .collect()
    };
    assert_eq!(
        intents(&mut client, Some("chicken-0"), head, 100),
        [serde_json::json!({"notice": 0, "toward": [1.0, 2.0]})]
    );
    // Held, it is not asked again, even as the server side shares the flock anew.
    let (_, again) = state("space-1", 5_200);
    let pointing = client.update(
        Input::new(vec![again], epoch + Duration::from_millis(150))
            .pointing(Some("chicken-0"), head),
    );
    assert!(pointing.intents.is_empty());
    assert!(intents(&mut client, Some("chicken-0"), head, 200).is_empty());
    // Pointing elsewhere, at something that is no chicken, or one it does not have, looks away.
    assert_eq!(
        intents(&mut client, Some("lantern"), head, 300),
        [serde_json::json!({"away": 0})]
    );
    assert!(intents(&mut client, Some("chicken-x"), head, 400).is_empty());
    assert!(intents(&mut client, Some("chicken-9"), head, 500).is_empty());
    // Asked again too soon, it is not; without a head, it is not; later, it is.
    assert!(intents(&mut client, Some("chicken-0"), head, 600).is_empty());
    assert!(intents(&mut client, Some("chicken-1"), None, 700).is_empty());
    assert_eq!(intents(&mut client, Some("chicken-1"), head, 800).len(), 1);
    assert_eq!(
        intents(&mut client, Some("chicken-0"), head, 100 + PER_CHICKEN),
        [
            serde_json::json!({"away": 1}),
            serde_json::json!({"notice": 0, "toward": [1.0, 2.0]})
        ]
    );
    assert_eq!(
        intents(&mut client, None, head, 200 + PER_CHICKEN),
        [serde_json::json!({"away": 0})]
    );
}

#[test]
fn chickens_cluck_when_noticed_squawk_when_startled_and_now_and_then_of_their_own_accord() {
    let epoch = Instant::now();
    let (mut client, flock) = joined("space-1", epoch, 5_000);
    let at = |after: u64| epoch + Duration::from_millis(after);
    client.update(Input::new(Vec::new(), at(0)));
    let behaviour = |since, doing| Behaviour {
        since,
        at: flock.hens()[0].behaviour.at,
        heading: 0.0,
        doing,
    };
    // Confused, it clucks from its head; startled, it squawks; walking, its steps make no sound.
    let output = client.update(Input::new(
        vec![event(
            5_100,
            0,
            behaviour(5_100, Doing::Confused { toward: [0.0, 0.0] }),
        )],
        at(500),
    ));
    let heard_as = |output: &Output, kind: usize| {
        output.sounds.iter().any(|(sound, position)| {
            Arc::ptr_eq(&sound.samples, &rendered()[kind])
                && sound.rate == RATE
                && position[1] > 0.2
        })
    };
    assert!(heard_as(&output, 2));
    let output = client.update(Input::new(
        vec![event(
            5_600,
            0,
            behaviour(5_600, Doing::Sprint { away: [1.0, 0.0] }),
        )],
        at(700),
    ));
    assert!(heard_as(&output, 3));
    client.update(Input::new(
        vec![event(
            9_000,
            1,
            behaviour(
                9_000,
                Doing::Walk {
                    to: [0.0, 0.0, -2.0],
                },
            ),
        )],
        at(4_000),
    ));
    // Over a few minutes, a frame at a time, some cluck of their own accord, the same moments for everyone in the space.
    let heard = |client: &mut FarmyardClient| {
        (1..=3_600u64)
            .map(|frame| {
                client
                    .update(Input::new(Vec::new(), at(4_000 + frame * 100)))
                    .sounds
                    .len()
            })
            .sum::<usize>()
    };
    let clucks = heard(&mut client);
    assert!(clucks > 2, "{clucks}");
    let (mut other, _) = joined("space-1", epoch, 5_000);
    other.update(Input::new(Vec::new(), at(4_000)));
    assert_eq!(heard(&mut other), clucks);
    // A client that joined long after a chicken was confused does not hear it, and nor does a clock that jumps.
    let mut late = FarmyardClient::new("space-1", Some(chicken()), epoch);
    let (_, update) = state("space-1", 0);
    late.update(Input::new(vec![update], at(0)));
    let output = late.update(Input::new(
        vec![event(
            0,
            0,
            behaviour(0, Doing::Confused { toward: [0.0, 0.0] }),
        )],
        at(0),
    ));
    assert!(output.sounds.is_empty());
    let output = late.update(Input::new(
        vec![event(600_000, 0, behaviour(0, Doing::Stand))],
        at(10),
    ));
    assert!(output.sounds.is_empty());
}

#[test]
fn a_state_or_event_at_the_ends_of_the_clock_is_shown_without_overflowing() {
    // What a server shares is read as untrusted: clocks and moments at the very ends of their range, far-off places and
    // headings, move and draw the chickens without overflowing, so a debug build does not panic.
    let epoch = Instant::now();
    for clock in [0, u64::MAX] {
        let mut client = FarmyardClient::new("space-1", Some(chicken()), epoch);
        let mut shared =
            serde_json::to_value(Flock::new(flock::seed("space-1"), chicken(), 0).state(0))
                .unwrap();
        shared["clock"] = clock.into();
        let mut updates = vec![Update::State(Payload::new(shared))];
        for (chicken, doing) in [
            Doing::Walk {
                to: [f32::MAX, 0.0, -f32::MAX],
            },
            Doing::Recover,
            Doing::Sprint {
                away: [f32::MAX, f32::MAX],
            },
        ]
        .into_iter()
        .enumerate()
        {
            updates.push(event(
                clock,
                chicken,
                Behaviour {
                    since: u64::MAX,
                    at: [-f32::MAX, 0.0, f32::MAX],
                    heading: 1.0e30,
                    doing,
                },
            ));
        }
        let later = epoch + Duration::from_secs(2);
        let output = client.update(Input::new(updates, later));
        assert!(!drawn(&output).chickens.is_empty());
        assert!(client.clock(later).is_some());
    }
}
