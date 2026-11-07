// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

//! What a chicken is doing, as the farmyard's server side decides it and sends it, with the moment it began, and where
//! that puts the chicken at any moment after: the server to plan the next thing, and every client to draw it. Both
//! compute it with the same functions, from the same root motion, so a chicken walks as far as its feet carry it, and
//! everyone sees it in the same place without the server streaming where it is.
//!
//! Moments are milliseconds on the farmyard's clock, which its server side keeps and sends with its state.
use glam::{Vec2, Vec3};
use serde::{Deserialize, Serialize};

use crate::chicken::Chicken;

/// How fast a chicken turns on the spot before it walks, in radians a second.
pub const TURN: f32 = 2.6;
/// How fast a startled chicken spins round to run, in radians a second.
pub const SPIN: f32 = 9.4;
/// How far a chicken leans forward as it pecks, in radians, and how long each peck takes, in milliseconds.
pub const PECK: f32 = 0.45;
pub const PECK_MILLIS: u64 = 700;
/// How far a chicken looks to one side and back, in radians.
pub const LOOK: f32 = 0.45;

/// What a chicken is doing.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "doing", rename_all = "snake_case")]
pub enum Doing {
    /// Standing where it is.
    Stand,
    /// Pecking at the ground, until a moment.
    Peck { until: u64 },
    /// Looking to one side, left if positive, and back, until a moment.
    Look { side: f32, until: u64 },
    /// Turning to face `to`, then walking straight to it, stopping with both feet down: up the coop's ramp, or down it,
    /// `to` is above or below where it is.
    Walk { to: [f32; 3] },
    /// Confused by someone pointing at it from `toward`, `[x, z]`: it turns to them and tilts its head, holding it.
    Confused { toward: [f32; 2] },
    /// No longer pointed at: it straightens its head, and stands.
    Recover,
    /// Startled: it spins round to face `away`, a direction `[x, z]`, and sprints that way.
    Sprint { away: [f32; 2] },
    /// Inside the coop, roosting.
    Roost,
}

/// What a chicken is doing, since when, and where it was and which way it faced as it began.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Behaviour {
    pub since: u64,
    pub at: [f32; 3],
    /// Which way it faces, in radians about the vertical: 0 faces +Z, and a quarter turn faces +X.
    pub heading: f32,
    #[serde(flatten)]
    pub doing: Doing,
}

/// Where a chicken is, and how it stands, at a moment.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Placed {
    pub position: Vec3,
    pub heading: f32,
    /// How far it leans forward, in radians, as it pecks.
    pub lean: f32,
}

/// The heading that faces `direction`, `[x, z]`.
pub fn heading(direction: Vec2) -> f32 {
    direction.x.atan2(direction.y)
}

/// The direction `[x, z]` that `heading` faces.
pub fn facing(heading: f32) -> Vec2 {
    Vec2::new(heading.sin(), heading.cos())
}

/// The smaller turn from `from` to `to`, from −π to π.
pub fn turn_between(from: f32, to: f32) -> f32 {
    (to - from + std::f32::consts::PI).rem_euclid(std::f32::consts::TAU) - std::f32::consts::PI
}

/// `from` turned towards `to` at `rate` radians a second for `seconds`, no further than `to`.
fn turned(from: f32, to: f32, rate: f32, seconds: f32) -> f32 {
    let turn = turn_between(from, to);
    from + turn.signum() * turn.abs().min(rate * seconds.max(0.0))
}

/// A walk, as it is planned: when it turns and begins to walk, when it stops with both feet down, and how far it goes.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Walk {
    pub direction: Vec3,
    pub heading: f32,
    /// When it begins to walk, having turned, and when it stops, in milliseconds.
    pub walks: u64,
    pub stops: u64,
    pub distance: f32,
}

