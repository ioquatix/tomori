// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

//! The frame step that the headset and the desktop window share: the controller's buttons, arrival, the hand menu, the right wrist's transcript, pointing and choosing, the swarm, the space's workers and the shared panels, from plain input. Each platform's own behaviour is in [`headset`] and [`desktop`], which turn their devices' input into [`Buttons`] and [`Senses`]; the shells in `xr.rs` and `desktop_participant.rs` make only device calls. What a frame draws is [`Frame`], which the renderer applies. This is the platform-neutral core that `design/clients.md` asks for.
#[cfg(not(target_os = "android"))]
pub mod desktop;
pub mod headset;

use std::time::Instant;

use glam::{Mat4, Vec2, Vec3};

use crate::{
    arrival, arrival_motion,
    cards::CardNavigation,
    controller::Pose,
    controller_ui::{self, Indicator, Moment, Sheet, Wrist, WristFrame},
    conversation::Conversation,
    delegation,
    hand_menu::{self, HandMenu, Palm},
    hands::Hand,
    interaction,
    locomotion::{self, Rig, Travel},
    panel_anchor::PanelAnchor,
    pointing::{Pointer, Ray},
    recording::Trackers,
    swarm::Swarm,
};
use tomori_client::feedback::{self, Feedback};

/// How far a stick must move before it counts.
const STICK: f32 = 0.6;
/// How far the right trigger is pulled before the right palm counts as pointing, which keeps the transcript from showing.
const POINTING: f32 = 0.2;

/// One frame of the controller's buttons: presses since the last frame, and what is held.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Buttons {
    /// A was pressed.
    pub a: bool,
    /// B was pressed.
    pub back: bool,
    /// B is held.
    pub back_held: bool,
    /// Y was pressed.
    pub y: bool,
    /// X is held: push-to-talk.
    pub talk: bool,
    /// The left stick was clicked: mute spoken replies.
    pub mute: bool,
    /// The right stick was clicked: replay the latest reply.
    pub replay: bool,
    pub left_stick: Vec2,
    pub right_stick: Vec2,
}

/// How the attractor is drawn.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Marker {
    /// Always, brighter while it attracts: the right controller, while neither its model nor the right hand is drawn.
    Always,
    /// Only while it attracts, as the mouse's point.
    #[default]
    WhileAttracting,
    /// Never: the right controller's model or hand is drawn instead.
    Hidden,
}

/// Where things are this frame, in the scene, from each platform's own logic.
#[derive(Clone, Copy, Debug, Default)]
pub struct Senses {
    /// The participant has input: the session is focused, or the window is.
    pub focused: bool,
    /// The head's transform.
    pub head: Option<Mat4>,
    /// The head as a pose: where the participant hears from and is seen by the others.
    pub head_pose: Option<Pose>,
    /// The point that what the hands hold faces, which stays put as the head turns.
    pub viewer: Option<Vec3>,
    /// The head in the eyes' space, which the floating panel follows.
    pub space_head: Mat4,
    /// The pointing ray: the right controller's aim, or the mouse's.
    pub ray: Option<Ray>,
    /// The trigger, while it can act; see [`Pointer::update`].
    pub trigger: Option<f32>,
    /// The right hand points, which keeps the transcript from showing.
    pub pointing: bool,
    /// The left palm, which shows the hand menu while it faces up.
    pub left_palm: Option<Palm>,
    /// The right palm, which shows the transcript while it faces up.
    pub right_palm: Option<Palm>,
    /// The right palm is a tracked hand without a controller, so the transcript only reads.
    pub hands_only: bool,
    /// The right grip, which detaches the transcript.
    pub grip: Option<f32>,
    /// Where the recording indicator can stand.
    pub trackers: Trackers,
    /// Where the swarm is attracted to, while the trigger attracts.
    pub attractor: Option<Vec3>,
    pub marker: Marker,
    /// The controllers' poses, for the others to see.
    pub controllers: [Option<Pose>; 2],
    /// The tracked hands, for the others to see.
    pub hands: [Option<Hand>; 2],
    /// Cue on the palms that something waits, while their panels are closed.
    pub cues: bool,
    /// Draw the pointing ray.
    pub beam: bool,
}

/// Arrival's objects to draw, with what the ray meets and the ray.
#[derive(Clone, Debug, PartialEq)]
pub struct Arrival {
    pub scene: arrival::Scene,
    pub hovered: Option<arrival::Key>,
    pub ray: Option<crate::pointing::Beam>,
    pub time: f32,
}

/// This participant's head, controllers and hands, for the others in the space.
#[derive(Clone, Copy, Debug)]
pub struct Presence {
    pub head: Pose,
    pub left: Option<Pose>,
    pub right: Option<Pose>,
    pub hands: [Option<Hand>; 2],
}

impl Presence {
    /// Report it to the server through `connection`, so the others in the space can render it.
    pub fn send(&self, connection: &mut tomori_client::connection::Connection) {
        connection.send_presence(
            self.head.into(),
            self.left.map(Into::into),
            self.right.map(Into::into),
            self.hands,
        );
    }
}

/// The shared panels this frame.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Panels {
    /// The status pill, in the eyes' space.
    pub pill: Sheet,
    /// The recording indicator and the `LIVE` badge, in the scene.
    pub indicator: [Sheet; 2],
    /// The right wrist's transcript, in the scene.
    pub wrist: Sheet,
}

/// What one frame draws and reports.
pub struct Frame<'a> {
    pub scenery: arrival_motion::Scenery,
    pub swarm: &'a Swarm,
    /// Where the attractor is drawn.
    pub marker: Option<Vec3>,
    pub attracting: bool,
    /// The result card shown, and its page.
    pub card: usize,
    pub content: usize,
    pub cubes: &'a [delegation::Cube],
    pub stations: &'a [delegation::Station],
    pub workspace: &'a delegation::Workspace,
    /// Arrival's objects, while there are any to draw.
    pub arrival: Option<Arrival>,
    pub panels: Panels,
    /// Where this participant hears from, and where the agent is.
    pub listener: Option<Pose>,
    pub agent: [f32; 3],
    /// What to tell the others in the space; visitors send nothing.
    pub presence: Option<Presence>,
    /// The GitHub environment's inbox and cards, in the scene, while the space shows them.
    pub cards: Vec<controller_ui::Sheet>,
    /// Where a teleport being aimed would land, which is drawn as a ring on the surface; see `design/locomotion.md`.
    pub landing: Option<Vec3>,
    /// How dark the view is drawn, from 0 to 1, as it comes back after a teleport; see `design/locomotion.md`.
    pub blink: f32,
    /// What the environment's client side shows, for its scenery to draw. See `design/environments.md`.
    pub environment: Option<crate::client_side::Drawn>,
}

/// Where travel applies: the joined space, by its identifier, and its environment as drawn.
fn place(conversation: &Conversation) -> locomotion::Place<'_> {
    conversation
        .joined_space()
        .map(|(space, _)| (space.id.as_str(), conversation.environment()))
}

/// The scene arrival heads for: its objects outside a space, and inside one, none, with the space's environment to grow out of its object.
pub fn arrival_target(conversation: &Conversation) -> arrival::Scene {
    match conversation.arrival() {
        Some(view) => arrival::Scene::layout(&view),
        None => arrival::Scene::entered(conversation.entered(), conversation.environment()),
    }
}

/// Everything the frame step keeps between frames.
pub struct Step {
    stick_latched: bool,
    /// When B was pressed, while it is held: holding it leaves a space.
    back_since: Option<Instant>,
    was_talking: bool,
    /// After focus loss, X must be released before it records again, so that holding it through a system menu does not start a recording.
    talk_blocked: bool,
    /// The arrival object the ray met on the last frame, which A chooses.
    highlighted: Option<arrival::Key>,
    /// Whether the hand menu showed last frame, when A is read, so that A then chooses only in it.
    menu_was_shown: bool,
    hand_menu: HandMenu,
    wrist: Wrist,
    indicator: Indicator,
    pointer: Pointer<arrival::Key>,
    hover: feedback::Hover,
    swarm: Swarm,
    signals: interaction::Signals,
    motion: arrival_motion::Motion,
    arrival_time: f32,
    delegation_scene: delegation::Scene,
    workers: delegation::Workers,
    coordinator_samples: Vec<[f32; 3]>,
    presentation_navigation: CardNavigation,
    /// In a space, the panel stands in the world and follows lazily; see `design/interaction.md`.
    panel: PanelAnchor,
    /// The GitHub environment's cards: the one carried and the one put down; see `design/github.md`.
    board: crate::board::Board,
    /// Where the participant has travelled, in an environment that allows it, and the teleport being aimed; see `design/locomotion.md`.
    travel: Travel,
    /// The left stick, which travels in such a space, from the last buttons.
    travel_stick: Vec2,
    /// The client side of the joined space's environment, for that space and environment. See `design/environments.md`.
    client: Option<(
        String,
        crate::environment::Environment,
        Box<dyn crate::client_side::ClientSide>,
    )>,
}

impl Default for Step {
    fn default() -> Self {
        Self {
            stick_latched: false,
            back_since: None,
            was_talking: false,
            talk_blocked: false,
            highlighted: None,
            menu_was_shown: false,
            hand_menu: HandMenu::default(),
            wrist: Wrist::default(),
            indicator: Indicator::default(),
            pointer: Pointer::default(),
            hover: feedback::Hover::default(),
            swarm: Swarm::default(),
            signals: interaction::Signals::default(),
            motion: arrival_motion::Motion::default(),
            arrival_time: 0.0,
            delegation_scene: delegation::Scene::new(),
            workers: delegation::Workers::default(),
            coordinator_samples: Vec::with_capacity(crate::swarm::AGENT_COUNT),
            presentation_navigation: CardNavigation::default(),
            panel: PanelAnchor::default(),
            board: crate::board::Board::default(),
            travel: Travel::default(),
            travel_stick: Vec2::ZERO,
            client: None,
        }
    }
}

impl Step {
    /// Focus is lost: nothing held carries over, and an utterance being recorded is cancelled.
    pub fn blur(&mut self, conversation: &mut Conversation) {
        self.stick_latched = false;
        self.back_since = None;
        conversation.hold_back(None);
        self.was_talking = false;
        self.talk_blocked = true;
        conversation.stop_live();
        conversation.stop_talking();
        self.pointer.release();
        self.highlighted = None;
        self.presentation_navigation.release();
        self.cancel_travel();
    }

    /// Drop a teleport being aimed, as when the left stick is lost, without travelling.
    pub fn cancel_travel(&mut self) {
        self.travel.cancel();
        self.travel_stick = Vec2::ZERO;
    }

    /// Where the participant's room is in the space's scene, in the space `conversation` is in: the transform the frame's poses are given in, which each platform applies to its head, eyes, controllers and hands before [`Self::update`]. Anywhere else, or in another space, the room is back home. See `design/locomotion.md`.
    pub fn rig(&mut self, conversation: &Conversation) -> Rig {
        self.travel
            .settle(place(conversation), conversation.locomotion())
    }

    /// The participant recentred: the floating panel stands in front of them again.
    pub fn recentre(&mut self) {
        self.panel.reset();
    }

    /// Whether the hand menu is pinned open with Y.
    pub fn pinned(&self) -> bool {
        self.hand_menu.pinned()
    }

    /// Let go of a menu pinned with Y. Returns whether it was pinned.
    #[cfg(not(target_os = "android"))]
    pub fn unpin(&mut self) -> bool {
        self.hand_menu.unpin()
    }

    /// Whether the right hand points this frame, from `trigger` and what the ray met: the palm turns while pointing across the body.
    pub fn pointing(&self, trigger: Option<f32>) -> bool {
        trigger.is_some_and(|trigger| trigger > POINTING)
            || matches!(self.highlighted, Some(arrival::Key::Menu(_)))
    }

