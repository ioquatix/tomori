// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

//! How a client shows the agent, Tomori's coordinator: an [`Avatar`], the one the joined space's backend names where
//! this client's build registers it, and otherwise the swarm, [`SwarmAvatar`], which is built in as [`SWARM`]. A build
//! registers an avatar beside its sceneries, with a scenery of its own that draws what [`Avatar::drawn`] shows; the
//! frame step makes one for the joined space and asks it, through a [`Coordinator`], where the agent's voice comes from
//! and where workers fly from and return to. See `design/avatars.md`.
use std::sync::Arc;

use glam::Vec3;

use crate::{
    anchors::Anchor, arrival_motion::Sphere, client_side::Drawn, interaction::Visual, swarm::Swarm,
};

/// The built-in avatar, the swarm, which a space shows when its backend names none, or names one this client's build
/// lacks.
pub const SWARM: &str = "swarm";

/// The radius of the sphere about the agent anchor that the swarm's rings and halo stand within, in metres.
pub const SWARM_RADIUS: f32 = 1.2;

/// How near whoever it stands for, in metres, about the vertical, the agent draws in.
pub const NEAR: f32 = 2.0;

/// How large the swarm draws in to, near whoever it stands for, as a share of its size: about 0.7 m across where it
/// reaches 1.35 m, as a flock gathering to listen.
pub const DRAWN: f32 = 0.52;

/// What an avatar is told each frame.
#[derive(Clone, Copy, Debug, PartialEq)]
#[non_exhaustive]
pub struct Input {
    /// The agent's state, as the frame step finds it this frame, before the swarm smooths its level: its turn's mode,
    /// audio level, whether it works, and the time.
    pub visual: Visual,
    /// Agent speech is playing on this device.
    pub speaking: bool,
    /// Where the agent stands and which way it faces: where it went, or was called, or else its home, the joined
    /// space's agent anchor, or the default. It may change at once; the coordinator glides an avatar's anchor there, by
    /// `via` or streaming, so that it never jumps, unless it is `appearing`. See `crate::glide`.
    pub anchor: Anchor,
    /// Whether the agent appears at `anchor` at once this frame, rather than moving there, as when a space opens with
    /// it at its home, or the space changes environment. See `design/environments.md`.
    pub appearing: bool,
    /// Whether the swarm's agents fly to `anchor` from where they are this frame, as this client's participant joins or
    /// leaves a space, though the agent's place appears there at once where `appearing` says: a flight that only this
    /// client shows, which never moves the agent for anyone else. See `design/arrival.md`.
    pub flying: bool,
    /// The participant's head, to face them, where it is known.
    pub head: Option<Vec3>,
    /// A point the agent is acting on or a participant is pointing at, if any. The frame step gives none yet: where it
    /// comes from is an open question in `design/avatars.md`.
    pub attention: Option<Vec3>,
    /// The point the trigger pulls towards, while it attracts, which the swarm is attracted to and a model may turn to.
    pub attractor: Option<Vec3>,
    /// The point the agent passes through on its way to `anchor`, where the server gives one.
    pub via: Option<Vec3>,
    /// Whether the agent streams to `anchor` as a flock, faster than it glides, as far away.
    pub stream: bool,
    /// How far the agent stands from whoever it stands for, about the vertical, while it is away, where it is known: an
    /// avatar draws in within [`NEAR`] of them, as the swarm does to [`DRAWN`] of its size, so that it never fills
    /// their view, and spreads out again at home.
    pub near: Option<f32>,
    /// The seconds since the last frame.
    pub dt: f32,
}

impl Input {
    /// A frame `dt` seconds after the last, showing `visual` at `anchor`, with nothing else known.
    pub fn new(visual: Visual, anchor: Anchor, dt: f32) -> Self {
        Self {
            visual,
            speaking: false,
            anchor,
            appearing: false,
            flying: false,
            head: None,
            attention: None,
            attractor: None,
            via: None,
            stream: false,
            near: None,
            dt,
        }
    }

    /// This input, standing `near` from whoever it stands for, or not away.
    pub fn near(mut self, near: Option<f32>) -> Self {
        self.near = near;
        self
    }

    /// This input, going to its anchor by `via`, and streaming there where `stream` says.
    pub fn going(mut self, via: Option<Vec3>, stream: bool) -> Self {
        self.via = via;
        self.stream = stream;
        self
    }

