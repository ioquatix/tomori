// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

use std::sync::Arc;

use super::*;
use crate::chicken::tests::chicken;

fn flock(space: &str) -> Flock {
    Flock::new(seed(space), Arc::new(chicken()), 0)
}

/// Run `flock` from `from` to `to` in steps of 50 ms, as the side does, calling `each` at every step.
fn run(flock: &mut Flock, (from, to): (u64, u64), mut each: impl FnMut(&Flock, u64)) {
    let mut now = from;
    while now < to {
        flock.update(now);
        each(flock, now);
        now += 50;
    }
}

#[test]
fn a_flock_is_the_same_for_the_same_space() {
    let one = flock("space-1");
    assert!((FEWEST..=MOST).contains(&one.hens().len()));
    assert_eq!(one.hens().iter().filter(|hen| hen.cone).count(), 1);
    assert_eq!(one.hens(), flock("space-1").hens());
    assert_ne!(one.hens(), flock("space-2").hens());
    assert!(one.hens().iter().all(|hen| (0.86..1.2).contains(&hen.pace)));
    // Each stands somewhere clear, at first.
    for hen in one.hens() {
        let at = Vec2::new(hen.behaviour.at[0], hen.behaviour.at[2]);
        assert!(Flock::clear(at), "{at}");
    }
    assert_eq!(one.state(5).clock, 5);
    assert_eq!(one.state(5).chickens, one.hens());
}

#[test]
fn chickens_wander_clear_of_the_yard_and_each_other_and_visit_the_coop_now_and_then() {
    for space in ["space-1", "space-2", "space-3", "space-4"] {
        let mut flock = flock(space);
        let mut roosted = false;
        let mut walked = false;
        let mut closest = f32::INFINITY;
        run(&mut flock, (0, 20 * 60_000), |flock, now| {
            for (index, hen) in flock.hens().iter().enumerate() {
                let here = flock.position(index, now);
                roosted |= hen.behaviour.doing == Doing::Roost;
                walked |= matches!(hen.behaviour.doing, Doing::Walk { .. });
                if here.y < 0.01 && hen.behaviour.doing != Doing::Roost {
                    // On the ground, it is in the yard, clear of what stands there, but at the ramp's foot.
                    let point = Vec2::new(here.x, here.z);
                    assert!(AREA.grown(0.05).contains(point), "{space} {index} {point}");
                    let ramp = layout::ramp().grown(CLEARANCE + 0.3);
                    assert!(
                        ramp.contains(point)
                            || layout::obstacles()
                                .iter()
                                .all(|obstacle| !obstacle.grown(CLEARANCE - 0.1).contains(point)),
                        "{space} {index} {point}"
                    );
                    for other in (0..flock.hens().len()).filter(|other| *other != index) {
                        let there = flock.position(other, now);
                        if there.y < 0.01 {
                            closest = closest.min(point.distance(Vec2::new(there.x, there.z)));
                        }
                    }
                }
            }
        });
        assert!(walked && roosted, "{space}");
        // Chickens pass near each other, but never through.
        assert!(closest > 0.15, "{space} {closest}");
    }
}

