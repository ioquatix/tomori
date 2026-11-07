// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

//! The flock, as the farmyard's server side owns it: what each chicken does, decided here, from the space's identifier,
//! as each thing it does ends, and how it reacts when a participant points at it. Chickens wander the yard, peck, look
//! about, and now and then go up the ramp into the coop to roost, and come out again; a chicken pointed at is confused,
//! turning to whoever points, for as long as they do, and one pointed at long enough, or by two at once, is startled
//! into a sprint. Every change is a [`Behaviour`] with the moment it begins, which the side sends to every client.
use std::collections::{BTreeMap, VecDeque};
use std::sync::Arc;

use glam::{Vec2, Vec3};
use serde::{Deserialize, Serialize};

use crate::behaviour::{Behaviour, Doing, facing, heading};
use crate::chicken::Chicken;
use crate::layout::{self, Footprint};

/// The fewest and most chickens in a flock.
pub const FEWEST: usize = 3;
pub const MOST: usize = 5;
/// How far ahead of the server's clock a reaction begins, so that every client has it before it shows.
pub const LEAD: u64 = 150;
/// How long after a participant noticed a chicken they can notice it again, and how long after they noticed any chicken
/// they can notice another, so that sweeping the ray across the flock does not set them all clucking.
pub const PER_CHICKEN: u64 = 6_000;
pub const PER_PARTICIPANT: u64 = 1_500;
/// How long one participant points at a chicken before it is startled, and the longest it stays confused without being
/// told they looked away.
pub const STARTLE: u64 = 4_000;
pub const HOLD: u64 = 8_000;
/// A chicken is startled at most once in this long, so that it stays rare.
pub const UNSTARTLED: u64 = 60_000;
/// How far a startled chicken looks for a clear way to run, a little further than its sprint carries it.
const ESCAPE: f32 = 1.4;
/// The nearest a chicken's path comes to what stands in the yard, and to another chicken.
pub const CLEARANCE: f32 = 0.3;
pub const SPACING: f32 = 0.4;
/// How far from the middle of the space someone can stand, as a notice says; further is taken as here.
pub const REACH: f32 = 50.0;

/// The chickens' names, given in turn from one the space chooses.
pub const NAMES: [&str; 8] = [
    "Pip",
    "Clementine",
    "Hazel",
    "Mabel",
    "Nugget",
    "Biscuit",
    "Marigold",
    "Pepper",
];

/// The chance a chicken goes to roost in the coop, each time it chooses what to do, if nobody else is.
pub const VISITS: f32 = 0.08;

/// The part of the yard the chickens keep to: inside the fence, and short of where participants arrive.
pub const AREA: Footprint = Footprint::new([-4.4, -10.4], [4.4, -1.8]);

/// Where each chicken roosts inside the coop: two nests and the perch.
pub const ROOSTS: [[f32; 3]; 3] = [
    [-3.1, layout::COOP_FLOOR, -9.0],
    [-2.5, layout::COOP_FLOOR, -9.0],
    [-2.3, layout::COOP_FLOOR, -8.4],
];

/// The SplitMix64 finalizer, for numbers that are the same everywhere.
fn mix(value: u64) -> u64 {
    let mut z = value.wrapping_add(0x9e37_79b9_7f4a_7c15);
    z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    z ^ (z >> 31)
}

/// The seed of the flock in the space `space`: FNV-1a, the same in every build.
pub fn seed(space: &str) -> u64 {
    space.bytes().fold(0xcbf2_9ce4_8422_2325, |hash, byte| {
        (hash ^ u64::from(byte)).wrapping_mul(0x0000_0100_0000_01b3)
    })
}

/// One chicken of the flock, as its clients learn of it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Hen {
    pub name: String,
    pub cone: bool,
    /// How fast it walks, as a share of its walk's own speed.
    pub pace: f32,
    pub behaviour: Behaviour,
}

/// What the side keeps for whoever joins: the farmyard's clock as it was sent, and every chicken.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct State {
    pub clock: u64,
    pub chickens: Vec<Hen>,
}

/// A chicken that began doing something else, as the side sends it to everyone in the space.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Event {
    pub clock: u64,
    pub chicken: usize,
    pub behaviour: Behaviour,
}

/// What a participant asks of the flock: that a chicken notices them, from where they stand, `[x, z]`, for as long as
/// they point at it, or that it no longer does, as they look away.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Intent {
    Notice { notice: usize, toward: [f32; 2] },
    Away { away: usize },
}

