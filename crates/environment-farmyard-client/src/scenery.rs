// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

//! The farmyard's scenery, as Tomori's clients draw it in each eye's pass: the yard and the coop, models drawn with
//! `tomori-assets`, lit by the moon and the lanterns' warm light; beyond them the backdrop, its foothills and its bush,
//! models too, its river and waterfall, drawn with a shader of its own that moves them, then the evening sky behind
//! everything, where nothing nearer covers it, and the ranges at the sky's depth; the trail round the outside of the fence, and the river's stones; the ferns
//! about the trail, drawn with a shader of their own that sways them in the breeze; the stations' plaques, their text drawn from the renderer's glyph atlas; the campsite by the pool, its bonfire's flames
//! and embers drawn with a shader of its own that moves them, lighting what is about it with a flickering lamp, and its
//! marshmallow sticks where its client side shows them, and the trails of light their burning marshmallows paint in
//! the air (see [`crate::embers`]); all of it growing out of the space through the reveal sphere as
//! the space's environment does. Its large models are split into chunks as they are uploaded ([`CHUNK`]), and its
//! models, ferns and plaques, with their bounds, its lamps and its moonlight are submitted to the renderer's world,
//! and once a frame what neither eye can see of them is culled, and each model's pieces, each chunk among them, and
//! each fern are lit by the few lamps the world lists for them, rather than by every lamp.
use std::ops::Range;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use bytemuck::{Pod, Zeroable};
use glam::{Mat4, Vec3};
use tomori_assets::bounds::{Aabb, Tally};
use tomori_assets::gpu::{Batch, GpuModel, Lamp, Lighting, Renderer};
use tomori_assets::{Instance, Library, Model};
use tomori_render::scenery::{Scenery, Target, Views};
use tomori_render::world::{
    EnvironmentLight, Item, ItemId, Layer, Light, LightId, Mobility, RootId, World,
};
use wgpu::util::DeviceExt;

use crate::embers::{self, EmberVertex, Embers};

use tomori_environment_farmyard::backdrop::{self, Kind};
use tomori_environment_farmyard::beach;
use tomori_environment_farmyard::campsite;
use tomori_environment_farmyard::coop::{self, Coop};
use tomori_environment_farmyard::ferns::{self, Detail, Fern};
use tomori_environment_farmyard::layout;
use tomori_environment_farmyard::props::{self, evening, palette};
use tomori_environment_farmyard::terrace;
use tomori_environment_farmyard::trail;
use tomori_render::multiview::Uniforms;
use tomori_render::ui_panel::{Canvas, Panel, Pipeline};
use tomori_scene::ui_text::{Row, Style};

// The farmyard's models are lit by as many lamps as the render world lights a root by, and each piece by as many as it
// lights an instance by.
const _: () = assert!(
    tomori_assets::gpu::LAMPS == tomori_render::world::LIGHTS
        && tomori_assets::gpu::PIECE_LAMPS == tomori_render::world::LIGHTS_PER_INSTANCE
);

/// How far across the ground, at most, each chunk of the farmyard's models is, so that each is culled and lit on its
/// own: the yard, its meadow and pines 43 by 45 m, the foothills 72 m, the trail 22 m and the upper terrace 10 m are
/// each split, and nothing else is as large. Of 32 lamps 5 to 6 m apart, at most 9 reach a chunk of 7 m, where 12
/// reached one of 8 m. See `design/rendering.md`.
pub const CHUNK: f32 = 7.0;

/// What the sky's, the ranges', the water's, the fire's and the ferns' shaders are told for each eye.
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct SkyCamera {
    inverse_view_projection: [f32; 16],
    reveal: [f32; 4],
    view_projection: [f32; 16],
    eye: [f32; 4],
    haze: [f32; 4],
    time: [f32; 4],
}

/// How the ferns are lit, as the farmyard's models are, as their shader reads it beside each eye's [`SkyCamera`]: the
/// same for both eyes, so written once a frame, as the first eye is prepared, and bound in every eye's pass.
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct FernLighting {
    moon: [f32; 4],
    moonlight: [f32; 4],
    sky: [f32; 4],
    ground: [f32; 4],
    lamps: [[f32; 4]; 2 * tomori_assets::gpu::LAMPS],
    /// How many lamps there are, in its first component.
    counts: [u32; 4],
}

/// A point of a fern's mesh, as its shader reads it.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Pod, Zeroable)]
pub(crate) struct FernVertex {
    position: [f32; 3],
    normal: [f32; 3],
    color: [f32; 3],
    underside: [f32; 3],
    sway: f32,
}

/// A fern placed, as its shader reads it: its foot and how it is turned, how large it is and its phase, and its lamps,
/// as [`tomori_assets::gpu::lamps`] packs them for the farmyard's models, or every lamp, [`EVERY`].
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Pod, Zeroable)]
pub(crate) struct FernInstance {
    foot: [f32; 4],
    size: [f32; 4],
    lamps: [u32; 4],
}

impl From<&Fern> for FernInstance {
    /// `fern`, lit by every lamp.
    fn from(fern: &Fern) -> Self {
        Self {
            foot: fern.at.extend(fern.turn).to_array(),
            size: [fern.scale, fern.phase, 0.0, 0.0],
            lamps: EVERY,
        }
    }
}

/// A fern's lamps when none are listed: every lamp of the farmyard's.
pub(crate) const EVERY: [u32; 4] = tomori_assets::gpu::EVERY_LAMP;

/// A point of the ranges, as their shader reads it.
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct FarVertex {
    direction: [f32; 3],
    color: [f32; 4],
}

/// A point of the water, as its shader reads it.
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct WaterVertex {
    position: [f32; 3],
    color: [f32; 4],
    flow: [f32; 2],
}

/// A corner of the fire's flames and embers, as its shader reads it.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Pod, Zeroable)]
pub(crate) struct FireVertex {
    foot: [f32; 3],
    corner: [f32; 2],
    color: [f32; 4],
    motion: [f32; 4],
}

/// A flame's tongue standing on `foot`, `height` tall and `width` across at its foot, moving by `phase`, from `base`'s
/// colour at its foot to `tip`'s, each with how brightly it glows: three triangles, its foot, its middle and its tip.
fn tongue(
    foot: Vec3,
    (height, width): (f32, f32),
    phase: f32,
    (base, tip): (glam::Vec4, glam::Vec4),
) -> [FireVertex; 9] {
    let corner = |x: f32, y: f32| FireVertex {
        foot: foot.to_array(),
        corner: [x, y],
        color: base.lerp(tip, y).to_array(),
        motion: [phase, height, width * 0.5, 0.0],
    };
    let (left, right) = (corner(-1.0, 0.0), corner(1.0, 0.0));
    let (middle_left, middle_right) = (corner(-1.0, 0.45), corner(1.0, 0.45));
    let top = corner(0.0, 1.0);
    [
        left,
        right,
        middle_right,
        left,
        middle_right,
        middle_left,
        middle_left,
        middle_right,
        top,
    ]
}

/// An ember rising from `foot` as far as `rise` over its life, `size` across, moving by `phase`: two triangles.
fn ember(foot: Vec3, rise: f32, size: f32, phase: f32) -> [FireVertex; 6] {
    let corner = |x: f32, y: f32| FireVertex {
        foot: foot.to_array(),
        corner: [x, y],
        color: [1.0, 0.5, 0.12, 2.2],
        motion: [phase, rise, size, 1.0],
    };
    let [a, b, c, d] = [
        corner(-1.0, 0.0),
        corner(1.0, 0.0),
        corner(1.0, 1.0),
        corner(-1.0, 1.0),
    ];
    [a, b, c, a, c, d]
}

/// How many embers rise from the bonfire at its biggest; at its default, about a dozen of them.
const EMBERS: u32 = 24;