impl Behaviour {
    /// The walk this is, if it is one, for `chicken`, walking at `pace` times its walk's own speed: it turns to face
    /// where it walks, then walks until both its feet are down at or past the distance, as far as its walk's root
    /// motion carries it.
    pub fn walk(&self, chicken: &Chicken, pace: f32) -> Option<Walk> {
        let Doing::Walk { to } = self.doing else {
            return None;
        };
        let offset = Vec3::from(to) - Vec3::from(self.at);
        let flat = Vec2::new(offset.x, offset.z);
        let heading = if flat.length() > 1e-4 {
            heading(flat)
        } else {
            self.heading
        };
        let turning = turn_between(self.heading, heading).abs() / TURN;
        let wanted = offset.length();
        // Its feet are both down at the start of each loop and halfway through it.
        let loop_seconds = chicken.loop_duration();
        let half = chicken.walk.at(loop_seconds / 2.0);
        let stride = chicken.stride();
        let mut halves = 0u32;
        let mut distance = 0.0;
        while distance < wanted - 1e-3 && halves < 10_000 {
            halves += 1;
            distance = (halves / 2) as f32 * stride + if halves % 2 == 1 { half } else { 0.0 };
        }
        // Moments a server shares are untrusted on its clients, so they saturate rather than overflow.
        let walks = self.since.saturating_add((turning * 1000.0).round() as u64);
        let seconds = halves as f32 * loop_seconds / 2.0 / pace;
        Some(Walk {
            direction: offset.normalize_or_zero(),
            heading,
            walks,
            stops: walks.saturating_add((seconds * 1000.0).round() as u64),
            distance,
        })
    }

    /// When it ends of its own accord, if it does: a walk as it stops, a peck or a look as its moment comes, a recovery
    /// once its head is straight, and a sprint once it has stopped.
    pub fn ends(&self, chicken: &Chicken, pace: f32) -> Option<u64> {
        match self.doing {
            Doing::Walk { .. } => self.walk(chicken, pace).map(|walk| walk.stops),
            Doing::Peck { until } | Doing::Look { until, .. } => Some(until),
            Doing::Recover => Some(self.since.saturating_add(RECOVERING)),
            Doing::Sprint { .. } => Some(self.since.saturating_add(
                (chicken.sprint.step * (chicken.sprint.distances.len() - 1) as f32 * 1000.0).round()
                    as u64,
            )),
            Doing::Stand | Doing::Confused { .. } | Doing::Roost => None,
        }
    }

    /// Where it is and how it stands at `now`, for `chicken`, walking at `pace`.
    pub fn place(&self, chicken: &Chicken, pace: f32, now: u64) -> Placed {
        let at = Vec3::from(self.at);
        let elapsed = now.saturating_sub(self.since) as f32 / 1000.0;
        let still = |heading| Placed {
            position: at,
            heading,
            lean: 0.0,
        };
        match self.doing {
            Doing::Stand | Doing::Roost | Doing::Recover => still(self.heading),
            Doing::Peck { until } => {
                let into = now.min(until).saturating_sub(self.since) % PECK_MILLIS;
                let down = (into as f32 / PECK_MILLIS as f32 * std::f32::consts::TAU)
                    .sin()
                    .max(0.0);
                Placed {
                    lean: PECK * down,
                    ..still(self.heading)
                }
            }
            Doing::Look { side, until } => {
                let length = until.saturating_sub(self.since).max(1) as f32 / 1000.0;
                let progress = (elapsed / length).min(1.0);
                still(self.heading + side * (progress * std::f32::consts::PI).sin())
            }
            Doing::Confused { toward } => {
                let to = heading(Vec2::from(toward) - Vec2::new(at.x, at.z));
                still(turned(self.heading, to, TURN, elapsed))
            }
            Doing::Walk { .. } => {
                let walk = self.walk(chicken, pace).expect("a walk");
                let walking =
                    now.clamp(walk.walks, walk.stops).saturating_sub(walk.walks) as f32 / 1000.0;
                let (start, end) = crate::chicken::seconds(crate::chicken::WALK);
                let length = end - start;
                let played = walking * pace;
                let loops = (played / length).floor();
                let moved = (loops * chicken.stride() + chicken.walk.at(played - loops * length))
                    .min(walk.distance);
                Placed {
                    position: at + walk.direction * moved,
                    heading: if now < walk.walks {
                        turned(self.heading, walk.heading, TURN, elapsed)
                    } else {
                        walk.heading
                    },
                    lean: 0.0,
                }
            }
            Doing::Sprint { away } => {
                let to = heading(Vec2::from(away));
                let direction = Vec2::from(away).normalize_or_zero();
                let moved = chicken.sprint.at(elapsed);
                Placed {
                    position: at + Vec3::new(direction.x, 0.0, direction.y) * moved,
                    heading: turned(self.heading, to, SPIN, elapsed),
                    lean: 0.0,
                }
            }
        }
    }
}

/// How long a chicken takes to straighten its head when nobody points at it any more, in milliseconds.
pub const RECOVERING: u64 = 1_600;

#[cfg(test)]
mod tests;