/// What the flock keeps of each chicken beyond what clients learn.
#[derive(Clone, Debug)]
struct Keeping {
    /// Where it lives, which its wandering keeps it near.
    home: Vec2,
    /// What it does next, before it chooses anew, as a visit to the coop plans several things, each with how long it
    /// stays, if it stands or roosts.
    plan: VecDeque<(Doing, u64)>,
    /// Until when it stands, when it stands.
    rests: u64,
    /// When it was last startled.
    startled: Option<u64>,
    /// Who points at it, by their number, and since when.
    pointers: BTreeMap<u32, u64>,
}

/// When a participant last noticed each chicken, and any.
#[derive(Clone, Debug, Default)]
struct Pointer {
    latest: u64,
    chickens: [Option<u64>; MOST],
}

/// The flock of one space.
pub struct Flock {
    chicken: Arc<Chicken>,
    random: u64,
    hens: Vec<Hen>,
    kept: Vec<Keeping>,
    pointers: BTreeMap<u32, Pointer>,
}

impl Flock {
    /// The flock that `seed` gives, standing about their homes at `now`, for `chicken`.
    pub fn new(seed: u64, chicken: Arc<Chicken>, now: u64) -> Self {
        let count = FEWEST + (mix(seed) % (MOST - FEWEST + 1) as u64) as usize;
        let coned = (mix(seed ^ 4) % count as u64) as usize;
        let first = (mix(seed ^ 7) % NAMES.len() as u64) as usize;
        let mut flock = Self {
            chicken,
            random: mix(seed ^ 0x5eed),
            hens: Vec::new(),
            kept: Vec::new(),
            pointers: BTreeMap::new(),
        };
        for index in 0..count {
            let angle = index as f32 / count as f32 * std::f32::consts::TAU + flock.unit();
            let home = AREA.centre() + Vec2::new(angle.cos() * 2.4, angle.sin() * 2.4);
            let home = flock.clear_spot(home).unwrap_or(AREA.centre());
            let pace = 0.86 + 0.33 * flock.unit();
            let heading = flock.unit() * std::f32::consts::TAU;
            let rests = now + (flock.unit() * 3_000.0) as u64;
            flock.hens.push(Hen {
                name: NAMES[(first + index) % NAMES.len()].to_owned(),
                cone: index == coned,
                pace,
                behaviour: Behaviour {
                    since: now,
                    at: [home.x, 0.0, home.y],
                    heading,
                    doing: Doing::Stand,
                },
            });
            flock.kept.push(Keeping {
                home,
                plan: VecDeque::new(),
                rests,
                startled: None,
                pointers: BTreeMap::new(),
            });
        }
        flock
    }

    /// A number from 0 up to 1, the next of the flock's.
    fn unit(&mut self) -> f32 {
        self.random = mix(self.random);
        (self.random >> 40) as f32 / (1u64 << 24) as f32
    }

    /// The chickens, as clients learn of them.
    pub fn hens(&self) -> &[Hen] {
        &self.hens
    }

    /// The state for whoever joins, at `clock`.
    pub fn state(&self, clock: u64) -> State {
        State {
            clock,
            chickens: self.hens.clone(),
        }
    }

    /// Where chicken `index` is at `now`.
    pub fn position(&self, index: usize, now: u64) -> Vec3 {
        let hen = &self.hens[index];
        hen.behaviour.place(&self.chicken, hen.pace, now).position
    }

    /// Whether `point` is somewhere a chicken may stand on the ground: in the area, clear of everything in the yard.
    fn clear(point: Vec2) -> bool {
        AREA.contains(point)
            && layout::obstacles()
                .iter()
                .all(|obstacle| !obstacle.grown(CLEARANCE).contains(point))
    }

    /// The spot nearest `point` that is clear, searching outwards, if there is one near.
    fn clear_spot(&self, point: Vec2) -> Option<Vec2> {
        (0..40)
            .flat_map(|ring| {
                let radius = ring as f32 * 0.15;
                (0..8).map(move |step| {
                    let angle = step as f32 / 8.0 * std::f32::consts::TAU;
                    point + Vec2::new(angle.cos(), angle.sin()) * radius
                })
            })
            .find(|candidate| Self::clear(*candidate))
    }