/// The bonfire's flames and embers: five orange tongues about the fire's middle, three yellow ones within them, and
/// [`EMBERS`] rising, each moving by a phase of its own; its shader draws its flames as big, and as many of its embers,
/// as the fire is.
pub(crate) fn bonfire() -> Vec<FireVertex> {
    use tomori_environment_farmyard::props::scatter;
    let middle = campsite::FIRE + Vec3::Y * (tomori_environment_farmyard::trail::LIFT + 0.02);
    let mut vertices = Vec::new();
    let outer = (
        glam::Vec4::new(1.0, 0.42, 0.07, 1.5),
        glam::Vec4::new(0.9, 0.16, 0.02, 0.45),
    );
    let inner = (
        glam::Vec4::new(1.0, 0.86, 0.5, 1.7),
        glam::Vec4::new(1.0, 0.5, 0.08, 0.8),
    );
    for index in 0..5u32 {
        let turn = index as f32 / 5.0 * std::f32::consts::TAU + 0.3;
        let foot = middle + Vec3::new(turn.sin(), 0.0, turn.cos()) * 0.1;
        let height = 0.58 + scatter(index + 9900) * 0.22;
        let width = 0.2 + scatter(index + 9910) * 0.06;
        vertices.extend(tongue(
            foot,
            (height, width),
            scatter(index + 9920) * 6.0,
            outer,
        ));
    }
    for index in 0..3u32 {
        let turn = index as f32 / 3.0 * std::f32::consts::TAU + 1.1;
        let foot = middle + Vec3::new(turn.sin(), 0.0, turn.cos()) * 0.04;
        let height = 0.38 + scatter(index + 9930) * 0.12;
        vertices.extend(tongue(
            foot,
            (height, 0.13),
            scatter(index + 9940) * 6.0,
            inner,
        ));
    }
    for index in 0..EMBERS {
        let turn = scatter(index + 9950) * std::f32::consts::TAU;
        let foot = middle + Vec3::new(turn.sin(), 0.0, turn.cos()) * 0.12;
        vertices.extend(ember(
            foot,
            1.1 + scatter(index + 9960) * 0.6,
            0.012,
            scatter(index + 9970),
        ));
    }
    vertices
}

/// The little flame of a marshmallow that burns, at `marshmallow`, the middle of its marshmallow: as tall as about two
/// and a half of it and a little wider than it is long, so that it grows with it, and as big however big the fire is.
pub(crate) fn alight(marshmallow: Vec3, phase: f32) -> [FireVertex; 9] {
    let (length, radius) = campsite::MARSHMALLOW;
    let mut flame = tongue(
        marshmallow - Vec3::Y * radius * 0.6,
        (length * 2.4, length * 1.1),
        phase,
        (
            glam::Vec4::new(1.0, 0.7, 0.25, 1.4),
            glam::Vec4::new(0.9, 0.2, 0.03, 0.5),
        ),
    );
    for corner in &mut flame {
        corner.motion[3] = 2.0;
    }
    flame
}

/// How the bonfire's light flickers at `seconds`: a few waves of its own, about 1, never below 0.75.
pub fn flicker(seconds: f32) -> f32 {
    1.0 + 0.12 * (seconds * 8.3).sin()
        + 0.08 * (seconds * 13.7 + 1.3).sin()
        + 0.05 * (seconds * 29.0).sin()
}

/// The bonfire's lamp at `seconds`, with the fire at `size`: warm, a little above its heart, reaching about 4 m at its
/// default size, a little smaller than it first was, and 7 m at its biggest, as much brighter, flickering.
pub fn fire_lamp(seconds: f32, size: f32) -> Lamp {
    let flicker = flicker(seconds);
    let size = campsite::unit(size);
    Lamp::new(
        campsite::heart_at(size) + Vec3::Y * 0.15,
        4.2 + 2.8 * size + 0.3 * (flicker - 1.0),
        Vec3::new(1.0, 0.5, 0.18) * (1.8 + 1.4 * size) * flicker,
    )
}

/// How many of the bonfire's [`EMBERS`] rise with the fire at `size`: half of them at its default, all at its biggest.
pub fn embers(size: f32) -> f32 {
    0.5 + 0.5 * campsite::unit(size)
}

/// What moves the water: the time since the scenery was made, wrapping every quarter of an hour, or a moment held
/// still, for pictures that must be the same each time they are drawn.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Clock {
    Running,
    Held(f32),
}

impl Clock {
    /// The time it says, in seconds, for a scenery made at `made`.
    pub fn seconds(self, made: Instant) -> f32 {
        match self {
            Clock::Running => (made.elapsed().as_secs_f64() % crate::embers::WRAP) as f32,
            Clock::Held(seconds) => seconds,
        }
    }

    /// As [`Clock::seconds`], unwrapped.
    pub fn elapsed(self, made: Instant) -> f64 {
        match self {
            Clock::Running => made.elapsed().as_secs_f64(),
            Clock::Held(seconds) => f64::from(seconds),
        }
    }
}

/// How the farmyard is lit: a cool moon low in the east, the sky's blue overhead and the earth's brown below, the
/// lanterns' warm light by the coop's door, on the gate and over the workbench, the campsite's bonfire, at its default
/// size, and a dusky haze over the far meadow.
pub fn lighting() -> Lighting {
    lighting_at(0.0, 0.0)
}

/// As [`lighting`], with the bonfire's light as it flickers at `seconds`, the fire at `size`.
pub fn lighting_at(seconds: f32, size: f32) -> Lighting {
    lanterns()
        .into_iter()
        .fold(
            Lighting::new(
                evening::MOON.normalize(),
                evening::MOONLIGHT,
                evening::SKY,
                evening::GROUND,
            )
            .with_haze(Vec3::new(0.03, 0.033, 0.06), 0.0008),
            Lighting::with_lamp,
        )
        .with_lamp(fire_lamp(seconds, size))
}

/// The lanterns, which never move: the coop's, the gate's and the workbench's.
fn lanterns() -> [Lamp; 3] {
    [
        Lamp::new(layout::LANTERN, 6.0, palette::GLOW * 1.4),
        Lamp::new(layout::GATE_LANTERN, 4.5, palette::GLOW * 1.1),
        Lamp::new(layout::workbench_lantern(), 3.5, palette::GLOW * 1.2),
    ]
}

/// Where the eye is, from its view and projection: the point every ray of the view begins from, as every scenery is
/// told it.
pub use tomori_render::scenery::eye;

/// One model the farmyard draws, uploaded once, with its instances placed, and the item it is in the render world, once
/// submitted.
struct Drawn {
    model: GpuModel,
    batch: Batch,
    item: Option<ItemId>,
}

impl Drawn {
    fn new(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        renderer: &Renderer,
        model: &Arc<Model>,
        placement: Mat4,
    ) -> Self {
        Self::placed(device, queue, renderer, model, &[placement])
    }

    /// `model`, uploaded once, with an instance at each of `placements`, drawn together.
    fn placed(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        renderer: &Renderer,
        model: &Arc<Model>,
        placements: &[Mat4],
    ) -> Self {
        let gpu = renderer.upload(device, queue, model);
        let mut batch = renderer.batch(device);
        let instances: Vec<Instance> = placements
            .iter()
            .map(|placement| {
                let mut instance = Instance::new(model.clone());
                instance.transform = *placement;
                instance
            })
            .collect();
        renderer.place(device, queue, &gpu, &mut batch, &instances);
        Self {
            model: gpu,
            batch,
            item: None,
        }
    }

    /// Add it to `world`, in `root`, if it is not there, and place each piece of it where its batch has it: once, if it is
    /// static, or each frame, if it is dynamic and placed anew as it moves.
    fn submit(&mut self, world: &mut World, root: RootId, mobility: Mobility) {
        let item = match self.item.filter(|item| world.contains(*item)) {
            Some(_) if mobility == Mobility::Static => return,
            Some(item) => item,
            None => *self
                .item
                .insert(world.add(Item::new(root, Layer::Opaque, mobility))),
        };
        world.place(item, self.batch.bounds());
    }