    /// Act on this frame's `buttons` at `now`, `focused` if the participant has input. Buttons act on every frame, even one that is not drawn.
    pub fn buttons(
        &mut self,
        buttons: &Buttons,
        focused: bool,
        conversation: &mut Conversation,
        feedback: &mut Feedback,
        now: Instant,
    ) {
        let stick = buttons.left_stick;
        // In a space whose environment allows it, the left stick travels, which the frame step does with the ray; see `design/locomotion.md`.
        self.travel_stick = if focused { stick } else { Vec2::ZERO };
        let moving = stick.x.abs() > STICK || stick.y.abs() > STICK;
        let navigate = moving && !self.stick_latched;
        self.stick_latched = moving;
        // In a space the left stick is free: the panel it paged is now the transcript on the right wrist.
        if navigate && conversation.joined_space().is_none() {
            if stick.y.abs() >= stick.x.abs() {
                conversation.navigate(0, if stick.y > 0.0 { -1 } else { 1 });
            } else {
                conversation.navigate(if stick.x > 0.0 { 1 } else { -1 }, 0);
            }
        }
        // While the right wrist's transcript shows, the right stick, A and B act on it, and B backs out of it before the space. See `design/wrist-transcript.md`.
        let wrist_active = self.wrist.state().active();
        let right = buttons.right_stick;
        // B lets go of a menu pinned with Y first.
        let unpinned = buttons.back && self.hand_menu.unpin();
        let wrist_input = self.wrist.input(
            conversation,
            right,
            buttons.a && !self.menu_was_shown,
            buttons.back && !unpinned,
            now,
        );
        // Moving the transcript's focus onto a turn, a request's button or a queued entry ticks as hover does, and A choosing taps; see `design/feedback.md`.
        if wrist_input.moved {
            feedback.emit(feedback::Event::Hover, now);
        }
        if wrist_input.on_request {
            feedback.request_opened();
        }
        if wrist_input.chose {
            feedback.emit(feedback::Event::Choose, now);
        }
        let back_taken = wrist_input.back || unpinned;
        if buttons.back && !back_taken {
            conversation.back();
        }
        // Only a hold whose press is in a space can leave it, so B held while joining does not.
        if buttons.back && conversation.joined_space().is_some() {
            self.back_since = Some(now);
        }
        match self.back_since {
            // A hold ends with the space it began in, so that it never leaves the next one.
            Some(since) if buttons.back_held && conversation.joined_space().is_some() => {
                let held = now.duration_since(since);
                feedback.hold_to_leave(Some(
                    held.as_secs_f32() / crate::conversation::LEAVE_HOLD.as_secs_f32(),
                ));
                if conversation.hold_back(Some(held)) {
                    feedback.emit(feedback::Event::Left, now);
                }
            }
            Some(_) => {
                self.back_since = None;
                conversation.hold_back(None);
            }
            None => {}
        }
        if self.back_since.is_none() {
            feedback.hold_to_leave(None);
        }
        conversation.tick_space(now);
        // Y in a space pins the hand menu open at its Space section, until Y or B; elsewhere it browses or lists again. See `design/hand-menu.md`.
        if conversation.joined_space().is_none() {
            self.hand_menu.unpin();
        }
        if buttons.y {
            if conversation.joined_space().is_some() {
                self.hand_menu.toggle_pin();
                if self.hand_menu.pinned() {
                    feedback.emit(feedback::Event::Shown(feedback::Hand::Left), now);
                }
            } else {
                conversation.refresh();
            }
        }
        // A chooses the object the ray highlighted last frame, as the trigger does; with nothing highlighted it accepts. Inside a space it never resets the swarm; see `design/interaction.md`.
        match &self.highlighted {
            // The ray only meets rows that can be chosen.
            Some(arrival::Key::Menu(key)) if buttons.a => {
                feedback.emit(feedback::Event::Choose, now);
                if let Some(choice) = self.hand_menu.choose(key) {
                    conversation.choose_menu(choice);
                }
            }
            // While the hand menu shows, A chooses only in it: nothing behind it, such as an approval; and while the transcript shows, only in that.
            _ if self.menu_was_shown || wrist_active => {}
            _ if buttons.a => {
                let chose =
                    !conversation.press_a(self.highlighted.as_ref()) && !conversation.a_refused();
                if chose {
                    feedback.emit(feedback::Event::Choose, now);
                }
            }
            _ => {}
        }
        // Hold X to talk to the agent; releasing it while focused sends the utterance.
        if focused && !buttons.talk {
            self.talk_blocked = false;
        }
        let talking = focused && buttons.talk && !self.talk_blocked;
        if talking != self.was_talking {
            conversation.talk(talking);
        }
        self.was_talking = talking;
        // Left stick click mutes spoken replies locally; right stick click plays the latest reply again, or stops it.
        if buttons.mute {
            conversation.toggle_mute();
        }
        if buttons.replay {
            conversation.replay();
        }
        // Otherwise the right stick pages result cards.
        self.presentation_navigation.update(
            None,
            &delegation::Workspace::default(),
            if wrist_active { 0.0 } else { right.x },
            if wrist_active { 0.0 } else { right.y },
        );
    }

    /// Move everything on by `dt` seconds to `now`, as `senses` find it, and say what to draw and report.
    pub fn update(
        &mut self,
        senses: &Senses,
        conversation: &mut Conversation,
        feedback: &mut Feedback,
        now: Instant,
        dt: f32,
    ) -> Frame<'_> {
        let focused = senses.focused;
        let visual = self.signals.update(
            interaction::Inputs {
                session: if focused {
                    conversation.activity()
                } else {
                    interaction::SessionActivity::Unknown
                },
                ..Default::default()
            },
            dt,
        );
        // Follows the agent's activity, which can change many times a minute.
        if visual.mode != self.swarm.visual.mode {
            log::debug!(
                "Swarm interaction: {:?}, session_working={}",
                visual.mode,
                visual.working
            );
        }
        self.swarm.set_visual(visual, dt);
        // Arrival's objects move between its steps rather than cutting, and are pointed at where they are now. See `design/arrival.md`.
        self.arrival_time += dt;
        self.motion
            .update(&arrival_target(conversation), self.arrival_time);
        let mut scene = self.motion.scene(self.arrival_time);
        let moment = Moment {
            now,
            dt,
            time: self.arrival_time,
        };
        // While recording, the indicator rises from the left controller, or the left wrist, or else stands low in the view. See `design/hand-menu.md`.
        let indicator = self
            .indicator
            .update(conversation, senses.trackers, senses.viewer, moment);
        // The hand menu shows above the left wrist while the left palm is turned up, as the right palm shows the transcript. Whichever of it and the right transcript shows first wins; while it shows, the trigger chooses only in it. See `design/hand-menu.md`.
        let menu_shown = focused
            && self.hand_menu.update(
                senses.left_palm.filter(|_| !self.wrist.state().on_hand()),
                now,
            );
        self.menu_was_shown = menu_shown;
        let in_space = conversation.joined_space().is_some();
        let (wrist_opened, wrist) = self.wrist.update(
            conversation,
            WristFrame {
                palm: senses.right_palm.filter(|_| focused),
                hands_only: senses.hands_only,
                head: senses.head,
                viewer: senses.viewer,
                in_space,
                menu_shown,
                menu_pinned: self.hand_menu.pinned(),
                pointing: senses.pointing,
                grip: senses.grip,
            },
            moment,
        );
        // The transcript appearing on the wrist ticks very lightly, and counts as looking at a request it shows.
        if wrist_opened {
            feedback.request_opened();
            feedback.emit(feedback::Event::Shown(feedback::Hand::Right), now);
        }
        let wrist_shown = self.wrist.state().on_hand();
        let wrist_active = self.wrist.state().active();
        let wrist_waiting = conversation.wrist_waiting();
        // What the ray pointed at last frame, so that the description of the mode pointed at shows beneath the modes, in rows that are there whatever is pointed at, so that the rows do not move under the ray.
        let pointed = match &self.highlighted {
            Some(arrival::Key::Menu(key)) => Some(key.clone()),
            _ => None,
        };
        let menu = |hand_menu: &HandMenu, conversation: &Conversation| {
            let mut objects = match senses.left_palm {
                Some(palm) if menu_shown => {
                    let mut model = conversation.menu_model();
                    model.pointed = pointed.clone();
                    hand_menu.objects(&palm, &model, senses.viewer.unwrap_or(arrival::HOME))
                }
                Some(palm) if senses.cues && conversation.menu_model().waiting() => {
                    vec![hand_menu::cue(&palm)]
                }
                _ => Vec::new(),
            };
            // A cue on the right wrist while the transcript is closed and something waits on it.
            if let (true, Some(palm), Some(problem), false) =
                (senses.cues, senses.right_palm, wrist_waiting, wrist_active)
            {
                objects.push(hand_menu::wrist_cue(&palm, problem));
            }
            objects
        };
        scene.objects.extend(menu(&self.hand_menu, conversation));
        // Labels and forms face the participant's head, in steps of 5 cm so that they are not rebuilt every frame as the head sways.
        let viewer = senses.viewer.map(|head| (head * 20.0).round() / 20.0);
        scene.viewer = viewer;
        let targets: Vec<_> = scene
            .targets()
            .into_iter()
            .filter(|target| !menu_shown || matches!(target.key, arrival::Key::Menu(_)))
            .collect();
        let pointing = self.pointer.update(senses.ray, senses.trigger, &targets);
        self.highlighted = pointing.hit.as_ref().map(|hit| hit.key.clone());
        if let Some(key) = &pointing.chosen {
            feedback.emit(feedback::Event::Choose, now);
            match key {
                arrival::Key::Menu(key) => {
                    if let Some(choice) = self.hand_menu.choose(key) {
                        conversation.choose_menu(choice);
                    }
                }
                key => conversation.choose_object(key),
            }
            self.motion
                .update(&arrival_target(conversation), self.arrival_time);
            scene = self.motion.scene(self.arrival_time);
            scene.objects.extend(menu(&self.hand_menu, conversation));
            scene.viewer = viewer;
        }
        let in_arrival = conversation.arrival().is_some();
        // Arrival and the lobby show the grid, and a space its own environment, growing out of the space's object as it is entered; no control switches it. See `design/environments.md`.
        let scenery = self.motion.scenery(self.arrival_time);
        // A trigger pull that misses the hand menu's rows does nothing while it shows, nor while the transcript shows.
        let attracting = pointing.attracting && !menu_shown && !wrist_active;
        let attractor = senses.attractor.filter(|_| attracting);
        self.swarm.step(dt, attractor);
        let marker = match senses.marker {
            Marker::Always => senses.attractor,
            Marker::WhileAttracting => attractor,
            Marker::Hidden => None,
        };
        // In a space, the ray points at workers too, which only highlights them: the trigger still attracts the swarm, and holding X sends the worker pointed at with the words. See `design/interaction.md`. While the transcript shows, the ray is hidden and points at nothing.
        let in_space = focused
            && !in_arrival
            && !menu_shown
            && !wrist_active
            && conversation.joined_space().is_some();
        // In an environment that allows it, the left stick travels: aiming a teleport with the ray, landing as it returns to the middle, turning and rising, each push owned by the direction it went first. The rig it moves applies from the next frame, as the platform places this frame's poses with it. See `design/locomotion.md`.
        let travel = self.travel.update(locomotion::Input {
            place: place(conversation),
            locomotion: conversation.locomotion(),
            stick: self.travel_stick,
            ray: senses.ray,
            head: senses.head_pose.map(|pose| Vec3::from_array(pose.position)),
            // Only once the environment's scenery has grown in, so that its surfaces are seen; and not while a card is carried, since aiming would put the card down.
            active: in_space
                && scenery.reveal.is_none()
                && scenery.environment == conversation.environment()
                && self.board.held().is_none(),
            dt,
        });
        if travel.teleported {
            feedback.emit(feedback::Event::Choose, now);
        }
        // While a teleport is aimed, the ray serves only it.
        let in_space = in_space && !travel.aiming;
        // The space's cards stand wherever it shows them; the ray and the right grip act on them only while nothing else takes them. See `design/github.md`.
        let board = self.board.update(crate::board::Input {
            cards: conversation.cards().filter(|_| !in_arrival),
            ray: senses.ray,
            grip: senses.grip,
            viewer: senses.viewer,
            active: in_space,
            now,
        });
        if board.picked {
            feedback.emit(feedback::Event::Choose, now);
        }
        if let Some((card, placement)) = board.placed {
            feedback.emit(feedback::Event::Hover, now);
            conversation.place_card(card, placement);
        }
        let worker = self
            .delegation_scene
            .point(senses.ray.filter(|_| in_space))
            .map(str::to_owned);
        // The nearer of the card and the worker the ray meets is pointed at; a card carried is pointed at all along.
        let reach = |point: Vec3| {
            senses
                .ray
                .map_or(f32::INFINITY, |ray| point.distance(ray.origin))
        };
        let card = self.board.held().map(str::to_owned).or_else(|| {
            board
                .pointed
                .as_ref()
                .filter(|(_, distance, _)| {
                    self.delegation_scene
                        .hit_point()
                        .is_none_or(|point| *distance < reach(point))
                })
                .map(|(card, _, _)| card.clone())
        });
        let worker = worker.filter(|_| card.is_none());
        // The environment's client side, in a space whose scenery has grown in, made for the space and environment it
        // is in. It points at what the environment shows only where nothing else is nearer, and not while a card is
        // carried.
        let shown = conversation.client_side().filter(|(environment, _)| {
            scenery.reveal.is_none() && scenery.environment == *environment
        });
        let space = conversation
            .joined_space()
            .map(|(space, _)| space.id.clone());
        match (shown, space) {
            (Some((environment, factory)), Some(space)) => {
                let current = self.client.as_ref().is_some_and(|(in_space, drawn, _)| {
                    *in_space == space && *drawn == environment
                });
                if !current {
                    let client = factory(&space);
                    self.client = Some((space, environment, client));
                }
            }
            _ => self.client = None,
        }
        let nearest = card
            .as_ref()
            .and_then(|_| board.pointed.as_ref().map(|(_, distance, _)| *distance))
            .or_else(|| {
                worker
                    .as_ref()
                    .and(self.delegation_scene.hit_point())
                    .map(reach)
            })
            .unwrap_or(f32::INFINITY);
        let object = self
            .client
            .as_ref()
            .zip(
                senses
                    .ray
                    .filter(|_| in_space && self.board.held().is_none()),
            )
            .and_then(|((_, _, client), ray)| client.pick(&ray))
            .filter(|picked| picked.distance < nearest);
        let (card, worker) = if object.is_some() {
            (None, None)
        } else {
            (card, worker)
        };
        // Entering a new target, an arrival object, a hand menu row, a card or a worker, ticks very lightly, but not while the trigger pulls the swarm.
        let target = self
            .highlighted
            .as_ref()
            .map(feedback::key)
            .or_else(|| card.as_ref().map(feedback::key))
            .or_else(|| worker.as_ref().map(feedback::key))
            .or_else(|| object.as_ref().map(|picked| feedback::key(&picked.object)))
            .or_else(|| travel.landing.map(|_| feedback::key("landing")));
        if self.hover.update(target, attracting) {
            feedback.emit(feedback::Event::Hover, now);
        }
        let card_end = card.as_ref().and(board.end);
        let object_end = object
            .as_ref()
            .zip(senses.ray)
            .map(|(picked, ray)| ray.at(picked.distance));
        match (card, worker, &object) {
            (Some(card), _, _) => conversation.point(Some(tomori_protocol::Resource::card(card))),
            (None, Some(worker), _) => {
                conversation.point(Some(tomori_protocol::Resource::worker(worker)))
            }
            (None, None, Some(picked)) => {
                conversation.point_at_object(&picked.object, &picked.label)
            }
            (None, None, None) => conversation.point(None),
        }
        // What the environment's server side sent reaches its client side, which says what to ask, what it sounds and
        // what its scenery draws.
        let drawn = match self.client.as_mut() {
            Some((_, environment, client)) => {
                let updates =
                    conversation.environment_updates(environment.id(), environment.version());
                let head = senses.head_pose.map(|pose| Vec3::from_array(pose.position));
                let output = client.update(
                    crate::client_side::Input::new(updates, now)
                        .pointing(object.as_ref().map(|picked| picked.object.as_str()), head),
                );
                for intent in output.intents {
                    conversation.environment_intent(
                        environment.id(),
                        environment.version(),
                        intent,
                    );
                }
                for (sound, position) in output.sounds {
                    feedback.ambient(sound, position, now);
                }
                output.drawn
            }
            None => None,
        };
        let (root, workspace) = self.workers.update(conversation.joined_space());
        self.coordinator_samples.clear();
        self.coordinator_samples.extend(
            self.swarm
                .agents
                .iter()
                .map(|agent| agent.position.to_array()),
        );
        self.delegation_scene
            .set_coordinator_samples(&self.coordinator_samples);
        self.delegation_scene.update(root, workspace, dt);
        // The ray is drawn wherever it points at something: at arrival's objects or the hand menu, and in a space at the workers, where it ends among the worker it meets or else stops short of them. See `design/interaction.md`.
        let ray = senses.ray.filter(|_| senses.beam);
        let beam = if in_arrival || menu_shown {
            ray.zip(pointing.endpoint(crate::pointing::RAY_LENGTH))
                .map(|(ray, end)| crate::pointing::Beam {
                    origin: ray.origin,
                    end,
                    hit: pointing.hit.is_some(),
                })
        } else if travel.aiming {
            // Aiming a teleport, it ends at the landing, highlighted, or is plain and short where there is nowhere to land.
            ray.map(|ray| crate::pointing::Beam {
                origin: ray.origin,
                end: travel
                    .landing
                    .unwrap_or_else(|| ray.at(crate::pointing::SPACE_RAY_LENGTH)),
                hit: travel.landing.is_some(),
            })
        } else {
            let hit = card_end
                .or(object_end)
                .or_else(|| self.delegation_scene.hit_point());
            ray.filter(|_| in_space).map(|ray| crate::pointing::Beam {
                origin: ray.origin,
                end: hit.unwrap_or_else(|| ray.at(crate::pointing::SPACE_RAY_LENGTH)),
                hit: hit.is_some(),
            })
        };
        // Arrival's objects are still drawn as they leave on entering a space.
        let arrival = (in_arrival
            || menu_shown
            || beam.is_some()
            || !scene.objects.is_empty()
            || scene.heading.is_some())
        .then(|| Arrival {
            hovered: pointing.hit.as_ref().map(|hit| hit.key.clone()),
            ray: beam,
            time: self.arrival_time,
            scene,
        });
        // Every source is heard from where it is, relative to the head; see `design/spatial-audio.md`.
        let agent = crate::spatial_audio::centre(&self.coordinator_samples);
        // The others in a space see this participant's head, controllers and hands, in the space's scene coordinates. Visitors send no presence; see `design/security.md`.
        let presence = senses
            .head_pose
            .filter(|_| conversation.role().participates())
            .map(|head| Presence {
                head,
                left: senses.controllers[crate::controller::LEFT],
                right: senses.controllers[crate::controller::RIGHT],
                hands: senses.hands,
            });
        // Arrival hides the panel; entering a space places it in front of the participant again.
        if in_arrival {
            self.panel.reset();
        }
        let panel_pose = self.panel.update(senses.space_head, dt);
        // Arrival has no pill, and it hides while a hand's panel shows, so that it does not cover it.
        let pill = controller_ui::pill(
            conversation,
            now,
            in_arrival || menu_shown || wrist_shown,
            panel_pose,
        );
        Frame {
            scenery,
            swarm: &self.swarm,
            marker,
            attracting,
            card: self.presentation_navigation.card,
            content: self.presentation_navigation.content,
            cubes: self.delegation_scene.cubes(),
            stations: self.delegation_scene.stations(),
            workspace,
            arrival,
            panels: Panels {
                pill,
                indicator,
                wrist,
            },
            listener: senses.head_pose,
            agent,
            presence,
            cards: board.sheets,
            landing: travel.landing,
            blink: travel.blink,
            environment: drawn,
        }
    }
}

