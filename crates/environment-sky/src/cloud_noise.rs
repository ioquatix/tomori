//! Periodic shape/detail noise baked at build time, not on the headset or per frame.
pub const SIZE: usize = 64;

fn hash(x: i32, y: i32, z: i32, period: i32, seed: u32) -> f32 {
    let mut h = (x.rem_euclid(period) as u32).wrapping_mul(374_761_393)
        ^ (y.rem_euclid(period) as u32).wrapping_mul(668_265_263)
        ^ (z.rem_euclid(period) as u32).wrapping_mul(2_147_483_647)
        ^ seed;
    h = (h ^ (h >> 13)).wrapping_mul(1_274_126_177);
    ((h ^ (h >> 16)) & 0x00ff_ffff) as f32 / 16_777_215.0
}

fn value(point: [f32; 3], period: i32) -> f32 {
    let cell = point.map(|v| v.floor() as i32);
    let t = point.map(|v| {
        let f = v - v.floor();
        f * f * (3.0 - 2.0 * f)
    });
    let mut result = 0.0;
    for z in 0..2 {
        for y in 0..2 {
            for x in 0..2 {
                let weight = [x, y, z]
                    .into_iter()
                    .zip(t)
                    .map(|(i, t)| if i == 0 { 1.0 - t } else { t })
                    .product::<f32>();
                result += weight * hash(cell[0] + x, cell[1] + y, cell[2] + z, period, 0x746f6d6f);
            }
        }
    }
    result
}

fn worley(point: [f32; 3], period: i32) -> f32 {
    let cell = point.map(|v| v.floor() as i32);
    let mut nearest = f32::INFINITY;
    for z in -1..=1 {
        for y in -1..=1 {
            for x in -1..=1 {
                let c = [cell[0] + x, cell[1] + y, cell[2] + z];
                let distance = (0..3)
                    .map(|axis| {
                        let offset = hash(c[0], c[1], c[2], period, 13 + axis as u32 * 113);
                        (c[axis] as f32 + offset - point[axis]).powi(2)
                    })
                    .sum::<f32>();
                nearest = nearest.min(distance);
            }
        }
    }
    (1.0 - nearest.sqrt() / 1.15).clamp(0.0, 1.0)
}

pub fn bake() -> Vec<u8> {
    let mut bytes = Vec::with_capacity(SIZE * SIZE * SIZE * 4);
    for z in 0..SIZE {
        for y in 0..SIZE {
            for x in 0..SIZE {
                let p = [x, y, z].map(|v| (v as f32 + 0.5) / SIZE as f32);
                let broad = value(p.map(|v| v * 2.0), 2);
                let shape =
                    0.65 * worley(p.map(|v| v * 4.0), 4) + 0.35 * value(p.map(|v| v * 4.0), 4);
                let detail = value(p.map(|v| v * 16.0), 16);
                for value in [shape, detail, broad, 1.0] {
                    bytes.push((value.clamp(0.0, 1.0) * 255.0).round() as u8);
                }
            }
        }
    }
    bytes
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn density_noise_is_periodic_and_repeatable() {
        for p in [[0.1, 1.9, -0.3], [2.2, -0.2, 3.4], [0.0; 3]] {
            let shifted = [p[0] + 4.0, p[1] - 4.0, p[2] + 8.0];
            assert!((value(p, 4) - value(shifted, 4)).abs() < 0.00001);
            assert!((worley(p, 4) - worley(shifted, 4)).abs() < 0.00001);
            assert_eq!(value(p, 4), value(p, 4));
        }
        let baked = bake();
        assert_eq!(
            baked,
            include_bytes!(concat!(env!("OUT_DIR"), "/cloud-noise.rgba"))
        );
        assert_eq!(baked.len(), SIZE * SIZE * SIZE * 4);
    }
}