    /// Draw only what `views` says the eyes may see of it, as the world culled it, each piece lit by the lamps the world
    /// lists for it, if `listed`, or every lamp, or, without a world, against its frustum, lit by every lamp, and say how
    /// much it then draws.
    fn cull(
        &mut self,
        queue: &wgpu::Queue,
        models: &Renderer,
        views: &Views<'_>,
        listed: bool,
    ) -> Tally {
        match self.item.zip(views.world()).filter(|_| listed) {
            Some((item, (world, _))) => models.light(
                &mut self.batch,
                world
                    .lit(item)
                    .map(|lit| lit.iter().map(|lit| (lit.light, lit.weight))),
            ),
            None => models.light(&mut self.batch, std::iter::empty::<[(u32, f32); 0]>()),
        }
        match self.item.and_then(|item| views.seen(item)) {
            Some(seen) => models.show(queue, &self.model, &mut self.batch, seen),
            None => models.cull(queue, &self.model, &mut self.batch, views.frustum()),
        }
    }
}

/// A pipeline of the farmyard's own, drawing triangles of `T` from `buffers`' layout with `shader`'s `vertex` and
/// `fragment` entry points, in the eyes' pass, writing depth or not.
#[allow(clippy::too_many_arguments)]
fn pipeline(
    target: &Target,
    layout: &wgpu::PipelineLayout,
    shader: &wgpu::ShaderModule,
    (vertex, fragment): (&str, &str),
    buffers: &[Option<wgpu::VertexBufferLayout<'_>>],
    (write, compare): (bool, wgpu::CompareFunction),
    label: &str,
) -> wgpu::RenderPipeline {
    blended(
        target,
        layout,
        shader,
        (vertex, fragment),
        buffers,
        (write, compare),
        None,
        label,
    )
}

/// As [`pipeline`], blending what it draws into what is behind it as `blend` says.
#[allow(clippy::too_many_arguments)]
fn blended(
    target: &Target,
    layout: &wgpu::PipelineLayout,
    shader: &wgpu::ShaderModule,
    (vertex, fragment): (&str, &str),
    buffers: &[Option<wgpu::VertexBufferLayout<'_>>],
    (write, compare): (bool, wgpu::CompareFunction),
    blend: Option<wgpu::BlendState>,
    label: &str,
) -> wgpu::RenderPipeline {
    target
        .device
        .create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some(label),
            layout: Some(layout),
            vertex: wgpu::VertexState {
                module: shader,
                entry_point: Some(vertex),
                compilation_options: Default::default(),
                buffers,
            },
            fragment: Some(wgpu::FragmentState {
                module: shader,
                entry_point: Some(fragment),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: target.format,
                    blend,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: Default::default(),
            depth_stencil: Some(wgpu::DepthStencilState {
                format: wgpu::TextureFormat::Depth32Float,
                depth_write_enabled: Some(write),
                depth_compare: Some(compare),
                stencil: Default::default(),
                bias: Default::default(),
            }),
            multisample: wgpu::MultisampleState {
                count: target.samples,
                ..Default::default()
            },
            multiview_mask: target.views.mask(),
            cache: None,
        })
}

/// Triangles of the farmyard's own, in a buffer, drawn with their pipeline.
struct Triangles {
    pipeline: wgpu::RenderPipeline,
    vertices: wgpu::Buffer,
    count: u32,
}

impl Triangles {
    fn new<T: Pod>(device: &wgpu::Device, pipeline: wgpu::RenderPipeline, vertices: &[T]) -> Self {
        Self {
            pipeline,
            vertices: device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("Farmyard backdrop"),
                contents: bytemuck::cast_slice(vertices),
                usage: wgpu::BufferUsages::VERTEX,
            }),
            count: vertices.len() as u32,
        }
    }

    fn draw(&self, pass: &mut wgpu::RenderPass<'_>) {
        pass.set_pipeline(&self.pipeline);
        pass.set_vertex_buffer(0, self.vertices.slice(..));
        pass.draw(0..self.count, 0..1);
    }
}

/// The bonfire's flames and embers, which never change, then the little flames of the marshmallows that burn, which
/// follow them, in one buffer, drawn in one draw, glowing.
struct Fire {
    pipeline: wgpu::RenderPipeline,
    vertices: wgpu::Buffer,
    lasting: u32,
    count: u32,
}

impl Fire {
    fn new(target: &Target, layout: &wgpu::PipelineLayout) -> Self {
        let device = target.device;
        let shader = tomori_render::multiview::shader(
            device,
            "fire.wgsl",
            include_str!("fire.wgsl"),
            target.views,
        );
        let pipeline = blended(
            target,
            layout,
            &shader,
            ("fire_vertex", "fire_fragment"),
            &[Some(wgpu::VertexBufferLayout {
                array_stride: size_of::<FireVertex>() as u64,
                step_mode: wgpu::VertexStepMode::Vertex,
                attributes: &wgpu::vertex_attr_array![0 => Float32x3, 1 => Float32x2, 2 => Float32x4, 3 => Float32x4],
            })],
            (false, wgpu::CompareFunction::Less),
            Some(wgpu::BlendState {
                color: wgpu::BlendComponent {
                    src_factor: wgpu::BlendFactor::One,
                    dst_factor: wgpu::BlendFactor::One,
                    operation: wgpu::BlendOperation::Add,
                },
                alpha: wgpu::BlendComponent::OVER,
            }),
            "Farmyard fire",
        );
        let lasting = bonfire();
        let vertices = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("Farmyard fire"),
            size: (size_of::<FireVertex>() * (lasting.len() + campsite::STICKS * 9)) as u64,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        target
            .queue
            .write_buffer(&vertices, 0, bytemuck::cast_slice(&lasting));
        Self {
            pipeline,
            vertices,
            lasting: lasting.len() as u32,
            count: lasting.len() as u32,
        }
    }

    /// Light the marshmallows `burning`, each stick's index and where its marshmallow is, and only those, after the
    /// bonfire's flames.
    fn burn(&mut self, queue: &wgpu::Queue, burning: &[(usize, Vec3)]) {
        let flames: Vec<FireVertex> = burning
            .iter()
            .take(campsite::STICKS)
            .enumerate()
            .flat_map(|(index, (_, at))| alight(*at, index as f32 * 1.7))
            .collect();
        if !flames.is_empty() {
            queue.write_buffer(
                &self.vertices,
                (size_of::<FireVertex>() * self.lasting as usize) as u64,
                bytemuck::cast_slice(&flames),
            );
        }
        self.count = self.lasting + flames.len() as u32;
    }

    fn draw(&self, pass: &mut wgpu::RenderPass<'_>) {
        pass.set_pipeline(&self.pipeline);
        pass.set_vertex_buffer(0, self.vertices.slice(..));
        pass.draw(0..self.count, 0..1);
    }
}

/// The trails burning marshmallows paint in the air ([`Embers`]): every one in one ring, in one buffer, written as they
/// are laid, drawn in one draw, or two as the ring wraps, glowing.
struct Trails {
    pipeline: wgpu::RenderPipeline,
    vertices: wgpu::Buffer,
    embers: Embers,
}

impl Trails {
    fn new(target: &Target, layout: &wgpu::PipelineLayout) -> Self {
        let device = target.device;
        let shader = tomori_render::multiview::shader(
            device,
            "embers.wgsl",
            include_str!("embers.wgsl"),
            target.views,
        );
        let pipeline = blended(
            target,
            layout,
            &shader,
            ("ember_vertex", "ember_fragment"),
            &[Some(wgpu::VertexBufferLayout {
                array_stride: size_of::<EmberVertex>() as u64,
                step_mode: wgpu::VertexStepMode::Vertex,
                attributes: &wgpu::vertex_attr_array![0 => Float32x3, 1 => Float32x3, 2 => Float32x4, 3 => Float32x4],
            })],
            (false, wgpu::CompareFunction::Less),
            Some(wgpu::BlendState {
                color: wgpu::BlendComponent {
                    src_factor: wgpu::BlendFactor::One,
                    dst_factor: wgpu::BlendFactor::One,
                    operation: wgpu::BlendOperation::Add,
                },
                alpha: wgpu::BlendComponent::OVER,
            }),
            "Farmyard embers",
        );
        let vertices = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("Farmyard embers"),
            size: (size_of::<EmberVertex>() * embers::QUADS * 6) as u64,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        Self {
            pipeline,
            vertices,
            embers: Embers::new(),
        }
    }