    /// Whether a chicken can walk straight from `from` to `to` on the ground: clear of everything in the yard all the
    /// way, and of the other chickens but `index`, where they are and the whole of the way they are going, so that no
    /// two chickens' paths cross.
    fn path_clear(&self, index: usize, from: Vec2, to: Vec2, now: u64) -> bool {
        let steps = ((to - from).length() / 0.1).ceil().max(1.0) as u32;
        let others: Vec<(Vec2, Vec2)> = (0..self.hens.len())
            .filter(|other| *other != index)
            .map(|other| {
                let here = self.position(other, now);
                let there = match self.hens[other].behaviour.doing {
                    Doing::Walk { to } => Vec3::from(to),
                    _ => here,
                };
                (Vec2::new(here.x, here.z), Vec2::new(there.x, there.z))
            })
            .collect();
        (0..=steps).all(|step| Self::clear(from.lerp(to, step as f32 / steps as f32)))
            && others
                .iter()
                .all(|(here, there)| segments_apart((from, to), (*here, *there)) >= SPACING)
    }

    /// Let chicken `index` begin `doing` at `since`, from where it is then.
    fn begin(&mut self, index: usize, doing: Doing, since: u64) -> Behaviour {
        let hen = &self.hens[index];
        let placed = hen.behaviour.place(&self.chicken, hen.pace, since);
        let behaviour = Behaviour {
            since,
            at: placed.position.to_array(),
            heading: placed.heading,
            doing,
        };
        self.hens[index].behaviour = behaviour;
        behaviour
    }

    /// Choose what chicken `index` does next, at `now`, and how long it stays, if it stands or roosts.
    fn choose(&mut self, index: usize, now: u64) -> (Doing, u64) {
        if let Some(planned) = self.kept[index].plan.pop_front() {
            return planned;
        }
        let here = self.position(index, now);
        let here = Vec2::new(here.x, here.z);
        let roll = self.unit();
        // Now and then, a visit to the coop, if nobody else is there or on their way.
        let visiting = self.kept.iter().any(|kept| !kept.plan.is_empty())
            || self
                .hens
                .iter()
                .any(|hen| hen.behaviour.doing == Doing::Roost);
        // In front of the ramp, clear of it, then at its foot, and in at the door.
        let approach = [
            layout::RAMP_FOOT.x,
            0.0,
            layout::ramp().grown(CLEARANCE).high.y + 0.05,
        ];
        let foot = layout::RAMP_FOOT.to_array();
        if roll < VISITS
            && !visiting
            && self.path_clear(index, here, Vec2::new(approach[0], approach[2]), now)
        {
            let door = (layout::DOOR - Vec3::Z * 0.15).to_array();
            let stays = 20_000 + (self.unit() * 40_000.0) as u64;
            self.kept[index].plan.extend([
                (Doing::Walk { to: foot }, 0),
                (Doing::Walk { to: door }, 0),
                (
                    Doing::Walk {
                        to: ROOSTS[index % ROOSTS.len()],
                    },
                    0,
                ),
                (Doing::Roost, stays),
                (Doing::Walk { to: door }, 0),
                (Doing::Walk { to: foot }, 0),
                (Doing::Walk { to: approach }, 0),
            ]);
            return (Doing::Walk { to: approach }, 0);
        }
        if roll < 0.55 {
            // Somewhere near, nearer home the further it has strayed.
            let home = self.kept[index].home;
            for _ in 0..12 {
                let angle = self.unit() * std::f32::consts::TAU;
                let length = 0.5 + self.unit() * 1.6;
                let toward_home = (home - here) * (0.2 + 0.4 * self.unit());
                let to = here + toward_home + Vec2::new(angle.cos(), angle.sin()) * length;
                if self.path_clear(index, here, to, now) {
                    return (
                        Doing::Walk {
                            to: [to.x, 0.0, to.y],
                        },
                        0,
                    );
                }
            }
        } else if roll < 0.75 {
            let pecks = 3 + (self.unit() * 5.0) as u64;
            return (
                Doing::Peck {
                    until: now + pecks * crate::behaviour::PECK_MILLIS,
                },
                0,
            );
        } else if roll < 0.9 {
            let side = if self.unit() < 0.5 {
                crate::behaviour::LOOK
            } else {
                -crate::behaviour::LOOK
            };
            return (
                Doing::Look {
                    side,
                    until: now + 1_500 + (self.unit() * 1_000.0) as u64,
                },
                0,
            );
        }
        (Doing::Stand, 800 + (self.unit() * 2_400.0) as u64)
    }

