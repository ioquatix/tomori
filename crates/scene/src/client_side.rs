// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

//! An environment's client side: what a client presents of an environment beyond its scenery, from what the
//! environment's server side shares over the state channel. A headset build registers one for an environment beside its
//! scenery; the frame step makes one for each space bound to the environment, while it draws the environment's scenery,
//! and asks it each frame what it shows, what the ray points at, what to ask the server side and what it sounds. Only the
//! environment's own crate knows what any of it means. See `design/environments.md`.
use std::any::Any;
use std::sync::Arc;
use std::time::Instant;

use glam::Vec3;
use tomori_protocol::Payload;

use crate::channel::Update;
use crate::pointing::Ray;

/// A sound an environment makes at a place in the scene, as the mixer plays it.
pub use tomori_client::playback::Sound;

/// What a client side shows this frame, for its scenery to draw: the environment's own, which only its own scenery reads.
pub type Drawn = Arc<dyn Any + Send + Sync>;

/// What a client side is told each frame.
#[non_exhaustive]
pub struct Input<'a> {
    /// What the server side sent since the last frame, in order.
    pub updates: Vec<Update>,
    pub now: Instant,
    /// What the participant points at in the environment, by the identity [`ClientSide::pick`] gave it, and where their
    /// head is, if they point at anything.
    pub pointed: Option<&'a str>,
    pub head: Option<Vec3>,
}

impl<'a> Input<'a> {
    pub fn new(updates: Vec<Update>, now: Instant) -> Self {
        Self {
            updates,
            now,
            pointed: None,
            head: None,
        }
    }

    /// This input, pointing at `pointed` from `head`.
    pub fn pointing(mut self, pointed: Option<&'a str>, head: Option<Vec3>) -> Self {
        self.pointed = pointed;
        self.head = head;
        self
    }
}

/// What the ray meets first of what an environment shows: its identity, which a participant attends to as
/// `Resource::object` and the environment's server side describes to the agent; a short name for it, as the recording
/// indicator shows it, such as an animal's name; and how far along the ray it is.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub struct Picked {
    pub object: String,
    pub label: String,
    pub distance: f32,
}

impl Picked {
    pub fn new(object: impl Into<String>, label: impl Into<String>, distance: f32) -> Self {
        Self {
            object: object.into(),
            label: label.into(),
            distance,
        }
    }
}

/// What a client side does each frame: the intents to send its server side, the sounds it makes, and what its scenery
/// draws.
#[derive(Default)]
#[non_exhaustive]
pub struct Output {
    pub intents: Vec<Payload>,
    pub sounds: Vec<(Sound, [f32; 3])>,
    pub drawn: Option<Drawn>,
}

impl Output {
    pub fn new() -> Self {
        Self::default()
    }

    /// This output, asking the server side `intent` too.
    pub fn with_intent(mut self, intent: Payload) -> Self {
        self.intents.push(intent);
        self
    }

    /// This output, making `sound` at `position` too.
    pub fn with_sound(mut self, sound: Sound, position: [f32; 3]) -> Self {
        self.sounds.push((sound, position));
        self
    }

    /// This output, drawing `drawn`.
    pub fn drawing(mut self, drawn: Drawn) -> Self {
        self.drawn = Some(drawn);
        self
    }
}

/// An environment's client side, for one space.
pub trait ClientSide: Send {
    /// What `ray` meets first of what the environment shows, if anything.
    fn pick(&self, ray: &Ray) -> Option<Picked>;

    /// A frame: take what the server side sent, move on to `input.now`, note what the participant points at, and say
    /// what to do.
    fn update(&mut self, input: Input<'_>) -> Output;
}

/// Makes an environment's client side for a space, given the space's identifier.
pub type Factory = Arc<dyn Fn(&str) -> Box<dyn ClientSide> + Send + Sync>;