/// Conversations and feedback in the states the frame step's tests need.
#[cfg(any(test, feature = "testing"))]
pub mod fixtures {
    use crate::conversation::Conversation;
    use tomori_client::{
        ConnectionState, Event,
        feedback::{Feedback, Settings},
        server_choice::{Choice, Source},
    };
    use tomori_protocol::{
        Approval, ApprovalOption, ConnectionRole, EnvironmentInfo, KnockRequest, Participant,
        ServerMessage, SpaceState,
    };

    pub const SPACE: &str = "space-1";

    /// Connected to a server as `role`, in the lobby.
    pub fn lobby(role: ConnectionRole) -> Conversation {
        let mut conversation = crate::test_support::conversation("Ada");
        conversation.connect(Choice::new("Studio", "10.0.0.1:8765", Source::Discovered));
        conversation.apply(Event::State(ConnectionState::Connected {
            source: 1,
            server: "studio".into(),
            role,
            certificate: None,
            backends: vec![],
            roots: vec![],
            environments: vec![EnvironmentInfo {
                id: "sky".into(),
                label: "Sky".into(),
                version: 1,
            }],
            utterance_limit: None,
            name: None,
        }));
        conversation.take_actions();
        conversation
    }

    /// The space's state, changed by `change`.
    pub fn space(change: impl FnOnce(&mut SpaceState)) -> Event {
        let mut state = SpaceState {
            id: SPACE.into(),
            title: "Tomori".into(),
            backend: "opencode".into(),
            directory: "/work/tomori".into(),
            environment: "sky".into(),
            participants: vec![Participant {
                name: "Ada".into(),
                source: 1,
                engagement: None,
            }],
            ..Default::default()
        };
        change(&mut state);
        Event::Message(ServerMessage::Space {
            state: Box::new(state),
        })
    }

    /// Joined to [`SPACE`] as its owner.
    pub fn joined() -> Conversation {
        let mut conversation = lobby(ConnectionRole::Owner);
        conversation.apply(space(|_| {}));
        conversation.take_actions();
        conversation
    }

    /// Show `cards` in [`SPACE`], which shows cards, as the server sends them.
    pub fn show_cards(conversation: &mut Conversation, cards: Vec<tomori_protocol::Card>) {
        conversation.apply(space(|state| state.cards = true));
        conversation.apply(Event::Message(ServerMessage::Cards {
            cards: Box::new(tomori_protocol::Cards {
                space: SPACE.into(),
                source: "o/r".into(),
                cards,
                status: None,
            }),
        }));
        conversation.take_actions();
    }

    /// Joined to [`SPACE`] with an approval waiting.
    pub fn approval_waiting() -> Conversation {
        let mut conversation = lobby(ConnectionRole::Owner);
        conversation.apply(space(|state| {
            state.approval = Some(Approval {
                id: "request-1".into(),
                title: "Bash".into(),
                text: "cargo test".into(),
                options: vec![ApprovalOption {
                    id: "once".into(),
                    label: "Allow once".into(),
                }],
            });
        }));
        conversation.take_actions();
        conversation
    }

    /// Joined to [`SPACE`], where the participant asked something and the agent replied.
    pub fn conversed() -> Conversation {
        let mut conversation = lobby(ConnectionRole::Owner);
        conversation.apply(space(|state| {
            state.messages = [
                (tomori_protocol::Role::User, "Fix the parser."),
                (tomori_protocol::Role::Assistant, "The parser is fixed."),
            ]
            .into_iter()
            .enumerate()
            .map(|(index, (role, text))| tomori_protocol::Message {
                id: format!("message-{index}"),
                role,
                text: text.into(),
                completed: true,
            })
            .collect();
        }));
        conversation.take_actions();
        conversation
    }

    /// A knock from `name` on the space.
    pub fn knock(conversation: &mut Conversation, id: &str, name: &str) {
        conversation.apply(Event::Message(ServerMessage::Knocks {
            knocks: vec![KnockRequest {
                id: id.into(),
                name: name.into(),
            }],
        }));
    }

    /// Feedback that vibrates, so that what would be felt can be seen.
    pub fn felt() -> Feedback {
        let mut feedback = Feedback::new(Settings::default());
        feedback.set_haptics(true);
        feedback
    }
}

#[cfg(test)]
mod tests {
    use super::fixtures::*;
    use super::*;
    use crate::{conversation::LEAVE_HOLD, hand_menu::HOLD};
    use std::time::Duration;
    use tomori_client::{
        connection::Action,
        feedback::{Hand as Side, Haptic},
        playback::Earcon,
    };
    use tomori_protocol::ConnectionRole;

    const FRAME: f32 = 1.0 / 72.0;

    /// A head standing at home, looking ahead.
    fn senses() -> Senses {
        let head = Mat4::from_translation(arrival::HOME);
        Senses {
            focused: true,
            head: Some(head),
            head_pose: Pose::from_transform(head),
            viewer: Some(arrival::HOME),
            space_head: head,
            trigger: Some(0.0),
            ..Default::default()
        }
    }

