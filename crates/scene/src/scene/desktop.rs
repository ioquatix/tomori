// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

//! The desktop window's own logic as a participant: the keyboard and mouse as the controller's buttons and ray, a head at standing height that the mouse turns, the palms held up in view while their panels are open, the window's focus, and knocks announced in the title. `desktop_participant.rs` turns winit's events into these calls and draws what they return. See `design/desktop.md`.
use std::time::Instant;

use glam::{EulerRot, Mat4, Quat, Vec2, Vec3};
use tomori_client::engagement::{Focus, Tracker};

use super::{Buttons, Frame, Marker, Senses, Step};
use crate::{
    arrival, controller::Pose, conversation::Conversation, hand_menu::Palm, pointing::Ray,
    recording::Trackers,
};
use tomori_client::feedback::Feedback;

/// The window's title: its name, then a short hint of the mouse's controls, which `desktop_participant.rs`'s `CONTROLS` gives in full; a knock waiting comes between them (see [`Drawn::title`]). The hint stays, since a title bar costs nothing.
pub const TITLE: &str = "Tomori | mouse: point · left choose · right-drag look · M menu · Esc quit";
/// The window's name, which starts [`TITLE`].
const NAME: &str = "Tomori";
/// The hint of the mouse's controls, which ends [`TITLE`].
const HINT: &str = "mouse: point · left choose · right-drag look · M menu · Esc quit";
/// How far the head turns, in radians, for each pixel the mouse is dragged.
const LOOK_SPEED: f32 = 0.004;
/// The longest step, in seconds, so that a window hidden for a while does not jump.
const MAX_STEP: f32 = 0.1;
/// How many rows of a panel a notch of the mouse's wheel scrolls.
pub const WHEEL_ROWS: f32 = 3.0;
/// How many pixels of a touchpad's scroll make a notch.
pub const WHEEL_PIXELS: f32 = 40.0;
/// How far along the mouse's ray the swarm is attracted to: about arm's length further than a controller would be.
const ATTRACT_AT: f32 = 1.2;

/// A key that stands for one of the headset's controls; `desktop_participant.rs` binds them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Key {
    /// X, held: push-to-talk.
    Talk,
    /// B; held for a second, it leaves the space.
    Back,
    A,
    Y,
    /// Mute spoken replies, as the hand menu's row does; the headset's left stick click toggles the live microphone instead.
    Mute,
    /// The right stick click.
    Replay,
    /// The left stick: up, down, left and right.
    Left(Direction),
    /// The right stick.
    Right(Direction),
    /// Open or close the hand menu.
    Menu,
    /// Show or hide the transcript on the right wrist.
    Transcript,
    /// The right grip, held.
    Grip,
    Knock,
    Pair,
    /// Admit the first knock waiting.
    Admit,
    /// Decline the first knock waiting.
    Decline,
    /// Close the hand menu, or quit.
    Escape,
}

/// A stick's direction.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Direction {
    Up,
    Down,
    Left,
    Right,
}

/// What a key did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeyResult {
    Handled,
    Quit,
}

/// The mouse's buttons that act.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Button {
    /// The trigger: choose what is pointed at, or attract the swarm.
    Left,
    /// Drag to look around.
    Right,
}

/// The keyboard and mouse as the headset's controls.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct Input {
    cursor: Option<Vec2>,
    trigger: bool,
    looking: bool,
    talk: bool,
    back_held: bool,
    /// A, held.
    a_held: bool,
    /// Presses since the last frame, which it consumes.
    a: bool,
    back: bool,
    y: bool,
    mute: bool,
    replay: bool,
    left: [bool; 4],
    right: [bool; 4],
    /// The right grip, held.
    grip: bool,
    /// Rows the wheel scrolled since the last frame, which it consumes.
    scroll: f32,
}

impl Input {
    /// Release everything held, as on losing focus.
    fn release(&mut self) {
        let cursor = self.cursor;
        *self = Self {
            cursor,
            ..Self::default()
        };
    }

    fn stick(keys: [bool; 4]) -> Vec2 {
        let [up, down, left, right] = keys.map(f32::from);
        Vec2::new(right - left, up - down)
    }

    /// This frame's buttons, consuming the presses since the last.
    fn take(&mut self) -> Buttons {
        Buttons {
            a: std::mem::take(&mut self.a),
            a_held: self.a_held,
            back: std::mem::take(&mut self.back),
            back_held: self.back_held,
            y: std::mem::take(&mut self.y),
            talk: self.talk,
            mute: std::mem::take(&mut self.mute),
            replay: std::mem::take(&mut self.replay),
            left_stick: Self::stick(self.left),
            right_stick: Self::stick(self.right),
            scroll: std::mem::take(&mut self.scroll),
        }
    }
}

/// The participant's head, standing at arrival's home and turned by the mouse.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Head {
    yaw: f32,
    pitch: f32,
}

impl Default for Head {
    fn default() -> Self {
        // Arrival is laid out ahead, along -Z, and a little below the eyes.
        Self {
            yaw: 0.0,
            pitch: -0.12,
        }
    }
}

impl Head {
    fn look(&mut self, delta: Vec2) {
        self.yaw -= delta.x * LOOK_SPEED;
        self.pitch = (self.pitch - delta.y * LOOK_SPEED).clamp(-1.4, 1.4);
    }

    fn rotation(&self) -> Quat {
        Quat::from_euler(EulerRot::YXZ, self.yaw, self.pitch, 0.0)
    }

    /// The head's pose in the scene, as the headset's views give it.
    pub fn transform(&self) -> Mat4 {
        Mat4::from_rotation_translation(self.rotation(), arrival::HOME)
    }

    pub fn view_projection(&self, aspect: f32) -> Mat4 {
        crate::depth::perspective(60_f32.to_radians(), aspect) * self.transform().inverse()
    }

    /// The ray from the eye through the cursor, as the right controller's aim on the headset.
    fn ray(&self, cursor: Vec2, size: Vec2) -> Option<Ray> {
        let ndc = Vec2::new(cursor.x / size.x * 2.0 - 1.0, 1.0 - cursor.y / size.y * 2.0);
        let inverse = self.view_projection(size.x / size.y).inverse();
        let near = inverse.project_point3(ndc.extend(0.0));
        let far = inverse.project_point3(ndc.extend(1.0));
        Ray::new(near, far - near)
    }

    /// A right palm turned up in front of the participant, where the transcript shows while it is open.
    fn right_palm(&self) -> Palm {
        let wrist = arrival::HOME + self.rotation() * Vec3::new(0.05, -0.2, -0.42);
        Palm {
            wrist,
            normal: Vec3::Y,
        }
    }

    /// A left palm turned up in front of the participant, as the gesture that shows the hand menu turns it, where the menu shows while it is open.
    fn palm(&self) -> Palm {
        let wrist = arrival::HOME + self.rotation() * Vec3::new(-0.06, -0.2, -0.45);
        Palm {
            wrist,
            normal: Vec3::Y,
        }
    }
}

/// What a desktop frame draws and reports.
pub struct Drawn<'a> {
    pub frame: Frame<'a>,
    /// The window's new title, when the knocks waiting change: [`TITLE`], or, with a knock waiting, the window's name, the first knock, which needs attention, and then the hint of the controls.
    pub title: Option<String>,
    /// The head's view and projection, in the participant's room.
    pub view_projection: Mat4,
    /// Takes the space's scene to the room, where the participant has travelled; see `design/locomotion.md`.
    pub scene_to_space: Mat4,
}

