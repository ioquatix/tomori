// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

use crate::interaction::{Mode, Visual};
use glam::Vec3;

pub const AGENT_COUNT: usize = 128;
pub const CENTER: Vec3 = Vec3::new(0.0, 1.4, -2.3);

#[derive(Clone, Copy)]
pub struct Agent {
    pub position: Vec3,
    pub velocity: Vec3,
}

pub struct Swarm {
    pub agents: Vec<Agent>,
    pub visual: Visual,
    next: Vec<Agent>,
    time: f32,
}

impl Swarm {
    pub fn new() -> Self {
        // A deterministic spherical distribution: no RNG or assets required.
        let agents: Vec<_> = (0..AGENT_COUNT)
            .map(|i| {
                let y = 1.0 - 2.0 * (i as f32 + 0.5) / AGENT_COUNT as f32;
                let angle = i as f32 * 2.399_963_1;
                let r = (1.0 - y * y).sqrt();
                let offset = Vec3::new(r * angle.cos(), y * 0.6, r * angle.sin());
                Agent {
                    position: CENTER + offset * 0.85,
                    velocity: Vec3::new(-offset.z, 0.05, offset.x) * 0.35,
                }
            })
            .collect();
        Self {
            next: agents.clone(),
            agents,
            visual: Visual::default(),
            time: 0.0,
        }
    }

    /// Smooth the audio envelope, but never smooth away a discrete turn state.
    pub fn set_visual(&mut self, mut visual: Visual, dt: f32) {
        visual.time = self.visual.time + dt.clamp(0.0, 0.1);
        let rate = if visual.level > self.visual.level {
            24.0
        } else {
            9.0
        };
        visual.level = self.visual.level
            + (visual.level - self.visual.level) * (1.0 - (-rate * dt.clamp(0.0, 0.1)).exp());
        self.visual = visual;
    }

