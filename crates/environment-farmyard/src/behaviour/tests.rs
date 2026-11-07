// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

use glam::{Vec2, Vec3};

use super::*;
use crate::chicken::tests::chicken;

fn at(doing: Doing) -> Behaviour {
    Behaviour {
        since: 1_000,
        at: [0.0, 0.0, -4.0],
        heading: 0.0,
        doing,
    }
}

#[test]
fn headings_and_turns_are_the_short_way_round() {
    assert!((heading(Vec2::Y) - 0.0).abs() < 1e-6);
    assert!((heading(Vec2::X) - std::f32::consts::FRAC_PI_2).abs() < 1e-6);
    assert!(facing(std::f32::consts::FRAC_PI_2).distance(Vec2::X) < 1e-6);
    assert!((turn_between(3.0, -3.0) - (std::f32::consts::TAU - 6.0)).abs() < 1e-5);
    assert!((turned(0.0, 1.0, 1.0, 0.25) - 0.25).abs() < 1e-6);
    assert!((turned(0.0, 1.0, 1.0, 9.0) - 1.0).abs() < 1e-6);
}

#[test]
fn a_walk_turns_then_goes_as_far_as_its_feet_carry_it_and_stops_with_them_down() {
    let chicken = chicken();
    let walk = at(Doing::Walk {
        to: [1.0, 0.0, -4.0],
    });
    let planned = walk.walk(&chicken, 1.0).unwrap();
    // A quarter turn first, at its rate.
    assert_eq!(
        planned.walks,
        1_000 + (std::f32::consts::FRAC_PI_2 / TURN * 1000.0).round() as u64
    );
    // It goes at least as far as asked, by whole steps, and no further than a step beyond.
    assert!(planned.distance >= 0.999 && planned.distance < 1.0 + chicken.stride());
    assert_eq!(walk.ends(&chicken, 1.0), Some(planned.stops));
    // Turning, it stays put; walking, it moves along; stopped, it is where its steps took it.
    let turning = walk.place(&chicken, 1.0, 1_200);
    assert_eq!(turning.position, Vec3::new(0.0, 0.0, -4.0));
    assert!(turning.heading > 0.0 && turning.heading < std::f32::consts::FRAC_PI_2);
    let midway = walk.place(&chicken, 1.0, (planned.walks + planned.stops) / 2);
    assert!(midway.position.x > 0.2 && midway.position.x < 0.8);
    let stopped = walk.place(&chicken, 1.0, planned.stops + 5_000);
    assert!((stopped.position.x - planned.distance).abs() < 1e-4);
    // A faster chicken stops sooner, as far.
    let quick = walk.walk(&chicken, 1.2).unwrap();
    assert!(quick.stops < planned.stops && quick.distance == planned.distance);
    // Walking nowhere, it stands where it is, facing as it did.
    let nowhere = at(Doing::Walk {
        to: [0.0, 0.0, -4.0],
    })
    .walk(&chicken, 1.0)
    .unwrap();
    assert_eq!(
        (nowhere.walks, nowhere.stops, nowhere.distance),
        (1_000, 1_000, 0.0)
    );
    // Up a ramp, it rises as it goes.
    let up = at(Doing::Walk {
        to: [0.0, 0.6, -5.6],
    });
    let climbing = up.place(&chicken, 1.0, up.ends(&chicken, 1.0).unwrap());
    assert!(climbing.position.y > 0.55);
    // Something else is no walk.
    assert_eq!(at(Doing::Stand).walk(&chicken, 1.0), None);
}

#[test]
fn standing_pecking_looking_and_being_confused_keep_it_where_it_is() {
    let chicken = chicken();
    for doing in [Doing::Stand, Doing::Roost, Doing::Recover] {
        let placed = at(doing).place(&chicken, 1.0, 5_000);
        assert_eq!(
            (placed.position, placed.heading, placed.lean),
            (Vec3::new(0.0, 0.0, -4.0), 0.0, 0.0)
        );
    }
    assert_eq!(at(Doing::Stand).ends(&chicken, 1.0), None);
    assert_eq!(
        at(Doing::Recover).ends(&chicken, 1.0),
        Some(1_000 + RECOVERING)
    );
    // Pecking, it leans down and up, a peck at a time, and stands straight once it has finished.
    let peck = at(Doing::Peck { until: 3_100 });
    assert_eq!(peck.ends(&chicken, 1.0), Some(3_100));
    assert!(peck.place(&chicken, 1.0, 1_000 + PECK_MILLIS / 4).lean > PECK * 0.9);
    assert_eq!(
        peck.place(&chicken, 1.0, 1_000 + PECK_MILLIS * 3 / 4).lean,
        0.0
    );
    // Looking, it turns to the side and back.
    let look = at(Doing::Look {
        side: LOOK,
        until: 3_000,
    });
    assert!((look.place(&chicken, 1.0, 2_000).heading - LOOK).abs() < 1e-4);
    assert!(look.place(&chicken, 1.0, 3_000).heading.abs() < 1e-4);
    // Confused, it turns to whoever points.
    let confused = at(Doing::Confused {
        toward: [5.0, -4.0],
    });
    assert!(
        (confused.place(&chicken, 1.0, 9_000).heading - std::f32::consts::FRAC_PI_2).abs() < 1e-4
    );
    assert_eq!(confused.ends(&chicken, 1.0), None);
}

#[test]
fn a_sprint_spins_round_and_runs_as_far_as_its_feet_carry_it() {
    let chicken = chicken();
    let sprint = at(Doing::Sprint { away: [0.0, 1.0] });
    let ends = sprint.ends(&chicken, 1.0).unwrap();
    assert_eq!(ends, 1_000 + 2_958);
    let done = sprint.place(&chicken, 1.0, ends + 1_000);
    assert!((done.position.z - (-4.0 + chicken.sprint.total())).abs() < 1e-4);
    assert!(chicken.sprint.total() > 1.0);
    // Facing the other way, it spins round first.
    let round = Behaviour {
        heading: std::f32::consts::PI,
        ..sprint
    };
    assert!(round.place(&chicken, 1.0, 1_100).heading.abs() > 1.0);
    assert!(
        round.place(&chicken, 1.0, 2_000).heading.abs() < 1e-4
            || (round.place(&chicken, 1.0, 2_000).heading.abs() - std::f32::consts::TAU).abs()
                < 1e-4
    );
}

#[test]
fn a_behaviour_is_sent_as_what_it_is_doing_beside_where() {
    let behaviour = at(Doing::Walk {
        to: [1.0, 0.0, -4.0],
    });
    let json = serde_json::to_value(behaviour).unwrap();
    assert_eq!(json["doing"], "walk");
    assert_eq!(json["to"], serde_json::json!([1.0, 0.0, -4.0]));
    assert_eq!(
        serde_json::from_value::<Behaviour>(json).unwrap(),
        behaviour
    );
}