    /// A right palm `degrees` from facing straight up, held in front of the head.
    fn palm_up(degrees: f32) -> Palm {
        let normal = glam::Quat::from_rotation_z(degrees.to_radians()) * Vec3::Y;
        Palm {
            wrist: arrival::HOME + Vec3::new(0.1, -0.35, -0.3),
            normal,
        }
    }

    /// A left palm `degrees` from facing straight up, held up in front of the head, as the right one in [`palm_up`] is.
    fn left_palm_up(degrees: f32) -> Palm {
        Palm {
            wrist: arrival::HOME + Vec3::new(-0.06, -0.2, -0.45),
            ..palm_up(degrees)
        }
    }

    /// A ray from the eye through the middle of `shape`.
    fn aim_at(shape: crate::pointing::Shape) -> Option<Ray> {
        Ray::new(arrival::HOME, shape.centre() - arrival::HOME)
    }

    /// The target with `key` in `scene`.
    fn target(scene: &arrival::Scene, key: &arrival::Key) -> crate::pointing::Target<arrival::Key> {
        scene
            .targets()
            .into_iter()
            .find(|target| &target.key == key)
            .expect("the target")
    }

    fn earcons(feedback: &mut Feedback) -> Vec<Earcon> {
        feedback.take_earcons().map(|(earcon, _)| earcon).collect()
    }

    fn felt_on(feedback: &mut Feedback, side: Side, now: Instant) -> Vec<Haptic> {
        feedback
            .haptics(now + Duration::from_secs(1))
            .into_iter()
            .filter(|(hand, _)| *hand == side)
            .map(|(_, haptic)| haptic)
            .collect()
    }

    fn press(change: impl FnOnce(&mut Buttons)) -> Buttons {
        let mut buttons = Buttons::default();
        change(&mut buttons);
        buttons
    }

    /// What a fake client side was told each frame: how many updates, what was pointed at, and whether the head was known.
    type Told = std::sync::Arc<std::sync::Mutex<Vec<(usize, Option<String>, bool)>>>;

    /// A client side that picks whatever the ray meets a metre away, asks and sounds once a frame, and shows how many
    /// frames it has had, keeping what it was told.
    struct Fake {
        frames: usize,
        told: Told,
    }

    impl crate::client_side::ClientSide for Fake {
        fn pick(&self, _ray: &Ray) -> Option<crate::client_side::Picked> {
            Some(crate::client_side::Picked::new("thing-1", "the thing", 1.0))
        }

        fn update(&mut self, input: crate::client_side::Input<'_>) -> crate::client_side::Output {
            self.frames += 1;
            self.told.lock().unwrap().push((
                input.updates.len(),
                input.pointed.map(str::to_owned),
                input.head.is_some(),
            ));
            crate::client_side::Output::new()
                .with_intent(tomori_protocol::Payload::new(serde_json::json!(
                    self.frames
                )))
                .with_sound(
                    crate::client_side::Sound::new(vec![0.0; 4], 48_000),
                    [0.0, 1.0, -1.0],
                )
                .drawing(std::sync::Arc::new(self.frames))
        }
    }

    #[test]
    fn an_environment_s_client_side_is_made_for_its_space_once_it_grows_in_and_presents_what_its_side_shares()
     {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::{Arc, Mutex};
        use tomori_client::Event;
        use tomori_protocol::Resource;
        use tomori_protocol::{ClientMessage, ServerMessage};
        let made = Arc::new(AtomicUsize::new(0));
        let told = Arc::new(Mutex::new(Vec::new()));
        let factory: crate::client_side::Factory = {
            let (made, told) = (made.clone(), told.clone());
            Arc::new(move |space: &str| {
                assert_eq!(space, SPACE);
                made.fetch_add(1, Ordering::Relaxed);
                Box::new(Fake {
                    frames: 0,
                    told: told.clone(),
                }) as Box<dyn crate::client_side::ClientSide>
            })
        };
        let mut conversation = lobby(ConnectionRole::Owner);
        conversation.set_client_sides(vec![(crate::test_support::SKY, factory)]);
        assert!(conversation.client_side().is_none());
        let mut step = Step::default();
        let mut feedback = felt();
        let now = Instant::now();
        let senses = Senses {
            ray: Ray::new(arrival::HOME, Vec3::NEG_Z),
            ..senses()
        };
        conversation.apply(space(|_| {}));
        conversation.take_actions();
        assert_eq!(
            conversation
                .client_side()
                .map(|(environment, _)| environment),
            Some(crate::test_support::SKY)
        );
        // Not while the space's scenery grows in.
        let growing = step.update(&senses, &mut conversation, &mut feedback, now, FRAME);
        assert!(growing.environment.is_none());
        let mut shown = None;
        for _ in 0..400 {
            shown = step
                .update(&senses, &mut conversation, &mut feedback, now, FRAME)
                .environment;
        }
        // Made once, it shows what it shows, sounds, and points at what the ray meets, which the participant attends to.
        assert_eq!(made.load(Ordering::Relaxed), 1);
        let frames = *shown.unwrap().downcast_ref::<usize>().unwrap();
        assert!(frames > 1);
        assert_eq!(
            told.lock().unwrap().last(),
            Some(&(0, Some("thing-1".to_owned()), true))
        );
        assert_eq!(feedback.take_ambient().count(), 1);
        conversation.talk(true);
        conversation.talk(false);
        let actions = conversation.take_actions();
        let Some(Action::BeginUtterance(utterance)) = actions.first() else {
            unreachable!("recording begins, not {actions:?}")
        };
        assert_eq!(utterance.attention, [Resource::object("thing-1")]);
        // Before the server sends the environment's state, its intents are not sent; after, they are, and it is told.
        assert!(!format!("{actions:?}").contains("EnvironmentIntent"));
        conversation.apply(Event::Message(ServerMessage::EnvironmentState {
            environment: "sky".into(),
            version: 1,
            state: tomori_protocol::Payload::new(serde_json::json!({})),
        }));
        step.update(&senses, &mut conversation, &mut feedback, now, FRAME);
        assert_eq!(told.lock().unwrap().last().unwrap().0, 1);
        assert!(conversation.take_actions().iter().any(|action| matches!(
            action,
            Action::Send(ClientMessage::EnvironmentIntent { .. })
        )));
        // Leaving, it is gone; joining again, it is made anew.
        conversation.apply(Event::Message(ServerMessage::Left));
        assert!(
            step.update(&senses, &mut conversation, &mut feedback, now, FRAME)
                .environment
                .is_none()
        );
        assert!(conversation.client_side().is_none());
        conversation.apply(space(|_| {}));
        for _ in 0..400 {
            step.update(&senses, &mut conversation, &mut feedback, now, FRAME);
        }
        assert_eq!(made.load(Ordering::Relaxed), 2);
        // An environment it has no client side for has none.
        conversation.set_client_sides(Vec::new());
        assert!(conversation.client_side().is_none());
        assert!(
            step.update(&senses, &mut conversation, &mut feedback, now, FRAME)
                .environment
                .is_none()
        );
    }

    #[test]
    fn travel_waits_for_the_space_s_scenery_to_grow_in() {
        use crate::locomotion::{Locomotion, Surface};
        let mut conversation = lobby(ConnectionRole::Owner);
        conversation.set_locomotion(vec![(
            crate::test_support::SKY,
            Locomotion::teleport(vec![Surface::rectangle(Vec3::ZERO, Vec2::splat(40.0))]),
        )]);
        let mut step = Step::default();
        let now = Instant::now();
        let senses = Senses {
            ray: Ray::new(arrival::HOME, Vec3::new(0.0, -1.0, -1.0)),
            ..senses()
        };
        step.update(&senses, &mut conversation, &mut felt(), now, FRAME);
        conversation.apply(space(|_| {}));
        step.buttons(
            &press(|buttons| buttons.left_stick = Vec2::Y),
            true,
            &mut conversation,
            &mut felt(),
            now,
        );
        let growing = step.update(&senses, &mut conversation, &mut felt(), now, FRAME);
        assert_ne!(
            growing.scenery,
            arrival_motion::Scenery {
                environment: crate::test_support::SKY,
                reveal: None
            }
        );
        assert_eq!(growing.landing, None);
        let mut landing = None;
        for _ in 0..400 {
            landing = step
                .update(&senses, &mut conversation, &mut felt(), now, FRAME)
                .landing;
        }
        // A push held from before it grew in aims nothing: the stick comes back to the middle first.
        assert_eq!(landing, None);
        for stick in [Vec2::ZERO, Vec2::Y] {
            step.buttons(
                &press(|buttons| buttons.left_stick = stick),
                true,
                &mut conversation,
                &mut felt(),
                now,
            );
            landing = step
                .update(&senses, &mut conversation, &mut felt(), now, FRAME)
                .landing;
        }
        assert!(landing.is_some(), "once it has grown in");
    }

    #[test]
    fn aiming_a_teleport_takes_the_ray_from_workers_and_the_hand_menu_or_focus_cancels_it() {
        use crate::locomotion::{Locomotion, Surface};
        use tomori_protocol::Resource;
        let mut conversation = lobby(ConnectionRole::Owner);
        conversation.set_locomotion(vec![(
            crate::test_support::SKY,
            Locomotion::teleport(vec![Surface::rectangle(Vec3::ZERO, Vec2::splat(40.0))]),
        )]);
        conversation.apply(space(|state| {
            state.workers = vec![tomori_protocol::Worker {
                id: "ses_a".into(),
                parent: None,
                title: "Review".into(),
                state: tomori_protocol::WorkerState::Running,
            }];
        }));
        let mut step = Step::default();
        let mut feedback = felt();
        let now = Instant::now();
        let mut senses = Senses {
            beam: true,
            ..senses()
        };
        // Travel waits for the space's scenery to grow in.
        for _ in 0..400 {
            step.update(&senses, &mut conversation, &mut feedback, now, FRAME);
        }
        let worker = Vec3::from_array(
            step.update(&senses, &mut conversation, &mut feedback, now, FRAME)
                .stations[0]
                .position,
        );
        senses.ray = Ray::new(arrival::HOME, worker - arrival::HOME);
        let attending = |conversation: &mut Conversation| {
            conversation.talk(true);
            conversation.talk(false);
            let actions = conversation.take_actions();
            let Some(Action::BeginUtterance(utterance)) = actions.first() else {
                unreachable!("recording begins, not {actions:?}")
            };
            utterance.attention.clone()
        };
        let stick = |step: &mut Step, conversation: &mut Conversation, stick: Vec2| {
            step.buttons(
                &press(|buttons| buttons.left_stick = stick),
                true,
                conversation,
                &mut felt(),
                now,
            );
        };
        conversation.take_actions();
        step.update(&senses, &mut conversation, &mut feedback, now, FRAME);
        assert_eq!(attending(&mut conversation), [Resource::worker("ses_a")]);
        // Aiming, the worker is no longer pointed at. It stands above the floor, so a ray at it has nowhere to land, and is drawn plain and short of it.
        stick(&mut step, &mut conversation, Vec2::Y);
        let output = step.update(&senses, &mut conversation, &mut feedback, now, FRAME);
        let beam = output
            .arrival
            .as_ref()
            .and_then(|arrival| arrival.ray)
            .unwrap();
        assert_eq!(output.landing, None);
        assert!(!beam.hit);
        let short = senses.ray.unwrap().at(crate::pointing::SPACE_RAY_LENGTH);
        assert!(beam.end.distance(short) < 1e-4);
        assert!(attending(&mut conversation).is_empty());
        // Down at the floor, it lands; with the hand menu showing, the aim is dropped and releasing does nothing.
        senses.ray = Ray::new(arrival::HOME, Vec3::new(0.0, -1.0, -1.0));
        assert!(
            step.update(&senses, &mut conversation, &mut feedback, now, FRAME)
                .landing
                .is_some()
        );
        senses.left_palm = Some(left_palm_up(0.0));
        step.update(&senses, &mut conversation, &mut feedback, now, FRAME);
        let shown = step.update(&senses, &mut conversation, &mut feedback, now + HOLD, FRAME);
        assert_eq!(shown.landing, None);
        senses.left_palm = None;
        stick(&mut step, &mut conversation, Vec2::ZERO);
        step.update(&senses, &mut conversation, &mut feedback, now + HOLD, FRAME);
        assert_eq!(step.rig(&conversation), Rig::default());
        // Losing focus drops it too.
        stick(&mut step, &mut conversation, Vec2::Y);
        step.update(&senses, &mut conversation, &mut feedback, now + HOLD, FRAME);
        step.blur(&mut conversation);
        step.update(&senses, &mut conversation, &mut feedback, now + HOLD, FRAME);
        assert_eq!(step.rig(&conversation), Rig::default());
        // Unfocused buttons hold the stick in the middle.
        step.buttons(
            &press(|buttons| buttons.left_stick = Vec2::Y),
            false,
            &mut conversation,
            &mut felt(),
            now,
        );
        assert_eq!(step.travel_stick, Vec2::ZERO);
    }

