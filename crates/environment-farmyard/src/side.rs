// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

//! The farmyard's server side: it owns the flock of each space bound to the farmyard, moves it on by the farmyard's
//! clock, the time since it started, takes participants' notices, and shares every change with the clients in the
//! space, with the clock, through the environment state channel. See `design/farmyard.md`.
use std::sync::Arc;
use std::time::{Duration, Instant};

use tomori_environment::{Error, Object, Result, ServerSide, Space};

use crate::chicken::Chicken;
use crate::flock::{Event, Flock, Intent, seed};

/// How long the side waits for an intent before moving the flock on, and how often it describes the chickens to the
/// space's agent.
pub const TICK: Duration = Duration::from_millis(50);
pub const DESCRIBE: Duration = Duration::from_secs(1);

/// The farmyard's server side.
#[derive(Default)]
pub struct FarmyardSide;

fn json(value: impl serde::Serialize) -> String {
    serde_json::to_string(&value).expect("the flock's messages are JSON")
}

impl ServerSide for FarmyardSide {
    fn run(&self, space: &Space) -> Result<()> {
        run(space, Chicken::shared(), Instant::now())
    }
}

/// Run the flock of `space`, for `chicken`, its clock beginning at `start`, until the space's cancellation; or fail at
/// once, without the chicken, which could not be loaded.
pub fn run(space: &Space, chicken: Option<Arc<Chicken>>, start: Instant) -> Result<()> {
    let Some(chicken) = chicken else {
        return Err(Error::new("The farmyard's chickens could not be loaded."));
    };
    let clock = || start.elapsed().as_millis() as u64;
    let mut flock = Flock::new(seed(space.id()), chicken, clock());
    space.share(json(flock.state(clock())))?;
    let mut described: Option<Instant> = None;
    while !space.cancellation().is_cancelled() {
        let mut changes = Vec::new();
        if let Some(intent) = space.next_intent(TICK) {
            match serde_json::from_str::<Intent>(intent.payload()) {
                Ok(asked) => changes.extend(flock.ask(intent.participant(), asked, clock())),
                Err(_) => log::debug!("A farmyard intent could not be read."),
            }
        }
        changes.extend(flock.update(clock()));
        for (chicken, behaviour) in changes {
            let now = clock();
            space.announce(
                json(Event {
                    clock: now,
                    chicken,
                    behaviour,
                }),
                json(flock.state(now)),
            )?;
        }
        if described.is_none_or(|described| described.elapsed() >= DESCRIBE) {
            described = Some(Instant::now());
            space.describe(
                flock
                    .describe(clock())
                    .into_iter()
                    .map(|(id, description)| Object::new(id, description))
                    .collect(),
            )?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests;
