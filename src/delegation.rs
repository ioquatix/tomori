//! Bounded worker geometry. Positions belong to this client, never to the bridge.
//!
//! Live anchors do not move when metadata, ordering, or membership changes. Slots
//! are remembered for the current root; inactive slots may be reused under churn,
//! but a returning identity gets its remembered slot whenever it is still free.
use std::collections::{BTreeMap, BTreeSet};

use glam::{Quat, Vec3};
use serde::{Deserialize, Serialize};

use crate::swarm::CENTER;

pub const MAX_CUBES: usize = 4096;
const MAX_NODES: usize = 64;
const MAX_CARDS: usize = 16;
// Match the spectator's station ID bound.
const MAX_ID_BYTES: usize = 160;
const MAX_WORKERS: usize = MAX_NODES * 2;
const SLOT_COUNT: usize = MAX_NODES * 3;
const PARTICLES: usize = 24;
const MAX_COORDINATOR_SAMPLES: usize = 128;
const ARRIVAL_SECONDS: f32 = 1.5;
const DEPARTURE_SECONDS: f32 = 1.2;
const TERMINAL_SECONDS: f32 = 0.9;
// Local-frame 4×3×2 partition of the completed cube. Cube scales are full
// side lengths, not half extents, in both the local and spectator renderer.
const COMPLETED_SIDE: f32 = 0.24;
const COMPLETED_PIECE: Vec3 = Vec3::new(
    COMPLETED_SIDE / 4.0,
    COMPLETED_SIDE / 3.0,
    COMPLETED_SIDE / 2.0,
);
const SPLIT_SECONDS: f32 = 0.35;

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Workspace {
    pub nodes: Vec<Node>,
    pub presentations: Vec<Presentation>,
    pub error: Option<String>,
    pub truncated: bool,
    pub stale: bool,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Node {
    pub id: String,
    pub parent_id: Option<String>,
    pub title: String,
    pub state: State,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum State {
    Running,
    Waiting,
    Idle,
    Completed,
    Error,
    #[default]
    Unknown,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Presentation {
    pub id: String,
    pub source_id: String,
    pub source_message_id: String,
    pub title: String,
    pub kind: Kind,
    pub text: String,
    pub columns: Vec<String>,
    pub rows: Vec<Vec<String>>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    #[default]
    Markdown,
    Table,
}

impl Workspace {
    /// Structural validation deliberately allows absent parents (including the
    /// coordinator). A partial archive is not evidence of a new root edge.
    pub fn valid(&self) -> bool {
        if self.nodes.len() > MAX_NODES
            || self.presentations.len() > MAX_CARDS
            || self.error.as_ref().is_some_and(|s| s.len() > 4096)
        {
            return false;
        }
        let mut nodes = BTreeMap::new();
        for node in &self.nodes {
            if !valid_id(&node.id)
                || node.parent_id.as_ref().is_some_and(|id| !valid_id(id))
                || node.title.chars().count() > 160
                || nodes.insert(node.id.as_str(), node).is_some()
            {
                return false;
            }
        }
        // At most 64 links per start, including self-parent cycles. No recursion
        // or unbounded traversal of untrusted parent references.
        for node in &self.nodes {
            let mut current = node.id.as_str();
            let mut seen = BTreeSet::new();
            while let Some(node) = nodes.get(current) {
                if !seen.insert(current) {
                    return false;
                }
                let Some(parent) = node.parent_id.as_deref() else {
                    break;
                };
                current = parent;
            }
        }
        let mut cards = BTreeSet::new();
        for card in &self.presentations {
            if !valid_id(&card.id)
                || !cards.insert(&card.id)
                || card.source_id.len() > MAX_ID_BYTES
                || card.source_message_id.len() > MAX_ID_BYTES
                || card.title.chars().count() > 160
                || card.text.len() > 4096
                || card.columns.len() > 6
                || card.columns.iter().any(|s| s.len() > 80)
                || card.rows.len() > 16
                || card.rows.iter().any(|row| {
                    row.len() > 6
                        || row.iter().any(|s| s.len() > 160)
                        || (card.kind == Kind::Table && row.len() != card.columns.len())
                })
            {
                return false;
            }
        }
        true
    }
}

fn valid_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= MAX_ID_BYTES
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub struct Cube {
    pub position: [f32; 3],
    pub scale: [f32; 3],
    pub color: [f32; 3],
    /// XYZW unit quaternion; old spectator frames remain axis-aligned.
    #[serde(default = "identity_orientation")]
    pub orientation: [f32; 4],
}

fn identity_orientation() -> [f32; 4] {
    Quat::IDENTITY.to_array()
}

impl Cube {
    /// Also suitable for validating geometry received by the spectator.
    pub fn valid(&self) -> bool {
        self.position
            .iter()
            .all(|v| v.is_finite() && v.abs() <= 100.0)
            && self
                .scale
                .iter()
                .all(|v| v.is_finite() && *v > 0.0 && *v <= 1.0)
            && self
                .color
                .iter()
                .all(|v| v.is_finite() && (0.0..=1.0).contains(v))
            && self
                .orientation
                .iter()
                .all(|v| v.is_finite() && v.abs() <= 1.0)
            && (Quat::from_array(self.orientation).length_squared() - 1.0).abs() <= 0.0001
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Station {
    pub id: String,
    pub position: [f32; 3],
    pub state: State,
    pub title: String,
    #[serde(default = "full_opacity")]
    pub opacity: f32,
}

fn full_opacity() -> f32 {
    1.0
}

impl Station {
    #[cfg(any(not(target_os = "android"), test))]
    pub fn valid(&self) -> bool {
        valid_id(&self.id)
            && self.title.chars().count() <= 160
            && self
                .position
                .iter()
                .all(|v| v.is_finite() && v.abs() <= 100.0)
            && self.opacity.is_finite()
            && (0.0..=1.0).contains(&self.opacity)
    }
}

struct Worker {
    slot: usize,
    parent_id: Option<String>,
    title: String,
    state: State,
    // The observed form is independent of temporary Unknown display status
    // caused by stale or incomplete evidence.
    shape_state: State,
    continuous_evidence: bool,
    #[cfg(test)]
    origin: Vec3,
    anchor: Vec3,
    particles: [Vec3; PARTICLES],
    particle_scales: [Vec3; PARTICLES],
    flight_start: [Vec3; PARTICLES],
    scale_start: [Vec3; PARTICLES],
    return_end: [Vec3; PARTICLES],
    color: Vec3,
    age: f32,
    arrival_seconds: f32,
    flying: bool,
    terminal_motion: Option<TerminalMotion>,
    retired: Option<f32>,
    departure_seconds: f32,
    visual_opacity: f32,
    seed: f32,
    orientation: Quat,
    orientation_start: Quat,
    ambient_time: f64,
}

#[derive(Clone, Copy)]
struct TerminalMotion {
    elapsed: f32,
    duration: f32,
    bounce: bool,
}

impl Worker {
    fn begin_terminal(&mut self, bounce: bool) {
        self.flying = false;
        self.flight_start = self.particles;
        self.scale_start = self.particle_scales;
        let target = std::array::from_fn(|i| terminal_goal(self, i));
        self.terminal_motion = Some(TerminalMotion {
            elapsed: 0.0,
            duration: flight_duration(&self.flight_start, &target, TERMINAL_SECONDS),
            bounce,
        });
    }
}

/// Session-local layout and animation; no bridge mutation or progress estimates.
pub struct Scene {
    root: Option<String>,
    // Small, bounded slot history, reset at every root switch.
    slots: BTreeMap<String, usize>,
    workers: BTreeMap<String, Worker>,
    cubes: Vec<Cube>,
    stations: Vec<Station>,
    time: f64,
    previous_trusted: bool,
    coordinator_samples: [Vec3; MAX_COORDINATOR_SAMPLES],
    coordinator_sample_count: usize,
}

impl Default for Scene {
    fn default() -> Self {
        Self::new()
    }
}

impl Scene {
    pub fn new() -> Self {
        Self {
            root: None,
            slots: BTreeMap::new(),
            workers: BTreeMap::new(),
            cubes: Vec::with_capacity(MAX_CUBES),
            stations: Vec::with_capacity(MAX_WORKERS),
            time: 0.0,
            previous_trusted: false,
            coordinator_samples: [CENTER; MAX_COORDINATOR_SAMPLES],
            coordinator_sample_count: 0,
        }
    }

    pub fn cubes(&self) -> &[Cube] {
        &self.cubes
    }

    #[cfg(test)]
    pub fn snapshot(&self) -> Vec<Cube> {
        self.cubes.clone()
    }

    pub fn stations(&self) -> &[Station] {
        &self.stations
    }

    /// Copy at most 128 current coordinator cube positions, without allocating.
    /// Invalid or out-of-bounds positions (coordinates outside ±90 m) are ignored.
    /// An empty/invalid input restores the CENTER fallback. Call before update.
    pub fn set_coordinator_samples(&mut self, positions: &[[f32; 3]]) {
        self.coordinator_sample_count = 0;
        for position in positions.iter().take(MAX_COORDINATOR_SAMPLES) {
            let position = Vec3::from_array(*position);
            if position.is_finite() && position.abs().max_element() <= 90.0 {
                self.coordinator_samples[self.coordinator_sample_count] = position;
                self.coordinator_sample_count += 1;
            }
        }
    }

    /// `workspace` must be the bridge projection for `root`. Its contract has
    /// no root identifier, so detached orphans cannot independently prove
    /// which root they belong to. A root change always clears prior state.
    pub fn update(&mut self, root: Option<&str>, workspace: &Workspace, dt: f32) {
        let root = root.filter(|id| valid_id(id));
        if self.root.as_deref() != root {
            self.root = root.map(str::to_owned);
            self.slots.clear();
            self.workers.clear();
            self.time = 0.0;
            self.previous_trusted = false;
        }
        self.cubes.clear();
        self.stations.clear();
        let Some(root) = root else { return };
        // NaN and resume stalls cannot poison the simulation or skip arrivals.
        let dt = if dt.is_finite() {
            dt.clamp(0.0, 0.05)
        } else {
            0.0
        };
        self.time += f64::from(dt);
        let valid = workspace.valid();
        let fresh = valid && !workspace.stale;
        let trusted = fresh && workspace.error.is_none() && !workspace.truncated;
        let launch_new = trusted && self.previous_trusted;
        if trusted {
            self.reconcile(root, &workspace.nodes, launch_new, None);
        } else if fresh {
            // Partial discovery still contains real observed workers. Preserve
            // existing stations when omissions aren't authoritative, and add
            // newly observed ones up to the same fixed budget. Omitted workers
            // become unknown; observed workers retain their independent states.
            let mut merged = BTreeMap::<String, Node>::new();
            for (id, worker) in &self.workers {
                if worker.retired.is_none() {
                    merged.insert(
                        id.clone(),
                        Node {
                            id: id.clone(),
                            parent_id: worker.parent_id.clone(),
                            title: worker.title.clone(),
                            state: State::Unknown,
                        },
                    );
                }
            }
            let candidates: BTreeMap<_, _> = workspace
                .nodes
                .iter()
                .map(|node| (node.id.as_str(), node))
                .collect();
            let mut observed = BTreeSet::new();
            for node in &workspace.nodes {
                // Only accept partial nodes whose observed parent chain reaches
                // this root. Stale projections are never merged at all.
                let mut parent = node.parent_id.as_deref();
                let mut reaches_root = false;
                for _ in 0..MAX_NODES {
                    match parent {
                        Some(id) if id == root => {
                            reaches_root = true;
                            break;
                        }
                        Some(id) => {
                            parent = candidates
                                .get(id)
                                .and_then(|next| next.parent_id.as_deref())
                        }
                        None => break,
                    }
                }
                if !reaches_root {
                    continue;
                }
                if merged.contains_key(&node.id) || merged.len() < MAX_NODES {
                    merged.insert(node.id.clone(), node.clone());
                    observed.insert(node.id.as_str());
                }
            }
            self.reconcile(
                root,
                &merged.into_values().collect::<Vec<_>>(),
                false,
                Some(&observed),
            );
        } else {
            for worker in self.workers.values_mut() {
                worker.continuous_evidence = false;
            }
        }
        self.previous_trusted = trusted;
        // Only stale or invalid status evidence is globally unknown. A fresh
        // partial hierarchy cannot remove workers, but can report their states.
        let time = self.time;
        let mut retiring_stations = Vec::new();
        for (id, worker) in &mut self.workers {
            // Only a fully fitted completed form rotates as a rigid body. Its
            // hidden partition follows exactly, so splitting never snaps back
            // into an axis-aligned frame. No metadata refresh resets this clock.
            let was_solid = solid_completed(worker);
            if dt > 0.0 && worker.retired.is_none() {
                worker.ambient_time += f64::from(dt);
                if was_solid {
                    worker.orientation =
                        (Quat::from_axis_angle(Vec3::new(0.35, 1.0, 0.2).normalize(), 0.09 * dt)
                            * worker.orientation)
                            .normalize();
                    worker.particles = std::array::from_fn(|i| terminal_goal(worker, i));
                } else if worker.shape_state != State::Completed {
                    worker.orientation = worker
                        .orientation
                        .slerp(Quat::IDENTITY, 1.0 - (-2.0 * dt).exp())
                        .normalize();
                }
            } else if dt > 0.0
                && let Some(retired) = worker.retired
            {
                worker.orientation = worker
                    .orientation_start
                    .slerp(
                        Quat::IDENTITY,
                        smooth(
                            ((retired + if trusted { dt } else { 0.0 }) / SPLIT_SECONDS).min(1.0),
                        ),
                    )
                    .normalize();
            }
            worker.age = (worker.age + dt).min(3600.0);
            if trusted && let Some(retired) = &mut worker.retired {
                *retired += dt;
            }
            let state = if fresh { worker.state } else { State::Unknown };
            let target_color = palette(state);
            worker.color = worker.color.lerp(target_color, 1.0 - (-6.0 * dt).exp());
            let opacity = opacity(worker) * if fresh { 1.0 } else { 0.55 };
            if worker.retired.is_none()
                && let Some(motion) = &mut worker.terminal_motion
            {
                motion.elapsed = (motion.elapsed + dt).min(motion.duration);
            }
            let terminal_motion = worker.terminal_motion;
            let flying = worker.flying;
            let target_opacity = if let Some(retired) = worker.retired {
                1.0 - smooth(((retired - (worker.departure_seconds - 0.3)) / 0.3).clamp(0.0, 1.0))
            } else {
                1.0
            } * if fresh { 1.0 } else { 0.55 };
            // In particular, do not snap back to full size if a partially
            // faded departure is resurrected, or evidence becomes stale.
            worker.visual_opacity +=
                (target_opacity - worker.visual_opacity).clamp(-4.0 * dt, 4.0 * dt);
            let goals: [Vec3; PARTICLES] = std::array::from_fn(|i| terminal_goal(worker, i));
            for (i, position) in worker.particles.iter_mut().enumerate() {
                let goal = if is_terminal(worker.shape_state) {
                    // Goals are precomputed to avoid borrowing the worker
                    // while mutating its particle array.
                    goals[i]
                } else {
                    worker.anchor + flock_offset(i, worker.seed, time, state)
                };
                if let Some(retired) = worker.retired {
                    let t = (retired / worker.departure_seconds).min(1.0);
                    *position = worker.flight_start[i].lerp(worker.return_end[i], smooth(t))
                        + flight_arc(i, worker.seed, t, 0.20);
                } else if let Some(motion) = terminal_motion {
                    let t = (motion.elapsed / motion.duration).min(1.0);
                    let bounce = if motion.bounce {
                        0.06 * (std::f32::consts::TAU * t).sin() * (1.0 - t)
                    } else {
                        0.0
                    };
                    *position = worker.flight_start[i].lerp(goal, smooth(t)) + Vec3::Y * bounce;
                } else if flying {
                    let t = (worker.age / worker.arrival_seconds).min(1.0);
                    let next = worker.flight_start[i].lerp(goal, smooth(t))
                        + flight_arc(i, worker.seed, t, 0.35);
                    // A status goal may change mid-flight. Bound the adjustment
                    // rather than jumping to the new trajectory at the same t.
                    *position += (next - *position).clamp_length_max(6.0 * dt);
                } else if is_terminal(worker.shape_state) {
                    // A terminal worker first seen in flight may need a final
                    // small correction. Error goals carry only a slow wave.
                    let delta = goal - *position;
                    if delta.length() <= 0.0001 && delta.length() <= 6.0 * dt {
                        *position = goal;
                    } else {
                        *position +=
                            (delta * (1.0 - (-12.0 * dt).exp())).clamp_length_max(6.0 * dt);
                    }
                } else {
                    let change = ((goal - *position) * (1.0 - (-12.0 * dt).exp()))
                        .clamp_length_max(6.0 * dt);
                    *position += change;
                }
                let small = particle_scale(i);
                let goal_scale = if worker.shape_state == State::Completed {
                    COMPLETED_PIECE
                } else {
                    small
                };
                let scale = &mut worker.particle_scales[i];
                if let Some(retired) = worker.retired {
                    *scale = worker.scale_start[i]
                        .lerp(small, smooth((retired / SPLIT_SECONDS).min(1.0)));
                } else if let Some(motion) = terminal_motion {
                    *scale = worker.scale_start[i].lerp(
                        goal_scale,
                        smooth((motion.elapsed / motion.duration).min(1.0)),
                    );
                } else if flying {
                    let t = (worker.age / worker.arrival_seconds).min(1.0);
                    *scale = worker.scale_start[i]
                        .lerp(goal_scale, smooth(((t - 0.65) / 0.35).clamp(0.0, 1.0)));
                } else {
                    *scale = scale.lerp(goal_scale, 1.0 - (-12.0 * dt).exp());
                    // A resurrection can interrupt the split just before its
                    // target; a zero-time reconciliation must not snap it.
                    if dt > 0.0 && scale.distance(goal_scale) < 0.00001 {
                        *scale = goal_scale;
                    }
                }
            }
            if flying && worker.age >= worker.arrival_seconds {
                worker.flying = false;
            }
            if terminal_motion.is_some_and(|motion| motion.elapsed >= motion.duration) {
                worker.terminal_motion = None;
            }
            let color = worker.color * (0.70 + 0.30 * worker.visual_opacity);
            if solid_completed(worker) {
                push_cube(
                    &mut self.cubes,
                    worker.anchor,
                    Vec3::splat(COMPLETED_SIDE * worker.visual_opacity),
                    color,
                    worker.orientation,
                );
            } else {
                // Scale the whole geometry about its centroid when fading. This
                // preserves a fitted partition, including at the solid/split
                // boundary, rather than opening gaps between shrinking pieces.
                let center = worker.particles.iter().copied().sum::<Vec3>() / PARTICLES as f32;
                for (position, scale) in worker.particles.iter().zip(worker.particle_scales) {
                    let position = if worker.visual_opacity == 1.0 {
                        *position
                    } else {
                        center + (*position - center) * worker.visual_opacity
                    };
                    push_cube(
                        &mut self.cubes,
                        position,
                        scale * worker.visual_opacity,
                        color,
                        worker.orientation,
                    );
                }
            }
            let station = Station {
                id: id.clone(),
                position: worker.anchor.to_array(),
                state,
                title: worker.title.clone(),
                opacity,
            };
            if worker.retired.is_none() {
                self.stations.push(station);
            } else {
                retiring_stations.push(station);
            }
        }
        // Prefer live stations when departures and arrivals overlap.
        let remaining = MAX_WORKERS.saturating_sub(self.stations.len());
        self.stations
            .extend(retiring_stations.into_iter().take(remaining));
        self.workers
            .retain(|_, worker| worker.retired.is_none_or(|t| t < worker.departure_seconds));
        // Only worker flocks are rendered. Without interaction telemetry, links
        // would imply communication that this projection cannot establish.
        // At most 128 workers (including departures) emit 24 cubes each.
    }

    fn reconcile(
        &mut self,
        root: &str,
        nodes: &[Node],
        launch_new: bool,
        observed: Option<&BTreeSet<&str>>,
    ) {
        // A parentless entry can describe the root, but is not evidence that an
        // unrelated session belongs to this workspace.
        let nodes: BTreeMap<_, _> = nodes
            .iter()
            .filter(|n| n.id != root && n.parent_id.is_some())
            .map(|n| (n.id.as_str(), n))
            .collect();
        for (id, worker) in &mut self.workers {
            if let Some(node) = nodes.get(id.as_str()) {
                worker.parent_id.clone_from(&node.parent_id);
                worker.title.clone_from(&node.title);
                worker.state = node.state;
                let resurrected = worker.retired.take().is_some();
                let actually_observed = observed.is_none_or(|ids| ids.contains(id.as_str()));
                if actually_observed && node.state != State::Unknown {
                    if worker.shape_state != node.state {
                        let was_terminal = is_terminal(worker.shape_state);
                        worker.shape_state = node.state;
                        if is_terminal(node.state) {
                            worker.begin_terminal(worker.continuous_evidence && !resurrected);
                        } else {
                            worker.terminal_motion = None;
                            if was_terminal {
                                worker.flying = false;
                            }
                        }
                    }
                    worker.continuous_evidence = true;
                } else {
                    worker.continuous_evidence = false;
                    // An explicitly observed Unknown is a real display state,
                    // unlike a synthetic Unknown caused by an omission. Flow
                    // back to its flock without inventing a later completion.
                    if actually_observed && node.state == State::Unknown {
                        if is_terminal(worker.shape_state) {
                            worker.flying = false;
                        }
                        worker.shape_state = State::Unknown;
                        worker.terminal_motion = None;
                    }
                }
                if resurrected {
                    // Re-form without an invented status bounce, starting at
                    // the exact position reached by the interrupted return.
                    worker.flying = false;
                    if is_terminal(worker.shape_state) {
                        worker.begin_terminal(false);
                    }
                }
            } else if worker.retired.is_none() {
                worker.retired = Some(0.0);
                worker.continuous_evidence = false;
                worker.terminal_motion = None;
                worker.flight_start = worker.particles;
                worker.scale_start = worker.particle_scales;
                worker.orientation_start = worker.orientation;
                worker.return_end = std::array::from_fn(|i| {
                    sample(
                        &self.coordinator_samples,
                        self.coordinator_sample_count,
                        id,
                        i,
                    )
                });
                worker.departure_seconds =
                    flight_duration(&worker.flight_start, &worker.return_end, DEPARTURE_SECONDS);
            }
        }
        // Reserve room for authoritative nodes first. Keep at most 64 departures;
        // extreme churn drops the oldest departure rather than overflowing slots.
        let mut retiring: Vec<_> = self
            .workers
            .iter()
            .filter_map(|(id, w)| w.retired.map(|age| (id.clone(), age)))
            .collect();
        retiring.sort_by(|a, b| b.1.total_cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        for (id, age) in retiring
            .iter()
            .take(retiring.len().saturating_sub(MAX_NODES))
        {
            let _ = age;
            self.workers.remove(id);
        }
        self.workers
            .retain(|_, w| w.retired.is_none_or(|t| t < w.departure_seconds));

        let mut required = BTreeSet::new();
        for node in nodes.values() {
            required.insert(node.id.as_str());
            if let Some(parent) = node.parent_id.as_deref().filter(|parent| *parent != root) {
                required.insert(parent);
            }
        }
        // Retain every active identity and currently referenced parent, then
        // evict excess old history deterministically rather than growing forever.
        let mut spare = SLOT_COUNT.saturating_sub(
            required.len().saturating_add(
                self.workers
                    .keys()
                    .filter(|id| !required.contains(id.as_str()))
                    .count(),
            ),
        );
        self.slots.retain(|id, _| {
            if required.contains(id.as_str()) || self.workers.contains_key(id) {
                true
            } else if spare > 0 {
                spare -= 1;
                true
            } else {
                false
            }
        });
        let mut order: Vec<_> = required.into_iter().collect();
        // Protect remembered absent-parent provenance, but do not let new
        // absent-parent slots consume the best positions before real workers.
        order.sort_by_key(|id| {
            let priority = if !nodes.contains_key(id) && self.slots.contains_key(*id) {
                0
            } else if nodes.contains_key(id) {
                1
            } else {
                2
            };
            (priority, depth(id, root, &nodes), *id)
        });
        let mut occupied = [false; SLOT_COUNT];
        for worker in self.workers.values() {
            occupied[worker.slot] = true;
        }
        for id in order {
            if self.workers.contains_key(id) {
                continue;
            }
            let remembered = self.slots.get(id).copied().filter(|slot| !occupied[*slot]);
            let parent = nodes
                .get(id)
                .and_then(|n| n.parent_id.as_deref())
                .filter(|parent| *parent != root)
                .and_then(|parent| self.slots.get(parent).copied());
            let slot = remembered.or_else(|| {
                (0..SLOT_COUNT)
                    .filter(|slot| !occupied[*slot])
                    .min_by(|a, b| {
                        // Fill each eight-place tier before spilling into the
                        // next. The first tier alternates sides even for a
                        // chain of children; later tiers favor nearby parents.
                        let tier = |slot: usize| slot / 8;
                        let score = |slot| match parent {
                            Some(parent) if tier(slot) > 0 => {
                                slot_position(slot).distance_squared(slot_position(parent))
                            }
                            _ => slot as f32,
                        };
                        tier(*a)
                            .cmp(&tier(*b))
                            .then_with(|| score(*a).total_cmp(&score(*b)))
                            .then_with(|| a.cmp(b))
                    })
            });
            // 64 workers + 64 departures + 64 absent parents fit exactly.
            let Some(slot) = slot else { continue };
            occupied[slot] = true;
            self.slots.insert(id.to_owned(), slot);
        }
        for (&id, &node) in &nodes {
            if self.workers.contains_key(id) {
                continue;
            }
            let Some(&slot) = self.slots.get(id) else {
                continue;
            };
            let Some(parent_id) = node.parent_id.as_deref() else {
                continue;
            };
            let origin = if parent_id == root {
                CENTER
            } else if let Some(&parent_slot) = self.slots.get(parent_id) {
                slot_position(parent_slot)
            } else {
                continue;
            };
            // A cached missing-parent slot preserves provenance, but is not an
            // observed station from which an outbound event can be shown.
            let launch = launch_new
                && (parent_id == root
                    || nodes.contains_key(parent_id)
                    || self
                        .workers
                        .get(parent_id)
                        .is_some_and(|parent| parent.retired.is_none()));
            let seed = (stable_hash(id) % 65536) as f32 / 65536.0 * std::f32::consts::TAU;
            let anchor = slot_position(slot);
            let particles = if !launch {
                std::array::from_fn(|i| {
                    if is_terminal(node.state) {
                        terminal_position(anchor, node.state, i)
                    } else {
                        anchor + flock_offset(i, seed, self.time, node.state)
                    }
                })
            } else if parent_id == root {
                std::array::from_fn(|i| {
                    sample(
                        &self.coordinator_samples,
                        self.coordinator_sample_count,
                        id,
                        i,
                    )
                })
            } else {
                [origin; PARTICLES]
            };
            let arrival_seconds = if launch {
                flight_duration(&particles, &[anchor; PARTICLES], ARRIVAL_SECONDS)
            } else {
                ARRIVAL_SECONDS
            };
            let particle_scales = std::array::from_fn(|i| {
                if !launch && node.state == State::Completed {
                    COMPLETED_PIECE
                } else {
                    particle_scale(i)
                }
            });
            self.workers.insert(
                id.to_owned(),
                Worker {
                    slot,
                    parent_id: node.parent_id.clone(),
                    title: node.title.clone(),
                    state: node.state,
                    shape_state: node.state,
                    continuous_evidence: node.state != State::Unknown,
                    #[cfg(test)]
                    origin,
                    anchor,
                    particles,
                    particle_scales,
                    flight_start: particles,
                    scale_start: particle_scales,
                    return_end: [CENTER; PARTICLES],
                    color: palette(node.state),
                    age: if launch { 0.0 } else { arrival_seconds },
                    arrival_seconds,
                    flying: launch,
                    terminal_motion: None,
                    retired: None,
                    departure_seconds: DEPARTURE_SECONDS,
                    // Spawn at full particle size on the actual coordinator
                    // samples. Centroid scaling must not collapse those source
                    // positions during an invented arrival fade.
                    visual_opacity: 1.0,
                    seed,
                    orientation: Quat::IDENTITY,
                    orientation_start: Quat::IDENTITY,
                    ambient_time: 0.0,
                },
            );
        }
    }
}

fn depth<'a>(mut id: &'a str, root: &str, nodes: &BTreeMap<&str, &'a Node>) -> usize {
    let mut depth = 0;
    while id != root && depth < MAX_NODES {
        let Some(node) = nodes.get(id) else { break };
        depth += 1;
        let Some(parent) = node.parent_id.as_deref() else {
            break;
        };
        id = parent;
    }
    depth
}

fn stable_hash(id: &str) -> u64 {
    id.bytes().fold(0xcbf2_9ce4_8422_2325, |h, byte| {
        (h ^ u64::from(byte)).wrapping_mul(0x100_0000_01b3)
    })
}

/// World-anchored polar arc around the viewer's recenter home (0, 0 in XZ).
/// Sixteen angular columns, at least 18⅔ degrees apart, and twelve 55 cm rows.
/// All 192 remembered slots have distinct sightlines, not hidden radial layers.
/// Early slots flank the coordinator; large workspaces require looking around
/// and up. This is fixed scene-home geometry, never a head-locked overlay.
pub(crate) fn slot_position(slot: usize) -> Vec3 {
    // Fill two comfortable front columns and two rows before wrapping around
    // the observer. Slot identity is permanent, independent of membership size.
    let lane = slot % 4 + (slot % 32) / 8 * 4;
    let side = if lane.is_multiple_of(2) { -1.0 } else { 1.0 };
    let column = lane / 2;
    let row = (slot / 32) * 2 + (slot % 8) / 4;
    let radius = 3.2;
    // Keep the entire coordinator cone clear, including the last columns.
    // Sixteen columns span the remaining 280 degrees, including the rear gap.
    let angle = (40.0 + column as f32 * (280.0 / 15.0)).to_radians();
    let height = [
        1.35, 1.90, 0.80, 2.45, 0.25, 3.0, 3.55, 4.10, 4.65, 5.20, 5.75, 6.30,
    ][row];
    Vec3::new(side * radius * angle.sin(), height, -radius * angle.cos())
}

fn sample(samples: &[Vec3; MAX_COORDINATOR_SAMPLES], count: usize, id: &str, i: usize) -> Vec3 {
    if count == 0 {
        CENTER
    } else {
        samples[(stable_hash(id) as usize).wrapping_add(i) % count]
    }
}

fn flight_arc(i: usize, seed: f32, t: f32, height: f32) -> Vec3 {
    let arc = (std::f32::consts::PI * t).sin();
    Vec3::new(
        (seed + i as f32 * 2.4).sin() * 0.07,
        height + (i % 5) as f32 * 0.015,
        (seed + i as f32 * 2.4).cos() * 0.07,
    ) * arc
}

fn flight_duration(start: &[Vec3; PARTICLES], end: &[Vec3; PARTICLES], minimum: f32) -> f32 {
    let distance = start
        .iter()
        .zip(end)
        .map(|(a, b)| a.distance(*b))
        .fold(0.0_f32, f32::max);
    // Smoothstep's peak derivative is 1.5. Allow room for the arcing path
    // and flock offsets, even for an unusually distant coordinator sample.
    minimum.max((distance + 0.2) * 1.5 / 4.0)
}

fn smooth(t: f32) -> f32 {
    t * t * (3.0 - 2.0 * t)
}

fn opacity(worker: &Worker) -> f32 {
    smooth((worker.age / 0.35).min(1.0))
        * (1.0 - smooth((worker.retired.unwrap_or(0.0) / worker.departure_seconds).min(1.0)))
}

fn is_terminal(state: State) -> bool {
    matches!(state, State::Completed | State::Error)
}

fn particle_scale(i: usize) -> Vec3 {
    Vec3::splat(0.024 + (i % 4) as f32 * 0.003)
}

fn solid_completed(worker: &Worker) -> bool {
    worker.shape_state == State::Completed
        && worker.retired.is_none()
        && !worker.flying
        && worker.terminal_motion.is_none()
        && (0..PARTICLES).all(|i| {
            worker.particles[i] == terminal_goal(worker, i)
                && worker.particle_scales[i] == COMPLETED_PIECE
        })
}

fn terminal_goal(worker: &Worker, i: usize) -> Vec3 {
    let base = terminal_position(worker.anchor, worker.shape_state, i);
    if worker.shape_state == State::Completed {
        worker.anchor + worker.orientation * (base - worker.anchor)
    } else {
        // Opposite fragments have opposite displacements: no mean drift and
        // no closing of the tangential fracture. Period is about ten seconds.
        let side = if i < 12 { -1.0 } else { 1.0 };
        let wave = (worker.ambient_time * 0.65) as f32;
        base + Vec3::Y * (side * 0.012 * wave.sin() * ((i % 12) as f32 * 0.4).cos())
    }
}

/// A fitted 4×3×2 axis-aligned partition, or two separated, offset
/// fragments. Error uses station-tangential X to keep its fracture visible.
fn terminal_position(anchor: Vec3, state: State, i: usize) -> Vec3 {
    if state == State::Completed {
        return anchor
            + Vec3::new(
                (i / 6) as f32 * COMPLETED_PIECE.x + COMPLETED_PIECE.x * 0.5 - COMPLETED_SIDE * 0.5,
                ((i % 6) / 2) as f32 * COMPLETED_PIECE.y + COMPLETED_PIECE.y * 0.5
                    - COMPLETED_SIDE * 0.5,
                (i % 2) as f32 * COMPLETED_PIECE.z + COMPLETED_PIECE.z * 0.5 - COMPLETED_SIDE * 0.5,
            );
    }
    let local = {
        let side = if i < 12 { -1.0 } else { 1.0 };
        let j = i % 12;
        let x = side * if j / 6 == 0 { 0.15 } else { 0.065 };
        let y = ((j % 6) / 2) as f32 * 0.09 - 0.09 + side * 0.025 + x * 0.12;
        let z = (j % 2) as f32 * 0.11 - 0.055 + side * 0.025;
        Vec3::new(x, y, z)
    };
    let radial = Vec3::new(anchor.x, 0.0, anchor.z).normalize();
    let tangent = Vec3::new(-radial.z, 0.0, radial.x);
    anchor + tangent * local.x + Vec3::Y * local.y + radial * local.z
}

fn palette(state: State) -> Vec3 {
    match state {
        State::Running => Vec3::new(0.22, 0.72, 0.95),
        State::Waiting => Vec3::new(0.95, 0.68, 0.25),
        State::Idle => Vec3::new(0.35, 0.65, 0.65),
        State::Completed => Vec3::new(0.30, 0.85, 0.52),
        State::Error => Vec3::new(1.00, 0.25, 0.20),
        State::Unknown => Vec3::new(0.50, 0.55, 0.65),
    }
}

fn flock_offset(i: usize, seed: f32, time: f64, state: State) -> Vec3 {
    let (shape, speed) = match state {
        State::Running => (Vec3::new(0.13, 0.13, 0.11), 0.85),
        State::Waiting => (Vec3::new(0.16, 0.065, 0.10), 0.22),
        State::Idle => (Vec3::new(0.12, 0.09, 0.09), 0.15),
        State::Completed => (Vec3::new(0.105, 0.08, 0.075), 0.08),
        State::Error => (Vec3::new(0.14, 0.13, 0.10), 0.48),
        State::Unknown => (Vec3::new(0.13, 0.10, 0.13), 0.12),
    };
    let y = 1.0 - 2.0 * (i as f32 + 0.5) / PARTICLES as f32;
    let radius = (1.0 - y * y).sqrt();
    let phase = (time * speed) as f32 + seed;
    let angle = i as f32 * 2.399_963_1 + phase;
    let fill = 0.55 + (i % 5) as f32 * 0.11;
    // Filled, tumbling little clouds, not planar agent rings. The small wobble
    // adds organic motion without an O(n^2) boids simulation per worker.
    (Vec3::new(radius * angle.cos(), y, radius * angle.sin()) * fill
        + Vec3::new(
            (phase * 1.3 + y * 5.0).sin(),
            (phase + angle).cos(),
            (phase * 0.7 + y).sin(),
        ) * 0.10)
        * shape
}

fn push_cube(cubes: &mut Vec<Cube>, position: Vec3, scale: Vec3, color: Vec3, orientation: Quat) {
    if cubes.len() >= MAX_CUBES || scale.min_element() <= 0.00001 {
        return;
    }
    let cube = Cube {
        position: position.to_array(),
        scale: scale.to_array(),
        color: color.to_array(),
        orientation: orientation.to_array(),
    };
    if cube.valid() {
        cubes.push(cube);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(id: &str, parent: &str) -> Node {
        Node {
            id: id.into(),
            parent_id: Some(parent.into()),
            title: id.into(),
            state: State::Running,
        }
    }

    fn workspace(nodes: Vec<Node>) -> Workspace {
        Workspace {
            nodes,
            ..Default::default()
        }
    }

    fn anchors(scene: &Scene) -> BTreeMap<String, [f32; 3]> {
        scene
            .stations()
            .iter()
            .map(|s| (s.id.clone(), s.position))
            .collect()
    }

    fn settle(scene: &mut Scene, workspace: &Workspace) {
        for _ in 0..150 {
            scene.update(Some("root"), workspace, 1.0 / 60.0);
        }
    }

    fn assert_terminal_form(worker: &Worker) {
        assert!(!worker.flying);
        assert!(worker.terminal_motion.is_none());
        for i in 0..PARTICLES {
            let goal = terminal_goal(worker, i);
            assert!(worker.particles[i].distance(goal) < 0.001);
        }
        let center = worker.particles.iter().sum::<Vec3>() / PARTICLES as f32;
        assert!(
            center.distance(worker.anchor) < 0.00001,
            "terminal form drifted"
        );
    }

    // Used when every worker is visible enough to emit its full geometry.
    fn assert_only_worker_flocks(scene: &Scene) {
        let mut cubes = scene.cubes().iter();
        for worker in scene.workers.values() {
            if solid_completed(worker) {
                let cube = cubes.next().unwrap();
                assert_eq!(cube.position, worker.anchor.to_array());
                assert_eq!(cube.scale, [COMPLETED_SIDE * worker.visual_opacity; 3]);
            } else {
                let center = worker.particles.iter().copied().sum::<Vec3>() / PARTICLES as f32;
                for (position, scale) in worker.particles.iter().zip(worker.particle_scales) {
                    let cube = cubes.next().unwrap();
                    assert_eq!(
                        cube.position,
                        if worker.visual_opacity == 1.0 {
                            position.to_array()
                        } else {
                            (center + (*position - center) * worker.visual_opacity).to_array()
                        }
                    );
                    assert_eq!(cube.scale, (scale * worker.visual_opacity).to_array());
                }
            }
        }
        assert!(cubes.next().is_none());
    }

    // Compare rendered volumes across the one-cube / fitted-pieces boundary,
    // not just the hidden particle centers. A solid has the same 24 volumes.
    fn rendered_volumes(scene: &Scene) -> [(Vec3, Vec3); PARTICLES] {
        match scene.cubes() {
            [solid] => {
                let center = Vec3::from_array(solid.position);
                let side = Vec3::from_array(solid.scale);
                std::array::from_fn(|i| {
                    let offset = Quat::from_array(solid.orientation)
                        * terminal_position(Vec3::ZERO, State::Completed, i);
                    (
                        center + offset * side / COMPLETED_SIDE,
                        side / Vec3::new(4.0, 3.0, 2.0),
                    )
                })
            }
            pieces => {
                assert_eq!(pieces.len(), PARTICLES);
                std::array::from_fn(|i| {
                    (
                        Vec3::from_array(pieces[i].position),
                        Vec3::from_array(pieces[i].scale),
                    )
                })
            }
        }
    }

    fn assert_volume_step(
        before: &[(Vec3, Vec3); PARTICLES],
        scene: &Scene,
        position_limit: f32,
        scale_limit: f32,
    ) {
        for ((position, scale), (next_position, next_scale)) in
            before.iter().zip(rendered_volumes(scene))
        {
            assert!(
                position.distance(next_position) <= position_limit,
                "rendered position jumped: {position:?} -> {next_position:?}"
            );
            assert!(
                scale.distance(next_scale) <= scale_limit,
                "rendered scale jumped: {scale:?} -> {next_scale:?}"
            );
        }
    }

    fn assert_fitted_partition(pieces: &[Cube], solid: Cube) {
        assert_eq!(pieces.len(), PARTICLES);
        let center = Vec3::from_array(solid.position);
        let side = Vec3::from_array(solid.scale);
        let inverse = Quat::from_array(solid.orientation).inverse();
        let min = -side * 0.5;
        let max = side * 0.5;
        let mut actual_min = Vec3::splat(f32::INFINITY);
        let mut actual_max = Vec3::splat(f32::NEG_INFINITY);
        let mut volume = 0.0;
        for (i, piece) in pieces.iter().enumerate() {
            assert!(piece.valid());
            let position = inverse * (Vec3::from_array(piece.position) - center);
            assert!(
                Quat::from_array(piece.orientation)
                    .angle_between(Quat::from_array(solid.orientation))
                    < 0.001
            );
            let scale = Vec3::from_array(piece.scale);
            assert!(scale.distance(side / Vec3::new(4.0, 3.0, 2.0)) < 0.000001);
            let lo = position - scale * 0.5;
            let hi = position + scale * 0.5;
            assert!((lo - min).min_element() > -0.000001);
            assert!((max - hi).min_element() > -0.000001);
            actual_min = actual_min.min(lo);
            actual_max = actual_max.max(hi);
            volume += scale.x * scale.y * scale.z;
            for other in &pieces[i + 1..] {
                let other_position = inverse * (Vec3::from_array(other.position) - center);
                let other_scale = Vec3::from_array(other.scale);
                let overlap = hi.min(other_position + other_scale * 0.5)
                    - lo.max(other_position - other_scale * 0.5);
                assert!(
                    overlap.min_element() < 0.000001,
                    "overlapping partition pieces"
                );
            }
        }
        assert!(actual_min.distance(min) < 0.000001);
        assert!(actual_max.distance(max) < 0.000001);
        // Contained, non-overlapping volumes whose sum equals the solid leave
        // no holes. Matching outer bounds alone would miss scale-only shrinkage.
        assert!((volume - side.x * side.y * side.z).abs() < 0.00000001);
    }

    #[test]
    fn rotated_solid_split_and_all_state_changes_preserve_rendered_orientations() {
        for to in [
            State::Completed,
            State::Error,
            State::Running,
            State::Unknown,
        ] {
            let mut scene = Scene::new();
            let mut w = workspace(vec![Node {
                state: State::Completed,
                ..node("a", "root")
            }]);
            // More than a minute of rotation exercises a far-from-identity
            // partition, not just an almost-axis-aligned completed cube.
            for _ in 0..1500 {
                scene.update(Some("root"), &w, 0.05);
            }
            assert_eq!(scene.cubes().len(), 1);
            let solid = scene.cubes()[0];
            let rotation = Quat::from_array(solid.orientation);
            assert!(rotation.angle_between(Quat::IDENTITY) > 0.3);
            let volumes = rendered_volumes(&scene);
            w.nodes[0].state = to;
            scene.update(Some("root"), &w, 0.0);
            assert_volume_step(&volumes, &scene, 0.000001, 0.000001);
            assert!(
                scene
                    .cubes()
                    .iter()
                    .all(|cube| cube.orientation == solid.orientation)
            );
            for _ in 0..120 {
                let before = Quat::from_array(scene.cubes()[0].orientation);
                let volumes = rendered_volumes(&scene);
                scene.update(Some("root"), &w, 1.0 / 60.0);
                assert!(
                    Quat::from_array(scene.cubes()[0].orientation).angle_between(before) < 0.11
                );
                assert_volume_step(&volumes, &scene, 0.101, 0.025);
            }
        }
        let mut scene = Scene::new();
        let w = workspace(vec![Node {
            state: State::Completed,
            ..node("a", "root")
        }]);
        for _ in 0..1500 {
            scene.update(Some("root"), &w, 0.05);
        }
        let solid = scene.cubes()[0];
        scene.update(Some("root"), &Workspace::default(), 0.0);
        assert_fitted_partition(scene.cubes(), solid);
        assert!(
            scene
                .cubes()
                .iter()
                .all(|c| c.orientation == solid.orientation)
        );
        for _ in 0..10 {
            scene.update(Some("root"), &Workspace::default(), 1.0 / 60.0);
        }
        let before: Vec<_> = scene.cubes().iter().map(|c| c.orientation).collect();
        let volumes = rendered_volumes(&scene);
        scene.update(Some("root"), &w, 0.0);
        assert_volume_step(&volumes, &scene, 0.000001, 0.000001);
        assert_eq!(
            scene
                .cubes()
                .iter()
                .map(|c| c.orientation)
                .collect::<Vec<_>>(),
            before
        );
    }

    #[test]
    fn error_wave_is_small_slow_zero_mean_and_does_not_close_the_fracture() {
        let mut scene = Scene::new();
        let w = workspace(vec![Node {
            state: State::Error,
            ..node("a", "root")
        }]);
        scene.update(Some("root"), &w, 0.0);
        let initial = scene.workers["a"].particles;
        let mut maximum = 0.0_f32;
        for frame in 0..1200 {
            let before = scene.workers["a"].particles;
            let evidence = if frame % 3 == 0 {
                Workspace {
                    stale: true,
                    ..w.clone()
                }
            } else {
                w.clone()
            };
            scene.update(Some("root"), &evidence, 1.0 / 60.0);
            let worker = &scene.workers["a"];
            assert_terminal_form(worker);
            let radial = Vec3::new(worker.anchor.x, 0.0, worker.anchor.z).normalize();
            let tangent = Vec3::new(-radial.z, 0.0, radial.x);
            for (i, position) in worker.particles.iter().enumerate() {
                let displacement = position.distance(initial[i]);
                maximum = maximum.max(displacement);
                assert!(displacement <= 0.01201);
                assert!(position.distance(before[i]) < 0.00015);
                let x = (*position - worker.anchor).dot(tangent);
                assert!(if i < 12 { x < -0.06 } else { x > 0.06 });
            }
        }
        assert!(maximum > 0.01, "the fracture must actually wave");
    }

    #[test]
    fn cube_quaternions_are_defaulted_bounded_normalized_and_roundtrip() {
        let legacy: Cube = serde_json::from_str(
            r#"{"position":[0,1,-2],"scale":[0.24,0.24,0.24],"color":[0.3,0.8,0.5]}"#,
        )
        .unwrap();
        assert_eq!(legacy.orientation, Quat::IDENTITY.to_array());
        assert!(legacy.valid());
        let rotated = Cube {
            orientation: Quat::from_rotation_y(1.2).to_array(),
            ..legacy
        };
        let decoded: Cube =
            serde_json::from_str(&serde_json::to_string(&rotated).unwrap()).unwrap();
        assert_eq!(rotated.orientation, decoded.orientation);
        assert!(decoded.valid());
        for orientation in [
            [0.0; 4],
            [1.0; 4],
            [f32::NAN, 0.0, 0.0, 1.0],
            [f32::INFINITY, 0.0, 0.0, 1.0],
            [0.0, 0.0, 0.0, 0.9],
            [0.0, 0.0, 0.0, 1.01],
        ] {
            assert!(
                !Cube {
                    orientation,
                    ..legacy
                }
                .valid()
            );
        }
    }

    #[test]
    fn completed_emits_one_solid_cube_after_history_arrival_or_observed_settle() {
        for discovery in 0..3 {
            let mut scene = Scene::new();
            let mut w = workspace(vec![Node {
                state: State::Completed,
                ..node("a", "root")
            }]);
            if discovery == 1 {
                scene.update(Some("root"), &Workspace::default(), 0.0);
            } else if discovery == 2 {
                w.nodes[0].state = State::Running;
                settle(&mut scene, &w);
                w.nodes[0].state = State::Completed;
            }
            scene.update(Some("root"), &w, 0.0);
            assert_eq!(scene.workers["a"].flying, discovery == 1);
            assert_eq!(scene.workers["a"].terminal_motion.is_some(), discovery == 2);
            settle(&mut scene, &w);
            assert!(solid_completed(&scene.workers["a"]));
            assert_eq!(scene.cubes().len(), 1, "discovery {discovery}");
            let solid = scene.cubes()[0];
            assert_eq!(solid.position, scene.workers["a"].anchor.to_array());
            assert_eq!(solid.scale, [COMPLETED_SIDE; 3]);
            assert!(Vec3::from_array(solid.color).distance(palette(State::Completed)) < 0.000001);
            for _ in 0..120 {
                scene.update(Some("root"), &w, 1.0 / 60.0);
                assert_eq!(scene.cubes().len(), 1);
                assert_eq!(scene.cubes()[0].position, solid.position);
                assert_eq!(scene.cubes()[0].scale, solid.scale);
            }
        }
    }

    #[test]
    fn solid_return_onset_exactly_tiles_full_and_uncertainty_faded_bounds() {
        for faded in [false, true] {
            let mut scene = Scene::new();
            let w = workspace(vec![Node {
                state: State::Completed,
                ..node("a", "root")
            }]);
            settle(&mut scene, &w);
            if faded {
                let stale = Workspace {
                    stale: true,
                    ..w.clone()
                };
                for _ in 0..20 {
                    scene.update(Some("root"), &stale, 0.05);
                }
                assert_eq!(scene.workers["a"].visual_opacity, 0.55);
            }
            assert_eq!(scene.cubes().len(), 1);
            let solid = scene.cubes()[0];
            let before = rendered_volumes(&scene);
            scene.update(Some("root"), &Workspace::default(), 0.0);
            assert_eq!(scene.workers["a"].retired, Some(0.0));
            assert_fitted_partition(scene.cubes(), solid);
            assert_volume_step(&before, &scene, 0.000001, 0.000001);
        }
    }

    #[test]
    fn completed_departure_emits_24_visible_pieces_smoothly_toward_captured_samples() {
        let mut scene = Scene::new();
        let w = workspace(vec![Node {
            state: State::Completed,
            ..node("a", "root")
        }]);
        settle(&mut scene, &w);
        let samples: Vec<_> = (0..PARTICLES)
            .map(|i| (CENTER + Vec3::new(i as f32 * 0.004, 0.0, -0.1)).to_array())
            .collect();
        scene.set_coordinator_samples(&samples);
        let start = scene.workers["a"].particles;
        let before = rendered_volumes(&scene);
        scene.update(Some("root"), &Workspace::default(), 0.0);
        assert_volume_step(&before, &scene, 0.000001, 0.000001);
        let targets = scene.workers["a"].return_end;
        assert!(targets.iter().all(|p| samples.contains(&p.to_array())));
        for (i, target) in targets.iter().enumerate() {
            assert!(!targets[i + 1..].contains(target));
        }
        // Moving coordinator samples do not teleport an in-flight destination.
        scene.set_coordinator_samples(&[[5.0, 5.0, 5.0]]);
        for frame in 1..=60 {
            let before = rendered_volumes(&scene);
            scene.update(Some("root"), &Workspace::default(), 1.0 / 60.0);
            assert_eq!(scene.cubes().len(), PARTICLES, "frame {frame}");
            assert!(scene.cubes().iter().all(Cube::valid));
            assert_volume_step(&before, &scene, 0.101, 0.025);
            assert_eq!(scene.workers["a"].return_end, targets);
            if frame == 30 {
                assert!(
                    scene
                        .cubes()
                        .iter()
                        .all(|cube| cube.scale.iter().all(|s| *s >= 0.023))
                );
                let worker = &scene.workers["a"];
                for i in 0..PARTICLES {
                    assert!(
                        worker.particles[i].distance(targets[i])
                            < start[i].distance(targets[i]) * 0.8
                    );
                    assert_eq!(worker.particle_scales[i], particle_scale(i));
                }
            }
        }
        assert!(
            scene.workers["a"]
                .particles
                .iter()
                .zip(targets)
                .all(|(p, target)| p.distance(target) < 0.4)
        );
        for _ in 0..30 {
            scene.update(Some("root"), &Workspace::default(), 1.0 / 60.0);
        }
        assert!(scene.workers.is_empty());
        assert!(scene.cubes().is_empty());
    }

    #[test]
    fn new_worker_renders_24_pieces_at_actual_samples_without_centroid_collapse() {
        let mut scene = Scene::new();
        scene.update(Some("root"), &Workspace::default(), 0.0);
        let samples: Vec<_> = (0..PARTICLES)
            .map(|i| (CENTER + Vec3::new(i as f32 * 0.03 - 0.35, 0.0, 0.0)).to_array())
            .collect();
        scene.set_coordinator_samples(&samples);
        let w = workspace(vec![node("a", "root")]);
        scene.update(Some("root"), &w, 0.0);
        assert_eq!(scene.cubes().len(), PARTICLES);
        for (i, cube) in scene.cubes().iter().enumerate() {
            assert!(samples.contains(&cube.position));
            assert_eq!(cube.scale, particle_scale(i).to_array());
        }
        for _ in 0..90 {
            let before = rendered_volumes(&scene);
            scene.update(Some("root"), &w, 1.0 / 60.0);
            assert_eq!(scene.cubes().len(), PARTICLES);
            assert_volume_step(&before, &scene, 0.101, 0.000001);
            assert!(scene.cubes().iter().all(Cube::valid));
            // Rendered centers, not only hidden simulation positions, follow
            // the bounded path starting at the actual coordinator samples.
            for (cube, position) in scene.cubes().iter().zip(scene.workers["a"].particles) {
                assert_eq!(cube.position, position.to_array());
            }
        }
    }

    #[test]
    fn rendered_positions_and_scales_are_continuous_across_every_state_pair() {
        let states = [
            State::Running,
            State::Waiting,
            State::Idle,
            State::Completed,
            State::Error,
            State::Unknown,
        ];
        for from in states {
            for to in states {
                let mut scene = Scene::new();
                let mut w = workspace(vec![Node {
                    state: from,
                    ..node("a", "root")
                }]);
                settle(&mut scene, &w);
                let before = rendered_volumes(&scene);
                w.nodes[0].state = to;
                scene.update(Some("root"), &w, 0.0);
                assert_volume_step(&before, &scene, 0.000001, 0.000001);
                for _ in 0..180 {
                    let before = rendered_volumes(&scene);
                    scene.update(Some("root"), &w, 1.0 / 60.0);
                    assert_volume_step(&before, &scene, 0.101, 0.025);
                    assert!(scene.cubes().iter().all(Cube::valid));
                }
                assert_eq!(
                    scene.cubes().len(),
                    if to == State::Completed { 1 } else { PARTICLES }
                );
                assert_eq!(scene.stations()[0].state, to);
            }
        }
    }

    #[test]
    fn solid_resurrection_retains_rendered_scale_and_position_even_during_late_fade() {
        for return_frames in [0, 12, 60, 68] {
            for resumed_state in [
                State::Completed,
                State::Running,
                State::Error,
                State::Unknown,
            ] {
                let mut scene = Scene::new();
                let mut w = workspace(vec![Node {
                    state: State::Completed,
                    ..node("a", "root")
                }]);
                settle(&mut scene, &w);
                scene.update(Some("root"), &Workspace::default(), 0.0);
                for _ in 0..return_frames {
                    scene.update(Some("root"), &Workspace::default(), 1.0 / 60.0);
                }
                let before = rendered_volumes(&scene);
                w.nodes[0].state = resumed_state;
                scene.update(Some("root"), &w, 0.0);
                assert!(scene.workers["a"].retired.is_none());
                assert!(
                    scene.workers["a"]
                        .terminal_motion
                        .is_none_or(|motion| !motion.bounce)
                );
                assert_volume_step(&before, &scene, 0.000001, 0.000001);
                for _ in 0..180 {
                    let before = rendered_volumes(&scene);
                    scene.update(Some("root"), &w, 1.0 / 60.0);
                    // Up to 0.1 m particle motion plus the bounded expansion
                    // about the centroid when a faded return is resurrected.
                    assert_volume_step(&before, &scene, 0.12, 0.025);
                    assert!(scene.cubes().iter().all(Cube::valid));
                }
                assert_eq!(
                    scene.cubes().len(),
                    if resumed_state == State::Completed {
                        1
                    } else {
                        PARTICLES
                    }
                );
            }
        }
    }

    #[test]
    fn terminal_history_metadata_and_resumed_evidence_do_not_invent_launches() {
        let mut scene = Scene::new();
        let mut w = workspace(vec![Node {
            state: State::Completed,
            ..node("history", "root")
        }]);
        scene.set_coordinator_samples(&[[0.4, 1.5, -2.4]]);
        scene.update(Some("root"), &w, 0.0);
        assert_eq!(scene.cubes().len(), 1);
        let position = scene.cubes()[0].position;
        w.presentations.push(Presentation {
            id: "historical-card".into(),
            source_id: "history".into(),
            source_message_id: "old-message".into(),
            ..Default::default()
        });
        w.nodes[0].title = "Historical result".into();
        scene.update(Some("root"), &w, 1000.0);
        assert_eq!(scene.cubes().len(), 1);
        assert_eq!(scene.cubes()[0].position, position);
        assert!(!scene.workers["history"].flying);
        assert!(scene.workers["history"].terminal_motion.is_none());
        for uncertain in [
            Workspace {
                stale: true,
                ..w.clone()
            },
            Workspace {
                truncated: true,
                ..Default::default()
            },
            Workspace {
                error: Some("offline".into()),
                ..Default::default()
            },
        ] {
            scene.update(Some("root"), &uncertain, 0.05);
            assert!(scene.workers["history"].retired.is_none());
            w.nodes.push(Node {
                state: State::Completed,
                ..node(&format!("recovered-{}", w.nodes.len()), "root")
            });
            scene.update(Some("root"), &w, 0.0);
            assert_eq!(scene.cubes().len(), w.nodes.len());
            assert!(scene.workers.values().all(|worker| !worker.flying
                && worker.terminal_motion.is_none()
                && worker.retired.is_none()));
        }
        // Only a genuinely new observation after continuous trusted evidence
        // launches 24 particles, each from an actual sampled coordinator cube.
        w.nodes.push(node("new", "root"));
        scene.update(Some("root"), &w, 0.0);
        assert!(scene.workers["new"].flying);
        assert_eq!(
            scene.workers["new"].particles,
            [Vec3::new(0.4, 1.5, -2.4); PARTICLES]
        );
        for _ in 0..12 {
            scene.update(Some("root"), &w, 1.0 / 60.0);
        }
        assert_eq!(scene.cubes().len(), w.nodes.len() - 1 + PARTICLES);
        assert_only_worker_flocks(&scene);
    }

    #[test]
    fn resurrecting_near_the_end_of_split_does_not_snap_scale_at_zero_dt() {
        let mut scene = Scene::new();
        let mut w = workspace(vec![Node {
            state: State::Completed,
            ..node("a", "root")
        }]);
        settle(&mut scene, &w);
        scene.update(Some("root"), &Workspace::default(), 0.0);
        for _ in 0..7 {
            scene.update(
                Some("root"),
                &Workspace::default(),
                (SPLIT_SECONDS - 0.001) / 7.0,
            );
        }
        let before = scene.workers["a"].particle_scales;
        assert!(before[0].distance(particle_scale(0)) > 0.0);
        assert!(before[0].distance(particle_scale(0)) < 0.00001);
        w.nodes[0].state = State::Running;
        scene.update(Some("root"), &w, 0.0);
        assert_eq!(scene.workers["a"].particle_scales, before);
    }

    #[test]
    fn serde_defaults_and_wire_names() {
        let empty: Workspace = serde_json::from_str("{}").unwrap();
        assert!(empty.valid());
        assert!(!empty.stale); // Older bridges did not send this field.
        let n: Node = serde_json::from_str(r#"{"id":"n","parent_id":"root"}"#).unwrap();
        assert_eq!(n.state, State::Unknown);
        assert_eq!(
            serde_json::to_string(&State::Completed).unwrap(),
            "\"completed\""
        );
        assert_eq!(
            serde_json::to_string(&Kind::Markdown).unwrap(),
            "\"markdown\""
        );
        assert!(serde_json::from_str::<State>("\"invented\"").is_err());
    }

    #[test]
    fn validates_ids_cycles_and_missing_parents() {
        assert!(workspace(vec![node("a", "absent")]).valid());
        assert!(!workspace(vec![node("a", "root"), node("a", "root")]).valid());
        assert!(!workspace(vec![node("a", "a")]).valid());
        assert!(!workspace(vec![node("a", "b"), node("b", "a")]).valid());
        assert!(!workspace(vec![node("", "root")]).valid());
        assert!(!workspace(vec![node(&"x".repeat(257), "root")]).valid());
        let mut w = workspace((0..65).map(|i| node(&i.to_string(), "root")).collect());
        assert!(!w.valid());
        w.nodes.pop();
        assert!(w.valid());
        w.nodes[0].title = "🦀".repeat(160);
        assert!(w.valid());
        w.nodes[0].title.push('x');
        assert!(!w.valid());
    }

    #[test]
    fn validates_bounded_rectangular_presentations() {
        let card = Presentation {
            id: "card".into(),
            source_id: "worker".into(),
            kind: Kind::Table,
            columns: vec!["one".into(), "two".into()],
            rows: vec![vec!["a".into(), "b".into()]],
            ..Default::default()
        };
        let mut w = Workspace {
            presentations: vec![card.clone()],
            ..Default::default()
        };
        assert!(w.valid());
        w.presentations[0].rows[0].pop();
        assert!(!w.valid());
        w.presentations[0] = card.clone();
        w.presentations[0].text = "é".repeat(2049);
        assert!(!w.valid());
        w.presentations[0] = card.clone();
        w.presentations[0].columns[0] = "x".repeat(81);
        assert!(!w.valid());
        w.presentations[0] = card.clone();
        w.presentations[0].rows[0][0] = "x".repeat(161);
        assert!(!w.valid());
        w.presentations = vec![card.clone(), card.clone()];
        assert!(!w.valid());
        w.presentations = (0..17)
            .map(|i| Presentation {
                id: i.to_string(),
                ..card.clone()
            })
            .collect();
        assert!(!w.valid());
    }

    #[test]
    fn cube_validation_rejects_bad_geometry() {
        let cube = Cube {
            position: CENTER.to_array(),
            scale: [0.02; 3],
            color: [0.5; 3],
            orientation: Quat::IDENTITY.to_array(),
        };
        assert!(cube.valid());
        for bad in [f32::NAN, f32::INFINITY, -f32::INFINITY, 101.0] {
            assert!(
                !Cube {
                    position: [bad, 1.0, 0.0],
                    ..cube
                }
                .valid()
            );
        }
        for bad in [f32::NAN, -0.1, 0.0, 1.1] {
            assert!(
                !Cube {
                    scale: [bad; 3],
                    ..cube
                }
                .valid()
            );
        }
        assert!(
            !Cube {
                color: [1.1; 3],
                ..cube
            }
            .valid()
        );
        assert!(
            !Cube {
                color: [f32::NAN; 3],
                ..cube
            }
            .valid()
        );
    }

    #[test]
    fn station_wire_roundtrip_and_validation() {
        let station = Station {
            id: "worker".into(),
            position: [1.0, 2.0, -3.0],
            state: State::Waiting,
            title: "A worker".into(),
            opacity: 0.4,
        };
        assert!(station.valid());
        let encoded = serde_json::to_string(&station).unwrap();
        let decoded: Station = serde_json::from_str(&encoded).unwrap();
        assert!(decoded.valid());
        assert_eq!(decoded.id, station.id);
        assert_eq!(decoded.opacity, station.opacity);
        let old: Station = serde_json::from_str(
            r#"{"id":"w","position":[0,1,-2],"state":"running","title":"Worker"}"#,
        )
        .unwrap();
        assert_eq!(old.opacity, 1.0);
        assert!(old.valid());
        for bad in [f32::NAN, f32::INFINITY, -0.1, 1.1] {
            assert!(
                !Station {
                    opacity: bad,
                    ..station.clone()
                }
                .valid()
            );
        }
        assert!(
            !Station {
                position: [f32::NAN, 0.0, 0.0],
                ..station.clone()
            }
            .valid()
        );
        assert!(
            !Station {
                id: "x".repeat(161),
                ..station.clone()
            }
            .valid()
        );
        assert!(
            !Station {
                title: "x".repeat(161),
                ..station
            }
            .valid()
        );
    }

    #[test]
    fn parentless_root_and_unrelated_entries_are_not_workers() {
        let mut scene = Scene::new();
        let w = workspace(vec![
            Node {
                id: "root".into(),
                ..Default::default()
            },
            Node {
                id: "unrelated".into(),
                ..Default::default()
            },
            node("child", "root"),
        ]);
        scene.update(Some("root"), &w, 0.02);
        assert_eq!(scene.stations().len(), 1);
        assert_eq!(scene.stations()[0].id, "child");
        scene.update(None, &w, 0.02);
        assert!(scene.stations().is_empty());
        assert!(scene.cubes().is_empty());
    }

    #[test]
    fn worker_supplied_positions_are_ignored() {
        let a: Workspace = serde_json::from_str(r#"{"nodes":[{"id":"a","parent_id":"root","title":"A","state":"running","position":[99,99,99],"xyz":[-80,80,80]}]}"#).unwrap();
        let b = workspace(vec![Node {
            title: "A".into(),
            ..node("a", "root")
        }]);
        let mut one = Scene::new();
        let mut two = Scene::new();
        settle(&mut one, &a);
        settle(&mut two, &b);
        assert_eq!(anchors(&one), anchors(&two));
        assert_eq!(
            serde_json::to_string(one.cubes()).unwrap(),
            serde_json::to_string(two.cubes()).unwrap()
        );
    }

    #[test]
    fn initial_order_and_animation_are_deterministic() {
        let a = workspace(vec![node("c", "b"), node("b", "root"), node("a", "root")]);
        let b = workspace(a.nodes.iter().cloned().rev().collect());
        let mut one = Scene::new();
        let mut two = Scene::default();
        settle(&mut one, &a);
        settle(&mut two, &b);
        assert_eq!(anchors(&one), anchors(&two));
        assert_eq!(
            serde_json::to_string(one.cubes()).unwrap(),
            serde_json::to_string(two.cubes()).unwrap()
        );
        assert_eq!(one.snapshot().len(), one.cubes().len());
    }

    #[test]
    fn surviving_anchors_do_not_move_on_add_remove_reorder_or_state() {
        let mut w = workspace(vec![node("b", "root"), node("c", "b")]);
        let mut scene = Scene::new();
        settle(&mut scene, &w);
        let before = anchors(&scene);
        w.nodes.reverse();
        w.nodes.push(node("a", "root"));
        w.nodes[0].state = State::Completed;
        scene.update(Some("root"), &w, 1.0 / 60.0);
        for id in ["b", "c"] {
            assert_eq!(anchors(&scene)[id], before[id]);
        }
        w.nodes.retain(|n| n.id != "b");
        settle(&mut scene, &w);
        assert_eq!(anchors(&scene)["c"], before["c"]);
        assert!(!anchors(&scene).contains_key("b"));
        // Archived parent provenance remains cached, rather than using CENTER.
        assert_eq!(slot_position(scene.slots["b"]).to_array(), before["b"]);
        w.nodes.push(node("b", "root"));
        scene.update(Some("root"), &w, 0.0);
        assert_eq!(anchors(&scene)["b"], before["b"]);
    }

    #[test]
    fn archived_parent_anchor_is_reserved_before_new_workers() {
        let mut scene = Scene::new();
        let mut w = workspace(vec![node("parent", "root"), node("child", "parent")]);
        settle(&mut scene, &w);
        let parent_anchor = scene.workers["parent"].anchor;
        let child_anchor = scene.workers["child"].anchor;
        w.nodes.retain(|n| n.id != "parent");
        settle(&mut scene, &w);
        assert!(!scene.workers.contains_key("parent"));
        w.nodes.push(node("new", "root"));
        scene.update(Some("root"), &w, 0.02);
        assert_eq!(slot_position(scene.slots["parent"]), parent_anchor);
        assert_eq!(scene.workers["child"].anchor, child_anchor);
        assert_ne!(scene.workers["new"].anchor, parent_anchor);
    }

    #[test]
    fn root_switch_and_deselect_clear_all_previous_geometry() {
        let w = workspace(vec![node("old", "root")]);
        let mut scene = Scene::new();
        settle(&mut scene, &w);
        let stale = Workspace {
            error: Some("offline".into()),
            stale: true,
            ..w.clone()
        };
        scene.update(Some("new-root"), &stale, 0.02);
        assert!(scene.cubes().is_empty());
        assert!(scene.stations().is_empty());
        assert!(scene.slots.is_empty());
        scene.update(None, &w, 0.02);
        assert!(scene.cubes().is_empty());
        scene.update(Some("root"), &w, 0.02);
        let mut fresh = Scene::new();
        fresh.update(Some("root"), &w, 0.02);
        assert_eq!(anchors(&scene), anchors(&fresh));
    }

    #[test]
    fn switching_between_fresh_root_projections_does_not_retain_old_workers() {
        let mut scene = Scene::new();
        settle(&mut scene, &workspace(vec![node("old", "root")]));
        let next = workspace(vec![node("new", "other-root")]);
        scene.update(Some("other-root"), &next, 0.02);
        assert_eq!(scene.stations().len(), 1);
        assert_eq!(scene.stations()[0].id, "new");
        assert!(!scene.workers.contains_key("old"));
        assert!(!scene.slots.contains_key("old"));
    }

    #[test]
    fn partial_old_root_nodes_cannot_leak_into_new_root() {
        let mut scene = Scene::new();
        settle(&mut scene, &workspace(vec![node("old", "root")]));
        let partial = Workspace {
            nodes: vec![
                node("old", "root"),
                node("nested", "old"),
                node("new", "other-root"),
            ],
            truncated: true,
            ..Default::default()
        };
        scene.update(Some("other-root"), &partial, 0.02);
        assert_eq!(scene.stations().len(), 1);
        assert_eq!(scene.stations()[0].id, "new");
        assert_eq!(scene.stations()[0].state, State::Running);
    }

    #[test]
    fn stale_old_projection_cannot_add_nodes_even_when_new_root_is_a_descendant() {
        let mut scene = Scene::new();
        let old = workspace(vec![node("parent", "root"), node("child", "parent")]);
        settle(&mut scene, &old);
        let stale = Workspace { stale: true, ..old };
        scene.update(Some("parent"), &stale, 0.02);
        assert!(scene.stations().is_empty());
        scene.update(Some("parent"), &stale, 0.02);
        assert!(scene.stations().is_empty());
    }

    #[test]
    fn slots_are_bounded_distinct_without_hidden_radial_overflow() {
        let points: Vec<_> = (0..SLOT_COUNT).map(slot_position).collect();
        for (i, p) in points.iter().enumerate() {
            assert!(p.is_finite());
            assert!((0.2..6.4).contains(&p.y));
            let radius = p.x.hypot(p.z);
            assert!((radius - 3.2).abs() < 0.0001);
            assert!(p.to_array().iter().all(|v| v.abs() <= 100.0));
            for q in points.iter().skip(i + 1) {
                assert!(p.distance(*q) > 0.54, "{p:?} {q:?}");
            }
        }
        assert!(points[128].y > points[0].y);
    }

    #[test]
    fn common_worker_counts_flank_coordinator_with_readable_labels() {
        // Direct workers, a deep chain, and independent missing parents must
        // all put the real stations in the first alternating row.
        for count in 1..=8 {
            for hierarchy in 0..3 {
                let nodes = (0..count)
                    .map(|i| {
                        let parent = match hierarchy {
                            0 => "root".to_owned(),
                            1 if i == 0 => "root".to_owned(),
                            1 => format!("worker-{:02}", i - 1),
                            _ => format!("absent-{i:02}"),
                        };
                        node(&format!("worker-{i:02}"), &parent)
                    })
                    .collect();
                let mut scene = Scene::new();
                scene.update(Some("root"), &workspace(nodes), 0.02);
                let stations = scene.stations();
                assert_eq!(stations.len(), count);
                for (i, station) in stations.iter().enumerate() {
                    let p = Vec3::from_array(station.position);
                    assert_eq!(p, slot_position(i));
                    assert!((p.x.hypot(p.z) - 3.2).abs() < 0.0001);
                    assert!(p.x.abs() > 1.6 && p.z < 0.0);
                    assert!((1.3..2.0).contains(&p.y));
                    for other in stations.iter().skip(i + 1) {
                        assert!(p.distance(Vec3::from_array(other.position)) >= 0.54);
                    }
                    // Conservative angular clearance: the coordinator's
                    // normal conversational envelope is ~1 m wide in each
                    // direction and ~0.42 m deep. Include the label's inner
                    // edge, not just the worker anchor, from a viewer at zero.
                    let inner_label_edge = p.x.abs() - 0.493;
                    assert!(inner_label_edge / -(p.z + 0.1) > 1.0 / -(CENTER.z + 0.42));
                }
                for (i, a) in stations.iter().enumerate() {
                    for b in stations.iter().skip(i + 1) {
                        // Current label bounds, projected from the origin;
                        // staggered heights separate adjacent columns too.
                        let interval = |s: &Station| {
                            let distance = -(s.position[2] + 0.1);
                            (
                                (s.position[0] - 0.48 - 3.0 * 0.0038) / distance,
                                (s.position[0] - 0.48 + 256.0 * 0.0038) / distance,
                            )
                        };
                        let (a0, a1) = interval(a);
                        let (b0, b1) = interval(b);
                        assert!(
                            a1 < b0 || b1 < a0 || (a.position[1] - b.position[1]).abs() > 0.13,
                            "{a:?} {b:?}"
                        );
                        // Distinct observer bearings, not world-X distance,
                        // determine clearance for the inward-facing signs.
                        let bearing = |s: &Station| s.position[0].atan2(-s.position[2]);
                        assert!(
                            (bearing(a) - bearing(b)).abs() > 0.3
                                || (a.position[1] - b.position[1]).abs() > 0.54
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn arc_is_balanced_curved_and_fixed_in_world_while_view_turns() {
        let w = workspace(
            (0..16)
                .map(|i| node(&format!("w-{i:02}"), "root"))
                .collect(),
        );
        let mut scene = Scene::new();
        scene.update(Some("root"), &w, 0.0);
        let original = anchors(&scene);
        for column in 0..2 {
            let left = slot_position(column * 2);
            let right = slot_position(column * 2 + 1);
            assert!((left.x + right.x).abs() < 0.00001);
            assert_eq!(left.z, right.z);
            if column > 0 {
                let previous = slot_position((column - 1) * 2 + 1);
                assert!(right.x > previous.x && right.z > previous.z);
            }
        }
        assert!(
            scene
                .workers
                .values()
                .all(|worker| { (worker.anchor.x.hypot(worker.anchor.z) - 3.2).abs() < 0.0001 })
        );
        // Rotation changes the view coordinates, not Scene's world positions.
        let world = Vec3::from_array(original["w-00"]);
        let view = glam::Quat::from_rotation_y(0.7).inverse() * world;
        assert!(view.distance(world) > 0.1);
        scene.update(Some("root"), &w, 0.02);
        assert_eq!(anchors(&scene), original);
    }

    #[test]
    fn first_and_uncertain_discoveries_are_settled_not_launches() {
        let mut scene = Scene::new();
        let mut w = workspace(vec![node("historical", "root")]);
        scene.update(Some("root"), &w, 0.0);
        let worker = &scene.workers["historical"];
        assert_eq!(worker.age, ARRIVAL_SECONDS);
        assert!(
            worker
                .particles
                .iter()
                .all(|p| p.distance(worker.anchor) < 0.25)
        );
        assert_eq!(scene.stations()[0].opacity, 1.0);

        w.nodes.push(node("partial", "root"));
        scene.update(
            Some("root"),
            &Workspace {
                truncated: true,
                ..w.clone()
            },
            0.0,
        );
        assert_eq!(scene.workers["partial"].age, ARRIVAL_SECONDS);
        w.nodes.push(node("after-partial", "root"));
        scene.update(Some("root"), &w, 0.0);
        assert_eq!(scene.workers["after-partial"].age, ARRIVAL_SECONDS);
        scene.update(
            Some("root"),
            &Workspace {
                stale: true,
                ..w.clone()
            },
            0.0,
        );
        w.nodes.push(node("after-stale", "root"));
        scene.update(Some("root"), &w, 0.0);
        assert_eq!(scene.workers["after-stale"].age, ARRIVAL_SECONDS);
        w.nodes.push(node("new", "root"));
        scene.update(Some("root"), &w, 0.0);
        assert_eq!(scene.workers["new"].age, 0.0);
        assert!(scene.workers["new"].particles.iter().all(|p| *p == CENTER));
    }

    #[test]
    fn bounded_actual_samples_drive_launch_and_return_with_center_fallback() {
        let mut scene = Scene::new();
        let mut w = Workspace::default();
        scene.update(Some("root"), &w, 0.0);
        let samples = [
            [f32::NAN, 1.0, -2.0],
            [0.2, 1.3, -2.2],
            [-0.4, 1.5, -2.5],
            [f32::INFINITY, 0.0, 0.0],
            [101.0, 1.4, -2.3],
        ];
        scene.set_coordinator_samples(&samples);
        assert_eq!(scene.coordinator_sample_count, 2);
        w.nodes.push(node("worker", "root"));
        scene.update(Some("root"), &w, 0.0);
        let worker = &scene.workers["worker"];
        assert!(
            worker
                .particles
                .iter()
                .all(|p| *p == Vec3::from_array(samples[1]) || *p == Vec3::from_array(samples[2]))
        );
        assert!(worker.particles.contains(&Vec3::from_array(samples[1])));
        assert!(worker.particles.contains(&Vec3::from_array(samples[2])));
        settle(&mut scene, &w);

        let destination = [0.3, 1.6, -2.4];
        scene.set_coordinator_samples(&[destination]);
        let before = scene.workers["worker"].particles;
        w.nodes.clear();
        scene.update(Some("root"), &w, 0.0);
        assert_eq!(scene.workers["worker"].particles, before);
        assert!(
            scene.workers["worker"]
                .return_end
                .iter()
                .all(|p| *p == Vec3::from_array(destination))
        );
        scene.set_coordinator_samples(&[]);
        for _ in 0..60 {
            scene.update(Some("root"), &w, 1.0 / 60.0);
        }
        let worker = &scene.workers["worker"];
        assert!(worker.retired.unwrap() >= 0.99);
        assert!(
            worker
                .particles
                .iter()
                .all(|p| p.distance(Vec3::from_array(destination)) < 0.4)
        );
        assert!(scene.cubes().iter().all(Cube::valid));
        for _ in 0..20 {
            scene.update(Some("root"), &w, 1.0 / 60.0);
        }
        assert!(!scene.workers.contains_key("worker"));
        w.nodes.push(node("fallback", "root"));
        scene.update(Some("root"), &w, 0.0);
        assert!(
            scene.workers["fallback"]
                .particles
                .iter()
                .all(|p| *p == CENTER)
        );
        w.nodes.clear();
        scene.update(Some("root"), &w, 0.0);
        assert!(
            scene.workers["fallback"]
                .return_end
                .iter()
                .all(|p| *p == CENTER)
        );

        let many = vec![CENTER.to_array(); 1000];
        scene.set_coordinator_samples(&many);
        assert_eq!(scene.coordinator_sample_count, MAX_COORDINATOR_SAMPLES);
    }

    #[test]
    fn distant_finite_coordinator_samples_extend_flight_without_teleports() {
        let mut scene = Scene::new();
        let mut w = Workspace::default();
        scene.update(Some("root"), &w, 0.0);
        let sample = Vec3::new(20.0, 1.4, -2.3);
        scene.set_coordinator_samples(&[sample.to_array()]);
        w.nodes.push(node("distant", "root"));
        scene.update(Some("root"), &w, 0.0);
        assert_eq!(scene.workers["distant"].particles[0], sample);
        assert!(scene.workers["distant"].arrival_seconds > ARRIVAL_SECONDS);
        for _ in 0..200 {
            let before = scene.workers["distant"].particles;
            scene.update(Some("root"), &w, 0.05);
            assert!(
                before
                    .iter()
                    .zip(scene.workers["distant"].particles)
                    .all(|(a, b)| a.distance(b) < 0.301)
            );
            assert!(scene.cubes().iter().all(Cube::valid));
        }
        let worker = &scene.workers["distant"];
        assert!(
            worker
                .particles
                .iter()
                .all(|p| p.distance(worker.anchor) < 0.25)
        );
        w.nodes.clear();
        scene.update(Some("root"), &w, 0.0);
        assert!(scene.workers["distant"].departure_seconds > DEPARTURE_SECONDS);
        for _ in 0..200 {
            let Some(worker) = scene.workers.get("distant") else {
                break;
            };
            let before = worker.particles;
            scene.update(Some("root"), &w, 0.05);
            if let Some(worker) = scene.workers.get("distant") {
                assert!(
                    before
                        .iter()
                        .zip(worker.particles)
                        .all(|(a, b)| a.distance(b) < 0.301)
                );
                assert!(scene.cubes().iter().all(Cube::valid));
            }
        }
        assert!(!scene.workers.contains_key("distant"));
    }

    #[test]
    fn return_is_continuous_pauses_on_uncertainty_and_reappearance_resumes_in_place() {
        let mut scene = Scene::new();
        let mut w = workspace(vec![node("gone", "root"), node("healthy", "root")]);
        settle(&mut scene, &w);
        let healthy_anchor = scene.workers["healthy"].anchor;
        let start = scene.workers["gone"].particles;
        w.nodes.retain(|node| node.id != "gone");
        scene.update(Some("root"), &w, 0.0);
        assert_eq!(scene.workers["gone"].particles, start);
        for _ in 0..30 {
            let before = scene.workers["gone"].particles;
            scene.update(Some("root"), &w, 1.0 / 60.0);
            assert!(
                before
                    .iter()
                    .zip(scene.workers["gone"].particles)
                    .all(|(a, b)| a.distance(b) < 0.1)
            );
            assert_eq!(scene.workers["healthy"].anchor, healthy_anchor);
            assert!(scene.workers["healthy"].retired.is_none());
        }
        let midway = scene.workers["gone"].particles;
        let distance =
            |points: &[Vec3; PARTICLES]| points.iter().map(|p| p.distance(CENTER)).sum::<f32>();
        assert!(distance(&midway) < distance(&start));
        for partial in [
            Workspace {
                stale: true,
                ..w.clone()
            },
            Workspace {
                truncated: true,
                ..w.clone()
            },
            Workspace {
                error: Some("offline".into()),
                ..w.clone()
            },
        ] {
            scene.update(Some("root"), &partial, 0.05);
            assert_eq!(scene.workers["gone"].particles, midway);
            assert!(scene.workers["healthy"].retired.is_none());
        }
        w.nodes.push(node("gone", "root"));
        scene.update(Some("root"), &w, 0.0);
        assert_eq!(scene.workers["gone"].particles, midway);
        assert!(scene.workers["gone"].retired.is_none());
        scene.update(Some("root"), &w, 1.0 / 60.0);
        assert!(
            midway
                .iter()
                .zip(scene.workers["gone"].particles)
                .all(|(a, b)| a.distance(b) < 0.101)
        );
        settle(&mut scene, &w);
        assert!(
            scene.workers["gone"]
                .particles
                .iter()
                .all(|p| p.distance(scene.workers["gone"].anchor) < 0.25)
        );
    }

    #[test]
    fn later_tiers_favor_nearby_parents_without_skipping_tiers() {
        let mut nodes: Vec<_> = (0..8)
            .map(|i| node(&format!("worker-{i:02}"), "root"))
            .collect();
        nodes.push(node("child", "worker-06"));
        let mut scene = Scene::new();
        scene.update(Some("root"), &workspace(nodes), 0.02);
        let parent = scene.workers["worker-06"].anchor;
        let child = &scene.workers["child"];
        assert_eq!(child.origin, parent);
        assert!((8..16).contains(&child.slot));
        assert!((8..16).all(|slot| {
            child.anchor.distance_squared(parent)
                <= slot_position(slot).distance_squared(parent) + 0.0001
        }));
    }

    #[test]
    fn over_sixty_four_workers_are_rejected_without_displacing_live_stations() {
        let mut scene = Scene::new();
        let mut w = workspace(
            (0..64)
                .map(|i| node(&format!("w-{i:02}"), "root"))
                .collect(),
        );
        scene.update(Some("root"), &w, 0.02);
        let before = anchors(&scene);
        w.nodes.push(node("overflow", "root"));
        assert!(!w.valid());
        scene.update(Some("root"), &w, 0.02);
        assert_eq!(anchors(&scene), before);
        assert!(scene.stations().iter().all(|s| s.state == State::Unknown));
    }

    #[test]
    fn ordinary_state_changes_never_teleport_particles_or_stations() {
        let mut w = workspace(vec![node("a", "root")]);
        let mut scene = Scene::new();
        settle(&mut scene, &w);
        let anchor = scene.stations()[0].position;
        for state in [
            State::Waiting,
            State::Completed,
            State::Error,
            State::Unknown,
            State::Running,
        ] {
            let before = scene.workers["a"].particles;
            w.nodes[0].state = state;
            scene.update(Some("root"), &w, 1.0 / 72.0);
            for (a, b) in before.iter().zip(scene.workers["a"].particles) {
                assert!(a.distance(b) < 0.035);
            }
            assert_eq!(scene.stations()[0].position, anchor);
        }
        assert_eq!(scene.stations()[0].state, State::Running);
    }

    #[test]
    fn historical_terminal_forms_are_distinct_deterministic_and_gently_ambient() {
        for state in [State::Completed, State::Error] {
            let w = workspace(vec![Node {
                state,
                ..node("a", "root")
            }]);
            let mut first = Scene::new();
            let mut second = Scene::new();
            first.update(Some("root"), &w, 0.0);
            second.update(Some("root"), &w, 0.0);
            let worker = &first.workers["a"];
            let positions = worker.particles;
            assert!(worker.terminal_motion.is_none());
            assert!(!worker.flying);
            assert_eq!(positions, second.workers["a"].particles);
            for (i, p) in positions.iter().enumerate() {
                assert_eq!(*p, terminal_position(worker.anchor, state, i));
                for other in positions.iter().skip(i + 1) {
                    assert!(p.distance(*other) > 0.059, "{p:?} {other:?}");
                }
            }
            for _ in 0..180 {
                let before = first.workers["a"].particles;
                first.update(Some("root"), &w, 1.0 / 60.0);
                second.update(Some("root"), &w, 1.0 / 60.0);
                assert_eq!(first.workers["a"].particles, second.workers["a"].particles);
                assert!(
                    before
                        .iter()
                        .zip(first.workers["a"].particles)
                        .all(|(a, b)| a.distance(b) < 0.0004)
                );
                assert_terminal_form(&first.workers["a"]);
                assert_eq!(
                    first.cubes().len(),
                    if state == State::Completed {
                        1
                    } else {
                        PARTICLES
                    }
                );
            }
            assert_ne!(first.workers["a"].particles, positions);
            assert!(first.cubes().iter().all(Cube::valid));
        }

        // The error form has an open split in station-tangential coordinates.
        let anchor = slot_position(0);
        let radial = Vec3::new(anchor.x, 0.0, anchor.z).normalize();
        let tangent = Vec3::new(-radial.z, 0.0, radial.x);
        let local_x = |state, i| (terminal_position(anchor, state, i) - anchor).dot(tangent);
        assert!((0..PARTICLES).all(|i| local_x(State::Error, i).abs() > 0.06));
        assert!((0..12).all(|i| local_x(State::Error, i) < 0.0));
        assert!((12..24).all(|i| local_x(State::Error, i) > 0.0));
    }

    #[test]
    fn terminal_bounce_occurs_once_then_resumes_and_replays_on_new_transition() {
        let mut scene = Scene::new();
        let mut w = workspace(vec![node("a", "root")]);
        settle(&mut scene, &w);
        let initial = scene.workers["a"].particles;
        w.nodes[0].state = State::Completed;
        scene.update(Some("root"), &w, 0.0);
        assert_eq!(scene.workers["a"].particles, initial);
        assert!(scene.workers["a"].terminal_motion.unwrap().bounce);
        for _ in 0..12 {
            let before = scene.workers["a"].particles;
            scene.update(Some("root"), &w, 1.0 / 60.0);
            assert!(
                before
                    .iter()
                    .zip(scene.workers["a"].particles)
                    .all(|(a, b)| a.distance(b) < 0.035)
            );
        }
        let worker = &scene.workers["a"];
        let motion = worker.terminal_motion.unwrap();
        let t = motion.elapsed / motion.duration;
        let baseline = worker.flight_start[0].lerp(
            terminal_position(worker.anchor, State::Completed, 0),
            smooth(t),
        );
        assert!(worker.particles[0].y - baseline.y > 0.03);
        settle(&mut scene, &w);
        assert!(scene.workers["a"].terminal_motion.is_none());
        for _ in 0..120 {
            let before = scene.workers["a"].particles;
            scene.update(Some("root"), &w, 1.0 / 60.0);
            assert!(
                before
                    .iter()
                    .zip(scene.workers["a"].particles)
                    .all(|(a, b)| a.distance(b) < 0.0004)
            );
            assert!(scene.workers["a"].terminal_motion.is_none());
        }

        let settled = scene.workers["a"].particles;
        w.nodes[0].state = State::Running;
        scene.update(Some("root"), &w, 0.0);
        assert_eq!(scene.workers["a"].particles, settled);
        settle(&mut scene, &w);
        assert_ne!(scene.workers["a"].particles, settled);
        w.nodes[0].state = State::Completed;
        scene.update(Some("root"), &w, 0.0);
        assert!(scene.workers["a"].terminal_motion.unwrap().bounce);
    }

    #[test]
    fn newly_observed_terminal_worker_arrives_without_a_fabricated_bounce() {
        for state in [State::Completed, State::Error] {
            let mut scene = Scene::new();
            scene.update(Some("root"), &Workspace::default(), 0.0);
            let w = workspace(vec![Node {
                state,
                ..node("a", "root")
            }]);
            scene.update(Some("root"), &w, 0.0);
            assert!(scene.workers["a"].flying);
            assert!(scene.workers["a"].terminal_motion.is_none());
            assert!(scene.workers["a"].particles.iter().all(|p| *p == CENTER));
            for _ in 0..180 {
                let before = scene.workers["a"].particles;
                let before_scales = scene.workers["a"].particle_scales;
                scene.update(Some("root"), &w, 1.0 / 60.0);
                assert!(
                    before
                        .iter()
                        .zip(scene.workers["a"].particles)
                        .all(|(a, b)| a.distance(b) <= 0.101)
                );
                assert!(scene.workers["a"].terminal_motion.is_none());
                assert!(
                    before_scales
                        .iter()
                        .zip(scene.workers["a"].particle_scales)
                        .all(|(a, b)| a.distance(b) < 0.025)
                );
            }
            let worker = &scene.workers["a"];
            assert!(!worker.flying);
            assert_terminal_form(worker);
            for _ in 0..60 {
                let before = scene.workers["a"].particles;
                scene.update(Some("root"), &w, 1.0 / 60.0);
                assert!(
                    before
                        .iter()
                        .zip(scene.workers["a"].particles)
                        .all(|(a, b)| a.distance(b) < 0.0004)
                );
                assert_terminal_form(&scene.workers["a"]);
            }
        }
    }

    #[test]
    fn nonterminal_running_waiting_and_unknown_keep_moving() {
        for state in [State::Running, State::Waiting, State::Unknown] {
            let mut scene = Scene::new();
            let w = workspace(vec![Node {
                state,
                ..node("a", "root")
            }]);
            settle(&mut scene, &w);
            let before = scene.workers["a"].particles;
            for _ in 0..60 {
                scene.update(Some("root"), &w, 1.0 / 60.0);
            }
            assert_ne!(scene.workers["a"].particles, before);
        }
    }

    #[test]
    fn stale_partial_unknown_and_recovery_do_not_replay_terminal_bounce() {
        for state in [State::Completed, State::Error] {
            let mut scene = Scene::new();
            let mut w = workspace(vec![node("a", "root")]);
            settle(&mut scene, &w);
            w.nodes[0].state = state;
            scene.update(Some("root"), &w, 0.02);
            assert!(scene.workers["a"].terminal_motion.unwrap().bounce);
            let elapsed = scene.workers["a"].terminal_motion.unwrap().elapsed;
            scene.update(
                Some("root"),
                &Workspace {
                    stale: true,
                    ..w.clone()
                },
                0.02,
            );
            assert!(scene.workers["a"].terminal_motion.unwrap().elapsed > elapsed);
            scene.update(Some("root"), &w, 0.02);
            assert!(scene.workers["a"].terminal_motion.unwrap().elapsed > elapsed + 0.02);
            settle(&mut scene, &w);
            for uncertain in [
                Workspace {
                    stale: true,
                    ..w.clone()
                },
                Workspace {
                    truncated: true,
                    ..Default::default()
                },
                Workspace {
                    error: Some("offline".into()),
                    ..Default::default()
                },
            ] {
                for _ in 0..20 {
                    scene.update(Some("root"), &uncertain, 0.05);
                }
                assert_terminal_form(&scene.workers["a"]);
                assert!(scene.workers["a"].terminal_motion.is_none());
                scene.update(Some("root"), &w, 0.02);
                assert_terminal_form(&scene.workers["a"]);
                assert!(scene.workers["a"].terminal_motion.is_none());
            }
            let unknown = workspace(vec![Node {
                state: State::Unknown,
                ..node("a", "root")
            }]);
            let positions = scene.workers["a"].particles;
            scene.update(Some("root"), &unknown, 0.0);
            assert_eq!(scene.workers["a"].particles, positions);
            for _ in 0..60 {
                let before = scene.workers["a"].particles;
                scene.update(Some("root"), &unknown, 1.0 / 60.0);
                assert!(
                    before
                        .iter()
                        .zip(scene.workers["a"].particles)
                        .all(|(a, b)| a.distance(b) <= 0.101)
                );
            }
            assert_ne!(scene.workers["a"].particles, positions);
            let before = scene.workers["a"].particles;
            scene.update(Some("root"), &w, 0.0);
            assert_eq!(scene.workers["a"].particles, before);
            assert!(!scene.workers["a"].terminal_motion.unwrap().bounce);
            settle(&mut scene, &w);
            assert_terminal_form(&scene.workers["a"]);
        }

        let mut scene = Scene::new();
        let mut w = workspace(vec![node("a", "root")]);
        settle(&mut scene, &w);
        scene.update(
            Some("root"),
            &Workspace {
                stale: true,
                ..w.clone()
            },
            0.02,
        );
        w.nodes[0].state = State::Completed;
        scene.update(Some("root"), &w, 0.0);
        assert!(!scene.workers["a"].terminal_motion.unwrap().bounce);
        settle(&mut scene, &w);
        assert!(scene.workers["a"].terminal_motion.is_none());
    }

    #[test]
    fn status_changes_mid_flight_and_terminal_return_never_teleport() {
        for state in [State::Completed, State::Error] {
            let mut scene = Scene::new();
            let mut w = workspace(vec![node("a", "root")]);
            scene.update(Some("root"), &Workspace::default(), 0.0);
            scene.update(Some("root"), &w, 0.0);
            for _ in 0..25 {
                scene.update(Some("root"), &w, 1.0 / 60.0);
            }
            let before = scene.workers["a"].particles;
            let before_scales = scene.workers["a"].particle_scales;
            w.nodes[0].state = state;
            scene.update(Some("root"), &w, 0.0);
            assert_eq!(scene.workers["a"].particles, before);
            assert_eq!(scene.workers["a"].particle_scales, before_scales);
            assert!(scene.workers["a"].terminal_motion.unwrap().bounce);
            for _ in 0..180 {
                let before = scene.workers["a"].particles;
                let before_scales = scene.workers["a"].particle_scales;
                scene.update(Some("root"), &w, 1.0 / 60.0);
                assert!(
                    before
                        .iter()
                        .zip(scene.workers["a"].particles)
                        .all(|(a, b)| a.distance(b) <= 0.101)
                );
                assert!(scene.cubes().iter().all(Cube::valid));
                assert!(
                    before_scales
                        .iter()
                        .zip(scene.workers["a"].particle_scales)
                        .all(|(a, b)| a.distance(b) < 0.025)
                );
            }
            let shape = scene.workers["a"].particles;
            assert!(scene.workers["a"].terminal_motion.is_none());
            let empty = Workspace::default();
            scene.update(Some("root"), &empty, 0.0);
            assert_eq!(scene.workers["a"].particles, shape);
            for _ in 0..30 {
                let before = scene.workers["a"].particles;
                scene.update(Some("root"), &empty, 1.0 / 60.0);
                assert!(
                    before
                        .iter()
                        .zip(scene.workers["a"].particles)
                        .all(|(a, b)| a.distance(b) <= 0.101)
                );
            }
            let returning = scene.workers["a"].particles;
            scene.update(Some("root"), &w, 0.0);
            assert_eq!(scene.workers["a"].particles, returning);
            assert!(!scene.workers["a"].terminal_motion.unwrap().bounce);
            settle(&mut scene, &w);
            assert_terminal_form(&scene.workers["a"]);
        }
    }

    #[test]
    fn nested_workers_depart_from_actual_parent_even_when_missing() {
        let mut scene = Scene::new();
        let w = workspace(vec![node("child", "parent"), node("parent", "root")]);
        scene.update(Some("root"), &w, 0.0);
        assert_eq!(scene.workers["parent"].origin, CENTER);
        assert_eq!(
            scene.workers["child"].origin,
            scene.workers["parent"].anchor
        );
        let mut missing = Scene::new();
        missing.update(
            Some("root"),
            &workspace(vec![node("child", "archived")]),
            0.02,
        );
        assert_eq!(
            missing.workers["child"].origin,
            slot_position(missing.slots["archived"])
        );
        assert_ne!(missing.workers["child"].origin, CENTER);
        assert_eq!(missing.stations().len(), 1);
    }

    #[test]
    fn new_child_without_an_observed_parent_does_not_launch_from_a_fictional_station() {
        let mut scene = Scene::new();
        scene.update(Some("root"), &Workspace::default(), 0.0);
        let w = workspace(vec![node("child", "missing")]);
        scene.update(Some("root"), &w, 0.0);
        let child = &scene.workers["child"];
        assert_eq!(child.origin, slot_position(scene.slots["missing"]));
        assert_eq!(child.age, child.arrival_seconds);
        assert!(
            child
                .particles
                .iter()
                .all(|p| p.distance(child.anchor) < 0.25)
        );
        assert_eq!(scene.stations().len(), 1);
    }

    #[test]
    fn snapshots_and_changes_render_only_flocks_not_inferred_traffic() {
        let mut scene = Scene::new();
        let mut w = workspace(vec![
            node("parent", "root"),
            node("child", "parent"),
            node("orphan", "absent"),
        ]);
        w.presentations.push(Presentation {
            id: "historical-card".into(),
            source_id: "parent".into(),
            source_message_id: "historical-message".into(),
            ..Default::default()
        });
        // An initial snapshot, including historical messages, is not a new
        // interaction. Even the first visible frame contains only flocks.
        scene.update(Some("root"), &w, 0.02);
        assert_only_worker_flocks(&scene);
        settle(&mut scene, &w);
        let original_anchors = anchors(&scene);
        let before = scene.workers["parent"].particles;
        for _ in 0..30 {
            scene.update(Some("root"), &w, 1.0 / 60.0);
            assert_only_worker_flocks(&scene);
        }
        // Natural status motion continues without a connector stream.
        assert_ne!(before, scene.workers["parent"].particles);

        w.nodes[0].state = State::Completed;
        w.nodes[1].title = "Updated title".into();
        w.presentations[0].source_message_id = "another-message".into();
        scene.update(Some("root"), &w, 0.02);
        assert_only_worker_flocks(&scene);
        assert_eq!(anchors(&scene), original_anchors);

        for partial in [
            Workspace {
                stale: true,
                ..w.clone()
            },
            Workspace {
                error: Some("unavailable".into()),
                ..w.clone()
            },
            Workspace {
                truncated: true,
                ..w.clone()
            },
        ] {
            scene.update(Some("root"), &partial, 0.02);
            assert_only_worker_flocks(&scene);
            assert_eq!(anchors(&scene), original_anchors);
        }

        // After the parent departs, neither its remembered slot nor the
        // independently absent parent produces a broken-end beacon.
        w.nodes.retain(|node| node.id != "parent");
        settle(&mut scene, &w);
        assert!(!scene.workers.contains_key("parent"));
        assert_eq!(
            slot_position(scene.slots["parent"]).to_array(),
            original_anchors["parent"]
        );
        assert_eq!(anchors(&scene)["child"], original_anchors["child"]);
        assert_only_worker_flocks(&scene);
    }

    #[test]
    fn flocks_form_smoothly_from_coordinator_and_parent_in_arrival_time() {
        let mut scene = Scene::new();
        let w = workspace(vec![node("parent", "root"), node("child", "parent")]);
        scene.update(Some("root"), &Workspace::default(), 0.0);
        scene.update(Some("root"), &w, 0.0);
        let parent_anchor = scene.workers["parent"].anchor;
        assert_eq!(scene.workers["parent"].origin, CENTER);
        assert_eq!(scene.workers["child"].origin, parent_anchor);
        for worker in scene.workers.values() {
            assert!(
                worker
                    .particles
                    .iter()
                    .all(|p| p.distance(worker.origin) < 0.05)
            );
        }
        let station_anchors = anchors(&scene);
        let dt = 1.0 / 60.0;
        // Include the first frame after the 1.5-second arrival boundary.
        for frame in 1..=91 {
            let before: BTreeMap<_, _> = scene
                .workers
                .iter()
                .map(|(id, worker)| (id.clone(), worker.particles))
                .collect();
            scene.update(Some("root"), &w, dt);
            assert_only_worker_flocks(&scene);
            assert_eq!(anchors(&scene), station_anchors);
            for (id, worker) in &scene.workers {
                for (previous, current) in before[id].iter().zip(worker.particles) {
                    assert!(previous.distance(current) <= 6.0 * dt + 0.00001);
                }
                if frame == 45 {
                    let centroid =
                        worker.particles.iter().copied().sum::<Vec3>() / PARTICLES as f32;
                    let path = worker.anchor - worker.origin;
                    let progress = (centroid - worker.origin).dot(path) / path.length_squared();
                    assert!((0.3..0.7).contains(&progress), "{id}: {progress}");
                }
                if frame == 90 {
                    assert!((worker.age - ARRIVAL_SECONDS).abs() < 0.00001);
                    assert!(
                        worker
                            .particles
                            .iter()
                            .all(|p| p.distance(worker.anchor) < 0.25)
                    );
                }
            }
        }
    }

    #[test]
    fn arrival_departure_and_completion_are_not_fake_progress() {
        let mut scene = Scene::new();
        let mut w = workspace(vec![node("a", "root")]);
        scene.update(Some("root"), &Workspace::default(), 0.0);
        scene.update(Some("root"), &w, 0.0);
        assert!(scene.workers["a"].particles[0].distance(CENTER) < 0.05);
        settle(&mut scene, &w);
        assert!(scene.workers["a"].particles[0].distance(scene.workers["a"].anchor) < 0.25);
        w.nodes[0].state = State::Completed;
        settle(&mut scene, &w);
        assert_eq!(scene.stations()[0].state, State::Completed);
        assert_eq!(scene.stations()[0].opacity, 1.0);
        scene.update(Some("root"), &Workspace::default(), 0.02);
        assert!(!scene.cubes().is_empty());
        assert!(scene.stations()[0].opacity < 1.0);
        settle(&mut scene, &Workspace::default());
        assert!(scene.stations().is_empty());
        assert!(scene.cubes().is_empty());
    }

    #[test]
    fn stale_partial_and_invalid_data_preserve_omitted_workers_as_unknown() {
        let w = workspace(vec![Node {
            state: State::Completed,
            ..node("a", "root")
        }]);
        let mut scene = Scene::new();
        settle(&mut scene, &w);
        for bad in [
            Workspace {
                error: Some("offline".into()),
                ..Default::default()
            },
            Workspace {
                truncated: true,
                ..Default::default()
            },
            Workspace {
                stale: true,
                ..w.clone()
            },
            workspace(vec![node("bad", "bad")]),
        ] {
            scene.update(Some("root"), &bad, 0.02);
            assert_eq!(scene.stations().len(), 1);
            assert_eq!(scene.stations()[0].state, State::Unknown);
            assert!(scene.cubes().iter().all(Cube::valid));
        }
        scene.update(Some("root"), &w, 0.02);
        assert_eq!(scene.stations()[0].state, State::Completed);
    }

    #[test]
    fn fresh_partial_tree_retains_omissions_as_unknown_and_observed_states() {
        let mut scene = Scene::new();
        let partial = Workspace {
            nodes: vec![
                Node {
                    state: State::Unknown,
                    ..node("a", "root")
                },
                Node {
                    state: State::Completed,
                    ..node("b", "root")
                },
            ],
            truncated: true,
            ..Default::default()
        };
        scene.update(Some("root"), &partial, 0.02);
        assert_eq!(scene.stations().len(), 2);
        assert_eq!(scene.workers["a"].state, State::Unknown);
        assert_eq!(scene.workers["b"].state, State::Completed);
        assert_eq!(scene.stations()[1].state, State::Completed);
        scene.update(
            Some("root"),
            &Workspace {
                nodes: vec![node("c", "root")],
                error: Some("unavailable".into()),
                ..Default::default()
            },
            0.02,
        );
        assert_eq!(scene.stations().len(), 3);
        assert_eq!(scene.stations()[0].state, State::Unknown);
        assert_eq!(scene.stations()[1].state, State::Unknown);
        assert_eq!(scene.stations()[2].state, State::Running);
        assert!(
            scene
                .workers
                .values()
                .all(|worker| worker.retired.is_none())
        );
    }

    #[test]
    fn truncated_sixty_four_workers_keep_observed_states() {
        let mut scene = Scene::new();
        let partial = Workspace {
            nodes: (0..64)
                .map(|i| Node {
                    state: if i == 7 {
                        State::Unknown
                    } else {
                        State::Completed
                    },
                    ..node(&format!("worker-{i:02}"), "root")
                })
                .collect(),
            truncated: true,
            ..Default::default()
        };
        scene.update(Some("root"), &partial, 0.02);
        assert_eq!(scene.stations().len(), 64);
        assert_eq!(scene.stations()[7].state, State::Unknown);
        assert!(scene.stations().iter().enumerate().all(|(i, s)| s.state
            == if i == 7 {
                State::Unknown
            } else {
                State::Completed
            }));
    }

    #[test]
    fn store_error_keeps_status_but_cannot_authorize_removals() {
        let mut scene = Scene::new();
        settle(
            &mut scene,
            &workspace(vec![node("a", "root"), node("b", "root")]),
        );
        let partial = Workspace {
            nodes: vec![Node {
                state: State::Completed,
                ..node("a", "root")
            }],
            error: Some("Presentation store unavailable".into()),
            ..Default::default()
        };
        settle(&mut scene, &partial);
        assert_eq!(scene.stations().len(), 2);
        assert_eq!(scene.stations()[0].state, State::Completed);
        assert_eq!(scene.stations()[1].state, State::Unknown);
        assert!(
            scene
                .workers
                .values()
                .all(|worker| worker.retired.is_none())
        );
        scene.update(
            Some("root"),
            &Workspace {
                stale: true,
                ..partial
            },
            0.02,
        );
        assert!(scene.stations().iter().all(|s| s.state == State::Unknown));
    }

    #[test]
    fn sixty_four_workers_and_rapid_churn_stay_within_budget() {
        let mut scene = Scene::new();
        for generation in 0..8 {
            let w = workspace(
                (0..64)
                    .map(|i| Node {
                        state: match i % 3 {
                            0 => State::Completed,
                            1 => State::Error,
                            _ => State::Running,
                        },
                        ..node(
                            &format!("worker-{generation}-{i}"),
                            &format!("missing-{generation}-{i}"),
                        )
                    })
                    .collect(),
            );
            for _ in 0..3 {
                scene.update(Some("root"), &w, 0.02);
                assert!(scene.cubes().len() <= MAX_CUBES);
                assert!(scene.cubes().iter().all(Cube::valid));
                assert_only_worker_flocks(&scene);
                assert_eq!(
                    scene.cubes().len(),
                    scene
                        .workers
                        .values()
                        .map(|worker| if solid_completed(worker) {
                            1
                        } else {
                            PARTICLES
                        })
                        .sum::<usize>()
                );
                assert!(scene.workers.len() <= MAX_WORKERS);
                assert!(scene.slots.len() <= SLOT_COUNT);
                assert!(scene.stations().len() <= MAX_WORKERS);
                assert!(scene.stations().iter().all(Station::valid));
                assert_eq!(
                    scene
                        .workers
                        .values()
                        .filter(|w| w.retired.is_none())
                        .count(),
                    64
                );
                assert!(
                    scene
                        .stations()
                        .iter()
                        .take(MAX_NODES)
                        .all(|station| scene.workers[&station.id].retired.is_none())
                );
                let live: BTreeSet<_> = scene
                    .workers
                    .values()
                    .filter(|w| w.retired.is_none())
                    .map(|w| w.slot)
                    .collect();
                assert_eq!(live.len(), 64);
                let required: BTreeSet<_> = w
                    .nodes
                    .iter()
                    .flat_map(|node| [node.id.as_str(), node.parent_id.as_deref().unwrap()])
                    .map(|id| scene.slots[id])
                    .collect();
                assert_eq!(required.len(), 128);
                if generation > 0 {
                    assert!(required.iter().any(|&slot| slot >= 128));
                }
            }
        }
    }

    #[test]
    fn nonfinite_negative_zero_and_resume_dt_are_safe() {
        let w = workspace(vec![node("a", "root")]);
        let mut scene = Scene::new();
        settle(&mut scene, &w);
        for dt in [f32::NAN, f32::INFINITY, -1.0, 0.0] {
            let before = scene.workers["a"].particles;
            scene.update(Some("root"), &w, dt);
            assert_eq!(before, scene.workers["a"].particles);
        }
        let before = scene.workers["a"].particles;
        scene.update(Some("root"), &w, 1000.0);
        for (a, b) in before.iter().zip(scene.workers["a"].particles) {
            assert!(a.distance(b) <= 0.301);
        }
        assert!(scene.cubes().iter().all(Cube::valid));
    }
}
