// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

//! The chicken coop: a hen house raised on legs, painted barn red, with a pitched roof, a little door at the top of a
//! cleated ramp, a window in its side to peek in at the nest boxes and the perch, and a lantern by the door.
//!
//! It is a model of its own, in its own space: metres, the ground beneath the middle of its footprint at the origin, its
//! door facing +Z. The farmyard places it on [`layout::COOP`]. It is built here, in code, until a coop modelled in Blender
//! replaces it: any `.glb` with the same footprint, door and ramp can, through [`Coop::Asset`], loaded and checked as
//! every asset is. See the crate's `context/coop.md`.
use glam::{Quat, Vec3};
use tomori_assets::{AssetRef, Library, Limits, Mesh, Model, Node, Transform};

use crate::layout;
use crate::props::{GLASS, PLAIN, lantern, materials, palette};
use crate::shapes::Shapes;

/// The coop the farmyard draws.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
#[non_exhaustive]
pub enum Coop {
    /// The coop built here, in code.
    #[default]
    Built,
    /// A coop modelled elsewhere, loaded from the program's assets; the built one stands in if it cannot be.
    Asset(AssetRef),
}

impl Coop {
    /// The coop's model: the asset's, loaded from `library`, or the one built here if there is no asset or it cannot be
    /// loaded, saying why in the log.
    pub fn model(&self, library: &Library) -> std::sync::Arc<Model> {
        match self {
            Self::Asset(asset) => library.model(asset).unwrap_or_else(|error| {
                log::warn!(
                    "The farmyard's coop could not be loaded, so it is built instead: {error}"
                );
                std::sync::Arc::new(model())
            }),
            Self::Built => std::sync::Arc::new(model()),
        }
    }
}

/// Where the coop stands in the farmyard: its model's space, placed on [`layout::COOP`].
pub fn placement() -> glam::Mat4 {
    let centre = layout::COOP.centre();
    glam::Mat4::from_translation(Vec3::new(centre.x, 0.0, centre.y))
}

/// A point of the farmyard, in the coop's own space.
fn local(point: Vec3) -> Vec3 {
    placement().inverse().transform_point3(point)
}