#[test]
fn a_chicken_pointed_at_is_confused_until_its_participant_looks_away() {
    let mut flock = flock("space-1");
    let notice = |chicken| Intent::Notice {
        notice: chicken,
        toward: [0.0, 0.0],
    };
    // At once, from the next moment, it turns to them.
    let (chicken, confused) = flock.ask(1, notice(0), 10_000).unwrap();
    assert_eq!((chicken, confused.since), (0, 10_000 + LEAD));
    assert_eq!(confused.doing, Doing::Confused { toward: [0.0, 0.0] });
    flock.update(10_000 + LEAD);
    // Again too soon, from the same participant, of it or another, nothing.
    assert!(flock.ask(1, notice(0), 11_000).is_none());
    assert!(flock.ask(1, notice(1), 11_000).is_none());
    // Looking away, it recovers; looking away again, nothing.
    let (_, recovering) = flock.ask(1, Intent::Away { away: 0 }, 12_000).unwrap();
    assert_eq!(recovering.doing, Doing::Recover);
    assert!(flock.ask(1, Intent::Away { away: 0 }, 12_100).is_none());
    // A chicken the flock does not have, or a place that is no place, nothing.
    assert!(flock.ask(2, notice(MOST + 1), 20_000).is_none());
    assert!(
        flock
            .ask(2, Intent::Away { away: MOST + 1 }, 20_000)
            .is_none()
    );
    assert!(
        flock
            .ask(
                2,
                Intent::Notice {
                    notice: 1,
                    toward: [f32::NAN, 0.0]
                },
                20_000
            )
            .is_none()
    );
    // Far away is taken as at the edge of reach.
    let (_, far) = flock
        .ask(
            2,
            Intent::Notice {
                notice: 1,
                toward: [1e9, -1e9],
            },
            20_000,
        )
        .unwrap();
    assert_eq!(
        far.doing,
        Doing::Confused {
            toward: [REACH, -REACH]
        }
    );
    // Held long enough, it is startled; and pointed at again, as it cannot be startled again so soon, it recovers of
    // its own accord when nobody has said they looked away for too long.
    let changes = flock.update(20_000 + LEAD + STARTLE);
    assert!(changes.iter().any(
        |(chicken, behaviour)| *chicken == 1 && matches!(behaviour.doing, Doing::Sprint { .. })
    ));
    flock.update(30_000);
    flock.update(40_000);
    let (_, again) = flock.ask(5, notice(1), 40_000).unwrap();
    assert!(matches!(again.doing, Doing::Confused { .. }));
    assert!(
        flock
            .update(40_000 + LEAD + STARTLE)
            .iter()
            .all(|(chicken, _)| *chicken != 1)
    );
    let changes = flock.update(40_000 + LEAD + HOLD);
    assert!(
        changes
            .iter()
            .any(|(chicken, behaviour)| *chicken == 1 && behaviour.doing == Doing::Recover),
        "{changes:?}"
    );
    // Another participant looking away from a chicken they do not point at changes nothing.
    assert!(flock.ask(3, Intent::Away { away: 1 }, 40_000).is_none());
}

#[test]
fn a_chicken_pointed_at_long_enough_or_by_two_is_startled_rarely() {
    let mut flock = flock("space-1");
    let notice = |chicken| Intent::Notice {
        notice: chicken,
        toward: [0.0, 0.0],
    };
    flock.ask(1, notice(0), 10_000).unwrap();
    assert!(
        flock.update(10_000 + STARTLE - 100).is_empty()
            || !flock.hens()[0]
                .behaviour
                .doing
                .eq(&Doing::Sprint { away: [0.0, 0.0] })
    );
    let changes = flock.update(10_000 + STARTLE + 100);
    let sprint = changes.iter().find(|(chicken, _)| *chicken == 0).unwrap().1;
    assert!(matches!(sprint.doing, Doing::Sprint { .. }));
    // Running, it pays no heed to pointing.
    assert!(flock.ask(2, notice(0), 15_000).is_none());
    // Two at once startle another at once, but not one startled lately.
    flock.ask(1, notice(1), 30_000).unwrap();
    flock.ask(2, notice(1), 30_000).unwrap();
    let changes = flock.update(30_000 + LEAD);
    assert!(changes.iter().any(
        |(chicken, behaviour)| *chicken == 1 && matches!(behaviour.doing, Doing::Sprint { .. })
    ));
    let mut flock = run_until_standing(flock, 0, 60_000);
    let now = 60_000;
    flock.ask(3, notice(0), now).unwrap();
    flock.ask(4, notice(0), now).unwrap();
    let changes = flock.update(now + LEAD);
    assert!(!changes.iter().any(
        |(chicken, behaviour)| *chicken == 0 && matches!(behaviour.doing, Doing::Sprint { .. })
    ));
    // Once a minute has passed since, it can be startled again.
    let later = 10_000 + STARTLE + 100 + UNSTARTLED;
    let changes = flock.update(later);
    assert!(
        changes
            .iter()
            .any(|(chicken, behaviour)| *chicken == 0
                && matches!(behaviour.doing, Doing::Sprint { .. })),
        "{changes:?}"
    );
}

