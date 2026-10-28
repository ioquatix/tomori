use bytemuck::{Pod, Zeroable};
use glam::{Mat3, Vec3};

pub(super) const DECK_TOP: f32 = -0.01;
pub(super) const ATLAS_SIZE: u32 = 1024;
pub(super) const LANTERNS: [Vec3; 2] = [Vec3::new(-1.55, 0.87, -3.9), Vec3::new(1.55, 0.87, -3.9)];

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
pub(super) struct Vertex {
    pub(super) position: [f32; 3],
    normal: [f32; 3],
    color: [f32; 3],
    emission: f32,
    pub(super) light_uv: [f32; 2],
}

// The same description is used by the build-time baker and runtime mesh builder.
#[allow(dead_code)]
pub(super) struct Scene {
    pub vertices: Vec<Vertex>,
    pub faces: Vec<Face>,
    pub objects: Vec<Cuboid>,
}

#[allow(dead_code)]
pub(super) struct Cuboid {
    pub center: Vec3,
    pub half_size: Vec3,
    pub inverse_rotation: Mat3,
    pub emission: f32,
}

#[derive(Clone, Copy)]
pub(super) struct Tile {
    pub x: u32,
    pub y: u32,
    pub size: u32,
}

impl Tile {
    pub fn uv(self, u: f32, v: f32) -> [f32; 2] {
        // Two-texel gutters, with endpoints at interior pixel centres.
        [
            (self.x as f32 + 2.5 + u * (self.size - 5) as f32) / ATLAS_SIZE as f32,
            (self.y as f32 + 2.5 + v * (self.size - 5) as f32) / ATLAS_SIZE as f32,
        ]
    }
}

#[allow(dead_code)]
pub(super) struct Face {
    pub corner: Vec3,
    pub u: Vec3,
    pub v: Vec3,
    pub normal: Vec3,
    pub tile: Tile,
    pub object: usize,
}

fn tile(face: usize) -> Tile {
    // The deck's upward face gets 508² interior texels (~2 cm on the deck).
    // Other static cuboid faces use unique 44² charts in the right half.
    if face == 2 {
        return Tile {
            x: 0,
            y: 0,
            size: 512,
        };
    }
    let index = face - usize::from(face > 2);
    let tile = Tile {
        x: 512 + index as u32 % 10 * 48,
        y: index as u32 / 10 * 48,
        size: 48,
    };
    assert!(tile.x + tile.size <= ATLAS_SIZE && tile.y + tile.size <= ATLAS_SIZE);
    tile
}

struct Mesh(Scene);

impl Mesh {
    fn triangle(&mut self, points: [Vec3; 3], uv: [[f32; 2]; 3], color: [f32; 3], emission: f32) {
        let normal = (points[1] - points[0])
            .cross(points[2] - points[0])
            .normalize_or_zero();
        for (point, light_uv) in points.into_iter().zip(uv) {
            self.0.vertices.push(Vertex {
                position: point.to_array(),
                normal: normal.to_array(),
                color,
                emission,
                light_uv,
            });
        }
    }

    fn cuboid(&mut self, center: Vec3, size: Vec3, rotation: Mat3, color: [f32; 3], emission: f32) {
        let object = self.0.objects.len();
        self.0.objects.push(Cuboid {
            center,
            half_size: size * 0.5,
            inverse_rotation: rotation.transpose(),
            emission,
        });
        for (normal, u, v) in [
            (Vec3::X, -Vec3::Z, Vec3::Y),
            (-Vec3::X, Vec3::Z, Vec3::Y),
            (Vec3::Y, Vec3::X, -Vec3::Z),
            (-Vec3::Y, Vec3::X, Vec3::Z),
            (Vec3::Z, Vec3::X, Vec3::Y),
            (-Vec3::Z, -Vec3::X, Vec3::Y),
        ] {
            let corners = [(-1.0, -1.0), (1.0, -1.0), (1.0, 1.0), (-1.0, 1.0)]
                .map(|(a, b)| center + rotation * ((normal + u * a + v * b) * size * 0.5));
            let tile = tile(self.0.faces.len());
            self.0.faces.push(Face {
                corner: corners[0],
                u: corners[1] - corners[0],
                v: corners[3] - corners[0],
                normal: rotation * normal,
                tile,
                object,
            });
            let uv = [
                tile.uv(0.0, 0.0),
                tile.uv(1.0, 0.0),
                tile.uv(1.0, 1.0),
                tile.uv(0.0, 1.0),
            ];
            self.triangle(
                [corners[0], corners[1], corners[2]],
                [uv[0], uv[1], uv[2]],
                color,
                emission,
            );
            self.triangle(
                [corners[0], corners[2], corners[3]],
                [uv[0], uv[2], uv[3]],
                color,
                emission,
            );
        }
    }
}

/// Static scene in metres. The user's floor stays at y=0; the coordinator's
/// work volume at (0, 1.35, -2.5) stays open. No asset/network loading required.
pub(super) fn build() -> Vec<Vertex> {
    scene().vertices
}

