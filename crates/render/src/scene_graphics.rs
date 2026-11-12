// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

//! Drawing what the frame step decided: [`draw`] applies a [`Frame`] to the renderer, and [`Panels`] holds the shared panels' GPU buffers, for both the headset and the desktop window.
use std::collections::HashMap;
use std::sync::Arc;

use glam::Mat4;

use crate::{
    arrival_graphics,
    controller_ui::{CAPACITY, Sheet},
    graphics::Renderer,
    scene::{self, Frame},
    ui_panel::Panel,
};

/// The shared panels' GPU buffers, with where each is drawn: the status pill, the recording indicator and the `LIVE` badge, and the right wrist's transcript.
pub struct Panels {
    pill: Panel,
    card: Panel,
    badge: Panel,
    wrist: Panel,
    /// The pill's model in the eyes' space, while it is drawn.
    pill_model: Option<Mat4>,
    /// The card's, the badge's and the transcript's models in the scene, while they are drawn.
    models: [Option<Mat4>; 3],
    /// The space's projection walls and panels, then the GitHub environment's inbox and cards, one panel each, kept with
    /// its mesh for as long as the frame step hands over the same face, wherever it is drawn among the others, and then
    /// a few more, the latest shown, so that their buffers need not be made again; see `design/rendering.md`.
    cards: Vec<Card>,
    /// Which of them show each face, by its canvas's address, which each holds, so that no other face has it: as many
    /// as draw it at once, as each of a curved wall's slots draws its one board.
    faces: HashMap<usize, Vec<usize>>,
    /// Those drawn now, in order, with their models.
    drawn: Vec<(usize, Mat4)>,
    /// How many times cards have been set, the current one's number.
    frame: u64,
}

/// A projection's panel on the GPU, the face it shows, by its canvas's address, and the last time it was drawn.
struct Card {
    panel: Panel,
    face: usize,
    shown: u64,
}

/// The meshes of the panels that [`draw`] built afresh, and those whose frame alone it recoloured.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Uploads {
    pub built: usize,
    pub recoloured: usize,
}

impl Uploads {
    /// Both together: every mesh written.
    pub fn meshes(&self) -> usize {
        self.built + self.recoloured
    }

    /// What `panels` built and recoloured since they had `before`.
    fn since<'a>(before: (usize, usize), panels: impl IntoIterator<Item = &'a Panel>) -> Self {
        let (built, recoloured) = builds(panels);
        Self {
            built: built - before.0,
            recoloured: recoloured - before.1,
        }
    }
}

impl std::ops::Add for Uploads {
    type Output = Self;

    fn add(self, other: Self) -> Self {
        Self {
            built: self.built + other.built,
            recoloured: self.recoloured + other.recoloured,
        }
    }
}

/// How many times each of `panels` built its mesh and recoloured its frame, together.
fn builds<'a>(panels: impl IntoIterator<Item = &'a Panel>) -> (usize, usize) {
    panels
        .into_iter()
        .fold((0, 0), |(built, recoloured), panel| {
            (built + panel.builds.0, recoloured + panel.builds.1)
        })
}

/// How many cards not drawn now are kept, the latest drawn first, for faces to come; beyond them, the GPU's buffers of
/// panels removed are freed.
pub const SPARE_CARDS: usize = 8;

/// The most vertices one card's face takes, about three times what a full face of text does: every wall's and floating
/// panel's, a chart panel's at its largest included (see `design/projection.md`).
pub const CARD_CAPACITY: usize = CAPACITY / 2;

/// The most vertices one card's buffer grows to hold, for a face larger than [`CARD_CAPACITY`]: four times as many, so
/// that a wall's content drawn at half its size, about four times as many cells in each slot, is drawn whole, a tall
/// slot's code included (see `design/projection.md`).
pub const CARD_MOST: usize = 4 * CARD_CAPACITY;

impl Panels {
    pub fn new(device: &wgpu::Device, renderer: &Renderer) -> Self {
        Self {
            pill: renderer.panel(device, CAPACITY),
            card: renderer.panel(device, CAPACITY),
            badge: renderer.panel(device, CAPACITY / 8),
            wrist: renderer.panel(device, CAPACITY * 4),
            pill_model: None,
            models: [None; 3],
            cards: Vec::new(),
            faces: HashMap::new(),
            drawn: Vec::new(),
            frame: 0,
        }
    }

    /// Upload the projection's walls and panels and the inbox and cards, `sheets`, in the order to draw them, making
    /// panels for them as needed, and say what it built.
    ///
    /// A face handed over again, the very canvas, is drawn by a card that showed it, its mesh as it was, wherever it now
    /// is in the order, by as many as draw it at once. Any other is shown by the card drawn where it is in the order last
    /// frame, if that card is free, which keeps its mesh if the face is the same, rewrites only its frame if that is all
    /// that changed, and otherwise builds it again; or else by the free card drawn longest ago, or a new one.
    pub fn set_cards<'a>(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        renderer: &Renderer,
        sheets: impl IntoIterator<Item = &'a Sheet>,
    ) -> Uploads {
        let before = builds(self.cards.iter().map(|card| &card.panel));
        self.frame += 1;
        let frame = self.frame;
        let drawn = std::mem::take(&mut self.drawn);
        let sheets: Vec<(&Arc<tomori_scene::canvas::Canvas>, Mat4)> = sheets
            .into_iter()
            .filter_map(|sheet| sheet.canvas.as_ref().zip(sheet.model))
            .collect();
        // Faces kept, first, so that nothing else takes their cards.
        let mut chosen: Vec<Option<usize>> = sheets
            .iter()
            .map(|(canvas, _)| {
                let card = self
                    .faces
                    .get(&address(canvas))?
                    .iter()
                    .copied()
                    .find(|card| self.cards[*card].shown != frame)?;
                // Built while glyphs it needs waited for the atlas, it is built again until it has them.
                self.cards[card].panel.show(queue, canvas);
                self.cards[card].shown = frame;
                Some(card)
            })
            .collect();
        for (place, (canvas, _)) in sheets.iter().enumerate() {
            if chosen[place].is_some() {
                continue;
            }
            let card = drawn
                .get(place)
                .map(|(card, _)| *card)
                .filter(|card| self.cards[*card].shown != frame)
                .or_else(|| {
                    (0..self.cards.len())
                        .filter(|card| self.cards[*card].shown != frame)
                        .min_by_key(|card| self.cards[*card].shown)
                })
                .unwrap_or_else(|| {
                    self.cards.push(Card {
                        panel: renderer.panel(device, CARD_CAPACITY).growing(CARD_MOST),
                        face: 0,
                        shown: 0,
                    });
                    self.cards.len() - 1
                });
            self.forget(card);
            self.cards[card].face = address(canvas);
            self.faces.entry(address(canvas)).or_default().push(card);
            self.cards[card].panel.show(queue, canvas);
            self.cards[card].shown = frame;
            chosen[place] = Some(card);
        }
        self.drawn = chosen
            .into_iter()
            .zip(&sheets)
            .map(|(card, (_, model))| (card.expect("each sheet has a card"), *model))
            .collect();
        let uploads = Uploads::since(before, self.cards.iter().map(|card| &card.panel));
        self.spare();
        uploads
    }

    /// Forget which face `card` showed, as it is to show another.
    fn forget(&mut self, card: usize) {
        let face = self.cards[card].face;
        if let Some(cards) = self.faces.get_mut(&face) {
            cards.retain(|shown| *shown != card);
            if cards.is_empty() {
                self.faces.remove(&face);
            }
        }
    }

    /// Free the cards not drawn now beyond [`SPARE_CARDS`], those drawn longest ago, and the faces they showed.
    fn spare(&mut self) {
        let frame = self.frame;
        let mut free: Vec<usize> = (0..self.cards.len())
            .filter(|card| self.cards[*card].shown != frame)
            .collect();
        if free.len() <= SPARE_CARDS {
            return;
        }
        free.sort_by_key(|card| std::cmp::Reverse(self.cards[*card].shown));
        let freed: std::collections::HashSet<usize> =
            free.split_off(SPARE_CARDS).into_iter().collect();
        // What each card kept is now, as the others close up behind them.
        let mut index = 0;
        let kept: Vec<Option<usize>> = (0..self.cards.len())
            .map(|card| {
                (!freed.contains(&card)).then(|| {
                    index += 1;
                    index - 1
                })
            })
            .collect();
        let mut card = 0;
        self.cards.retain(|_| {
            card += 1;
            kept[card - 1].is_some()
        });
        self.faces.clear();
        for (card, kept) in self.cards.iter().enumerate() {
            self.faces.entry(kept.face).or_default().push(card);
        }
        for (card, _) in &mut self.drawn {
            *card = kept[*card].expect("a card drawn now is kept");
        }
    }

    /// Upload what each panel shows now, remember where it is drawn, and say what it built.
    pub fn set(&mut self, queue: &wgpu::Queue, panels: &scene::Panels) -> Uploads {
        let before = builds([&self.pill, &self.card, &self.badge, &self.wrist]);
        let upload = |panel: &mut Panel, sheet: &Sheet| {
            if let Some(canvas) = &sheet.canvas {
                panel.set(queue, canvas);
            }
            sheet.model
        };
        self.pill_model = upload(&mut self.pill, &panels.pill);
        let [card, badge] = &panels.indicator;
        self.models = [
            upload(&mut self.card, card),
            upload(&mut self.badge, badge),
            upload(&mut self.wrist, &panels.wrist),
        ];
        Uploads::since(before, [&self.pill, &self.card, &self.badge, &self.wrist])
    }

    /// The panels to draw, each with its model in the eyes' space, which `scene_to_space` takes the scene to: the
    /// projection's walls and panels, then the inbox and cards, in the order the frame step gave them, farthest first, so
    /// that a nearer card blends over one behind it, as panels write depth only once drawn (`design/rendering.md`); then
    /// the pill, card, badge and transcript.
    pub fn list(&self, scene_to_space: Mat4) -> Vec<(&Panel, Mat4)> {
        let cards = self
            .drawn
            .iter()
            .map(|(card, model)| (&self.cards[*card].panel, scene_to_space * *model));
        let scene = [&self.card, &self.badge, &self.wrist]
            .into_iter()
            .zip(self.models)
            .filter_map(|(panel, model)| Some((panel, scene_to_space * model?)));
        cards
            .chain(self.pill_model.map(|model| (&self.pill, model)))
            .chain(scene)
            .collect()
    }
}

/// The address of `canvas`, by which the card that shows it knows it while it holds it.
fn address(canvas: &Arc<tomori_scene::canvas::Canvas>) -> usize {
    Arc::as_ptr(canvas) as usize
}