/// The coop built in code.
pub fn model() -> Model {
    let mut shapes = Shapes::new();
    let mut glass = Shapes::new();
    let size = layout::COOP.size();
    let (half_x, half_z) = (size.x * 0.5, size.y * 0.5);
    let (floor, walls, ridge) = (layout::COOP_FLOOR, layout::COOP_WALLS, layout::COOP_RIDGE);
    let wall = 0.05;
    let door = local(layout::DOOR);
    let half_door = layout::DOOR_SIZE.x * 0.5;
    let door_top = floor + layout::DOOR_SIZE.y;
    // Legs, and the floor they hold up.
    for x in [-half_x + 0.08, half_x - 0.08] {
        for z in [-half_z + 0.08, half_z - 0.08] {
            shapes.block(
                Vec3::new(x - 0.06, 0.0, z - 0.06),
                Vec3::new(x + 0.06, floor, z + 0.06),
                palette::DARK_WOOD,
            );
        }
    }
    shapes.block(
        Vec3::new(-half_x, floor - 0.06, -half_z),
        Vec3::new(half_x, floor, half_z),
        palette::WOOD,
    );
    // The front wall, around the door, and its gable.
    let front = half_z - wall;
    let panel = |shapes: &mut Shapes, x: [f32; 2], y: [f32; 2], z: [f32; 2]| {
        shapes.block(
            Vec3::new(x[0], y[0], z[0]),
            Vec3::new(x[1], y[1], z[1]),
            palette::RED,
        );
    };
    panel(
        &mut shapes,
        [-half_x, door.x - half_door],
        [floor, walls],
        [front, half_z],
    );
    panel(
        &mut shapes,
        [door.x + half_door, half_x],
        [floor, walls],
        [front, half_z],
    );
    panel(
        &mut shapes,
        [door.x - half_door, door.x + half_door],
        [door_top, walls],
        [front, half_z],
    );
    // The back wall.
    panel(
        &mut shapes,
        [-half_x, half_x],
        [floor, walls],
        [-half_z, -front],
    );
    // The far side, and the near side around its window.
    panel(
        &mut shapes,
        [-half_x, -half_x + wall],
        [floor, walls],
        [-half_z, half_z],
    );
    let window = layout::WINDOW.map(|[z, y]| (local(Vec3::new(0.0, y, z)).z, y));
    let side = [half_x - wall, half_x];
    panel(&mut shapes, side, [floor, window[0].1], [-half_z, half_z]);
    panel(&mut shapes, side, [window[1].1, walls], [-half_z, half_z]);
    panel(
        &mut shapes,
        side,
        [window[0].1, window[1].1],
        [-half_z, window[0].0],
    );
    panel(
        &mut shapes,
        side,
        [window[0].1, window[1].1],
        [window[1].0, half_z],
    );
    // The gables, front and back.
    for z in [half_z, -half_z] {
        let facing = z.signum();
        let corners = [
            Vec3::new(-half_x * facing, walls, z),
            Vec3::new(half_x * facing, walls, z),
            Vec3::new(0.0, ridge, z),
        ];
        shapes.polygon(&corners, palette::RED * 0.9);
    }
    // White trim at the corners, around the door and under the window.
    for x in [-half_x, half_x] {
        for z in [-half_z, half_z] {
            shapes.block(
                Vec3::new(x - 0.03, floor, z - 0.03),
                Vec3::new(x + 0.03, walls, z + 0.03),
                palette::WEATHERED,
            );
        }
    }
    shapes.block(
        Vec3::new(door.x - half_door - 0.04, door_top, half_z),
        Vec3::new(door.x + half_door + 0.04, door_top + 0.05, half_z + 0.02),
        palette::WEATHERED,
    );
    shapes.block(
        Vec3::new(half_x, window[0].1 - 0.04, window[0].0 - 0.04),
        Vec3::new(half_x + 0.04, window[0].1, window[1].0 + 0.04),
        palette::WEATHERED,
    );
    // The roof: two pitched panels overhanging every side.
    let overhang = 0.18;
    let pitch = ((ridge - walls) / half_x).atan();
    let run = (half_x + overhang) / pitch.cos();
    for side in [-1.0f32, 1.0] {
        let middle = Vec3::new(
            side * (half_x + overhang) * 0.5,
            (ridge + walls - overhang * pitch.tan()) * 0.5,
            0.0,
        );
        shapes.cuboid(
            middle + Vec3::Y * 0.03,
            Vec3::new(run * 0.5, 0.03, half_z + overhang),
            Quat::from_rotation_z(-side * pitch),
            palette::ROOF,
        );
    }
    shapes.beam(
        Vec3::new(0.0, ridge + 0.04, -half_z - overhang),
        Vec3::new(0.0, ridge + 0.04, half_z + overhang),
        0.07,
        palette::DARK_WOOD,
    );
    // Inside, seen through the window and the door, in the shade: a lining of dark boards, which the lantern outside
    // hardly reaches, straw on the floor, two nest boxes along the back, and a perch.
    let lining = 0.012;
    let (inner_x, inner_z) = (half_x - wall - lining, half_z - wall - lining);
    for (low, high) in [
        (
            Vec3::new(-inner_x - lining, floor, -inner_z),
            Vec3::new(-inner_x, walls, inner_z),
        ),
        (
            Vec3::new(-inner_x, floor, -inner_z - lining),
            Vec3::new(inner_x, walls, -inner_z),
        ),
        (
            Vec3::new(-inner_x, walls - 0.02, -inner_z),
            Vec3::new(inner_x, walls, inner_z),
        ),
    ] {
        shapes.block(low, high, palette::SHADOW * 2.0);
    }
    shapes.block(
        Vec3::new(-half_x + wall, floor, -half_z + wall),
        Vec3::new(half_x - wall, floor + 0.02, half_z - wall),
        palette::STRAW * 0.45,
    );
    for x in [-0.55f32, 0.05] {
        shapes.block(
            Vec3::new(x, floor, -half_z + wall),
            Vec3::new(x + 0.5, floor + 0.32, -half_z + 0.42),
            palette::WOOD,
        );
        shapes.block(
            Vec3::new(x + 0.05, floor + 0.04, -half_z + 0.38),
            Vec3::new(x + 0.45, floor + 0.24, -half_z + 0.43),
            palette::SHADOW,
        );
        shapes.block(
            Vec3::new(x + 0.08, floor + 0.04, -half_z + 0.12),
            Vec3::new(x + 0.42, floor + 0.1, -half_z + 0.36),
            palette::STRAW,
        );
    }
    shapes.beam(
        Vec3::new(-half_x + wall, floor + 0.5, 0.15),
        Vec3::new(half_x - wall, floor + 0.5, 0.15),
        0.05,
        palette::WOOD,
    );
    // The door, swung open beside its opening, and the ramp down from its sill, with cleats to grip.
    shapes.block(
        Vec3::new(door.x + half_door + 0.02, floor + 0.02, half_z + 0.01),
        Vec3::new(door.x + half_door + 0.32, door_top - 0.02, half_z + 0.04),
        palette::RED * 0.8,
    );
    let foot = local(layout::RAMP_FOOT);
    let sill = Vec3::new(door.x, floor, half_z);
    let along = foot - sill;
    let slope = along.normalize();
    let width = layout::RAMP_WIDTH * 0.5;
    shapes.cuboid(
        (sill + foot) * 0.5 + Vec3::Y * 0.02,
        Vec3::new(width, 0.02, along.length() * 0.5),
        Quat::from_rotation_arc(Vec3::Z, slope),
        palette::WOOD,
    );
    let cleats = (along.length() / 0.22) as u32;
    for cleat in 1..cleats {
        let at = sill + along * (cleat as f32 / cleats as f32) + Vec3::Y * 0.05;
        shapes.beam(
            at - Vec3::X * width,
            at + Vec3::X * width,
            0.025,
            palette::DARK_WOOD,
        );
    }
    // The lantern by the door, on a bracket from the wall.
    let hung = local(layout::LANTERN);
    shapes.beam(
        Vec3::new(hung.x, hung.y + 0.2, half_z),
        hung + Vec3::Y * 0.2,
        0.03,
        palette::METAL,
    );
    shapes.beam(
        hung + Vec3::Y * 0.2,
        hung + Vec3::Y * 0.15,
        0.015,
        palette::METAL,
    );
    lantern(&mut shapes, &mut glass, hung);
    Model::built(
        vec![Node::new("coop", Transform::IDENTITY).with_mesh(0)],
        vec![0],
        vec![Mesh::new(
            "coop",
            vec![shapes.primitive(PLAIN), glass.primitive(GLASS)],
        )],
        materials(),
        &Limits::default(),
    )
    .expect("the coop is built within the limits")
}