    /// Move the flock on to `now`: each chicken whose doing has ended, and each confused one held long enough to be
    /// startled, or held too long, does what is next. Returns each change, in the order made.
    pub fn update(&mut self, now: u64) -> Vec<(usize, Behaviour)> {
        let mut changes = Vec::new();
        for index in 0..self.hens.len() {
            let behaviour = self.hens[index].behaviour;
            if behaviour.since > now {
                continue;
            }
            if let Doing::Confused { toward } = behaviour.doing {
                if let Some(changed) = self.react(index, Vec2::from(toward), now) {
                    changes.push((index, changed));
                }
                continue;
            }
            let ended = match behaviour.doing {
                Doing::Stand | Doing::Roost => self.kept[index].rests <= now,
                _ => behaviour
                    .ends(&self.chicken, self.hens[index].pace)
                    .is_some_and(|end| end <= now),
            };
            if !ended {
                continue;
            }
            let end = match behaviour.doing {
                Doing::Stand | Doing::Roost => self.kept[index].rests,
                _ => behaviour
                    .ends(&self.chicken, self.hens[index].pace)
                    .expect("checked above"),
            };
            // It begins the next as the last ended, so that it stops and starts where its feet are down.
            let start = end.max(behaviour.since);
            let (doing, stays) = self.choose(index, start);
            let begun = self.begin(index, doing, start);
            self.kept[index].rests = begun.since + stays;
            changes.push((index, begun));
        }
        changes
    }

    /// A chicken confused by someone at `toward`, at `now`: startled by one pointing at it long enough or two at once,
    /// unless it was startled lately; or recovered, if nobody has said they looked away for too long.
    fn react(&mut self, index: usize, toward: Vec2, now: u64) -> Option<Behaviour> {
        let since = self.hens[index].behaviour.since;
        let kept = &self.kept[index];
        let longest = kept
            .pointers
            .values()
            .map(|began| now.saturating_sub(*began))
            .max()
            .unwrap_or(0);
        let rested = kept
            .startled
            .is_none_or(|startled| now >= startled + UNSTARTLED);
        if rested && (kept.pointers.len() > 1 || longest >= STARTLE) {
            let away = self.escape(index, toward, now);
            self.kept[index].startled = Some(now);
            self.kept[index].pointers.clear();
            return Some(self.begin(
                index,
                Doing::Sprint {
                    away: away.to_array(),
                },
                now,
            ));
        }
        if now >= since + HOLD {
            self.kept[index].pointers.clear();
            return Some(self.begin(index, Doing::Recover, now));
        }
        None
    }

    /// The way chicken `index` runs from whoever points at it from `toward`: straight away, or as near that as is clear
    /// for as far as it runs, within 100° either side; or else whichever of those is clear furthest, the nearer straight
    /// away of two as clear.
    fn escape(&self, index: usize, toward: Vec2, now: u64) -> Vec2 {
        let here = self.position(index, now);
        let here = Vec2::new(here.x, here.z);
        let away = (here - toward).try_normalize().unwrap_or(facing(
            self.hens[index].behaviour.heading + std::f32::consts::PI,
        ));
        let ways = [0, 1, -1, 2, -2, 3, -3, 4, -4, 5, -5]
            .into_iter()
            .enumerate()
            .map(|(order, step)| {
                let direction = Vec2::from_angle(step as f32 * 0.349).rotate(away);
                let clear = (1..=14)
                    .take_while(|reach| {
                        let point = here + direction * (*reach as f32 * ESCAPE / 14.0);
                        Self::clear(point)
                            && (0..self.hens.len())
                                .filter(|other| *other != index)
                                .all(|other| {
                                    let there = self.position(other, now);
                                    Vec2::new(there.x, there.z).distance(point) >= SPACING
                                })
                    })
                    .count();
                (direction, std::cmp::Reverse(clear), order)
            });
        ways.min_by_key(|(_, clear, order)| (*clear, *order))
            .map(|(direction, ..)| direction)
            .expect("there are ways to run")
    }

