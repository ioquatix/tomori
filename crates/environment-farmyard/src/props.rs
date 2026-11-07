// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

//! The yard, built in code as one model: the meadow, the packed earth inside the fence, the fence and its gate posts,
//! the water trough, the feeder and its scattered grain, the hay bales, the trees beyond, and the lantern on the gate
//! post, whose glass glows. The coop is a model of its own (see [`crate::coop`]).
use glam::{Quat, Vec3, Vec4};
use tomori_assets::{Limits, Material, Mesh, Model, Node, Transform};

use crate::layout;
use crate::shapes::Shapes;

/// The farmyard's palette, linear RGB, for an evening under the moon.
pub mod palette {
    use glam::Vec3;

    pub const GRASS: Vec3 = Vec3::new(0.10, 0.17, 0.07);
    pub const EARTH: Vec3 = Vec3::new(0.26, 0.17, 0.10);
    pub const WOOD: Vec3 = Vec3::new(0.34, 0.22, 0.13);
    pub const DARK_WOOD: Vec3 = Vec3::new(0.20, 0.12, 0.07);
    pub const WEATHERED: Vec3 = Vec3::new(0.42, 0.36, 0.30);
    pub const RED: Vec3 = Vec3::new(0.42, 0.09, 0.06);
    pub const ROOF: Vec3 = Vec3::new(0.23, 0.13, 0.11);
    pub const STRAW: Vec3 = Vec3::new(0.62, 0.48, 0.20);
    pub const GRAIN: Vec3 = Vec3::new(0.70, 0.55, 0.25);
    pub const WATER: Vec3 = Vec3::new(0.10, 0.16, 0.22);
    pub const METAL: Vec3 = Vec3::new(0.30, 0.31, 0.33);
    pub const PINE: Vec3 = Vec3::new(0.05, 0.11, 0.07);
    pub const BARK: Vec3 = Vec3::new(0.15, 0.10, 0.07);
    pub const SHADOW: Vec3 = Vec3::new(0.03, 0.025, 0.02);
    /// A lantern's warm glow, which its glass gives off.
    pub const GLOW: Vec3 = Vec3::new(1.0, 0.62, 0.28);
}

/// The material most of the yard is drawn with, white, so that its faces' own colours show.
pub const PLAIN: usize = 0;
/// The material of a lantern's glass, which glows.
pub const GLASS: usize = 1;

/// The materials of the farmyard's models: [`PLAIN`] and [`GLASS`].
pub fn materials() -> Vec<Material> {
    vec![
        Material::new("plain", Vec4::ONE),
        Material::new("glass", Vec4::new(1.0, 0.85, 0.6, 1.0)).with_emissive(palette::GLOW * 1.6),
    ]
}

/// A deterministic number from 0 up to 1, for scattering things alike everywhere.
pub fn scatter(seed: u32) -> f32 {
    let mut hash = seed.wrapping_mul(0x2c1b_3c6d) ^ 0x297a_2d39;
    hash ^= hash >> 13;
    hash = hash.wrapping_mul(0x7fb5_d329);
    hash ^= hash >> 16;
    (hash & 0xffff) as f32 / 65536.0
}

/// A lantern hung at `at`: its cap and base, and its glass, which glows, into `glass`.
pub fn lantern(shapes: &mut Shapes, glass: &mut Shapes, at: Vec3) {
    shapes.block(
        at + Vec3::new(-0.09, 0.11, -0.09),
        at + Vec3::new(0.09, 0.15, 0.09),
        palette::METAL,
    );
    shapes.block(
        at + Vec3::new(-0.08, -0.14, -0.08),
        at + Vec3::new(0.08, -0.11, 0.08),
        palette::METAL,
    );
    glass.block(
        at + Vec3::new(-0.065, -0.11, -0.065),
        at + Vec3::new(0.065, 0.11, 0.065),
        palette::GLOW,
    );
}