    /// Take what the trails laid since last to the GPU.
    fn upload(&mut self, queue: &wgpu::Queue) {
        for range in self.embers.written() {
            if !range.is_empty() {
                queue.write_buffer(
                    &self.vertices,
                    (size_of::<EmberVertex>() * range.start) as u64,
                    bytemuck::cast_slice(&self.embers.vertices()[range]),
                );
            }
        }
    }

    fn draw(&self, pass: &mut wgpu::RenderPass<'_>) {
        let drawn = self.embers.drawn();
        if drawn.iter().all(|range| range.is_empty()) {
            return;
        }
        pass.set_pipeline(&self.pipeline);
        pass.set_vertex_buffer(0, self.vertices.slice(..));
        for range in drawn.into_iter().filter(|range| !range.is_empty()) {
            pass.draw(range, 0..1);
        }
    }
}

/// Each fern mesh, by kind and detail, and its range of the ferns' vertices.
pub(crate) type FernMeshes = Vec<((ferns::Kind, Detail), Range<u32>)>;

/// The draws of the ferns' meshes: each mesh's range of vertices, and its range of instances.
pub(crate) type FernDraws = Vec<(Range<u32>, Range<u32>)>;

/// The ferns: every kind's mesh in each detail, in one buffer, and their instances in another, grouped by the mesh each
/// is drawn with, as the eyes were when last placed, of those the eyes may see; one draw for each mesh with instances,
/// all with one pipeline, swaying in the breeze.
struct Ferns {
    pipeline: wgpu::RenderPipeline,
    vertices: wgpu::Buffer,
    instances: wgpu::Buffer,
    /// Each fern's lamps, as its instance holds them, as last culled and as a cull lights them, kept so that lighting
    /// allocates nothing; and how many times they have changed.
    lamps: Vec<[u32; 4]>,
    next_lamps: Vec<[u32; 4]>,
    relit: u64,
    /// Each mesh, and its range of the vertices.
    meshes: FernMeshes,
    ferns: Vec<Fern>,
    /// Where each fern's heart is, found once, so that placing them each frame only measures how far each is.
    hearts: Vec<Vec3>,
    /// Where each fern may be drawn, however it sways, whether the eyes may see it this frame, and the item they are in
    /// the render world, once submitted.
    bounds: Vec<Aabb>,
    seen: Vec<bool>,
    item: Option<ItemId>,
    /// How each fern was drawn when last placed, if it was, each mesh's range of vertices with its range of instances,
    /// and how each would be drawn now, kept so that placing them allocates nothing unless that changes, and how many
    /// times their lamps had changed then.
    placed: Mutex<(Details, FernDraws, Details, u64)>,
}

/// How each fern is drawn, in order: in which detail, or not at all.
type Details = Vec<Option<Detail>>;

/// Where `fern` may be drawn, in either detail, however it sways: about its foot, as far out as any point of its
/// kind's meshes reaches whichever way it is turned, as high and as low, and as far again as the breeze sways its
/// points, its swell and its flutter together at their most, 1.3 times the most a point sways.
pub(crate) fn fern_bounds(fern: &Fern) -> Aabb {
    let points: Vec<ferns::Point> = fern
        .kind
        .details()
        .iter()
        .flat_map(|detail| ferns::mesh(fern.kind, *detail))
        .collect();
    let out = points
        .iter()
        .map(|point| point.position.x.hypot(point.position.z))
        .fold(0.0, f32::max);
    let (low, high) = points.iter().fold((0.0f32, 0.0f32), |(low, high), point| {
        (low.min(point.position.y), high.max(point.position.y))
    });
    let sway = points.iter().map(|point| point.sway).fold(0.0, f32::max);
    Aabb::new(Vec3::new(-out, low, -out), Vec3::new(out, high, out))
        .transformed(fern.transform())
        .grown(1.3 * sway * fern.scale)
}

impl Ferns {
    fn new(target: &Target, layout: &wgpu::PipelineLayout) -> Self {
        let device = target.device;
        let shader = tomori_render::multiview::shader(
            device,
            "fern.wgsl",
            include_str!("fern.wgsl"),
            target.views,
        );
        let pipeline = pipeline(
            target,
            layout,
            &shader,
            ("fern_vertex", "fern_fragment"),
            &[
                Some(wgpu::VertexBufferLayout {
                    array_stride: size_of::<FernVertex>() as u64,
                    step_mode: wgpu::VertexStepMode::Vertex,
                    attributes: &wgpu::vertex_attr_array![0 => Float32x3, 1 => Float32x3, 2 => Float32x3, 3 => Float32x3, 4 => Float32],
                }),
                Some(wgpu::VertexBufferLayout {
                    array_stride: size_of::<FernInstance>() as u64,
                    step_mode: wgpu::VertexStepMode::Instance,
                    attributes: &wgpu::vertex_attr_array![5 => Float32x4, 6 => Float32x4, 7 => Uint32x4],
                }),
            ],
            (true, wgpu::CompareFunction::Less),
            "Farmyard ferns",
        );
        let (vertices, meshes) = fern_vertices();
        let ferns = ferns::all();
        let instances = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("Farmyard fern instances"),
            size: (size_of::<FernInstance>() * ferns.len().max(1)) as u64,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        Self {
            pipeline,
            lamps: vec![EVERY; ferns.len()],
            next_lamps: Vec::with_capacity(ferns.len()),
            relit: 0,
            vertices: device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("Farmyard fern meshes"),
                contents: bytemuck::cast_slice(&vertices),
                usage: wgpu::BufferUsages::VERTEX,
            }),
            instances,
            meshes,
            hearts: ferns.iter().map(Fern::heart).collect(),
            bounds: ferns.iter().map(fern_bounds).collect(),
            seen: vec![true; ferns.len()],
            item: None,
            ferns,
            placed: Mutex::new((Vec::new(), Vec::new(), Vec::new(), 0)),
        }
    }

    /// Add them to `world`, in `root`, once, each an instance, where it may be drawn however it sways.
    fn submit(&mut self, world: &mut World, root: RootId) {
        if self.item.is_some_and(|item| world.contains(item)) {
            return;
        }
        let item = world.add(Item::new(root, Layer::Opaque, Mobility::Static));
        world.place(item, self.bounds.iter().copied().map(Some));
        self.item = Some(item);
    }

    /// Draw only the ferns `views` says the eyes may see, as the world culled them, each lit by the lamps the world lists
    /// for it, if `listed`, or every lamp, or, without a world, against its frustum, lit by every lamp, from the next
    /// [`Self::place`], and say how many it culled.
    fn cull(&mut self, views: &Views<'_>, listed: bool) -> Tally {
        match self.item.and_then(|item| views.seen(item)) {
            Some(seen) => self.seen.copy_from_slice(seen),
            None => {
                for (seen, bounds) in self.seen.iter_mut().zip(&self.bounds) {
                    *seen = views.sees(bounds);
                }
            }
        }
        self.next_lamps.clear();
        match self.item.zip(views.world()).filter(|_| listed) {
            Some((item, (world, _))) if world.instances(item) == self.ferns.len() => {
                self.next_lamps.extend(world.lit(item).map(|lit| {
                    tomori_assets::gpu::lamps(lit.iter().map(|lit| (lit.light, lit.weight)))
                }));
            }
            _ => self.next_lamps.resize(self.ferns.len(), EVERY),
        }
        if self.next_lamps != self.lamps {
            std::mem::swap(&mut self.lamps, &mut self.next_lamps);
            self.relit += 1;
        }
        let drawn = self.seen.iter().filter(|seen| **seen).count();
        Tally {
            drawn,
            culled: self.ferns.len() - drawn,
            ..Tally::default()
        }
    }

    /// What the ferns draw in each eye as last placed: their draws and triangles.
    fn tally(&self) -> Tally {
        let placed = self
            .placed
            .lock()
            .expect("the ferns' placing is never poisoned");
        Tally {
            draws: placed.1.len(),
            triangles: placed
                .1
                .iter()
                .map(|(vertices, instances)| vertices.len() / 3 * instances.len())
                .sum(),
            ..Tally::default()
        }
    }

    /// Group the ferns the eyes may see by the mesh each is drawn with for eyes at `eyes`, writing their instances only
    /// when that changes.
    fn place(&self, queue: &wgpu::Queue, eyes: Vec3) {
        let mut placed = self
            .placed
            .lock()
            .expect("the ferns' placing is never poisoned");
        let (last, draws, details, relit) = &mut *placed;
        details.clear();
        details.extend(
            self.hearts
                .iter()
                .zip(&self.seen)
                .map(|(heart, seen)| seen.then(|| Detail::seen(*heart, eyes))),
        );
        if last == details && !draws.is_empty() && *relit == self.relit {
            return;
        }
        let (instances, placed) = fern_instances(&self.ferns, details, &self.lamps, &self.meshes);
        queue.write_buffer(&self.instances, 0, bytemuck::cast_slice(&instances));
        *draws = placed;
        *relit = self.relit;
        std::mem::swap(last, details);
    }

    fn draw(&self, pass: &mut wgpu::RenderPass<'_>) {
        let placed = self
            .placed
            .lock()
            .expect("the ferns' placing is never poisoned");
        pass.set_pipeline(&self.pipeline);
        pass.set_vertex_buffer(0, self.vertices.slice(..));
        pass.set_vertex_buffer(1, self.instances.slice(..));
        for (vertices, instances) in &placed.1 {
            pass.draw(vertices.clone(), instances.clone());
        }
    }
}