    /// This input, with the agent appearing at its anchor at once, or not.
    pub fn appearing(mut self, appearing: bool) -> Self {
        self.appearing = appearing;
        self
    }

    /// This input, with the swarm's agents flying to its anchor from where they are, or not.
    pub fn flying(mut self, flying: bool) -> Self {
        self.flying = flying;
        self
    }

    /// This input, with agent speech playing or not.
    pub fn speaking(mut self, speaking: bool) -> Self {
        self.speaking = speaking;
        self
    }

    /// This input, with the participant's head at `head`.
    pub fn head(mut self, head: Option<Vec3>) -> Self {
        self.head = head;
        self
    }

    /// This input, attending to `attention`.
    pub fn attention(mut self, attention: Option<Vec3>) -> Self {
        self.attention = attention;
        self
    }

    /// This input, with the trigger pulling towards `attractor`.
    pub fn attracted(mut self, attractor: Option<Vec3>) -> Self {
        self.attractor = attractor;
        self
    }
}

/// The agent as a client shows it in one space.
pub trait Avatar: Send {
    /// A frame: move on as `input` says.
    fn update(&mut self, input: &Input);

    /// Append the points the agent's voice comes from now, one or several; it sounds from their middle. Points that are
    /// not finite or are beyond [`REACH`] on an axis are left out; with none, it sounds from the agent anchor.
    fn voices(&self, voices: &mut Vec<[f32; 3]>);

    /// Append the points a worker's flock flies out from as it arrives, and back to as it leaves. Points that are not
    /// finite or are beyond [`REACH`] on an axis are left out; with none, workers fly from the agent anchor.
    fn departures(&self, departures: &mut Vec<[f32; 3]>);

    /// A sphere it stands within now. Nothing uses it yet; see `design/avatars.md`.
    fn bounds(&self) -> Sphere;

    /// What it shows this frame, for its scenery to draw through `Scenery::animate`: its own, which only its own
    /// scenery reads, such as where its model stands and its pose. None draws nothing new.
    fn drawn(&self) -> Option<Drawn> {
        None
    }
}

/// Makes an avatar for a space, given the space's identifier.
pub type Factory = Arc<dyn Fn(&str) -> Box<dyn Avatar> + Send + Sync>;

/// How far from the space's middle, on each axis, in metres, an avatar's voice and departure points may be, as a
/// worker placement's places may: [`crate::anchors::PLACEMENT_REACH`].
pub const REACH: f32 = crate::anchors::PLACEMENT_REACH;

/// The swarm, Tomori's built-in avatar: 128 agents that take a shape for each of the agent's states, glide to the
/// anchor, and are attracted to the trigger's point, exactly as [`Swarm`] does. The renderer draws it itself.
#[derive(Default)]
pub struct SwarmAvatar {
    swarm: Swarm,
}

impl SwarmAvatar {
    /// The swarm, for the renderer to draw.
    pub fn swarm(&self) -> &Swarm {
        &self.swarm
    }
}

impl Avatar for SwarmAvatar {
    fn update(&mut self, input: &Input) {
        self.swarm.set_visual(input.visual, input.dt);
        let size = if input.near.is_some_and(|near| near <= NEAR) {
            DRAWN
        } else {
            1.0
        };
        self.swarm.draw_in(size, input.dt);
        if input.flying {
            self.swarm.fly_to(input.anchor);
        } else if input.appearing {
            self.swarm.appear_at(input.anchor);
        } else {
            self.swarm.set_anchor(input.anchor);
        }
        self.swarm.step(input.dt, input.attractor);
    }

    /// Its agents: it speaks from their middle.
    fn voices(&self, voices: &mut Vec<[f32; 3]>) {
        voices.extend(
            self.swarm
                .agents
                .iter()
                .map(|agent| agent.position.to_array()),
        );
    }

    /// Its agents: workers fly out from among them and back.
    fn departures(&self, departures: &mut Vec<[f32; 3]>) {
        self.voices(departures);
    }

    fn bounds(&self) -> Sphere {
        Sphere {
            centre: self.swarm.anchor().position,
            radius: SWARM_RADIUS,
        }
    }
}