#[cfg(test)]
mod tests {
    use super::*;
    use tomori_assets::{Digest, Embedded};

    #[test]
    fn the_built_coop_fits_its_footprint_and_reaches_its_ramp() {
        let model = model();
        let (low, high) = model.meshes[0].primitives[0].bounds();
        let placed = |point: Vec3| placement().transform_point3(point);
        let (low, high) = (placed(low), placed(high));
        // Its roof overhangs, and its ramp reaches the ground in front of its door.
        assert!(low.x >= layout::COOP.low.x - 0.25 && high.x <= layout::COOP.high.x + 0.25);
        assert!((high.z - layout::RAMP_FOOT.z).abs() < 0.1);
        assert!(low.y.abs() < 1e-3 && (high.y - layout::COOP_RIDGE).abs() < 0.15);
        assert!(local(layout::DOOR).z > 0.0);
    }

    #[test]
    fn a_coop_from_an_asset_replaces_the_built_one_unless_it_cannot_be_loaded() {
        let built = Coop::default().model(Library::shared());
        assert_eq!(*built, model());
        // Missing, the built one stands in.
        let library = Library::default();
        let missing = Coop::Asset(AssetRef::new(
            "farmyard",
            1,
            "coop.glb",
            Digest::of(b"coop"),
        ));
        assert_eq!(*missing.model(&library), model());
        // Present, it is the asset's.
        let bytes: &'static [u8] = Box::leak(crate::tests::cube_glb().into_boxed_slice());
        library.add(Embedded::new().with(bytes));
        let replaced = Coop::Asset(AssetRef::new("farmyard", 1, "coop.glb", Digest::of(bytes)))
            .model(&library);
        assert_eq!(replaced.meshes[0].primitives[0].positions.len(), 3);
    }
}