/// Apply `frame` to `renderer` and `panels`, ready to render, and say which of the panels' meshes it wrote.
pub fn draw(
    renderer: &mut Renderer,
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    panels: &mut Panels,
    frame: &Frame,
) -> Uploads {
    renderer.set_carriage(frame.carriage);
    renderer.set_scenery(frame.scenery);
    if let Some(drawn) = &frame.environment {
        renderer.animate(device, queue, drawn.as_ref());
    }
    // The avatar the space's backend names, while the swarm does not show; see `design/avatars.md`.
    renderer.set_avatar(device, queue, frame.avatar.as_ref());
    renderer.set_landing(frame.landing);
    renderer.set_pads(&frame.pads);
    renderer.set_blink(frame.blink);
    renderer.set_home_ring(Some(frame.home_ring));
    renderer.update(queue, frame.swarm, frame.marker, frame.attracting);
    renderer.update_volumes(queue, &frame.volumes);
    renderer.set_presentation_page(frame.card);
    renderer.set_presentation_content_page(frame.content);
    renderer.set_dials(frame.dials);
    renderer.update_workspace(
        device,
        queue,
        frame.cubes,
        frame.stations,
        frame.workspace,
        frame.sign_facing,
    );
    if let Some(arrival) = &frame.arrival {
        renderer.update_arrival(
            device,
            queue,
            &arrival_graphics::Frame {
                scene: &arrival.scene,
                hovered: arrival.hovered.as_ref(),
                ray: arrival.ray,
                left_ray: arrival.left_ray,
                time: arrival.time,
            },
        );
    }
    // The others in the space, among Tomori's cubes; see `design/people.md`.
    renderer.set_people(frame.people);
    renderer.update_images(device, queue, &frame.images);
    let shared = panels.set(queue, &frame.panels);
    // The walls stand farther away than the inbox, beside the swarm, so they go first.
    shared
        + panels.set_cards(
            device,
            queue,
            renderer,
            frame.projection.iter().chain(&frame.cards),
        )
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_gpu;
    use crate::test_support::read_back;
    use crate::{
        arrival,
        controller::Pose,
        hand_menu::{HOLD, Palm},
        scene::{Senses, Step, fixtures::*},
    };
    use glam::Vec3;
    use std::time::Instant;

    mod cards;
    mod chart_readout;
    mod chips;
    mod freeze;
    mod image_sharpness;
    mod linked_cursor;
    mod multiview;
    mod padlock_corner;
    mod padlock_floor;
    mod people;
    mod timers;
    mod turning;
    mod volumes;

    #[test]
    fn a_frame_draws_its_panels_where_the_step_placed_them() {
        test_gpu::with(|test_gpu::Gpu { device, queue, .. }| {
            let scope = device.push_error_scope(wgpu::ErrorFilter::Validation);
            let mut renderer =
                Renderer::new(&device, &queue, wgpu::TextureFormat::Rgba8UnormSrgb, 64, 64);
            let mut panels = Panels::new(&device, &renderer);
            assert!(panels.list(Mat4::IDENTITY).is_empty(), "nothing drawn yet");
            let head = Mat4::from_translation(arrival::HOME);
            let senses = Senses {
                focused: true,
                head: Some(head),
                head_pose: Pose::from_transform(head),
                viewer: Some(arrival::HOME),
                space_head: head,
                trigger: Some(0.0),
                // A right palm turned up, which shows the transcript.
                right_palm: Some(Palm {
                    wrist: arrival::HOME + Vec3::new(0.1, -0.35, -0.3),
                    normal: Vec3::Y,
                }),
                ..Default::default()
            };
            let mut conversation = joined();
            let mut step = Step::default();
            let start = Instant::now();
            step.update(&senses, &mut conversation, &mut felt(), start, 0.01);
            let mut frame =
                step.update(&senses, &mut conversation, &mut felt(), start + HOLD, 0.01);
            let pill = frame.panels.pill.model;
            let wrist = frame.panels.wrist.model.expect("the transcript shows");
            // What an environment's client side shows is handed to the renderer, which here has no scenery for it.
            frame.environment = Some(std::sync::Arc::new(1usize));
            draw(&mut renderer, &device, &queue, &mut panels, &frame);
            // The pill is hidden while a hand's panel shows; the transcript is moved from the scene to the eyes' space.
            assert!(pill.is_none());
            let scene_to_space = Mat4::from_translation(Vec3::X);
            let list = panels.list(scene_to_space);
            assert_eq!(list.len(), 1);
            assert_eq!(list[0].1, scene_to_space * wrist);
            // In arrival, the objects are drawn, with the pill hidden and the transcript gone.
            let mut lobby = lobby(tomori_protocol::ConnectionRole::Owner);
            let mut step = Step::default();
            let mut frame = step.update(&senses, &mut lobby, &mut felt(), start, 1.0);
            assert!(frame.arrival.is_some());
            // The grid has no scenery to show it.
            frame.environment = Some(std::sync::Arc::new(1usize));
            draw(&mut renderer, &device, &queue, &mut panels, &frame);
            assert!(panels.list(scene_to_space).is_empty());
            // In a space with nothing on the hands, only the pill, already in the eyes' space.
            let mut step = Step::default();
            let plain = Senses {
                right_palm: None,
                ..senses
            };
            let frame = step.update(&plain, &mut conversation, &mut felt(), start, 0.01);
            let pill = frame.panels.pill.model.expect("the pill");
            draw(&mut renderer, &device, &queue, &mut panels, &frame);
            let list = panels.list(scene_to_space);
            assert_eq!(list.len(), 1);
            assert_eq!(list[0].1, pill);
            // Recording, the pill comes first, then the indicator, moved into the eyes' space.
            let recording = Senses {
                trackers: tomori_scene::recording::Trackers {
                    view: Some(arrival::HOME - Vec3::Z * 0.5),
                    ..Default::default()
                },
                ..plain
            };
            conversation.talk(true);
            let frame = step.update(&recording, &mut conversation, &mut felt(), start, 0.01);
            let card = frame.panels.indicator[0].model.expect("the recording card");
            let pill = frame.panels.pill.model.expect("the pill");
            draw(&mut renderer, &device, &queue, &mut panels, &frame);
            let models: Vec<Mat4> = panels
                .list(scene_to_space)
                .into_iter()
                .map(|(_, model)| model)
                .collect();
            assert_eq!(models, [pill, scene_to_space * card]);
            assert!(pollster::block_on(scope.pop()).is_none());
        });
    }

    #[test]
    fn the_inbox_and_cards_are_drawn_first_in_the_order_the_step_gave() {
        test_gpu::with(|test_gpu::Gpu { device, queue, .. }| {
            let scope = device.push_error_scope(wgpu::ErrorFilter::Validation);
            let mut renderer =
                Renderer::new(&device, &queue, wgpu::TextureFormat::Rgba8UnormSrgb, 64, 64);
            let mut panels = Panels::new(&device, &renderer);
            let card = |number: u64| tomori_protocol::Card {
                id: format!("github://o/r/issues/{number}"),
                number,
                kind: tomori_protocol::CardKind::Issue,
                title: "A card".into(),
                author: "ada".into(),
                labels: Vec::new(),
                checks: tomori_protocol::Checks::Passing,
                age: 0,
                placement: None,
            };
            let mut conversation = joined();
            let show = |conversation: &mut tomori_scene::conversation::Conversation, count: u64| {
                show_cards(conversation, (1..=count).map(card).collect());
            };
            show(&mut conversation, 3);
            let head = Mat4::from_translation(arrival::HOME);
            let senses = Senses {
                focused: true,
                head: Some(head),
                head_pose: Pose::from_transform(head),
                viewer: Some(arrival::HOME),
                space_head: head,
                ..Default::default()
            };
            let mut step = Step::default();
            let frame = step.update(
                &senses,
                &mut conversation,
                &mut felt(),
                Instant::now(),
                0.01,
            );
            let models: Vec<Mat4> = frame
                .cards
                .iter()
                .map(|sheet| sheet.model.unwrap())
                .collect();
            assert_eq!(models.len(), 4);
            draw(&mut renderer, &device, &queue, &mut panels, &frame);
            let list = panels.list(Mat4::IDENTITY);
            let listed: Vec<Mat4> = list.iter().take(4).map(|(_, model)| *model).collect();
            assert_eq!(listed, models);
            // Fewer cards keep the panels made, and draw only those shown.
            show(&mut conversation, 1);
            let frame = step.update(
                &senses,
                &mut conversation,
                &mut felt(),
                Instant::now(),
                0.01,
            );
            draw(&mut renderer, &device, &queue, &mut panels, &frame);
            assert_eq!(panels.cards.len(), 4);
            assert_eq!(panels.drawn.len(), 2);
            // A sheet without a canvas is not drawn.
            panels.set_cards(&device, &queue, &renderer, &[Sheet::default()]);
            assert!(panels.drawn.is_empty());
            assert!(pollster::block_on(scope.pop()).is_none());
        });
    }

    /// Set `TOMORI_CARDS_PREVIEW` to a PNG path to save what this renders: the inbox from home, with a card carried.
    #[test]
    fn the_inbox_and_its_cards_render_over_the_scene() {
        const WIDTH: u32 = 960;
        const HEIGHT: u32 = 720;
        test_gpu::with(|test_gpu::Gpu { device, queue, .. }| {
            let scope = device.push_error_scope(wgpu::ErrorFilter::Validation);
            let format = wgpu::TextureFormat::Rgba8UnormSrgb;
            let texture = device.create_texture(&wgpu::TextureDescriptor {
                label: Some("cards test eye"),
                size: wgpu::Extent3d {
                    width: WIDTH,
                    height: HEIGHT,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
                view_formats: &[],
            });
            let view = texture.create_view(&Default::default());
            let mut renderer = Renderer::new(&device, &queue, format, WIDTH, HEIGHT);
            renderer.set_environment(tomori_scene::environment::Environment::MINIMAL);
            let mut panels = Panels::new(&device, &renderer);
            let titles = [
                (
                    164,
                    tomori_protocol::CardKind::PullRequest,
                    tomori_protocol::Checks::Passing,
                    "Make the client crates ready to publish",
                ),
                (
                    163,
                    tomori_protocol::CardKind::Draft,
                    tomori_protocol::Checks::Failing,
                    "Move the headset app to com.codeotaku.tomori",
                ),
                (
                    149,
                    tomori_protocol::CardKind::Issue,
                    tomori_protocol::Checks::None,
                    "Triage boxes that apply labels on GitHub",
                ),
                (
                    148,
                    tomori_protocol::CardKind::Issue,
                    tomori_protocol::Checks::None,
                    "An inbox of issue and pull request cards",
                ),
                (
                    146,
                    tomori_protocol::CardKind::Issue,
                    tomori_protocol::Checks::None,
                    "Text rendering: a glyph atlas",
                ),
                (
                    97,
                    tomori_protocol::CardKind::Issue,
                    tomori_protocol::Checks::None,
                    "Physics with Rapier",
                ),
            ];
            let cards: Vec<tomori_protocol::Card> = titles
                .iter()
                .map(|(number, kind, checks, title)| tomori_protocol::Card {
                    id: format!("github://ioquatix/tomori/issues/{number}"),
                    number: *number,
                    kind: *kind,
                    title: (*title).into(),
                    author: "ioquatix".into(),
                    labels: vec![tomori_protocol::Label {
                        name: "enhancement".into(),
                        color: "a2eeef".into(),
                    }],
                    checks: *checks,
                    age: 3 * 86_400,
                    placement: None,
                })
                .collect();
            let mut conversation = joined();
            show_cards(&mut conversation, cards);
            let head = Mat4::from_translation(arrival::HOME);
            let inbox = tomori_scene::board::INBOX;
            let senses = Senses {
                focused: true,
                head: Some(head),
                head_pose: Pose::from_transform(head),
                viewer: Some(arrival::HOME),
                space_head: head,
                trigger: Some(0.0),
                beam: true,
                ray: tomori_scene::pointing::Ray::new(
                    arrival::HOME,
                    inbox + Vec3::new(0.0, 0.05, 0.0) - arrival::HOME,
                ),
                ..Default::default()
            };
            let mut step = Step::default();
            let now = Instant::now();
            let capture = |renderer: &mut Renderer, panels: &Panels| {
                // About the field of view the headset gives the inbox, to judge its text at the headset's resolution.
                let matrix = glam::camera::rh::proj::directx::perspective(
                    0.45,
                    WIDTH as f32 / HEIGHT as f32,
                    0.05,
                    50.0,
                ) * glam::camera::rh::view::look_at_mat4(
                    arrival::HOME,
                    inbox + Vec3::new(0.1, 0.0, 0.0),
                    Vec3::Y,
                );
                renderer.render(
                    &device,
                    &queue,
                    std::slice::from_ref(&view),
                    &[matrix],
                    Mat4::IDENTITY,
                    &panels.list(Mat4::IDENTITY),
                );
                let buffer = device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("cards test readback"),
                    size: (WIDTH * HEIGHT * 4) as u64,
                    usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
                    mapped_at_creation: false,
                });
                let mut encoder = device.create_command_encoder(&Default::default());
                encoder.copy_texture_to_buffer(
                    texture.as_image_copy(),
                    wgpu::TexelCopyBufferInfo {
                        buffer: &buffer,
                        layout: wgpu::TexelCopyBufferLayout {
                            offset: 0,
                            bytes_per_row: Some(WIDTH * 4),
                            rows_per_image: Some(HEIGHT),
                        },
                    },
                    texture.size(),
                );
                queue.submit([encoder.finish()]);
                let (sender, receiver) = std::sync::mpsc::channel();
                buffer
                    .slice(..)
                    .map_async(wgpu::MapMode::Read, move |result| {
                        let _ = sender.send(result);
                    });
                device.poll(wgpu::PollType::wait_indefinitely()).unwrap();
                receiver.recv().unwrap().unwrap();
                let data = buffer.slice(..).get_mapped_range().unwrap().to_vec();
                buffer.unmap();
                image::RgbaImage::from_raw(WIDTH, HEIGHT, data).unwrap()
            };
            let bare = capture(&mut renderer, &panels);
            let frame = step.update(&senses, &mut conversation, &mut felt(), now, 0.01);
            draw(&mut renderer, &device, &queue, &mut panels, &frame);
            let pointed = capture(&mut renderer, &panels);
            assert_ne!(pointed, bare, "the inbox must draw over the bare scene");
            // Picked up and carried to the right.
            let squeezed = Senses {
                grip: Some(1.0),
                ..senses
            };
            step.update(&squeezed, &mut conversation, &mut felt(), now, 0.01);
            let carried = Senses {
                ray: tomori_scene::pointing::Ray::new(
                    arrival::HOME,
                    inbox + Vec3::new(0.2, 0.1, 0.1) - arrival::HOME,
                ),
                ..squeezed
            };
            let frame = step.update(&carried, &mut conversation, &mut felt(), now, 0.01);
            draw(&mut renderer, &device, &queue, &mut panels, &frame);
            let moved = capture(&mut renderer, &panels);
            assert_ne!(moved, pointed);
            let mut sheet = image::RgbaImage::new(WIDTH, HEIGHT * 2);
            image::imageops::replace(&mut sheet, &pointed, 0, 0);
            image::imageops::replace(&mut sheet, &moved, 0, HEIGHT.into());
            crate::test_support::preview("TOMORI_CARDS_PREVIEW", &sheet);
            assert!(pollster::block_on(scope.pop()).is_none());
        });
    }

    /// Set `TOMORI_PROJECTION_PREVIEW` to a PNG path to save what this renders: the Tomori wall where the server stands it
    /// by default, curving below the swarm, with four text panels and two code panels, from home, then with one pointed
    /// at and named on the ray, and one pulled off and carried, fitting its text, towards a slot whose outline shows
    /// where it would go, and the Rust code panel close up, highlighted.
    #[test]
    fn a_projection_wall_and_its_panels_render_over_the_scene() {
        const WIDTH: u32 = 960;
        const HEIGHT: u32 = 720;
        test_gpu::with(|test_gpu::Gpu { device, queue, .. }| {
            let scope = device.push_error_scope(wgpu::ErrorFilter::Validation);
            let format = wgpu::TextureFormat::Rgba8UnormSrgb;
            let texture = device.create_texture(&wgpu::TextureDescriptor {
                label: Some("projection test eye"),
                size: wgpu::Extent3d {
                    width: WIDTH,
                    height: HEIGHT,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
                view_formats: &[],
            });
            let view = texture.create_view(&Default::default());
            let mut renderer = Renderer::new(&device, &queue, format, WIDTH, HEIGHT);
            renderer.set_environment(tomori_scene::environment::Environment::MINIMAL);
            let mut panels = Panels::new(&device, &renderer);
            let place = |slot: u32| tomori_protocol::Place::Wall {
                wall: tomori_protocol::TOMORI_WALL.into(),
                slot,
            };
            let shown = vec![
                tomori_protocol::Panel::new(
                    "panel-1",
                    "Build status",
                    "**3 crates** failed:\n\n- `tomori-scene`: 2 tests\n- `tomori-render`: lavapipe\n- `tomori-server`: coverage",
                    place(0),
                )
                .with_meta(vec![tomori_protocol::Meta::new("source", "CI")])
                .with_age(300),
                tomori_protocol::Panel::new(
                    "panel-2",
                    "Steps to release",
                    "1. Bump the version.\n2. Run `cargo bake test:coverage`.\n3. Tag and push.\n4. Upload the APK.",
                    place(1),
                )
                .with_age(120),
                tomori_protocol::Panel::new(
                    "panel-3",
                    "Summary",
                    "Projection walls hold **text panels** the agent writes in `panel` blocks. Anyone can pull one off, put it back, or carry it and press B to throw it away.",
                    place(3),
                )
                .with_meta(vec![tomori_protocol::Meta::new("issue", "#82")])
                .with_age(30),
                tomori_protocol::Panel::new(
                    "panel-4",
                    "日本語のパネル",
                    "壁に投影されたテキスト。\n\n| crate | regions |\n|---|---|\n| scene | 100% |",
                    place(4),
                )
                .with_age(10),
            ];
            // Two code panels, as the server highlights them, with spans of every kind.
            let span = tomori_protocol::Span::new;
            use tomori_protocol::TokenKind as Kind;
            let rust = "#[derive(Debug)]\nstruct Point { x: f32 }\n\n/// The origin.\nfn origin() -> Point {\n    let x = 0.0 + 1.5 * 2.0; // half\n    println!(\"{x}\");\n    Point { x: X_MIN }\n}";
            let code = vec![
                tomori_protocol::Panel::new("panel-5", "rust", rust, place(2))
                    .with_code(tomori_protocol::Code::new("rust", 9).with_spans(vec![
                        span(0, 16, Kind::Attribute),
                        span(17, 6, Kind::Keyword),
                        span(24, 5, Kind::Type),
                        span(30, 1, Kind::Punctuation),
                        span(32, 1, Kind::Variable),
                        span(35, 3, Kind::Type),
                        span(42, 15, Kind::Comment),
                        span(58, 2, Kind::Keyword),
                        span(61, 6, Kind::Function),
                        span(70, 2, Kind::Operator),
                        span(73, 5, Kind::Type),
                        span(85, 3, Kind::Keyword),
                        span(91, 1, Kind::Operator),
                        span(93, 3, Kind::Number),
                        span(97, 1, Kind::Operator),
                        span(99, 3, Kind::Number),
                        span(103, 1, Kind::Operator),
                        span(105, 3, Kind::Number),
                        span(108, 1, Kind::Punctuation),
                        span(110, 7, Kind::Comment),
                        span(122, 8, Kind::Function),
                        span(131, 5, Kind::String),
                        span(143, 5, Kind::Type),
                        span(154, 5, Kind::Constant),
                    ]))
                    .with_age(5),
                tomori_protocol::Panel::new(
                    "panel-6",
                    "python",
                    (1..=16)
                        .map(|line| format!("total += values[{line}]  # step {line}"))
                        .collect::<Vec<_>>()
                        .join("\n"),
                    place(5),
                )
                .with_code(tomori_protocol::Code::new("python", 40))
                .with_pinned(true)
                .with_age(60),
            ];
            let plain: Vec<tomori_protocol::Panel> = shown
                .iter()
                .cloned()
                .chain(code.iter().cloned().map(|mut panel| {
                    panel.code = Some(tomori_protocol::Code::new("rust", 9));
                    panel
                }))
                .collect();
            let shown: Vec<tomori_protocol::Panel> = shown.into_iter().chain(code).collect();
            let mut conversation = joined();
            show_projection(&mut conversation, Vec::new(), shown);
            let head = Mat4::from_translation(arrival::HOME);
            let wall = tomori_wall();
            let wall_at = Vec3::from_array(wall.pose.position);
            let wall_frame = Mat4::from_rotation_translation(
                glam::Quat::from_array(wall.pose.orientation),
                wall_at,
            );
            let slots = tomori_scene::projection::slot_frames(&wall, wall_frame);
            let slot = |index: u32| slots[index as usize].w_axis.truncate();
            // Looking ahead at the swarm, with the wall below it.
            let looking = Vec3::new(0.0, 1.0, -2.3);
            let senses = Senses {
                focused: true,
                head: Some(head),
                head_pose: Pose::from_transform(head),
                viewer: Some(arrival::HOME),
                space_head: head,
                trigger: Some(0.0),
                beam: true,
                ray: tomori_scene::pointing::Ray::new(arrival::HOME, slot(0) - arrival::HOME),
                ..Default::default()
            };
            let mut step = Step::default();
            let now = Instant::now();
            // About the field of view the headset gives the wall, to judge its text at the headset's resolution.
            let lens = glam::camera::rh::proj::directx::perspective(
                1.1,
                WIDTH as f32 / HEIGHT as f32,
                0.05,
                50.0,
            );
            let from_home =
                lens * glam::camera::rh::view::look_at_mat4(arrival::HOME, looking, Vec3::Y);
            // Close up: the Rust code panel, in the third slot of the curved wall, from 0.8 m in front of it.
            let facing = slots[2].transform_vector3(Vec3::Z).normalize();
            let close = lens
                * glam::camera::rh::view::look_at_mat4(slot(2) + facing * 0.8, slot(2), Vec3::Y);
            let capture_from = |renderer: &mut Renderer, panels: &Panels, matrix: Mat4| {
                renderer.render(
                    &device,
                    &queue,
                    std::slice::from_ref(&view),
                    &[matrix],
                    Mat4::IDENTITY,
                    &panels.list(Mat4::IDENTITY),
                );
                let buffer = device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("projection test readback"),
                    size: (WIDTH * HEIGHT * 4) as u64,
                    usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
                    mapped_at_creation: false,
                });
                let mut encoder = device.create_command_encoder(&Default::default());
                encoder.copy_texture_to_buffer(
                    texture.as_image_copy(),
                    wgpu::TexelCopyBufferInfo {
                        buffer: &buffer,
                        layout: wgpu::TexelCopyBufferLayout {
                            offset: 0,
                            bytes_per_row: Some(WIDTH * 4),
                            rows_per_image: Some(HEIGHT),
                        },
                    },
                    texture.size(),
                );
                queue.submit([encoder.finish()]);
                let (sender, receiver) = std::sync::mpsc::channel();
                buffer
                    .slice(..)
                    .map_async(wgpu::MapMode::Read, move |result| {
                        let _ = sender.send(result);
                    });
                device.poll(wgpu::PollType::wait_indefinitely()).unwrap();
                receiver.recv().unwrap().unwrap();
                let data = buffer.slice(..).get_mapped_range().unwrap().to_vec();
                buffer.unmap();
                image::RgbaImage::from_raw(WIDTH, HEIGHT, data).unwrap()
            };
            let capture = |renderer: &mut Renderer, panels: &Panels| {
                capture_from(renderer, panels, from_home)
            };
            let bare = capture(&mut renderer, &panels);
            // Looking at the wall, pointing at nothing.
            let away = Senses {
                ray: tomori_scene::pointing::Ray::new(arrival::HOME, -Vec3::Y),
                ..senses
            };
            let frame = step.update(&away, &mut conversation, &mut felt(), now, 0.01);
            let wall_sheets = tomori_protocol::WALL_SLOTS as usize + 1;
            assert_eq!(
                frame.projection.len(),
                wall_sheets + 6,
                "the wall's slots, its label and its panels"
            );
            draw(&mut renderer, &device, &queue, &mut panels, &frame);
            let wall_drawn = capture(&mut renderer, &panels);
            assert_ne!(wall_drawn, bare, "the wall must draw over the bare scene");
            let highlighted = capture_from(&mut renderer, &panels, close);
            // The same code without its spans is drawn plain, which differs: its kinds are coloured.
            let mut plain_conversation = joined();
            show_projection(&mut plain_conversation, Vec::new(), plain);
            let mut plain_step = Step::default();
            let frame = plain_step.update(&away, &mut plain_conversation, &mut felt(), now, 0.01);
            draw(&mut renderer, &device, &queue, &mut panels, &frame);
            let unhighlighted = capture_from(&mut renderer, &panels, close);
            assert_ne!(highlighted, unhighlighted, "code must be coloured by kind");
            // Coloured pixels, such as the keywords' violet, where plain code has none.
            let violet = |image: &image::RgbaImage| {
                image
                    .pixels()
                    .filter(|pixel| {
                        let [r, g, b, _] = pixel.0.map(i32::from);
                        b > 150 && r > 120 && g + 30 < r
                    })
                    .count()
            };
            assert!(violet(&highlighted) > 20 + violet(&unhighlighted));
            // Pointing at the first panel names it on the ray.
            let frame = step.update(&senses, &mut conversation, &mut felt(), now, 0.01);
            assert_eq!(
                frame.projection.len(),
                wall_sheets + 6 + 1,
                "with the label"
            );
            draw(&mut renderer, &device, &queue, &mut panels, &frame);
            let pointed = capture(&mut renderer, &panels);
            assert_ne!(pointed, wall_drawn);
            // Pulled off and carried towards the last slot, whose outline shows where it would go, over the code panel there.
            let squeezed = Senses {
                grip: Some(1.0),
                ..senses
            };
            step.update(&squeezed, &mut conversation, &mut felt(), now, 0.01);
            let carried = Senses {
                ray: tomori_scene::pointing::Ray::new(arrival::HOME, slot(5) - arrival::HOME),
                ..squeezed
            };
            let frame = step.update(&carried, &mut conversation, &mut felt(), now, 0.01);
            draw(&mut renderer, &device, &queue, &mut panels, &frame);
            let moved = capture(&mut renderer, &panels);
            assert_ne!(moved, pointed);
            let mut sheet = image::RgbaImage::new(WIDTH, HEIGHT * 4);
            image::imageops::replace(&mut sheet, &wall_drawn, 0, 0);
            image::imageops::replace(&mut sheet, &pointed, 0, HEIGHT.into());
            image::imageops::replace(&mut sheet, &moved, 0, (2 * HEIGHT).into());
            image::imageops::replace(&mut sheet, &highlighted, 0, (3 * HEIGHT).into());
            crate::test_support::preview("TOMORI_PROJECTION_PREVIEW", &sheet);
            assert!(pollster::block_on(scope.pop()).is_none());
        });
    }

    /// A synthetic render, 640 by 360: a sky fading to blue, an orange disc, and a grid of thin white lines, which
    /// shimmer at a distance unless the image is mipmapped.
    fn render_png() -> Vec<u8> {
        let picture = image::RgbImage::from_fn(640, 360, |x, y| {
            let (dx, dy) = (x as f32 - 420.0, y as f32 - 170.0);
            if x % 40 == 0 || y % 40 == 0 {
                image::Rgb([255, 255, 255])
            } else if dx * dx + dy * dy < 90.0 * 90.0 {
                image::Rgb([250, 130, 20])
            } else {
                image::Rgb([30, 60 + (y / 3) as u8, 200])
            }
        });
        let mut png = std::io::Cursor::new(Vec::new());
        picture.write_to(&mut png, image::ImageFormat::Png).unwrap();
        png.into_inner()
    }

    /// Set `TOMORI_IMAGE_PANEL_PREVIEW` to a PNG path to save what this renders: the wall from home before the image
    /// arrives, and once it has, then the same image floating in front of home, close up.
    #[test]
    fn an_image_panel_draws_its_image_on_a_wall_and_floating_from_one_texture() {
        const WIDTH: u32 = 960;
        const HEIGHT: u32 = 720;
        test_gpu::with(|test_gpu::Gpu { device, queue, .. }| {
            let scope = device.push_error_scope(wgpu::ErrorFilter::Validation);
            let format = wgpu::TextureFormat::Rgba8UnormSrgb;
            let texture = device.create_texture(&wgpu::TextureDescriptor {
                label: Some("image panel test eye"),
                size: wgpu::Extent3d {
                    width: WIDTH,
                    height: HEIGHT,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
                view_formats: &[],
            });
            let view = texture.create_view(&Default::default());
            let mut renderer = Renderer::new(&device, &queue, format, WIDTH, HEIGHT);
            renderer.set_environment(tomori_scene::environment::Environment::MINIMAL);
            let mut panels = Panels::new(&device, &renderer);
            let png = render_png();
            let hash = "ab".repeat(32);
            let image = tomori_protocol::Image::new(hash.clone(), 640, 360, png.len() as u32);
            // To the right of home, facing it, clear of the wall ahead.
            let floating =
                tomori_protocol::Pose::upright_facing([1.1, 1.45, -0.9], [0.0, 1.45, 0.0]);
            let shown = vec![
                tomori_protocol::Panel::new(
                    "panel-1",
                    "Session browser",
                    "The layout, rendered: folders on the left, sessions on the right.",
                    tomori_protocol::Place::Wall {
                        wall: tomori_protocol::TOMORI_WALL.into(),
                        slot: 2,
                    },
                )
                .with_image(image.clone())
                .with_age(30),
                tomori_protocol::Panel::new(
                    "panel-2",
                    "Session browser",
                    "The same render, pulled off the wall.",
                    tomori_protocol::Place::Free { pose: floating },
                )
                .with_image(image),
            ];
            let mut conversation = joined();
            show_projection(&mut conversation, Vec::new(), shown);
            let head = Mat4::from_translation(arrival::HOME);
            let senses = Senses {
                focused: true,
                head: Some(head),
                head_pose: Pose::from_transform(head),
                viewer: Some(arrival::HOME),
                space_head: head,
                trigger: Some(0.0),
                beam: true,
                ray: tomori_scene::pointing::Ray::new(arrival::HOME, -Vec3::Y),
                ..Default::default()
            };
            let mut step = Step::default();
            let now = Instant::now();
            let lens = glam::camera::rh::proj::directx::perspective(
                1.1,
                WIDTH as f32 / HEIGHT as f32,
                0.05,
                50.0,
            );
            let at_wall = lens
                * glam::camera::rh::view::look_at_mat4(
                    arrival::HOME,
                    Vec3::new(0.0, 0.7, -2.3),
                    Vec3::Y,
                );
            // From 0.7 m in front of the floating panel, square on.
            let floating_at = Vec3::from_array(floating.position);
            let facing = glam::Quat::from_array(floating.orientation) * Vec3::Z;
            let at_floating = lens
                * glam::camera::rh::view::look_at_mat4(
                    floating_at + facing * 0.7,
                    floating_at,
                    Vec3::Y,
                );
            let capture = |renderer: &mut Renderer, panels: &Panels, matrix: Mat4| {
                renderer.render(
                    &device,
                    &queue,
                    std::slice::from_ref(&view),
                    &[matrix],
                    Mat4::IDENTITY,
                    &panels.list(Mat4::IDENTITY),
                );
                read_back(&device, &queue, &texture, (WIDTH, HEIGHT))
            };
            let orange = |image: &image::RgbaImage| {
                image
                    .pixels()
                    .filter(|pixel| {
                        let [r, g, b, _] = pixel.0.map(i32::from);
                        r > 200 && (100..200).contains(&g) && b < 90
                    })
                    .count()
            };
            // Before its image arrives, each panel shows where it will be, and nothing is decoded.
            let frame = step.update(&senses, &mut conversation, &mut felt(), now, 0.01);
            assert!(frame.images.is_empty());
            draw(&mut renderer, &device, &queue, &mut panels, &frame);
            let waiting = capture(&mut renderer, &panels, at_wall);
            assert_eq!(orange(&waiting), 0);
            // Once it arrives, both panels draw it, from one texture with every mipmap level.
            conversation.apply(tomori_client::Event::Image {
                hash: hash.clone(),
                png: png.into(),
            });
            let deadline = Instant::now() + std::time::Duration::from_secs(20);
            while renderer.images().binding(&hash).is_none() {
                assert!(Instant::now() < deadline, "the image was never uploaded");
                let frame = step.update(&senses, &mut conversation, &mut felt(), now, 0.01);
                assert_eq!(frame.images.len(), 2, "one for each panel that shows it");
                draw(&mut renderer, &device, &queue, &mut panels, &frame);
                std::thread::sleep(std::time::Duration::from_millis(2));
            }
            assert_eq!(renderer.images().levels(&hash), Some((0, 10)));
            let one: usize = crate::panel_images::mipmaps(crate::panel_images::Level {
                width: 640,
                height: 360,
                pixels: vec![0; 640 * 360 * 4],
            })
            .iter()
            .map(|level| level.pixels.len())
            .sum();
            assert_eq!(renderer.images().used(), one);
            let on_wall = capture(&mut renderer, &panels, at_wall);
            let close = capture(&mut renderer, &panels, at_floating);
            // The disc is drawn in the wall's slot, and large floating close up.
            let (small, large) = (orange(&on_wall), orange(&close));
            assert!(small > 100, "{small}");
            assert!(large > 20 * small.min(500), "{large}");
            let mut sheet = image::RgbaImage::new(WIDTH, HEIGHT * 3);
            image::imageops::replace(&mut sheet, &waiting, 0, 0);
            image::imageops::replace(&mut sheet, &on_wall, 0, HEIGHT.into());
            image::imageops::replace(&mut sheet, &close, 0, (2 * HEIGHT).into());
            crate::test_support::preview("TOMORI_IMAGE_PANEL_PREVIEW", &sheet);
            assert!(pollster::block_on(scope.pop()).is_none());
        });
    }

    /// The colours of an edged picture's top, bottom, left and right edges, and of its checks.
    const EDGES: [[u8; 3]; 4] = [[255, 0, 0], [0, 255, 0], [0, 0, 255], [255, 255, 0]];
    const CHECKS: [[u8; 3]; 2] = [[40, 40, 40], [200, 200, 200]];

    /// A PNG `width` by `height`, its own size, of dark and light checks `cell` pixels square, edged one pixel wide in
    /// [`EDGES`]: red along its top, green along its bottom, blue down its left and yellow down its right.
    fn edged_png(width: u32, height: u32, cell: u32) -> Vec<u8> {
        let picture = image::RgbImage::from_fn(width, height, |x, y| {
            image::Rgb(match (x, y) {
                (_, 0) => EDGES[0],
                (_, y) if y == height - 1 => EDGES[1],
                (0, _) => EDGES[2],
                (x, _) if x == width - 1 => EDGES[3],
                (x, y) => CHECKS[((x / cell + y / cell) % 2) as usize],
            })
        });
        let mut png = std::io::Cursor::new(Vec::new());
        picture.write_to(&mut png, image::ImageFormat::Png).unwrap();
        png.into_inner()
    }

    /// `png`, an image `width` by `height`, shown on one panel at `place`, drawn with `gpu` square on from in front of
    /// it, so that its image fills most of an eye `size` pixels across, once its texture is ready; with where, in the
    /// eye's pixels, the image's left, top, right and bottom edges are, from its canvas.
    fn image_panel_square_on(
        test_gpu::Gpu { device, queue, .. }: &test_gpu::Gpu,
        png: Vec<u8>,
        (width, height): (u32, u32),
        place: tomori_protocol::Place,
        (eye_width, eye_height): (u32, u32),
    ) -> (image::RgbaImage, [f32; 4]) {
        let scope = device.push_error_scope(wgpu::ErrorFilter::Validation);
        let format = wgpu::TextureFormat::Rgba8UnormSrgb;
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("image panel square on"),
            size: wgpu::Extent3d {
                width: eye_width,
                height: eye_height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let view = texture.create_view(&Default::default());
        let mut renderer = Renderer::new(device, queue, format, eye_width, eye_height);
        renderer.set_environment(tomori_scene::environment::Environment::MINIMAL);
        let mut panels = Panels::new(device, &renderer);
        let hash = format!("{:064x}", u64::from(width) << 32 | u64::from(height));
        let image = tomori_protocol::Image::new(hash.clone(), width, height, png.len() as u32);
        let panel = tomori_protocol::Panel::new("panel-1", "Edges", "Its caption.", place)
            .with_image(image);
        let mut conversation = joined();
        show_projection(&mut conversation, Vec::new(), vec![panel]);
        conversation.apply(tomori_client::Event::Image {
            hash: hash.clone(),
            png: png.into(),
        });
        let head = Mat4::from_translation(arrival::HOME);
        let senses = Senses {
            focused: true,
            head: Some(head),
            head_pose: Pose::from_transform(head),
            viewer: Some(arrival::HOME),
            space_head: head,
            ..Default::default()
        };
        let mut step = Step::default();
        let deadline = Instant::now() + std::time::Duration::from_secs(20);
        let frame = loop {
            let frame = step.update(
                &senses,
                &mut conversation,
                &mut felt(),
                Instant::now(),
                0.01,
            );
            draw(&mut renderer, device, queue, &mut panels, &frame);
            if renderer.images().binding(&hash).is_some() {
                break frame;
            }
            assert!(Instant::now() < deadline, "the image was never uploaded");
            std::thread::sleep(std::time::Duration::from_millis(2));
        };
        let sheet = frame
            .projection
            .iter()
            .find(|sheet| {
                sheet
                    .canvas
                    .as_deref()
                    .is_some_and(|canvas| canvas.image.is_some())
            })
            .expect("the image panel is drawn");
        let (canvas, model) = (sheet.canvas.as_deref().unwrap(), sheet.model.unwrap());
        let (_, [left, bottom, right, top]) = canvas.image_rect().unwrap();
        // Square on, from far enough in front of the image's middle that it fills three quarters of the eye.
        let middle =
            model.transform_point3(Vec3::new((left + right) / 2.0, (bottom + top) / 2.0, 0.0));
        let (across, up) = (model.x_axis.truncate(), model.y_axis.truncate());
        let (shown_width, shown_height) = (
            (right - left) * across.length(),
            (top - bottom) * up.length(),
        );
        let fov: f32 = 0.8;
        let aspect = eye_width as f32 / eye_height as f32;
        let distance = (shown_width / aspect).max(shown_height) / (2.0 * (fov / 2.0).tan()) / 0.75;
        let facing = model.z_axis.truncate().normalize();
        let matrix = glam::camera::rh::proj::directx::perspective(fov, aspect, 0.05, 50.0)
            * glam::camera::rh::view::look_at_mat4(
                middle + facing * distance,
                middle,
                up.normalize(),
            );
        renderer.render(
            device,
            queue,
            std::slice::from_ref(&view),
            &[matrix],
            Mat4::IDENTITY,
            &panels.list(Mat4::IDENTITY),
        );
        let picture = read_back(device, queue, &texture, (eye_width, eye_height));
        let pixel = |x: f32, y: f32| {
            let clip = matrix * model * glam::Vec4::new(x, y, 0.0, 1.0);
            let ndc = clip.truncate() / clip.w;
            (
                (ndc.x + 1.0) / 2.0 * eye_width as f32,
                (1.0 - ndc.y) / 2.0 * eye_height as f32,
            )
        };
        let (x0, y0) = pixel(left, top);
        let (x1, y1) = pixel(right, bottom);
        let error = pollster::block_on(scope.pop());
        assert!(error.is_none(), "{error:?}");
        (picture, [x0, y0, x1, y1])
    }

    /// Set `TOMORI_IMAGE_EDGES_PREVIEW` to a PNG path to save what this renders: edged images of four shapes, floating
    /// and on the Tomori wall, each close up and square on, its image several screen pixels to each of its own.
    #[test]
    fn an_image_panel_draws_every_edge_of_its_image_undistorted() {
        const EYE: (u32, u32) = (960, 720);
        // The colour nearest `pixel` of those an edged picture has, if it is near one.
        let nearest = |pixel: &image::Rgba<u8>| {
            EDGES.iter().chain(&CHECKS).copied().find(|colour| {
                colour
                    .iter()
                    .zip(pixel.0)
                    .all(|(want, have)| (i32::from(*want) - i32::from(have)).abs() <= 12)
            })
        };
        let floating = tomori_protocol::Place::Free {
            pose: tomori_protocol::Pose::upright_facing([1.6, 1.45, 0.0], [0.0, 1.45, 0.0]),
        };
        let wall = tomori_protocol::Place::Wall {
            wall: tomori_protocol::TOMORI_WALL.into(),
            slot: 1,
        };
        let mut sheet = image::RgbaImage::new(EYE.0 * 4, EYE.1 * 2);
        test_gpu::with(|gpu| {
            for (row, place) in [floating, wall].into_iter().enumerate() {
                // 4:3, 3:4, 16:9 and square, in checks of 4 pixels.
                for (column, (width, height)) in [(64, 48), (48, 64), (64, 36), (48, 48)]
                    .into_iter()
                    .enumerate()
                {
                    let (picture, [left, top, right, bottom]) = image_panel_square_on(
                        &gpu,
                        edged_png(width, height, 4),
                        (width, height),
                        place.clone(),
                        EYE,
                    );
                    let at = |x: f32, y: f32| nearest(picture.get_pixel(x as u32, y as u32));
                    let shape = format!("{place:?}, {width} × {height}");
                    // Drawn its own shape: its edges are as far apart as its pixels say, a pixel of it as wide as it is tall.
                    let (across, down) = (
                        (right - left) / width as f32,
                        (bottom - top) / height as f32,
                    );
                    assert!(
                        across > 4.0,
                        "{shape}: {across} screen pixels to each of its own"
                    );
                    assert!(
                        (across / down - 1.0).abs() < 0.01,
                        "{shape}: {across} by {down}"
                    );
                    // Every edge is drawn, a whole pixel of the image wide, at the middle of each side and near its ends.
                    // A quarter of the image's pixel in, short of its middle, where the next pixel would begin to show.
                    let inside = across / 4.0;
                    for fraction in [0.1, 0.5, 0.9] {
                        let x = left + (right - left) * fraction;
                        let y = top + (bottom - top) * fraction;
                        assert_eq!(
                            at(x, top + inside),
                            Some(EDGES[0]),
                            "{shape}: top at {fraction}"
                        );
                        assert_eq!(
                            at(x, bottom - inside),
                            Some(EDGES[1]),
                            "{shape}: bottom at {fraction}"
                        );
                        assert_eq!(
                            at(left + inside, y),
                            Some(EDGES[2]),
                            "{shape}: left at {fraction}"
                        );
                        assert_eq!(
                            at(right - inside, y),
                            Some(EDGES[3]),
                            "{shape}: right at {fraction}"
                        );
                        // And nothing of the image beyond them.
                        for outside in [
                            at(x, top - 3.0),
                            at(x, bottom + 3.0),
                            at(left - 3.0, y),
                            at(right + 3.0, y),
                        ] {
                            assert!(
                                outside.is_none(),
                                "{shape}: {outside:?} beyond an edge at {fraction}"
                            );
                        }
                    }
                    // Its checks are square: each colour changes along the middle row and column every 4 of its pixels.
                    let changes = |points: Vec<(f32, f32)>| {
                        let mut last = None;
                        let mut at_change = Vec::new();
                        for (index, (x, y)) in points.iter().enumerate() {
                            let colour = at(*x, *y).filter(|colour| CHECKS.contains(colour));
                            if colour.is_some() && last.is_some() && colour != last {
                                at_change.push(index as f32);
                            }
                            if colour.is_some() {
                                last = colour;
                            }
                        }
                        let spans: Vec<f32> =
                            at_change.windows(2).map(|pair| pair[1] - pair[0]).collect();
                        spans.iter().sum::<f32>() / spans.len() as f32
                    };
                    // Through the middle of a check near the image's middle.
                    let (middle_x, middle_y) = (
                        left + (4 * (width / 8) + 2) as f32 * across,
                        top + (4 * (height / 8) + 2) as f32 * down,
                    );
                    let check_across = changes(
                        (left.ceil() as u32..right as u32)
                            .map(|x| (x as f32, middle_y))
                            .collect(),
                    );
                    let check_down = changes(
                        (top.ceil() as u32..bottom as u32)
                            .map(|y| (middle_x, y as f32))
                            .collect(),
                    );
                    assert!(
                        (check_across / (4.0 * across) - 1.0).abs() < 0.02,
                        "{shape}: {check_across}"
                    );
                    assert!(
                        (check_down / (4.0 * down) - 1.0).abs() < 0.02,
                        "{shape}: {check_down}"
                    );
                    image::imageops::replace(
                        &mut sheet,
                        &picture,
                        i64::from(EYE.0) * column as i64,
                        i64::from(EYE.1) * row as i64,
                    );
                }
            }
        });
        crate::test_support::preview("TOMORI_IMAGE_EDGES_PREVIEW", &sheet);
    }

    /// Set `TOMORI_PLACEMENT_PREVIEW` to a PNG path to save what this renders: a participant at home, who added a wall
    /// to their right and is looking at it as they ask, sees the agent's answer there, while the Tomori wall ahead of
    /// them stays empty, as a running fake server places it.
    #[test]
    fn the_agent_s_panel_is_drawn_on_the_wall_whoever_asked_was_looking_at() {
        use tomori_protocol::{ClientMessage, ServerMessage};
        const WIDTH: u32 = 640;
        const HEIGHT: u32 = 480;
        // A server whose agent answers only when told to, and a participant whose client reads the projection.
        let backend = tomori_server::fake::FakeBackend::manual();
        let server = tomori_server::test_support::TestServer::with_backend(backend.clone());
        let mut client = tomori_server::test_support::TestClient::open(server.address);
        client.send(&ClientMessage::Hello {
            protocol: tomori_protocol::PROTOCOL_VERSION,
            participant: "ada".into(),
            name: "Ada".into(),
            names: false,
            environments: None,
            environment_state: false,
            projection: true,
            pairing_window: false,
            speech_held: false,
            images: false,
            diagrams: false,
            world_edges: false,
            projection_patches: false,
            live_panels: false,
            spawns: false,
            limits: None,
        });
        client.wait_for(|message| matches!(message, ServerMessage::Welcome { .. }).then_some(()));
        client.create_space("Studio");
        // A wall to the right of home, facing it, and Ada looking at it as she asks.
        let at = Vec3::new(2.2, 1.5, -0.8);
        client.send(&ClientMessage::AddWall {
            pose: tomori_protocol::Pose::upright_facing(at.to_array(), [0.0, 1.5, 0.0]),
        });
        let added = client.wait_for(|message| match message {
            ServerMessage::Projection { projection } if projection.walls.len() == 2 => {
                Some(projection.walls[1].clone())
            }
            _ => None,
        });
        let towards = at - arrival::HOME;
        let looking = glam::Quat::from_rotation_y((-towards.x).atan2(-towards.z))
            * glam::Quat::from_rotation_x(towards.y.atan2(towards.x.hypot(towards.z)));
        let head = Mat4::from_rotation_translation(looking, arrival::HOME);
        client.send(&ClientMessage::Presence {
            head: tomori_protocol::Pose {
                position: arrival::HOME.to_array(),
                orientation: looking.to_array(),
            },
            left: None,
            right: None,
            hands: None,
            carrying: None,
            carried: Vec::new(),
            rays: None,
            pointing: None,
        });
        client.say("u1", "show me the build status");
        let session = tomori_server::test_support::eventually(|| {
            backend
                .started()
                .into_iter()
                .find(|session| !session.log().prompts.is_empty())
        });
        session.reply("Here.\n\n```panel\ntitle: Build status\n---\n**All green.**\n```\n");
        let projection = client.wait_for(|message| match message {
            ServerMessage::Projection { projection } if !projection.panels.is_empty() => {
                Some(projection.clone())
            }
            _ => None,
        });
        assert!(matches!(
            &projection.panels[0].place,
            tomori_protocol::Place::Wall { wall, .. } if *wall == added.id
        ));

        test_gpu::with(|test_gpu::Gpu { device, queue, .. }| {
            let scope = device.push_error_scope(wgpu::ErrorFilter::Validation);
            let format = wgpu::TextureFormat::Rgba8UnormSrgb;
            let texture = device.create_texture(&wgpu::TextureDescriptor {
                label: Some("placement test eye"),
                size: wgpu::Extent3d {
                    width: WIDTH,
                    height: HEIGHT,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
                view_formats: &[],
            });
            let view = texture.create_view(&Default::default());
            let mut renderer = Renderer::new(&device, &queue, format, WIDTH, HEIGHT);
            renderer.set_environment(tomori_scene::environment::Environment::MINIMAL);
            let mut panels = Panels::new(&device, &renderer);
            let lens = glam::camera::rh::proj::directx::perspective(
                1.1,
                WIDTH as f32 / HEIGHT as f32,
                0.05,
                50.0,
            );
            let at_wall = lens * glam::camera::rh::view::look_at_mat4(arrival::HOME, at, Vec3::Y);
            let ahead = lens
                * glam::camera::rh::view::look_at_mat4(
                    arrival::HOME,
                    Vec3::new(0.0, 1.0, -2.3),
                    Vec3::Y,
                );
            let capture = |renderer: &mut Renderer, panels: &Panels, matrix: Mat4| {
                renderer.render(
                    &device,
                    &queue,
                    std::slice::from_ref(&view),
                    &[matrix],
                    Mat4::IDENTITY,
                    &panels.list(Mat4::IDENTITY),
                );
                read_back(&device, &queue, &texture, (WIDTH, HEIGHT))
            };
            let senses = Senses {
                focused: true,
                head: Some(head),
                head_pose: Pose::from_transform(head),
                viewer: Some(arrival::HOME),
                space_head: head,
                trigger: Some(0.0),
                beam: true,
                ray: tomori_scene::pointing::Ray::new(arrival::HOME, -Vec3::Y),
                ..Default::default()
            };
            // The space as the server sent it, and the same space before the agent answered.
            let mut shown = |panels_shown: Vec<tomori_protocol::Panel>| {
                let mut conversation = joined();
                show_projection(&mut conversation, vec![added.clone()], panels_shown);
                let mut step = Step::default();
                let frame = step.update(
                    &senses,
                    &mut conversation,
                    &mut felt(),
                    Instant::now(),
                    0.01,
                );
                draw(&mut renderer, &device, &queue, &mut panels, &frame);
                (
                    capture(&mut renderer, &panels, at_wall),
                    capture(&mut renderer, &panels, ahead),
                )
            };
            let (answered, answered_ahead) = shown(projection.panels.clone());
            let (asked, asked_ahead) = shown(Vec::new());
            // The panel is drawn on the wall Ada looks at, and nothing changes on the Tomori wall ahead of her.
            assert_ne!(
                answered, asked,
                "the panel must be drawn on the wall she looks at"
            );
            assert_eq!(
                answered_ahead, asked_ahead,
                "nothing is drawn on the Tomori wall"
            );
            let mut sheet = image::RgbaImage::new(WIDTH, HEIGHT * 2);
            image::imageops::replace(&mut sheet, &answered, 0, 0);
            image::imageops::replace(&mut sheet, &answered_ahead, 0, HEIGHT.into());
            crate::test_support::preview("TOMORI_PLACEMENT_PREVIEW", &sheet);
            assert!(pollster::block_on(scope.pop()).is_none());
        });
    }

    /// Set `TOMORI_WALL_LAYOUT_PREVIEW` to a PNG path to save what this renders: a flat wall four slots wide and one
    /// high to the left of home, and a wall curved about home of four columns of two rows to the right, each with panels in
    /// its slots, as the server lays them out, from home looking at each, and the curved wall from beside it.
    #[test]
    fn walls_of_their_own_grid_size_and_curve_are_drawn_with_a_panel_in_every_slot() {
        const WIDTH: u32 = 960;
        const HEIGHT: u32 = 720;
        use tomori_protocol::{Grid, Panel, Place, Wall, WallKind, WallShape};
        let ahead = Vec3::new(-2.5, 1.45, -1.0);
        let right = Vec3::new(2.3, 1.45, -0.8);
        let wide = Wall::new(
            "wall-1",
            WallKind::Added,
            tomori_protocol::Pose::upright_facing(ahead.to_array(), [0.0, 1.45, 0.0]),
        )
        .with_grid(Grid::new(4, 1).unwrap())
        .with_scale(1.0);
        let curved = Wall::new(
            "wall-2",
            WallKind::Added,
            tomori_protocol::Pose::upright_facing(right.to_array(), [0.0, 1.45, 0.0]),
        )
        .with_shape(WallShape::Arc { radius: 2.4 })
        .with_grid(Grid::new(4, 2).unwrap())
        .with_scale(0.75);
        let titles = ["Build", "Tests", "Coverage", "Release"];
        let mut shown: Vec<Panel> = titles
            .iter()
            .enumerate()
            .map(|(slot, title)| {
                Panel::new(
                    format!("panel-{slot}"),
                    *title,
                    format!("**{title}** is green.\n\n- one\n- two"),
                    Place::Wall {
                        wall: "wall-1".into(),
                        slot: slot as u32,
                    },
                )
            })
            .collect();
        shown.extend((0..8).map(|slot| {
            Panel::new(
                format!("panel-c{slot}"),
                format!("Slot {}", slot + 1),
                "Curved about home.",
                Place::Wall {
                    wall: "wall-2".into(),
                    slot,
                },
            )
        }));
        test_gpu::with(|test_gpu::Gpu { device, queue, .. }| {
            let scope = device.push_error_scope(wgpu::ErrorFilter::Validation);
            let format = wgpu::TextureFormat::Rgba8UnormSrgb;
            let texture = device.create_texture(&wgpu::TextureDescriptor {
                label: Some("wall layout test eye"),
                size: wgpu::Extent3d {
                    width: WIDTH,
                    height: HEIGHT,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
                view_formats: &[],
            });
            let view = texture.create_view(&Default::default());
            let mut renderer = Renderer::new(&device, &queue, format, WIDTH, HEIGHT);
            renderer.set_environment(tomori_scene::environment::Environment::MINIMAL);
            let mut panels = Panels::new(&device, &renderer);
            let lens = glam::camera::rh::proj::directx::perspective(
                1.3,
                WIDTH as f32 / HEIGHT as f32,
                0.05,
                50.0,
            );
            let at_wide =
                lens * glam::camera::rh::view::look_at_mat4(arrival::HOME, ahead, Vec3::Y);
            let at_curved =
                lens * glam::camera::rh::view::look_at_mat4(arrival::HOME, right, Vec3::Y);
            // From beside it, to see it curve.
            let beside = lens
                * glam::camera::rh::view::look_at_mat4(Vec3::new(0.6, 2.4, 1.6), right, Vec3::Y);
            let head = Mat4::from_translation(arrival::HOME);
            let senses = Senses {
                focused: true,
                head: Some(head),
                head_pose: Pose::from_transform(head),
                viewer: Some(arrival::HOME),
                space_head: head,
                trigger: Some(0.0),
                beam: true,
                ray: tomori_scene::pointing::Ray::new(arrival::HOME, -Vec3::Y),
                ..Default::default()
            };
            let capture = |renderer: &mut Renderer, panels: &Panels, matrix: Mat4| {
                renderer.render(
                    &device,
                    &queue,
                    std::slice::from_ref(&view),
                    &[matrix],
                    Mat4::IDENTITY,
                    &panels.list(Mat4::IDENTITY),
                );
                read_back(&device, &queue, &texture, (WIDTH, HEIGHT))
            };
            let mut drawn = |panels_shown: Vec<Panel>| {
                let count = panels_shown.len();
                let mut conversation = joined();
                show_projection(
                    &mut conversation,
                    vec![wide.clone(), curved.clone()],
                    panels_shown,
                );
                let mut step = Step::default();
                // The frame step lays out eight panels that arrive a frame: after as many frames as that takes, every
                // one stands.
                let now = Instant::now();
                for _ in 0..count / 8 {
                    step.update(&senses, &mut conversation, &mut felt(), now, 0.01);
                }
                let frame = step.update(&senses, &mut conversation, &mut felt(), now, 0.01);
                draw(&mut renderer, &device, &queue, &mut panels, &frame);
                (
                    capture(&mut renderer, &panels, at_wide),
                    capture(&mut renderer, &panels, at_curved),
                    capture(&mut renderer, &panels, beside),
                )
            };
            let (wide_full, curved_full, beside_full) = drawn(shown.clone());
            let (wide_empty, curved_empty, _) = drawn(Vec::new());
            // Where a slot's middle is drawn, in pixels.
            let pixel = |matrix: Mat4, at: Vec3| {
                let clip = matrix * at.extend(1.0);
                let (x, y) = (clip.x / clip.w, clip.y / clip.w);
                assert!(x.abs() < 1.0 && y.abs() < 1.0, "{at} is in view");
                (
                    ((x + 1.0) / 2.0 * WIDTH as f32) as u32,
                    ((1.0 - y) / 2.0 * HEIGHT as f32) as u32,
                )
            };
            let frame_of = |wall: &Wall| {
                Mat4::from_rotation_translation(
                    glam::Quat::from_array(wall.pose.orientation),
                    Vec3::from_array(wall.pose.position),
                )
            };
            // A panel is drawn in each of the four slots of the wide wall, its fourth past where a wall of three ends,
            // and in each of the curved wall's eight, its second row below its first.
            for (wall, matrix, full, empty) in [
                (&wide, at_wide, &wide_full, &wide_empty),
                (&curved, at_curved, &curved_full, &curved_empty),
            ] {
                let slots = tomori_scene::projection::slot_frames(wall, frame_of(wall));
                assert_eq!(slots.len() as u32, wall.slots());
                for (index, slot) in slots.iter().enumerate() {
                    // A little above its middle, on its title or first rows.
                    let at = slot.transform_point3(Vec3::new(0.0, 0.05, 0.0));
                    let (x, y) = pixel(matrix, at);
                    let around = |image: &image::RgbaImage| -> Vec<[u8; 4]> {
                        (x.saturating_sub(12)..x + 12)
                            .flat_map(|x| (y.saturating_sub(12)..y + 12).map(move |y| (x, y)))
                            .map(|(x, y)| image.get_pixel(x, y).0)
                            .collect()
                    };
                    assert_ne!(around(full), around(empty), "{} slot {index}", wall.id);
                }
            }
            let mut sheet = image::RgbaImage::new(WIDTH, HEIGHT * 3);
            image::imageops::replace(&mut sheet, &wide_full, 0, 0);
            image::imageops::replace(&mut sheet, &curved_full, 0, HEIGHT.into());
            image::imageops::replace(&mut sheet, &beside_full, 0, (HEIGHT * 2).into());
            crate::test_support::preview("TOMORI_WALL_LAYOUT_PREVIEW", &sheet);
            assert!(pollster::block_on(scope.pop()).is_none());
        });
    }

    /// Set `TOMORI_WALL_HANDLES_PREVIEW` to a PNG path to save what this renders: a wall someone added, taken hold of
    /// with the grip, its handles around it, the knob the ray meets highlighted, and the hint of the controls above it;
    /// then its right edge dragged out to a fourth column; then its knob taken and the hand moved to the right half its
    /// travel, bending it, its radius said above it, before either is let go.
    #[test]
    fn a_wall_taken_hold_of_shows_its_handles_and_hint_and_changes_as_they_are_dragged() {
        const WIDTH: u32 = 960;
        const HEIGHT: u32 = 720;
        use tomori_protocol::{Panel, Place, Wall, WallKind};
        use tomori_scene::projection::editing::{Handle, handle_point};
        let middle = Vec3::new(-0.2, 1.45, -2.3);
        let wall = Wall::new(
            "wall-1",
            WallKind::Added,
            tomori_protocol::Pose::upright_facing(middle.to_array(), [-0.2, 1.45, 0.0]),
        );
        let frame =
            Mat4::from_rotation_translation(glam::Quat::from_array(wall.pose.orientation), middle);
        let panels: Vec<Panel> = ["Build", "Tests", "Coverage"]
            .iter()
            .enumerate()
            .map(|(slot, title)| {
                Panel::new(
                    format!("panel-{slot}"),
                    *title,
                    format!("**{title}** is green."),
                    Place::Wall {
                        wall: "wall-1".into(),
                        slot: slot as u32,
                    },
                )
            })
            .collect();
        let point = |handle: Handle| handle_point(&wall, frame, handle).0;
        test_gpu::with(|test_gpu::Gpu { device, queue, .. }| {
            let scope = device.push_error_scope(wgpu::ErrorFilter::Validation);
            let format = wgpu::TextureFormat::Rgba8UnormSrgb;
            let texture = device.create_texture(&wgpu::TextureDescriptor {
                label: Some("wall handles test eye"),
                size: wgpu::Extent3d {
                    width: WIDTH,
                    height: HEIGHT,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
                view_formats: &[],
            });
            let view = texture.create_view(&Default::default());
            let mut renderer = Renderer::new(&device, &queue, format, WIDTH, HEIGHT);
            renderer.set_environment(tomori_scene::environment::Environment::MINIMAL);
            let mut drawn_panels = Panels::new(&device, &renderer);
            let lens = glam::camera::rh::proj::directx::perspective(
                1.2,
                WIDTH as f32 / HEIGHT as f32,
                0.05,
                50.0,
            );
            let view_matrix = lens
                * glam::camera::rh::view::look_at_mat4(
                    arrival::HOME,
                    middle - Vec3::Y * 0.25,
                    Vec3::Y,
                );
            let head = Mat4::from_translation(arrival::HOME);
            let from = |hand: Vec3, aim: Vec3, grip: f32| Senses {
                focused: true,
                head: Some(head),
                head_pose: Pose::from_transform(head),
                viewer: Some(arrival::HOME),
                space_head: head,
                trigger: Some(0.0),
                beam: true,
                grip: Some(grip),
                ray: tomori_scene::pointing::Ray::new(hand, aim - hand),
                ..Default::default()
            };
            let senses = |aim: Vec3, grip: f32| from(arrival::HOME, aim, grip);
            let mut conversation = joined();
            show_editable_walls(&mut conversation, vec![wall.clone()], panels.clone());
            let mut step = Step::default();
            let now = Instant::now();
            let mut capture = |step: &mut Step,
                               conversation: &mut tomori_scene::conversation::Conversation,
                               senses: &Senses| {
                let frame = step.update(senses, conversation, &mut felt(), now, 0.01);
                draw(&mut renderer, &device, &queue, &mut drawn_panels, &frame);
                renderer.render(
                    &device,
                    &queue,
                    std::slice::from_ref(&view),
                    &[view_matrix],
                    Mat4::IDENTITY,
                    &drawn_panels.list(Mat4::IDENTITY),
                );
                read_back(&device, &queue, &texture, (WIDTH, HEIGHT))
            };
            // Not yet taken hold of: no handles.
            let board = middle + Vec3::Y * 0.42;
            let plain = capture(&mut step, &mut conversation, &senses(board + Vec3::Y, 0.0));
            // Taken hold of with a squeeze on its board, let go, and the knob pointed at.
            capture(&mut step, &mut conversation, &senses(board, 1.0));
            capture(&mut step, &mut conversation, &senses(board, 0.0));
            let held = capture(
                &mut step,
                &mut conversation,
                &senses(point(Handle::Bend), 0.0),
            );
            assert_ne!(held, plain, "its handles and hint must be drawn");
            // The right edge dragged out to where a fourth column's edge would be.
            let four = wall
                .clone()
                .with_grid(tomori_protocol::Grid::new(4, 2).unwrap());
            let edge = frame.transform_point3(Vec3::new(four.board().unwrap().0 / 2.0, 0.0, 0.0));
            capture(
                &mut step,
                &mut conversation,
                &senses(point(Handle::Columns), 0.0),
            );
            capture(
                &mut step,
                &mut conversation,
                &senses(point(Handle::Columns), 1.0),
            );
            let widened = capture(&mut step, &mut conversation, &senses(edge, 1.0));
            capture(&mut step, &mut conversation, &senses(edge, 0.0));
            let sent = conversation.take_actions();
            assert!(
                sent.iter().any(|action| matches!(
                    action,
                    tomori_client::connection::Action::Send(tomori_protocol::ClientMessage::SetWall { grid: Some(grid), .. })
                        if grid.columns == 4
                )),
                "{sent:?}"
            );
            // The knob taken and the hand moved to the right half its travel, bending the wall, as it was before the
            // server answered.
            let mut conversation = joined();
            show_editable_walls(&mut conversation, vec![wall.clone()], panels.clone());
            let mut step = Step::default();
            capture(&mut step, &mut conversation, &senses(board, 1.0));
            capture(&mut step, &mut conversation, &senses(board, 0.0));
            capture(
                &mut step,
                &mut conversation,
                &senses(point(Handle::Bend), 0.0),
            );
            capture(
                &mut step,
                &mut conversation,
                &senses(point(Handle::Bend), 1.0),
            );
            let half = tomori_scene::projection::editing::DEAD_ZONE
                + tomori_scene::projection::editing::TRAVEL / 2.0;
            let drawn = arrival::HOME + Vec3::X * half;
            let bent = capture(
                &mut step,
                &mut conversation,
                &from(drawn, point(Handle::Bend), 1.0),
            );
            assert_ne!(bent, held);
            capture(
                &mut step,
                &mut conversation,
                &from(drawn, point(Handle::Bend), 0.0),
            );
            let sent = conversation.take_actions();
            assert!(
                sent.iter().any(|action| matches!(
                    action,
                    tomori_client::connection::Action::Send(
                        tomori_protocol::ClientMessage::SetWall {
                            shape: Some(tomori_protocol::WallShape::Arc { .. }),
                            ..
                        }
                    )
                )),
                "{sent:?}"
            );
            let mut sheet = image::RgbaImage::new(WIDTH, HEIGHT * 3);
            image::imageops::replace(&mut sheet, &held, 0, 0);
            image::imageops::replace(&mut sheet, &widened, 0, HEIGHT.into());
            image::imageops::replace(&mut sheet, &bent, 0, (HEIGHT * 2).into());
            crate::test_support::preview("TOMORI_WALL_HANDLES_PREVIEW", &sheet);
            assert!(pollster::block_on(scope.pop()).is_none());
        });
    }

    /// Set `TOMORI_WALL_LOCK_PREVIEW` to a PNG path to save what this renders: an environment's own wall, two slots wide
    /// and one high, locked, as an owner near it sees it, no bar and its padlock alone, closed and warm; then unlocked
    /// with a squeeze on it, as it is before the server answers, taken hold of, its padlock open, every handle around it
    /// and the control that puts it back above the padlock, with the hint.
    #[test]
    fn a_locked_wall_shows_an_owner_its_padlock_alone_and_once_unlocked_its_handles_with_the_padlock_open()
     {
        const WIDTH: u32 = 960;
        const HEIGHT: u32 = 720;
        use tomori_protocol::{Panel, Place, Wall, WallKind};
        use tomori_scene::projection::editing::{
            Handle, LOCKED, LockControls, handle_points_with, padlock_point,
        };
        let middle = Vec3::new(-0.2, 1.45, -2.1);
        let wall = Wall::new(
            "environment-0",
            WallKind::Environment,
            tomori_protocol::Pose::upright_facing(middle.to_array(), [-0.2, 1.45, 0.0]),
        )
        .with_grid(tomori_protocol::Grid::new(2, 1).unwrap())
        .with_locked(true);
        let frame =
            Mat4::from_rotation_translation(glam::Quat::from_array(wall.pose.orientation), middle);
        let panels = vec![Panel::new(
            "panel-0",
            "Notices",
            "**Feed** the hens at six.",
            Place::Wall {
                wall: "environment-0".into(),
                slot: 0,
            },
        )];
        let lock = LockControls {
            padlock: true,
            reset: false,
        };
        let (points, _) = handle_points_with(&wall, frame, lock);
        assert_eq!(points.len(), 1);
        // Drawn at the place nearest the owner, at home.
        let padlock = padlock_point(&wall, frame, &[&wall], arrival::HOME).unwrap();
        test_gpu::with(|test_gpu::Gpu { device, queue, .. }| {
            let scope = device.push_error_scope(wgpu::ErrorFilter::Validation);
            let format = wgpu::TextureFormat::Rgba8UnormSrgb;
            let texture = device.create_texture(&wgpu::TextureDescriptor {
                label: Some("wall lock test eye"),
                size: wgpu::Extent3d {
                    width: WIDTH,
                    height: HEIGHT,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
                view_formats: &[],
            });
            let view = texture.create_view(&Default::default());
            let mut renderer = Renderer::new(&device, &queue, format, WIDTH, HEIGHT);
            renderer.set_environment(tomori_scene::environment::Environment::MINIMAL);
            let mut drawn_panels = Panels::new(&device, &renderer);
            let lens = glam::camera::rh::proj::directx::perspective(
                1.2,
                WIDTH as f32 / HEIGHT as f32,
                0.05,
                50.0,
            );
            let view_matrix = lens
                * glam::camera::rh::view::look_at_mat4(
                    arrival::HOME,
                    middle - Vec3::Y * 0.1,
                    Vec3::Y,
                );
            let head = Mat4::from_translation(arrival::HOME);
            let senses = |aim: Vec3, grip: f32| Senses {
                focused: true,
                head: Some(head),
                head_pose: Pose::from_transform(head),
                viewer: Some(arrival::HOME),
                space_head: head,
                trigger: Some(0.0),
                beam: true,
                grip: Some(grip),
                ray: tomori_scene::pointing::Ray::new(arrival::HOME, aim - arrival::HOME),
                ..Default::default()
            };
            let mut conversation = joined();
            show_lockable_walls(&mut conversation, vec![wall.clone()], panels.clone());
            let mut step = Step::default();
            let now = Instant::now();
            let mut capture = |step: &mut Step,
                               conversation: &mut tomori_scene::conversation::Conversation,
                               senses: &Senses| {
                let frame = step.update(senses, conversation, &mut felt(), now, 0.01);
                draw(&mut renderer, &device, &queue, &mut drawn_panels, &frame);
                renderer.render(
                    &device,
                    &queue,
                    std::slice::from_ref(&view),
                    &[view_matrix],
                    Mat4::IDENTITY,
                    &drawn_panels.list(Mat4::IDENTITY),
                );
                read_back(&device, &queue, &texture, (WIDTH, HEIGHT))
            };
            // Drawn with the ray on its board, beside its panel, so that the ray's end hides nothing of the padlock.
            let aside = frame.transform_point3(Vec3::new(0.35, -0.1, 0.0));
            let locked = capture(&mut step, &mut conversation, &senses(aside, 0.0));
            // Squeezed, the padlock unlocks it, sent to the server and shown so at once.
            capture(&mut step, &mut conversation, &senses(padlock, 0.0));
            capture(&mut step, &mut conversation, &senses(padlock, 1.0));
            let sent = conversation.take_actions();
            assert!(
                sent.iter().any(|action| matches!(
                    action,
                    tomori_client::connection::Action::Send(
                        tomori_protocol::ClientMessage::LockWall { locked: false, .. }
                    )
                )),
                "{sent:?}"
            );
            let unlocked_points = handle_points_with(
                &wall.clone().with_locked(false),
                frame,
                LockControls {
                    padlock: true,
                    reset: true,
                },
            )
            .0;
            assert!(
                unlocked_points
                    .iter()
                    .any(|(handle, _)| *handle == Handle::Reset)
            );
            capture(&mut step, &mut conversation, &senses(padlock, 0.0));
            let unlocked = capture(&mut step, &mut conversation, &senses(aside, 0.0));
            assert_ne!(
                locked, unlocked,
                "its handles must be drawn once it is unlocked"
            );
            let mut sheet = image::RgbaImage::new(WIDTH, HEIGHT * 2);
            image::imageops::replace(&mut sheet, &locked, 0, 0);
            image::imageops::replace(&mut sheet, &unlocked, 0, HEIGHT.into());
            crate::test_support::preview("TOMORI_WALL_LOCK_PREVIEW", &sheet);
            // The padlock is warm while locked, and bright once open: the warm colour is drawn only while it is locked.
            let warm = |image: &image::RgbaImage| {
                let [red, green, blue, _] =
                    LOCKED.map(|channel| (channel.powf(1.0 / 2.2) * 255.0) as i32);
                image
                    .pixels()
                    .filter(|pixel| {
                        let [r, g, b, _] = pixel.0.map(i32::from);
                        (r - red).abs() < 30 && (g - green).abs() < 30 && (b - blue).abs() < 40
                    })
                    .count()
            };
            let (closed, open) = (warm(&locked), warm(&unlocked));
            assert!(closed > 4, "{closed}");
            assert!(open * 4 < closed, "{open} {closed}");
            assert!(pollster::block_on(scope.pop()).is_none());
        });
    }

    /// Set `TOMORI_WALL_SHAPES_PREVIEW` to a PNG path to save what this renders: a wall of three tall slots, 9:16, holding a
    /// long list shown whole, a code panel and a short note, from home; and set `TOMORI_WALL_STEPPERS_PREVIEW` to save a
    /// small wall, one slot at half size, and a large one, eight columns of three, each taken hold of, its steppers apart.
    #[test]
    fn walls_of_tall_slots_and_the_steppers_of_small_and_large_walls_are_drawn_apart() {
        const WIDTH: u32 = 960;
        const HEIGHT: u32 = 720;
        use tomori_protocol::{Code, Grid, Panel, Place, Wall, WallKind};
        use tomori_scene::projection::editing::{Handle, handle_points};
        let wall_at = |at: Vec3| {
            Wall::new(
                "wall-1",
                WallKind::Added,
                tomori_protocol::Pose::upright_facing(at.to_array(), [at.x, at.y, 0.0]),
            )
        };
        let middle = Vec3::new(0.0, 1.45, -2.6);
        let tall = wall_at(middle)
            .with_grid(Grid::new(3, 1).unwrap())
            .with_scale(1.0)
            .with_aspect(Some(9.0 / 16.0));
        let on = |slot: u32| Place::Wall {
            wall: "wall-1".into(),
            slot,
        };
        let list = (1..=30)
            .map(|item| format!("- [{}] Step {item}", if item < 6 { "x" } else { " " }))
            .collect::<Vec<_>>()
            .join("\n");
        let code = (1..=40)
            .map(|line| {
                format!("    let value_{line} = compute({line}); // step {line} of the plan")
            })
            .collect::<Vec<_>>()
            .join("\n");
        let shown = vec![
            Panel::new("panel-0", "Plan", list, on(0)),
            Panel::new("panel-1", "rust", code, on(1)).with_code(Code::new("rust", 40)),
            Panel::new("panel-2", "Note", "A short note in a tall slot.", on(2)),
        ];
        test_gpu::with(|test_gpu::Gpu { device, queue, .. }| {
            let scope = device.push_error_scope(wgpu::ErrorFilter::Validation);
            let format = wgpu::TextureFormat::Rgba8UnormSrgb;
            let texture = device.create_texture(&wgpu::TextureDescriptor {
                label: Some("wall shapes test eye"),
                size: wgpu::Extent3d {
                    width: WIDTH,
                    height: HEIGHT,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
                view_formats: &[],
            });
            let view = texture.create_view(&Default::default());
            let mut renderer = Renderer::new(&device, &queue, format, WIDTH, HEIGHT);
            renderer.set_environment(tomori_scene::environment::Environment::MINIMAL);
            let mut drawn_panels = Panels::new(&device, &renderer);
            let lens = glam::camera::rh::proj::directx::perspective(
                1.3,
                WIDTH as f32 / HEIGHT as f32,
                0.05,
                50.0,
            );
            let head = Mat4::from_translation(arrival::HOME);
            let senses = |aim: Vec3, grip: f32| Senses {
                focused: true,
                head: Some(head),
                head_pose: Pose::from_transform(head),
                viewer: Some(arrival::HOME),
                space_head: head,
                trigger: Some(0.0),
                beam: true,
                grip: Some(grip),
                ray: tomori_scene::pointing::Ray::new(arrival::HOME, aim - arrival::HOME),
                ..Default::default()
            };
            let now = Instant::now();
            let mut capture = |step: &mut Step,
                               conversation: &mut tomori_scene::conversation::Conversation,
                               senses: &Senses,
                               looking: Vec3| {
                let frame = step.update(senses, conversation, &mut felt(), now, 0.01);
                draw(&mut renderer, &device, &queue, &mut drawn_panels, &frame);
                renderer.render(
                    &device,
                    &queue,
                    std::slice::from_ref(&view),
                    &[
                        lens * glam::camera::rh::view::look_at_mat4(
                            arrival::HOME,
                            looking,
                            Vec3::Y,
                        ),
                    ],
                    Mat4::IDENTITY,
                    &drawn_panels.list(Mat4::IDENTITY),
                );
                read_back(&device, &queue, &texture, (WIDTH, HEIGHT))
            };
            // Three tall slots, their panels filling them: the list shown to its last item.
            let mut conversation = joined();
            show_editable_walls(&mut conversation, vec![tall.clone()], shown.clone());
            let mut step = Step::default();
            let aside = senses(middle + Vec3::new(2.0, 0.0, 0.0), 0.0);
            let shapes = capture(&mut step, &mut conversation, &aside, middle);
            let mut conversation = joined();
            show_editable_walls(&mut conversation, vec![tall.clone()], Vec::new());
            let empty = capture(&mut Step::default(), &mut conversation, &aside, middle);
            assert_ne!(shapes, empty);
            // The list's last item is drawn low in its tall slot, below where a standard slot ends.
            let frame_of = |wall: &Wall| {
                Mat4::from_rotation_translation(
                    glam::Quat::from_array(wall.pose.orientation),
                    Vec3::from_array(wall.pose.position),
                )
            };
            let slot = tomori_scene::projection::slot_frames(&tall, frame_of(&tall))[0];
            let (_, height) = tall.slot_size();
            let low = slot.transform_point3(Vec3::new(0.0, -(height / 2.0 - 0.08), 0.0));
            let clip = lens
                * glam::camera::rh::view::look_at_mat4(arrival::HOME, middle, Vec3::Y)
                * low.extend(1.0);
            let (x, y) = (
                ((clip.x / clip.w + 1.0) / 2.0 * WIDTH as f32) as u32,
                ((1.0 - clip.y / clip.w) / 2.0 * HEIGHT as f32) as u32,
            );
            let around = |image: &image::RgbaImage| -> Vec<[u8; 4]> {
                (x.saturating_sub(40)..x + 40)
                    .flat_map(|x| (y.saturating_sub(6)..y + 6).map(move |y| (x, y)))
                    .map(|(x, y)| image.get_pixel(x, y).0)
                    .collect()
            };
            assert_ne!(
                around(&shapes),
                around(&empty),
                "the list fills its tall slot"
            );
            crate::test_support::preview("TOMORI_WALL_SHAPES_PREVIEW", &shapes);
            // A small wall and a large one, each taken hold of on its board, the ray on its column steppers' middle.
            let small = wall_at(Vec3::new(0.0, 1.45, -2.0))
                .with_grid(Grid::new(1, 1).unwrap())
                .with_scale(0.5);
            let large = wall_at(Vec3::new(0.0, 1.6, -4.5))
                .with_grid(Grid::new(8, 3).unwrap())
                .with_scale(1.0);
            let mut pictures = Vec::new();
            for wall in [small, large] {
                let mut conversation = joined();
                show_editable_walls(&mut conversation, vec![wall.clone()], Vec::new());
                let mut step = Step::default();
                let centre = Vec3::from_array(wall.pose.position);
                let (_, board_height) = wall.board().unwrap();
                let board = centre + Vec3::Y * (board_height / 2.0 - 0.01);
                capture(&mut step, &mut conversation, &senses(board, 1.0), centre);
                capture(&mut step, &mut conversation, &senses(board, 0.0), centre);
                let (points, _) = handle_points(&wall, frame_of(&wall));
                let point =
                    |handle: Handle| points.iter().find(|(shown, _)| *shown == handle).unwrap().1;
                let more = point(Handle::Step(
                    tomori_scene::projection::editing::Axis::Columns,
                    1,
                ));
                pictures.push(capture(
                    &mut step,
                    &mut conversation,
                    &senses(more, 0.0),
                    centre,
                ));
            }
            let mut sheet = image::RgbaImage::new(WIDTH, HEIGHT * 2);
            image::imageops::replace(&mut sheet, &pictures[0], 0, 0);
            image::imageops::replace(&mut sheet, &pictures[1], 0, HEIGHT.into());
            crate::test_support::preview("TOMORI_WALL_STEPPERS_PREVIEW", &sheet);
            assert!(pollster::block_on(scope.pop()).is_none());
        });
    }

    /// Set `TOMORI_WALL_HANDLES_LAYOUT_PREVIEW` to a PNG path to save what this renders: four walls, each taken hold of,
    /// its handles where its own frame puts them: a flat wall of three by two, a curved wall of four, a wall above tilted
    /// down towards the participant, and a small wall, one slot at half size, whose frame is as large as its handles need.
    #[test]
    fn walls_taken_hold_of_draw_every_handle_where_their_frame_puts_it() {
        const WIDTH: u32 = 640;
        const HEIGHT: u32 = 480;
        use tomori_protocol::{Grid, Wall, WallKind, WallShape};
        use tomori_scene::projection::editing::handle_points;
        let wall_at = |at: Vec3| {
            Wall::new(
                "wall-1",
                WallKind::Added,
                tomori_protocol::Pose::upright_facing(at.to_array(), [at.x, at.y, 0.0]),
            )
        };
        let walls = [
            wall_at(Vec3::new(0.0, 1.5, -2.6)),
            wall_at(Vec3::new(0.0, 1.5, -2.4))
                .with_grid(Grid::new(4, 1).unwrap())
                .with_scale(0.8)
                .with_shape(WallShape::Arc { radius: 2.0 }),
            wall_at(Vec3::new(0.0, 2.3, -2.4)).with_tilt(Some(25.0)),
            wall_at(Vec3::new(0.0, 1.5, -1.3))
                .with_grid(Grid::new(1, 1).unwrap())
                .with_scale(0.5),
        ];
        test_gpu::with(|test_gpu::Gpu { device, queue, .. }| {
            let scope = device.push_error_scope(wgpu::ErrorFilter::Validation);
            let format = wgpu::TextureFormat::Rgba8UnormSrgb;
            let texture = device.create_texture(&wgpu::TextureDescriptor {
                label: Some("wall handles layout test eye"),
                size: wgpu::Extent3d {
                    width: WIDTH,
                    height: HEIGHT,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
                view_formats: &[],
            });
            let view = texture.create_view(&Default::default());
            let mut renderer = Renderer::new(&device, &queue, format, WIDTH, HEIGHT);
            renderer.set_environment(tomori_scene::environment::Environment::MINIMAL);
            let mut drawn_panels = Panels::new(&device, &renderer);
            let lens = glam::camera::rh::proj::directx::perspective(
                1.2,
                WIDTH as f32 / HEIGHT as f32,
                0.05,
                50.0,
            );
            let head = Mat4::from_translation(arrival::HOME);
            let senses = |aim: Vec3, grip: f32| Senses {
                focused: true,
                head: Some(head),
                head_pose: Pose::from_transform(head),
                viewer: Some(arrival::HOME),
                space_head: head,
                trigger: Some(0.0),
                beam: true,
                grip: Some(grip),
                ray: tomori_scene::pointing::Ray::new(arrival::HOME, aim - arrival::HOME),
                ..Default::default()
            };
            let now = Instant::now();
            let mut capture = |step: &mut Step,
                               conversation: &mut tomori_scene::conversation::Conversation,
                               senses: &Senses,
                               looking: Vec3| {
                let frame = step.update(senses, conversation, &mut felt(), now, 0.01);
                draw(&mut renderer, &device, &queue, &mut drawn_panels, &frame);
                renderer.render(
                    &device,
                    &queue,
                    std::slice::from_ref(&view),
                    &[
                        lens * glam::camera::rh::view::look_at_mat4(
                            arrival::HOME,
                            looking,
                            Vec3::Y,
                        ),
                    ],
                    Mat4::IDENTITY,
                    &drawn_panels.list(Mat4::IDENTITY),
                );
                read_back(&device, &queue, &texture, (WIDTH, HEIGHT))
            };
            let mut sheet = image::RgbaImage::new(WIDTH * 2, HEIGHT * 2);
            for (index, wall) in walls.iter().enumerate() {
                let mut conversation = joined();
                show_editable_walls(&mut conversation, vec![wall.clone()], Vec::new());
                let mut step = Step::default();
                let pose = wall.frame();
                let frame = Mat4::from_rotation_translation(
                    glam::Quat::from_array(pose.orientation),
                    Vec3::from_array(pose.position),
                );
                let centre = frame.transform_point3(Vec3::ZERO);
                // Taken hold of by its first slot, empty, the ray then at the sky.
                let slot = tomori_scene::projection::slot_frames(wall, frame)[0]
                    .transform_point3(Vec3::ZERO);
                let sky = arrival::HOME + Vec3::new(0.0, 3.0, 0.5);
                let before = capture(&mut step, &mut conversation, &senses(sky, 0.0), centre);
                capture(&mut step, &mut conversation, &senses(slot, 1.0), centre);
                capture(&mut step, &mut conversation, &senses(slot, 0.0), centre);
                let held = capture(&mut step, &mut conversation, &senses(sky, 0.0), centre);
                // Every handle is drawn where its point is: what it draws over the wall, within most of a handle of
                // its middle, as seen, is centred on its middle.
                let (points, size) = handle_points(wall, frame);
                assert!(points.len() >= 12);
                let eye =
                    lens * glam::camera::rh::view::look_at_mat4(arrival::HOME, centre, Vec3::Y);
                let focal = HEIGHT as f32 / 2.0 / (1.2f32 / 2.0).tan();
                for (handle, at) in &points {
                    let clip = eye * at.extend(1.0);
                    let (x, y) = (
                        (clip.x / clip.w + 1.0) / 2.0 * WIDTH as f32,
                        (1.0 - clip.y / clip.w) / 2.0 * HEIGHT as f32,
                    );
                    let reach = 0.7 * size / at.distance(arrival::HOME) * focal;
                    let (mut sum, mut count) = (glam::Vec2::ZERO, 0.0);
                    for px in (x - reach).max(0.0) as u32..((x + reach) as u32).min(WIDTH) {
                        for py in (y - reach).max(0.0) as u32..((y + reach) as u32).min(HEIGHT) {
                            if held.get_pixel(px, py) != before.get_pixel(px, py) {
                                sum += glam::Vec2::new(px as f32 + 0.5, py as f32 + 0.5);
                                count += 1.0;
                            }
                        }
                    }
                    assert!(count > reach * reach, "{handle:?} of wall {index} drawn");
                    let middle = sum / count;
                    assert!(
                        middle.distance(glam::Vec2::new(x, y)) < (0.15 * reach).max(1.5),
                        "{handle:?} of wall {index} drawn at {middle}, not {x} {y}"
                    );
                }
                image::imageops::replace(
                    &mut sheet,
                    &held,
                    (index as u32 % 2 * WIDTH).into(),
                    (index as u32 / 2 * HEIGHT).into(),
                );
            }
            crate::test_support::preview("TOMORI_WALL_HANDLES_LAYOUT_PREVIEW", &sheet);
            assert!(pollster::block_on(scope.pop()).is_none());
        });
    }

    /// Set `TOMORI_WALL_GROUP_PREVIEW` to a PNG path to save what this renders: a wall two by two and a big one by one
    /// snapped to its right, a panel in every slot, drawn as one board, with the same walls drawn apart below for
    /// comparison; `TOMORI_WALL_CURVED_GROUP_PREVIEW`, a curved group of three about home, labelled once; and
    /// `TOMORI_WALL_SNAP_PREVIEW`, the big wall carried mid-way to the other's edge, where it would snap outlined; and
    /// `TOMORI_WALL_STACKED_GROUP_PREVIEW`, a wide wall with two tall walls snapped above its ends, near and far, drawn for
    /// someone who may adjust walls, its one bar above the top left tall wall.
    #[test]
    fn walls_snapped_together_read_as_one_board_and_a_wall_carried_near_one_shows_where_it_snaps() {
        const WIDTH: u32 = 960;
        const HEIGHT: u32 = 720;
        use tomori_protocol::{
            Align, Edge, Grid, Join, Panel, Place, Wall, WallKind, WallShape, group,
        };
        let added =
            |id: &str, pose: tomori_protocol::Pose, (columns, rows): (u32, u32), scale: f32| {
                Wall::new(id, WallKind::Added, pose)
                    .with_grid(Grid::new(columns, rows).unwrap())
                    .with_scale(scale)
            };
        let two = added(
            "wall-1",
            tomori_protocol::Pose::upright_facing([-0.75, 1.45, -2.6], [-0.75, 1.45, 0.0]),
            (2, 2),
            0.95,
        );
        let mut big = added("wall-2", two.pose, (1, 1), 1.9);
        big.pose = group::attached_pose(&two, &big, Edge::Right, Align::Start).unwrap();
        let grouped = vec![
            two.clone().with_group(
                Some("wall-1".into()),
                vec![Join::new(Edge::Right, "wall-2")],
            ),
            big.clone()
                .with_group(Some("wall-1".into()), vec![Join::new(Edge::Left, "wall-1")]),
        ];
        let filling = |wall: &str, slots: u32, offset: usize| -> Vec<Panel> {
            (0..slots)
                .map(|slot| {
                    let titles = ["Build", "Tests", "Coverage", "Release", "Overview"];
                    let title = titles[(slot as usize + offset) % titles.len()];
                    Panel::new(
                        format!("{wall}-{slot}"),
                        title,
                        format!("**{title}** is green.\n\n- one\n- two"),
                        Place::Wall {
                            wall: wall.into(),
                            slot,
                        },
                    )
                })
                .collect()
        };
        let mut panels = filling("wall-1", 4, 0);
        panels.extend(filling("wall-2", 1, 4));
        // A curved group about home: two of two columns side by side, and one above the first.
        let radius = 2.4;
        let first = added(
            "wall-3",
            tomori_protocol::Pose::upright_facing([0.0, 1.2, -radius], [0.0, 1.2, 0.0]),
            (2, 1),
            0.8,
        )
        .with_shape(WallShape::Arc { radius });
        let mut beside =
            added("wall-4", first.pose, (2, 1), 0.8).with_shape(WallShape::Arc { radius });
        beside.pose = group::attached_pose(&first, &beside, Edge::Right, Align::Start).unwrap();
        let mut above =
            added("wall-5", first.pose, (3, 1), 0.8).with_shape(WallShape::Arc { radius });
        above.pose = group::attached_pose(&first, &above, Edge::Top, Align::Start).unwrap();
        let curved: Vec<Wall> = [first, beside, above]
            .into_iter()
            .map(|wall| {
                let joined = Vec::new();
                wall.with_group(Some("wall-3".into()), joined)
            })
            .collect();
        let mut curved_panels = filling("wall-3", 2, 0);
        curved_panels.extend(filling("wall-4", 2, 2));
        curved_panels.extend(filling("wall-5", 3, 1));
        let frame_of = |wall: &Wall| {
            Mat4::from_rotation_translation(
                glam::Quat::from_array(wall.pose.orientation),
                Vec3::from_array(wall.pose.position),
            )
        };
        test_gpu::with(|test_gpu::Gpu { device, queue, .. }| {
            let scope = device.push_error_scope(wgpu::ErrorFilter::Validation);
            let format = wgpu::TextureFormat::Rgba8UnormSrgb;
            let texture = device.create_texture(&wgpu::TextureDescriptor {
                label: Some("wall group test eye"),
                size: wgpu::Extent3d {
                    width: WIDTH,
                    height: HEIGHT,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
                view_formats: &[],
            });
            let view = texture.create_view(&Default::default());
            let mut renderer = Renderer::new(&device, &queue, format, WIDTH, HEIGHT);
            renderer.set_environment(tomori_scene::environment::Environment::MINIMAL);
            let mut drawn_panels = Panels::new(&device, &renderer);
            let lens = glam::camera::rh::proj::directx::perspective(
                1.1,
                WIDTH as f32 / HEIGHT as f32,
                0.05,
                50.0,
            );
            let middle = Vec3::new(0.05, 1.45, -2.6);
            let at_group =
                lens * glam::camera::rh::view::look_at_mat4(arrival::HOME, middle, Vec3::Y);
            let at_curved = lens
                * glam::camera::rh::view::look_at_mat4(
                    arrival::HOME,
                    Vec3::new(0.3, 1.45, -2.4),
                    Vec3::Y,
                );
            let head = Mat4::from_translation(arrival::HOME);
            let senses = |aim: Vec3, grip: f32| Senses {
                focused: true,
                head: Some(head),
                head_pose: Pose::from_transform(head),
                viewer: Some(arrival::HOME),
                space_head: head,
                trigger: Some(0.0),
                beam: true,
                grip: Some(grip),
                ray: tomori_scene::pointing::Ray::new(arrival::HOME, aim - arrival::HOME),
                ..Default::default()
            };
            let away = senses(arrival::HOME - Vec3::Y, 0.0);
            let mut capture = |step: &mut Step,
                               conversation: &mut tomori_scene::conversation::Conversation,
                               senses: &Senses,
                               matrix: Mat4| {
                let frame = step.update(senses, conversation, &mut felt(), Instant::now(), 0.01);
                draw(&mut renderer, &device, &queue, &mut drawn_panels, &frame);
                renderer.render(
                    &device,
                    &queue,
                    std::slice::from_ref(&view),
                    &[matrix],
                    Mat4::IDENTITY,
                    &drawn_panels.list(Mat4::IDENTITY),
                );
                read_back(&device, &queue, &texture, (WIDTH, HEIGHT))
            };
            let mut shown = |walls: Vec<Wall>, panels: Vec<Panel>, matrix: Mat4| {
                let mut conversation = joined();
                show_projection(&mut conversation, walls, panels);
                capture(&mut Step::default(), &mut conversation, &away, matrix)
            };
            let one = shown(grouped.clone(), panels.clone(), at_group);
            let apart = shown(vec![two.clone(), big.clone()], panels.clone(), at_group);
            // Where the two meet, halfway up the shorter, in pixels.
            let pixel = |at: Vec3| {
                let clip = at_group * at.extend(1.0);
                let (x, y) = (clip.x / clip.w, clip.y / clip.w);
                (
                    ((x + 1.0) / 2.0 * WIDTH as f32) as u32,
                    ((1.0 - y) / 2.0 * HEIGHT as f32) as u32,
                )
            };
            let slots = big.extent();
            let (_, y, _, _) = group::on_surface(&two.pose, None, &big.pose);
            let seam = frame_of(&two).transform_point3(Vec3::new(
                two.extent().right + big.gap() / 2.0,
                (slots.bottom + slots.top) / 2.0 + y,
                0.0,
            ));
            let (x, y) = pixel(seam);
            // Across the seam, the group's board is as dark as it is either side, with no border; apart, each wall's
            // own border and margin show between them.
            let brightest = |image: &image::RgbaImage| {
                (x - 10..x + 10)
                    .map(|x| {
                        let [r, g, b, _] = image.get_pixel(x, y).0;
                        u32::from(r) + u32::from(g) + u32::from(b)
                    })
                    .max()
                    .unwrap()
            };
            let side = |image: &image::RgbaImage| {
                let [r, g, b, _] = image.get_pixel(x - 40, y).0;
                u32::from(r) + u32::from(g) + u32::from(b)
            };
            assert!(brightest(&one) <= side(&one) + 30);
            assert!(brightest(&apart) > brightest(&one) + 30);
            let mut sheet = image::RgbaImage::new(WIDTH, HEIGHT * 2);
            image::imageops::replace(&mut sheet, &one, 0, 0);
            image::imageops::replace(&mut sheet, &apart, 0, HEIGHT.into());
            crate::test_support::preview("TOMORI_WALL_GROUP_PREVIEW", &sheet);
            let curved_group = shown(curved.clone(), curved_panels.clone(), at_curved);
            assert_ne!(curved_group, shown(Vec::new(), Vec::new(), at_curved));
            crate::test_support::preview("TOMORI_WALL_CURVED_GROUP_PREVIEW", &curved_group);
            // The big wall carried from a little to its right towards the other's edge: where it would snap is outlined.
            let mut apart_big = big.clone();
            let swing = glam::Quat::from_rotation_y(-0.07);
            apart_big.pose.position =
                (swing * (Vec3::from_array(big.pose.position) - arrival::HOME) + arrival::HOME)
                    .to_array();
            let grip_at = |pose: &tomori_protocol::Pose| {
                let (_, height) = big.board().unwrap();
                Mat4::from_rotation_translation(
                    glam::Quat::from_array(pose.orientation),
                    Vec3::from_array(pose.position),
                )
                .transform_point3(Vec3::new(0.0, height / 2.0 - 0.005, 0.0))
            };
            let mut conversation = joined();
            show_editable_walls(
                &mut conversation,
                vec![two.clone(), apart_big.clone()],
                panels.clone(),
            );
            let mut step = Step::default();
            let start = grip_at(&apart_big.pose);
            capture(&mut step, &mut conversation, &senses(start, 1.0), at_group);
            let swing = glam::Quat::from_rotation_y(-0.03);
            let midway = swing * (grip_at(&big.pose) - arrival::HOME) + arrival::HOME;
            let carried = capture(&mut step, &mut conversation, &senses(midway, 1.0), at_group);
            assert_ne!(carried, apart);
            crate::test_support::preview("TOMORI_WALL_SNAP_PREVIEW", &carried);
            // The maintainer's arrangement, a wide wall below and two tall walls snapped above its ends, near and far:
            // drawn for someone who may adjust walls, its one bar shows above the top left tall wall, and every slot
            // looks exactly as it does without the bar, which covers none of them.
            let mut stacked_sheet = image::RgbaImage::new(WIDTH, HEIGHT * 2);
            for (row, distance) in [2.6_f32, 9.0].into_iter().enumerate() {
                let base =
                    tomori_protocol::Pose::upright_facing([0.0, 1.0, -distance], [0.0, 1.0, 0.0]);
                let wide = added("wall-6", base, (4, 1), 1.0);
                let tall = |id: &str, align| {
                    let mut tall = added(id, base, (1, 1), 1.0).with_aspect(Some(0.5));
                    tall.pose = group::attached_pose(&wide, &tall, Edge::Top, align).unwrap();
                    tall
                };
                let (left, right) = (tall("wall-7", Align::Start), tall("wall-8", Align::End));
                let joins = |wall: &Wall, others: [&Wall; 2]| {
                    others
                        .into_iter()
                        .filter_map(|other| {
                            group::joined(wall, other)
                                .map(|(edge, ..)| Join::new(edge, other.id.clone()))
                        })
                        .collect::<Vec<_>>()
                };
                let walls = vec![
                    wide.clone()
                        .with_group(Some("wall-6".into()), joins(&wide, [&left, &right])),
                    left.clone()
                        .with_group(Some("wall-6".into()), joins(&left, [&wide, &right])),
                    right
                        .clone()
                        .with_group(Some("wall-6".into()), joins(&right, [&wide, &left])),
                ];
                let mut stacked_panels = filling("wall-6", 4, 0);
                stacked_panels.extend(filling("wall-7", 1, 1));
                stacked_panels.extend(filling("wall-8", 1, 2));
                let looking = lens
                    * glam::camera::rh::view::look_at_mat4(
                        arrival::HOME,
                        Vec3::new(0.0, 1.0 + 0.1 * distance, -distance),
                        Vec3::Y,
                    );
                let mut conversation = joined();
                show_projection(&mut conversation, walls.clone(), stacked_panels.clone());
                let plain = capture(&mut Step::default(), &mut conversation, &away, looking);
                let mut conversation = joined();
                show_editable_walls(&mut conversation, walls.clone(), stacked_panels.clone());
                let editable = capture(&mut Step::default(), &mut conversation, &away, looking);
                assert_ne!(plain, editable, "the bar is drawn at {distance} m");
                let pixel = |at: Vec3| {
                    let clip = looking * at.extend(1.0);
                    let (x, y) = (clip.x / clip.w, clip.y / clip.w);
                    (
                        ((x + 1.0) / 2.0 * WIDTH as f32) as u32,
                        ((1.0 - y) / 2.0 * HEIGHT as f32) as u32,
                    )
                };
                for wall in &walls {
                    for slot in wall.slot_frames() {
                        let ([x, y, _], [width, height]) = (slot.position, slot.size);
                        for (across, up) in (0..=4).flat_map(|a| (0..=4).map(move |b| (a, b))) {
                            let at = frame_of(wall).transform_point3(Vec3::new(
                                x + width * (across as f32 / 4.0 - 0.5) * 0.98,
                                y + height * (up as f32 / 4.0 - 0.5) * 0.98,
                                0.0,
                            ));
                            let (px, py) = pixel(at);
                            assert_eq!(
                                plain.get_pixel(px, py),
                                editable.get_pixel(px, py),
                                "{} at {distance} m, {across} across and {up} up",
                                wall.id
                            );
                        }
                    }
                }
                image::imageops::replace(
                    &mut stacked_sheet,
                    &editable,
                    0,
                    (HEIGHT * row as u32).into(),
                );
            }
            crate::test_support::preview("TOMORI_WALL_STACKED_GROUP_PREVIEW", &stacked_sheet);
            assert!(pollster::block_on(scope.pop()).is_none());
        });
    }

    /// Set `TOMORI_WALL_BAR_PREVIEW` to a PNG path to save what this renders: a curved wall someone added, four columns
    /// of two rows, a panel in every slot, its bar above it, subtle; the ray on the bar, highlighted, saying what the grip
    /// does; the wall taken hold of by its bar and let go, its handles around it and the bar still there, the knob a row
    /// above it; and the Tomori wall, every slot full, its bar below its label, the ray on it.
    #[test]
    fn a_full_curved_wall_shows_its_bar_hovered_and_held_and_the_tomori_wall_s_below_it() {
        const WIDTH: u32 = 960;
        const HEIGHT: u32 = 720;
        use tomori_protocol::{Grid, Panel, Place, Wall, WallKind, WallShape};
        use tomori_scene::projection::editing::bar;
        let middle = Vec3::new(0.0, 1.5, -2.6);
        let wall = Wall::new(
            "wall-1",
            WallKind::Added,
            tomori_protocol::Pose::upright_facing(middle.to_array(), [0.0, 1.5, 0.0]),
        )
        .with_grid(Grid::new(4, 2).unwrap())
        .with_shape(WallShape::Arc { radius: 2.0 });
        let filling = |wall: &'static str, slots: u32| {
            (0..slots).map(move |slot| {
                Panel::new(
                    format!("{wall}-{slot}"),
                    format!("Topic {}", slot + 1),
                    format!("- **Point** {}\n- Another point\n- And one more", slot + 1),
                    Place::Wall {
                        wall: wall.into(),
                        slot,
                    },
                )
            })
        };
        let tomori = tomori_scene::scene::fixtures::tomori_wall();
        let panels: Vec<Panel> = filling("wall-1", 8)
            .chain(filling(tomori_protocol::TOMORI_WALL, tomori.grid().slots()))
            .collect();
        let frame_of = |wall: &Wall| {
            Mat4::from_rotation_translation(
                glam::Quat::from_array(wall.pose.orientation),
                Vec3::from_array(wall.pose.position),
            )
        };
        let curved = bar(&wall, frame_of(&wall));
        let below = bar(&tomori, frame_of(&tomori));
        test_gpu::with(|test_gpu::Gpu { device, queue, .. }| {
            let scope = device.push_error_scope(wgpu::ErrorFilter::Validation);
            let format = wgpu::TextureFormat::Rgba8UnormSrgb;
            let texture = device.create_texture(&wgpu::TextureDescriptor {
                label: Some("wall bar test eye"),
                size: wgpu::Extent3d {
                    width: WIDTH,
                    height: HEIGHT,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
                view_formats: &[],
            });
            let view = texture.create_view(&Default::default());
            let mut renderer = Renderer::new(&device, &queue, format, WIDTH, HEIGHT);
            renderer.set_environment(tomori_scene::environment::Environment::MINIMAL);
            let mut drawn_panels = Panels::new(&device, &renderer);
            let lens = glam::camera::rh::proj::directx::perspective(
                1.2,
                WIDTH as f32 / HEIGHT as f32,
                0.05,
                50.0,
            );
            let head = Mat4::from_translation(arrival::HOME);
            let senses = |aim: Vec3, grip: f32| Senses {
                focused: true,
                head: Some(head),
                head_pose: Pose::from_transform(head),
                viewer: Some(arrival::HOME),
                space_head: head,
                trigger: Some(0.0),
                beam: true,
                grip: Some(grip),
                ray: tomori_scene::pointing::Ray::new(arrival::HOME, aim - arrival::HOME),
                ..Default::default()
            };
            let now = Instant::now();
            let mut capture = |step: &mut Step,
                               conversation: &mut tomori_scene::conversation::Conversation,
                               senses: &Senses,
                               looking: Vec3| {
                let frame = step.update(senses, conversation, &mut felt(), now, 0.01);
                draw(&mut renderer, &device, &queue, &mut drawn_panels, &frame);
                let eye =
                    lens * glam::camera::rh::view::look_at_mat4(arrival::HOME, looking, Vec3::Y);
                renderer.render(
                    &device,
                    &queue,
                    std::slice::from_ref(&view),
                    &[eye],
                    Mat4::IDENTITY,
                    &drawn_panels.list(Mat4::IDENTITY),
                );
                (read_back(&device, &queue, &texture, (WIDTH, HEIGHT)), eye)
            };
            // The pixels about where `point` is seen through `eye`.
            let around = |(image, eye): &(image::RgbaImage, Mat4), point: Vec3| -> Vec<[u8; 4]> {
                let clip = *eye * point.extend(1.0);
                let (x, y) = (
                    ((clip.x / clip.w + 1.0) / 2.0 * WIDTH as f32) as u32,
                    ((1.0 - clip.y / clip.w) / 2.0 * HEIGHT as f32) as u32,
                );
                (x.saturating_sub(20)..x + 20)
                    .flat_map(|x| (y.saturating_sub(2)..y + 2).map(move |y| (x, y)))
                    .map(|(x, y)| image.get_pixel(x, y).0)
                    .collect()
            };
            let looking = middle + Vec3::Y * 0.1;
            let aside = arrival::HOME + Vec3::new(3.0, 2.0, 0.0);
            // A visitor sees no bar.
            let mut conversation = joined();
            tomori_scene::scene::fixtures::show_projection(
                &mut conversation,
                vec![wall.clone()],
                panels.clone(),
            );
            let visitor = capture(
                &mut Step::default(),
                &mut conversation,
                &senses(aside, 0.0),
                looking,
            );
            // A participant who may adjust walls sees it, subtly, then highlighted as the ray meets it.
            let mut conversation = joined();
            show_editable_walls(&mut conversation, vec![wall.clone()], panels.clone());
            let mut step = Step::default();
            let plain = capture(&mut step, &mut conversation, &senses(aside, 0.0), looking);
            assert_ne!(
                around(&plain, curved.middle),
                around(&visitor, curved.middle),
                "the bar is drawn"
            );
            let hovered = capture(
                &mut step,
                &mut conversation,
                &senses(curved.middle, 0.0),
                looking,
            );
            assert_ne!(
                around(&hovered, curved.middle),
                around(&plain, curved.middle)
            );
            // Squeezed, the wall is carried; let go where it was, it is held, its handles shown.
            capture(
                &mut step,
                &mut conversation,
                &senses(curved.middle, 1.0),
                looking,
            );
            capture(
                &mut step,
                &mut conversation,
                &senses(curved.middle, 0.0),
                looking,
            );
            let held = capture(&mut step, &mut conversation, &senses(aside, 0.0), looking);
            assert_ne!(held.0, plain.0, "its handles and hint are drawn");
            // The Tomori wall, full, its bar below it, the ray on it.
            let mut step = Step::default();
            let low = capture(
                &mut step,
                &mut conversation,
                &senses(below.middle, 0.0),
                Vec3::from_array(tomori.pose.position),
            );
            let mut sheet = image::RgbaImage::new(WIDTH, HEIGHT * 4);
            for (row, (image, _)) in [&plain, &hovered, &held, &low].into_iter().enumerate() {
                image::imageops::replace(&mut sheet, image, 0, (HEIGHT * row as u32).into());
            }
            crate::test_support::preview("TOMORI_WALL_BAR_PREVIEW", &sheet);
            assert!(pollster::block_on(scope.pop()).is_none());
        });
    }

    /// Set `TOMORI_CARRY_HINT_PREVIEW` to a PNG path to save what this renders, at about the headset's resolution: a
    /// panel carried 1.6 m ahead, with the hint below it saying what each control does, then carried over a wall's slot,
    /// the hint saying it would swap, and a Rust code panel on the Tomori wall whose long lines wrap under a hanging
    /// indent, close up from 0.5 m, then the same from home.
    #[test]
    fn a_panel_carried_shows_its_hint_and_a_code_panel_wraps_its_long_lines() {
        const WIDTH: u32 = 960;
        const HEIGHT: u32 = 720;
        test_gpu::with(|test_gpu::Gpu { device, queue, .. }| {
            let scope = device.push_error_scope(wgpu::ErrorFilter::Validation);
            let format = wgpu::TextureFormat::Rgba8UnormSrgb;
            let texture = device.create_texture(&wgpu::TextureDescriptor {
                label: Some("carry hint test eye"),
                size: wgpu::Extent3d {
                    width: WIDTH,
                    height: HEIGHT,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
                view_formats: &[],
            });
            let view = texture.create_view(&Default::default());
            let mut renderer = Renderer::new(&device, &queue, format, WIDTH, HEIGHT);
            renderer.set_environment(tomori_scene::environment::Environment::MINIMAL);
            let mut panels = Panels::new(&device, &renderer);
            // Lines of Rust longer than a code panel's 45 columns, with spans of their kinds that cross where they wrap.
            let rust = [
                "async fn fetch(client: &Client, request: Request) -> Result<()> {",
                "    let response = client.send(request, timeout(30)).await?;",
                "    log::info!(\"sent {} and got {}\", response.id, response.code);",
                "    Ok(())",
                "}",
            ]
            .join("\n");
            let mut spans = Vec::new();
            let mut from = 0;
            for (text, kind) in [
                ("async", tomori_protocol::TokenKind::Keyword),
                ("fn", tomori_protocol::TokenKind::Keyword),
                ("fetch", tomori_protocol::TokenKind::Function),
                ("Client", tomori_protocol::TokenKind::Type),
                ("Request", tomori_protocol::TokenKind::Type),
                ("Result", tomori_protocol::TokenKind::Type),
                ("let", tomori_protocol::TokenKind::Keyword),
                ("send", tomori_protocol::TokenKind::Function),
                ("timeout", tomori_protocol::TokenKind::Function),
                ("30", tomori_protocol::TokenKind::Number),
                ("await", tomori_protocol::TokenKind::Keyword),
                ("info!", tomori_protocol::TokenKind::Function),
                ("\"sent {} and got {}\"", tomori_protocol::TokenKind::String),
                ("Ok", tomori_protocol::TokenKind::Type),
            ] {
                let at = from + rust[from..].find(text).unwrap();
                spans.push(tomori_protocol::Span::new(
                    rust[..at].chars().count() as u32,
                    text.chars().count() as u32,
                    kind,
                ));
                from = at + text.len();
            }
            assert!(tomori_protocol::valid_spans(&rust, &spans));
            let code = tomori_protocol::Panel::new(
                "panel-2",
                "rust",
                rust.clone(),
                tomori_protocol::Place::Wall {
                    wall: tomori_protocol::TOMORI_WALL.into(),
                    slot: 2,
                },
            )
            .with_code(tomori_protocol::Code::new("rust", 5).with_spans(spans))
            .with_age(5);
            let ahead = arrival::HOME + Vec3::new(0.0, -0.1, -1.6);
            let shown = vec![
                tomori_protocol::Panel::new(
                    "panel-1",
                    "Steps to release",
                    "1. Bump the version.\n2. Run `cargo bake test:coverage`.\n3. Tag and push.",
                    tomori_protocol::Place::Free {
                        pose: tomori_protocol::Pose::upright_facing(
                            ahead.to_array(),
                            arrival::HOME.to_array(),
                        ),
                    },
                )
                .with_age(60),
                code.clone(),
            ];
            let mut conversation = joined();
            show_projection(&mut conversation, Vec::new(), shown);
            let wall = tomori_wall();
            let wall_frame = Mat4::from_rotation_translation(
                glam::Quat::from_array(wall.pose.orientation),
                Vec3::from_array(wall.pose.position),
            );
            let slots = tomori_scene::projection::slot_frames(&wall, wall_frame);
            let slot = |index: usize| slots[index].w_axis.truncate();
            let head = Mat4::from_translation(arrival::HOME);
            let senses = Senses {
                focused: true,
                head: Some(head),
                head_pose: Pose::from_transform(head),
                viewer: Some(arrival::HOME),
                space_head: head,
                trigger: Some(0.0),
                beam: true,
                grip: Some(0.0),
                ray: tomori_scene::pointing::Ray::new(arrival::HOME, ahead - arrival::HOME),
                ..Default::default()
            };
            let mut step = Step::default();
            let now = Instant::now();
            // About the headset's field of view and resolution, to judge the text as it would be read.
            let lens = glam::camera::rh::proj::directx::perspective(
                1.1,
                WIDTH as f32 / HEIGHT as f32,
                0.05,
                50.0,
            );
            let capture = |renderer: &mut Renderer, panels: &Panels, matrix: Mat4| {
                renderer.render(
                    &device,
                    &queue,
                    std::slice::from_ref(&view),
                    &[matrix],
                    Mat4::IDENTITY,
                    &panels.list(Mat4::IDENTITY),
                );
                read_back(&device, &queue, &texture, (WIDTH, HEIGHT))
            };
            // Pulled off and carried, the hint shows below it.
            step.update(&senses, &mut conversation, &mut felt(), now, 0.01);
            let carried = Senses {
                grip: Some(1.0),
                ..senses
            };
            step.update(&carried, &mut conversation, &mut felt(), now, 0.01);
            let frame = step.update(&carried, &mut conversation, &mut felt(), now, 0.01);
            let hint = frame.projection.last().unwrap().canvas.as_deref().unwrap();
            let text = |canvas: &tomori_scene::canvas::Canvas| {
                canvas
                    .rows
                    .iter()
                    .map(|row| row.plain())
                    .collect::<Vec<_>>()
                    .join("\n")
            };
            assert!(text(hint).contains("throw away, for all"));
            let hint_at = frame
                .projection
                .last()
                .unwrap()
                .model
                .unwrap()
                .w_axis
                .truncate();
            assert!(hint_at.y < ahead.y);
            draw(&mut renderer, &device, &queue, &mut panels, &frame);
            let looking = ahead - Vec3::Y * 0.2;
            let from_home =
                lens * glam::camera::rh::view::look_at_mat4(arrival::HOME, looking, Vec3::Y);
            let hinted = capture(&mut renderer, &panels, from_home);
            // Without its hint, the same frame differs: the hint draws, and its warm row with it.
            let mut bare = frame.projection.clone();
            bare.pop();
            let without = tomori_scene::scene::Frame {
                projection: bare,
                ..frame
            };
            draw(&mut renderer, &device, &queue, &mut panels, &without);
            let unhinted = capture(&mut renderer, &panels, from_home);
            assert_ne!(hinted, unhinted, "the hint must draw");
            let warm = |image: &image::RgbaImage| {
                image
                    .pixels()
                    .filter(|pixel| {
                        let [r, g, b, _] = pixel.0.map(i32::from);
                        r > 150 && g + 25 < r && b + 25 < g
                    })
                    .count()
            };
            assert!(
                warm(&hinted) > 20 + warm(&unhinted),
                "the throw row is warm"
            );
            // Pushed with the right stick up to the wall's slot holding the code panel, the hint says the two would
            // change places.
            let over = Senses {
                ray: tomori_scene::pointing::Ray::new(arrival::HOME, slot(2) - arrival::HOME),
                ..carried
            };
            let stick = |right_stick: glam::Vec2| tomori_scene::scene::Buttons {
                right_stick,
                ..Default::default()
            };
            let push = (slot(2).distance(arrival::HOME) / ahead.distance(arrival::HOME)).ln()
                / tomori_scene::manipulation::RATE;
            step.buttons(
                &stick(glam::Vec2::Y),
                true,
                &mut conversation,
                &mut felt(),
                now,
            );
            step.update(&over, &mut conversation, &mut felt(), now, push);
            step.buttons(
                &stick(glam::Vec2::ZERO),
                true,
                &mut conversation,
                &mut felt(),
                now,
            );
            let frame = step.update(&over, &mut conversation, &mut felt(), now, 1.0);
            let hint = frame.projection.last().unwrap().canvas.as_deref().unwrap();
            let said = text(hint);
            assert!(said.ends_with("swap with that one"), "{said}");
            draw(&mut renderer, &device, &queue, &mut panels, &frame);
            let toward = lens
                * glam::camera::rh::view::look_at_mat4(
                    arrival::HOME,
                    slot(2) + Vec3::Y * 0.25,
                    Vec3::Y,
                );
            let swapping = capture(&mut renderer, &panels, toward);
            // The code panel on its own, close up, from 0.5 m, and from home.
            let mut code_only = joined();
            show_projection(&mut code_only, Vec::new(), vec![code]);
            let away = Senses {
                ray: tomori_scene::pointing::Ray::new(arrival::HOME, -Vec3::Y),
                ..senses
            };
            let mut code_step = Step::default();
            let frame = code_step.update(&away, &mut code_only, &mut felt(), now, 0.01);
            draw(&mut renderer, &device, &queue, &mut panels, &frame);
            let facing = slots[2].transform_vector3(Vec3::Z).normalize();
            let close = lens
                * glam::camera::rh::view::look_at_mat4(slot(2) + facing * 0.5, slot(2), Vec3::Y);
            let wrapped = capture(&mut renderer, &panels, close);
            let from_home = lens
                * glam::camera::rh::view::look_at_mat4(
                    arrival::HOME,
                    slot(2) + Vec3::Y * 0.1,
                    Vec3::Y,
                );
            let wrapped_from_home = capture(&mut renderer, &panels, from_home);
            // The string's green carries onto the row its line wraps onto.
            let green = |image: &image::RgbaImage| {
                image
                    .pixels()
                    .filter(|pixel| {
                        let [r, g, b, _] = pixel.0.map(i32::from);
                        g > 120 && r + 15 < g && b + 20 < g
                    })
                    .count()
            };
            let mut sheet = image::RgbaImage::new(WIDTH, HEIGHT * 4);
            image::imageops::replace(&mut sheet, &hinted, 0, 0);
            image::imageops::replace(&mut sheet, &swapping, 0, HEIGHT.into());
            image::imageops::replace(&mut sheet, &wrapped, 0, (2 * HEIGHT).into());
            image::imageops::replace(&mut sheet, &wrapped_from_home, 0, (3 * HEIGHT).into());
            crate::test_support::preview("TOMORI_CARRY_HINT_PREVIEW", &sheet);
            assert!(green(&wrapped) > 20, "the string is coloured");
            assert!(pollster::block_on(scope.pop()).is_none());
        });
    }

    /// Set `TOMORI_CHECKLIST_PREVIEW` to a PNG path to save what this renders, at about the headset's resolution: an
    /// agenda the agent projected as a checklist, some items ticked and some not, one nested, on the Tomori wall from
    /// 0.6 m and from home, then the same list with nothing ticked.
    #[test]
    fn a_checklist_draws_empty_and_ticked_boxes_its_ticked_items_dimmed() {
        const WIDTH: u32 = 960;
        const HEIGHT: u32 = 720;
        test_gpu::with(|test_gpu::Gpu { device, queue, .. }| {
            let scope = device.push_error_scope(wgpu::ErrorFilter::Validation);
            let format = wgpu::TextureFormat::Rgba8UnormSrgb;
            let texture = device.create_texture(&wgpu::TextureDescriptor {
                label: Some("checklist test eye"),
                size: wgpu::Extent3d {
                    width: WIDTH,
                    height: HEIGHT,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
                view_formats: &[],
            });
            let view = texture.create_view(&Default::default());
            let mut renderer = Renderer::new(&device, &queue, format, WIDTH, HEIGHT);
            renderer.set_environment(tomori_scene::environment::Environment::MINIMAL);
            let mut panels = Panels::new(&device, &renderer);
            let ticked = "To cover today:\n\n- [x] Panel ids\n- [x] Updating in place\n- [ ] Decide the pairing window length\n  - [x] Ask about the headset\n  - [ ] Agree a default\n- [ ] Checklists on the wrist";
            let unticked = ticked.replace("[x]", "[ ]");
            let panel = |body: &str| {
                tomori_protocol::Panel::new(
                    "panel-1",
                    "Agenda",
                    body,
                    tomori_protocol::Place::Wall {
                        wall: tomori_protocol::TOMORI_WALL.into(),
                        slot: 2,
                    },
                )
                .with_slug(Some("agenda".into()))
                .with_age(5)
            };
            let wall = tomori_wall();
            let wall_frame = Mat4::from_rotation_translation(
                glam::Quat::from_array(wall.pose.orientation),
                Vec3::from_array(wall.pose.position),
            );
            let slots = tomori_scene::projection::slot_frames(&wall, wall_frame);
            let slot = slots[2].w_axis.truncate();
            let facing = slots[2].transform_vector3(Vec3::Z).normalize();
            let head = Mat4::from_translation(arrival::HOME);
            let senses = Senses {
                focused: true,
                head: Some(head),
                head_pose: Pose::from_transform(head),
                viewer: Some(arrival::HOME),
                space_head: head,
                ray: tomori_scene::pointing::Ray::new(arrival::HOME, -Vec3::Y),
                ..Default::default()
            };
            let lens = glam::camera::rh::proj::directx::perspective(
                1.1,
                WIDTH as f32 / HEIGHT as f32,
                0.05,
                50.0,
            );
            let close =
                lens * glam::camera::rh::view::look_at_mat4(slot + facing * 0.6, slot, Vec3::Y);
            let from_home = lens
                * glam::camera::rh::view::look_at_mat4(
                    arrival::HOME,
                    slot + Vec3::Y * 0.1,
                    Vec3::Y,
                );
            let mut capture = |body: &str, matrix: Mat4| {
                let mut conversation = joined();
                show_projection(&mut conversation, Vec::new(), vec![panel(body)]);
                let mut step = Step::default();
                let frame = step.update(
                    &senses,
                    &mut conversation,
                    &mut felt(),
                    Instant::now(),
                    0.01,
                );
                draw(&mut renderer, &device, &queue, &mut panels, &frame);
                renderer.render(
                    &device,
                    &queue,
                    std::slice::from_ref(&view),
                    &[matrix],
                    Mat4::IDENTITY,
                    &panels.list(Mat4::IDENTITY),
                );
                read_back(&device, &queue, &texture, (WIDTH, HEIGHT))
            };
            let close_ticked = capture(ticked, close);
            let home_ticked = capture(ticked, from_home);
            let close_unticked = capture(&unticked, close);
            let mut sheet = image::RgbaImage::new(WIDTH, HEIGHT * 3);
            image::imageops::replace(&mut sheet, &close_ticked, 0, 0);
            image::imageops::replace(&mut sheet, &home_ticked, 0, HEIGHT.into());
            image::imageops::replace(&mut sheet, &close_unticked, 0, (2 * HEIGHT).into());
            crate::test_support::preview("TOMORI_CHECKLIST_PREVIEW", &sheet);
            // A ticked box is green, so the ticked list has green ink and the unticked one none.
            let green = |image: &image::RgbaImage| {
                image
                    .pixels()
                    .filter(|pixel| {
                        let [r, g, b, _] = pixel.0.map(i32::from);
                        g > 120 && g > r + 60 && g > b + 30
                    })
                    .count()
            };
            let bright = |image: &image::RgbaImage| {
                image
                    .pixels()
                    .filter(|pixel| pixel.0[..3].iter().all(|channel| *channel > 200))
                    .count()
            };
            assert!(green(&close_ticked) > 100);
            assert!(green(&home_ticked) > 5);
            assert_eq!(green(&close_unticked), 0);
            // Ticked items' text is dimmed, so the list with four ticked has less bright ink than with none.
            let (ticked_ink, unticked_ink) = (bright(&close_ticked), bright(&close_unticked));
            assert!(ticked_ink * 10 < unticked_ink * 9);
            assert!(pollster::block_on(scope.pop()).is_none());
        });
    }

    /// Set `TOMORI_CODE_WRAP_PREVIEW` to a PNG path to save what this renders, at about the headset's resolution: the
    /// block of Rust the maintainer looked at on the Quest 3, on the Tomori wall from 0.5 m, as an older server sent it,
    /// each line cut to 64 characters with "…", then as it is sent now, every line whole, a long one wrapping onto as many
    /// rows as it needs, then the same from home.
    #[test]
    fn a_code_panel_shows_every_line_whole_however_many_rows_it_wraps_onto() {
        const WIDTH: u32 = 960;
        const HEIGHT: u32 = 720;
        test_gpu::with(|test_gpu::Gpu { device, queue, .. }| {
            let scope = device.push_error_scope(wgpu::ErrorFilter::Validation);
            let format = wgpu::TextureFormat::Rgba8UnormSrgb;
            let texture = device.create_texture(&wgpu::TextureDescriptor {
                label: Some("code wrap test eye"),
                size: wgpu::Extent3d {
                    width: WIDTH,
                    height: HEIGHT,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
                view_formats: &[],
            });
            let view = texture.create_view(&Default::default());
            let mut renderer = Renderer::new(&device, &queue, format, WIDTH, HEIGHT);
            renderer.set_environment(tomori_scene::environment::Environment::MINIMAL);
            let mut panels = Panels::new(&device, &renderer);
            let rust = [
                "use std::collections::HashMap;",
                "",
                "fn summarise(orders: &[(String, u32, f64)]) -> HashMap<String, (u32, f64)> {",
                "    let mut totals = HashMap::new();",
                "    for (customer, quantity, price) in orders {",
                "        let entry = totals.entry(customer.clone()).or_insert((0, 0.0));",
                "        entry.0 += quantity;",
                "        entry.1 += *quantity as f64 * price; // running total of what each customer has spent across every order so far",
                "    }",
                "    totals",
                "}",
            ];
            // The spans of `body`, of these texts in order, as a highlighter would find them, those past where a line
            // is cut left out.
            let spans = |body: &str| {
                let mut spans = Vec::new();
                let mut from = 0;
                for (text, kind) in [
                    ("use", tomori_protocol::TokenKind::Keyword),
                    ("HashMap", tomori_protocol::TokenKind::Type),
                    ("fn", tomori_protocol::TokenKind::Keyword),
                    ("summarise", tomori_protocol::TokenKind::Function),
                    ("String", tomori_protocol::TokenKind::Type),
                    ("HashMap", tomori_protocol::TokenKind::Type),
                    ("let", tomori_protocol::TokenKind::Keyword),
                    ("mut", tomori_protocol::TokenKind::Keyword),
                    ("new", tomori_protocol::TokenKind::Function),
                    ("for", tomori_protocol::TokenKind::Keyword),
                    ("in", tomori_protocol::TokenKind::Keyword),
                    ("let", tomori_protocol::TokenKind::Keyword),
                    ("entry", tomori_protocol::TokenKind::Function),
                    ("clone", tomori_protocol::TokenKind::Function),
                    ("or_insert", tomori_protocol::TokenKind::Function),
                    ("0", tomori_protocol::TokenKind::Number),
                    ("0.0", tomori_protocol::TokenKind::Number),
                    ("as", tomori_protocol::TokenKind::Keyword),
                    (
                        "// running total of what each customer has spent across every order so far",
                        tomori_protocol::TokenKind::Comment,
                    ),
                ] {
                    let Some(at) = body[from..].find(text).map(|at| from + at) else {
                        continue;
                    };
                    spans.push(tomori_protocol::Span::new(
                        body[..at].chars().count() as u32,
                        text.chars().count() as u32,
                        kind,
                    ));
                    from = at + text.len();
                }
                assert!(tomori_protocol::valid_spans(body, &spans));
                spans
            };
            let panel = |body: String| {
                let spans = spans(&body);
                tomori_protocol::Panel::new(
                    "panel-1",
                    "rust",
                    body,
                    tomori_protocol::Place::Wall {
                        wall: tomori_protocol::TOMORI_WALL.into(),
                        slot: 2,
                    },
                )
                .with_code(tomori_protocol::Code::new("rust", rust.len() as u32).with_spans(spans))
                .with_age(5)
            };
            let cut = rust
                .iter()
                .map(|line| match line.char_indices().nth(64) {
                    Some((at, _)) => format!("{}…", &line[..at]),
                    None => (*line).to_owned(),
                })
                .collect::<Vec<_>>()
                .join("\n");
            let whole = rust.join("\n");
            let wall = tomori_wall();
            let wall_frame = Mat4::from_rotation_translation(
                glam::Quat::from_array(wall.pose.orientation),
                Vec3::from_array(wall.pose.position),
            );
            let slots = tomori_scene::projection::slot_frames(&wall, wall_frame);
            let slot = slots[2].w_axis.truncate();
            let facing = slots[2].transform_vector3(Vec3::Z).normalize();
            let head = Mat4::from_translation(arrival::HOME);
            let senses = Senses {
                focused: true,
                head: Some(head),
                head_pose: Pose::from_transform(head),
                viewer: Some(arrival::HOME),
                space_head: head,
                ray: tomori_scene::pointing::Ray::new(arrival::HOME, -Vec3::Y),
                ..Default::default()
            };
            let lens = glam::camera::rh::proj::directx::perspective(
                1.1,
                WIDTH as f32 / HEIGHT as f32,
                0.05,
                50.0,
            );
            let close =
                lens * glam::camera::rh::view::look_at_mat4(slot + facing * 0.5, slot, Vec3::Y);
            let from_home = lens
                * glam::camera::rh::view::look_at_mat4(
                    arrival::HOME,
                    slot + Vec3::Y * 0.1,
                    Vec3::Y,
                );
            let mut capture = |body: String, matrix: Mat4| {
                let mut conversation = joined();
                show_projection(&mut conversation, Vec::new(), vec![panel(body)]);
                let mut step = Step::default();
                let frame = step.update(
                    &senses,
                    &mut conversation,
                    &mut felt(),
                    Instant::now(),
                    0.01,
                );
                draw(&mut renderer, &device, &queue, &mut panels, &frame);
                renderer.render(
                    &device,
                    &queue,
                    std::slice::from_ref(&view),
                    &[matrix],
                    Mat4::IDENTITY,
                    &panels.list(Mat4::IDENTITY),
                );
                read_back(&device, &queue, &texture, (WIDTH, HEIGHT))
            };
            let before = capture(cut, close);
            let after = capture(whole.clone(), close);
            let after_from_home = capture(whole, from_home);
            // What was cut is drawn, the rest of the sixth line and of the comment, so there is more text on the panel.
            let ink = |image: &image::RgbaImage| {
                image
                    .pixels()
                    .filter(|pixel| pixel.0[..3].iter().any(|channel| *channel > 120))
                    .count()
            };
            let mut sheet = image::RgbaImage::new(WIDTH, HEIGHT * 3);
            image::imageops::replace(&mut sheet, &before, 0, 0);
            image::imageops::replace(&mut sheet, &after, 0, HEIGHT.into());
            image::imageops::replace(&mut sheet, &after_from_home, 0, (2 * HEIGHT).into());
            crate::test_support::preview("TOMORI_CODE_WRAP_PREVIEW", &sheet);
            assert_ne!(before, after, "the whole lines must draw");
            let (after_ink, before_ink) = (ink(&after), ink(&before));
            assert!(
                after_ink > before_ink + 200,
                "what was cut is drawn: {after_ink} against {before_ink}"
            );
            assert!(pollster::block_on(scope.pop()).is_none());
        });
    }

    /// Set `TOMORI_CODE_SCROLL_PREVIEW` to a PNG path to save what this renders, at about the headset's resolution: the
    /// 17 lines of JavaScript the maintainer could not see all of on the Quest 3, on the Tomori wall from 0.6 m, at its
    /// top, saying how many more lines are below, then scrolled to its end with the wheel, saying how many are above;
    /// and then floating 1.2 m ahead, grown to show every line.
    #[test]
    fn a_code_panel_scrolls_on_a_wall_and_grows_to_show_every_line_floating() {
        const WIDTH: u32 = 960;
        const HEIGHT: u32 = 960;
        test_gpu::with(|test_gpu::Gpu { device, queue, .. }| {
            let scope = device.push_error_scope(wgpu::ErrorFilter::Validation);
            let format = wgpu::TextureFormat::Rgba8UnormSrgb;
            let texture = device.create_texture(&wgpu::TextureDescriptor {
                label: Some("code scroll test eye"),
                size: wgpu::Extent3d {
                    width: WIDTH,
                    height: HEIGHT,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
                view_formats: &[],
            });
            let view = texture.create_view(&Default::default());
            let mut renderer = Renderer::new(&device, &queue, format, WIDTH, HEIGHT);
            renderer.set_environment(tomori_scene::environment::Environment::MINIMAL);
            let mut panels = Panels::new(&device, &renderer);
            let javascript = [
                "import { build, publish } from './release.js';",
                "",
                "export async function release(project, options = {}) {",
                "  const version = await project.nextVersion(options.channel ?? 'stable');",
                "  log(`Releasing ${project.name} ${version} to the ${options.channel} channel`);",
                "  const artifacts = await build(project, { target: 'quest', release: true, version });",
                "  for (const artifact of artifacts) {",
                "    await artifact.sign(options.key);",
                "  }",
                "  if (options.dryRun) {",
                "    return { version, artifacts, published: false };",
                "  }",
                "  await publish(artifacts, { channel: options.channel, notes: project.releaseNotes(version) });",
                "  log('Published.');",
                "  return { version, artifacts, published: true };",
                "}",
                "export default release;",
            ];
            let spans = |body: &str| {
                let mut spans = Vec::new();
                let mut from = 0;
                for (text, kind) in [
                    ("import", tomori_protocol::TokenKind::Keyword),
                    ("'./release.js'", tomori_protocol::TokenKind::String),
                    ("export", tomori_protocol::TokenKind::Keyword),
                    ("async", tomori_protocol::TokenKind::Keyword),
                    ("function", tomori_protocol::TokenKind::Keyword),
                    ("release", tomori_protocol::TokenKind::Function),
                    ("const", tomori_protocol::TokenKind::Keyword),
                    ("await", tomori_protocol::TokenKind::Keyword),
                    ("'stable'", tomori_protocol::TokenKind::String),
                    ("log", tomori_protocol::TokenKind::Function),
                    ("const", tomori_protocol::TokenKind::Keyword),
                    ("await", tomori_protocol::TokenKind::Keyword),
                    ("'quest'", tomori_protocol::TokenKind::String),
                    ("true", tomori_protocol::TokenKind::Constant),
                    ("for", tomori_protocol::TokenKind::Keyword),
                    ("await", tomori_protocol::TokenKind::Keyword),
                    ("if", tomori_protocol::TokenKind::Keyword),
                    ("return", tomori_protocol::TokenKind::Keyword),
                    ("false", tomori_protocol::TokenKind::Constant),
                    ("await", tomori_protocol::TokenKind::Keyword),
                    ("publish", tomori_protocol::TokenKind::Function),
                    ("'Published.'", tomori_protocol::TokenKind::String),
                    ("return", tomori_protocol::TokenKind::Keyword),
                    ("true", tomori_protocol::TokenKind::Constant),
                    ("export", tomori_protocol::TokenKind::Keyword),
                    ("default", tomori_protocol::TokenKind::Keyword),
                ] {
                    let at = from
                        + body[from..]
                            .find(text)
                            .expect("each text is there, in order");
                    spans.push(tomori_protocol::Span::new(
                        body[..at].chars().count() as u32,
                        text.chars().count() as u32,
                        kind,
                    ));
                    from = at + text.len();
                }
                assert!(tomori_protocol::valid_spans(body, &spans));
                spans
            };
            let body = javascript.join("\n");
            let panel = |place: tomori_protocol::Place| {
                tomori_protocol::Panel::new("panel-1", "javascript", body.clone(), place)
                    .with_code(
                        tomori_protocol::Code::new("javascript", javascript.len() as u32)
                            .with_spans(spans(&body)),
                    )
                    .with_age(5)
            };
            let wall = tomori_wall();
            let wall_frame = Mat4::from_rotation_translation(
                glam::Quat::from_array(wall.pose.orientation),
                Vec3::from_array(wall.pose.position),
            );
            let slots = tomori_scene::projection::slot_frames(&wall, wall_frame);
            let slot = slots[2].w_axis.truncate();
            let facing = slots[2].transform_vector3(Vec3::Z).normalize();
            let head = Mat4::from_translation(arrival::HOME);
            let senses = |at: Vec3| Senses {
                focused: true,
                head: Some(head),
                head_pose: Pose::from_transform(head),
                viewer: Some(arrival::HOME),
                space_head: head,
                ray: tomori_scene::pointing::Ray::new(arrival::HOME, at - arrival::HOME),
                ..Default::default()
            };
            let lens = glam::camera::rh::proj::directx::perspective(
                1.1,
                WIDTH as f32 / HEIGHT as f32,
                0.05,
                50.0,
            );
            let close =
                lens * glam::camera::rh::view::look_at_mat4(slot + facing * 0.6, slot, Vec3::Y);
            let ahead = arrival::HOME + Vec3::new(0.0, 0.0, -1.2);
            let from_home =
                lens * glam::camera::rh::view::look_at_mat4(arrival::HOME, ahead, Vec3::Y);
            // What the participant sees of the panel at `place`, pointed at, after the wheel scrolls it `rows` rows.
            let mut capture = |place: tomori_protocol::Place, at: Vec3, rows: f32, matrix: Mat4| {
                let mut conversation = joined();
                show_projection(&mut conversation, Vec::new(), vec![panel(place)]);
                let mut step = Step::default();
                let now = Instant::now();
                step.update(&senses(at), &mut conversation, &mut felt(), now, 0.01);
                step.buttons(
                    &tomori_scene::scene::Buttons {
                        scroll: rows,
                        ..Default::default()
                    },
                    true,
                    &mut conversation,
                    &mut felt(),
                    now,
                );
                // Drawn pointing past it, so that the label on the ray does not cover it.
                let frame = step.update(
                    &senses(arrival::HOME + Vec3::Y),
                    &mut conversation,
                    &mut felt(),
                    now,
                    0.01,
                );
                draw(&mut renderer, &device, &queue, &mut panels, &frame);
                renderer.render(
                    &device,
                    &queue,
                    std::slice::from_ref(&view),
                    &[matrix],
                    Mat4::IDENTITY,
                    &panels.list(Mat4::IDENTITY),
                );
                read_back(&device, &queue, &texture, (WIDTH, HEIGHT))
            };
            let on_wall = tomori_protocol::Place::Wall {
                wall: tomori_protocol::TOMORI_WALL.into(),
                slot: 2,
            };
            let top = capture(on_wall.clone(), slot, 0.0, close);
            let end = capture(on_wall, slot, 100.0, close);
            let floating = tomori_protocol::Place::Free {
                pose: tomori_protocol::Pose::upright_facing(
                    ahead.to_array(),
                    arrival::HOME.to_array(),
                ),
            };
            let grown = capture(floating, ahead, 0.0, from_home);
            let mut sheet = image::RgbaImage::new(WIDTH * 3, HEIGHT);
            image::imageops::replace(&mut sheet, &top, 0, 0);
            image::imageops::replace(&mut sheet, &end, WIDTH.into(), 0);
            image::imageops::replace(&mut sheet, &grown, (2 * WIDTH).into(), 0);
            crate::test_support::preview("TOMORI_CODE_SCROLL_PREVIEW", &sheet);
            assert_ne!(top, end, "scrolling shows other rows");
            // Floating, it is taller than in its slot: lit rows reach further down the view.
            let lowest = |image: &image::RgbaImage| {
                (0..image.height())
                    .rev()
                    .find(|&y| {
                        (0..image.width())
                            .any(|x| image.get_pixel(x, y).0[..3].iter().any(|c| *c > 120))
                    })
                    .unwrap_or_default()
            };
            let highest = |image: &image::RgbaImage| {
                (0..image.height())
                    .find(|&y| {
                        (0..image.width())
                            .any(|x| image.get_pixel(x, y).0[..3].iter().any(|c| *c > 120))
                    })
                    .unwrap_or_default()
            };
            assert!(lowest(&grown) > highest(&grown) + HEIGHT / 3, "it grows");
            assert!(pollster::block_on(scope.pop()).is_none());
        });
    }

    /// Set `TOMORI_TABLE_PREVIEW` to a PNG path to save what this renders, at about the headset's resolution: the table
    /// the maintainer could not read on the Quest 3, every row cut with "…", on the Tomori wall from 0.6 m, at its top,
    /// then scrolled into its rows with its header kept at the top; and then floating 1.2 m ahead, grown wider to show it
    /// whole.
    #[test]
    fn a_table_is_a_grid_in_a_slot_and_floating_grows_to_show_it_whole() {
        const WIDTH: u32 = 960;
        const HEIGHT: u32 = 960;
        test_gpu::with(|test_gpu::Gpu { device, queue, .. }| {
            let scope = device.push_error_scope(wgpu::ErrorFilter::Validation);
            let format = wgpu::TextureFormat::Rgba8UnormSrgb;
            let texture = device.create_texture(&wgpu::TextureDescriptor {
                label: Some("table test eye"),
                size: wgpu::Extent3d {
                    width: WIDTH,
                    height: HEIGHT,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
                view_formats: &[],
            });
            let view = texture.create_view(&Default::default());
            let mut renderer = Renderer::new(&device, &queue, format, WIDTH, HEIGHT);
            renderer.set_environment(tomori_scene::environment::Environment::MINIMAL);
            let mut panels = Panels::new(&device, &renderer);
            let body = [
                "| PR | What | Recommendation |",
                "|---|---|---|",
                "| 233 | Environments get trigger, B, sticks | **Your call:** clashes with scrolling, wall B, checklist ticking |",
                "| 229 | Prometheus data source crate | **Your call:** own and publish it now? |",
                "| 231 | Environment anchors for swarm, workers | Rebase, then try on headset |",
                "| 234 | Backend names its avatar (on 231) | After 231, check the Space menu fits |",
                "| 232 | Help text, licences, program names | Rebase after 236, then merge |",
                "| 90 | Old environment framework proposal | Superseded: suggest closing |",
            ]
            .join("\n");
            let panel = |place: tomori_protocol::Place| {
                tomori_protocol::Panel::new("panel-1", "Open PRs", body.clone(), place).with_age(5)
            };
            let wall = tomori_wall();
            let wall_frame = Mat4::from_rotation_translation(
                glam::Quat::from_array(wall.pose.orientation),
                Vec3::from_array(wall.pose.position),
            );
            let slots = tomori_scene::projection::slot_frames(&wall, wall_frame);
            let slot = slots[2].w_axis.truncate();
            let facing = slots[2].transform_vector3(Vec3::Z).normalize();
            let head = Mat4::from_translation(arrival::HOME);
            let senses = |at: Vec3| Senses {
                focused: true,
                head: Some(head),
                head_pose: Pose::from_transform(head),
                viewer: Some(arrival::HOME),
                space_head: head,
                ray: tomori_scene::pointing::Ray::new(arrival::HOME, at - arrival::HOME),
                ..Default::default()
            };
            let lens = glam::camera::rh::proj::directx::perspective(
                1.1,
                WIDTH as f32 / HEIGHT as f32,
                0.05,
                50.0,
            );
            let close =
                lens * glam::camera::rh::view::look_at_mat4(slot + facing * 0.6, slot, Vec3::Y);
            let ahead = arrival::HOME + Vec3::new(0.0, 0.0, -1.2);
            let from_home =
                lens * glam::camera::rh::view::look_at_mat4(arrival::HOME, ahead, Vec3::Y);
            // What the participant sees of the panel at `place`, pointed at, after the wheel scrolls it `rows` rows.
            let mut capture = |place: tomori_protocol::Place, at: Vec3, rows: f32, matrix: Mat4| {
                let mut conversation = joined();
                show_projection(&mut conversation, Vec::new(), vec![panel(place)]);
                let mut step = Step::default();
                let now = Instant::now();
                step.update(&senses(at), &mut conversation, &mut felt(), now, 0.01);
                step.buttons(
                    &tomori_scene::scene::Buttons {
                        scroll: rows,
                        ..Default::default()
                    },
                    true,
                    &mut conversation,
                    &mut felt(),
                    now,
                );
                // Drawn pointing past it, so that the label on the ray does not cover it.
                let frame = step.update(
                    &senses(arrival::HOME + Vec3::Y),
                    &mut conversation,
                    &mut felt(),
                    now,
                    0.01,
                );
                draw(&mut renderer, &device, &queue, &mut panels, &frame);
                renderer.render(
                    &device,
                    &queue,
                    std::slice::from_ref(&view),
                    &[matrix],
                    Mat4::IDENTITY,
                    &panels.list(Mat4::IDENTITY),
                );
                read_back(&device, &queue, &texture, (WIDTH, HEIGHT))
            };
            let on_wall = tomori_protocol::Place::Wall {
                wall: tomori_protocol::TOMORI_WALL.into(),
                slot: 2,
            };
            let top = capture(on_wall.clone(), slot, 0.0, close);
            let scrolled = capture(on_wall, slot, 8.0, close);
            let floating = tomori_protocol::Place::Free {
                pose: tomori_protocol::Pose::upright_facing(
                    ahead.to_array(),
                    arrival::HOME.to_array(),
                ),
            };
            let grown = capture(floating, ahead, 0.0, from_home);
            let mut sheet = image::RgbaImage::new(WIDTH * 3, HEIGHT);
            image::imageops::replace(&mut sheet, &top, 0, 0);
            image::imageops::replace(&mut sheet, &scrolled, WIDTH.into(), 0);
            image::imageops::replace(&mut sheet, &grown, (2 * WIDTH).into(), 0);
            crate::test_support::preview("TOMORI_TABLE_PREVIEW", &sheet);
            assert_ne!(top, scrolled, "scrolling shows other rows");
            // The header's bar, a blue tint across the grid, is drawn at the top of the slot both times.
            let tinted = |image: &image::RgbaImage| {
                image
                    .pixels()
                    .filter(|pixel| {
                        let [r, g, b, _] = pixel.0.map(i32::from);
                        b > r + 25 && b > g + 10 && b > 50
                    })
                    .count()
            };
            assert!(tinted(&top) > 1000);
            assert!(tinted(&scrolled) > 1000);
            // Floating, it is wider than a text panel goes: lit text spans most of the view.
            let lit = |image: &image::RgbaImage| {
                let columns: Vec<u32> = (0..image.width())
                    .filter(|&x| {
                        (0..image.height())
                            .any(|y| image.get_pixel(x, y).0[..3].iter().all(|c| *c > 150))
                    })
                    .collect();
                columns.last().copied().unwrap_or_default()
                    - columns.first().copied().unwrap_or_default()
            };
            let wide = lit(&grown);
            assert!(wide > WIDTH * 3 / 5, "{wide}");
            assert!(pollster::block_on(scope.pop()).is_none());
        });
    }

    /// Set `TOMORI_DEPTH_ORDER_PREVIEW` to a PNG path to save what this renders, each eye side by side: the hand menu with
    /// a panel floating behind it, then in front of it, and the status pill with a panel behind it, then in front of it.
    #[test]
    fn what_is_nearer_covers_a_panel_behind_it_and_a_panel_covers_what_is_behind_it() {
        const WIDTH: u32 = 512;
        const HEIGHT: u32 = 512;
        test_gpu::with(|test_gpu::Gpu { device, queue, .. }| {
            let scope = device.push_error_scope(wgpu::ErrorFilter::Validation);
            let format = wgpu::TextureFormat::Rgba8UnormSrgb;
            let textures: [wgpu::Texture; 2] = std::array::from_fn(|_| {
                device.create_texture(&wgpu::TextureDescriptor {
                    label: Some("depth order test eye"),
                    size: wgpu::Extent3d {
                        width: WIDTH,
                        height: HEIGHT,
                        depth_or_array_layers: 1,
                    },
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    format,
                    usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
                    view_formats: &[],
                })
            });
            let views = textures
                .each_ref()
                .map(|texture| texture.create_view(&Default::default()));
            let mut renderer = Renderer::new(&device, &queue, format, WIDTH, HEIGHT);
            renderer.set_environment(tomori_scene::environment::Environment::MINIMAL);
            let mut panels = Panels::new(&device, &renderer);
            let head = Mat4::from_translation(arrival::HOME);
            let left_palm = Palm {
                wrist: arrival::HOME + Vec3::new(-0.06, -0.2, -0.45),
                normal: Vec3::Y,
            };
            let senses = Senses {
                focused: true,
                head: Some(head),
                head_pose: Pose::from_transform(head),
                viewer: Some(arrival::HOME),
                space_head: head,
                trigger: Some(0.0),
                ray: tomori_scene::pointing::Ray::new(arrival::HOME, -Vec3::Y),
                ..Default::default()
            };
            // A text panel floating at `at`, facing home.
            let floating = |at: Vec3| {
                tomori_protocol::Panel::new(
                    "panel-1",
                    "Steps to release",
                    "1. Bump the version.\n2. Run `cargo bake test:coverage`.\n3. Tag and push.\n4. Upload the APK.",
                    tomori_protocol::Place::Free {
                        pose: tomori_protocol::Pose::upright_facing(
                            at.to_array(),
                            arrival::HOME.to_array(),
                        ),
                    },
                )
            };
            // Hand `show` the frame with the panel at `at`, if any, and with the hand menu, as `menu` says, from a frame step
            // held past the menu's dwell.
            let frame_with = |at: Option<Vec3>, menu: bool, show: &mut dyn FnMut(&Frame<'_>)| {
                let mut conversation = joined();
                show_projection(
                    &mut conversation,
                    Vec::new(),
                    at.map(floating).into_iter().collect(),
                );
                let senses = Senses {
                    left_palm: menu.then_some(left_palm),
                    ..senses
                };
                let mut step = Step::default();
                let start = Instant::now();
                step.update(&senses, &mut conversation, &mut felt(), start, 0.01);
                let frame =
                    step.update(&senses, &mut conversation, &mut felt(), start + HOLD, 0.01);
                assert_eq!(
                    frame.arrival.is_some(),
                    menu,
                    "the menu shows only when asked for"
                );
                show(&frame);
            };
            // Where the menu and the pill stand: the middle of the menu's rows, and the pill's middle.
            let mut menu_at = Vec3::ZERO;
            frame_with(None, true, &mut |frame| {
                let objects = &frame.arrival.as_ref().unwrap().scene.objects;
                assert!(!objects.is_empty());
                menu_at = objects.iter().map(|object| object.position).sum::<Vec3>()
                    / objects.len() as f32;
            });
            let mut pill_at = Vec3::ZERO;
            frame_with(None, false, &mut |frame| {
                pill_at = frame
                    .panels
                    .pill
                    .model
                    .expect("the pill shows")
                    .w_axis
                    .truncate();
            });
            // Both eyes, 64 mm apart, looking at `target`.
            let eyes = |target: Vec3| {
                let lens = glam::camera::rh::proj::directx::perspective(1.2, 1.0, 0.05, 50.0);
                [-0.032, 0.032].map(|x| {
                    let eye = arrival::HOME + Vec3::X * x;
                    lens * glam::camera::rh::view::look_at_mat4(eye, target + Vec3::X * x, Vec3::Y)
                })
            };
            // Each eye's image of the frame with the panel at `at`, the menu, as `menu` says, and the pill where it shows, as
            // `pill` says, seen looking at `target`.
            let mut capture = |at: Option<Vec3>, menu: bool, pill: bool, target: Vec3| {
                frame_with(at, menu, &mut |frame| {
                    draw(&mut renderer, &device, &queue, &mut panels, frame);
                });
                renderer.render(
                    &device,
                    &queue,
                    &views,
                    &eyes(target),
                    Mat4::IDENTITY,
                    &panels
                        .list(Mat4::IDENTITY)
                        .into_iter()
                        .filter(|(panel, _)| pill || !std::ptr::eq(*panel, &panels.pill))
                        .collect::<Vec<_>>(),
                );
                textures
                    .each_ref()
                    .map(|texture| read_back(&device, &queue, texture, (WIDTH, HEIGHT)))
            };
            // Of the pixels where `near` and `far` each draw over the `bare` scene without the other, differently, how many
            // look like `near`, which should cover `far`, and how many like `far`, where both draw.
            let covering = |bare: &image::RgbaImage,
                            near: &image::RgbaImage,
                            far: &image::RgbaImage,
                            both: &image::RgbaImage| {
                let distance = |a: &image::Rgba<u8>, b: &image::Rgba<u8>| {
                    a.0.iter()
                        .zip(b.0)
                        .map(|(a, b)| (i32::from(*a) - i32::from(b)).abs())
                        .sum::<i32>()
                };
                let (mut nearer, mut farther) = (0, 0);
                for (((bare, near), far), both) in bare
                    .pixels()
                    .zip(near.pixels())
                    .zip(far.pixels())
                    .zip(both.pixels())
                {
                    // Both draw here, and differently.
                    if distance(bare, near) > 60
                        && distance(bare, far) > 60
                        && distance(near, far) > 60
                    {
                        if distance(both, near) < distance(both, far) {
                            nearer += 1;
                        } else {
                            farther += 1;
                        }
                    }
                }
                (nearer, farther)
            };
            let mut sheet = image::RgbaImage::new(WIDTH * 2, HEIGHT * 4);
            let mut row = 0;
            let mut check = |name: &str, near_first: bool, at: Vec3, menu: bool, target: Vec3| {
                // Neither, either alone, and both.
                let bare = capture(None, false, false, target);
                let thing = capture(None, menu, true, target);
                let panel = capture(Some(at), false, false, target);
                let both = capture(Some(at), menu, true, target);
                for eye in 0..2 {
                    let (near, far) = if near_first {
                        (&thing[eye], &panel[eye])
                    } else {
                        (&panel[eye], &thing[eye])
                    };
                    let (nearer, farther) = covering(&bare[eye], near, far, &both[eye]);
                    image::imageops::replace(
                        &mut sheet,
                        &both[eye],
                        i64::from(eye as u32 * WIDTH),
                        i64::from(row * HEIGHT),
                    );
                    let overlapping = nearer + farther;
                    assert!(overlapping > 100, "{name}, eye {eye}: they must overlap");
                    assert!(
                        farther * 20 < nearer,
                        "{name}, eye {eye}: the nearer must cover the farther, but {farther} of {overlapping} pixels show the farther"
                    );
                }
                row += 1;
            };
            let menu_ray = menu_at - arrival::HOME;
            let pill_ray = pill_at - arrival::HOME;
            check(
                "the menu in front of a panel",
                true,
                arrival::HOME + menu_ray * 3.0,
                true,
                menu_at,
            );
            check(
                "a panel in front of the menu",
                false,
                arrival::HOME + menu_ray * 0.6,
                true,
                menu_at,
            );
            check(
                "the pill in front of a panel",
                true,
                arrival::HOME + pill_ray * 1.1,
                false,
                pill_at,
            );
            check(
                "a panel in front of the pill",
                false,
                arrival::HOME + pill_ray * 0.6,
                false,
                pill_at,
            );
            crate::test_support::preview("TOMORI_DEPTH_ORDER_PREVIEW", &sheet);
            assert!(pollster::block_on(scope.pop()).is_none());
        });
    }

    /// A figure standing at its anchor, heard from above it, showing what its scenery draws from its second frame.
    struct Figure {
        anchor: tomori_scene::anchors::Anchor,
        frames: usize,
    }

    impl tomori_scene::avatar::Avatar for Figure {
        fn update(&mut self, input: &tomori_scene::avatar::Input) {
            self.anchor = input.anchor;
            self.frames += 1;
        }

        fn voices(&self, voices: &mut Vec<[f32; 3]>) {
            voices.push((self.anchor.position + Vec3::Y * 0.3).to_array());
        }

        fn departures(&self, departures: &mut Vec<[f32; 3]>) {
            self.voices(departures);
        }

        fn bounds(&self) -> crate::arrival_motion::Sphere {
            crate::arrival_motion::Sphere {
                centre: self.anchor.position,
                radius: 0.5,
            }
        }

        fn drawn(&self) -> Option<tomori_scene::client_side::Drawn> {
            (self.frames > 1)
                .then(|| std::sync::Arc::new(self.frames) as tomori_scene::client_side::Drawn)
        }
    }

    /// A scenery that draws nothing, and panics when given what its avatar shows.
    struct Breaking;

    impl crate::scenery::Scenery for Breaking {
        fn animate(
            &mut self,
            _device: &wgpu::Device,
            _queue: &wgpu::Queue,
            _drawn: &(dyn std::any::Any + Send + Sync),
        ) {
            panic!("the figure's scenery cannot show it");
        }

        fn prepare(&self, _queue: &wgpu::Queue, _eye: usize, _scene_to_clip: Mat4) {}

        fn draw(&self, _pass: &mut wgpu::RenderPass<'_>, _eye: usize) {}

        fn set_reveal(&mut self, _reveal: Option<(Vec3, f32)>) {}
    }

    #[test]
    fn an_avatar_whose_scenery_panics_falls_back_to_the_swarm_seen_and_heard() {
        use crate::scenery::{Sceneries, Scenery};
        // Its scenery panics as it is made, or as it is given what the figure shows.
        let made = |_: &crate::scenery::Target| -> Box<dyn Scenery> {
            panic!("the figure's scenery cannot be made")
        };
        let animated = |_: &crate::scenery::Target| -> Box<dyn Scenery> { Box::new(Breaking) };
        Breaking.set_reveal(None);
        let figure = |_: &str| -> Box<dyn tomori_scene::avatar::Avatar> {
            Box::new(Figure {
                anchor: tomori_scene::anchors::Anchor::AGENT,
                frames: 0,
            })
        };
        test_gpu::with(|test_gpu::Gpu { device, queue, .. }| {
            let format = wgpu::TextureFormat::Rgba8UnormSrgb;
            let view = device
                .create_texture(&wgpu::TextureDescriptor {
                    label: Some("figure eye"),
                    size: wgpu::Extent3d {
                        width: 16,
                        height: 16,
                        depth_or_array_layers: 1,
                    },
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    format,
                    usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
                    view_formats: &[],
                })
                .create_view(&Default::default());
            for (scenery, frames) in [
                (
                    Box::new(made)
                        as Box<dyn Fn(&crate::scenery::Target) -> Box<dyn Scenery> + Send + Sync>,
                    1,
                ),
                (Box::new(animated), 2),
            ] {
                let mut sceneries = Sceneries::empty();
                sceneries.set_avatar("figure", figure, scenery);
                let mut conversation = lobby(tomori_protocol::ConnectionRole::Owner);
                conversation.set_avatars(sceneries.avatars());
                conversation.set_avatar_failures(sceneries.avatar_failures());
                conversation.apply(space(|state| state.avatar = Some("figure".into())));
                let mut renderer =
                    Renderer::with_sceneries(&device, &queue, format, 16, 16, 1, &sceneries);
                let mut panels = Panels::new(&device, &renderer);
                let mut step = Step::default();
                let senses = Senses {
                    focused: true,
                    ..Default::default()
                };
                let now = Instant::now();
                // The figure shows for as many frames as its scenery lasts.
                for _ in 0..frames {
                    let frame = step.update(&senses, &mut conversation, &mut felt(), now, 0.01);
                    assert!(frame.avatar.is_some());
                    assert!(frame.swarm.agents.is_empty());
                    draw(&mut renderer, &device, &queue, &mut panels, &frame);
                    renderer.render(
                        &device,
                        &queue,
                        std::slice::from_ref(&view),
                        &[Mat4::IDENTITY],
                        Mat4::IDENTITY,
                        &[],
                    );
                }
                // Then, from the next frame on, the swarm shows in its place, and the agent is heard from it.
                for _ in 0..3 {
                    let frame = step.update(&senses, &mut conversation, &mut felt(), now, 0.01);
                    assert!(frame.avatar.is_none());
                    assert_eq!(frame.swarm.agents.len(), crate::swarm::AGENT_COUNT);
                    let agents: Vec<[f32; 3]> = frame
                        .swarm
                        .agents
                        .iter()
                        .map(|agent| agent.position.to_array())
                        .collect();
                    assert_eq!(frame.agent, tomori_scene::spatial_audio::centre(&agents));
                    draw(&mut renderer, &device, &queue, &mut panels, &frame);
                }
            }
        });
    }

    /// An offscreen eye `WIDTH` by `HEIGHT` for the wall tests below: its texture and its view.
    fn eye(
        device: &wgpu::Device,
        label: &str,
        size: (u32, u32),
    ) -> (wgpu::Texture, wgpu::TextureView) {
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some(label),
            size: wgpu::Extent3d {
                width: size.0,
                height: size.1,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8UnormSrgb,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let view = texture.create_view(&Default::default());
        (texture, view)
    }

    /// The mean difference between `a` and `b`, as a proportion of full, within the rectangle from `from` to `to`, in
    /// pixels.
    fn difference(
        a: &image::RgbaImage,
        b: &image::RgbaImage,
        from: (u32, u32),
        to: (u32, u32),
    ) -> f32 {
        let mut total = 0.0;
        let mut count = 0.0;
        for x in from.0..to.0 {
            for y in from.1..to.1 {
                let (p, q) = (a.get_pixel(x, y).0, b.get_pixel(x, y).0);
                total += (0..3)
                    .map(|channel| (f32::from(p[channel]) - f32::from(q[channel])).abs())
                    .sum::<f32>()
                    / (3.0 * 255.0);
                count += 1.0;
            }
        }
        total / count
    }

    /// Set `TOMORI_WALL_TILT_PREVIEW` to a PNG path to save what this renders: a wall someone added above and ahead of
    /// the participant, upright; the same wall tilted 30° down towards them, as the server sends it; taken hold of, its
    /// handles around it and the hinge at the middle of its left edge highlighted under the ray; and its hinge dragged,
    /// the hand moved down, tilting it towards them as it would be before it is let go, the tilt said above it.
    #[test]
    fn a_wall_above_is_drawn_tilted_down_towards_the_participant_and_tilts_as_its_hinge_is_dragged()
    {
        const WIDTH: u32 = 960;
        const HEIGHT: u32 = 720;
        use tomori_protocol::{Panel, Place, Wall, WallKind};
        use tomori_scene::projection::editing::{Handle, handle_point};
        let middle = Vec3::new(0.0, 2.75, -2.2);
        let upright = Wall::new(
            "wall-1",
            WallKind::Added,
            tomori_protocol::Pose::upright_facing(middle.to_array(), [0.0, 2.75, 0.0]),
        );
        let tilted = upright.clone().with_tilt(Some(30.0));
        let panels: Vec<Panel> = ["Build", "Tests", "Coverage", "Release"]
            .iter()
            .enumerate()
            .map(|(slot, title)| {
                Panel::new(
                    format!("panel-{slot}"),
                    *title,
                    format!("**{title}** is green.\n\n- checked\n- kept"),
                    Place::Wall {
                        wall: "wall-1".into(),
                        slot: slot as u32,
                    },
                )
            })
            .collect();
        let frame_of = |wall: &Wall| {
            let frame = wall.frame();
            Mat4::from_rotation_translation(
                glam::Quat::from_array(frame.orientation),
                Vec3::from_array(frame.position),
            )
        };
        let hinge = handle_point(&upright, frame_of(&upright), Handle::Tilt).0;
        test_gpu::with(|test_gpu::Gpu { device, queue, .. }| {
            let scope = device.push_error_scope(wgpu::ErrorFilter::Validation);
            let format = wgpu::TextureFormat::Rgba8UnormSrgb;
            let (texture, view) = eye(&device, "wall tilt test eye", (WIDTH, HEIGHT));
            let mut renderer = Renderer::new(&device, &queue, format, WIDTH, HEIGHT);
            renderer.set_environment(tomori_scene::environment::Environment::MINIMAL);
            let mut drawn_panels = Panels::new(&device, &renderer);
            let lens = glam::camera::rh::proj::directx::perspective(
                1.3,
                WIDTH as f32 / HEIGHT as f32,
                0.05,
                50.0,
            );
            let view_matrix =
                lens * glam::camera::rh::view::look_at_mat4(arrival::HOME, middle, Vec3::Y);
            let head = Mat4::from_translation(arrival::HOME);
            let from = |hand: Vec3, aim: Vec3, grip: f32| Senses {
                focused: true,
                head: Some(head),
                head_pose: Pose::from_transform(head),
                viewer: Some(arrival::HOME),
                space_head: head,
                trigger: Some(0.0),
                beam: true,
                grip: Some(grip),
                ray: tomori_scene::pointing::Ray::new(hand, aim - hand),
                ..Default::default()
            };
            let senses = |aim: Vec3, grip: f32| from(arrival::HOME, aim, grip);
            let now = Instant::now();
            let mut capture = |step: &mut Step,
                               conversation: &mut tomori_scene::conversation::Conversation,
                               senses: &Senses| {
                let frame = step.update(senses, conversation, &mut felt(), now, 0.01);
                draw(&mut renderer, &device, &queue, &mut drawn_panels, &frame);
                renderer.render(
                    &device,
                    &queue,
                    std::slice::from_ref(&view),
                    &[view_matrix],
                    Mat4::IDENTITY,
                    &drawn_panels.list(Mat4::IDENTITY),
                );
                read_back(&device, &queue, &texture, (WIDTH, HEIGHT))
            };
            let away = senses(arrival::HOME + Vec3::new(2.0, -1.0, 0.0), 0.0);
            // Upright, and tilted as the server sends it.
            let mut conversation = joined();
            show_editable_walls(&mut conversation, vec![upright.clone()], panels.clone());
            let shown_upright = capture(&mut Step::default(), &mut conversation, &away);
            let mut conversation = joined();
            show_editable_walls(&mut conversation, vec![tilted.clone()], panels.clone());
            let shown_tilted = capture(&mut Step::default(), &mut conversation, &away);
            // Where the top row's first slot is drawn, in pixels, upright and tilted: the tilted one is drawn where it
            // stands tilted, and not where it stood.
            let pixel = |at: Vec3| {
                let clip = view_matrix * at.extend(1.0);
                let (x, y) = (clip.x / clip.w, clip.y / clip.w);
                assert!(x.abs() < 1.0 && y.abs() < 1.0, "{at} is in view");
                (
                    ((x + 1.0) / 2.0 * WIDTH as f32) as u32,
                    ((1.0 - y) / 2.0 * HEIGHT as f32) as u32,
                )
            };
            let around = |(x, y): (u32, u32)| ((x - 20, y - 20), (x + 20, y + 20));
            // The wall as drawn, upright or tilted, in pixels: over it, the tilted one is drawn where it stands, square
            // to the eyes, and the upright one foreshortened.
            let (board_width, board_height) = upright.board().unwrap();
            let corners: Vec<(u32, u32)> = [-1.0, 1.0]
                .iter()
                .flat_map(|x| [-1.0, 1.0].map(|y| (*x, y)))
                .flat_map(|(x, y)| {
                    let local = Vec3::new(x * board_width / 2.0, y * board_height / 2.0, 0.0);
                    [&upright, &tilted].map(|wall| pixel(frame_of(wall).transform_point3(local)))
                })
                .collect();
            let over = (
                (
                    corners.iter().map(|at| at.0).min().unwrap(),
                    corners.iter().map(|at| at.1).min().unwrap(),
                ),
                (
                    corners.iter().map(|at| at.0).max().unwrap(),
                    corners.iter().map(|at| at.1).max().unwrap(),
                ),
            );
            assert!(
                difference(&shown_upright, &shown_tilted, over.0, over.1) > 0.01,
                "the tilted wall is drawn where it stands"
            );
            // Tilted towards the eyes below it, its first slot is drawn taller than upright.
            let height = |wall: &Wall| {
                let frame = tomori_scene::projection::slot_frames(wall, frame_of(wall))[0];
                let (_, slot_height) = wall.slot_size();
                let half = Vec3::Y * slot_height / 2.0 / wall.scale();
                pixel(frame.transform_point3(-half)).1 - pixel(frame.transform_point3(half)).1
            };
            let (square, foreshortened) = (height(&tilted), height(&upright));
            assert!(square > foreshortened + 5, "{square} {foreshortened}");
            // Taken hold of, its hinge pointed at, highlighted.
            let mut conversation = joined();
            show_editable_walls(&mut conversation, vec![upright.clone()], panels.clone());
            let mut step = Step::default();
            let board = middle + Vec3::Y * 0.42;
            capture(&mut step, &mut conversation, &senses(board, 1.0));
            capture(&mut step, &mut conversation, &senses(board, 0.0));
            let held = capture(&mut step, &mut conversation, &senses(hinge, 0.0));
            assert!(
                difference(
                    &shown_upright,
                    &held,
                    around(pixel(hinge)).0,
                    around(pixel(hinge)).1
                ) > 0.02,
                "the hinge is drawn"
            );
            // Its hinge dragged, the hand moved down 25 cm: the wall tilts towards the participant before it is let go.
            capture(&mut step, &mut conversation, &senses(hinge, 1.0));
            let dragged = capture(
                &mut step,
                &mut conversation,
                &from(arrival::HOME - Vec3::Y * 0.25, hinge, 1.0),
            );
            assert!(
                difference(&held, &dragged, over.0, over.1) > 0.01,
                "the wall tilts as the hinge is dragged"
            );
            capture(
                &mut step,
                &mut conversation,
                &from(arrival::HOME - Vec3::Y * 0.25, hinge, 0.0),
            );
            let sent = conversation.take_actions();
            assert!(
                sent.iter().any(|action| matches!(
                    action,
                    tomori_client::connection::Action::Send(tomori_protocol::ClientMessage::SetWall { tilt: Some(tilt), .. })
                        if *tilt > 15.0
                )),
                "{sent:?}"
            );
            let mut sheet = image::RgbaImage::new(WIDTH, HEIGHT * 4);
            for (row, image) in [&shown_upright, &shown_tilted, &held, &dragged]
                .iter()
                .enumerate()
            {
                image::imageops::replace(&mut sheet, *image, 0, (HEIGHT * row as u32).into());
            }
            crate::test_support::preview("TOMORI_WALL_TILT_PREVIEW", &sheet);
            assert!(pollster::block_on(scope.pop()).is_none());
        });
    }

    /// Set `TOMORI_BIG_CURVED_HANDLES_PREVIEW` to a PNG path to save what this renders: a wall of one slot five times a
    /// standard one, bent as tightly as it may, 4.5 m away, before and after it is taken hold of, its hinge pointed at:
    /// its handles stand beside its slot's edges, where the slot is drawn, not out in front of them where an arc as long
    /// as the slot would end.
    #[test]
    fn a_big_wall_bent_tightly_shows_its_handles_beside_its_slot_s_edges() {
        const WIDTH: u32 = 960;
        const HEIGHT: u32 = 720;
        use tomori_protocol::{Grid, Wall, WallKind, WallShape};
        use tomori_scene::projection::editing::{Handle, handle_point};
        let middle = Vec3::new(0.0, 1.6, -4.5);
        let flat = Wall::new(
            "wall-1",
            WallKind::Added,
            tomori_protocol::Pose::upright_facing(middle.to_array(), arrival::HOME.to_array()),
        )
        .with_grid(Grid::new(1, 1).unwrap())
        .with_scale(tomori_protocol::wall::SCALES.1);
        // As tightly as it may bend: as the knob dragged all the way makes it.
        let radius = tomori_scene::projection::editing::bent(&flat, 1e3).unwrap();
        let wall = flat.with_shape(WallShape::Arc { radius });
        let frame =
            Mat4::from_rotation_translation(glam::Quat::from_array(wall.pose.orientation), middle);
        let slot = tomori_scene::projection::slot_frames(&wall, frame)[0];
        let half = wall.slot_size().0 / 2.0 / wall.scale();
        let (left, right) = (
            slot.transform_point3(-Vec3::X * half),
            slot.transform_point3(Vec3::X * half),
        );
        let point = |handle: Handle| handle_point(&wall, frame, handle).0;
        test_gpu::with(|test_gpu::Gpu { device, queue, .. }| {
            let scope = device.push_error_scope(wgpu::ErrorFilter::Validation);
            let format = wgpu::TextureFormat::Rgba8UnormSrgb;
            let (texture, view) = eye(&device, "big curved wall test eye", (WIDTH, HEIGHT));
            let mut renderer = Renderer::new(&device, &queue, format, WIDTH, HEIGHT);
            renderer.set_environment(tomori_scene::environment::Environment::MINIMAL);
            let mut drawn_panels = Panels::new(&device, &renderer);
            let lens = glam::camera::rh::proj::directx::perspective(
                1.2,
                WIDTH as f32 / HEIGHT as f32,
                0.05,
                50.0,
            );
            let view_matrix =
                lens * glam::camera::rh::view::look_at_mat4(arrival::HOME, middle, Vec3::Y);
            let head = Mat4::from_translation(arrival::HOME);
            let senses = |aim: Vec3, grip: f32| Senses {
                focused: true,
                head: Some(head),
                head_pose: Pose::from_transform(head),
                viewer: Some(arrival::HOME),
                space_head: head,
                trigger: Some(0.0),
                beam: true,
                grip: Some(grip),
                ray: tomori_scene::pointing::Ray::new(arrival::HOME, aim - arrival::HOME),
                ..Default::default()
            };
            let now = Instant::now();
            let mut capture = |step: &mut Step,
                               conversation: &mut tomori_scene::conversation::Conversation,
                               senses: &Senses| {
                let frame = step.update(senses, conversation, &mut felt(), now, 0.01);
                draw(&mut renderer, &device, &queue, &mut drawn_panels, &frame);
                renderer.render(
                    &device,
                    &queue,
                    std::slice::from_ref(&view),
                    &[view_matrix],
                    Mat4::IDENTITY,
                    &drawn_panels.list(Mat4::IDENTITY),
                );
                read_back(&device, &queue, &texture, (WIDTH, HEIGHT))
            };
            let pixel = |at: Vec3| {
                let clip = view_matrix * at.extend(1.0);
                let (x, y) = (clip.x / clip.w, clip.y / clip.w);
                assert!(x.abs() < 1.0 && y.abs() < 1.0, "{at} is in view");
                glam::Vec2::new(
                    (x + 1.0) / 2.0 * WIDTH as f32,
                    (1.0 - y) / 2.0 * HEIGHT as f32,
                )
            };
            let around = |at: glam::Vec2| {
                (
                    (at.x as u32 - 20, at.y as u32 - 20),
                    (at.x as u32 + 20, at.y as u32 + 20),
                )
            };
            let mut conversation = joined();
            show_editable_walls(&mut conversation, vec![wall.clone()], Vec::new());
            let mut step = Step::default();
            let empty = slot.transform_point3(Vec3::Y * 0.1);
            let plain = capture(
                &mut step,
                &mut conversation,
                &senses(middle + Vec3::Y * 3.0, 0.0),
            );
            // Taken hold of by a squeeze on its empty slot, let go, and its hinge pointed at.
            capture(&mut step, &mut conversation, &senses(empty, 1.0));
            capture(&mut step, &mut conversation, &senses(empty, 0.0));
            let hinge = point(Handle::Tilt);
            let held = capture(&mut step, &mut conversation, &senses(hinge, 0.0));
            // Its hinge and the handle of its columns are drawn, each beside its slot's edge as seen, within a handle
            // or so, where the slot is drawn.
            for (handle, edge) in [(Handle::Tilt, left), (Handle::Columns, right)] {
                let at = pixel(point(handle));
                let (from, to) = around(at);
                assert!(
                    difference(&plain, &held, from, to) > 0.02,
                    "{handle:?} is drawn"
                );
                let apart = at.distance(pixel(edge));
                assert!(apart < 70.0, "{handle:?} is {apart} pixels from its edge");
            }
            let mut sheet = image::RgbaImage::new(WIDTH, HEIGHT * 2);
            image::imageops::replace(&mut sheet, &plain, 0, 0);
            image::imageops::replace(&mut sheet, &held, 0, HEIGHT.into());
            crate::test_support::preview("TOMORI_BIG_CURVED_HANDLES_PREVIEW", &sheet);
            assert!(pollster::block_on(scope.pop()).is_none());
        });
    }

    /// Set `TOMORI_BIG_WALL_PREVIEW` to a PNG path to save what this renders: a standard wall 3 m away, a wall five times
    /// as large 15 m away, which looks the same, its text as large to the eye, and a standard wall 15 m away, too small
    /// to read, each with the same panels, from home.
    #[test]
    fn a_big_wall_far_away_reads_as_a_standard_wall_does_near() {
        const WIDTH: u32 = 960;
        const HEIGHT: u32 = 720;
        use tomori_protocol::{Panel, Place, Wall, WallKind};
        // To the right of straight ahead, clear of the swarm and the pill.
        let ahead = Vec3::new(1.0, 0.0, -1.0).normalize();
        let wall_at = |distance: f32, scale: f32| {
            // Its middle at eye height, so that it is seen square on.
            let at = arrival::HOME + ahead * distance;
            Wall::new(
                "wall-1",
                WallKind::Added,
                tomori_protocol::Pose::upright_facing(at.to_array(), arrival::HOME.to_array()),
            )
            .with_scale(scale)
        };
        let panels: Vec<Panel> = ["Build", "Deploys", "Errors"]
            .iter()
            .enumerate()
            .map(|(slot, title)| {
                Panel::new(
                    format!("panel-{slot}"),
                    *title,
                    format!("**{title}**: 12 today, 3 failing.\n\n- api: ok\n- web: slow"),
                    Place::Wall {
                        wall: "wall-1".into(),
                        slot: slot as u32,
                    },
                )
            })
            .collect();
        test_gpu::with(|test_gpu::Gpu { device, queue, .. }| {
            let scope = device.push_error_scope(wgpu::ErrorFilter::Validation);
            let format = wgpu::TextureFormat::Rgba8UnormSrgb;
            let (texture, view) = eye(&device, "big wall test eye", (WIDTH, HEIGHT));
            let mut renderer = Renderer::new(&device, &queue, format, WIDTH, HEIGHT);
            renderer.set_environment(tomori_scene::environment::Environment::MINIMAL);
            let mut drawn_panels = Panels::new(&device, &renderer);
            let lens = glam::camera::rh::proj::directx::perspective(
                1.0,
                WIDTH as f32 / HEIGHT as f32,
                0.05,
                60.0,
            );
            let view_matrix = lens
                * glam::camera::rh::view::look_at_mat4(
                    arrival::HOME,
                    arrival::HOME + ahead,
                    Vec3::Y,
                );
            let head = Mat4::from_translation(arrival::HOME);
            let senses = Senses {
                focused: true,
                head: Some(head),
                head_pose: Pose::from_transform(head),
                viewer: Some(arrival::HOME),
                space_head: head,
                trigger: Some(0.0),
                ray: tomori_scene::pointing::Ray::new(arrival::HOME, -Vec3::Y),
                ..Default::default()
            };
            let mut capture = |wall: Wall| {
                let mut conversation = joined();
                show_projection(&mut conversation, vec![wall], panels.clone());
                let mut step = Step::default();
                let frame = step.update(
                    &senses,
                    &mut conversation,
                    &mut felt(),
                    Instant::now(),
                    0.01,
                );
                draw(&mut renderer, &device, &queue, &mut drawn_panels, &frame);
                renderer.render(
                    &device,
                    &queue,
                    std::slice::from_ref(&view),
                    &[view_matrix],
                    Mat4::IDENTITY,
                    &drawn_panels.list(Mat4::IDENTITY),
                );
                read_back(&device, &queue, &texture, (WIDTH, HEIGHT))
            };
            let near = capture(wall_at(3.0, 1.0));
            let big = capture(wall_at(15.0, tomori_protocol::wall::SCALES.1));
            let small = capture(wall_at(15.0, 1.0));
            // Over the wall, as the standard one near is seen: the big one far away is drawn alike, its text as large,
            // while the standard one as far away is not.
            let (width, height) = wall_at(3.0, 1.0).size();
            let half =
                |size: f32| (size / 2.0 / 3.0 / (1.0f32 / 2.0).tan() * HEIGHT as f32 / 2.0) as u32;
            let (across, down) = (
                half(width).min(WIDTH / 2 - 1),
                half(height).min(HEIGHT / 2 - 1),
            );
            let from = (WIDTH / 2 - across, HEIGHT / 2 - down);
            let to = (WIDTH / 2 + across, HEIGHT / 2 + down);
            let alike = difference(&near, &big, from, to);
            let unlike = difference(&near, &small, from, to);
            assert!(alike < unlike / 3.0, "{alike} against {unlike}");
            let mut sheet = image::RgbaImage::new(WIDTH, HEIGHT * 3);
            for (row, image) in [&near, &big, &small].iter().enumerate() {
                image::imageops::replace(&mut sheet, *image, 0, (HEIGHT * row as u32).into());
            }
            crate::test_support::preview("TOMORI_BIG_WALL_PREVIEW", &sheet);
            assert!(pollster::block_on(scope.pop()).is_none());
        });
    }
}