/// The avatar a client is to show in the joined space, as the conversation chooses it: one its build registers, made
/// for the space by `factory`.
#[derive(Clone, Copy)]
pub struct Choice<'a> {
    pub space: &'a str,
    pub avatar: &'a str,
    pub factory: &'a Factory,
    /// The avatars whose scenery failed in the renderer, which this client shows as the swarm from then on.
    pub failures: &'a Failures,
}

/// The registered avatars whose scenery panicked in a renderer, as it was made or given what its avatar shows, by
/// identifier: shared between a build's renderer, which reports them, and its conversation, whose coordinator then
/// drops that avatar for the swarm as for a panic of its own. See `design/avatars.md`.
#[derive(Clone, Debug, Default)]
pub struct Failures(Arc<std::sync::Mutex<Vec<String>>>);

impl Failures {
    /// The scenery of the avatar `avatar` panicked.
    pub fn report(&self, avatar: &str) {
        let mut failed = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if !failed.iter().any(|failed| failed == avatar) {
            failed.push(avatar.to_owned());
        }
    }

    /// Whether the scenery of the avatar `avatar` has panicked.
    pub fn contains(&self, avatar: &str) -> bool {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .iter()
            .any(|failed| failed == avatar)
    }
}

/// A registered avatar shown now, by its identifier, the space it was made for, and what it shows, for the renderer to
/// draw with its scenery, which it makes anew for each avatar and space.
#[derive(Clone)]
pub struct Shown<'a> {
    pub avatar: &'a str,
    pub space: &'a str,
    pub drawn: Option<Drawn>,
}

/// An avatar made for a space.
struct Made {
    space: String,
    avatar: String,
    made: Box<dyn Avatar>,
}

/// The agent as a client shows it: the built-in swarm, which never stops, so that it never jumps, and the avatar made
/// for the joined space, if it shows another; with where the agent's voice comes from and where workers fly from and
/// return to, from whichever shows. A registered avatar is the build's own code: one that panics, as it is made or in
/// any of its methods, is dropped, the swarm shows in its place, and it is never made again by this coordinator. See
/// `design/avatars.md`.
pub struct Coordinator {
    swarm: SwarmAvatar,
    made: Option<Made>,
    /// The registered avatars that have panicked, by identifier, which are never made again.
    poisoned: Vec<String>,
    /// No agents, for the renderer while another avatar shows, at the anchor, as the ring on the floor beneath it is.
    still: Swarm,
    voices: Vec<[f32; 3]>,
    departures: Vec<[f32; 3]>,
    /// Where the agent's place is as it glides to where it is to stand. See `crate::glide`.
    glide: crate::glide::Glide,
    /// What the avatar shown showed and the sphere it stood within, as of the last frame.
    drawn: Option<Drawn>,
    bounds: Sphere,
}

impl Default for Coordinator {
    fn default() -> Self {
        let mut still = Swarm::default();
        still.agents.clear();
        let swarm = SwarmAvatar::default();
        Self {
            bounds: swarm.bounds(),
            swarm,
            made: None,
            poisoned: Vec::new(),
            still,
            voices: Vec::with_capacity(crate::swarm::AGENT_COUNT),
            departures: Vec::with_capacity(crate::swarm::AGENT_COUNT),
            glide: crate::glide::Glide::default(),
            drawn: None,
        }
    }
}

