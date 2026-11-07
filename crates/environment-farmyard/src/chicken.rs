// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

//! The chicken, as its author modelled and animated it: its asset, loaded at runtime, the clips the farmyard plays of its
//! three animations, how far its walk and sprint carry it, measured from its planted feet, and the animator graph that
//! blends them. The server side and every client load it alike, so that they agree on how far a chicken goes and when.
use std::sync::{Arc, Once};

use glam::{Mat4, Vec3};
use tomori_assets::animation::{Clip, Condition, Graph, RootMotion};
use tomori_assets::{AssetRef, Digest, Error, Library, Model, Pose, deformed, joint_matrices};

use crate::{ID, VERSION};

/// The SHA-256 of `assets/models/chicken.glb`, which names it: exporting it again must give the same bytes, or this
/// changes with it, and with it the farmyard's version, if what the server and clients agree on changes.
pub const DIGEST: &str = "eb491ea326546b7c60db46c88cff9d1abf544a41e01eda0ac09de823e2d6515b";

/// The chicken's asset.
pub fn asset() -> AssetRef {
    AssetRef::new(
        ID,
        VERSION,
        "chicken.glb",
        Digest::parse(DIGEST).expect("a digest"),
    )
}

/// The chicken's bytes, embedded in the program everywhere but on Android, whose APK carries them in its `assets`
/// folder instead. See Tomori's `design/assets.md`.
#[cfg(not(target_os = "android"))]
const EMBEDDED: &[u8] = include_bytes!("../assets/models/chicken.glb");

/// Add the farmyard's embedded assets to the program's library, once.
pub fn bundle() {
    static BUNDLED: Once = Once::new();
    BUNDLED.call_once(|| {
        #[cfg(not(target_os = "android"))]
        Library::shared().add(tomori_assets::Embedded::new().with(EMBEDDED));
    });
}

/// How tall a chicken stands at the top of its walk, cone and all, in metres.
pub const HEIGHT: f32 = 0.4;
/// The rate its animations were made at, in frames a second, and so where frame `n` of an action is in seconds.
pub const FPS: f32 = 24.0;
/// The walk's loop: frames 37 to 73 of the walk, both feet down at its start as the right takes the weight, and again
/// halfway, at frame 55, as the left does. The whole action starts from standing and stops, and repeats only from frame 27
/// to frame 126.
pub const WALK: (f32, f32) = (37.0, 73.0);
/// The confused head tilt, frames 1 to 90: it has tilted its head by frame 24, holds it to frame 57, and has straightened
/// it again by frame 90, standing still throughout.
pub const TILT: (f32, f32) = (1.0, 24.0);
pub const HOLD: (f32, f32) = (24.0, 57.0);
pub const STRAIGHTEN: (f32, f32) = (57.0, 90.0);
/// The startled sprint, frames 1 to 72: it crouches, scrambles off, and slows to a stop, standing as it began.
pub const SPRINT: (f32, f32) = (1.0, 72.0);
/// Its feet, the last bones of its legs, right then left, as its author named them.
pub const FEET: [&str; 2] = ["Bone.012", "Bone.011"];
/// The mesh of the traffic cone it may wear, apart from the chicken's own.
pub const CONE: &str = "cone";

/// Seconds of an action, from frames of it.
pub fn seconds((from, to): (f32, f32)) -> (f32, f32) {
    (from / FPS, to / FPS)
}

/// The chicken, ready to animate: its model, how it is placed to stand on the origin, its feet about it, at its height
/// and facing +Z, the root motion of its walk and sprint, in metres, and its animator graph.
#[derive(Clone, Debug)]
pub struct Chicken {
    pub model: Arc<Model>,
    /// Takes the model's space to the chicken's: standing on the origin, [`HEIGHT`] tall.
    pub placement: Mat4,
    pub walk: RootMotion,
    pub sprint: RootMotion,
    /// Its animations: its walk, its confused head tilt and its sprint.
    animations: [usize; 3],
}

/// The index of the animation `name` of `model`, or why the chicken lacks it.
fn animation(model: &Model, name: &str) -> Result<usize, String> {
    model
        .animation(name)
        .ok_or_else(|| format!("the chicken has no {name} animation"))
}