/// Every fern mesh's points, one after another, as their shader reads them, and each mesh's range of them.
pub(crate) fn fern_vertices() -> (Vec<FernVertex>, FernMeshes) {
    let mut vertices = Vec::new();
    let meshes = ferns::drawn()
        .into_iter()
        .map(|(kind, detail)| {
            let start = vertices.len() as u32;
            vertices.extend(
                ferns::mesh(kind, detail)
                    .into_iter()
                    .map(|point| FernVertex {
                        position: point.position.to_array(),
                        normal: point.normal.to_array(),
                        color: point.color.to_array(),
                        underside: point.underside.to_array(),
                        sway: point.sway,
                    }),
            );
            ((kind, detail), start..vertices.len() as u32)
        })
        .collect();
    (vertices, meshes)
}

/// `ferns`' instances, each drawn in its detail of `details`, or not at all for none, lit by its lamps of `lamps`,
/// grouped by mesh in `meshes`' order, and each mesh with instances, its range of vertices with its range of
/// instances.
pub(crate) fn fern_instances(
    ferns: &[Fern],
    details: &[Option<Detail>],
    lamps: &[[u32; 4]],
    meshes: &[((ferns::Kind, Detail), Range<u32>)],
) -> (Vec<FernInstance>, FernDraws) {
    let mut instances = Vec::with_capacity(ferns.len());
    let mut draws = Vec::new();
    for ((kind, detail), vertices) in meshes {
        let start = instances.len() as u32;
        instances.extend(
            ferns
                .iter()
                .zip(details)
                .zip(lamps)
                .filter(|((fern, drawn), _)| fern.kind == *kind && **drawn == Some(*detail))
                .map(|((fern, _), lamps)| FernInstance {
                    lamps: *lamps,
                    ..FernInstance::from(fern)
                }),
        );
        if instances.len() as u32 > start {
            draws.push((vertices.clone(), start..instances.len() as u32));
        }
    }
    (instances, draws)
}

/// The backdrop beyond the fence: its foothills, each kind of plant with all its instances, the river's stones, with the
/// upper terrace's, the waterfall rock, the trail, the upper terrace, the water, above the waterfall too, and the
/// ranges.
struct Backdrop {
    foothills: Drawn,
    plants: Vec<Drawn>,
    stones: Drawn,
    rock: Drawn,
    trail: Drawn,
    terrace: Drawn,
    water: Triangles,
    ranges: Triangles,
}

impl Backdrop {
    fn new(
        target: &Target,
        models: &Renderer,
        layout: &wgpu::PipelineLayout,
        sky: &wgpu::ShaderModule,
        chunked: &dyn Fn(&Model) -> Arc<Model>,
    ) -> Self {
        let (device, queue) = (target.device, target.queue);
        let foothills = Drawn::new(
            device,
            queue,
            models,
            &chunked(&backdrop::model()),
            Mat4::IDENTITY,
        );
        let placed = backdrop::plants();
        // Every kind but the tree ferns, which the ferns draw.
        let plants = Kind::ALL
            .iter()
            .filter_map(|kind| {
                let placements: Vec<Mat4> = placed
                    .iter()
                    .filter(|plant| plant.kind == *kind)
                    .map(|plant| plant.transform())
                    .collect();
                let model = Arc::new(kind.model()?);
                Some(Drawn::placed(device, queue, models, &model, &placements))
            })
            .collect();
        let stones: Vec<Mat4> = backdrop::stones()
            .iter()
            .chain(&terrace::stones())
            .map(backdrop::Stone::transform)
            .collect();
        let stones = Drawn::placed(device, queue, models, &Arc::new(backdrop::stone()), &stones);
        let rock = Drawn::new(
            device,
            queue,
            models,
            &chunked(&backdrop::rock()),
            Mat4::IDENTITY,
        );
        let trail = Drawn::new(
            device,
            queue,
            models,
            &chunked(&trail::model()),
            Mat4::IDENTITY,
        );
        let terrace = Drawn::new(
            device,
            queue,
            models,
            &chunked(&terrace::model()),
            Mat4::IDENTITY,
        );
        let shader = tomori_render::multiview::shader(
            device,
            "water.wgsl",
            include_str!("water.wgsl"),
            target.views,
        );
        let water: Vec<WaterVertex> = backdrop::water()
            .into_iter()
            .chain(backdrop::upper_water())
            .map(|point| WaterVertex {
                position: point.position.to_array(),
                color: point.color.extend(point.kind).to_array(),
                flow: point.flow.to_array(),
            })
            .collect();
        let water = Triangles::new(
            device,
            pipeline(
                target,
                layout,
                &shader,
                ("water_vertex", "water_fragment"),
                &[Some(wgpu::VertexBufferLayout {
                    array_stride: size_of::<WaterVertex>() as u64,
                    step_mode: wgpu::VertexStepMode::Vertex,
                    attributes: &wgpu::vertex_attr_array![0 => Float32x3, 1 => Float32x4, 2 => Float32x2],
                })],
                (true, wgpu::CompareFunction::Less),
                "Farmyard water",
            ),
            &water,
        );
        let ranges: Vec<FarVertex> = backdrop::ranges()
            .into_iter()
            .map(|far| FarVertex {
                direction: far.direction.to_array(),
                color: far.color.extend(far.fade).to_array(),
            })
            .collect();
        // At the sky's depth, where nothing nearer has been drawn, each range over the one beyond it.
        let ranges = Triangles::new(
            device,
            pipeline(
                target,
                layout,
                sky,
                ("range_vertex", "range_fragment"),
                &[Some(wgpu::VertexBufferLayout {
                    array_stride: size_of::<FarVertex>() as u64,
                    step_mode: wgpu::VertexStepMode::Vertex,
                    attributes: &wgpu::vertex_attr_array![0 => Float32x3, 1 => Float32x4],
                })],
                (false, wgpu::CompareFunction::LessEqual),
                "Farmyard ranges",
            ),
            &ranges,
        );
        Self {
            foothills,
            plants,
            stones,
            rock,
            trail,
            terrace,
            water,
            ranges,
        }
    }
}

/// The colour of a plaque's text, pale as paint on dark wood.
const LETTERING: [f32; 4] = [0.82, 0.76, 0.6, 1.0];

/// How many cells wide a plaque's lines are, and how large a font pixel is on it, so that its text fills its board.
const PLAQUE_COLUMNS: usize = 18;
pub(crate) const PLAQUE_PIXEL: f32 = 0.0028;

