// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

use glam::Vec3;
use tomori_assets::animation::{Animator, Value};
use tomori_assets::{Embedded, Limits, Mesh, Node, Primitive, Transform};

use super::*;

/// The chicken, loaded as the server and clients load it.
pub(crate) fn chicken() -> Chicken {
    Chicken::load(Library::shared()).unwrap()
}

/// Where each vertex of the mesh of node `name` is, in the model's space, at `time` seconds into `animation`.
fn skinned(model: &Model, name: &str, animation: usize, time: f32) -> Vec<Vec3> {
    let node = &model.nodes[model.node(name).unwrap()];
    let globals = Pose::sampled(model, animation, time).globals(model);
    let joints = joint_matrices(model, node.skin.unwrap(), &globals);
    model.meshes[node.mesh.unwrap()]
        .primitives
        .iter()
        .flat_map(|primitive| deformed(primitive, &joints, Mat4::IDENTITY))
        .collect()
}

#[test]
fn the_chicken_moves_as_blender_moves_it() {
    // A few of each mesh's vertices, as Blender deforms them, at a few frames of each animation, in glTF's axes, written
    // by the export script beside the `.blend`s, in tenths of a millimetre.
    let reference: serde_json::Value =
        serde_json::from_str(include_str!("../../assets/source/chicken-reference.json")).unwrap();
    let scale = reference["scale"].as_f64().unwrap() as f32;
    let chicken = chicken();
    let model = &chicken.model;
    let mut compared = 0;
    let mut worst = 0.0f32;
    for (animation, frames) in reference["animations"].as_object().unwrap() {
        let index = model.animation(animation).unwrap();
        let mut frames: Vec<(u32, &serde_json::Value)> = frames
            .as_object()
            .unwrap()
            .iter()
            .map(|(frame, meshes)| (frame.parse().unwrap(), meshes))
            .collect();
        frames.sort_by_key(|(frame, _)| *frame);
        for mesh in ["chicken", "cone"] {
            let blender = |frame: &serde_json::Value| -> Vec<Vec3> {
                frame[mesh]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|point| {
                        let point: Vec<f32> = point
                            .as_array()
                            .unwrap()
                            .iter()
                            .map(|c| c.as_f64().unwrap() as f32 / scale)
                            .collect();
                        Vec3::new(point[0], point[1], point[2])
                    })
                    .collect()
            };
            // Match each of Blender's vertices to the nearest of the runtime's at the first frame, as the export splits
            // vertices along seams, then follow the match through every frame.
            let (first, _) = frames[0];
            let at = |frame: u32| skinned(model, mesh, index, frame as f32 / FPS);
            let ours = at(first);
            let matched: Vec<usize> = blender(frames[0].1)
                .iter()
                .map(|point| {
                    (0..ours.len())
                        .min_by(|a, b| {
                            ours[*a]
                                .distance(*point)
                                .total_cmp(&ours[*b].distance(*point))
                        })
                        .unwrap()
                })
                .collect();
            for (frame, meshes) in &frames {
                let ours = at(*frame);
                for (point, index) in blender(meshes).iter().zip(&matched) {
                    worst = worst.max(ours[*index].distance(*point));
                    compared += 1;
                }
            }
        }
    }
    assert!(compared > 1000, "{compared}");
    // Within a millimetre of Blender's, in its own units, a chicken some 6 of them tall.
    assert!(
        worst < 0.001,
        "the runtime's skinning is {worst} from Blender's"
    );
}

#[test]
fn the_chicken_stands_on_the_origin_at_its_height_and_moves_as_its_feet_carry_it() {
    let chicken = chicken();
    let (start, _) = seconds(WALK);
    let points = skinned(
        &chicken.model,
        "chicken",
        chicken.model.animation("walk").unwrap(),
        start,
    );
    let placed: Vec<Vec3> = points
        .iter()
        .map(|point| chicken.placement.transform_point3(*point))
        .collect();
    let lowest = placed
        .iter()
        .map(|point| point.y)
        .fold(f32::INFINITY, f32::min);
    let tallest = placed
        .iter()
        .map(|point| point.y)
        .fold(f32::NEG_INFINITY, f32::max);
    assert!(lowest.abs() < 1e-4, "{lowest}");
    assert!(tallest < HEIGHT && tallest > HEIGHT * 0.7, "{tallest}");
    // Its walk carries it a few centimetres a loop, unevenly, and its sprint about a metre and a quarter, as the first
    // chickens' bake measured; a test keeps them so, so that a new export that changes them is noticed.
    assert!(
        (0.06..0.1).contains(&chicken.stride()),
        "{}",
        chicken.stride()
    );
    assert!((chicken.loop_duration() - 1.5).abs() < 1e-5);
    assert!(
        (1.15..1.35).contains(&chicken.sprint.total()),
        "{}",
        chicken.sprint.total()
    );
    let pace: Vec<f32> = chicken
        .walk
        .distances
        .windows(2)
        .map(|pair| pair[1] - pair[0])
        .collect();
    assert!(
        pace.iter().any(|step| *step < 0.0),
        "it rocks back a little as a foot comes down"
    );
    // Loaded once, and shared.
    let shared = Chicken::shared().unwrap();
    assert!(Arc::ptr_eq(&shared, &Chicken::shared().unwrap()));
    assert_eq!(asset().name(), "chicken.glb");
    assert!(asset().matches(include_bytes!("../../assets/models/chicken.glb")));
}