impl Chicken {
    /// The chicken, loaded from `library`, as the server and every client load it.
    pub fn load(library: &Library) -> Result<Self, Error> {
        bundle();
        let model = library.model(&asset())?;
        Ok(Self::new(model).expect("the chicken's asset is the one this was written for"))
    }

    /// The chicken of `model`, or what it lacks.
    pub fn new(model: Arc<Model>) -> Result<Self, String> {
        let walk = animation(&model, "walk")?;
        let confused = animation(&model, "confused")?;
        let sprint = animation(&model, "sprint")?;
        let feet = FEET.map(|foot| {
            model
                .node(foot)
                .ok_or_else(|| format!("the chicken has no foot {foot}"))
        });
        let feet = [feet[0].clone()?, feet[1].clone()?];
        let body = model.node("chicken").ok_or("the chicken has no body")?;
        let skin = model.nodes[body]
            .skin
            .ok_or("the chicken's body is not skinned")?;
        let mesh = model.nodes[body].mesh.ok_or("the chicken has no mesh")?;
        // Stand it on the origin from the walk's first pose, where it stops: its lowest point is the ground, and its feet
        // are centred; it bobs as it walks, so it is its height at the top of its walk.
        let (start, end) = seconds(WALK);
        // Cone and all.
        let cone = model.node(CONE).and_then(|cone| model.nodes[cone].mesh);
        let skinned = |time: f32, meshes: &[usize]| {
            let globals = Pose::sampled(&model, walk, time).globals(&model);
            let joints = joint_matrices(&model, skin, &globals);
            meshes
                .iter()
                .flat_map(|mesh| &model.meshes[*mesh].primitives)
                .flat_map(|primitive| deformed(primitive, &joints, Mat4::IDENTITY))
                .collect::<Vec<Vec3>>()
        };
        let whole: Vec<usize> = std::iter::once(mesh).chain(cone).collect();
        let standing = skinned(start, &[mesh]);
        let ground = standing
            .iter()
            .map(|point| point.y)
            .fold(f32::INFINITY, f32::min);
        let top = (0..36)
            .flat_map(|frame| skinned(start + frame as f32 / FPS, &whole))
            .map(|point| point.y)
            .fold(f32::NEG_INFINITY, f32::max);
        let globals = Pose::sampled(&model, walk, start).globals(&model);
        let centre = feet
            .iter()
            .map(|foot| globals[*foot].w_axis.truncate())
            .sum::<Vec3>()
            / 2.0;
        let scale = HEIGHT / (top - ground);
        let placement = Mat4::from_scale(Vec3::splat(scale))
            * Mat4::from_translation(-Vec3::new(centre.x, ground, centre.z));
        let metres = |motion: RootMotion| {
            RootMotion::new(
                motion.step,
                motion
                    .distances
                    .iter()
                    .map(|distance| distance * scale)
                    .collect(),
            )
        };
        // Each foot's sole: its lowest point as it stands, of the points of the mesh that move with that foot above all,
        // in the foot's own space.
        let height = top - ground;
        let primitives = &model.meshes[mesh].primitives;
        let soles = feet.map(|foot| {
            let joint = model.skins[skin]
                .joints
                .iter()
                .position(|joint| *joint == foot);
            let lowest = primitives
                .iter()
                .flat_map(|primitive| primitive.joints.iter().zip(&primitive.weights))
                .zip(&standing)
                .filter(|((joints, weights), _)| {
                    let most = weights.iter().copied().fold(0.0, f32::max);
                    let strongest = weights.iter().position(|weight| *weight == most);
                    strongest.map(|strongest| usize::from(joints[strongest])) == joint
                })
                .map(|(_, point)| *point)
                .fold(Vec3::splat(f32::INFINITY), |lowest, point| {
                    if point.y < lowest.y { point } else { lowest }
                });
            (foot, globals[foot].inverse().transform_point3(lowest))
        });
        let walk_motion = metres(RootMotion::planted(
            &model,
            walk,
            (start, end),
            &soles,
            Vec3::Z,
            FPS,
            height * 0.02,
        ));
        let sprint_motion = metres(RootMotion::planted(
            &model,
            sprint,
            seconds(SPRINT),
            &soles,
            Vec3::Z,
            FPS,
            f32::INFINITY,
        ));
        Ok(Self {
            model,
            placement,
            walk: walk_motion,
            sprint: sprint_motion,
            animations: [walk, confused, sprint],
        })
    }