    #[test]
    fn the_ray_points_at_the_nearer_of_a_card_and_a_worker_and_the_grip_carries_a_card() {
        use tomori_protocol::{Card, CardKind, Cards, Checks, Resource, ServerMessage};
        let mut conversation = lobby(ConnectionRole::Owner);
        conversation.apply(space(|state| {
            state.cards = true;
            state.workers = vec![tomori_protocol::Worker {
                id: "ses_a".into(),
                parent: None,
                title: "Review".into(),
                state: tomori_protocol::WorkerState::Running,
            }];
        }));
        let mut step = Step::default();
        let mut feedback = felt();
        let now = Instant::now();
        let mut senses = Senses {
            beam: true,
            ..senses()
        };
        let mut frame = |step: &mut Step, conversation: &mut Conversation, senses: &Senses| {
            step.update(senses, conversation, &mut feedback, now, FRAME)
                .stations
                .iter()
                .map(|station| Vec3::from_array(station.position))
                .collect::<Vec<_>>()
        };
        let worker = frame(&mut step, &mut conversation, &senses)[0];
        // Before the cards arrive, there are none to draw.
        assert!(
            step.update(&senses, &mut conversation, &mut felt(), now, FRAME)
                .cards
                .is_empty()
        );
        // One card stands halfway to the worker, facing the eye, and another beyond it.
        let between = arrival::HOME.lerp(worker, 0.5);
        let beyond = arrival::HOME.lerp(worker, 1.5);
        let placed = |at: Vec3| {
            let model = crate::recording::facing(at, arrival::HOME, 1.0);
            let (_, rotation, _) = model.to_scale_rotation_translation();
            tomori_protocol::Pose {
                position: at.to_array(),
                orientation: rotation.to_array(),
            }
        };
        let card = |number: u64, at: Vec3| Card {
            id: format!("github://o/r/issues/{number}"),
            number,
            kind: CardKind::Issue,
            title: "A card".into(),
            author: "ada".into(),
            labels: Vec::new(),
            checks: Checks::None,
            age: 0,
            placement: Some(placed(at)),
        };
        let show = |conversation: &mut Conversation, at: Vec3| {
            conversation.apply(tomori_client::Event::Message(ServerMessage::Cards {
                cards: Box::new(Cards {
                    space: SPACE.into(),
                    source: "o/r".into(),
                    cards: vec![card(1, at)],
                    status: None,
                }),
            }));
        };
        show(&mut conversation, between);
        conversation.take_actions();
        senses.ray = Ray::new(arrival::HOME, worker - arrival::HOME);
        let output = step.update(&senses, &mut conversation, &mut felt(), now, FRAME);
        assert_eq!(output.cards.len(), 2, "the inbox and the card");
        let beam = output
            .arrival
            .as_ref()
            .and_then(|arrival| arrival.ray)
            .unwrap();
        assert!(beam.hit && beam.end.distance(between) < 1e-3);
        // What X begins attending to as it is pressed.
        let attending = |conversation: &mut Conversation| {
            conversation.talk(true);
            conversation.talk(false);
            let actions = conversation.take_actions();
            let Some(Action::BeginUtterance(utterance)) = actions.first() else {
                unreachable!("recording begins, not {actions:?}")
            };
            utterance.attention.clone()
        };
        let card_pointed = Resource::card("github://o/r/issues/1");
        assert_eq!(
            attending(&mut conversation),
            std::slice::from_ref(&card_pointed)
        );
        // Beyond the worker, the worker is pointed at.
        show(&mut conversation, beyond);
        frame(&mut step, &mut conversation, &senses);
        assert_eq!(attending(&mut conversation), [Resource::worker("ses_a")]);
        // Back between, the grip picks it up and, carried, it is what is pointed at, wherever the ray goes.
        show(&mut conversation, between);
        frame(&mut step, &mut conversation, &senses);
        senses.grip = Some(1.0);
        frame(&mut step, &mut conversation, &senses);
        assert_eq!(step.board.held(), Some("github://o/r/issues/1"));
        // In an environment that allows travel, the left stick does not aim while a card is carried, which would put it down.
        conversation.set_locomotion(vec![(
            crate::test_support::SKY,
            crate::locomotion::Locomotion::teleport(vec![crate::locomotion::Surface::disc(
                Vec3::ZERO,
                20.0,
            )]),
        )]);
        // Once the scenery has grown in, where travel could otherwise aim.
        for _ in 0..400 {
            step.update(&senses, &mut conversation, &mut felt(), now, FRAME);
        }
        assert_eq!(step.board.held(), Some("github://o/r/issues/1"));
        conversation.take_actions();
        step.buttons(
            &press(|buttons| buttons.left_stick = Vec2::Y),
            true,
            &mut conversation,
            &mut felt(),
            now,
        );
        let output = step.update(&senses, &mut conversation, &mut felt(), now, FRAME);
        assert_eq!(output.landing, None);
        assert_eq!(step.board.held(), Some("github://o/r/issues/1"));
        assert!(conversation.take_actions().is_empty(), "nothing put down");
        step.buttons(
            &Buttons::default(),
            true,
            &mut conversation,
            &mut felt(),
            now,
        );
        conversation.set_locomotion(Vec::new());
        senses.ray = Ray::new(arrival::HOME, Vec3::new(0.5, 0.0, -1.0));
        let output = step.update(&senses, &mut conversation, &mut felt(), now, FRAME);
        let beam = output
            .arrival
            .as_ref()
            .and_then(|arrival| arrival.ray)
            .unwrap();
        assert!(
            beam.end
                .distance(senses.ray.unwrap().at(between.distance(arrival::HOME)))
                < 1e-3
        );
        assert_eq!(attending(&mut conversation), [card_pointed]);
        // Letting go puts it down there, for everyone in the space.
        senses.grip = Some(0.0);
        frame(&mut step, &mut conversation, &senses);
        let actions = conversation.take_actions();
        let [Action::Send(tomori_protocol::ClientMessage::PlaceCard { card, placement })] =
            actions.as_slice()
        else {
            unreachable!("one card put down, not {actions:?}");
        };
        assert_eq!(card, "github://o/r/issues/1");
        assert!(placement.is_some());
    }

    #[test]
    fn labels_face_the_viewer_in_steps_of_five_centimetres() {
        let mut conversation = joined();
        let mut step = Step::default();
        let start = Instant::now();
        let swaying = Senses {
            left_palm: Some(left_palm_up(0.0)),
            viewer: Some(arrival::HOME + Vec3::new(0.012, -0.013, 0.031)),
            ..senses()
        };
        step.update(&swaying, &mut conversation, &mut felt(), start, FRAME);
        let frame = step.update(
            &swaying,
            &mut conversation,
            &mut felt(),
            start + HOLD,
            FRAME,
        );
        let viewer = frame.arrival.expect("the menu").scene.viewer.unwrap();
        assert!(
            viewer.abs_diff_eq(Vec3::new(0.0, 1.6, 0.05), 1e-6),
            "{viewer}"
        );
        // The desktop's viewer, at home, is already on the grid, so it is unchanged.
        assert_eq!((arrival::HOME * 20.0).round() / 20.0, arrival::HOME);
    }

    #[test]
    fn a_stick_push_navigates_once_until_it_returns_and_never_in_a_space() {
        let mut conversation = Conversation::new("Ada");
        conversation.set_servers(
            ["10.0.0.1:8765", "10.0.0.2:8765", "10.0.0.3:8765"]
                .map(|address| {
                    tomori_client::server_choice::Choice::new(
                        address,
                        address,
                        tomori_client::server_choice::Source::Discovered,
                    )
                })
                .to_vec(),
        );
        let mut feedback = felt();
        let mut step = Step::default();
        let now = Instant::now();
        let down = press(|buttons| buttons.left_stick = Vec2::new(0.0, -1.0));
        let before = conversation.text();
        step.buttons(&down, true, &mut conversation, &mut feedback, now);
        let after = conversation.text();
        assert_ne!(before, after, "a push moves");
        step.buttons(&down, true, &mut conversation, &mut feedback, now);
        assert_eq!(conversation.text(), after, "holding it does not repeat");
        step.buttons(
            &Buttons::default(),
            true,
            &mut conversation,
            &mut feedback,
            now,
        );
        step.buttons(
            &press(|buttons| buttons.left_stick = Vec2::new(0.0, 1.0)),
            true,
            &mut conversation,
            &mut feedback,
            now,
        );
        assert_eq!(conversation.text(), before, "pushing back moves back");
        // Sideways, it moves between pages rather than rows, and all three servers are on one page.
        step.buttons(
            &Buttons::default(),
            true,
            &mut conversation,
            &mut feedback,
            now,
        );
        for x in [1.0, -1.0] {
            step.buttons(
                &press(|buttons| buttons.left_stick = Vec2::new(x, 0.2)),
                true,
                &mut conversation,
                &mut feedback,
                now,
            );
            step.buttons(
                &Buttons::default(),
                true,
                &mut conversation,
                &mut feedback,
                now,
            );
        }
        assert_eq!(conversation.text(), before);
        let mut conversation = joined();
        let before = conversation.text();
        step.buttons(&down, true, &mut conversation, &mut feedback, now);
        assert_eq!(conversation.text(), before);
    }

    #[test]
    fn holding_b_in_a_space_ramps_then_leaves_once_the_hold_is_long_enough() {
        let mut conversation = joined();
        let mut feedback = felt();
        let mut step = Step::default();
        let start = Instant::now();
        let held = press(|buttons| buttons.back_held = true);
        step.buttons(
            &press(|buttons| {
                buttons.back = true;
                buttons.back_held = true;
            }),
            true,
            &mut conversation,
            &mut feedback,
            start,
        );
        step.buttons(
            &held,
            true,
            &mut conversation,
            &mut feedback,
            start + LEAVE_HOLD / 2,
        );
        // The right controller ramps from light to firm as it is held.
        let ramp = felt_on(&mut feedback, Side::Right, start + LEAVE_HOLD / 2);
        assert!(
            matches!(ramp[..], [Haptic::Vibrate { amplitude, .. }] if (amplitude - 0.35).abs() < 0.01),
            "{ramp:?}"
        );
        assert!(conversation.joined_space().is_some());
        step.buttons(
            &held,
            true,
            &mut conversation,
            &mut feedback,
            start + LEAVE_HOLD,
        );
        assert!(earcons(&mut feedback).contains(&Earcon::Leave));
        assert!(
            conversation.take_actions().iter().any(|action| matches!(
                action,
                Action::Send(tomori_protocol::ClientMessage::Leave)
            ))
        );
    }

    #[test]
    fn letting_go_of_b_early_stays_and_stops_the_ramp() {
        let mut conversation = joined();
        let mut feedback = felt();
        let mut step = Step::default();
        let start = Instant::now();
        step.buttons(
            &press(|buttons| {
                buttons.back = true;
                buttons.back_held = true;
            }),
            true,
            &mut conversation,
            &mut feedback,
            start,
        );
        let half = start + LEAVE_HOLD / 2;
        step.buttons(
            &press(|buttons| buttons.back_held = true),
            true,
            &mut conversation,
            &mut feedback,
            half,
        );
        assert!(!felt_on(&mut feedback, Side::Right, half).is_empty());
        step.buttons(
            &Buttons::default(),
            true,
            &mut conversation,
            &mut feedback,
            half,
        );
        assert_eq!(felt_on(&mut feedback, Side::Right, half), [Haptic::Stop]);
        step.buttons(
            &press(|buttons| buttons.back_held = true),
            true,
            &mut conversation,
            &mut feedback,
            start + LEAVE_HOLD * 2,
        );
        assert!(conversation.joined_space().is_some());
    }