impl Coordinator {
    /// Show `choice` from now on, making it for its space unless it is shown already, or the swarm with none, or with
    /// one that has panicked, here or in its scenery.
    pub fn choose(&mut self, choice: Option<Choice<'_>>) {
        let Some(choice) =
            choice.filter(|choice| !self.poisoned.iter().any(|id| id == choice.avatar))
        else {
            self.made = None;
            return;
        };
        if choice.failures.contains(choice.avatar) {
            self.made = None;
            self.give_up(choice.avatar);
            return;
        }
        let current = self
            .made
            .as_ref()
            .is_some_and(|made| made.space == choice.space && made.avatar == choice.avatar);
        if current {
            return;
        }
        let made = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            (choice.factory)(choice.space)
        }));
        self.made = match made {
            Ok(made) => Some(Made {
                space: choice.space.to_owned(),
                avatar: choice.avatar.to_owned(),
                made,
            }),
            Err(_) => {
                self.give_up(choice.avatar);
                None
            }
        };
    }

    /// Never make the avatar `avatar` again, saying so once.
    fn give_up(&mut self, avatar: &str) {
        log::warn!(
            "The avatar {avatar:?} panicked; the swarm shows the agent in its place from now on."
        );
        self.poisoned.push(avatar.to_owned());
    }

    /// A frame: the swarm and the avatar shown move on as `input` says, and say where the agent is heard from and
    /// workers fly from.
    pub fn update(&mut self, input: &Input) {
        // Every avatar is given its place as it glides there, never where it is to stand at once.
        let anchor = self.glide.step(
            input.anchor,
            input.via,
            input.stream,
            input.appearing,
            input.dt,
        );
        let input = &Input { anchor, ..*input };
        // Unseen, the swarm is not attracted by the trigger, so that it glides to the anchor, to show there again.
        let unseen = Input {
            attractor: None,
            ..*input
        };
        self.swarm
            .update(if self.made.is_some() { &unseen } else { input });
        self.still.set_anchor(input.anchor);
        self.voices.clear();
        self.departures.clear();
        if let Some(made) = &mut self.made {
            let (voices, departures) = (&mut self.voices, &mut self.departures);
            let asked = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                made.made.update(input);
                made.made.voices(voices);
                made.made.departures(departures);
                (made.made.bounds(), made.made.drawn())
            }));
            match asked {
                Ok((bounds, drawn)) => {
                    self.bounds = bounds;
                    self.drawn = drawn;
                }
                Err(_) => {
                    let avatar = self.made.take().map(|made| made.avatar).unwrap_or_default();
                    self.give_up(&avatar);
                    self.voices.clear();
                    self.departures.clear();
                }
            }
        }
        if self.made.is_none() {
            self.swarm.voices(&mut self.voices);
            self.swarm.departures(&mut self.departures);
            self.bounds = self.swarm.bounds();
            self.drawn = None;
        }
        // Only points within reach; with none, the agent anchor.
        for points in [&mut self.voices, &mut self.departures] {
            points.retain(|point| {
                let point = Vec3::from_array(*point);
                crate::anchors::within(point, REACH)
            });
            if points.is_empty() {
                points.push(input.anchor.position.to_array());
            }
        }
    }

    /// Carry the swarm's agents as what stood at `from` is carried to `to`, as when the participant's room is moved in
    /// the scene under them as they leave a space, so that they stay where the participant saw them. See
    /// [`Swarm::carry`].
    pub fn carry(&mut self, from: Anchor, to: Anchor) {
        self.swarm.swarm.carry(from, to);
    }

    /// The swarm for the renderer to draw: the built-in swarm, or none, at the anchor, while another avatar shows.
    pub fn swarm(&self) -> &Swarm {
        match &self.made {
            Some(_) => &self.still,
            None => self.swarm.swarm(),
        }
    }

    /// The agent's state as the built-in swarm shows it, its level smoothed, whichever avatar shows.
    pub fn visual(&self) -> Visual {
        self.swarm.swarm.visual
    }

    /// The registered avatar shown now, if the swarm is not, and what it showed as of the last frame.
    pub fn shown(&self) -> Option<Shown<'_>> {
        self.made.as_ref().map(|made| Shown {
            avatar: &made.avatar,
            space: &made.space,
            drawn: self.drawn.clone(),
        })
    }

    /// Where the agent's voice comes from, as of the last frame: the middle of the avatar's voices.
    pub fn voice(&self) -> [f32; 3] {
        crate::spatial_audio::centre(&self.voices)
    }

    /// Where workers fly from and return to, as of the last frame.
    pub fn departures(&self) -> &[[f32; 3]] {
        &self.departures
    }

    /// The sphere the avatar shown stood within, as of the last frame.
    pub fn bounds(&self) -> Sphere {
        self.bounds
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_swarm_draws_in_within_reach_of_whoever_it_stands_for_and_spreads_out_again() {
        let mut swarm = SwarmAvatar::default();
        let frames = |swarm: &mut SwarmAvatar, near: Option<f32>| {
            for _ in 0..360 {
                swarm.update(&Input::new(Visual::default(), Anchor::AGENT, 1.0 / 72.0).near(near));
            }
            swarm.swarm.size()
        };
        assert!((frames(&mut swarm, Some(1.5)) - DRAWN).abs() < 1e-3);
        assert!((frames(&mut swarm, Some(NEAR + 0.5)) - 1.0).abs() < 1e-3);
        assert!((frames(&mut swarm, Some(NEAR)) - DRAWN).abs() < 1e-3);
        assert!((frames(&mut swarm, None) - 1.0).abs() < 1e-3);
    }
    use crate::interaction::Mode;
    use std::sync::Mutex;

    /// The frames of a session: the agent in each state in turn, the trigger attracting for a while, and the anchor
    /// moving and turning.
    fn frames() -> Vec<Input> {
        let moved = Anchor {
            position: Vec3::new(3.0, 1.8, -4.0),
            orientation: glam::Quat::from_rotation_y(1.0),
        };
        let mut frames = Vec::new();
        for (index, mode) in [
            Mode::Neutral,
            Mode::Working,
            Mode::Speaking,
            Mode::Ready,
            Mode::Listening,
            Mode::Waiting,
        ]
        .into_iter()
        .enumerate()
        {
            for frame in 0..120 {
                let visual = Visual {
                    mode,
                    level: (frame % 7) as f32 * 0.1,
                    working: index % 2 == 0,
                    ..Default::default()
                };
                let anchor = if index >= 3 { moved } else { Anchor::AGENT };
                let attractor = (index == 1 && frame < 60).then_some(Vec3::new(1.0, 1.2, -1.0));
                frames.push(
                    Input::new(visual, anchor, 1.0 / 72.0 + (frame % 3) as f32 * 0.004)
                        .attracted(attractor),
                );
            }
        }
        frames
    }

    #[test]
    fn the_swarm_avatar_is_the_swarm_exactly() {
        let mut avatar = SwarmAvatar::default();
        let mut swarm = Swarm::default();
        for input in frames() {
            avatar.update(&input);
            swarm.set_visual(input.visual, input.dt);
            swarm.set_anchor(input.anchor);
            swarm.step(input.dt, input.attractor);
            let drawn = avatar.swarm();
            assert_eq!(drawn.visual, swarm.visual);
            assert_eq!(drawn.anchor(), swarm.anchor());
            for (a, b) in drawn.agents.iter().zip(&swarm.agents) {
                assert_eq!(a.position, b.position);
                assert_eq!(a.velocity, b.velocity);
            }
        }
        // It speaks from its agents, and workers fly from them.
        let agents: Vec<[f32; 3]> = swarm
            .agents
            .iter()
            .map(|agent| agent.position.to_array())
            .collect();
        let mut voices = Vec::new();
        avatar.voices(&mut voices);
        assert_eq!(voices, agents);
        let mut departures = Vec::new();
        avatar.departures(&mut departures);
        assert_eq!(departures, agents);
        assert_eq!(
            avatar.bounds(),
            Sphere {
                centre: swarm.anchor().position,
                radius: SWARM_RADIUS
            }
        );
        // It draws nothing of its own.
        assert!(avatar.drawn().is_none());
    }

    #[test]
    fn the_coordinator_shows_the_swarm_exactly_by_default() {
        let mut coordinator = Coordinator::default();
        let mut swarm = Swarm::default();
        // Its place glides where it is to stand, as every avatar's does.
        let mut glide = crate::glide::Glide::default();
        assert!(coordinator.shown().is_none());
        for input in frames() {
            coordinator.choose(None);
            coordinator.update(&input);
            swarm.set_visual(input.visual, input.dt);
            swarm.set_anchor(glide.step(
                input.anchor,
                input.via,
                input.stream,
                input.appearing,
                input.dt,
            ));
            swarm.step(input.dt, input.attractor);
            let agents: Vec<[f32; 3]> = swarm
                .agents
                .iter()
                .map(|agent| agent.position.to_array())
                .collect();
            // Heard from the middle of its agents, and workers fly from them, as before.
            assert_eq!(coordinator.voice(), crate::spatial_audio::centre(&agents));
            assert_eq!(coordinator.departures(), agents.as_slice());
            assert_eq!(coordinator.visual(), swarm.visual);
            assert_eq!(coordinator.swarm().anchor(), swarm.anchor());
            assert_eq!(
                coordinator.swarm().agents[0].position,
                swarm.agents[0].position
            );
        }
        assert_eq!(coordinator.bounds().centre, swarm.anchor().position);
        assert!(coordinator.shown().is_none());
    }

    /// A model of an avatar: it stands at its anchor, speaks from a mouth above it, sends workers from its hands and
    /// remembers what it was told, and what it was made for.
    struct Figure {
        space: String,
        told: Arc<Mutex<Vec<Input>>>,
        anchor: Anchor,
    }

    impl Avatar for Figure {
        fn update(&mut self, input: &Input) {
            self.anchor = input.anchor;
            self.told.lock().unwrap().push(*input);
        }

        fn voices(&self, voices: &mut Vec<[f32; 3]>) {
            voices.push(self.anchor.place(Vec3::new(0.0, 0.3, 0.0)).to_array());
        }

        fn departures(&self, departures: &mut Vec<[f32; 3]>) {
            departures.push(self.anchor.place(Vec3::new(-0.4, 0.0, 0.0)).to_array());
            departures.push(self.anchor.place(Vec3::new(0.4, 0.0, 0.0)).to_array());
        }

        fn bounds(&self) -> Sphere {
            Sphere {
                centre: self.anchor.position,
                radius: 0.5,
            }
        }

        fn drawn(&self) -> Option<Drawn> {
            Some(Arc::new(self.space.clone()))
        }
    }

    /// An avatar that shows nothing and says nothing of its own: the defaults.
    struct Plain;

    impl Avatar for Plain {
        fn update(&mut self, _input: &Input) {}

        fn voices(&self, _voices: &mut Vec<[f32; 3]>) {}

        fn departures(&self, _departures: &mut Vec<[f32; 3]>) {}

        fn bounds(&self) -> Sphere {
            Sphere {
                centre: Vec3::ZERO,
                radius: 0.0,
            }
        }
    }

    #[test]
    fn the_coordinator_makes_a_chosen_avatar_once_for_its_space_and_asks_it() {
        let told = Arc::new(Mutex::new(Vec::new()));
        let made = Arc::new(Mutex::new(Vec::new()));
        let factory: Factory = {
            let (told, made) = (told.clone(), made.clone());
            Arc::new(move |space: &str| {
                made.lock().unwrap().push(space.to_owned());
                Box::new(Figure {
                    space: space.to_owned(),
                    told: told.clone(),
                    anchor: Anchor::AGENT,
                }) as Box<dyn Avatar>
            })
        };
        let failures = Failures::default();
        let choice = |space| {
            Some(Choice {
                space,
                avatar: "figure",
                factory: &factory,
                failures: &failures,
            })
        };
        let anchor = Anchor {
            position: Vec3::new(2.0, 0.0, -3.0),
            orientation: glam::Quat::IDENTITY,
        };
        let input = Input::new(
            Visual {
                mode: Mode::Speaking,
                ..Default::default()
            },
            anchor,
            0.01,
        )
        .speaking(true)
        .head(Some(Vec3::new(0.0, 1.6, 0.0)))
        .attention(Some(Vec3::X))
        .attracted(Some(Vec3::Y));
        let mut coordinator = Coordinator::default();
        for _ in 0..3 {
            coordinator.choose(choice("space-1"));
            coordinator.update(&input);
        }
        // Made once, for its space, and told every frame what the frame step knows.
        assert_eq!(*made.lock().unwrap(), ["space-1"]);
        assert_eq!(*told.lock().unwrap(), [input; 3]);
        // The agent is heard from its mouth, and workers fly from its hands.
        assert_eq!(coordinator.voice(), [2.0, 0.3, -3.0]);
        assert_eq!(
            coordinator.departures(),
            [[1.6, 0.0, -3.0], [2.4, 0.0, -3.0]]
        );
        assert_eq!(coordinator.bounds().radius, 0.5);
        // No swarm is drawn, but where it would stand is the anchor, for the ring on the floor beneath it.
        assert!(coordinator.swarm().agents.is_empty());
        assert_eq!(coordinator.swarm().anchor(), anchor);
        let shown = coordinator.shown().expect("the figure shows");
        assert_eq!(shown.avatar, "figure");
        let drawn = shown.drawn.expect("it shows something");
        assert_eq!(drawn.downcast_ref::<String>().unwrap(), "space-1");
        // The swarm moves on underneath, gliding to the anchor, as the agent's state says.
        assert_eq!(coordinator.visual().mode, Mode::Speaking);
        // In another space, it begins anew.
        coordinator.choose(choice("space-2"));
        assert_eq!(*made.lock().unwrap(), ["space-1", "space-2"]);
        // Dropped, the swarm shows again where it has glided meanwhile, at the anchor: unseen, the trigger, still pulled,
        // did not attract it.
        for _ in 0..1000 {
            coordinator.update(&input);
        }
        coordinator.choose(None);
        coordinator.update(&input);
        assert!(coordinator.shown().is_none());
        let middle = coordinator
            .swarm()
            .agents
            .iter()
            .map(|agent| agent.position)
            .sum::<Vec3>()
            / crate::swarm::AGENT_COUNT as f32;
        assert!(middle.distance(anchor.position) < 0.6, "{middle}");
        assert_eq!(coordinator.departures().len(), crate::swarm::AGENT_COUNT);
    }

    #[test]
    fn an_avatar_that_says_nowhere_is_heard_and_sends_workers_from_its_anchor() {
        let factory: Factory = Arc::new(|_: &str| Box::new(Plain) as Box<dyn Avatar>);
        let anchor = Anchor {
            position: Vec3::new(4.0, 0.5, -6.0),
            orientation: glam::Quat::IDENTITY,
        };
        let mut coordinator = Coordinator::default();
        coordinator.choose(Some(Choice {
            space: "space-1",
            avatar: "plain",
            factory: &factory,
            failures: &Failures::default(),
        }));
        coordinator.update(&Input::new(Visual::default(), anchor, 0.01));
        assert!(coordinator.shown().unwrap().drawn.is_none());
        // With no voice or departure of its own, the agent is heard, and workers fly, from where it stands.
        assert_eq!(coordinator.voice(), anchor.position.to_array());
        assert_eq!(coordinator.departures(), [anchor.position.to_array()]);
        assert_eq!(coordinator.bounds().radius, 0.0);
    }

    /// An avatar whose points are partly nonsense: one beyond reach, one not finite, and one where it stands.
    struct Stray;

    impl Avatar for Stray {
        fn update(&mut self, _input: &Input) {}

        fn voices(&self, voices: &mut Vec<[f32; 3]>) {
            voices.extend([[0.0, 1.0, -2.0], [0.0, 0.0, -(REACH + 1.0)], [f32::NAN; 3]]);
        }

        fn departures(&self, departures: &mut Vec<[f32; 3]>) {
            departures.extend([[f32::INFINITY, 0.0, 0.0], [REACH + 1.0, 0.0, 0.0]]);
        }

        fn bounds(&self) -> Sphere {
            Sphere {
                centre: Vec3::ZERO,
                radius: 1.0,
            }
        }
    }

    #[test]
    fn points_out_of_reach_or_not_finite_are_left_out() {
        let factory: Factory = Arc::new(|_: &str| Box::new(Stray) as Box<dyn Avatar>);
        let mut coordinator = Coordinator::default();
        coordinator.choose(Some(Choice {
            space: "space-1",
            avatar: "stray",
            factory: &factory,
            failures: &Failures::default(),
        }));
        coordinator.update(&Input::new(Visual::default(), Anchor::AGENT, 0.01));
        assert_eq!(coordinator.voice(), [0.0, 1.0, -2.0]);
        // None left, workers fly from the anchor.
        assert_eq!(
            coordinator.departures(),
            [Anchor::AGENT.position.to_array()]
        );
    }

    /// An avatar that panics in the method it names.
    struct Faulty(&'static str);

    impl Faulty {
        fn maybe(&self, method: &str) {
            assert_ne!(self.0, method, "the avatar's {method} fails");
        }
    }

    impl Avatar for Faulty {
        fn update(&mut self, _input: &Input) {
            self.maybe("update");
        }

        fn voices(&self, _voices: &mut Vec<[f32; 3]>) {
            self.maybe("voices");
        }

        fn departures(&self, _departures: &mut Vec<[f32; 3]>) {
            self.maybe("departures");
        }

        fn bounds(&self) -> Sphere {
            self.maybe("bounds");
            Sphere {
                centre: Vec3::ZERO,
                radius: 0.25,
            }
        }

        fn drawn(&self) -> Option<Drawn> {
            self.maybe("drawn");
            None
        }
    }

    #[test]
    fn an_avatar_that_panics_is_dropped_for_the_swarm_and_never_made_again() {
        crate::test_support::log_everything();
        let input = Input::new(Visual::default(), Anchor::AGENT, 0.01);
        for method in [
            "factory",
            "update",
            "voices",
            "departures",
            "bounds",
            "drawn",
        ] {
            let made = Arc::new(std::sync::atomic::AtomicUsize::new(0));
            let factory: Factory = {
                let made = made.clone();
                Arc::new(move |_: &str| {
                    made.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    assert_ne!(method, "factory", "the avatar cannot be made");
                    Box::new(Faulty(method)) as Box<dyn Avatar>
                })
            };
            let failures = Failures::default();
            let choice = |space| {
                Some(Choice {
                    space,
                    avatar: "faulty",
                    factory: &factory,
                    failures: &failures,
                })
            };
            let mut coordinator = Coordinator::default();
            for space in ["space-1", "space-1", "space-2"] {
                coordinator.choose(choice(space));
                coordinator.update(&input);
                // The swarm shows in its place, heard from its agents, workers flying from them.
                assert!(coordinator.shown().is_none(), "{method}");
                assert_eq!(coordinator.swarm().agents.len(), crate::swarm::AGENT_COUNT);
                assert_eq!(coordinator.departures().len(), crate::swarm::AGENT_COUNT);
                assert_eq!(coordinator.bounds().radius, SWARM_RADIUS);
            }
            // Made once, and never again, even for another space.
            assert_eq!(
                made.load(std::sync::atomic::Ordering::Relaxed),
                1,
                "{method}"
            );
        }
        // One that does not panic still works beside it.
        let figure: Factory = Arc::new(|_: &str| Box::new(Faulty("none")) as Box<dyn Avatar>);
        let mut coordinator = Coordinator::default();
        coordinator.choose(Some(Choice {
            space: "space-1",
            avatar: "figure",
            factory: &figure,
            failures: &Failures::default(),
        }));
        coordinator.update(&input);
        assert_eq!(coordinator.bounds().radius, 0.25);
        assert!(coordinator.shown().is_some());
    }

    #[test]
    fn an_avatar_whose_scenery_failed_is_dropped_for_the_swarm_and_never_made_again() {
        let made = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let factory: Factory = {
            let made = made.clone();
            Arc::new(move |_: &str| {
                made.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                Box::new(Faulty("none")) as Box<dyn Avatar>
            })
        };
        let failures = Failures::default();
        let choice = |space| {
            Some(Choice {
                space,
                avatar: "figure",
                factory: &factory,
                failures: &failures,
            })
        };
        let input = Input::new(Visual::default(), Anchor::AGENT, 0.01);
        let mut coordinator = Coordinator::default();
        coordinator.choose(choice("space-1"));
        coordinator.update(&input);
        assert!(coordinator.shown().is_some());
        // The renderer reports its scenery failed, once or more: from the next frame, the swarm shows and is heard.
        failures.report("figure");
        failures.report("figure");
        assert!(failures.contains("figure"));
        assert!(!failures.contains("fox"));
        for space in ["space-1", "space-2"] {
            coordinator.choose(choice(space));
            coordinator.update(&input);
            assert!(coordinator.shown().is_none());
            assert_eq!(coordinator.swarm().agents.len(), crate::swarm::AGENT_COUNT);
            assert_eq!(coordinator.departures().len(), crate::swarm::AGENT_COUNT);
        }
        assert_eq!(made.load(std::sync::atomic::Ordering::Relaxed), 1);
    }

    #[test]
    fn the_swarm_appears_at_its_anchor_when_told_and_otherwise_glides() {
        let home = Anchor {
            position: Vec3::new(-1.0, 1.6, -5.0),
            orientation: glam::Quat::IDENTITY,
        };
        let mut appearing = SwarmAvatar::default();
        let mut gliding = SwarmAvatar::default();
        let input = Input::new(Visual::default(), home, 1.0 / 72.0);
        appearing.update(&input.appearing(true));
        gliding.update(&input);
        let middle = |avatar: &SwarmAvatar| {
            avatar
                .swarm()
                .agents
                .iter()
                .map(|agent| agent.position)
                .sum::<Vec3>()
                / crate::swarm::AGENT_COUNT as f32
        };
        // Carried there at once, rather than setting out.
        assert!(middle(&appearing).distance(home.position) < 1.0);
        assert!(middle(&gliding).distance(home.position) > 2.0);
        assert_eq!(appearing.swarm().anchor(), home);
    }
}