    /// Participant `by` asks `intent` at `now`. A notice is taken only of a chicken the flock has, on the ground and
    /// not running, at most once in [`PER_CHICKEN`] from each participant for each chicken and once in
    /// [`PER_PARTICIPANT`] from each participant for any; looking away only from one they point at. Returns the change
    /// it makes, if any, beginning [`LEAD`] from now.
    pub fn ask(&mut self, by: u32, intent: Intent, now: u64) -> Option<(usize, Behaviour)> {
        match intent {
            Intent::Notice { notice, toward } => {
                let hen = self.hens.get(notice)?;
                let toward = Vec2::from(toward);
                let here = self.position(notice, now);
                let grounded = here.y < 0.01 && hen.behaviour.doing != Doing::Roost;
                let running = matches!(hen.behaviour.doing, Doing::Sprint { .. });
                if !toward.is_finite() || !grounded || running {
                    return None;
                }
                let pointer = self.pointers.entry(by).or_default();
                let recent = |then: Option<u64>, limit| then.is_some_and(|then| now < then + limit);
                if recent(pointer.chickens[notice], PER_CHICKEN)
                    || (pointer.latest > 0 && now < pointer.latest + PER_PARTICIPANT)
                {
                    return None;
                }
                pointer.chickens[notice] = Some(now);
                pointer.latest = now.max(1);
                self.kept[notice].pointers.insert(by, now);
                let toward = toward.clamp(Vec2::splat(-REACH), Vec2::splat(REACH));
                // Already confused, it turns to the latest to point.
                Some((
                    notice,
                    self.begin(
                        notice,
                        Doing::Confused {
                            toward: toward.to_array(),
                        },
                        now + LEAD,
                    ),
                ))
            }
            Intent::Away { away } => {
                let kept = self.kept.get_mut(away)?;
                kept.pointers.remove(&by)?;
                // Only a confused chicken has anyone pointing at it: startled or recovered, it forgets them.
                let alone = kept.pointers.is_empty();
                alone.then(|| (away, self.begin(away, Doing::Recover, now + LEAD)))
            }
        }
    }

    /// What the agent is told of each chicken at `now`, as a participant points at it: its name, its cone, what it is
    /// doing and where.
    pub fn describe(&self, now: u64) -> Vec<(String, String)> {
        (0..self.hens.len())
            .map(|index| {
                let hen = &self.hens[index];
                let here = self.position(index, now);
                let doing = match hen.behaviour.doing {
                    Doing::Stand | Doing::Recover => "standing",
                    Doing::Peck { .. } => "pecking at the ground",
                    Doing::Look { .. } => "looking about",
                    Doing::Walk { .. } if here.y > 0.01 => "on the coop's ramp",
                    Doing::Walk { .. } => "wandering",
                    Doing::Confused { .. } => "looking up, puzzled, at whoever is pointing at her",
                    Doing::Sprint { .. } => "running off, startled",
                    Doing::Roost => "roosting in the coop",
                };
                let cone = if hen.cone {
                    ", wearing a traffic cone"
                } else {
                    ""
                };
                format!(
                    "{}{cone}, a hen in the farmyard, {doing} {}",
                    hen.name,
                    near(here)
                )
            })
            .enumerate()
            .map(|(index, description)| (format!("chicken-{index}"), description))
            .collect()
    }
}

/// The distance from `point` to the segment from `from` to `to`.
fn to_segment(point: Vec2, (from, to): (Vec2, Vec2)) -> f32 {
    let along = to - from;
    let length = along.length_squared();
    let fraction = if length > 0.0 {
        ((point - from).dot(along) / length).clamp(0.0, 1.0)
    } else {
        0.0
    };
    point.distance(from + along * fraction)
}

/// The nearest two segments come to each other.
fn segments_apart(a: (Vec2, Vec2), b: (Vec2, Vec2)) -> f32 {
    let crossing = {
        let (d1, d2) = (a.1 - a.0, b.1 - b.0);
        let denominator = d1.perp_dot(d2);
        let t = (b.0 - a.0).perp_dot(d2) / denominator;
        let u = (b.0 - a.0).perp_dot(d1) / denominator;
        denominator != 0.0 && (0.0..=1.0).contains(&t) && (0.0..=1.0).contains(&u)
    };
    if crossing {
        return 0.0;
    }
    [
        to_segment(a.0, b),
        to_segment(a.1, b),
        to_segment(b.0, a),
        to_segment(b.1, a),
    ]
    .into_iter()
    .fold(f32::INFINITY, f32::min)
}

/// What `point` is nearest, in words.
fn near(point: Vec3) -> &'static str {
    let point = Vec2::new(point.x, point.z);
    let places = [
        (layout::COOP.centre(), "by the coop"),
        (layout::TROUGH.centre(), "by the water trough"),
        (layout::FEEDER, "by the feeder"),
        (layout::HAY.centre(), "by the hay"),
        (Vec2::new(0.0, layout::FENCE.high.y), "near the gate"),
    ];
    places
        .iter()
        .min_by(|a, b| a.0.distance(point).total_cmp(&b.0.distance(point)))
        .map(|(_, words)| *words)
        .expect("places")
}

/// The heading `[x, z]` from `from` to `to`.
pub fn bearing(from: Vec2, to: Vec2) -> f32 {
    heading(to - from)
}

#[cfg(test)]
mod tests;
