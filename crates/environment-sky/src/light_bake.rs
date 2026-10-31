// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

//! Build-time static lighting: cosine-weighted AO and occluded lantern irradiance.
//! Ray/box queries use the exact cuboids which generate the rendering mesh.
use crate::mesh::{ATLAS_SIZE, Cuboid, Face, LANTERNS, Scene};
use glam::Vec3;

fn blocks(object: &Cuboid, origin: Vec3, direction: Vec3, distance: f32) -> bool {
    let origin = object.inverse_rotation * (origin - object.center);
    let direction = object.inverse_rotation * direction;
    let mut near: f32 = 0.0;
    let mut far = distance;
    for axis in 0..3 {
        if direction[axis].abs() < 0.000001 {
            if origin[axis].abs() > object.half_size[axis] {
                return false;
            }
        } else {
            let a = (-object.half_size[axis] - origin[axis]) / direction[axis];
            let b = (object.half_size[axis] - origin[axis]) / direction[axis];
            near = near.max(a.min(b));
            far = far.min(a.max(b));
            if far < near {
                return false;
            }
        }
    }
    far > 0.0001 && near < distance
}

fn near_surface(object: &Cuboid, point: Vec3, radius: f32) -> bool {
    let local = object.inverse_rotation * (point - object.center);
    (local.abs() - object.half_size)
        .max(Vec3::ZERO)
        .length_squared()
        < radius * radius
}

fn ambient_occlusion(scene: &Scene, face: &Face, point: Vec3) -> f32 {
    const REACH: f32 = 0.9;
    let normal = face.normal;
    let tangent = face.u.normalize();
    let bitangent = normal.cross(tangent);
    let origin = point + normal * 0.002;
    let candidates: Vec<_> = scene
        .objects
        .iter()
        .enumerate()
        .filter(|(i, object)| *i != face.object && near_surface(object, point, REACH))
        .map(|(_, object)| object)
        .collect();
    if candidates.is_empty() {
        return 1.0;
    }
    let mut blocked = 0;
    for sample in 0..24 {
        let r = ((sample as f32 + 0.5) / 24.0).sqrt();
        let angle = sample as f32 * 2.399_963_1;
        let direction = tangent * (r * angle.cos())
            + bitangent * (r * angle.sin())
            + normal * (1.0 - r * r).sqrt();
        if candidates
            .iter()
            .any(|object| blocks(object, origin, direction, REACH))
        {
            blocked += 1;
        }
    }
    1.0 - blocked as f32 / 24.0
}

fn lantern_irradiance(scene: &Scene, face: &Face, point: Vec3) -> Vec3 {
    let origin = point + face.normal * 0.002;
    let mut irradiance = Vec3::ZERO;
    for light in LANTERNS {
        let to_light = light - origin;
        let distance = to_light.length();
        if distance > 3.5 || distance < 0.001 {
            continue;
        }
        let cosine = face.normal.dot(to_light / distance).max(0.0);
        if cosine == 0.0 {
            continue;
        }
        // Sixteen deterministic samples inside the emitting box soften its baked
        // shadow. Ignore emissive boxes themselves, not their caps or pedestals.
        let mut visible = 0;
        for sample in 0..16 {
            let y = 1.0 - 2.0 * (sample as f32 + 0.5) / 16.0;
            let radius = (1.0 - y * y).sqrt();
            let angle = sample as f32 * 2.399_963_1;
            let offset = Vec3::new(
                radius * angle.cos() * 0.09,
                y * 0.12,
                radius * angle.sin() * 0.09,
            );
            let ray = light + offset - origin;
            let length = ray.length();
            if !scene.objects.iter().enumerate().any(|(i, object)| {
                i != face.object
                    && object.emission < 0.5
                    && blocks(object, origin, ray / length, length)
            }) {
                visible += 1;
            }
        }
        let falloff =
            (1.0 - (distance / 3.5).powi(2)).max(0.0).powi(2) / (0.4 + distance * distance);
        irradiance += Vec3::new(3.2, 1.65, 0.55) * (cosine * falloff * visible as f32 / 16.0);
    }
    irradiance.min(Vec3::ONE)
}