/// The conversation's participant in a window, and everything the frame step keeps between frames, without the headset.
pub struct Desktop {
    step: Step,
    input: Input,
    head: Head,
    focused: bool,
    visible: bool,
    engagement: Tracker,
    microphone_requested: bool,
    menu_open: bool,
    /// Whether the right wrist's transcript is turned up, with T.
    wrist_open: bool,
    knocks: usize,
    last_frame: Instant,
}

impl Desktop {
    pub fn new(conversation: &mut Conversation, now: Instant) -> Self {
        let engagement = Tracker::new();
        conversation.set_engagement(engagement.engagement());
        Self {
            // The hint beside a panel carried names the keys that stand for the controls.
            step: Step {
                controls: crate::projection::Controls::Keyboard,
                ..Step::default()
            },
            input: Input::default(),
            head: Head::default(),
            focused: false,
            visible: true,
            engagement,
            microphone_requested: false,
            menu_open: false,
            wrist_open: false,
            knocks: 0,
            last_frame: now,
        }
    }

    pub fn focused(&self) -> bool {
        self.focused
    }

    /// The window gained or lost focus. Returns whether to ask for the microphone, which happens once, on first gaining focus.
    pub fn set_focused(&mut self, focused: bool, conversation: &mut Conversation) -> bool {
        if self.focused && !focused {
            // As the headset does on losing focus: nothing held carries over.
            self.input.release();
            self.step.blur(conversation);
        }
        self.focused = focused;
        let request = focused && !self.microphone_requested;
        self.microphone_requested |= request;
        self.report_engagement(conversation);
        request
    }

    /// The window was shown or hidden.
    pub fn set_visible(&mut self, visible: bool, conversation: &mut Conversation) {
        self.visible = visible;
        self.report_engagement(conversation);
    }

    /// A focused window has input, an unfocused one is only seen, and a hidden one is neither; a desktop has no sensor for being worn.
    fn report_engagement(&mut self, conversation: &mut Conversation) {
        let focus = match (self.visible, self.focused) {
            (false, _) => Focus::Hidden,
            (true, true) => Focus::Focused,
            (true, false) => Focus::Visible,
        };
        if let Some(changed) = self.engagement.set_focus(focus) {
            log::info!("Engagement: {changed:?}, as the window is {focus:?}");
            conversation.set_engagement(changed);
        }
    }

    /// The cursor left the window: it points at nothing, and a drag ends.
    pub fn cursor_left(&mut self) {
        self.input.cursor = None;
        self.input.looking = false;
    }

    /// The cursor moved to `cursor`, in pixels; dragging with the right button turns the head.
    pub fn cursor_moved(&mut self, cursor: Vec2) {
        if self.input.looking
            && let Some(last) = self.input.cursor
        {
            self.head.look(cursor - last);
        }
        self.input.cursor = Some(cursor);
    }

    /// The mouse's wheel turned `lines` notches, up where more than none, as winit counts them: it scrolls the panel the
    /// mouse points at by [`WHEEL_ROWS`] rows a notch, down for a turn towards the participant, or pushes an
    /// environment's object that takes the right stick as the stick would to scroll a panel as far. See
    /// `design/desktop.md`.
    pub fn wheel(&mut self, lines: f32) {
        if lines.is_finite() {
            self.input.scroll -= lines * WHEEL_ROWS;
        }
    }

    pub fn mouse(&mut self, button: Button, pressed: bool) {
        match button {
            Button::Left => self.input.trigger = pressed,
            Button::Right => self.input.looking = pressed,
        }
    }

    /// `key` was pressed or released, `repeat` if the key is held and repeating.
    pub fn key(
        &mut self,
        key: Key,
        pressed: bool,
        repeat: bool,
        conversation: &mut Conversation,
    ) -> KeyResult {
        let press = pressed && !repeat;
        let input = &mut self.input;
        let index = |direction| match direction {
            Direction::Up => 0,
            Direction::Down => 1,
            Direction::Left => 2,
            Direction::Right => 3,
        };
        match key {
            Key::Talk => input.talk = pressed,
            Key::Back => {
                input.back_held = pressed;
                input.back |= press;
            }
            Key::A => {
                input.a_held = pressed;
                input.a |= press;
            }
            Key::Y => input.y |= press,
            Key::Mute => input.mute |= press,
            Key::Replay => input.replay |= press,
            Key::Left(direction) => input.left[index(direction)] = pressed,
            Key::Right(direction) => input.right[index(direction)] = pressed,
            Key::Grip => input.grip = pressed,
            _ if !press => {}
            Key::Menu => self.menu_open = !self.menu_open,
            Key::Transcript => self.wrist_open = !self.wrist_open,
            Key::Knock => conversation.knock(),
            Key::Pair => conversation.pair(),
            Key::Admit | Key::Decline => {
                match conversation.knocks().first().map(|knock| knock.id.clone()) {
                    Some(id) => conversation.decide_knock(&id, key == Key::Admit),
                    None => log::info!("No knock is waiting."),
                }
            }
            // Esc closes the hand menu, then lets go of a pinned one, before it quits.
            Key::Escape => {
                if self.menu_open {
                    self.menu_open = false;
                } else if !self.step.unpin() {
                    return KeyResult::Quit;
                }
            }
        }
        KeyResult::Handled
    }