pub(super) fn scene() -> Scene {
    let mut mesh = Mesh(Scene {
        vertices: Vec::new(),
        faces: Vec::new(),
        objects: Vec::new(),
    });
    let stone = [0.12, 0.15, 0.19];
    let wood = [0.19, 0.085, 0.055];
    let roof = [0.055, 0.075, 0.12];
    let identity = Mat3::IDENTITY;
    // A broad stable deck: it is scenery, not a locomotion or collision surface.
    mesh.cuboid(
        Vec3::new(0.0, -0.17, -2.5),
        Vec3::new(9.0, 0.32, 10.0),
        identity,
        stone,
        0.0,
    );
    for x in [-4.4, 4.4] {
        mesh.cuboid(
            Vec3::new(x, 0.0, -2.5),
            Vec3::new(0.04, 0.02, 9.8),
            identity,
            [0.22, 0.25, 0.29],
            0.0,
        );
    }
    // Open-sided shrine blockout behind the coordinator, not over the text arc.
    for x in [-0.95, 0.95] {
        for z in [-3.9, -5.4] {
            mesh.cuboid(
                Vec3::new(x, (2.4 + DECK_TOP) * 0.5, z),
                Vec3::new(0.12, 2.4 - DECK_TOP, 0.12),
                identity,
                wood,
                0.0,
            );
        }
    }
    mesh.cuboid(
        Vec3::new(0.0, 2.34, -4.65),
        Vec3::new(2.2, 0.14, 1.9),
        identity,
        wood,
        0.0,
    );
    for side in [-1.0, 1.0] {
        mesh.cuboid(
            Vec3::new(side * 0.66, 2.65, -4.65),
            Vec3::new(1.55, 0.13, 2.35),
            Mat3::from_rotation_z(-side * 0.38),
            roof,
            0.0,
        );
    }
    mesh.cuboid(
        Vec3::new(0.0, 2.96, -4.65),
        Vec3::new(0.12, 0.1, 2.5),
        identity,
        wood,
        0.0,
    );
    // Lantern emission and its occluded illumination are baked separately.
    for light in LANTERNS {
        let x = light.x;
        mesh.cuboid(
            Vec3::new(x, (0.72 + DECK_TOP) * 0.5, -3.9),
            Vec3::new(0.12, 0.72 - DECK_TOP, 0.12),
            identity,
            stone,
            0.0,
        );
        mesh.cuboid(
            light,
            Vec3::new(0.22, 0.3, 0.22),
            identity,
            [1.0, 0.68, 0.24],
            1.0,
        );
        mesh.cuboid(
            Vec3::new(x, 1.06, -3.9),
            Vec3::new(0.34, 0.08, 0.34),
            identity,
            roof,
            0.0,
        );
    }
    mesh.0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn post_and_lantern_bases_meet_the_deck() {
        let scene = scene();
        let bases: Vec<_> = scene
            .objects
            .iter()
            .filter(|o| {
                (o.half_size.x - 0.06).abs() < 0.00001
                    && (o.half_size.z - 0.06).abs() < 0.00001
                    && o.half_size.y > 0.35
            })
            .collect();
        assert_eq!(bases.len(), 6);
        for base in bases {
            assert!((base.center.y - base.half_size.y - DECK_TOP).abs() < 0.000001);
        }
    }

    #[test]
    fn lightmap_charts_are_disjoint_and_have_valid_uvs() {
        let scene = scene();
        for (i, face) in scene.faces.iter().enumerate() {
            for other in &scene.faces[i + 1..] {
                let a = face.tile;
                let b = other.tile;
                assert!(
                    a.x + a.size <= b.x
                        || b.x + b.size <= a.x
                        || a.y + a.size <= b.y
                        || b.y + b.size <= a.y
                );
            }
        }
        assert!(
            scene
                .vertices
                .iter()
                .all(|v| v.light_uv.into_iter().all(|u| u > 0.0 && u < 1.0))
        );
    }

    #[test]
    fn geometry_is_finite_bounded_and_deterministic() {
        let a = build();
        let b = build();
        assert_eq!(
            bytemuck::cast_slice::<_, u8>(&a),
            bytemuck::cast_slice::<_, u8>(&b)
        );
        assert!(a.len() < 60_000);
        assert_eq!(a.len() % 3, 0);
        for vertex in &a {
            assert!(
                vertex
                    .position
                    .iter()
                    .chain(&vertex.normal)
                    .chain(&vertex.color)
                    .all(|v| v.is_finite())
            );
            assert!(vertex.position.iter().all(|v| v.abs() < 40.0));
            assert!((Vec3::from_array(vertex.normal).length() - 1.0).abs() < 0.01);
        }
    }

    #[test]
    fn scenery_preserves_the_central_working_volume() {
        // Check triangle bounds, not just vertices: no large surface may cross
        // the box occupied by the coordinator and nearby interaction space.
        for triangle in build().as_chunks::<3>().0 {
            let points = triangle.iter().map(|v| Vec3::from_array(v.position));
            let min = points.clone().fold(Vec3::splat(f32::INFINITY), Vec3::min);
            let max = points.fold(Vec3::splat(f32::NEG_INFINITY), Vec3::max);
            assert!(
                max.x < -0.7
                    || min.x > 0.7
                    || max.y < 0.4
                    || min.y > 2.0
                    || max.z < -3.5
                    || min.z > -1.5
            );
        }
    }
}