    pub fn step(&mut self, dt: f32, attractor: Option<Vec3>) {
        // Bound stalls (sleep/resume) rather than launching agents across the room.
        let dt = dt.clamp(0.0, 1.0 / 30.0);
        self.time += dt;
        let target =
            attractor.unwrap_or(CENTER + Vec3::new((self.time * 0.3).sin() * 0.35, 0.0, 0.0));
        if self.visual.mode != Mode::Neutral && attractor.is_none() {
            // A damped spring keeps state changes gentle and world-anchored.
            // Shape and motion, not color alone, distinguish the main states.
            for (i, agent) in self.agents.iter_mut().enumerate() {
                let t = i as f32 / AGENT_COUNT as f32;
                let (radius, height, depth, speed) = match self.visual.mode {
                    Mode::Working => (0.52, 0.36, 0.42, 0.8),
                    Mode::Preparing | Mode::Transcribing => (0.65, 0.42, 0.32, 0.45),
                    Mode::Ready => (0.94, 0.50, 0.20, 0.06),
                    Mode::Waiting => (0.85, 0.52, 0.17, 0.03),
                    Mode::Listening => (0.98, 0.44, 0.14, 0.08),
                    Mode::Speaking => (0.80, 0.44, 0.27, 0.16),
                    _ => (0.70, 0.40, 0.20, 0.025),
                };
                let angle = t * std::f32::consts::TAU + self.visual.time * speed;
                let ring = 0.75 + 0.25 * (i % 3) as f32 / 2.0;
                let ripple = if self.visual.mode == Mode::Speaking {
                    self.visual.amplitude() * (angle * 3.0 - self.visual.time * 5.0).sin() * 0.12
                } else {
                    0.0
                };
                let goal = CENTER
                    + Vec3::new(
                        angle.cos() * radius * ring,
                        angle.sin() * height * ring + ripple,
                        (angle * 3.0 + i as f32).sin() * depth,
                    );
                let force = (goal - agent.position) * 5.0 - agent.velocity * 3.8;
                agent.velocity =
                    (agent.velocity + force.clamp_length_max(3.0) * dt).clamp_length_max(0.85);
                agent.position += agent.velocity * dt;
            }
            return;
        }
        for (i, agent) in self.agents.iter().enumerate() {
            let mut separation = Vec3::ZERO;
            let mut alignment = Vec3::ZERO;
            let mut cohesion = Vec3::ZERO;
            let mut neighbors = 0.0;
            for (j, other) in self.agents.iter().enumerate() {
                if i == j {
                    continue;
                }
                let delta = agent.position - other.position;
                let d2 = delta.length_squared();
                if d2 < 0.22 * 0.22 {
                    separation += delta / d2.max(0.0025);
                }
                if d2 < 0.65 * 0.65 {
                    alignment += other.velocity;
                    cohesion += other.position;
                    neighbors += 1.0;
                }
            }
            let attraction = if attractor.is_some() { 1.5 } else { 0.7 };
            let mut force =
                separation * 0.055 + (target - agent.position) * attraction - agent.velocity * 0.45;
            if neighbors > 0.0 {
                force += (alignment / neighbors - agent.velocity) * 0.55;
                force += (cohesion / neighbors - agent.position) * 0.22;
            }
            let offset = agent.position - target;
            force += Vec3::new(-offset.z, (self.time + i as f32).sin() * 0.12, offset.x) * 0.3;
            // Keep agents above the floor even when a controller is lowered.
            force.y += (0.35 - agent.position.y).max(0.0) * 5.0;
            let velocity =
                (agent.velocity + force.clamp_length_max(3.0) * dt).clamp_length_max(0.85);
            self.next[i] = Agent {
                position: agent.position + velocity * dt,
                velocity,
            };
        }
        std::mem::swap(&mut self.agents, &mut self.next);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn conversational_shapes_stay_finite_and_settle_without_teleporting() {
        let mut swarm = Swarm::new();
        for mode in [
            Mode::Working,
            Mode::Speaking,
            Mode::Ready,
            Mode::Waiting,
            Mode::Listening,
            Mode::Unavailable,
        ] {
            for _ in 0..360 {
                let previous = swarm.agents[0].position;
                swarm.set_visual(
                    Visual {
                        mode,
                        level: 0.8,
                        ..Default::default()
                    },
                    1.0 / 72.0,
                );
                swarm.step(1.0 / 72.0, None);
                assert!(swarm.agents[0].position.distance(previous) <= 0.012);
                for agent in &swarm.agents {
                    assert!(agent.position.is_finite());
                    assert!(agent.position.distance(CENTER) < 1.5);
                }
            }
        }
    }

    #[test]
    fn swarm_is_finite_bounded_and_deterministic() {
        let mut a = Swarm::new();
        let mut b = Swarm::new();
        for _ in 0..1500 {
            a.step(1.0 / 72.0, None);
            b.step(1.0 / 72.0, None);
        }
        assert_eq!(a.agents.len(), AGENT_COUNT);
        for (a, b) in a.agents.iter().zip(&b.agents) {
            assert!(a.position.is_finite());
            assert!(a.position.distance(CENTER) < 3.0);
            assert!(a.velocity.length() <= 0.851);
            assert_eq!(a.position, b.position);
        }
    }

    #[test]
    fn trigger_attractor_moves_the_swarm() {
        let mut swarm = Swarm::new();
        let target = CENTER + Vec3::X * 1.5;
        for _ in 0..1000 {
            swarm.step(1.0 / 72.0, Some(target));
        }
        let mean = swarm.agents.iter().map(|a| a.position).sum::<Vec3>() / AGENT_COUNT as f32;
        assert!(
            mean.distance(target) < 0.6,
            "centroid {mean}, target {target}"
        );
    }

    #[test]
    fn zero_delta_and_resume_stalls_are_safe() {
        let mut swarm = Swarm::new();
        let start = swarm.agents[0].position;
        swarm.step(0.0, None);
        assert_eq!(start, swarm.agents[0].position);
        swarm.step(100.0, None);
        assert!(start.distance(swarm.agents[0].position) < 0.03);
    }
}