/// What a station's plaque shows: its name, then its exercise, each line centred, on a clear backing, the board drawn
/// beneath it.
pub(crate) fn plaque(lines: &[&str]) -> Canvas {
    let mut canvas = Canvas::new(PLAQUE_COLUMNS, PLAQUE_PIXEL);
    canvas.background = [0.0; 4];
    canvas.padding = (2, 2);
    for (index, line) in lines.iter().filter(|line| !line.is_empty()).enumerate() {
        let style = if index == 0 {
            Style::bold(LETTERING)
        } else {
            Style::plain(LETTERING)
        };
        let centred = format!("{line:^PLAQUE_COLUMNS$}");
        canvas.rows.push(Row::text(&centred, style, PLAQUE_COLUMNS));
    }
    canvas
}

/// The stations' plaques, each a panel of its own on its board, drawn from the renderer's glyph atlas.
struct Plaques {
    glyphs: tomori_render::glyphs::SharedGlyphs,
    pipeline: Pipeline,
    plaques: Vec<(Panel, Arc<Canvas>, Mat4)>,
    /// Whether the eyes may see each, this frame, and the item they are in the render world, once submitted.
    seen: Vec<bool>,
    item: Option<ItemId>,
}

impl Plaques {
    fn new(target: &Target) -> Self {
        let pipeline = Pipeline::for_target(target);
        let plaques: Vec<_> = trail::plaques()
            .into_iter()
            .map(|plaque| {
                (
                    Panel::new(target.device, &pipeline, 600),
                    Arc::new(self::plaque(&plaque.lines)),
                    plaque.transform,
                )
            })
            .collect();
        Self {
            glyphs: target.glyphs.clone(),
            pipeline,
            seen: vec![true; plaques.len()],
            item: None,
            plaques,
        }
    }

    /// Add them to `world`, in `root`, if they are not there, translucent, and place each where its text is now, each
    /// frame, since its mesh grows as the glyphs it waits for are drawn.
    fn submit(&mut self, world: &mut World, root: RootId) {
        let item = match self.item.filter(|item| world.contains(*item)) {
            Some(item) => item,
            None => {
                *self
                    .item
                    .insert(world.add(Item::new(root, Layer::Translucent, Mobility::Dynamic)))
            }
        };
        world.place(
            item,
            self.plaques
                .iter()
                .map(|(panel, _, transform)| Some(panel.bounds().transformed(*transform))),
        );
    }

    /// Draw only the plaques `views` says the eyes may see, as the world culled them, or, without a world, against its
    /// frustum, and say how much they then draw.
    fn cull(&mut self, views: &Views<'_>) -> Tally {
        match self.item.and_then(|item| views.seen(item)) {
            Some(seen) => self.seen.copy_from_slice(seen),
            None => {
                for (seen, (panel, _, transform)) in self.seen.iter_mut().zip(&self.plaques) {
                    *seen = views.sees(&panel.bounds().transformed(*transform));
                }
            }
        }
        let mut tally = Tally::default();
        for (seen, (panel, _, _)) in self.seen.iter().zip(&self.plaques) {
            tally += if *seen {
                Tally {
                    drawn: 1,
                    draws: 1,
                    triangles: panel.triangles(),
                    ..Tally::default()
                }
            } else {
                Tally {
                    culled: 1,
                    ..Tally::default()
                }
            };
        }
        tally
    }

    /// Show each plaque's text, the very canvas it showed, so that it is built again only while glyphs it needs wait
    /// for the atlas.
    fn set(&mut self, queue: &wgpu::Queue) {
        for (panel, canvas, _) in &mut self.plaques {
            panel.show(queue, canvas);
        }
    }

    /// Each plaque as `eye` sees it, through the farmyard's `haze`.
    fn prepare(
        &self,
        queue: &wgpu::Queue,
        eye: usize,
        scene_to_clip: Mat4,
        haze: tomori_assets::haze::Haze,
    ) {
        let seen = tomori_render::ui_panel::Seen::new(scene_to_clip, haze);
        for (panel, _, transform) in &self.plaques {
            panel.prepare_seen(queue, eye, &seen, *transform);
        }
    }

    /// Draw those the reveal sphere, if the scenery is growing, has reached.
    fn draw(&self, pass: &mut wgpu::RenderPass<'_>, eye: usize, reveal: Option<(Vec3, f32)>) {
        let mut shown = self
            .plaques
            .iter()
            .zip(&self.seen)
            .filter(|((_, _, transform), seen)| {
                **seen
                    && reveal.is_none_or(|(centre, radius)| {
                        transform.transform_point3(Vec3::ZERO).distance(centre) < radius
                    })
            })
            .map(|((panel, _, _), _)| panel)
            .peekable();
        if shown.peek().is_none() {
            return;
        }
        let binding = self.glyphs.binding();
        self.pipeline.set(pass, &binding);
        for panel in shown {
            panel.draw(pass, &self.pipeline, eye);
        }
    }
}

/// The farmyard's scenery.
pub struct Farmyard {
    sky: wgpu::RenderPipeline,
    /// Each eye's [`SkyCamera`], the ferns' lighting, one for both, and their binding for each pass.
    cameras: Uniforms,
    fern_lighting: wgpu::Buffer,
    bindings: Vec<wgpu::BindGroup>,
    /// How many views each pass it draws in draws, as its target said.
    views: tomori_render::multiview::Views,
    models: Renderer,
    yard: Drawn,
    coop: Drawn,
    backdrop: Backdrop,
    plaques: Plaques,
    /// The haze's colour and density, which the water is hazed by as the models are.
    haze: [f32; 4],
    clock: Clock,
    made: Instant,
    /// The chickens, uploaded once their client side first shows them, placed as it last did.
    chickens: Option<Drawn>,
    /// The campsite, its sticks and logs where its client side last showed them, its bonfire's flames, and how big its
    /// fire is drawn.
    campsite: Drawn,
    sticks: Drawn,
    logs: Drawn,
    fire: Fire,
    trails: Trails,
    fire_size: f32,
    ferns: Ferns,
    /// The beach's shingle, and its stones where its client side last showed them.
    beach: Drawn,
    stones: Drawn,
    reveal: Option<(Vec3, f32)>,
    /// The lamps it submitted to the render world: the lanterns, then the bonfire's, which flickers.
    lamps: Vec<LightId>,
    /// The lamps the world lit it by when last culled, if it was culled in a world, which its models and ferns are
    /// lit by, each by those listed for it, if `listed`, or by every one.
    lit: Option<Vec<Lamp>>,
    listed: bool,
}

impl Farmyard {
    /// The farmyard for `target`, with `coop`, whose asset, if it has one, comes from `library`, its water moving with
    /// the time.
    pub fn new(target: &Target, coop: &Coop, library: &Library) -> Self {
        Self::with_clock(target, coop, library, Clock::Running)
    }

    /// As [`Self::new`], its water moved by `clock`.
    pub fn with_clock(target: &Target, coop: &Coop, library: &Library, clock: Clock) -> Self {
        Self::with_chunks(target, coop, library, clock, Some(CHUNK))
    }