    /// The chicken, loaded from the program's library once, shared by the server side and every client side in the
    /// program, or `None` if it cannot be loaded, which is logged once.
    pub fn shared() -> Option<Arc<Self>> {
        static SHARED: std::sync::OnceLock<Option<Arc<Chicken>>> = std::sync::OnceLock::new();
        SHARED
            .get_or_init(|| Self::kept(Self::load(Library::shared())))
            .clone()
    }

    /// The chicken `loaded`, to share, or `None`, saying why in the log.
    fn kept(loaded: Result<Self, Error>) -> Option<Arc<Self>> {
        match loaded {
            Ok(chicken) => Some(Arc::new(chicken)),
            Err(error) => {
                log::warn!("The farmyard's chicken could not be loaded: {error}");
                None
            }
        }
    }

    /// Its animator's graph, for a chicken that walks at `pace` times its walk's own speed.
    pub fn graph(&self, pace: f32) -> Arc<Graph> {
        let [walk, confused, sprint] = self.animations;
        graph((walk, confused, sprint), pace, &self.walk, &self.sprint)
    }

    /// How far one loop of its walk carries it, in metres: a step with each foot.
    pub fn stride(&self) -> f32 {
        self.walk.total()
    }

    /// How long one loop of its walk takes, in seconds, as it was animated.
    pub fn loop_duration(&self) -> f32 {
        let (start, end) = seconds(WALK);
        end - start
    }
}

/// The walk's events: each foot coming down, at the start of its loop and halfway.
pub const STEP: &str = "step";
/// The cue for a cluck, as a confused chicken first looks up, and for a squawk, as a startled one springs off.
pub const CLUCK: &str = "cluck";
pub const SQUAWK: &str = "squawk";

/// The states of the chicken's animator, and its parameters: `walking` while it walks, `confused` while someone points at
/// it, and `startle`, which makes it sprint, once.
pub fn graph(
    (walk, confused, sprint): (usize, usize, usize),
    pace: f32,
    walk_motion: &RootMotion,
    sprint_motion: &RootMotion,
) -> Arc<Graph> {
    let span = |animation: usize, frames: (f32, f32)| {
        let (start, end) = seconds(frames);
        Clip::new(animation, start, end)
    };
    let (walk_start, walk_end) = seconds(WALK);
    let half = (walk_end - walk_start) / 2.0;
    Arc::new(
        Graph::builder()
            // Standing is the walk's first pose, both feet down, held.
            .state(
                "stand",
                Clip::new(walk, walk_start, walk_start + 0.001).looping(),
            )
            .state(
                "walk",
                span(walk, WALK)
                    .looping()
                    .at_speed(pace)
                    .with_event(0.0, STEP)
                    .with_event(half, STEP)
                    .with_motion(walk_motion.clone()),
            )
            .state("tilt", span(confused, TILT).with_event(0.08, CLUCK))
            .state("hold", span(confused, HOLD))
            .state("straighten", span(confused, STRAIGHTEN))
            .state(
                "sprint",
                span(sprint, SPRINT)
                    .with_event(0.0, SQUAWK)
                    .with_motion(sprint_motion.clone()),
            )
            .bool("walking", false)
            .bool("confused", false)
            .trigger("startle")
            .from_any("sprint", [Condition::Triggered("startle")], 0.15)
            .transition("stand", "walk", [Condition::Is("walking", true)], 0.15)
            .transition("walk", "stand", [Condition::Is("walking", false)], 0.15)
            .transition("stand", "tilt", [Condition::Is("confused", true)], 0.25)
            .transition("walk", "tilt", [Condition::Is("confused", true)], 0.25)
            .transition(
                "tilt",
                "straighten",
                [Condition::Is("confused", false)],
                0.15,
            )
            .transition("tilt", "hold", [Condition::Finished], 0.0)
            .transition(
                "hold",
                "straighten",
                [Condition::Is("confused", false)],
                0.15,
            )
            // Held, it sways back to the start of its hold, a little unsteadily.
            .transition("hold", "hold", [Condition::Finished], 0.4)
            .transition("straighten", "stand", [Condition::Finished], 0.2)
            .transition("sprint", "stand", [Condition::Finished], 0.25)
            .build()
            .expect("the chicken's graph is well formed"),
    )
}

#[cfg(test)]
pub(crate) mod tests;