    #[test]
    fn b_held_from_before_joining_never_leaves() {
        let mut conversation = lobby(ConnectionRole::Owner);
        let mut feedback = felt();
        let mut step = Step::default();
        let start = Instant::now();
        step.buttons(
            &press(|buttons| {
                buttons.back = true;
                buttons.back_held = true;
            }),
            true,
            &mut conversation,
            &mut feedback,
            start,
        );
        assert!(step.back_since.is_none());
        // Held on into a space, it never leaves it.
        let mut conversation = joined();
        for seconds in 1..4 {
            step.buttons(
                &press(|buttons| buttons.back_held = true),
                true,
                &mut conversation,
                &mut feedback,
                start + Duration::from_secs(seconds),
            );
        }
        assert!(conversation.joined_space().is_some());
    }

    #[test]
    fn x_talks_while_held_and_after_focus_loss_only_once_released() {
        let mut conversation = joined();
        let mut feedback = felt();
        let mut step = Step::default();
        let now = Instant::now();
        let talk = press(|buttons| buttons.talk = true);
        step.buttons(&talk, true, &mut conversation, &mut feedback, now);
        assert!(conversation.recording().is_some());
        step.blur(&mut conversation);
        assert!(conversation.recording().is_none());
        // Still held through the system menu, it does not record again.
        step.buttons(&talk, true, &mut conversation, &mut feedback, now);
        assert!(conversation.recording().is_none());
        step.buttons(
            &Buttons::default(),
            true,
            &mut conversation,
            &mut feedback,
            now,
        );
        step.buttons(&talk, true, &mut conversation, &mut feedback, now);
        assert!(conversation.recording().is_some());
        // Without focus, X does nothing.
        step.buttons(
            &Buttons::default(),
            true,
            &mut conversation,
            &mut feedback,
            now,
        );
        let mut unfocused = Step::default();
        let mut conversation = joined();
        unfocused.buttons(&talk, false, &mut conversation, &mut feedback, now);
        assert!(conversation.recording().is_none());
    }

    #[test]
    fn y_pins_the_menu_in_a_space_until_b_and_lists_again_elsewhere() {
        let mut conversation = joined();
        let mut feedback = felt();
        let mut step = Step::default();
        let now = Instant::now();
        let y = press(|buttons| buttons.y = true);
        step.buttons(&y, true, &mut conversation, &mut feedback, now);
        assert!(step.pinned());
        assert!(
            !felt_on(&mut feedback, Side::Left, now).is_empty(),
            "pinning is felt"
        );
        // B lets go of it before anything else, so the space is not backed out of or left.
        step.buttons(
            &press(|buttons| {
                buttons.back = true;
                buttons.back_held = true;
            }),
            true,
            &mut conversation,
            &mut feedback,
            now,
        );
        assert!(!step.pinned());
        step.buttons(&y, true, &mut conversation, &mut feedback, now);
        step.buttons(&y, true, &mut conversation, &mut feedback, now);
        assert!(!step.pinned(), "Y again lets go");
        assert!(felt_on(&mut feedback, Side::Left, now).len() == 1);
        // Leaving the space lets go of a pinned menu.
        step.buttons(&y, true, &mut conversation, &mut feedback, now);
        let mut lobby = lobby(ConnectionRole::Owner);
        step.buttons(&Buttons::default(), true, &mut lobby, &mut feedback, now);
        assert!(!step.pinned());
        // Outside a space, Y lists the sessions again.
        step.buttons(&y, true, &mut lobby, &mut feedback, now);
        assert!(!step.pinned());
        assert!(!lobby.take_actions().is_empty());
    }

    #[test]
    fn mute_and_replay_reach_the_conversation() {
        let mut conversation = joined();
        let mut feedback = felt();
        let mut step = Step::default();
        let now = Instant::now();
        let muted = |conversation: &Conversation| conversation.menu_model().muted;
        let before = muted(&conversation);
        step.buttons(
            &press(|buttons| buttons.mute = true),
            true,
            &mut conversation,
            &mut feedback,
            now,
        );
        assert_ne!(muted(&conversation), before);
        step.buttons(
            &press(|buttons| buttons.replay = true),
            true,
            &mut conversation,
            &mut feedback,
            now,
        );
        // This server does not say what it speaks, so it cannot play a reply again, and says so.
        assert_eq!(
            conversation.menu_model().notice.as_deref(),
            Some("This server cannot play replies again.")
        );
    }

    #[test]
    fn the_left_palm_within_45_degrees_of_up_shows_the_menu_after_the_dwell_until_past_55() {
        let mut conversation = joined();
        let mut feedback = felt();
        let start = Instant::now();
        let at = |degrees| Senses {
            left_palm: Some(left_palm_up(degrees)),
            ..senses()
        };
        let mut step = Step::default();
        let frame = step.update(&at(40.0), &mut conversation, &mut feedback, start, FRAME);
        assert!(frame.arrival.is_none(), "not before the dwell");
        let frame = step.update(
            &at(40.0),
            &mut conversation,
            &mut feedback,
            start + HOLD / 2,
            FRAME,
        );
        assert!(frame.arrival.is_none(), "not halfway through it");
        let frame = step.update(
            &at(40.0),
            &mut conversation,
            &mut feedback,
            start + HOLD,
            FRAME,
        );
        let arrival = frame.arrival.expect("the menu");
        assert!(!arrival.scene.objects.is_empty());
        assert_eq!(arrival.scene.viewer, Some(arrival::HOME));
        // The pill hides while a hand's panel shows.
        assert_eq!(frame.panels.pill, Sheet::default());
        // Between the angles it stays, and past 55° it hides.
        let frame = step.update(
            &at(50.0),
            &mut conversation,
            &mut feedback,
            start + HOLD * 2,
            FRAME,
        );
        assert!(frame.arrival.is_some());
        let frame = step.update(
            &at(60.0),
            &mut conversation,
            &mut feedback,
            start + HOLD * 3,
            FRAME,
        );
        assert!(frame.arrival.is_none());
        // 50° from hidden never shows it.
        let mut step = Step::default();
        step.update(&at(50.0), &mut conversation, &mut feedback, start, FRAME);
        let frame = step.update(
            &at(50.0),
            &mut conversation,
            &mut feedback,
            start + HOLD,
            FRAME,
        );
        assert!(frame.arrival.is_none());
        // Without focus it never shows.
        let unfocused = Senses {
            focused: false,
            ..at(0.0)
        };
        let mut step = Step::default();
        step.update(&unfocused, &mut conversation, &mut feedback, start, FRAME);
        let frame = step.update(
            &unfocused,
            &mut conversation,
            &mut feedback,
            start + HOLD,
            FRAME,
        );
        assert!(frame.arrival.is_none());
    }

    #[test]
    fn turning_the_head_does_not_change_whether_either_palm_shows_its_panel() {
        let mut conversation = joined();
        let start = Instant::now();
        let neck = arrival::HOME - Vec3::new(0.0, 0.075, -0.08);
        for yaw in [-90.0, -45.0, 0.0, 45.0, 90.0, 180.0] {
            let head = crate::hand_menu::head_turned(neck, yaw, -20.0);
            let turned = Senses {
                head: Some(head),
                head_pose: Pose::from_transform(head),
                viewer: Some(crate::hand_menu::head_centre(head)),
                ..senses()
            };
            for (degrees, shown) in [(0.0, true), (40.0, true), (-40.0, true), (90.0, false)] {
                let mut step = Step::default();
                let left = Senses {
                    left_palm: Some(left_palm_up(degrees)),
                    ..turned
                };
                step.update(&left, &mut conversation, &mut felt(), start, FRAME);
                let frame = step.update(&left, &mut conversation, &mut felt(), start + HOLD, FRAME);
                assert_eq!(frame.arrival.is_some(), shown, "left {yaw} {degrees}");
                let mut step = Step::default();
                let right = Senses {
                    right_palm: Some(palm_up(degrees)),
                    ..turned
                };
                let wrist = wrist_after(&mut step, &mut conversation, &right, start);
                assert_eq!(wrist.model.is_some(), shown, "right {yaw} {degrees}");
            }
        }
    }

    /// The transcript's sheet after holding the right palm `degrees` from up, from hidden.
    fn wrist_after(
        step: &mut Step,
        conversation: &mut Conversation,
        senses: &Senses,
        start: Instant,
    ) -> Sheet {
        step.update(senses, conversation, &mut felt(), start, FRAME);
        step.update(senses, conversation, &mut felt(), start + HOLD, FRAME)
            .panels
            .wrist
    }

    #[test]
    fn the_right_palm_within_45_degrees_of_up_shows_the_transcript_after_the_dwell_until_past_55() {
        let mut conversation = joined();
        let start = Instant::now();
        let at = |degrees| Senses {
            right_palm: Some(palm_up(degrees)),
            ..senses()
        };
        let mut step = Step::default();
        let mut feedback = felt();
        let frame = step.update(&at(40.0), &mut conversation, &mut feedback, start, FRAME);
        assert!(frame.panels.wrist.model.is_none(), "not before the dwell");
        let frame = step.update(
            &at(40.0),
            &mut conversation,
            &mut feedback,
            start + HOLD / 2,
            FRAME,
        );
        assert!(frame.panels.wrist.model.is_none(), "not halfway through it");
        let frame = step.update(
            &at(40.0),
            &mut conversation,
            &mut feedback,
            start + HOLD,
            FRAME,
        );
        let shown = frame.panels.wrist.clone();
        assert!(shown.model.is_some() && shown.canvas.is_some());
        // Opening is felt lightly on the right hand.
        assert!(!felt_on(&mut feedback, Side::Right, start + HOLD).is_empty());
        // Between the angles it stays.
        let frame = step.update(
            &at(50.0),
            &mut conversation,
            &mut feedback,
            start + HOLD * 2,
            FRAME,
        );
        assert!(frame.panels.wrist.model.is_some());
        // Past 55° it hides.
        let frame = step.update(
            &at(60.0),
            &mut conversation,
            &mut feedback,
            start + HOLD * 3,
            FRAME,
        );
        assert_eq!(frame.panels.wrist, Sheet::default());
        // 50° from hidden never shows it.
        let mut step = Step::default();
        assert!(
            wrist_after(&mut step, &mut conversation, &at(50.0), start)
                .model
                .is_none()
        );
        // Nor does pointing, nor a palm without focus.
        let mut step = Step::default();
        let pointing = Senses {
            pointing: true,
            ..at(0.0)
        };
        assert!(
            wrist_after(&mut step, &mut conversation, &pointing, start)
                .model
                .is_none()
        );
        let mut step = Step::default();
        let unfocused = Senses {
            focused: false,
            ..at(0.0)
        };
        assert!(
            wrist_after(&mut step, &mut conversation, &unfocused, start)
                .model
                .is_none()
        );
        // Outside a space there is no transcript.
        let mut lobby = lobby(ConnectionRole::Owner);
        let mut step = Step::default();
        assert!(
            wrist_after(&mut step, &mut lobby, &at(0.0), start)
                .model
                .is_none()
        );
    }

    #[test]
    fn whichever_of_the_menu_and_the_transcript_shows_first_wins() {
        let mut conversation = joined();
        let start = Instant::now();
        let both = Senses {
            left_palm: Some(left_palm_up(0.0)),
            right_palm: Some(palm_up(0.0)),
            ..senses()
        };
        // The transcript first: the menu then does not show.
        let mut step = Step::default();
        let transcript = Senses {
            left_palm: None,
            ..both
        };
        wrist_after(&mut step, &mut conversation, &transcript, start);
        step.update(
            &both,
            &mut conversation,
            &mut felt(),
            start + HOLD * 2,
            FRAME,
        );
        let frame = step.update(
            &both,
            &mut conversation,
            &mut felt(),
            start + HOLD * 3,
            FRAME,
        );
        assert!(frame.panels.wrist.model.is_some());
        assert!(frame.arrival.is_none(), "no menu");
        // The menu first: the transcript then does not show.
        let mut step = Step::default();
        let menu = Senses {
            right_palm: None,
            ..both
        };
        step.update(&menu, &mut conversation, &mut felt(), start, FRAME);
        step.update(&menu, &mut conversation, &mut felt(), start + HOLD, FRAME);
        step.update(
            &both,
            &mut conversation,
            &mut felt(),
            start + HOLD * 2,
            FRAME,
        );
        let frame = step.update(
            &both,
            &mut conversation,
            &mut felt(),
            start + HOLD * 3,
            FRAME,
        );
        assert!(frame.panels.wrist.model.is_none());
        assert!(frame.arrival.is_some());
    }