#[test]
fn its_animator_walks_tilts_its_head_and_sprints() {
    let chicken = chicken();
    let mut animator = Animator::new(chicken.graph(1.0), 0.0);
    assert_eq!(animator.state(), "stand");
    animator.set("walking", Value::Bool(true), 1.0);
    let walked = animator.advance(4.0);
    assert_eq!(animator.state(), "walk");
    // A step with each foot, each loop.
    assert_eq!(
        walked
            .events
            .iter()
            .filter(|event| event.name == STEP)
            .count(),
        4
    );
    assert!((walked.moved - 2.0 * f64::from(chicken.stride())).abs() < 1e-3);
    animator.set("confused", Value::Bool(true), 4.0);
    let confused = animator.advance(6.0);
    assert!(confused.events.iter().any(|event| event.name == CLUCK));
    assert_eq!(animator.state(), "hold");
    animator.set("walking", Value::Bool(false), 6.0);
    animator.set("confused", Value::Bool(false), 6.0);
    animator.advance(9.0);
    assert_eq!(animator.state(), "stand");
    animator.set("startle", Value::Trigger(true), 9.0);
    let sprinted = animator.advance(10.0);
    assert!(sprinted.events.iter().any(|event| event.name == SQUAWK));
    assert_eq!(animator.state(), "sprint");
    animator.advance(13.0);
    assert_eq!(animator.state(), "stand");
    // A quicker chicken's walk plays quicker.
    let quick = chicken.graph(1.2);
    assert_eq!(quick.clip("walk").unwrap().speed, 1.2);
    assert!(animator.pose(&chicken.model).local(0).rotation.is_finite());
}

#[test]
fn a_model_that_is_not_the_chicken_is_refused() {
    let refused = |model: Model| Chicken::new(Arc::new(model)).unwrap_err();
    let triangle = || {
        Mesh::new(
            "chicken",
            vec![Primitive::new(
                vec![Vec3::ZERO, Vec3::X, Vec3::Y],
                Vec::new(),
                vec![0, 1, 2],
            )],
        )
    };
    let built = |nodes: Vec<Node>| {
        Model::built(
            nodes,
            vec![0],
            vec![triangle()],
            Vec::new(),
            &Limits::default(),
        )
        .unwrap()
    };
    assert_eq!(
        refused(Model::default()),
        "the chicken has no walk animation"
    );
    // Its own, but for what each check needs.
    let full = (*chicken().model).clone();
    let without = |change: &dyn Fn(&mut Model)| {
        let mut model = full.clone();
        change(&mut model);
        refused(model)
    };
    assert_eq!(
        without(&|model| model.animations[1].name = None),
        "the chicken has no confused animation"
    );
    assert_eq!(
        without(&|model| model.animations[2].name = None),
        "the chicken has no sprint animation"
    );
    for name in FEET {
        let foot = full.node(name).unwrap();
        assert_eq!(
            without(&|model| model.nodes[foot].name = None),
            format!("the chicken has no foot {name}")
        );
    }
    let body = full.node("chicken").unwrap();
    assert_eq!(
        without(&|model| model.nodes[body].name = None),
        "the chicken has no body"
    );
    assert_eq!(
        without(&|model| model.nodes[body].skin = None),
        "the chicken's body is not skinned"
    );
    assert_eq!(
        without(&|model| model.nodes[body].mesh = None),
        "the chicken has no mesh"
    );
    let _ = built(vec![Node::new("x", Transform::IDENTITY).with_mesh(0)]);
    // A library without it cannot load it, and what cannot be loaded is not shared; one with it embedded can.
    assert!(Chicken::kept(Chicken::load(&Library::default())).is_none());
    let library = Library::default();
    library.add(Embedded::new().with(include_bytes!("../../assets/models/chicken.glb")));
    assert!(Chicken::new(library.model(&asset()).unwrap()).is_ok());
}