pub fn bake() -> Vec<u8> {
    let scene = crate::mesh::scene();
    let mut atlas = vec![0; (ATLAS_SIZE * ATLAS_SIZE * 4) as usize];
    for pixel in atlas.as_chunks_mut::<4>().0 {
        pixel[3] = 255;
    }
    for face in &scene.faces {
        // Bake the large deck chart at ~4 cm, then bilinearly expand to its
        // ~2 cm atlas grid. Other faces use 22² samples. Gutters clamp to edges.
        let size = if face.tile.size == 512 { 256 } else { 22 };
        let mut samples = Vec::with_capacity(size * size);
        for y in 0..size {
            for x in 0..size {
                let point = face.corner
                    + face.u * (x as f32 / (size - 1) as f32)
                    + face.v * (y as f32 / (size - 1) as f32);
                let light = lantern_irradiance(&scene, face, point);
                samples.push([
                    light.x,
                    light.y,
                    light.z,
                    ambient_occlusion(&scene, face, point),
                ]);
            }
        }
        for y in 0..face.tile.size {
            for x in 0..face.tile.size {
                let u = ((x as f32 - 2.0) / (face.tile.size - 5) as f32).clamp(0.0, 1.0)
                    * (size - 1) as f32;
                let v = ((y as f32 - 2.0) / (face.tile.size - 5) as f32).clamp(0.0, 1.0)
                    * (size - 1) as f32;
                let a = u.floor() as usize;
                let b = v.floor() as usize;
                let mut value = [0.0; 4];
                for dy in 0..2 {
                    for dx in 0..2 {
                        let weight = if dx == 0 { 1.0 - u.fract() } else { u.fract() }
                            * if dy == 0 { 1.0 - v.fract() } else { v.fract() };
                        let sample =
                            samples[(b + dy).min(size - 1) * size + (a + dx).min(size - 1)];
                        for channel in 0..4 {
                            value[channel] += weight * sample[channel];
                        }
                    }
                }
                let index = ((face.tile.y + y) * ATLAS_SIZE + face.tile.x + x) as usize * 4;
                for channel in 0..4 {
                    atlas[index + channel] = (value[channel].clamp(0.0, 1.0) * 255.0).round() as u8;
                }
            }
        }
    }
    atlas
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn baked_atlas_is_reproducible_and_contains_both_light_and_occlusion() {
        let atlas = bake();
        assert_eq!(
            atlas,
            include_bytes!(concat!(env!("OUT_DIR"), "/scenery-lightmap.rgba"))
        );
        assert!(
            atlas
                .as_chunks::<4>()
                .0
                .iter()
                .any(|p| p[0] > 80 && p[0] > p[1] && p[1] > p[2])
        );
        assert!(atlas.as_chunks::<4>().0.iter().any(|p| p[3] < 180));
        assert!(atlas.as_chunks::<4>().0.iter().any(|p| p[3] == 255));
    }

    #[test]
    fn bake_queries_respect_geometry_and_local_contacts() {
        let scene = crate::mesh::scene();
        let deck = &scene.faces[2];
        let contact =
            ambient_occlusion(&scene, deck, Vec3::new(1.025, crate::mesh::DECK_TOP, -3.9));
        let open = ambient_occlusion(&scene, deck, Vec3::new(0.0, crate::mesh::DECK_TOP, 1.0));
        assert!(
            contact < 0.85 && open > 0.99,
            "AO contact {contact}, open {open}"
        );
        let warm = lantern_irradiance(&scene, deck, Vec3::new(-1.55, crate::mesh::DECK_TOP, -3.8));
        let distant = lantern_irradiance(&scene, deck, Vec3::new(0.0, crate::mesh::DECK_TOP, 2.0));
        assert!(warm.x > 0.2 && warm.x > warm.y && warm.y > warm.z);
        assert_eq!(distant, Vec3::ZERO);
        assert!(blocks(
            &scene.objects[0],
            Vec3::new(0.0, 1.0, -2.5),
            -Vec3::Y,
            2.0
        ));
        assert!(!blocks(
            &scene.objects[0],
            Vec3::new(0.0, 1.0, -2.5),
            Vec3::Y,
            2.0
        ));
    }

    #[test]
    fn lantern_bake_is_occluded_by_intervening_geometry() {
        let mut scene = crate::mesh::scene();
        let point = Vec3::new(-2.2, crate::mesh::DECK_TOP, -3.9);
        let lit = lantern_irradiance(&scene, &scene.faces[2], point);
        assert!(lit.x > 0.2);
        scene.objects.push(Cuboid {
            center: Vec3::new(-1.8, 0.45, -3.9),
            half_size: Vec3::new(0.8, 0.05, 0.8),
            inverse_rotation: glam::Mat3::IDENTITY,
            emission: 0.0,
        });
        assert_eq!(
            lantern_irradiance(&scene, &scene.faces[2], point),
            Vec3::ZERO
        );
    }
}
