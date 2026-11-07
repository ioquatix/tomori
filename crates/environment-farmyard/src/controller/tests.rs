// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

use glam::Vec2;

use crate::behaviour::facing;

use super::*;
use crate::chicken::{CLUCK, SQUAWK, STEP};

fn hen(cone: bool, doing: Doing) -> Hen {
    Hen {
        name: "Pip".into(),
        cone,
        pace: 1.0,
        behaviour: Behaviour {
            since: 1_000,
            at: [0.0, 0.0, -4.0],
            heading: 0.0,
            doing,
        },
    }
}

fn controller(cone: bool, doing: Doing) -> ChickenController {
    ChickenController::new(
        Arc::new(crate::chicken::tests::chicken()),
        &hen(cone, doing),
    )
}

#[test]
fn a_chicken_wears_its_cone_only_if_it_has_one() {
    for cone in [true, false] {
        let controller = controller(cone, Doing::Stand);
        let node = controller.instance().model().node(CONE).unwrap();
        assert_eq!(controller.instance().shown(node), cone);
    }
}

#[test]
fn a_walking_chicken_steps_where_its_behaviour_puts_it() {
    let mut controller = controller(
        false,
        Doing::Walk {
            to: [0.0, 0.0, -3.0],
        },
    );
    let chicken = crate::chicken::tests::chicken();
    let walk = controller.behaviour().walk(&chicken, 1.0).unwrap();
    // Before it begins, it stands where it was.
    assert!(controller.update(900).is_empty());
    assert_eq!(controller.placed().position, Vec3::new(0.0, 0.0, -4.0));
    // Walking, it steps, and is where its behaviour says.
    let steps = controller.update(walk.stops - 10);
    assert!(steps.iter().any(|event| event.name == STEP));
    let placed = controller.placed();
    let expected = controller.behaviour().place(&chicken, 1.0, walk.stops - 10);
    assert_eq!(placed, expected);
    assert!(placed.position.z > -4.0);
    // Its head is above and ahead of where it stands, and its body about it, facing its way.
    let ahead = facing(placed.heading);
    let head = controller.head() - placed.position;
    assert!(head.y > 0.25 && Vec2::new(head.x, head.z).dot(ahead) > 0.0);
    let (centre, radius) = controller.body();
    assert!(centre.distance(placed.position) < radius + 0.05);
    // Drawn where it stands, on its feet.
    let feet = chicken.placement.inverse().transform_point3(Vec3::ZERO);
    let origin = controller.instance().transform.transform_point3(feet);
    assert!(
        origin.distance(placed.position) < 1e-3,
        "{origin} {}",
        placed.position
    );
    // Stopped, it stands.
    controller.update(walk.stops + 2_000);
    assert!(
        controller
            .placed()
            .position
            .distance(Vec3::new(0.0, 0.0, -4.0 + walk.distance))
            < 1e-3
    );
}

#[test]
fn a_chicken_pointed_at_clucks_and_a_startled_one_squawks() {
    let mut controller = controller(true, Doing::Stand);
    controller.update(2_000);
    controller.behave(Behaviour {
        since: 2_000,
        at: [0.0, 0.0, -4.0],
        heading: 0.0,
        doing: Doing::Confused { toward: [1.0, 0.0] },
    });
    let confused = controller.update(3_000);
    assert!(confused.iter().any(|event| event.name == CLUCK));
    // Turned towards whoever points.
    assert!(controller.placed().heading > 0.0);
    for doing in [
        Doing::Recover,
        Doing::Peck { until: 9_000 },
        Doing::Look {
            side: 0.3,
            until: 9_000,
        },
        Doing::Roost,
    ] {
        controller.behave(Behaviour {
            since: 6_000,
            at: [0.0, 0.0, -4.0],
            heading: 0.0,
            doing,
        });
    }
    controller.update(8_000);
    controller.behave(Behaviour {
        since: 9_000,
        at: [0.0, 0.0, -4.0],
        heading: 0.0,
        doing: Doing::Sprint { away: [0.0, -1.0] },
    });
    let sprinted = controller.update(10_000);
    assert!(sprinted.iter().any(|event| event.name == SQUAWK));
    assert!(controller.placed().position.z < -4.0);
    // Pecking leans it forward.
    let mut pecking = self::controller(false, Doing::Peck { until: 5_000 });
    pecking.update(1_000 + crate::behaviour::PECK_MILLIS / 4);
    assert!(pecking.placed().lean > 0.0);
}