    /// As [`Self::with_clock`], its models split into chunks at most `chunk` across the ground, or kept whole, `None`, to
    /// compare.
    pub fn with_chunks(
        target: &Target,
        coop: &Coop,
        library: &Library,
        clock: Clock,
        chunk: Option<f32>,
    ) -> Self {
        let chunked = |model: &Model| {
            Arc::new(chunk.map_or_else(|| model.clone(), |size| model.chunked(size)))
        };
        let device = target.device;
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("Farmyard sky layout"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: Uniforms::binding_size(
                            size_of::<SkyCamera>() as u64,
                            target.views,
                        ),
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: wgpu::BufferSize::new(size_of::<FernLighting>() as u64),
                    },
                    count: None,
                },
            ],
        });
        let cameras = Uniforms::new(
            device,
            "Farmyard sky camera",
            size_of::<SkyCamera>() as u64,
            target.views,
        );
        let fern_lighting = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("Farmyard fern lighting"),
            size: size_of::<FernLighting>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let bindings = cameras
            .buffers()
            .iter()
            .map(|buffer| {
                device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some("Farmyard sky camera"),
                    layout: &layout,
                    entries: &[
                        wgpu::BindGroupEntry {
                            binding: 0,
                            resource: buffer.as_entire_binding(),
                        },
                        wgpu::BindGroupEntry {
                            binding: 1,
                            resource: fern_lighting.as_entire_binding(),
                        },
                    ],
                })
            })
            .collect();
        let shader = tomori_render::multiview::shader(
            device,
            "sky.wgsl",
            include_str!("sky.wgsl"),
            target.views,
        );
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("Farmyard sky"),
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
        let sky = pipeline(
            target,
            &pipeline_layout,
            &shader,
            ("sky_vertex", "sky_fragment"),
            &[],
            (false, wgpu::CompareFunction::LessEqual),
            "Farmyard sky",
        );
        let models = Renderer::with_views(
            device,
            target.queue,
            target.format,
            target.samples,
            target.views,
        );
        let lighting = lighting();
        models.set_lighting(target.queue, &lighting);
        let yard = Drawn::new(
            device,
            target.queue,
            &models,
            &chunked(&props::model()),
            Mat4::IDENTITY,
        );
        let coop = Drawn::new(
            device,
            target.queue,
            &models,
            &chunked(&coop.model(library)),
            coop::placement(),
        );
        let backdrop = Backdrop::new(target, &models, &pipeline_layout, &shader, &chunked);
        let plaques = Plaques::new(target);
        let campsite = Drawn::new(
            device,
            target.queue,
            &models,
            &chunked(&campsite::model()),
            Mat4::IDENTITY,
        );
        let sticks = Drawn::placed(device, target.queue, &models, &stick(), &[]);
        let logs = Drawn::placed(device, target.queue, &models, &log(), &[]);
        let shingle = Drawn::new(
            device,
            target.queue,
            &models,
            &chunked(&beach::model()),
            Mat4::IDENTITY,
        );
        let stones = Drawn::placed(device, target.queue, &models, &stones(), &[]);
        let fire = Fire::new(target, &pipeline_layout);
        let trails = Trails::new(target, &pipeline_layout);
        let ferns = Ferns::new(target, &pipeline_layout);
        Self {
            sky,
            cameras,
            fern_lighting,
            bindings,
            views: target.views,
            models,
            yard,
            coop,
            backdrop,
            plaques,
            haze: tomori_assets::haze::Haze::from(&lighting).to_array(),
            clock,
            made: Instant::now(),
            chickens: None,
            campsite,
            sticks,
            logs,
            fire,
            trails,
            fire_size: 0.0,
            ferns,
            beach: shingle,
            stones,
            reveal: None,
            lamps: Vec::new(),
            lit: None,
            listed: true,
        }
    }

    /// The trails burning marshmallows paint in the air, to draw one as if by hand, for pictures and measures; what is
    /// painted is drawn from the next [`Scenery::animate`].
    pub fn embers_mut(&mut self) -> &mut Embers {
        &mut self.trails.embers
    }

    /// Draw closed models' parts without their back faces, as by default, or with them, `false`, to compare.
    pub fn set_back_faces_culled(&mut self, culled: bool) {
        self.models.set_back_faces_culled(culled);
    }

    /// Light each model's pieces and each fern by the lamps the render world lists for it, as by default, or by every
    /// lamp, `false`, to compare, from the next cull.
    pub fn set_lamps_listed(&mut self, listed: bool) {
        self.listed = listed;
    }
}

/// The beach's stones' model, built once.
fn stones() -> Arc<Model> {
    static STONES: std::sync::OnceLock<Arc<Model>> = std::sync::OnceLock::new();
    STONES
        .get_or_init(|| Arc::new(beach::stones_model()))
        .clone()
}

/// The stones as `shown`, each an instance of the stones' model showing its own.
pub(crate) fn stone_instances(shown: &[crate::stones::Shown]) -> Vec<Instance> {
    let model = stones();
    shown
        .iter()
        .map(|stone| {
            let mut instance = Instance::new(model.clone());
            instance.transform = stone.transform;
            for index in 0..beach::STONES {
                instance.set_shown(&beach::stone_node(index), index == stone.stone);
            }
            instance
        })
        .collect()
}

/// The stick's model, built once.
fn stick() -> Arc<Model> {
    static STICK: std::sync::OnceLock<Arc<Model>> = std::sync::OnceLock::new();
    STICK.get_or_init(|| Arc::new(campsite::stick())).clone()
}

/// The log's model, built once.
fn log() -> Arc<Model> {
    static LOG: std::sync::OnceLock<Arc<Model>> = std::sync::OnceLock::new();
    LOG.get_or_init(|| Arc::new(campsite::log_model())).clone()
}

/// The logs as `shown`, each an instance of the log's model showing it in its shade.
pub(crate) fn log_instances(shown: &[crate::campsite::ShownLog]) -> Vec<Instance> {
    let model = log();
    shown
        .iter()
        .map(|log| {
            let mut instance = Instance::new(model.clone());
            instance.transform = log.transform;
            for shade in 0..campsite::LOG_SHADES {
                instance.set_shown(&campsite::log_node(shade), shade == log.shade);
            }
            instance
        })
        .collect()
}

/// The sticks as `shown`, each an instance of the stick's model showing its marshmallow in its shade.
pub(crate) fn stick_instances(shown: &[crate::campsite::Shown]) -> Vec<Instance> {
    let model = stick();
    shown
        .iter()
        .map(|stick| {
            let mut instance = Instance::new(model.clone());
            instance.transform = stick.transform;
            for shade in 0..campsite::SHADES {
                instance.set_shown(&campsite::shade_node(shade), shade == stick.shade);
            }
            instance
        })
        .collect()
}

impl Scenery for Farmyard {
    fn views(&self) -> tomori_render::multiview::Views {
        self.views
    }

    fn haze(&self) -> tomori_assets::haze::Haze {
        let [red, green, blue, density] = self.haze;
        tomori_assets::haze::Haze::new(Vec3::new(red, green, blue), density)
    }

    fn animate(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        drawn: &(dyn std::any::Any + Send + Sync),
    ) {
        self.plaques.set(queue);
        let Some(drawn) = drawn.downcast_ref::<crate::client::Drawn>() else {
            return;
        };
        // The sticks, where the client side shows them, and the little flames of those that burn.
        self.models.place(
            device,
            queue,
            &self.sticks.model,
            &mut self.sticks.batch,
            &stick_instances(&drawn.sticks),
        );
        // The logs, where the client side shows them, and the fire, as big as it shows it.
        self.models.place(
            device,
            queue,
            &self.logs.model,
            &mut self.logs.batch,
            &log_instances(&drawn.logs),
        );
        self.fire_size = campsite::unit(drawn.fire);
        // The stones, where the client side shows them.
        self.models.place(
            device,
            queue,
            &self.stones.model,
            &mut self.stones.batch,
            &stone_instances(&drawn.stones),
        );
        let burning: Vec<(usize, Vec3)> = drawn
            .sticks
            .iter()
            .filter(|stick| stick.burning)
            .map(|stick| (stick.index, stick.marshmallow))
            .collect();
        self.fire.burn(queue, &burning);
        // The trails their marshmallows paint in the air, by the clock the shader ages them by.
        self.trails
            .embers
            .update(self.clock.elapsed(self.made), &burning);
        self.trails.upload(queue);
        let Some(first) = drawn.chickens.first() else {
            return;
        };
        let models = &self.models;
        let chickens = self.chickens.get_or_insert_with(|| Drawn {
            model: models.upload(device, queue, first.model()),
            batch: models.batch(device),
            item: None,
        });
        models.place(
            device,
            queue,
            &chickens.model,
            &mut chickens.batch,
            &drawn.chickens,
        );
    }