/// The flock, moved on until chicken `index` is on the ground and doing nothing in particular, at `now`.
fn run_until_standing(mut flock: Flock, index: usize, now: u64) -> Flock {
    let mut time = now - 5_000;
    while time < now {
        flock.update(time);
        time += 50;
    }
    let _ = index;
    flock
}

#[test]
fn a_chicken_in_the_coop_or_on_its_ramp_cannot_be_pointed_at() {
    let mut flock = flock("space-1");
    let mut refused = 0;
    run(&mut flock, (0, 30 * 60_000), |_, _| {});
    // Somewhere in all that, chickens roosted; now point at each, while one is up there.
    let mut flock = Flock::new(seed("space-1"), Arc::new(chicken()), 0);
    let mut now = 0;
    while now < 30 * 60_000 {
        flock.update(now);
        for index in 0..flock.hens().len() {
            let up = flock.position(index, now).y > 0.01
                || flock.hens()[index].behaviour.doing == Doing::Roost;
            if up
                && flock
                    .ask(
                        9,
                        Intent::Notice {
                            notice: index,
                            toward: [0.0, 0.0],
                        },
                        now,
                    )
                    .is_none()
            {
                refused += 1;
            }
        }
        now += 250;
    }
    assert!(refused > 0);
}

#[test]
fn what_a_chicken_is_doing_is_described_with_its_name_and_where() {
    let mut flock = flock("space-1");
    let described = flock.describe(0);
    assert_eq!(described.len(), flock.hens().len());
    assert_eq!(described[0].0, "chicken-0");
    let coned = flock.hens().iter().position(|hen| hen.cone).unwrap();
    assert!(described[coned].1.contains("wearing a traffic cone"));
    assert!(described[0].1.starts_with(&flock.hens()[0].name));
    flock
        .ask(
            1,
            Intent::Notice {
                notice: 0,
                toward: [0.0, 0.0],
            },
            1_000,
        )
        .unwrap();
    assert!(flock.describe(2_000)[0].1.contains("puzzled"));
    // Each kind of doing has words, and so has each place.
    let mut seen = std::collections::BTreeSet::new();
    run(&mut flock, (2_000, 30 * 60_000), |flock, now| {
        for (_, description) in flock.describe(now) {
            seen.insert(
                description
                    .split(" a hen in the farmyard, ")
                    .nth(1)
                    .unwrap()
                    .split(" by ")
                    .next()
                    .unwrap()
                    .split(" near ")
                    .next()
                    .unwrap()
                    .to_owned(),
            );
        }
    });
    assert!(seen.len() >= 5, "{seen:?}");
    assert_eq!(near(Vec3::new(0.0, 0.0, 1.0)), "near the gate");
    assert_eq!(near(Vec3::new(3.7, 0.0, -10.0)), "by the hay");
    assert!((bearing(Vec2::ZERO, Vec2::X) - std::f32::consts::FRAC_PI_2).abs() < 1e-6);
}

#[test]
fn the_flock_is_sent_as_state_events_and_intents_its_clients_read() {
    let flock = flock("space-1");
    let state = flock.state(42);
    let json = serde_json::to_string(&state).unwrap();
    assert_eq!(serde_json::from_str::<State>(&json).unwrap(), state);
    let event = Event {
        clock: 43,
        chicken: 1,
        behaviour: flock.hens()[1].behaviour,
    };
    assert_eq!(
        serde_json::from_value::<Event>(serde_json::to_value(&event).unwrap()).unwrap(),
        event
    );
    assert_eq!(
        serde_json::from_str::<Intent>(r#"{"notice": 2, "toward": [1.0, -2.0]}"#).unwrap(),
        Intent::Notice {
            notice: 2,
            toward: [1.0, -2.0]
        }
    );
    assert_eq!(
        serde_json::from_str::<Intent>(r#"{"away": 2}"#).unwrap(),
        Intent::Away { away: 2 }
    );
    assert!(serde_json::from_str::<Intent>(r#"{"peck": 2}"#).is_err());
}