    #[test]
    fn the_attractor_is_drawn_as_each_platform_draws_it() {
        let mut conversation = joined();
        let mut feedback = felt();
        let now = Instant::now();
        let at = Vec3::new(0.2, 1.0, -0.5);
        for (marker, idle, pulling) in [
            (Marker::Always, Some(at), Some(at)),
            (Marker::WhileAttracting, None, Some(at)),
            (Marker::Hidden, None, None),
        ] {
            let mut step = Step::default();
            let mut senses = Senses {
                attractor: Some(at),
                marker,
                ..senses()
            };
            let frame = step.update(&senses, &mut conversation, &mut feedback, now, FRAME);
            assert!(!frame.attracting);
            assert_eq!(frame.marker, idle, "{marker:?}");
            senses.trigger = Some(1.0);
            let frame = step.update(&senses, &mut conversation, &mut feedback, now, FRAME);
            assert!(frame.attracting, "a pull on nothing attracts");
            assert_eq!(frame.marker, pulling, "{marker:?}");
        }
    }

    #[test]
    fn the_ray_chooses_what_it_points_at_and_draws_the_beam_where_the_platform_does() {
        let now = Instant::now();
        for beam in [true, false] {
            let mut conversation = Conversation::new("Ada");
            conversation.set_servers(vec![tomori_client::server_choice::Choice::new(
                "Studio",
                "10.0.0.1:8765",
                tomori_client::server_choice::Source::Discovered,
            )]);
            let mut feedback = felt();
            let mut step = Step::default();
            // Arrival's objects move in over a few seconds.
            step.update(&senses(), &mut conversation, &mut feedback, now, FRAME);
            let frame = step.update(&senses(), &mut conversation, &mut feedback, now, 5.0);
            let scene = frame.arrival.expect("arrival's objects").scene;
            let target = target(&scene, &arrival::Key::Server("10.0.0.1:8765".into()));
            let aiming = Senses {
                ray: aim_at(target.shape),
                beam,
                ..senses()
            };
            let frame = step.update(&aiming, &mut conversation, &mut feedback, now, FRAME);
            let arrival = frame.arrival.expect("arrival");
            assert_eq!(arrival.hovered, Some(target.key.clone()));
            assert_eq!(arrival.ray.is_some_and(|ray| ray.hit), beam);
            assert!(earcons(&mut feedback).is_empty());
            // Pulling the trigger on it chooses it, and is heard.
            let pull = Senses {
                trigger: Some(1.0),
                ..aiming
            };
            let frame = step.update(&pull, &mut conversation, &mut feedback, now, FRAME);
            assert!(!frame.attracting);
            assert_eq!(earcons(&mut feedback), [Earcon::Choose]);
            assert!(
                conversation
                    .take_actions()
                    .iter()
                    .any(|action| matches!(action, Action::Connect(_)))
            );
        }
    }

    #[test]
    fn a_chooses_what_the_ray_met_last_frame_or_else_accepts() {
        let now = Instant::now();
        let mut conversation = Conversation::new("Ada");
        conversation.set_servers(vec![tomori_client::server_choice::Choice::new(
            "Studio",
            "10.0.0.1:8765",
            tomori_client::server_choice::Source::Discovered,
        )]);
        let mut feedback = felt();
        let mut step = Step::default();
        step.update(&senses(), &mut conversation, &mut feedback, now, FRAME);
        // With nothing highlighted, A accepts the selected server.
        step.buttons(
            &press(|buttons| buttons.a = true),
            true,
            &mut conversation,
            &mut feedback,
            now,
        );
        assert_eq!(earcons(&mut feedback), [Earcon::Choose]);
        assert!(
            conversation
                .take_actions()
                .iter()
                .any(|action| matches!(action, Action::Connect(_)))
        );
    }

    #[test]
    fn what_waits_is_cued_on_the_palms_only_where_the_platform_cues() {
        let now = Instant::now();
        // Palms turned down and away from the viewer, so that neither panel shows.
        let away = Palm {
            wrist: arrival::HOME + Vec3::new(0.0, -0.4, -0.3),
            normal: -Vec3::Y,
        };
        let cued = |conversation: &mut Conversation, cues: bool| {
            let senses = Senses {
                left_palm: Some(away),
                right_palm: Some(away),
                cues,
                ..senses()
            };
            Step::default()
                .update(&senses, conversation, &mut felt(), now, FRAME)
                .arrival
                .map_or(0, |arrival| arrival.scene.objects.len())
        };
        // A knock is cued on the left palm, and an approval on the right wrist.
        let mut knocked = joined();
        knock(&mut knocked, "knock-1", "Grace");
        assert_eq!(cued(&mut knocked, true), 1);
        assert_eq!(cued(&mut knocked, false), 0);
        let mut approval = approval_waiting();
        assert_eq!(cued(&mut approval, true), 1);
        assert_eq!(cued(&mut approval, false), 0);
        knock(&mut approval, "knock-1", "Grace");
        assert_eq!(cued(&mut approval, true), 2);
        assert_eq!(cued(&mut joined(), true), 0);
    }

    #[test]
    fn participants_report_their_head_and_hands_and_hear_from_their_head() {
        let now = Instant::now();
        let controller = Pose {
            position: [0.2, 1.1, -0.3],
            orientation: [0.0, 0.0, 0.0, 1.0],
        };
        let senses = Senses {
            controllers: [None, Some(controller)],
            ..senses()
        };
        let mut conversation = joined();
        let mut step = Step::default();
        let frame = step.update(&senses, &mut conversation, &mut felt(), now, FRAME);
        let presence = frame.presence.expect("a participant is seen");
        assert_eq!(Some(presence.head), senses.head_pose);
        assert_eq!((presence.left, presence.right), (None, Some(controller)));
        assert_eq!(frame.listener, senses.head_pose);
        // The agent is heard from the swarm's middle.
        assert!(Vec3::from_array(frame.agent).distance(crate::swarm::CENTER) < 1.0);
        // Without a head there is nothing to report, and visitors report nothing.
        let headless = Senses {
            head_pose: None,
            ..senses
        };
        assert!(
            step.update(&headless, &mut conversation, &mut felt(), now, FRAME)
                .presence
                .is_none()
        );
        let mut visitor = lobby(ConnectionRole::Visitor);
        visitor.apply(space(|_| {}));
        let mut step = Step::default();
        assert!(
            step.update(&senses, &mut visitor, &mut felt(), now, FRAME)
                .presence
                .is_none()
        );
    }

    #[test]
    fn the_pill_stands_ahead_of_the_head_and_a_recentre_places_it_again() {
        let now = Instant::now();
        let mut conversation = joined();
        let mut step = Step::default();
        let frame = step.update(&senses(), &mut conversation, &mut felt(), now, FRAME);
        let model = frame.panels.pill.model.expect("the pill shows in a space");
        let ahead = model.transform_point3(Vec3::ZERO);
        assert!(ahead.z < arrival::HOME.z - 1.0, "{ahead}");
        // Turned round, the pill follows only lazily, until a recentre places it ahead again.
        let turned = Senses {
            space_head: Mat4::from_rotation_translation(
                glam::Quat::from_rotation_y(std::f32::consts::PI),
                arrival::HOME,
            ),
            ..senses()
        };
        let frame = step.update(&turned, &mut conversation, &mut felt(), now, FRAME);
        let lazily = frame
            .panels
            .pill
            .model
            .unwrap()
            .transform_point3(Vec3::ZERO);
        assert!(lazily.z < arrival::HOME.z, "{lazily}");
        step.recentre();
        let frame = step.update(&turned, &mut conversation, &mut felt(), now, FRAME);
        let placed = frame
            .panels
            .pill
            .model
            .unwrap()
            .transform_point3(Vec3::ZERO);
        assert!(placed.z > arrival::HOME.z + 1.0, "{placed}");
    }

    #[test]
    fn the_transcript_takes_the_right_stick_a_b_and_the_grip_while_it_shows() {
        let mut conversation = approval_waiting();
        let mut step = Step::default();
        let start = Instant::now();
        let open = Senses {
            right_palm: Some(palm_up(0.0)),
            ..senses()
        };
        assert!(
            wrist_after(&mut step, &mut conversation, &open, start)
                .model
                .is_some()
        );
        let mut feedback = felt();
        let clock = std::cell::Cell::new(start + HOLD);
        let tap = |step: &mut Step,
                   conversation: &mut Conversation,
                   feedback: &mut Feedback,
                   buttons: Buttons| {
            let now = clock.get() + Duration::from_millis(50);
            clock.set(now);
            step.buttons(&buttons, true, conversation, feedback, now);
            step.buttons(&Buttons::default(), true, conversation, feedback, now);
            now
        };
        // The stick moves the focus, which ticks on the right hand, instead of paging result cards.
        let moved = tap(
            &mut step,
            &mut conversation,
            &mut feedback,
            press(|buttons| buttons.right_stick = Vec2::new(0.0, -1.0)),
        );
        assert!(!felt_on(&mut feedback, Side::Right, moved).is_empty());
        assert_eq!(step.presentation_navigation.card, 0);
        // A expands the agent's turn, which is heard.
        let mut conversation = conversed();
        let mut step = Step::default();
        assert!(
            wrist_after(&mut step, &mut conversation, &open, start)
                .model
                .is_some()
        );
        tap(
            &mut step,
            &mut conversation,
            &mut feedback,
            press(|buttons| buttons.right_stick = Vec2::new(0.0, 1.0)),
        );
        // Sideways, it jumps between turns.
        let turn = conversation.wrist_focus().cloned();
        assert!(turn.is_some());
        tap(
            &mut step,
            &mut conversation,
            &mut feedback,
            press(|buttons| buttons.right_stick = Vec2::new(-1.0, 0.0)),
        );
        assert_ne!(conversation.wrist_focus().cloned(), turn);
        tap(
            &mut step,
            &mut conversation,
            &mut feedback,
            press(|buttons| buttons.right_stick = Vec2::new(1.0, 0.0)),
        );
        assert_eq!(conversation.wrist_focus().cloned(), turn);
        earcons(&mut feedback);
        tap(
            &mut step,
            &mut conversation,
            &mut feedback,
            press(|buttons| buttons.a = true),
        );
        assert_eq!(earcons(&mut feedback), [Earcon::Choose]);
        // The grip detaches it where it stands; with no head to follow, it stays.
        let detach = Senses {
            grip: Some(1.0),
            ..open
        };
        let now = clock.get();
        let frame = step.update(&detach, &mut conversation, &mut feedback, now, FRAME);
        let model = frame.panels.wrist.model;
        assert!(step.wrist.state().detached());
        let headless = Senses {
            head: None,
            grip: Some(0.0),
            right_palm: None,
            ..open
        };
        let frame = step.update(&headless, &mut conversation, &mut feedback, now, FRAME);
        assert_eq!(frame.panels.wrist.model, model);
        // B backs out of the expanded turn first, then returns it to the wrist, rather than backing out of the space.
        tap(
            &mut step,
            &mut conversation,
            &mut feedback,
            press(|buttons| buttons.back = true),
        );
        assert!(step.wrist.state().detached());
        tap(
            &mut step,
            &mut conversation,
            &mut feedback,
            press(|buttons| buttons.back = true),
        );
        assert!(!step.wrist.state().detached());
        assert!(conversation.joined_space().is_some());
    }

