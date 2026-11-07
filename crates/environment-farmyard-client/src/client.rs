// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

//! The farmyard's client side, for one space: the flock as its server side shares it, each chicken with its controller,
//! moved on by the farmyard's clock, which it reads from the server's messages; what the ray points at; the notices it
//! asks for as a participant points at a chicken and looks away; the clucks it hears; and what its scenery draws.
use std::sync::Arc;
use std::time::Instant;

use glam::Vec3;
use tomori_assets::Instance;
use tomori_scene::channel::Update;
use tomori_scene::client_side::{ClientSide, Input, Output, Picked, Sound};
use tomori_scene::pointing::Ray;

use tomori_environment_farmyard::chicken::{CLUCK, Chicken, SQUAWK};
use tomori_environment_farmyard::cluck::{Cluck, RATE, rendered};
use tomori_environment_farmyard::controller::ChickenController;
use tomori_environment_farmyard::flock::{self, Event, Intent, MOST, PER_CHICKEN, State};

/// What the farmyard's scenery draws this frame: each chicken, posed and placed.
pub struct Drawn {
    pub chickens: Vec<Instance>,
}

/// How often, on average, a chicken clucks of its own accord, in slots of this many milliseconds, and the chance it
/// does in each.
pub const CLUCK_SLOT: u64 = 30_000;
const CLUCK_CHANCE: f32 = 0.3;
/// Clucks older than this, such as those a client joining late would hear of, are not heard.
const LATE: u64 = 1_000;

/// The SplitMix64 finalizer.
fn mix(value: u64) -> u64 {
    let mut z = value.wrapping_add(0x9e37_79b9_7f4a_7c15);
    z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    z ^ (z >> 31)
}

/// The farmyard's client side for one space.
pub struct FarmyardClient {
    chicken: Option<Arc<Chicken>>,
    seed: u64,
    /// When this client side began, which its own clock counts from.
    epoch: Instant,
    /// The farmyard's clock less this one's, in milliseconds, as the server's messages say.
    offset: Option<i64>,
    controllers: Vec<ChickenController>,
    /// Each chicken's name, by which the participant knows it.
    names: Vec<String>,
    /// The farmyard's clock when it last moved on, for what clucked since.
    last: Option<u64>,
    /// The chicken whose notice this participant holds, pointing at it, and when they last asked each to notice them.
    held: Option<usize>,
    asked: [Option<Instant>; MOST],
}

impl FarmyardClient {
    /// The client side for the space `space`, with `chicken`, or none, if it could not be loaded, when it shows nothing.
    pub fn new(space: &str, chicken: Option<Arc<Chicken>>, now: Instant) -> Self {
        Self {
            chicken,
            seed: flock::seed(space),
            epoch: now,
            offset: None,
            controllers: Vec::new(),
            names: Vec::new(),
            last: None,
            held: None,
            asked: [None; MOST],
        }
    }

    /// This client's own clock at `now`, in milliseconds.
    fn local(&self, now: Instant) -> i64 {
        now.saturating_duration_since(self.epoch).as_millis() as i64
    }

    /// The farmyard's clock at `now`, once a message has said what it is.
    pub fn clock(&self, now: Instant) -> Option<u64> {
        self.offset
            .map(|offset| self.local(now).saturating_add(offset).max(0) as u64)
    }

    /// Take what the server side sent at `now`: a state, which begins anew, or a chicken's new behaviour. Each says the
    /// farmyard's clock as it was sent, which can only be later by the time it arrives, so the latest reading is kept
    /// for a state, and the earliest offset any event shows.
    fn receive(&mut self, update: Update, now: Instant) {
        let local = self.local(now);
        // `Update` is non-exhaustive: an update of a kind this client does not know, such as a later step of state
        // synchronization's, is ignored.
        if let Update::State(state) = &update {
            let Ok(state) = serde_json::from_value::<State>(state.value().clone()) else {
                log::debug!("A farmyard state could not be read.");
                return;
            };
            self.offset = Some((state.clock as i64).saturating_sub(local));
            self.controllers = match &self.chicken {
                Some(chicken) => state
                    .chickens
                    .iter()
                    .take(MOST)
                    .map(|hen| ChickenController::new(chicken.clone(), hen))
                    .collect(),
                None => Vec::new(),
            };
            self.names = state.chickens.iter().map(|hen| hen.name.clone()).collect();
            self.held = self.held.filter(|held| *held < self.controllers.len());
        } else if let Update::Event(event) = &update {
            let Ok(event) = serde_json::from_value::<Event>(event.value().clone()) else {
                log::debug!("A farmyard event could not be read.");
                return;
            };
            self.offset = self
                .offset
                .map(|offset| offset.max((event.clock as i64).saturating_sub(local)));
            if let Some(controller) = self.controllers.get_mut(event.chicken) {
                controller.behave(event.behaviour);
            }
        }
    }