    /// One frame at `now`, in a window `size` pixels large: the controller's buttons from the keys, then the scene, as the headset's frame, from the keyboard and mouse instead of the controllers.
    pub fn frame(
        &mut self,
        conversation: &mut Conversation,
        feedback: &mut Feedback,
        now: Instant,
        size: Vec2,
    ) -> Drawn<'_> {
        let dt = now
            .duration_since(self.last_frame)
            .as_secs_f32()
            .min(MAX_STEP);
        self.last_frame = now;
        let focused = self.focused;
        let buttons = self.input.take();
        // As on the headset, approvals are decided and queued entries removed on the right wrist.
        conversation.decide_on_wrist();
        self.step
            .buttons(&buttons, focused, conversation, feedback, now);
        // Knocks are announced in the title as well, since there is no wrist to cue while the menu is closed.
        let knocks = conversation.knocks().len();
        let title = (knocks != self.knocks).then(|| {
            self.knocks = knocks;
            for knock in conversation.knocks() {
                log::info!(
                    "{}{} is knocking: + admits, - declines.",
                    knock.name,
                    crate::hand_menu::VISITOR
                );
            }
            match conversation.knocks().first() {
                Some(knock) => {
                    format!(
                        "{NAME} — {}{} is knocking: + admits, - declines | {HINT}",
                        knock.name,
                        crate::hand_menu::VISITOR
                    )
                }
                None => TITLE.to_owned(),
            }
        });
        // The head stands in the participant's room, which travel takes into the space's scene, as the headset's reference space; see `design/locomotion.md`.
        let rig = self.step.rig(conversation).matrix();
        let room_head = self.head.transform();
        let head = rig * room_head;
        let ray = self
            .input
            .cursor
            .filter(|_| focused)
            .and_then(|cursor| self.head.ray(cursor, size))
            .and_then(|ray| {
                Ray::new(
                    rig.transform_point3(ray.origin),
                    rig.transform_vector3(ray.direction),
                )
            });
        let travelled = |palm: Palm| Palm {
            wrist: rig.transform_point3(palm.wrist),
            normal: rig.transform_vector3(palm.normal),
        };
        let senses = Senses {
            focused,
            head: Some(head),
            head_pose: Pose::from_transform(head),
            // The head turns about the eye, so what faces the head faces the eye.
            viewer: Some(rig.transform_point3(arrival::HOME)),
            space_head: room_head,
            ray,
            trigger: focused.then_some(if self.input.trigger { 1.0 } else { 0.0 }),
            // The mouse is the right hand; the window has no left ray.
            left_ray: None,
            left_trigger: None,
            left_grip: None,
            left_attractor: None,
            // The arrow keys aim a teleport where the mouse points.
            teleport: crate::sides::Side::Right,
            pointing: self.input.trigger,
            left_palm: (self.menu_open || self.step.pinned()).then(|| travelled(self.head.palm())),
            right_palm: self.wrist_open.then(|| travelled(self.head.right_palm())),
            hands_only: false,
            grip: Some(if self.input.grip { 1.0 } else { 0.0 }),
            // A desktop has no controllers, so the recording indicator stands low in the view.
            trackers: Trackers {
                view: Some(crate::controller_ui::view_spot(head)),
                ..Default::default()
            },
            attractor: ray.map(|ray| ray.at(ATTRACT_AT)),
            marker: Marker::WhileAttracting,
            // The others see this participant's head; a desktop has no controllers or hands to report.
            controllers: [None, None],
            hands: [None, None],
            // There are no palms to cue on, and the mouse cursor already shows where it points, so no ray is drawn from the eye.
            cues: false,
            beam: false,
        };
        Drawn {
            frame: self.step.update(&senses, conversation, feedback, now, dt),
            title,
            view_projection: self.head.view_projection(size.x / size.y),
            scene_to_space: rig.inverse(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{hand_menu::HOLD, scene::fixtures::*};
    use std::time::Duration;
    use tomori_client::connection::Action;
    use tomori_protocol::{ClientMessage, ConnectionRole, Engagement};

    const SIZE: Vec2 = Vec2::new(1280.0, 800.0);

    #[test]
    fn the_head_starts_at_home_looking_towards_arrival() {
        let transform = Head::default().transform();
        assert!(transform.w_axis.truncate().distance(arrival::HOME) < 1e-6);
        let forward = transform.transform_vector3(-Vec3::Z);
        // Ahead, along -Z, where arrival's objects are, and a little downwards.
        assert!(forward.z < -0.9);
        assert!(forward.y < 0.0);
    }

    #[test]
    fn the_centre_of_the_window_points_where_the_head_looks() {
        let head = Head {
            yaw: 0.7,
            pitch: 0.2,
        };
        let ray = head.ray(SIZE * 0.5, SIZE).unwrap();
        let forward = head.transform().transform_vector3(-Vec3::Z);
        assert!(ray.direction.dot(forward) > 0.999);
        // From the eye, give or take the near plane.
        assert!(ray.origin.distance(arrival::HOME) < 0.1);
    }

    #[test]
    fn looking_turns_the_head_and_never_past_straight_up_or_down() {
        let mut head = Head::default();
        head.look(Vec2::new(100.0, 0.0));
        assert!(head.yaw < 0.0, "dragging right turns right");
        head.look(Vec2::new(0.0, -100_000.0));
        assert_eq!(head.pitch, 1.4);
        head.look(Vec2::new(0.0, 100_000.0));
        assert_eq!(head.pitch, -1.4);
    }

    #[test]
    fn the_palms_are_in_view_and_both_turned_up() {
        let head = Head {
            yaw: -1.2,
            pitch: 0.3,
        };
        let palm = head.palm();
        assert_eq!(palm.normal, Vec3::Y);
        let right = head.right_palm();
        assert_eq!(right.normal, Vec3::Y);
        // Within the 60° vertical field of view, so the menu shows in the window.
        for wrist in [palm.wrist, right.wrist] {
            let clip = head.view_projection(SIZE.x / SIZE.y).project_point3(wrist);
            assert!(clip.x.abs() < 1.0 && clip.y.abs() < 1.0 && (0.0..1.0).contains(&clip.z));
        }
    }

    #[test]
    fn arrow_keys_and_brackets_are_sticks() {
        assert_eq!(Input::stick([true, false, false, false]), Vec2::Y);
        assert_eq!(Input::stick([false, false, false, true]), Vec2::X);
        assert_eq!(Input::stick([true, true, true, true]), Vec2::ZERO);
    }

    fn desktop(conversation: &mut Conversation) -> Desktop {
        let mut desktop = Desktop::new(conversation, Instant::now());
        assert!(
            desktop.set_focused(true, conversation),
            "asks for the microphone"
        );
        desktop
    }

    fn tap(desktop: &mut Desktop, key: Key, conversation: &mut Conversation) -> KeyResult {
        let result = desktop.key(key, true, false, conversation);
        desktop.key(key, false, false, conversation);
        result
    }

    #[test]
    fn the_wheel_scrolls_by_rows_down_for_a_turn_towards_the_participant() {
        let mut conversation = joined();
        let mut desktop = desktop(&mut conversation);
        // A notch away from the participant scrolls up three rows; a turn of two towards them, down six; together, once.
        desktop.wheel(1.0);
        assert_eq!(desktop.input.take().scroll, -WHEEL_ROWS);
        desktop.wheel(-1.0);
        desktop.wheel(-1.0);
        assert_eq!(desktop.input.take().scroll, 2.0 * WHEEL_ROWS);
        assert_eq!(desktop.input.take().scroll, 0.0);
        // Nonsense from the platform scrolls nothing.
        desktop.wheel(f32::NAN);
        desktop.wheel(f32::INFINITY);
        assert_eq!(desktop.input.take().scroll, 0.0);
    }

    #[test]
    fn keys_press_once_hold_while_down_and_ignore_repeats() {
        let mut conversation = joined();
        let mut desktop = desktop(&mut conversation);
        for key in [Key::Y, Key::Mute, Key::Replay] {
            desktop.key(key, true, true, &mut conversation);
        }
        assert_eq!(
            desktop.input.take(),
            Buttons::default(),
            "repeats are not presses"
        );
        // A repeat of a key held is still held, and no new press of A.
        desktop.key(Key::A, true, true, &mut conversation);
        assert_eq!(
            desktop.input.take(),
            Buttons {
                a_held: true,
                ..Buttons::default()
            }
        );
        desktop.key(Key::A, false, false, &mut conversation);
        assert_eq!(desktop.input.take(), Buttons::default());
        desktop.key(Key::Back, true, false, &mut conversation);
        for key in [Key::A, Key::Y, Key::Mute, Key::Replay, Key::Talk, Key::Grip] {
            desktop.key(key, true, false, &mut conversation);
        }
        for direction in [Direction::Up, Direction::Right] {
            desktop.key(Key::Left(direction), true, false, &mut conversation);
            desktop.key(Key::Right(direction), true, false, &mut conversation);
        }
        let expected = Buttons {
            a: true,
            a_held: true,
            back: true,
            back_held: true,
            y: true,
            talk: true,
            mute: true,
            replay: true,
            left_stick: Vec2::new(1.0, 1.0),
            right_stick: Vec2::new(1.0, 1.0),
            scroll: 0.0,
        };
        assert_eq!(desktop.input.take(), expected);
        assert!(desktop.input.grip);
        // Presses are consumed by a frame; what is held stays.
        assert_eq!(
            desktop.input.take(),
            Buttons {
                a: false,
                back: false,
                y: false,
                mute: false,
                replay: false,
                ..expected
            }
        );
        for direction in [Direction::Down, Direction::Left] {
            desktop.key(Key::Left(direction), true, false, &mut conversation);
        }
        assert_eq!(desktop.input.take().left_stick, Vec2::ZERO);
        // Releasing a key lets go of what it holds.
        for key in [
            Key::A,
            Key::Talk,
            Key::Back,
            Key::Grip,
            Key::Left(Direction::Up),
            Key::Left(Direction::Down),
            Key::Left(Direction::Left),
            Key::Left(Direction::Right),
            Key::Right(Direction::Up),
            Key::Right(Direction::Right),
        ] {
            desktop.key(key, false, false, &mut conversation);
        }
        assert_eq!(desktop.input.take(), Buttons::default());
        assert!(!desktop.input.grip);
        // Losing focus lets go of everything but where the cursor is.
        desktop.cursor_moved(Vec2::new(10.0, 20.0));
        desktop.set_focused(false, &mut conversation);
        assert_eq!(
            desktop.input,
            Input {
                cursor: Some(Vec2::new(10.0, 20.0)),
                ..Input::default()
            }
        );
    }

    #[test]
    fn escape_closes_the_menu_then_lets_go_of_a_pinned_one_then_quits() {
        let mut conversation = joined();
        let mut desktop = desktop(&mut conversation);
        tap(&mut desktop, Key::Menu, &mut conversation);
        assert!(desktop.menu_open);
        assert_eq!(
            tap(&mut desktop, Key::Escape, &mut conversation),
            KeyResult::Handled
        );
        assert!(!desktop.menu_open);
        tap(&mut desktop, Key::Y, &mut conversation);
        desktop.frame(&mut conversation, &mut felt(), Instant::now(), SIZE);
        assert!(desktop.step.pinned());
        assert_eq!(
            tap(&mut desktop, Key::Escape, &mut conversation),
            KeyResult::Handled
        );
        assert!(!desktop.step.pinned());
        assert_eq!(
            tap(&mut desktop, Key::Escape, &mut conversation),
            KeyResult::Quit
        );
        // Releasing Escape does nothing.
        assert_eq!(
            desktop.key(Key::Escape, false, false, &mut conversation),
            KeyResult::Handled
        );
    }

    #[test]
    fn the_title_names_the_window_then_hints_at_the_mouse_controls_in_a_title_bars_width() {
        assert_eq!(TITLE, format!("{NAME} | {HINT}"));
        for control in [
            "mouse: point",
            "left choose",
            "right-drag look",
            "M menu",
            "Esc quit",
        ] {
            assert!(HINT.contains(control), "{control}");
        }
        assert!(TITLE.chars().count() <= 80);
    }

    #[test]
    fn knocks_are_admitted_or_declined_first_come_and_announced_in_the_title() {
        crate::test_support::log_everything();
        let mut conversation = joined();
        let mut desktop = desktop(&mut conversation);
        let now = Instant::now();
        tap(&mut desktop, Key::Admit, &mut conversation);
        assert!(conversation.take_actions().is_empty(), "no knock waiting");
        assert!(
            desktop
                .frame(&mut conversation, &mut felt(), now, SIZE)
                .title
                .is_none()
        );
        knock(&mut conversation, "knock-1", "Grace");
        let title = desktop
            .frame(&mut conversation, &mut felt(), now, SIZE)
            .title;
        assert_eq!(
            title.as_deref(),
            Some(
                "Tomori — Grace (visitor) is knocking: + admits, - declines | mouse: point · left choose · right-drag look · M menu · Esc quit"
            )
        );
        assert!(
            desktop
                .frame(&mut conversation, &mut felt(), now, SIZE)
                .title
                .is_none(),
            "once"
        );
        for (key, admit) in [(Key::Admit, true), (Key::Decline, false)] {
            tap(&mut desktop, key, &mut conversation);
            let sent = conversation.take_actions();
            assert!(
                sent.iter().any(|action| matches!(
                    action,
                    Action::Send(ClientMessage::DecideKnock { knock, admit: a, .. }) if knock == "knock-1" && *a == admit
                )),
                "{sent:?}"
            );
        }
        conversation.apply(tomori_client::Event::Message(
            tomori_protocol::ServerMessage::Knocks { knocks: vec![] },
        ));
        assert_eq!(
            desktop
                .frame(&mut conversation, &mut felt(), now, SIZE)
                .title
                .as_deref(),
            Some(TITLE)
        );
        // P pairs this device as an owner, which it already is, so nothing is sent.
        tap(&mut desktop, Key::Pair, &mut conversation);
        assert!(conversation.take_actions().is_empty());
        // A visitor watching a space knocks with K.
        let mut visitor = lobby(ConnectionRole::Visitor);
        visitor.apply(space(|_| {}));
        visitor.take_actions();
        let mut desktop = Desktop::new(&mut visitor, now);
        tap(&mut desktop, Key::Knock, &mut visitor);
        let sent = visitor.take_actions();
        assert!(
            sent.iter()
                .any(|action| matches!(action, Action::Send(ClientMessage::Knock))),
            "{sent:?}"
        );
    }

    #[test]
    fn focus_and_visibility_report_engagement_and_the_microphone_is_asked_for_once() {
        let mut conversation = joined();
        let mut desktop = Desktop::new(&mut conversation, Instant::now());
        assert!(!desktop.focused());
        assert!(desktop.set_focused(true, &mut conversation));
        assert_eq!(desktop.engagement.engagement(), Engagement::Engaged);
        assert!(!desktop.set_focused(false, &mut conversation));
        assert_eq!(desktop.engagement.engagement(), Engagement::Distracted);
        desktop.set_visible(false, &mut conversation);
        assert_eq!(desktop.engagement.engagement(), Engagement::Away);
        assert!(!desktop.set_focused(true, &mut conversation));
    }

    #[test]
    fn the_mouse_points_only_with_focus_and_dragging_looks() {
        let mut conversation = lobby(ConnectionRole::Owner);
        let mut desktop = desktop(&mut conversation);
        let now = Instant::now();
        desktop.cursor_moved(SIZE * 0.5);
        desktop.mouse(Button::Left, true);
        let drawn = desktop.frame(&mut conversation, &mut felt(), now, SIZE);
        assert!(drawn.frame.attracting, "a pull on nothing attracts");
        // The attractor is along the ray, and drawn only while it attracts.
        let marker = drawn.frame.marker.expect("drawn");
        assert!((marker.distance(arrival::HOME) - ATTRACT_AT).abs() < 0.1);
        desktop.mouse(Button::Left, false);
        let drawn = desktop.frame(&mut conversation, &mut felt(), now, SIZE);
        assert_eq!(drawn.frame.marker, None);
        // Dragging with the right button turns the head.
        desktop.mouse(Button::Right, true);
        desktop.cursor_moved(SIZE * 0.5 + Vec2::new(100.0, 0.0));
        assert!(desktop.head.yaw < 0.0);
        desktop.mouse(Button::Right, false);
        let yaw = desktop.head.yaw;
        desktop.cursor_moved(SIZE * 0.5);
        assert_eq!(
            desktop.head.yaw, yaw,
            "moving without the button only points"
        );
        desktop.cursor_left();
        assert!(desktop.input.cursor.is_none());
        desktop.mouse(Button::Right, true);
        desktop.cursor_moved(SIZE * 0.5);
        assert_eq!(
            desktop.head.yaw, yaw,
            "a drag starting outside does not jump"
        );
        // Unfocused, the trigger does nothing.
        desktop.mouse(Button::Left, true);
        desktop.set_focused(false, &mut conversation);
        desktop.mouse(Button::Left, true);
        let drawn = desktop.frame(&mut conversation, &mut felt(), now, SIZE);
        assert!(!drawn.frame.attracting);
    }

    #[test]
    fn m_holds_up_the_left_palm_and_t_turns_up_the_right() {
        let mut conversation = joined();
        let mut desktop = desktop(&mut conversation);
        let start = Instant::now();
        tap(&mut desktop, Key::Menu, &mut conversation);
        desktop.frame(&mut conversation, &mut felt(), start, SIZE);
        let drawn = desktop.frame(&mut conversation, &mut felt(), start + HOLD, SIZE);
        assert!(
            drawn
                .frame
                .arrival
                .is_some_and(|arrival| !arrival.scene.objects.is_empty())
        );
        tap(&mut desktop, Key::Menu, &mut conversation);
        tap(&mut desktop, Key::Transcript, &mut conversation);
        desktop.frame(&mut conversation, &mut felt(), start + HOLD * 2, SIZE);
        let drawn = desktop.frame(&mut conversation, &mut felt(), start + HOLD * 3, SIZE);
        assert!(drawn.frame.panels.wrist.model.is_some());
        // G detaches it into the world.
        desktop.key(Key::Grip, true, false, &mut conversation);
        desktop.frame(&mut conversation, &mut felt(), start + HOLD * 4, SIZE);
        assert!(desktop.step.wrist.state().detached());
    }

    #[test]
    fn a_frame_steps_at_most_a_tenth_of_a_second_and_draws_from_the_head() {
        let mut conversation = joined();
        let start = Instant::now();
        let mut desktop = Desktop::new(&mut conversation, start);
        let drawn = desktop.frame(
            &mut conversation,
            &mut felt(),
            start + Duration::from_secs(5),
            SIZE,
        );
        assert_eq!(
            drawn.view_projection,
            Head::default().view_projection(SIZE.x / SIZE.y)
        );
        assert_eq!(
            drawn.frame.listener,
            Pose::from_transform(Head::default().transform())
        );
        let presence = drawn.frame.presence.expect("a participant");
        assert_eq!((presence.left, presence.right), (None, None));
        assert!((desktop.step.arrival_time - MAX_STEP).abs() < 1e-6);
    }

    #[test]
    fn the_arrow_keys_teleport_where_the_mouse_points_and_turn() {
        use crate::locomotion::{Locomotion, Surface};
        let mut conversation = joined();
        conversation.set_locomotion(vec![(
            crate::test_support::SKY,
            Locomotion::teleport(vec![Surface::rectangle(Vec3::ZERO, Vec2::splat(20.0))]),
        )]);
        let mut desktop = desktop(&mut conversation);
        // Travel waits for the space's scenery to grow in, so that its surfaces are seen.
        let mut start = Instant::now();
        for _ in 0..40 {
            start += Duration::from_millis(100);
            desktop.frame(&mut conversation, &mut felt(), start, SIZE);
        }
        // Low in the window, the mouse points at the floor a little ahead.
        desktop.cursor_moved(Vec2::new(SIZE.x * 0.5, SIZE.y * 0.95));
        desktop.key(Key::Left(Direction::Up), true, false, &mut conversation);
        let drawn = desktop.frame(&mut conversation, &mut felt(), start, SIZE);
        let landing = drawn.frame.landing.expect("somewhere to land");
        let pill = drawn.frame.panels.pill.model.expect("the pill");
        assert!(landing.y == 0.0 && landing.z < -0.5 && landing.x.abs() < 1e-3);
        // The window draws the ring but no ray, and here nothing else of arrival.
        assert!(drawn.frame.arrival.is_none());
        desktop.key(Key::Left(Direction::Up), false, false, &mut conversation);
        desktop.frame(&mut conversation, &mut felt(), start + HOLD, SIZE);
        let drawn = desktop.frame(&mut conversation, &mut felt(), start + HOLD * 2, SIZE);
        let head = drawn.frame.presence.unwrap().head;
        let travelled = Vec3::new(landing.x, arrival::HOME.y, landing.z);
        assert!(Vec3::from_array(head.position).distance(travelled) < 1e-4);
        assert_eq!(drawn.frame.listener, Some(head));
        // The pill stands in the participant's room, so it came along.
        assert_eq!(drawn.frame.panels.pill.model, Some(pill));
        // The window looks from there.
        let ahead = travelled + Head::default().transform().transform_vector3(-Vec3::Z);
        let seen = (drawn.view_projection * drawn.scene_to_space).project_point3(ahead);
        assert!(seen.x.abs() < 1e-3 && seen.y.abs() < 1e-3);
        // Left turns 30° about the head, and the hand menu turns with it.
        desktop.key(Key::Left(Direction::Left), true, false, &mut conversation);
        desktop.frame(&mut conversation, &mut felt(), start + HOLD * 3, SIZE);
        desktop.key(Key::Left(Direction::Left), false, false, &mut conversation);
        let drawn = desktop.frame(&mut conversation, &mut felt(), start + HOLD * 4, SIZE);
        let head = drawn.frame.presence.unwrap().head;
        assert!(Vec3::from_array(head.position).distance(travelled) < 1e-4);
        let turned = Quat::from_array(head.orientation).angle_between(Head::default().rotation());
        assert!((turned - crate::locomotion::SNAP_TURN).abs() < 1e-3);
        tap(&mut desktop, Key::Menu, &mut conversation);
        let palm = desktop
            .step
            .rig(&conversation)
            .matrix()
            .transform_point3(desktop.head.palm().wrist);
        desktop.frame(&mut conversation, &mut felt(), start + HOLD * 5, SIZE);
        let drawn = desktop.frame(&mut conversation, &mut felt(), start + HOLD * 6, SIZE);
        let menu = drawn.frame.arrival.expect("the menu");
        assert!(
            menu.scene
                .objects
                .iter()
                .any(|object| object.position.distance(palm) < 0.5)
        );
    }

    #[test]
    fn the_right_stick_s_keys_turn_and_teleport_too_with_the_mouse_on_nothing() {
        use crate::locomotion::{Locomotion, Surface};
        let mut conversation = joined();
        conversation.set_locomotion(vec![(
            crate::test_support::SKY,
            Locomotion::teleport(vec![Surface::rectangle(Vec3::ZERO, Vec2::splat(20.0))]),
        )]);
        let mut desktop = desktop(&mut conversation);
        let mut start = Instant::now();
        for _ in 0..40 {
            start += Duration::from_millis(100);
            desktop.frame(&mut conversation, &mut felt(), start, SIZE);
        }
        // With the mouse on nothing, ] turns 30° about the head, as → does.
        desktop.cursor_moved(Vec2::new(SIZE.x * 0.5, SIZE.y * 0.3));
        desktop.frame(&mut conversation, &mut felt(), start, SIZE);
        desktop.key(Key::Right(Direction::Right), true, false, &mut conversation);
        desktop.frame(&mut conversation, &mut felt(), start + HOLD, SIZE);
        desktop.key(
            Key::Right(Direction::Right),
            false,
            false,
            &mut conversation,
        );
        let drawn = desktop.frame(&mut conversation, &mut felt(), start + HOLD * 2, SIZE);
        let head = drawn.frame.presence.unwrap().head;
        let turned = Quat::from_array(head.orientation).angle_between(Head::default().rotation());
        assert!(
            (turned - crate::locomotion::SNAP_TURN).abs() < 1e-3,
            "{turned}"
        );
        // [ turns back; Page Up, low in the window, aims where the mouse points, and lands as it is released.
        desktop.key(Key::Right(Direction::Left), true, false, &mut conversation);
        desktop.frame(&mut conversation, &mut felt(), start + HOLD * 3, SIZE);
        desktop.key(Key::Right(Direction::Left), false, false, &mut conversation);
        desktop.frame(&mut conversation, &mut felt(), start + HOLD * 3, SIZE);
        desktop.cursor_moved(Vec2::new(SIZE.x * 0.5, SIZE.y * 0.95));
        desktop.key(Key::Right(Direction::Up), true, false, &mut conversation);
        let drawn = desktop.frame(&mut conversation, &mut felt(), start + HOLD * 4, SIZE);
        let landing = drawn.frame.landing.expect("somewhere to land");
        desktop.key(Key::Right(Direction::Up), false, false, &mut conversation);
        desktop.frame(&mut conversation, &mut felt(), start + HOLD * 5, SIZE);
        let drawn = desktop.frame(&mut conversation, &mut felt(), start + HOLD * 6, SIZE);
        let head = drawn.frame.presence.unwrap().head;
        let travelled = Vec3::new(landing.x, arrival::HOME.y, landing.z);
        assert!(Vec3::from_array(head.position).distance(travelled) < 1e-4);
        let turned = Quat::from_array(head.orientation).angle_between(Head::default().rotation());
        assert!(turned < 1e-3, "{turned}");
    }

    #[test]
    fn a_click_or_enter_on_a_pad_the_mouse_points_at_takes_the_participant_to_its_partner() {
        use crate::locomotion::{Locomotion, Pad, Surface};
        let forward = Head::default()
            .transform()
            .transform_vector3(-Vec3::Z)
            .with_y(0.0)
            .normalize();
        let pad = forward * 2.0;
        let racks = pad + Vec3::new(0.0, -4.5, 0.0);
        for click in [true, false] {
            let mut conversation = joined();
            conversation.set_locomotion(vec![(
                crate::test_support::SKY,
                Locomotion::teleport(vec![
                    Surface::rectangle(Vec3::ZERO, Vec2::splat(20.0)),
                    Surface::rectangle(Vec3::new(0.0, -4.5, 0.0), Vec2::splat(20.0)),
                ])
                .with_pads([[Pad::at(pad), Pad::at(racks).named("Racks")]]),
            )]);
            let mut desktop = desktop(&mut conversation);
            let mut start = Instant::now();
            for _ in 0..40 {
                start += Duration::from_millis(100);
                desktop.frame(&mut conversation, &mut felt(), start, SIZE);
            }
            // Down the middle of the window, the mouse comes to the pad, whose label names the click.
            let on_pad = (0..400).find(|row| {
                desktop.cursor_moved(Vec2::new(SIZE.x * 0.5, SIZE.y * 0.5 + *row as f32));
                let drawn = desktop.frame(&mut conversation, &mut felt(), start, SIZE);
                drawn.frame.pads.first().is_some_and(|pad| pad.highlighted)
                    && drawn.frame.projection.iter().any(|sheet| {
                        sheet.canvas.as_deref().unwrap().plain() == "Racks ↓\nClick  go"
                    })
            });
            assert!(on_pad.is_some(), "the mouse meets the pad");
            // A click, or Enter, as A, takes them down to the racks.
            if click {
                desktop.mouse(Button::Left, true);
                desktop.frame(&mut conversation, &mut felt(), start, SIZE);
                desktop.mouse(Button::Left, false);
            } else {
                tap(&mut desktop, Key::A, &mut conversation);
                desktop.frame(&mut conversation, &mut felt(), start, SIZE);
            }
            let drawn = desktop.frame(&mut conversation, &mut felt(), start + HOLD, SIZE);
            let head = Vec3::from_array(drawn.frame.presence.unwrap().head.position);
            assert!(
                head.distance(racks + Vec3::Y * arrival::HOME.y) < 1e-3,
                "{click} {head}"
            );
        }
    }

    #[test]
    fn holding_b_after_travelling_keeps_the_environment_where_the_window_saw_it_as_the_swarm_flies_back()
     {
        use crate::locomotion::{Locomotion, Surface};
        let mut conversation = lobby(ConnectionRole::Owner);
        conversation.set_locomotion(vec![(
            crate::test_support::SKY,
            Locomotion::teleport(vec![Surface::rectangle(Vec3::ZERO, Vec2::splat(40.0))]),
        )]);
        // Participants arrive 8 m along +X, facing the environment's origin, and the agent has its own home.
        conversation.apply(space(|state| {
            state.anchors = tomori_protocol::Anchors::new()
                .with_arrival(tomori_protocol::Pose::at([8.0, 0.0, 0.0]).facing_towards([0.0; 3]))
                .with_agent(
                    tomori_protocol::Pose::at([1.0, 1.4, -3.0]).facing_towards([0.0, 1.6, 0.0]),
                );
        }));
        let mut desktop = desktop(&mut conversation);
        let mut now = Instant::now();
        for _ in 0..40 {
            now += Duration::from_millis(100);
            desktop.frame(&mut conversation, &mut felt(), now, SIZE);
        }
        // Teleported a little ahead.
        desktop.cursor_moved(Vec2::new(SIZE.x * 0.5, SIZE.y * 0.95));
        desktop.key(Key::Left(Direction::Up), true, false, &mut conversation);
        desktop.frame(&mut conversation, &mut felt(), now, SIZE);
        desktop.key(Key::Left(Direction::Up), false, false, &mut conversation);
        desktop.frame(&mut conversation, &mut felt(), now, SIZE);
        // What the window shows, in the participant's room: the environment's carriage and the swarm's agents.
        let seen = |drawn: &Drawn<'_>| {
            let carriage = drawn.scene_to_space
                * Mat4::from_rotation_translation(
                    drawn.frame.carriage.orientation,
                    drawn.frame.carriage.position,
                );
            let agents: Vec<Vec3> = drawn
                .frame
                .swarm
                .agents
                .iter()
                .map(|agent| drawn.scene_to_space.transform_point3(agent.position))
                .collect();
            (carriage, agents)
        };
        // None of the swarm's agents jumps: each flies at most as far as it streams in the longest step it takes.
        let furthest = |before: &[Vec3], after: &[Vec3]| {
            before
                .iter()
                .zip(after)
                .map(|(before, after)| before.distance(*after))
                .fold(0.0f32, f32::max)
        };
        let leap = crate::glide::STREAM / 30.0 + 1e-4;
        let drawn = desktop.frame(&mut conversation, &mut felt(), now, SIZE);
        assert_ne!(drawn.scene_to_space, Mat4::IDENTITY, "travelled");
        let (carriage, mut agents) = seen(&drawn);
        // B held for a second leaves, and the environment shrinks away as the rig goes home, with nothing jumping: it
        // stays where it was seen, and the swarm flies back to the lobby's spot from where it was seen.
        desktop.key(Key::Back, true, false, &mut conversation);
        desktop.frame(&mut conversation, &mut felt(), now, SIZE);
        let mut shrinking = 0;
        loop {
            now += Duration::from_millis(50);
            let drawn = desktop.frame(&mut conversation, &mut felt(), now, SIZE);
            let (now_carriage, now_agents) = seen(&drawn);
            assert!(furthest(&agents, &now_agents) <= leap, "{shrinking}");
            agents = now_agents;
            if conversation.joined_space().is_some() {
                continue;
            }
            assert_eq!(drawn.frame.swarm.anchor(), crate::anchors::Anchor::AGENT);
            if drawn.frame.scenery.environment == crate::environment::Environment::MINIMAL {
                break;
            }
            shrinking += 1;
            assert!(drawn.frame.swarm.flying(), "{shrinking}");
            assert_eq!(drawn.scene_to_space, Mat4::IDENTITY, "home");
            assert!(now_carriage.abs_diff_eq(carriage, 1e-4), "{shrinking}");
        }
        assert!(shrinking > 5, "{shrinking}");
        desktop.key(Key::Back, false, false, &mut conversation);
    }

    #[test]
    fn enter_ticks_or_unticks_the_checklist_item_the_mouse_points_at_as_a_does() {
        let mut conversation = lobby(ConnectionRole::Owner);
        let ahead = arrival::HOME + Head::default().transform().transform_vector3(-Vec3::Z) * 2.0;
        show_checklist(&mut conversation, ahead, arrival::HOME);
        let mut desktop = desktop(&mut conversation);
        let now = Instant::now();
        // Down the middle of the window, the mouse comes to the ticked item, whose label names Enter, A's key.
        let on_item = (0..400).find(|row| {
            desktop.cursor_moved(Vec2::new(SIZE.x * 0.5, SIZE.y * 0.25 + *row as f32));
            let drawn = desktop.frame(&mut conversation, &mut felt(), now, SIZE);
            label_hint(&drawn.frame.projection).as_deref() == Some("Enter  untick")
        });
        assert!(on_item.is_some(), "the mouse meets the ticked item");
        // The left button is still the trigger, which attracts the swarm and ticks nothing.
        desktop.mouse(Button::Left, true);
        desktop.frame(&mut conversation, &mut felt(), now, SIZE);
        desktop.mouse(Button::Left, false);
        desktop.frame(&mut conversation, &mut felt(), now, SIZE);
        assert!(conversation.take_actions().is_empty());
        // Enter unticks it, for everyone.
        tap(&mut desktop, Key::A, &mut conversation);
        desktop.frame(&mut conversation, &mut felt(), now, SIZE);
        assert_eq!(
            conversation.take_actions(),
            [Action::Send(ClientMessage::CheckPanelItem {
                panel: "panel-1".into(),
                line: 2,
                checked: false,
                revision: Some(4),
            })]
        );
        let drawn = desktop.frame(&mut conversation, &mut felt(), now, SIZE);
        assert_eq!(
            label_hint(&drawn.frame.projection).as_deref(),
            Some("Enter  tick")
        );
    }

    #[test]
    fn enter_or_a_click_chooses_the_row_the_mouse_points_at_as_a_and_the_trigger_do() {
        let mut conversation = lobby(ConnectionRole::Owner);
        let ahead = arrival::HOME + Head::default().transform().transform_vector3(-Vec3::Z) * 2.0;
        show_queue(&mut conversation, ahead, arrival::HOME);
        let mut desktop = desktop(&mut conversation);
        let now = Instant::now();
        let choose = |row: &str| {
            Action::Send(ClientMessage::ChoosePanelRow {
                panel: "panel-2".into(),
                row: row.into(),
                revision: Some(6),
                key: None,
            })
        };
        // Down the middle of the window, the mouse comes to the first choosable row, whose label names Enter.
        let on_row = (0..400).find(|row| {
            desktop.cursor_moved(Vec2::new(SIZE.x * 0.5, SIZE.y * 0.25 + *row as f32));
            let drawn = desktop.frame(&mut conversation, &mut felt(), now, SIZE);
            label_hint(&drawn.frame.projection).as_deref() == Some("Enter  choose")
        });
        assert!(on_row.is_some(), "the mouse meets a choosable row");
        // Enter chooses it.
        tap(&mut desktop, Key::A, &mut conversation);
        desktop.frame(&mut conversation, &mut felt(), now, SIZE);
        assert_eq!(conversation.take_actions(), [choose("pod112")]);
        // So does a click, the left button being the trigger.
        desktop.mouse(Button::Left, true);
        desktop.frame(&mut conversation, &mut felt(), now, SIZE);
        desktop.mouse(Button::Left, false);
        desktop.frame(&mut conversation, &mut felt(), now, SIZE);
        assert_eq!(conversation.take_actions(), [choose("pod112")]);
    }

    #[test]
    fn enter_or_a_click_on_a_copy_s_time_chip_freezes_it_as_a_and_the_trigger_do() {
        let mut conversation = lobby(ConnectionRole::Owner);
        let ahead = arrival::HOME + Head::default().transform().transform_vector3(-Vec3::Z) * 1.2;
        show_copy(
            &mut conversation,
            ahead,
            arrival::HOME,
            tomori_protocol::context::ChipPress::Freeze,
        );
        let mut desktop = desktop(&mut conversation);
        let now = Instant::now();
        let freeze = Action::Send(ClientMessage::FreezePanel {
            panel: "panel-4".into(),
            frozen: true,
            revision: Some(6),
        });
        // Over the upper part of the window, the mouse comes to the copy's time chip, whose label names Enter.
        let on_chip = (0..160).find_map(|step| {
            let (column, row) = (step % 16, step / 16);
            let at = Vec2::new(
                SIZE.x * (0.3 + 0.025 * column as f32),
                SIZE.y * (0.2 + 0.03 * row as f32),
            );
            desktop.cursor_moved(at);
            let drawn = desktop.frame(&mut conversation, &mut felt(), now, SIZE);
            (titled_label_hint(&drawn.frame.projection, "Latency").as_deref()
                == Some("Enter  freeze"))
            .then_some(at)
        });
        assert!(on_chip.is_some(), "the mouse meets the time chip");
        // Enter freezes it, as seen.
        tap(&mut desktop, Key::A, &mut conversation);
        desktop.frame(&mut conversation, &mut felt(), now, SIZE);
        assert_eq!(conversation.take_actions(), std::slice::from_ref(&freeze));
        // So does a click, the left button being the trigger.
        desktop.mouse(Button::Left, true);
        desktop.frame(&mut conversation, &mut felt(), now, SIZE);
        desktop.mouse(Button::Left, false);
        desktop.frame(&mut conversation, &mut felt(), now, SIZE);
        assert_eq!(conversation.take_actions(), [freeze]);
        // Frozen, its chip unfreezes it, as its label says.
        show_copy(
            &mut conversation,
            ahead,
            arrival::HOME,
            tomori_protocol::context::ChipPress::Unfreeze,
        );
        let drawn = desktop.frame(&mut conversation, &mut felt(), now, SIZE);
        assert_eq!(
            titled_label_hint(&drawn.frame.projection, "Latency").as_deref(),
            Some("Enter  unfreeze")
        );
    }

    #[test]
    fn a_panel_carried_with_g_docks_on_a_wall_only_once_page_up_pushes_it_there() {
        use tomori_protocol::{Grid, Panel, Place, Pose as Placed, Wall, WallKind};
        let mut conversation = lobby(ConnectionRole::Owner);
        // A panel 1.5 m ahead, where the middle of the window points, and a wall of three slots 8.3 m ahead, behind it.
        let forward = Head::default().transform().transform_vector3(-Vec3::Z);
        let ahead = arrival::HOME + forward * 1.5;
        let behind = arrival::HOME + forward * 8.3;
        let wall = Wall::new(
            "deck",
            WallKind::Added,
            Placed::upright_facing(behind.to_array(), arrival::HOME.to_array()),
        )
        .with_grid(Grid::new(3, 1).unwrap());
        let panel = Panel::new(
            "panel-1",
            "Notes",
            "A note.",
            Place::Free {
                pose: Placed::upright_facing(ahead.to_array(), arrival::HOME.to_array()),
            },
        );
        show_projection(&mut conversation, vec![wall], vec![panel]);
        let mut desktop = desktop(&mut conversation);
        let mut now = Instant::now();
        let mut frame = |desktop: &mut Desktop, conversation: &mut Conversation| {
            now += Duration::from_millis(100);
            desktop.frame(conversation, &mut felt(), now, SIZE);
        };
        desktop.cursor_moved(SIZE * 0.5);
        frame(&mut desktop, &mut conversation);
        // Held with G and let go, with the mouse on the wall behind it, it floats where it is.
        desktop.key(Key::Grip, true, false, &mut conversation);
        frame(&mut desktop, &mut conversation);
        assert_eq!(desktop.step.projector.held(), Some("panel-1"));
        desktop.key(Key::Grip, false, false, &mut conversation);
        frame(&mut desktop, &mut conversation);
        let floated = conversation.take_actions();
        assert!(
            matches!(
                floated.as_slice(),
                [Action::Send(ClientMessage::PlacePanel {
                    wall: None,
                    slot: None,
                    pose: Some(_),
                    ..
                })]
            ),
            "{floated:?}"
        );
        // Held again and pushed with Page Up as far as it goes, within reach of the wall, it goes into the slot under it.
        desktop.key(Key::Grip, true, false, &mut conversation);
        frame(&mut desktop, &mut conversation);
        assert_eq!(desktop.step.projector.held(), Some("panel-1"));
        desktop.key(Key::Right(Direction::Up), true, false, &mut conversation);
        for _ in 0..30 {
            frame(&mut desktop, &mut conversation);
        }
        desktop.key(Key::Right(Direction::Up), false, false, &mut conversation);
        frame(&mut desktop, &mut conversation);
        desktop.key(Key::Grip, false, false, &mut conversation);
        frame(&mut desktop, &mut conversation);
        assert_eq!(
            conversation.take_actions(),
            [Action::Send(ClientMessage::PlacePanel {
                panel: "panel-1".into(),
                wall: Some("deck".into()),
                slot: Some(1),
                pose: None,
                size: None,
                revision: None,
            })]
        );
    }

    #[test]
    fn page_up_and_down_and_the_brackets_reach_an_environment_with_nothing_carried() {
        let (mut conversation, done) = crate::scene::tests::lamp(Default::default(), false);
        let mut desktop = desktop(&mut conversation);
        let mut now = Instant::now();
        for _ in 0..40 {
            now += Duration::from_millis(100);
            desktop.frame(&mut conversation, &mut felt(), now, SIZE);
        }
        assert!(desktop.step.client.is_some());
        for (key, stick) in [
            (Direction::Up, Vec2::Y),
            (Direction::Down, Vec2::NEG_Y),
            (Direction::Left, Vec2::NEG_X),
            (Direction::Right, Vec2::X),
        ] {
            desktop.key(Key::Right(key), true, false, &mut conversation);
            now += Duration::from_millis(16);
            desktop.frame(&mut conversation, &mut felt(), now, SIZE);
            assert_eq!(done.lock().unwrap().last().unwrap().3, stick, "{key:?}");
            desktop.key(Key::Right(key), false, false, &mut conversation);
            now += Duration::from_millis(16);
            desktop.frame(&mut conversation, &mut felt(), now, SIZE);
        }
    }

    #[test]
    fn enter_selects_the_selectable_object_the_mouse_points_at_as_a_does_and_never_beside_the_left_button()
     {
        use crate::client_side::Select;
        let (mut conversation, done) = crate::scene::tests::lamp(Default::default(), true);
        let mut desktop = desktop(&mut conversation);
        // Down the middle of the window, the mouse points at the lamp, which is selectable.
        desktop.cursor_moved(SIZE * 0.5);
        let mut now = Instant::now();
        let mut frame = |desktop: &mut Desktop, conversation: &mut Conversation| {
            now += Duration::from_millis(100);
            desktop.frame(conversation, &mut felt(), now, SIZE);
            done.lock()
                .unwrap()
                .drain(..)
                .filter_map(|told| told.0)
                .collect::<Vec<_>>()
        };
        for _ in 0..40 {
            frame(&mut desktop, &mut conversation);
        }
        assert!(desktop.step.client.is_some());
        let lamp = || "lamp".to_owned();
        // Held across frames, Enter presses once, and lets go as it is let go, wherever the mouse is then.
        desktop.key(Key::A, true, false, &mut conversation);
        assert_eq!(
            frame(&mut desktop, &mut conversation),
            [Select::Pressed(lamp())]
        );
        desktop.key(Key::A, true, true, &mut conversation);
        assert!(frame(&mut desktop, &mut conversation).is_empty());
        desktop.cursor_left();
        desktop.key(Key::A, false, false, &mut conversation);
        assert_eq!(
            frame(&mut desktop, &mut conversation),
            [Select::Released(lamp())]
        );
        assert!(frame(&mut desktop, &mut conversation).is_empty());
        desktop.cursor_moved(SIZE * 0.5);
        frame(&mut desktop, &mut conversation);
        // Struck and let go between two frames, it presses on the one and lets go on the next.
        tap(&mut desktop, Key::A, &mut conversation);
        assert_eq!(
            frame(&mut desktop, &mut conversation),
            [Select::Pressed(lamp())]
        );
        assert_eq!(
            frame(&mut desktop, &mut conversation),
            [Select::Released(lamp())]
        );
        // The left button, the trigger, keeps the select it began: Enter meanwhile selects nothing more.
        desktop.mouse(Button::Left, true);
        assert_eq!(
            frame(&mut desktop, &mut conversation),
            [Select::Pressed(lamp())]
        );
        tap(&mut desktop, Key::A, &mut conversation);
        assert!(frame(&mut desktop, &mut conversation).is_empty());
        desktop.mouse(Button::Left, false);
        assert_eq!(
            frame(&mut desktop, &mut conversation),
            [Select::Released(lamp())]
        );
        // Losing the window's focus lets go, once.
        desktop.key(Key::A, true, false, &mut conversation);
        assert_eq!(
            frame(&mut desktop, &mut conversation),
            [Select::Pressed(lamp())]
        );
        desktop.set_focused(false, &mut conversation);
        assert_eq!(
            frame(&mut desktop, &mut conversation),
            [Select::Released(lamp())]
        );
        assert!(frame(&mut desktop, &mut conversation).is_empty());
    }
}