    #[test]
    fn the_ray_and_a_choose_only_in_the_hand_menu_while_it_shows() {
        use crate::hand_menu::MenuKey;
        let start = Instant::now();
        let menu = Senses {
            left_palm: Some(left_palm_up(0.0)),
            ..senses()
        };
        // Y pins the menu open at its Space section, whose rows choose at once.
        let open = |conversation: &mut Conversation, feedback: &mut Feedback| {
            let mut step = Step::default();
            step.buttons(
                &press(|buttons| buttons.y = true),
                true,
                conversation,
                feedback,
                start,
            );
            let frame = step.update(&menu, conversation, feedback, start, FRAME);
            let scene = frame.arrival.expect("the pinned menu").scene;
            (step, scene)
        };
        let mut conversation = joined();
        let mut feedback = felt();
        let (mut step, scene) = open(&mut conversation, &mut feedback);
        let mute = arrival::Key::Menu(MenuKey::Mute);
        let aiming = Senses {
            ray: aim_at(target(&scene, &mute).shape),
            beam: true,
            ..menu
        };
        let frame = step.update(&aiming, &mut conversation, &mut feedback, start, FRAME);
        let arrival = frame.arrival.expect("the menu");
        assert_eq!(arrival.hovered, Some(mute.clone()));
        assert!(arrival.ray.is_some_and(|ray| ray.hit));
        // Pointing at the menu counts as pointing, which keeps the transcript from showing.
        assert!(step.pointing(None));
        earcons(&mut feedback);
        // A chooses the row the ray is on.
        let muted = conversation.menu_model().muted;
        step.buttons(
            &press(|buttons| buttons.a = true),
            true,
            &mut conversation,
            &mut feedback,
            start,
        );
        assert_eq!(earcons(&mut feedback), [Earcon::Choose]);
        assert_ne!(conversation.menu_model().muted, muted);
        // As does the trigger, which never attracts while the menu shows.
        let mut conversation = joined();
        let (mut step, _) = open(&mut conversation, &mut feedback);
        step.update(&aiming, &mut conversation, &mut feedback, start, FRAME);
        let pull = Senses {
            trigger: Some(1.0),
            ..aiming
        };
        let frame = step.update(&pull, &mut conversation, &mut feedback, start, FRAME);
        assert!(!frame.attracting);
        assert_eq!(earcons(&mut feedback), [Earcon::Choose]);
        assert_ne!(conversation.menu_model().muted, muted);
        // The trigger on a section opens it, which is heard too.
        let mut conversation = joined();
        let mut step = Step::default();
        step.update(&menu, &mut conversation, &mut feedback, start, FRAME);
        let frame = step.update(&menu, &mut conversation, &mut feedback, start + HOLD, FRAME);
        let section = arrival::Key::Menu(MenuKey::Section(hand_menu::Section::Sound));
        let aiming = Senses {
            ray: aim_at(target(&frame.arrival.expect("the menu").scene, &section).shape),
            ..menu
        };
        step.update(
            &aiming,
            &mut conversation,
            &mut feedback,
            start + HOLD,
            FRAME,
        );
        let pull = Senses {
            trigger: Some(1.0),
            ..aiming
        };
        let frame = step.update(&pull, &mut conversation, &mut feedback, start + HOLD, FRAME);
        assert_eq!(earcons(&mut feedback), [Earcon::Choose]);
        let rows = frame.arrival.expect("the menu").scene;
        assert!(
            rows.object(&arrival::Key::Menu(MenuKey::Sounds)).is_some(),
            "the Sound section is open"
        );
    }

    /// Hold the ray on the menu's row `key`, as `menu` shows it, for a second of frames, through which it stays on that row; then pull the trigger, which chooses it, and let go.
    fn hold_and_choose(
        step: &mut Step,
        conversation: &mut Conversation,
        menu: &Senses,
        key: hand_menu::MenuKey,
        now: Instant,
    ) {
        let key = arrival::Key::Menu(key);
        let mut feedback = felt();
        let frame = step.update(menu, conversation, &mut feedback, now, FRAME);
        let scene = frame.arrival.expect("the menu").scene;
        let aiming = Senses {
            ray: aim_at(target(&scene, &key).shape),
            ..*menu
        };
        for frame in 0..72 {
            let hovered = step
                .update(&aiming, conversation, &mut feedback, now, FRAME)
                .arrival
                .expect("the menu")
                .hovered;
            assert_eq!(hovered.as_ref(), Some(&key), "frame {frame}");
        }
        let pull = Senses {
            trigger: Some(1.0),
            ..aiming
        };
        step.update(&pull, conversation, &mut feedback, now, FRAME);
        assert_eq!(earcons(&mut feedback), [Earcon::Choose], "{key:?}");
        step.update(&aiming, conversation, &mut feedback, now, FRAME);
    }

    #[test]
    fn a_steady_ray_stays_on_a_menu_row_and_chooses_it() {
        use crate::hand_menu::{MenuKey, Section};
        use tomori_client::{ConnectionState, Event};
        use tomori_protocol::{ClientMessage, EnvironmentInfo, Mode};
        // An owner, in a space whose agent has three modes, each described, the middle one current, on a server that changes environments.
        let mut conversation = lobby(ConnectionRole::Owner);
        conversation.apply(Event::State(ConnectionState::Connected {
            source: 1,
            server: "studio".into(),
            role: ConnectionRole::Owner,
            certificate: None,
            backends: vec![],
            roots: vec![],
            environments: [("sky", "Sky"), ("minimal", "Grid")]
                .map(|(id, label)| EnvironmentInfo {
                    id: id.into(),
                    label: label.into(),
                    version: 1,
                })
                .to_vec(),
            utterance_limit: None,
            name: None,
        }));
        conversation.apply(space(|state| {
            state.modes = [
                (
                    "plan",
                    "Plans without changing files, and says how it would.",
                ),
                ("build", "Makes changes."),
                ("review", "Reviews the changes made so far."),
            ]
            .map(|(id, description)| Mode {
                id: id.into(),
                name: id.into(),
                description: Some(description.into()),
            })
            .to_vec();
            state.mode = Some("build".into());
            state.speech = tomori_protocol::SpeechSetting::Brief;
            state.environment_changes = true;
        }));
        conversation.take_actions();
        let start = Instant::now();
        let menu = Senses {
            left_palm: Some(left_palm_up(0.0)),
            ..senses()
        };
        let mut step = Step::default();
        step.buttons(
            &press(|buttons| buttons.y = true),
            true,
            &mut conversation,
            &mut felt(),
            start,
        );
        // The modes above and below the current one, each held for a second, stay highlighted and are chosen; pointing at one shows its description beneath the modes without moving a row.
        let sent = |conversation: &mut Conversation, message: ClientMessage| {
            assert_eq!(conversation.take_actions(), [Action::Send(message)]);
        };
        for mode in ["plan", "review"] {
            hold_and_choose(
                &mut step,
                &mut conversation,
                &menu,
                MenuKey::Mode(mode.into()),
                start,
            );
            sent(
                &mut conversation,
                ClientMessage::SetMode { mode: mode.into() },
            );
        }
        // As do the environment the space is not in, and the space's other settings.
        hold_and_choose(
            &mut step,
            &mut conversation,
            &menu,
            MenuKey::Environment("minimal".into()),
            start,
        );
        sent(
            &mut conversation,
            ClientMessage::SetEnvironment {
                environment: "minimal".into(),
            },
        );
        hold_and_choose(&mut step, &mut conversation, &menu, MenuKey::Speech, start);
        sent(
            &mut conversation,
            ClientMessage::SetSpeech {
                speech: tomori_protocol::SpeechSetting::Full,
            },
        );
        // The Sound section's levels, stepped one after another.
        hold_and_choose(&mut step, &mut conversation, &menu, MenuKey::Back, start);
        hold_and_choose(
            &mut step,
            &mut conversation,
            &menu,
            MenuKey::Section(Section::Sound),
            start,
        );
        let feedback = conversation.menu_model().feedback;
        for key in [MenuKey::Sounds, MenuKey::Sounds, MenuKey::Vibration] {
            hold_and_choose(&mut step, &mut conversation, &menu, key, start);
        }
        let stepped = conversation.menu_model().feedback;
        assert_eq!(stepped.sounds, feedback.sounds.next().next());
        assert_eq!(stepped.vibration, feedback.vibration.next());
    }

    #[test]
    fn while_the_menu_shows_in_arrival_the_ray_meets_only_its_rows() {
        let mut conversation = Conversation::new("Ada");
        conversation.set_servers(vec![tomori_client::server_choice::Choice::new(
            "Studio",
            "10.0.0.1:8765",
            tomori_client::server_choice::Source::Discovered,
        )]);
        let mut step = Step::default();
        let now = Instant::now();
        step.update(&senses(), &mut conversation, &mut felt(), now, FRAME);
        let frame = step.update(&senses(), &mut conversation, &mut felt(), now, 5.0);
        let server = target(
            &frame.arrival.expect("arrival").scene,
            &arrival::Key::Server("10.0.0.1:8765".into()),
        );
        let menu = Senses {
            left_palm: Some(left_palm_up(0.0)),
            ray: aim_at(server.shape),
            ..senses()
        };
        step.update(&menu, &mut conversation, &mut felt(), now, FRAME);
        let frame = step.update(&menu, &mut conversation, &mut felt(), now + HOLD, FRAME);
        let hovered = frame.arrival.expect("arrival and the menu").hovered;
        assert!(
            !matches!(hovered, Some(arrival::Key::Server(_))),
            "{hovered:?}"
        );
        // Nor does A choose the server behind it.
        step.buttons(
            &press(|buttons| buttons.a = true),
            true,
            &mut conversation,
            &mut felt(),
            now + HOLD,
        );
        assert!(conversation.take_actions().is_empty());
    }

    #[test]
    fn a_with_nothing_to_choose_is_not_heard() {
        let mut conversation = joined();
        let mut feedback = felt();
        Step::default().buttons(
            &press(|buttons| buttons.a = true),
            true,
            &mut conversation,
            &mut feedback,
            Instant::now(),
        );
        assert!(earcons(&mut feedback).is_empty());
    }

    #[test]
    fn in_a_space_the_beam_reaches_out_and_ends_short_of_nothing() {
        let mut conversation = joined();
        let mut step = Step::default();
        let ray = Ray::new(arrival::HOME, -Vec3::Z);
        let pointing = Senses {
            ray,
            beam: true,
            ..senses()
        };
        let frame = step.update(
            &pointing,
            &mut conversation,
            &mut felt(),
            Instant::now(),
            FRAME,
        );
        let beam = frame
            .arrival
            .and_then(|arrival| arrival.ray)
            .expect("the beam");
        assert!(!beam.hit);
        assert_eq!(beam.end, ray.unwrap().at(crate::pointing::SPACE_RAY_LENGTH));
    }

    #[test]
    fn the_recording_indicator_and_the_live_badge_show_on_the_left() {
        let mut conversation = joined();
        let mut step = Step::default();
        let now = Instant::now();
        let grip = Pose {
            position: [-0.2, 1.1, -0.35],
            orientation: [0.0, 0.0, 0.0, 1.0],
        };
        let senses = Senses {
            trackers: Trackers {
                grip: Some(grip),
                ..Default::default()
            },
            ..senses()
        };
        let frame = step.update(&senses, &mut conversation, &mut felt(), now, FRAME);
        assert_eq!(frame.panels.indicator, [Sheet::default(), Sheet::default()]);
        conversation.toggle_live();
        assert!(conversation.live());
        let frame = step.update(&senses, &mut conversation, &mut felt(), now, FRAME);
        let [card, badge] = &frame.panels.indicator;
        assert_eq!(card, &Sheet::default());
        assert!(
            badge.canvas.is_some() && badge.model.is_some(),
            "the live badge"
        );
        conversation.stop_live();
        step.buttons(
            &press(|buttons| buttons.talk = true),
            true,
            &mut conversation,
            &mut felt(),
            now,
        );
        let frame = step.update(&senses, &mut conversation, &mut felt(), now, FRAME);
        let [card, badge] = &frame.panels.indicator;
        assert!(card.canvas.is_some(), "the recording card");
        // It rises from the left controller.
        let at = card.model.expect("drawn").transform_point3(Vec3::ZERO);
        assert!(at.distance(Vec3::from_array(grip.position)) < 0.2, "{at}");
        assert!(at.y > grip.position[1]);
        assert_eq!(badge, &Sheet::default());
        // With the live microphone on too, only the card shows.
        conversation.toggle_live();
        let frame = step.update(&senses, &mut conversation, &mut felt(), now, FRAME);
        let [card, badge] = &frame.panels.indicator;
        assert!(card.model.is_some());
        assert_eq!(badge, &Sheet::default());
    }
}