    fn submit(&mut self, world: &mut World, root: RootId) {
        let Self {
            yard,
            coop,
            backdrop,
            chickens,
            campsite,
            sticks,
            logs,
            beach,
            stones,
            ferns,
            plaques,
            lamps,
            ..
        } = self;
        // What stays where it is, once, and what moves, each frame.
        for drawn in [yard, coop, campsite, beach]
            .into_iter()
            .chain([
                &mut backdrop.trail,
                &mut backdrop.terrace,
                &mut backdrop.foothills,
            ])
            .chain(&mut backdrop.plants)
            .chain([&mut backdrop.stones, &mut backdrop.rock])
        {
            drawn.submit(world, root, Mobility::Static);
        }
        for drawn in chickens.as_mut().into_iter().chain([sticks, logs, stones]) {
            drawn.submit(world, root, Mobility::Dynamic);
        }
        ferns.submit(world, root);
        plaques.submit(world, root);
        // The moonlight and the lanterns once, and the bonfire's light where it now flickers.
        if lamps
            .first()
            .is_none_or(|lamp| world.light_mut(*lamp).is_none())
        {
            let lit = lighting();
            world.set_environment(
                root,
                Some(EnvironmentLight {
                    sun_direction: lit.sun_direction,
                    sun: lit.sun,
                    sky: lit.sky,
                    ground: lit.ground,
                    haze: lit.haze,
                    haze_density: lit.haze_density,
                }),
            );
            lamps.clear();
            lamps.extend(lanterns().iter().map(|lamp| {
                world.add_light(Light {
                    mobility: Mobility::Static,
                    ..Light::new(root, lamp.position, lamp.reach, lamp.color)
                })
            }));
            lamps.push(world.add_light(Light::new(root, Vec3::ZERO, 0.0, Vec3::ZERO)));
        }
        let fire = fire_lamp(self.clock.seconds(self.made), self.fire_size);
        let light = world
            .light_mut(lamps[lanterns().len()])
            .expect("the bonfire's light is submitted with the lanterns'");
        (light.position, light.reach, light.color) = (fire.position, fire.reach, fire.color);
    }

    fn cull(&mut self, queue: &wgpu::Queue, views: &Views<'_>) -> Tally {
        let Self {
            models,
            yard,
            coop,
            backdrop,
            chickens,
            campsite,
            sticks,
            logs,
            beach,
            stones,
            ferns,
            plaques,
            lit,
            listed,
            ..
        } = self;
        // The lamps the world lights it by this frame, in its order, which each piece's and fern's lamps index.
        match views.world() {
            Some((world, root)) => {
                let lamps = lit.get_or_insert_with(Vec::new);
                lamps.clear();
                lamps.extend(
                    world
                        .root_lights(root)
                        .iter()
                        .map(|light| Lamp::new(light.position, light.reach, light.radiance())),
                );
            }
            None => *lit = None,
        }
        let listed = *listed;
        let mut tally = Tally::default();
        for drawn in [Some(yard), Some(coop), chickens.as_mut()]
            .into_iter()
            .flatten()
            .chain([
                sticks,
                logs,
                stones,
                campsite,
                beach,
                &mut backdrop.trail,
                &mut backdrop.terrace,
                &mut backdrop.foothills,
            ])
            .chain(&mut backdrop.plants)
            .chain([&mut backdrop.stones, &mut backdrop.rock])
        {
            tally += drawn.cull(queue, models, views, listed);
        }
        tally += ferns.cull(views, listed);
        // The ferns are placed for the first eye, as they are again when it is prepared.
        let first = views
            .clips()
            .first()
            .map_or(Vec3::ZERO, |clip| self::eye(*clip));
        ferns.place(queue, first);
        tally + ferns.tally() + plaques.cull(views)
    }

    fn prepare(&self, queue: &wgpu::Queue, eye: usize, scene_to_clip: Mat4) {
        // Lit, once a frame, as the first eye is prepared, by the lamps the world lit it by when last culled, the bonfire's
        // flickering as it was submitted, or, culled in no world, by its own, the bonfire's flickering now; and the ferns
        // are drawn in the detail its distance from them asks.
        let seconds = self.clock.seconds(self.made);
        let mut lit = lighting_at(seconds, self.fire_size);
        if let Some(lamps) = &self.lit {
            lit.lamps.clone_from(lamps);
        }
        if eye == 0 {
            self.models.set_lighting(queue, &lit);
            self.ferns.place(queue, self::eye(scene_to_clip));
            let lighting = FernLighting {
                moon: lit.sun_direction.normalize_or_zero().extend(0.0).to_array(),
                moonlight: lit.sun.extend(0.0).to_array(),
                sky: lit.sky.extend(0.0).to_array(),
                ground: lit.ground.extend(0.0).to_array(),
                lamps: std::array::from_fn(|index| {
                    lit.lamps.get(index / 2).map_or([0.0; 4], |lamp| {
                        if index % 2 == 0 {
                            lamp.position.extend(lamp.reach).to_array()
                        } else {
                            lamp.color.extend(0.0).to_array()
                        }
                    })
                }),
                counts: [
                    lit.lamps.len().min(tomori_assets::gpu::LAMPS) as u32,
                    0,
                    0,
                    0,
                ],
            };
            queue.write_buffer(&self.fern_lighting, 0, bytemuck::bytes_of(&lighting));
        }
        self.models
            .prepare(queue, eye, scene_to_clip, self::eye(scene_to_clip));
        self.plaques.prepare(
            queue,
            eye,
            scene_to_clip,
            tomori_assets::haze::Haze::from(&lit),
        );
        let inverse = scene_to_clip.inverse();
        let camera = SkyCamera {
            inverse_view_projection: if inverse.is_finite() {
                inverse
            } else {
                Mat4::IDENTITY
            }
            .to_cols_array(),
            reveal: match self.reveal {
                Some((centre, radius)) => centre.extend(radius.max(0.0)).to_array(),
                None => [0.0, 0.0, 0.0, -1.0],
            },
            view_projection: scene_to_clip.to_cols_array(),
            eye: self::eye(scene_to_clip).extend(1.0).to_array(),
            haze: self.haze,
            time: [
                seconds,
                campsite::flames(self.fire_size),
                embers(self.fire_size),
                embers::WRAP as f32,
            ],
        };
        self.cameras.write(queue, eye, bytemuck::bytes_of(&camera));
    }

    fn draw(&self, pass: &mut wgpu::RenderPass<'_>, eye: usize) {
        // Nearest first, so that what they hide is not shaded: the yard, the coop and the chickens, the trail, the ferns
        // about it, then the upper terrace, the foothills, the bush, the river's stones and the waterfall rock beyond,
        // the water, then the sky, only where nothing nearer has been drawn, and the ranges at its depth.
        let backdrop = &self.backdrop;
        for drawn in [Some(&self.yard), Some(&self.coop), self.chickens.as_ref()]
            .into_iter()
            .flatten()
            .chain([
                &self.sticks,
                &self.logs,
                &self.stones,
                &self.campsite,
                &self.beach,
                &backdrop.trail,
            ])
        {
            self.models.draw(pass, eye, &drawn.model, &drawn.batch);
        }
        pass.set_bind_group(0, &self.bindings[eye], &[]);
        self.ferns.draw(pass);
        for drawn in [&backdrop.terrace, &backdrop.foothills]
            .into_iter()
            .chain(&backdrop.plants)
            .chain([&backdrop.stones, &backdrop.rock])
        {
            self.models.draw(pass, eye, &drawn.model, &drawn.batch);
        }
        pass.set_bind_group(0, &self.bindings[eye], &[]);
        backdrop.water.draw(pass);
        // The sky at the far plane, comparing `LessEqual` without writing depth, is shaded only where nothing nearer
        // covers it, as before everything it was shaded everywhere.
        pass.set_pipeline(&self.sky);
        pass.draw(0..3, 0..1);
        backdrop.ranges.draw(pass);
        // The bonfire's flames last, and the trails of burning marshmallows, added to what is behind them, writing no
        // depth.
        self.fire.draw(pass);
        self.trails.draw(pass);
    }

    fn draw_translucent(&self, pass: &mut wgpu::RenderPass<'_>, eye: usize) {
        self.plaques.draw(pass, eye, self.reveal);
    }

    fn set_reveal(&mut self, reveal: Option<(Vec3, f32)>) {
        self.reveal = reveal.filter(|(centre, radius)| centre.is_finite() && radius.is_finite());
        self.models.set_reveal(self.reveal);
    }
}

#[cfg(test)]
mod tests;
