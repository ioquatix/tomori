// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

//! A chicken's controller: it takes what the server side says the chicken does, and since when, and drives the
//! chicken's model and animator from it, so that every client shows the same chicken doing the same thing at the same
//! moment without the server sending a pose. It is the farmyard's own, and deliberately specific: see Tomori's
//! `design/animation.md`.
use std::sync::Arc;

use glam::{Mat4, Quat, Vec3};
use tomori_assets::Instance;
use tomori_assets::animation::{Animator, Event, Value};

use crate::behaviour::{Behaviour, Doing};
use crate::chicken::{CONE, Chicken};
use crate::flock::Hen;

/// Seconds on the animator's clock, from milliseconds on the farmyard's.
fn seconds(millis: u64) -> f64 {
    millis as f64 / 1000.0
}

/// One chicken, as a client shows it.
pub struct ChickenController {
    chicken: Arc<Chicken>,
    pace: f32,
    behaviour: Behaviour,
    animator: Animator,
    instance: Instance,
    /// Where it is now, and which way it faces.
    placed: crate::behaviour::Placed,
}

impl ChickenController {
    /// The chicken `hen`, as the server last said, for `chicken`'s model.
    pub fn new(chicken: Arc<Chicken>, hen: &Hen) -> Self {
        let animator = Animator::new(chicken.graph(hen.pace), seconds(hen.behaviour.since));
        let mut instance = Instance::new(chicken.model.clone());
        instance.set_shown(CONE, hen.cone);
        let placed = hen.behaviour.place(&chicken, hen.pace, hen.behaviour.since);
        let mut controller = Self {
            chicken,
            pace: hen.pace,
            behaviour: hen.behaviour,
            animator,
            instance,
            placed,
        };
        controller.behave(hen.behaviour);
        controller
    }

    /// What the chicken does now, as the server says.
    pub fn behaviour(&self) -> &Behaviour {
        &self.behaviour
    }

    /// The chicken begins `behaviour`, at its moment: its animator's parameters change then, and at the moments it
    /// plans, such as when a walk stops.
    pub fn behave(&mut self, behaviour: Behaviour) {
        self.behaviour = behaviour;
        let since = seconds(behaviour.since);
        let mut set = |name: &str, value| {
            self.animator.set(name, value, since);
        };
        match behaviour.doing {
            Doing::Walk { .. } => {
                set("confused", Value::Bool(false));
                let walk = behaviour.walk(&self.chicken, self.pace).expect("a walk");
                self.animator
                    .set("walking", Value::Bool(true), seconds(walk.walks));
                self.animator
                    .set("walking", Value::Bool(false), seconds(walk.stops));
            }
            Doing::Confused { .. } => {
                set("walking", Value::Bool(false));
                set("confused", Value::Bool(true));
            }
            Doing::Sprint { .. } => {
                set("walking", Value::Bool(false));
                set("confused", Value::Bool(false));
                set("startle", Value::Trigger(true));
            }
            Doing::Stand
            | Doing::Peck { .. }
            | Doing::Look { .. }
            | Doing::Recover
            | Doing::Roost => {
                set("walking", Value::Bool(false));
                set("confused", Value::Bool(false));
            }
        }
    }

    /// Move on to `now`, on the farmyard's clock: place and pose the chicken, and return the events its animator passed.
    pub fn update(&mut self, now: u64) -> Vec<Event> {
        let events = self.animator.advance(seconds(now)).events;
        let placed = self.behaviour.place(&self.chicken, self.pace, now);
        self.placed = placed;
        // It leans as it pecks about its feet, and faces its heading.
        self.instance.transform =
            Mat4::from_rotation_translation(Quat::from_rotation_y(placed.heading), placed.position)
                * Mat4::from_rotation_x(placed.lean)
                * self.chicken.placement;
        self.instance
            .set_pose(self.animator.pose(&self.chicken.model));
        events
    }

    /// The chicken, as drawn.
    pub fn instance(&self) -> &Instance {
        &self.instance
    }

    /// Where it stands, and which way it faces.
    pub fn placed(&self) -> crate::behaviour::Placed {
        self.placed
    }

    /// Where its head is, which it clucks from.
    pub fn head(&self) -> Vec3 {
        let ahead = crate::behaviour::facing(self.placed.heading) * 0.08;
        self.placed.position + Vec3::new(ahead.x, 0.3, ahead.y)
    }

    /// What the ray meets it by: a sphere about its body, its centre and radius.
    pub fn body(&self) -> (Vec3, f32) {
        let ahead = crate::behaviour::facing(self.placed.heading) * 0.03;
        (
            self.placed.position + Vec3::new(ahead.x, 0.18, ahead.y),
            0.2,
        )
    }
}

#[cfg(test)]
mod tests;
