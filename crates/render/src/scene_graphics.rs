// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

//! Drawing what the frame step decided: [`draw`] applies a [`Frame`] to the renderer, and [`Panels`] holds the shared panels' GPU buffers, for both the headset and the desktop window.
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
    /// The GitHub environment's inbox and cards, one panel each, kept as their number falls so that they need not be
    /// made again, with the models of those drawn now.
    cards: Vec<Panel>,
    card_models: Vec<Mat4>,
}

/// The most vertices one card's face takes, about three times what a full face of text does.
const CARD_CAPACITY: usize = CAPACITY / 2;

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
            card_models: Vec::new(),
        }
    }

    /// Upload the inbox and cards, `sheets`, making panels for them as needed.
    pub fn set_cards(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        renderer: &Renderer,
        sheets: &[Sheet],
    ) {
        self.card_models.clear();
        for sheet in sheets {
            let (Some(canvas), Some(model)) = (&sheet.canvas, sheet.model) else {
                continue;
            };
            let index = self.card_models.len();
            if index == self.cards.len() {
                self.cards.push(renderer.panel(device, CARD_CAPACITY));
            }
            self.cards[index].set(queue, canvas);
            self.card_models.push(model);
        }
    }

    /// Upload what each panel shows now, and remember where it is drawn.
    pub fn set(&mut self, queue: &wgpu::Queue, panels: &scene::Panels) {
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
    }

    /// The panels to draw, each with its model in the eyes' space, which `scene_to_space` takes the scene to: the
    /// inbox and cards, in the order the frame step gave them, farthest first, so that a nearer card covers one behind
    /// it, as panels do not write depth; then the pill, card, badge and transcript.
    pub fn list(&self, scene_to_space: Mat4) -> Vec<(&Panel, Mat4)> {
        let cards = self
            .cards
            .iter()
            .zip(&self.card_models)
            .map(|(panel, model)| (panel, scene_to_space * *model));
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

/// Apply `frame` to `renderer` and `panels`, ready to render.
pub fn draw(
    renderer: &mut Renderer,
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    panels: &mut Panels,
    frame: &Frame,
) {
    renderer.set_scenery(frame.scenery);
    if let Some(drawn) = &frame.environment {
        renderer.animate(device, queue, drawn.as_ref());
    }
    renderer.set_landing(frame.landing);
    renderer.set_blink(frame.blink);
    renderer.update(queue, frame.swarm, frame.marker, frame.attracting);
    renderer.set_presentation_page(frame.card);
    renderer.set_presentation_content_page(frame.content);
    renderer.update_workspace(device, queue, frame.cubes, frame.stations, frame.workspace);
    if let Some(arrival) = &frame.arrival {
        renderer.update_arrival(
            device,
            queue,
            &arrival_graphics::Frame {
                scene: &arrival.scene,
                hovered: arrival.hovered.as_ref(),
                ray: arrival.ray,
                time: arrival.time,
            },
        );
    }
    panels.set(queue, &frame.panels);
    panels.set_cards(device, queue, renderer, &frame.cards);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_gpu;
    use crate::{
        arrival,
        controller::Pose,
        hand_menu::{HOLD, Palm},
        scene::{Senses, Step, fixtures::*},
    };
    use glam::Vec3;
    use std::time::Instant;

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
            assert_eq!(panels.card_models.len(), 2);
            // A sheet without a canvas is not drawn.
            panels.set_cards(&device, &queue, &renderer, &[Sheet::default()]);
            assert!(panels.card_models.is_empty());
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
            let capture = |renderer: &Renderer, panels: &Panels| {
                // About the field of view the headset gives the inbox, to judge its text at the headset's resolution.
                let matrix = Mat4::perspective_rh(0.45, WIDTH as f32 / HEIGHT as f32, 0.05, 50.0)
                    * Mat4::look_at_rh(arrival::HOME, inbox + Vec3::new(0.1, 0.0, 0.0), Vec3::Y);
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
            let bare = capture(&renderer, &panels);
            let frame = step.update(&senses, &mut conversation, &mut felt(), now, 0.01);
            draw(&mut renderer, &device, &queue, &mut panels, &frame);
            let pointed = capture(&renderer, &panels);
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
            let moved = capture(&renderer, &panels);
            assert_ne!(moved, pointed);
            let mut sheet = image::RgbaImage::new(WIDTH, HEIGHT * 2);
            image::imageops::replace(&mut sheet, &pointed, 0, 0);
            image::imageops::replace(&mut sheet, &moved, 0, HEIGHT.into());
            crate::test_support::preview("TOMORI_CARDS_PREVIEW", &sheet);
            assert!(pollster::block_on(scope.pop()).is_none());
        });
    }
}
