// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

//! Validated OpenXR hand joint positions for the optional skeleton view.
use serde::{Deserialize, Serialize};

pub const JOINT_COUNT: usize = 26;

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub struct Joint {
    pub position: [f32; 3],
    pub radius: f32,
}

impl Joint {
    pub fn valid(&self) -> bool {
        self.position
            .iter()
            .all(|value| value.is_finite() && value.abs() < 10_000.0)
            && self.radius.is_finite()
            && (0.001..=0.1).contains(&self.radius)
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub struct Hand {
    pub joints: [Option<Joint>; JOINT_COUNT],
}

impl Hand {
    pub fn valid(&self) -> bool {
        self.joints.iter().flatten().all(Joint::valid)
    }
}

// OpenXR EXT hand-joint order: wrist followed by thumb and four fingers.
pub const BONES: [(usize, usize); 24] = [
    (1, 2),
    (2, 3),
    (3, 4),
    (4, 5),
    (1, 6),
    (6, 7),
    (7, 8),
    (8, 9),
    (9, 10),
    (1, 11),
    (11, 12),
    (12, 13),
    (13, 14),
    (14, 15),
    (1, 16),
    (16, 17),
    (17, 18),
    (18, 19),
    (19, 20),
    (1, 21),
    (21, 22),
    (22, 23),
    (23, 24),
    (24, 25),
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_joint_geometry_and_standard_indices() {
        assert!(
            BONES
                .iter()
                .all(|(a, b)| *a < JOINT_COUNT && *b < JOINT_COUNT)
        );
        assert!(
            Joint {
                position: [1.0, 2.0, 3.0],
                radius: 0.01
            }
            .valid()
        );
        assert!(
            !Joint {
                position: [f32::NAN, 0.0, 0.0],
                radius: 0.01
            }
            .valid()
        );
        assert!(
            !Joint {
                position: [0.0; 3],
                radius: -1.0
            }
            .valid()
        );
    }
}