    /// The clucks due after `from` and up to `to`, on the farmyard's clock: each chicken's own, at most one in each slot
    /// of [`CLUCK_SLOT`], at a moment the space, the chicken and the slot choose, the same for everyone.
    fn clucks_between(&self, from: u64, to: u64) -> Vec<(usize, Cluck)> {
        let mut clucks = Vec::new();
        for slot in from / CLUCK_SLOT..=to / CLUCK_SLOT {
            for chicken in 0..self.controllers.len() {
                let draw = mix(self.seed ^ mix(chicken as u64 ^ (slot << 8)));
                let at = slot * CLUCK_SLOT + mix(draw) % CLUCK_SLOT;
                let chance = (draw >> 40) as f32 / (1u64 << 24) as f32;
                if chance < CLUCK_CHANCE && from < at && at <= to {
                    let kind = if mix(mix(draw)).is_multiple_of(3) {
                        Cluck::Double
                    } else {
                        Cluck::Single
                    };
                    clucks.push((chicken, kind));
                }
            }
        }
        clucks
    }

    /// What to ask as the participant points at `pointed`, from `head`, at `now`: a notice of a chicken they begin
    /// pointing at, unless they asked it within [`PER_CHICKEN`], and the end of the one they held as they look away.
    fn point(
        &mut self,
        pointed: Option<&str>,
        head: Option<Vec3>,
        now: Instant,
    ) -> Vec<tomori_protocol::Payload> {
        let chicken = pointed
            .and_then(|pointed| pointed.strip_prefix("chicken-"))
            .and_then(|index| index.parse::<usize>().ok())
            .filter(|index| *index < self.controllers.len());
        let mut intents = Vec::new();
        if self.held.is_some() && self.held != chicken {
            let away = self.held.take().expect("held");
            intents.push(Intent::Away { away });
        }
        if let (Some(chicken), Some(head)) = (chicken, head)
            && self.held != Some(chicken)
            && self.asked[chicken].is_none_or(|asked| {
                now.saturating_duration_since(asked).as_millis() as u64 >= PER_CHICKEN
            })
        {
            self.asked[chicken] = Some(now);
            self.held = Some(chicken);
            intents.push(Intent::Notice {
                notice: chicken,
                toward: [head.x, head.z],
            });
        }
        intents
            .into_iter()
            .map(|intent| {
                tomori_protocol::Payload::new(
                    serde_json::to_value(intent).expect("an intent is JSON"),
                )
            })
            .collect()
    }
}

impl ClientSide for FarmyardClient {
    fn pick(&self, ray: &Ray) -> Option<Picked> {
        self.controllers
            .iter()
            .zip(&self.names)
            .enumerate()
            .filter_map(|(index, (controller, name))| {
                let (centre, radius) = controller.body();
                tomori_scene::pointing::Shape::Sphere {
                    center: centre,
                    radius,
                }
                .intersect(ray)
                .map(|distance| Picked::new(format!("chicken-{index}"), name.clone(), distance))
            })
            .min_by(|a, b| a.distance.total_cmp(&b.distance))
    }

    fn update(&mut self, input: Input<'_>) -> Output {
        for update in input.updates {
            self.receive(update, input.now);
        }
        let mut output = Output::new();
        let Some(clock) = self.clock(input.now) else {
            return output;
        };
        let sound = |kind: Cluck| {
            Sound::new(
                rendered()[Cluck::ALL
                    .iter()
                    .position(|cluck| *cluck == kind)
                    .expect("listed")]
                .clone(),
                RATE,
            )
        };
        for controller in &mut self.controllers {
            for event in controller.update(clock) {
                let kind = match event.name {
                    CLUCK => Cluck::Noticed,
                    SQUAWK => Cluck::Startled,
                    _ => continue,
                };
                if clock.saturating_sub((event.at * 1000.0) as u64) < LATE {
                    output = output.with_sound(sound(kind), controller.head().to_array());
                }
            }
        }
        // Clucks of their own accord, only as the clock moves on by less than a second, so that joining, or a clock that
        // jumps, clucks nothing.
        if let Some(last) = self
            .last
            .filter(|last| *last < clock && clock - last < LATE)
        {
            for (chicken, kind) in self.clucks_between(last, clock) {
                output =
                    output.with_sound(sound(kind), self.controllers[chicken].head().to_array());
            }
        }
        self.last = Some(clock);
        for intent in self.point(input.pointed, input.head, input.now) {
            output = output.with_intent(intent);
        }
        output.drawing(Arc::new(Drawn {
            chickens: self
                .controllers
                .iter()
                .map(|controller| controller.instance().clone())
                .collect(),
        }))
    }
}

#[cfg(test)]
mod tests;