fn fence_around(shapes: &mut Shapes) {
    let fence = layout::FENCE;
    let height = layout::FENCE_HEIGHT;
    let corners = [
        Vec3::new(fence.low.x, 0.0, fence.high.y),
        Vec3::new(fence.low.x, 0.0, fence.low.y),
        Vec3::new(fence.high.x, 0.0, fence.low.y),
        Vec3::new(fence.high.x, 0.0, fence.high.y),
    ];
    // Three sides, then the front, either side of the gate.
    let mut runs = vec![
        (corners[0], corners[1]),
        (corners[1], corners[2]),
        (corners[2], corners[3]),
    ];
    runs.push((corners[3], Vec3::new(layout::GATE[1], 0.0, fence.high.y)));
    runs.push((Vec3::new(layout::GATE[0], 0.0, fence.high.y), corners[0]));
    for (index, (from, to)) in runs.into_iter().enumerate() {
        let posts = ((to - from).length() / 1.6).ceil().max(1.0) as u32;
        for post in 0..=posts {
            let at = from.lerp(to, post as f32 / posts as f32);
            let lean = (scatter(index as u32 * 97 + post) - 0.5) * 0.06;
            shapes.cuboid(
                at + Vec3::Y * height * 0.5,
                Vec3::new(0.06, height * 0.5, 0.06),
                Quat::from_rotation_z(lean),
                palette::WEATHERED * (0.9 + scatter(post) * 0.2),
            );
        }
        for rail in [0.38, 0.8] {
            shapes.beam(
                from + Vec3::Y * rail,
                to + Vec3::Y * rail,
                0.07,
                palette::WEATHERED,
            );
        }
    }
    // The gate's posts, taller, and the lantern's arm.
    for x in layout::GATE {
        shapes.block(
            Vec3::new(x - 0.08, 0.0, fence.high.y - 0.08),
            Vec3::new(x + 0.08, height * 1.35, fence.high.y + 0.08),
            palette::DARK_WOOD,
        );
    }
    let arm = Vec3::new(layout::GATE[1], height * 1.3, fence.high.y);
    shapes.beam(
        arm,
        Vec3::new(layout::GATE_LANTERN.x, arm.y, arm.z),
        0.04,
        palette::DARK_WOOD,
    );
}

fn trough(shapes: &mut Shapes) {
    let trough = layout::TROUGH;
    let top = layout::TROUGH_HEIGHT;
    let wall = 0.05;
    let (low, high) = (trough.low, trough.high);
    shapes.block(
        Vec3::new(low.x, 0.08, low.y),
        Vec3::new(high.x, 0.13, high.y),
        palette::DARK_WOOD,
    );
    shapes.block(
        Vec3::new(low.x, 0.13, low.y),
        Vec3::new(low.x + wall, top, high.y),
        palette::WOOD,
    );
    shapes.block(
        Vec3::new(high.x - wall, 0.13, low.y),
        Vec3::new(high.x, top, high.y),
        palette::WOOD,
    );
    shapes.block(
        Vec3::new(low.x, 0.13, low.y),
        Vec3::new(high.x, top, low.y + wall),
        palette::WOOD,
    );
    shapes.block(
        Vec3::new(low.x, 0.13, high.y - wall),
        Vec3::new(high.x, top, high.y),
        palette::WOOD,
    );
    shapes.polygon(
        &[
            Vec3::new(low.x + wall, top - 0.06, high.y - wall),
            Vec3::new(high.x - wall, top - 0.06, high.y - wall),
            Vec3::new(high.x - wall, top - 0.06, low.y + wall),
            Vec3::new(low.x + wall, top - 0.06, low.y + wall),
        ],
        palette::WATER,
    );
    for x in [low.x + 0.12, high.x - 0.12] {
        shapes.block(
            Vec3::new(x - 0.04, 0.0, low.y + 0.04),
            Vec3::new(x + 0.04, 0.08, high.y - 0.04),
            palette::DARK_WOOD,
        );
    }
}

fn feeder(shapes: &mut Shapes) {
    let at = Vec3::new(layout::FEEDER.x, 0.0, layout::FEEDER.y);
    shapes.cylinder(at, layout::FEEDER_RADIUS, 0.06, 10, palette::METAL);
    shapes.disc(
        at + Vec3::Y * 0.061,
        layout::FEEDER_RADIUS * 0.85,
        10,
        palette::GRAIN,
    );
    // Grain scattered about it, thinning outwards.
    for grain in 0..90 {
        let angle = scatter(grain * 3) * std::f32::consts::TAU;
        let distance =
            layout::FEEDER_RADIUS + scatter(grain * 3 + 1).powi(2) * layout::GRAIN_RADIUS;
        let centre = at + Vec3::new(angle.cos() * distance, 0.012, angle.sin() * distance);
        let size = 0.012 + scatter(grain * 3 + 2) * 0.01;
        shapes.block(
            centre - Vec3::splat(size),
            centre + Vec3::new(size, 0.0, size),
            palette::GRAIN,
        );
    }
}

fn hay(shapes: &mut Shapes) {
    let hay = layout::HAY;
    let (low, high) = (hay.low, hay.high);
    let mid = (low.x + high.x) * 0.5;
    let bale = |shapes: &mut Shapes, low: Vec3, high: Vec3| {
        shapes.block(low, high, palette::STRAW);
        for band in [0.3, 0.7] {
            let x = low.x + (high.x - low.x) * band;
            shapes.block(
                Vec3::new(x - 0.015, low.y - 0.005, low.z - 0.005),
                Vec3::new(x + 0.015, high.y + 0.005, high.z + 0.005),
                palette::DARK_WOOD,
            );
        }
    };
    bale(
        shapes,
        Vec3::new(low.x, 0.0, low.y),
        Vec3::new(mid - 0.02, 0.42, low.y + 0.55),
    );
    bale(
        shapes,
        Vec3::new(mid + 0.02, 0.0, low.y),
        Vec3::new(high.x, 0.42, low.y + 0.55),
    );
    bale(
        shapes,
        Vec3::new(low.x + 0.35, 0.42, low.y + 0.02),
        Vec3::new(high.x - 0.35, 0.84, low.y + 0.53),
    );
    bale(
        shapes,
        Vec3::new(low.x + 0.1, 0.0, high.y - 0.55),
        Vec3::new(mid + 0.2, 0.42, high.y),
    );
    // Loose straw strewn in front.
    for straw in 0..40 {
        let at = Vec3::new(
            low.x + scatter(straw * 5 + 400) * (high.x - low.x),
            0.006,
            high.y + 0.1 + scatter(straw * 5 + 401) * 0.6,
        );
        let turn = Quat::from_rotation_y(scatter(straw * 5 + 402) * std::f32::consts::TAU);
        shapes.cuboid(at, Vec3::new(0.09, 0.004, 0.006), turn, palette::STRAW);
    }
}

fn trees(shapes: &mut Shapes) {
    for tree in 0..14 {
        let angle = (tree as f32 / 14.0 + scatter(tree * 7) * 0.05) * std::f32::consts::TAU;
        let distance = 13.0 + scatter(tree * 7 + 1) * 10.0;
        // None behind home's view of the yard's gate, so that the way in stays open.
        let base = Vec3::new(angle.cos() * distance, 0.0, angle.sin() * distance - 4.0);
        if base.z > 4.0 && base.x.abs() < 6.0 {
            continue;
        }
        let height = 4.0 + scatter(tree * 7 + 2) * 3.5;
        shapes.cylinder(base, 0.18, height * 0.25, 5, palette::BARK);
        for (layer, (from, radius)) in [(0.18, 1.0), (0.42, 0.78), (0.64, 0.55)]
            .into_iter()
            .enumerate()
        {
            shapes.cone(
                base + Vec3::Y * height * from,
                radius * height * 0.32,
                height * 0.45,
                7,
                palette::PINE * (1.0 + layer as f32 * 0.12),
            );
        }
    }
}

/// The yard, as a model of one node whose mesh draws it all.
pub fn model() -> Model {
    let mut shapes = Shapes::new();
    let mut glass = Shapes::new();
    // The meadow, reaching to the trees and beyond, and the packed earth inside the fence, a little above it.
    shapes.disc(Vec3::new(0.0, -0.01, -4.0), 70.0, 28, palette::GRASS);
    let fence = layout::FENCE;
    shapes.polygon(
        &[
            Vec3::new(fence.low.x, 0.0, fence.high.y),
            Vec3::new(fence.high.x, 0.0, fence.high.y),
            Vec3::new(fence.high.x, 0.0, fence.low.y),
            Vec3::new(fence.low.x, 0.0, fence.low.y),
        ],
        palette::EARTH,
    );
    // A worn path from the gate towards the coop and the feeder.
    for step in 0..9 {
        let at = Vec3::new(-0.2 - step as f32 * 0.25, 0.003, 0.8 - step as f32 * 0.8);
        shapes.disc(at, 0.5, 7, palette::EARTH * 1.12);
    }
    fence_around(&mut shapes);
    trough(&mut shapes);
    feeder(&mut shapes);
    hay(&mut shapes);
    trees(&mut shapes);
    lantern(&mut shapes, &mut glass, layout::GATE_LANTERN);
    Model::built(
        vec![Node::new("yard", Transform::IDENTITY).with_mesh(0)],
        vec![0],
        vec![Mesh::new(
            "yard",
            vec![shapes.primitive(PLAIN), glass.primitive(GLASS)],
        )],
        materials(),
        &Limits::default(),
    )
    .expect("the yard is built within the limits")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_yard_is_one_checked_model_whose_lantern_glows() {
        let model = model();
        assert_eq!(model.node("yard"), Some(0));
        let primitives = &model.meshes[0].primitives;
        assert_eq!(primitives.len(), 2);
        assert!(primitives[0].positions.len() > 1000);
        assert_eq!(model.materials[GLASS].emissive, palette::GLOW * 1.6);
        // Everything but the meadow and the trees is inside the fence.
        let (low, high) = primitives[1].bounds();
        assert!(
            layout::FENCE
                .grown(0.5)
                .contains(glam::Vec2::new(low.x, low.z))
        );
        assert!(
            layout::FENCE
                .grown(0.5)
                .contains(glam::Vec2::new(high.x, high.z))
        );
        assert!(
            (0..100)
                .map(scatter)
                .all(|value| (0.0..1.0).contains(&value))
        );
    }
}
