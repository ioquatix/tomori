// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

//! How the arrival scene looks: placeholder forms for its objects, the pointing ray, the highlight on what it points at, and labels anchored in the world.
//!
//! [`cubes`] and [`label_mesh`] are pure, so they are tested without a GPU. Every object's form comes from [`form`] alone, so the placeholders can be replaced, for example by server constellations or environment previews, without touching the layout or the pointing.
use crate::{
    arrival::{Align, Key, Kind, Label, Object, Scene, attention_colour, clip, facing_from},
    delegation::Cube,
    hand_menu::MenuKind,
    panel::Plane,
    server_choice::Source,
};
use bytemuck::{Pod, Zeroable};
use glam::{Mat3, Quat, Vec3};
use std::f32::consts::{FRAC_PI_4, TAU};
use tomori_protocol::{Activity, Attention};
use tomori_text::atlas::SOLID;

/// The most cubes arrival adds to the scene.
pub const MAX_CUBES: usize = 2048;
/// The most label vertices, which bounds the buffer.
const MAX_VERTICES: usize = 1 << 19;
const INITIAL_VERTICES: usize = 1 << 14;
pub use tomori_scene::pointing::Beam;

/// The colour of the participant cubes around a space, distinct from the swarm's blues so that people read differently from the agent.
const PARTICIPANT: [f32; 3] = [1.0, 0.55, 0.75];
const HOVER: [f32; 3] = [0.45, 0.95, 1.0];
const TEXT: [f32; 3] = [0.92, 0.95, 1.0];
const MUTED: [f32; 3] = [0.48, 0.72, 0.85];
const BACKGROUND: [f32; 3] = [0.012, 0.024, 0.045];
const HOVER_BACKGROUND: [f32; 3] = [0.03, 0.11, 0.15];

/// What to draw this frame.
pub struct Frame<'a> {
    pub scene: &'a Scene,
    /// The object the ray meets.
    pub hovered: Option<&'a Key>,
    /// The right ray and the left, drawn in arrival and, from the headset, in a space.
    pub ray: Option<Beam>,
    pub left_ray: Option<Beam>,
    /// Seconds, for slow motion such as turning.
    pub time: f32,
}

fn cube(position: Vec3, scale: Vec3, color: [f32; 3], orientation: Quat) -> Cube {
    Cube {
        position: position.to_array(),
        scale: scale.to_array(),
        color,
        orientation: orientation.normalize().to_array(),
    }
}

/// A cube from `a` to `b`, `width` thick.
fn bar(a: Vec3, b: Vec3, width: f32, color: [f32; 3]) -> Option<Cube> {
    let along = b - a;
    let length = along.length();
    let direction = along.try_normalize()?;
    Some(cube(
        (a + b) * 0.5,
        Vec3::new(width, width, length),
        color,
        Quat::from_rotation_arc(Vec3::Z, direction),
    ))
}

/// The colour marking `attention` at `time`. Input and problems pulse, quickly enough to be noticed; a reply only glows gently, since it can wait.
fn attention_pulse(attention: Attention, time: f32) -> [f32; 3] {
    let (speed, depth) = match attention {
        Attention::Input | Attention::Problem => (3.0, 0.3),
        Attention::Reply | Attention::Unknown => (1.2, 0.15),
    };
    let pulse = 1.0 - depth + depth * (time * speed).sin();
    attention_colour(attention).map(|value| value * pulse)
}

/// `color` glowing at `time`: its brightness breathes gently, about once every three seconds, so that a cue is noticed without flashing.
fn glow(color: [f32; 3], time: f32) -> [f32; 3] {
    let breath = 0.85 + 0.15 * (time * TAU / 3.0).sin();
    color.map(|value| value * breath)
}

fn hsv(hue: f32, saturation: f32, value: f32) -> [f32; 3] {
    let h = hue.rem_euclid(1.0) * 6.0;
    let c = value * saturation;
    let x = c * (1.0 - (h % 2.0 - 1.0).abs());
    let (r, g, b) = match h as u32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let m = value - c;
    [r + m, g + m, b + m]
}

/// How an object's form and highlight turn, their +Z towards the viewer: in the plane of its panel when it is a row of one, as the hand menu's are, and otherwise facing `viewer`.
fn orientation(object: &Object, viewer: Option<Vec3>) -> Quat {
    match object.panel {
        Some(area) => Quat::from_mat3(&Mat3::from_cols(
            area.plane.right,
            area.plane.up,
            area.plane.normal(),
        )),
        None => Quat::from_rotation_arc(
            Vec3::Z,
            facing_from(object.position, viewer, object.label.tilt),
        ),
    }
}

/// An object's placeholder form, in the world. Replace this to change how arrival looks.
fn form(object: &Object, time: f32, viewer: Option<Vec3>, out: &mut Vec<Cube>) {
    let p = object.position;
    let s = object.size;
    let face = orientation(object, viewer);
    match object.kind {
        Kind::Server {
            source,
            available,
            attention,
        } => {
            let color = match (available, source) {
                (false, _) => [0.32, 0.33, 0.36],
                (true, Source::Discovered) => [0.45, 0.8, 1.0],
                (true, Source::Usb) => [0.62, 0.74, 0.9],
                // Remembered or given at startup, or known another way, by a later client.
                (true, _) => [1.0, 0.72, 0.38],
            };
            let turn = Quat::from_rotation_y(time * 0.4) * Quat::from_rotation_x(0.5);
            out.push(cube(p, Vec3::splat(s * 1.3), color, turn));
            if source == Source::Usb {
                out.extend(bar(
                    Vec3::new(p.x, 0.0, p.z),
                    p - Vec3::Y * s,
                    0.004,
                    [0.14, 0.2, 0.26],
                ));
            }
            // A glow of points in the attention colour around a server whose spaces need it.
            if let Some(attention) = attention {
                let color = attention_pulse(attention, time);
                for n in 0..10 {
                    let angle = n as f32 * TAU / 10.0 - time * 0.6;
                    let offset = face * Vec3::new(angle.cos(), angle.sin(), 0.0) * s * 1.55;
                    out.push(cube(p + offset, Vec3::splat(s * 0.16), color, face));
                }
            }
        }
        Kind::Jump { attention } => {
            // A diamond in the attention colour, turning quickly, with a stalk down towards its server.
            let diamond = Quat::from_rotation_y(time * 1.4)
                * Quat::from_rotation_z(FRAC_PI_4)
                * Quat::from_rotation_x(0.6155);
            let color = attention_pulse(attention, time);
            out.push(cube(p, Vec3::splat(s * 1.2), color, diamond));
            out.extend(bar(
                p - Vec3::Y * s * 1.1,
                p - Vec3::Y * s * 2.4,
                0.003,
                color.map(|value| value * 0.6),
            ));
        }
        Kind::Connected => {
            let turn = Quat::from_rotation_y(time * 0.4) * Quat::from_rotation_x(0.5);
            out.push(cube(p, Vec3::splat(s * 1.2), [1.0, 0.72, 0.38], turn));
            for n in 0..8 {
                let angle = n as f32 * TAU / 8.0 + time * 0.3;
                let offset = face * Vec3::new(angle.cos(), angle.sin(), 0.0) * s * 1.7;
                out.push(cube(
                    p + offset,
                    Vec3::splat(s * 0.18),
                    [1.0, 0.85, 0.6],
                    Quat::IDENTITY,
                ));
            }
        }
        Kind::Space {
            activity,
            participants,
            hue,
            attention,
        } => {
            let (saturation, value) = if activity == Activity::Closed {
                (0.2, 0.45)
            } else {
                (0.6, 0.95)
            };
            let speed = match activity {
                Activity::Running => 1.6,
                Activity::Closed => 0.0,
                _ => 0.25,
            };
            // A cube on its corner reads as a diamond from any side.
            let diamond = Quat::from_rotation_y(time * speed + hue * TAU)
                * Quat::from_rotation_z(FRAC_PI_4)
                * Quat::from_rotation_x(0.6155);
            out.push(cube(
                p,
                Vec3::splat(s * 1.15),
                hsv(hue, saturation, value),
                diamond,
            ));
            // The mark of what the space needs, above it; a waiting space from a server that does not list attention is
            // marked as needing input.
            let mark = attention.or((activity == Activity::Waiting).then_some(Attention::Input));
            if let Some(mark) = mark {
                out.push(cube(
                    p + Vec3::Y * s * 1.6,
                    Vec3::splat(s * 0.3),
                    attention_pulse(mark, time),
                    Quat::IDENTITY,
                ));
            }
            let shown = participants.min(12);
            for n in 0..shown {
                let angle = n as f32 * TAU / shown as f32 + time * 0.5;
                out.push(cube(
                    p + Vec3::new(angle.cos(), -0.6, angle.sin()) * s * 1.7,
                    Vec3::splat(s * 0.25),
                    PARTICIPANT,
                    Quat::IDENTITY,
                ));
            }
        }
        Kind::Create { open } => {
            // A plus, which turns into a cross while its rings are open, to step back.
            let twist = face * Quat::from_rotation_z(if open { FRAC_PI_4 } else { 0.0 });
            let color = [0.55, 0.95, 0.72];
            out.push(cube(
                p,
                Vec3::new(s * 2.0, s * 0.45, s * 0.45),
                color,
                twist,
            ));
            out.push(cube(
                p,
                Vec3::new(s * 0.45, s * 2.0, s * 0.45),
                color,
                twist,
            ));
        }
        Kind::Choice => {
            let turn = face * Quat::from_rotation_z(FRAC_PI_4);
            out.push(cube(p, Vec3::splat(s * 1.2), [0.7, 0.85, 1.0], turn));
        }
        Kind::Menu(kind) => {
            let pulse = 0.6 + 0.4 * (time * 6.0).sin().abs();
            match kind {
                // A section that holds something waiting flashes.
                MenuKind::Section { flashing } => {
                    let color = if flashing {
                        [1.0 * pulse, 0.6 * pulse, 0.2 * pulse]
                    } else {
                        [0.4, 0.85, 0.9]
                    };
                    out.push(cube(p, Vec3::splat(s), color, face));
                }
                MenuKind::Button => {
                    out.push(cube(p, Vec3::splat(s * 0.8), [0.55, 0.75, 1.0], face))
                }
                MenuKind::Heading | MenuKind::Text => {}
                // A swatch of a participant's colour.
                MenuKind::Person(color) => {
                    out.push(cube(p, Vec3::splat(s * 0.9), color, face));
                }
                // A wrist's cue is a small glow, of a steady size, breathing slowly rather than flashing.
                MenuKind::Cue => {
                    out.push(cube(p, Vec3::splat(s), glow([1.0, 0.6, 0.2], time), face))
                }
                MenuKind::ProblemCue => {
                    out.push(cube(p, Vec3::splat(s), glow([1.0, 0.2, 0.15], time), face))
                }
            }
        }
        Kind::Pair { pending } => {
            // A key: a diamond bow and a shaft with a bit, which pulses while its code waits to be confirmed.
            let glow = if pending {
                0.75 + 0.25 * (time * 4.0).sin()
            } else {
                1.0
            };
            let color = [0.95 * glow, 0.78 * glow, 0.3 * glow];
            let bow = face * Quat::from_rotation_z(FRAC_PI_4);
            out.push(cube(
                p + face * Vec3::new(-s * 0.9, 0.0, 0.0),
                Vec3::splat(s * 0.8),
                color,
                bow,
            ));
            out.push(cube(
                p + face * Vec3::new(s * 0.5, 0.0, 0.0),
                Vec3::new(s * 1.6, s * 0.3, s * 0.3),
                color,
                face,
            ));
            out.push(cube(
                p + face * Vec3::new(s * 1.1, -s * 0.35, 0.0),
                Vec3::new(s * 0.3, s * 0.5, s * 0.3),
                color,
                face,
            ));
        }
    }
}

fn brighten(color: [f32; 3], amount: f32) -> [f32; 3] {
    color.map(|v| v + (1.0 - v) * amount)
}

/// Every cube arrival adds: object forms, the highlight on the object pointed at and on the stick's selection, and the ray. An object's size is already scaled by its presence; its colour fades with it too.
pub fn cubes(frame: &Frame) -> Vec<Cube> {
    let mut out = Vec::new();
    for object in &frame.scene.objects {
        if object.presence <= 0.0 {
            continue;
        }
        let hovered = object.choosable && frame.hovered == Some(&object.key);
        let fade = object.presence * (1.0 - 0.7 * object.dim);
        let start = out.len();
        form(object, frame.time, frame.scene.viewer, &mut out);
        for cube in &mut out[start..] {
            if hovered {
                let position = Vec3::from_array(cube.position);
                cube.position = (object.position + (position - object.position) * 1.25).to_array();
                cube.scale = cube.scale.map(|v| v * 1.25);
                cube.color = brighten(cube.color, 0.35);
            }
            cube.color = cube.color.map(|v| v * fade);
        }
        let face = orientation(object, frame.scene.viewer);
        if hovered {
            for n in 0..16 {
                let angle = n as f32 * TAU / 16.0 + frame.time;
                let offset = face * Vec3::new(angle.cos(), angle.sin(), 0.0) * object.size * 2.1;
                out.push(cube(
                    object.position + offset,
                    Vec3::splat(object.size * 0.14),
                    HOVER,
                    face,
                ));
            }
        }
        if object.selected && object.choosable {
            out.push(cube(
                object.position - Vec3::Y * object.size * 1.45,
                Vec3::new(object.size * 2.0, 0.006, 0.006),
                [0.9, 0.92, 1.0],
                face,
            ));
        }
    }
    for beam in [frame.ray, frame.left_ray].into_iter().flatten() {
        let color = if beam.hit { HOVER } else { [0.55, 0.6, 0.75] };
        out.extend(bar(beam.origin, beam.end, 0.003, color));
        out.push(cube(
            beam.end,
            Vec3::splat(0.012),
            brighten(color, 0.4),
            Quat::IDENTITY,
        ));
    }
    out.truncate(MAX_CUBES);
    out
}

#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Pod, Zeroable)]
pub struct Vertex {
    position: [f32; 3],
    color: [f32; 4],
    /// Where in the glyph atlas, in its pixels: a glyph's tile, or the solid block for a background.
    uv: [f32; 2],
    weight: f32,
}

const ATTRIBUTES: [wgpu::VertexAttribute; 4] =
    wgpu::vertex_attr_array![0 => Float32x3, 1 => Float32x4, 2 => Float32x2, 3 => Float32];

/// A label and how to draw it.
struct Styled<'a> {
    label: &'a Label,
    /// Where the viewer is, which the label faces.
    viewer: Option<Vec3>,
    /// The plane of the panel the label's object is a row of, which it is drawn in instead.
    plane: Option<Plane>,
    hovered: bool,
    /// How opaque it is, from its own alpha and its object's dimming.
    alpha: f32,
}

/// One label's text, on a translucent background, facing [`crate::arrival::HOME`] and anchored where its object is. Every quad lies in the label's plane, so that a label tilted to face the viewer draws its glyphs whole and its background behind all of them.
fn label_into(styled: &Styled, mesh: &mut Vec<Vertex>) {
    let label = styled.label;
    let p = label.pixel;
    let columns = label
        .lines
        .iter()
        .map(|line| crate::ui_text::drawn(line).count())
        .max()
        .unwrap_or(0);
    if columns == 0 {
        return;
    }
    // In its panel's plane, or else upright unless the label tilts to face the viewer.
    let Plane { right, up, .. } = styled.plane.unwrap_or_else(|| {
        Plane::facing(
            label.anchor,
            facing_from(label.anchor, styled.viewer, label.tilt),
        )
    });
    let normal = right.cross(up);
    let width = columns as f32 * 9.0 * p;
    let height = label.lines.len() as f32 * 11.0 * p;
    let origin = match label.align {
        Align::Left => label.anchor,
        Align::Centre => label.anchor - right * width * 0.5,
        Align::Right => label.anchor - right * width,
    };
    let alpha = styled.alpha;
    if alpha <= 0.0 {
        return;
    }
    let rgba = |rgb: [f32; 3], a: f32| [rgb[0], rgb[1], rgb[2], a * alpha];
    // A quad `x` and `y` along and down from the origin and `z` towards the viewer, in metres, drawn from `tile` of the glyph atlas.
    let mut quad =
        |x: f32, y: f32, w: f32, h: f32, z: f32, color: [f32; 4], tile: [f32; 4], weight: f32| {
            if mesh.len() + 6 > MAX_VERTICES {
                return;
            }
            let corner = origin + right * x - up * y + normal * z;
            for [u, v] in [
                [0.0, 0.0],
                [1.0, 0.0],
                [1.0, 1.0],
                [0.0, 0.0],
                [1.0, 1.0],
                [0.0, 1.0],
            ] {
                mesh.push(Vertex {
                    position: (corner + right * (u * w) - up * (v * h)).to_array(),
                    color,
                    uv: [tile[0] + u * tile[2], tile[1] + v * tile[3]],
                    weight,
                });
            }
        };
    let (background, opacity) = if styled.hovered {
        (HOVER_BACKGROUND, 0.9)
    } else {
        (BACKGROUND, 0.72)
    };
    quad(
        -3.0 * p,
        -3.0 * p,
        width + 5.0 * p,
        height + 4.0 * p,
        0.0,
        rgba(background, opacity),
        [SOLID[0], SOLID[1], 0.0, 0.0],
        0.0,
    );
    for (row, line) in label.lines.iter().enumerate() {
        let color = rgba(
            match (row, styled.hovered) {
                (0, true) => HOVER,
                (0, false) => TEXT,
                _ => MUTED,
            },
            1.0,
        );
        let cells: Vec<(char, ())> = crate::ui_text::drawn(line).map(|ch| (ch, ())).collect();
        let top = (row * 11) as f32;
        for (glyph, ()) in crate::ui_text::quads(&cells, 0.0, top, 11.0, |()| false) {
            quad(
                glyph.x * p,
                glyph.y * p,
                glyph.width * p,
                glyph.height * p,
                0.002,
                color,
                glyph.tile,
                glyph.weight,
            );
        }
    }
}

/// The heading, with the full text of the object pointed at beneath it. A description of several lines, such as a
/// space's, raises the heading by the lines beyond its first, so that it grows upwards, clear of the objects below.
fn heading(scene: &Scene, hovered: Option<&Key>) -> Option<Label> {
    let mut heading = scene.heading.clone()?;
    if let Some(object) = hovered.and_then(|key| scene.object(key)) {
        let before = heading.lines.len();
        heading
            .lines
            .extend(object.description.lines().map(|line| clip(line, 48)));
        let raised = heading.lines.len().saturating_sub(before + 1);
        heading.anchor.y += raised as f32 * 11.0 * heading.pixel;
    }
    Some(heading)
}

/// Every label's vertices in scene coordinates, the farthest from [`crate::arrival::HOME`] first, so that translucent backgrounds blend over what is behind them.
pub fn label_mesh(scene: &Scene, hovered: Option<&Key>) -> Vec<Vertex> {
    let heading = heading(scene, hovered);
    let mut labels: Vec<Styled> = scene
        .objects
        .iter()
        .map(|object| Styled {
            label: &object.label,
            viewer: scene.viewer,
            plane: object.panel.map(|area| area.plane),
            hovered: object.choosable && hovered == Some(&object.key),
            alpha: object.label.alpha * (1.0 - 0.65 * object.dim),
        })
        .chain(heading.iter().map(|label| Styled {
            label,
            viewer: scene.viewer,
            plane: None,
            hovered: false,
            alpha: label.alpha,
        }))
        .collect();
    let viewer = scene.viewer.unwrap_or(crate::arrival::HOME);
    let distance = |styled: &Styled| styled.label.anchor.distance(viewer);
    labels.sort_by(|a, b| distance(b).total_cmp(&distance(a)));
    let mut mesh = Vec::new();
    for styled in &labels {
        label_into(styled, &mut mesh);
    }
    mesh
}

/// The arrival labels on the GPU, rebuilt only when the scene or what is pointed at changes, or when glyphs they need were waiting for the atlas.
pub struct ArrivalLabels {
    pipeline: wgpu::RenderPipeline,
    vertices: wgpu::Buffer,
    capacity: usize,
    count: u32,
    /// The vertices in the buffer, drawn unless hidden.
    length: u32,
    /// What the mesh was built for, while it has every glyph, rather than lacking some that waited for a later frame's budget.
    cached: Option<(Scene, Option<Key>)>,
}

impl ArrivalLabels {
    #[cfg(test)]
    pub fn capacity(&self) -> usize {
        self.capacity
    }

    /// `samples` is the scene pass's sample count, which this draws within.
    pub fn new(
        device: &wgpu::Device,
        format: wgpu::TextureFormat,
        camera_layout: &wgpu::BindGroupLayout,
        glyphs: &crate::glyphs::Glyphs,
        samples: u32,
    ) -> Self {
        Self::with_views(
            device,
            format,
            camera_layout,
            glyphs,
            samples,
            crate::multiview::Views::One,
        )
    }

    /// As [`Self::new`], drawn in passes of `views`; see [`crate::multiview`].
    pub fn with_views(
        device: &wgpu::Device,
        format: wgpu::TextureFormat,
        camera_layout: &wgpu::BindGroupLayout,
        glyphs: &crate::glyphs::Glyphs,
        samples: u32,
        views: crate::multiview::Views,
    ) -> Self {
        let shader = crate::multiview::shader(
            device,
            "workspace.wgsl",
            include_str!("workspace.wgsl"),
            views,
        );
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("arrival labels layout"),
            bind_group_layouts: &[Some(camera_layout), Some(glyphs.layout())],
            immediate_size: 0,
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("arrival labels"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                compilation_options: Default::default(),
                buffers: &[Some(wgpu::VertexBufferLayout {
                    array_stride: size_of::<Vertex>() as u64,
                    step_mode: wgpu::VertexStepMode::Vertex,
                    attributes: &ATTRIBUTES,
                })],
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                compilation_options: crate::glyphs::compilation(format),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: wgpu::PrimitiveState {
                cull_mode: None,
                ..Default::default()
            },
            depth_stencil: Some(wgpu::DepthStencilState {
                format: wgpu::TextureFormat::Depth32Float,
                // Translucent backgrounds must not hide the glyphs or other labels.
                depth_write_enabled: Some(false),
                depth_compare: Some(wgpu::CompareFunction::LessEqual),
                stencil: Default::default(),
                bias: Default::default(),
            }),
            multisample: wgpu::MultisampleState {
                count: samples,
                ..Default::default()
            },
            multiview_mask: views.mask(),
            cache: None,
        });
        let vertices = Self::buffer(device, INITIAL_VERTICES);
        Self {
            pipeline,
            vertices,
            capacity: INITIAL_VERTICES,
            count: 0,
            length: 0,
            cached: None,
        }
    }

    fn buffer(device: &wgpu::Device, capacity: usize) -> wgpu::Buffer {
        device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("arrival label quads"),
            size: (capacity * size_of::<Vertex>()) as u64,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        })
    }

    pub fn hide(&mut self) {
        self.count = 0;
    }

    pub fn update(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        scene: &Scene,
        hovered: Option<&Key>,
    ) {
        let unchanged = self
            .cached
            .as_ref()
            .is_some_and(|(cached, key)| cached == scene && key.as_ref() == hovered);
        if !unchanged {
            let waited = tomori_text::atlas::waited();
            let mesh = label_mesh(scene, hovered);
            if mesh.len() > self.capacity {
                self.capacity = mesh.len().next_power_of_two().min(MAX_VERTICES);
                self.vertices = Self::buffer(device, self.capacity);
            }
            if !mesh.is_empty() {
                queue.write_buffer(&self.vertices, 0, bytemuck::cast_slice(&mesh));
            }
            self.length = mesh.len().min(self.capacity) as u32;
            self.cached =
                (tomori_text::atlas::waited() == waited).then(|| (scene.clone(), hovered.cloned()));
        }
        // `hide` clears only the count; the buffer still holds the mesh.
        self.count = self.length;
    }

    /// Draw for the eye `camera` binds, from the glyph atlas `glyphs` binds.
    pub fn draw(
        &self,
        pass: &mut wgpu::RenderPass<'_>,
        camera: &wgpu::BindGroup,
        glyphs: &wgpu::BindGroup,
    ) {
        if self.count == 0 {
            return;
        }
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, camera, &[]);
        pass.set_bind_group(1, glyphs, &[]);
        pass.set_vertex_buffer(0, self.vertices.slice(..));
        pass.draw(0..self.count, 0..1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::arrival::{HOME, Item, View, facing};

    fn view() -> View {
        let space = |id: &str, participants| Item {
            key: Key::Space(id.into()),
            kind: Kind::Space {
                activity: Activity::Running,
                participants,
                hue: crate::arrival::hue(id),
                attention: None,
            },
            label: id.into(),
            detail: Some("running".into()),
            selected: false,
        };
        View {
            heading: "Spaces".into(),
            items: vec![
                space("one", 2),
                space("two", 0),
                Item {
                    key: Key::Create,
                    kind: Kind::Create { open: false },
                    label: "New space".into(),
                    detail: None,
                    selected: true,
                },
            ],
            ..Default::default()
        }
    }

    fn frame(scene: &Scene, hovered: Option<&Key>) -> Vec<Cube> {
        cubes(&Frame {
            scene,
            hovered,
            left_ray: None,
            ray: None,
            time: 0.0,
        })
    }

    #[test]
    fn every_object_has_a_form_near_it() {
        let scene = Scene::layout(&view());
        let cubes = frame(&scene, None);
        for object in &scene.objects {
            assert!(cubes.iter().any(|cube| {
                Vec3::from_array(cube.position).distance(object.position) < object.size * 3.0
            }));
        }
        assert!(cubes.iter().all(Cube::valid));
    }

    #[test]
    fn participants_show_as_cubes_in_their_own_colour() {
        let scene = Scene::layout(&view());
        let cubes = frame(&scene, None);
        assert_eq!(
            cubes
                .iter()
                .filter(|cube| cube.color == PARTICIPANT)
                .count(),
            2
        );
    }

    #[test]
    fn pointing_at_an_object_highlights_it_and_shows_its_full_text() {
        let scene = Scene::layout(&view());
        let key = Key::Space("one".into());
        let plain = frame(&scene, None);
        let hovered = frame(&scene, Some(&key));
        assert_eq!(
            hovered.iter().filter(|cube| cube.color == HOVER).count(),
            16
        );
        assert!(hovered.len() > plain.len());
        let shown = heading(&scene, Some(&key)).unwrap();
        assert_eq!(shown.lines.last().unwrap(), "one: running");
        // A one-line description leaves the heading where it was.
        assert_eq!(shown.anchor, scene.heading.as_ref().unwrap().anchor);
        assert_ne!(label_mesh(&scene, Some(&key)), label_mesh(&scene, None));

        // A space's description, a line each, is shown whole, and raises the heading by the lines beyond its first.
        let mut described = view();
        described.items[0].detail = Some("ioquatix/tomori · Sky\nrunning · 2 here\n“Done.”".into());
        let scene = Scene::layout(&described);
        let plain = scene.heading.clone().unwrap();
        let shown = heading(&scene, Some(&key)).unwrap();
        assert_eq!(
            shown.lines[plain.lines.len()..],
            [
                "one",
                "ioquatix/tomori · Sky",
                "running · 2 here",
                "“Done.”"
            ]
        );
        assert!((shown.anchor.y - plain.anchor.y - 3.0 * 11.0 * plain.pixel).abs() < 1e-6);
    }

    #[test]
    fn the_ray_reaches_from_the_controller_to_its_end() {
        let scene = Scene::default();
        let origin = Vec3::new(0.2, 1.2, -0.3);
        let end = Vec3::new(0.0, 1.3, -1.6);
        let cubes = cubes(&Frame {
            scene: &scene,
            hovered: None,
            left_ray: None,
            ray: Some(Beam {
                origin,
                end,
                hit: false,
            }),
            time: 0.0,
        });
        assert_eq!(cubes.len(), 2);
        assert_ne!(cubes[0].color, HOVER);
        let beam = &cubes[0];
        assert!(Vec3::from_array(beam.position).distance((origin + end) * 0.5) < 1e-5);
        assert!((beam.scale[2] - origin.distance(end)).abs() < 1e-5);
        assert!(Vec3::from_array(cubes[1].position).distance(end) < 1e-5);
    }

    #[test]
    fn a_ray_that_meets_something_is_highlighted_without_an_arrival_object() {
        // In a space, the ray meets a worker rather than an arrival object.
        let scene = Scene::default();
        let cubes = cubes(&Frame {
            scene: &scene,
            hovered: None,
            left_ray: None,
            ray: Some(Beam {
                origin: Vec3::new(0.2, 1.2, -0.3),
                end: Vec3::new(0.0, 1.3, -2.6),
                hit: true,
            }),
            time: 0.0,
        });
        assert_eq!(cubes.len(), 2);
        assert_eq!(cubes[0].color, HOVER);
    }

    #[test]
    fn labels_face_home_and_stay_at_their_anchors() {
        let scene = Scene::layout(&view());
        let mesh = label_mesh(&scene, None);
        assert!(!mesh.is_empty());
        // Every vertex lies within a label's extent of some anchor, whichever way the head turns.
        for vertex in &mesh {
            let position = Vec3::from_array(vertex.position);
            assert!(
                scene
                    .objects
                    .iter()
                    .map(|object| object.label.anchor)
                    .chain(scene.heading.iter().map(|label| label.anchor))
                    .any(|anchor| position.distance(anchor) < 0.35),
                "{position}"
            );
        }
        // A label straight ahead is upright in the plane facing home.
        let object = scene.object(&Key::Create).unwrap();
        let normal = facing(object.label.anchor);
        let mut one = Scene::default();
        one.objects.push(object.clone());
        let mesh = label_mesh(&one, None);
        let depth =
            |vertex: &Vertex| (Vec3::from_array(vertex.position) - object.label.anchor).dot(normal);
        assert!(mesh.iter().all(|vertex| depth(vertex).abs() < 0.003));
        assert!(object.label.anchor.distance(HOME) > 0.5);
    }

    #[test]
    fn a_space_looks_the_same_wherever_it_is_drawn() {
        let mut a = Scene::layout(&view());
        let b = a.clone();
        a.objects.swap(0, 1);
        let colour = |scene: &Scene, id: &str| {
            let object = scene.object(&Key::Space(id.into())).unwrap().clone();
            let mut cubes = Vec::new();
            form(&object, 0.0, None, &mut cubes);
            cubes[0].color
        };
        assert_eq!(colour(&a, "one"), colour(&b, "one"));
        assert_ne!(colour(&b, "one"), colour(&b, "two"));
    }

    #[test]
    fn attention_marks_servers_jumps_and_spaces_in_its_colour() {
        let at = |kind: Kind| Object {
            key: Key::Create,
            kind,
            position: Vec3::new(0.0, 1.4, -1.6),
            size: 0.07,
            label: Label {
                anchor: Vec3::new(0.0, 1.3, -1.6),
                lines: Vec::new(),
                align: Align::Centre,
                pixel: 0.0026,
                alpha: 1.0,
                tilt: false,
            },
            description: String::new(),
            selected: false,
            dim: 0.0,
            presence: 1.0,
            choosable: true,
            panel: None,
        };
        let drawn = |kind: Kind| {
            let mut cubes = Vec::new();
            form(&at(kind), 0.0, None, &mut cubes);
            cubes
        };
        // At time zero the pulse is at its middle.
        let marked = |cubes: &[Cube], attention: Attention| {
            let colour = attention_pulse(attention, 0.0);
            cubes.iter().filter(|cube| cube.color == colour).count()
        };
        let server = |attention| Kind::Server {
            source: Source::Discovered,
            available: true,
            attention,
        };
        assert_eq!(drawn(server(None)).len(), 1);
        let glowing = drawn(server(Some(Attention::Problem)));
        assert_eq!(marked(&glowing, Attention::Problem), 10);
        let jump = drawn(Kind::Jump {
            attention: Attention::Input,
        });
        assert_eq!(marked(&jump, Attention::Input), 1);
        assert!(jump.iter().all(Cube::valid));
        let space = |activity, attention| Kind::Space {
            activity,
            participants: 0,
            hue: 0.3,
            attention,
        };
        assert_eq!(
            marked(
                &drawn(space(Activity::Idle, Some(Attention::Reply))),
                Attention::Reply
            ),
            1
        );
        assert_eq!(drawn(space(Activity::Idle, None)).len(), 1);
        // An older server's waiting space is still marked as needing input.
        assert_eq!(
            marked(&drawn(space(Activity::Waiting, None)), Attention::Input),
            1
        );
        // The levels look different from each other.
        assert_ne!(
            attention_pulse(Attention::Input, 0.0),
            attention_pulse(Attention::Problem, 0.0)
        );
        assert_ne!(
            attention_pulse(Attention::Problem, 0.0),
            attention_pulse(Attention::Reply, 0.0)
        );
    }

    fn object(kind: Kind) -> Object {
        Object {
            key: Key::Create,
            kind,
            position: Vec3::new(0.0, 1.4, -1.6),
            size: 0.07,
            label: Label {
                anchor: Vec3::new(0.0, 1.3, -1.6),
                lines: vec!["A label".into()],
                align: Align::Left,
                pixel: 0.0026,
                alpha: 1.0,
                tilt: false,
            },
            description: String::new(),
            selected: false,
            dim: 0.0,
            presence: 1.0,
            choosable: true,
            panel: None,
        }
    }

    fn drawn(kind: Kind, time: f32) -> Vec<Cube> {
        let mut cubes = Vec::new();
        form(&object(kind), time, Some(HOME), &mut cubes);
        // Arrival makes its own cubes, which may be longer than a metre, such as the USB cable.
        assert!(cubes.iter().all(|cube| {
            cube.position
                .iter()
                .chain(&cube.scale)
                .chain(&cube.orientation)
                .all(|value| value.is_finite())
        }));
        cubes
    }

    #[test]
    fn every_kind_of_object_has_its_form() {
        use crate::hand_menu::MenuKind;
        for (source, available) in [
            (Source::Override, false),
            (Source::Remembered, true),
            (Source::Usb, true),
        ] {
            let server = Kind::Server {
                source,
                available,
                attention: None,
            };
            assert!(!drawn(server, 1.0).is_empty());
        }
        for open in [false, true] {
            assert_eq!(drawn(Kind::Create { open }, 0.0).len(), 2);
        }
        assert_eq!(drawn(Kind::Choice, 0.0).len(), 1);
        for pending in [false, true] {
            assert_eq!(drawn(Kind::Pair { pending }, 0.5).len(), 3);
        }
        for (kind, cubes) in [
            (MenuKind::Section { flashing: false }, 1),
            (MenuKind::Section { flashing: true }, 1),
            (MenuKind::Button, 1),
            (MenuKind::Heading, 0),
            (MenuKind::Text, 0),
            (MenuKind::Person([0.2, 0.4, 0.6]), 1),
            (MenuKind::Cue, 1),
            (MenuKind::ProblemCue, 1),
        ] {
            assert_eq!(drawn(Kind::Menu(kind), 0.25).len(), cubes);
        }
    }

    #[test]
    fn a_wrist_cue_glows_steadily_rather_than_flashing() {
        for kind in [MenuKind::Cue, MenuKind::ProblemCue] {
            let frames: Vec<Cube> = (0..60)
                .flat_map(|step| drawn(Kind::Menu(kind), step as f32 * 0.05))
                .collect();
            assert_eq!(frames.len(), 60);
            // Its size never changes, and it never dims far, so it does not blink.
            assert!(frames.iter().all(|cube| cube.scale == frames[0].scale));
            // Both are at full red at their brightest.
            let brightness: Vec<f32> = frames.iter().map(|cube| cube.color[0]).collect();
            assert!(
                brightness
                    .iter()
                    .all(|&value| (0.69..=1.0).contains(&value))
            );
            // Over three seconds it changes by little from one frame to the next.
            assert!(
                brightness
                    .windows(2)
                    .all(|pair| (pair[0] - pair[1]).abs() < 0.03)
            );
        }
    }

    #[test]
    fn a_bar_needs_a_length() {
        assert!(bar(Vec3::ONE, Vec3::ONE, 0.01, [1.0; 3]).is_none());
        assert!(bar(Vec3::ZERO, Vec3::ONE, 0.01, [1.0; 3]).is_some());
    }

    #[test]
    fn hues_go_round_the_colour_wheel() {
        let colours: Vec<[f32; 3]> = (0..6).map(|n| hsv(n as f32 / 6.0, 1.0, 1.0)).collect();
        assert_eq!(
            colours,
            [
                [1.0, 0.0, 0.0],
                [1.0, 1.0, 0.0],
                [0.0, 1.0, 0.0],
                [0.0, 1.0, 1.0],
                [0.0, 0.0, 1.0],
                [1.0, 0.0, 1.0],
            ]
        );
    }

    #[test]
    fn labels_without_text_or_opacity_draw_nothing_and_unknown_characters_draw_a_question_mark() {
        let mut empty = object(Kind::Choice);
        empty.label.lines.clear();
        let mut faded = object(Kind::Choice);
        faded.label.alpha = 0.0;
        let mut unknown = object(Kind::Choice);
        unknown.label.lines = vec!["\u{1f600}".into()];
        let mut question = object(Kind::Choice);
        question.label.lines = vec!["?".into()];
        let mesh = |object: Object| {
            let scene = Scene {
                objects: vec![object],
                ..Default::default()
            };
            label_mesh(&scene, None)
        };
        assert!(mesh(empty).is_empty());
        assert!(mesh(faded).is_empty());
        assert_eq!(mesh(unknown), mesh(question));
        // An object that has gone draws no form.
        let mut gone = object(Kind::Choice);
        gone.presence = 0.0;
        let scene = Scene {
            objects: vec![gone],
            ..Default::default()
        };
        let frame = Frame {
            scene: &scene,
            hovered: None,
            left_ray: None,
            ray: None,
            time: 0.0,
        };
        assert!(cubes(&frame).is_empty());
    }

    /// Where the viewer is, and a label below and in front of them, tilted 45° back to face them, as the hand menu's are: a row of solid block glyphs, which fill their row.
    fn tilted() -> (Vec3, Scene) {
        let eye = Vec3::new(0.0, 1.6, 0.0);
        let anchor = eye + Vec3::new(0.0, -1.0, -1.0).normalize() * 0.6;
        let mut label = object(Kind::Menu(MenuKind::Text));
        label.position = anchor;
        label.choosable = false;
        label.label = Label {
            anchor,
            lines: vec!["████".into()],
            align: Align::Left,
            pixel: 0.004,
            alpha: 1.0,
            tilt: true,
        };
        let scene = Scene {
            objects: vec![label],
            viewer: Some(eye),
            ..Default::default()
        };
        (eye, scene)
    }

    /// A point of the label's first glyph, `x` and `y` font pixels right and down from its anchor, in the label's plane.
    fn on_label(scene: &Scene, x: f32, y: f32) -> Vec3 {
        let label = &scene.objects[0].label;
        let normal = facing_from(label.anchor, scene.viewer, true);
        let right = Vec3::Y.cross(normal).normalize();
        let up = normal.cross(right);
        label.anchor + (right * x - up * y) * label.pixel
    }

    #[test]
    fn a_tilted_label_lies_in_its_plane_and_draws_every_character_the_transcript_does() {
        let (_, scene) = tilted();
        let label = &scene.objects[0].label;
        let normal = facing_from(label.anchor, scene.viewer, true);
        assert!(normal.y > 0.7, "tilted back to face the viewer: {normal}");
        let mesh = label_mesh(&scene, None);
        for vertex in &mesh {
            let depth = (Vec3::from_array(vertex.position) - label.anchor).dot(normal);
            assert!(depth.abs() < 0.003, "{depth}");
        }
        // A glyph is one quad from the atlas, with the background's quad.
        assert_eq!(mesh.len(), 6 * (1 + 4));
        // Accented letters, ligatures and Japanese are drawn as themselves, and what no font has as `?`.
        let drawn = |text: &str| {
            let mut scene = scene.clone();
            scene.objects[0].label.lines = vec![text.into()];
            label_mesh(&scene, None)
        };
        assert_ne!(drawn("José"), drawn("Jos?"));
        assert_ne!(drawn("Ł"), drawn("L"));
        assert_ne!(drawn("\u{153}"), drawn("?"));
        assert_eq!(drawn("🎤"), drawn("?"));
        // A letter and its combining mark are one character.
        assert_eq!(drawn("e\u{301}"), drawn("é"));
        // A wide character takes two cells, so its label is wider.
        let width = |mesh: &[Vertex]| mesh[1].position[0] - mesh[0].position[0];
        assert!(width(&drawn("日")) > width(&drawn("a")));
    }

    /// The hand menu's widest rows, of as many characters as it holds, fill its panel as they are drawn, their backgrounds ending within it and less than a cell short of its edge.
    #[test]
    fn the_hand_menus_widest_rows_fill_its_panel_without_overflowing() {
        use crate::hand_menu::{HandMenu, MenuKey, Model, PANEL, PIXEL, Palm, Section, WIDTH};
        let mut menu = HandMenu::default();
        menu.choose(&MenuKey::Section(Section::PeopleAndSecurity));
        let model = Model {
            notice: Some("x".repeat(3 * WIDTH)),
            ..Default::default()
        };
        let palm = Palm {
            wrist: Vec3::new(-0.15, 1.1, -0.35),
            normal: Vec3::new(0.3, 0.8, 0.4).normalize(),
        };
        let head = Vec3::new(0.0, 1.6, 0.0);
        let objects = menu.objects(&palm, &model, head);
        let widest: Vec<_> = objects
            .iter()
            .filter(|object| object.label.lines[0].chars().count() == WIDTH)
            .collect();
        assert_eq!(widest.len(), 3);
        for object in widest {
            let scene = Scene {
                objects: vec![object.clone()],
                viewer: Some(head),
                ..Default::default()
            };
            let right = object.panel.unwrap().plane.right;
            let reach = label_mesh(&scene, None)
                .iter()
                .map(|vertex| (Vec3::from_array(vertex.position) - object.position).dot(right))
                .fold(f32::MIN, f32::max);
            assert!(reach <= PANEL, "{reach}");
            assert!(reach > PANEL - 9.0 * PIXEL, "{reach}");
        }
    }

    /// Each row of the hand menu, its marker and its label, highlighted or not, is drawn within the rectangle of its panel that points at it, and in the panel's plane, however the panel tilts: what is pointed at is what is drawn.
    #[test]
    fn the_hand_menus_rows_are_drawn_within_the_rectangles_that_point_at_them() {
        use crate::hand_menu::{HandMenu, MenuKey, Model, Palm, Section, WIDTH};
        let mut menu = HandMenu::default();
        menu.choose(&MenuKey::Section(Section::PeopleAndSecurity));
        let model = Model {
            name: "x".repeat(2 * WIDTH),
            notice: Some("x".repeat(3 * WIDTH)),
            pairing: Some("Pair".into()),
            ..Default::default()
        };
        let palm = Palm {
            wrist: Vec3::new(-0.15, 1.05, -0.35),
            normal: Vec3::Y,
        };
        for tilt in [0.0f32, 30.0, 60.0] {
            let (sin, cos) = tilt.to_radians().sin_cos();
            let head = palm.wrist + Vec3::new(0.0, 0.2 + sin * 0.45, cos * 0.45);
            let objects = menu.objects(&palm, &model, head);
            for object in &objects {
                let area = object.panel.unwrap();
                let normal = area.plane.normal();
                for hovered in [false, true] {
                    let scene = Scene {
                        objects: vec![object.clone()],
                        viewer: Some(head),
                        ..Default::default()
                    };
                    let key = hovered.then_some(&object.key);
                    let mesh = label_mesh(&scene, key);
                    assert!(!mesh.is_empty());
                    let mut points: Vec<Vec3> = mesh
                        .iter()
                        .map(|vertex| Vec3::from_array(vertex.position))
                        .collect();
                    // A marker's cube, enlarged when highlighted, to its corners.
                    let mut forms = Vec::new();
                    form(object, 0.0, Some(head), &mut forms);
                    let scale = if hovered { 1.25 } else { 1.0 };
                    for cube in &forms {
                        let centre = Vec3::from_array(cube.position);
                        let orientation = Quat::from_array(cube.orientation);
                        let half = Vec3::from_array(cube.scale) * scale;
                        for corner in [-1.0, 1.0] {
                            points.push(
                                centre + orientation * (half * Vec3::new(corner, corner, 0.0)),
                            );
                            points.push(
                                centre + orientation * (half * Vec3::new(corner, -corner, 0.0)),
                            );
                        }
                    }
                    for point in points {
                        let depth = (point - area.plane.origin).dot(normal);
                        assert!(depth.abs() < 0.003, "{tilt}: {depth}");
                        let at = area.local(point);
                        assert!(
                            area.rect.contains(at),
                            "{tilt}: {at} outside {:?}",
                            area.rect
                        );
                    }
                }
            }
        }
    }

    /// The label's text is solid when seen from where it faces: before its quads were built along the label's up, a label tilted by 45° drew each pixel row about 70% as tall as the rows' spacing, leaving stripes, and its background short of its last row.
    /// `scene` drawn for one eye looking through `matrix` onto an image `width` by `height`, with `hovered` pointed at,
    /// or `None` without a Vulkan adapter.
    fn render(
        scene: &Scene,
        hovered: Option<&Key>,
        matrix: glam::Mat4,
        (width, height): (u32, u32),
    ) -> Option<image::RgbaImage> {
        crate::test_gpu::with(|gpu| {
            let device = &gpu.device;
            let queue = &gpu.queue;
            let format = wgpu::TextureFormat::Rgba8UnormSrgb;
            let texture = device.create_texture(&wgpu::TextureDescriptor {
                label: Some("arrival preview eye"),
                size: wgpu::Extent3d {
                    width,
                    height,
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
            let mut renderer = crate::graphics::Renderer::new(device, queue, format, width, height);
            renderer.update_arrival(
                device,
                queue,
                &Frame {
                    scene,
                    hovered,
                    left_ray: None,
                    ray: None,
                    time: 0.0,
                },
            );
            renderer.render(
                device,
                queue,
                std::slice::from_ref(&view),
                &[matrix],
                glam::Mat4::IDENTITY,
                &[],
            );
            let buffer = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("arrival preview readback"),
                size: u64::from(width * height * 4),
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
                        bytes_per_row: Some(width * 4),
                        rows_per_image: Some(height),
                    },
                },
                texture.size(),
            );
            queue.submit([encoder.finish()]);
            let (tx, rx) = std::sync::mpsc::channel();
            buffer
                .slice(..)
                .map_async(wgpu::MapMode::Read, move |result| {
                    let _ = tx.send(result);
                });
            device.poll(wgpu::PollType::wait_indefinitely()).unwrap();
            rx.recv().unwrap().unwrap();
            let data = buffer.slice(..).get_mapped_range().unwrap().to_vec();
            buffer.unmap();
            image::RgbaImage::from_raw(width, height, data).unwrap()
        })
    }

    /// The lobby of a server with spaces in several projects, each with its title, project folder and environment, what
    /// it is doing, who is here and when it was last active, and the start of its newest reply, as the headset sees it
    /// from home, at about a Quest 3 eye image's 13 pixels a degree, with the second space pointed at. Set
    /// `TOMORI_LOBBY_PREVIEW` to a PNG path to save it.
    #[test]
    fn the_lobby_tells_spaces_apart_at_a_glance() {
        const SIZE: (u32, u32) = (1280, 960);
        let spaces = [
            (
                "Tomori development from the headset",
                "ioquatix/tomori · Sky",
                "running · 2 here · just now",
                Some("All 42 tests pass, and the build is green."),
                None,
            ),
            (
                "Release planning",
                "ioquatix/tomori · Farmyard",
                "needs you · 1 here · 5m ago",
                Some("Shall I tag v0.3.0 now?"),
                Some(Attention::Input),
            ),
            (
                "Base scheduler",
                "base-rs/base-scheduler · Sky",
                "new reply · 2h ago",
                Some("The timer wheel now handles long sleeps."),
                Some(Attention::Reply),
            ),
            (
                "Shrine lighting",
                "tomori-assets · Sky",
                "idle · 1d ago",
                Some("I baked the lanterns' light into the floor."),
                None,
            ),
            (
                "Old session",
                "Developer · Sky",
                "closed · 3d ago",
                None,
                None,
            ),
            (
                "Coverage",
                "ioquatix/tomori · Minimal",
                "problem · 4h ago",
                Some("The coverage run failed to start."),
                Some(Attention::Problem),
            ),
        ];
        let mut items: Vec<Item> = spaces
            .iter()
            .enumerate()
            .map(|(index, (title, place, status, latest, attention))| {
                let mut detail = format!("{place}\n{status}");
                if let Some(latest) = latest {
                    detail.push_str(&format!("\n“{latest}”"));
                }
                let id = format!("space-{index}");
                Item {
                    key: Key::Space(id.clone()),
                    kind: Kind::Space {
                        activity: if status.starts_with("closed") {
                            Activity::Closed
                        } else {
                            Activity::Idle
                        },
                        participants: 1,
                        hue: crate::arrival::hue(&id),
                        attention: *attention,
                    },
                    label: (*title).into(),
                    detail: Some(detail),
                    selected: false,
                }
            })
            .collect();
        items.push(Item {
            key: Key::Create,
            kind: Kind::Create { open: false },
            label: "New space".into(),
            detail: None,
            selected: false,
        });
        let view = View {
            heading: "Spaces on Studio".into(),
            connected: Some(Item {
                key: Key::Connected("studio:8765".into()),
                kind: Kind::Connected,
                label: "Studio".into(),
                detail: Some("Choose to change".into()),
                selected: false,
            }),
            items,
            ..Default::default()
        };
        let scene = Scene {
            viewer: Some(HOME),
            ..Scene::layout(&view)
        };
        let hovered = Key::Space("space-1".into());
        // About 13 pixels a degree across the image's 960 rows.
        let fov = (SIZE.1 as f32 / 13.0).to_radians();
        let matrix =
            glam::camera::rh::proj::directx::perspective(
                fov,
                SIZE.0 as f32 / SIZE.1 as f32,
                0.05,
                50.0,
            ) * glam::camera::rh::view::look_at_mat4(HOME, crate::arrival::CENTRE, Vec3::Y);
        let checked = render(&scene, Some(&hovered), matrix, SIZE)
            .inspect(|image| crate::test_support::preview("TOMORI_LOBBY_PREVIEW", image));
        // Each space's label is drawn, every line of it: light text beneath its object, in view.
        let at = |point: Vec3| {
            let clip = matrix * point.extend(1.0);
            let (x, y) = (clip.x / clip.w, clip.y / clip.w);
            (
                ((x * 0.5 + 0.5) * SIZE.0 as f32) as i64,
                ((0.5 - y * 0.5) * SIZE.1 as f32) as i64,
            )
        };
        for (object, image) in scene
            .objects
            .iter()
            .filter(|object| matches!(object.key, Key::Space(_)))
            .flat_map(|object| checked.as_ref().map(|image| (object, image)))
        {
            let label = &object.label;
            let line = 11.0 * label.pixel;
            for (index, _) in label.lines.iter().enumerate() {
                let middle = label.anchor - Vec3::Y * line * (index as f32 + 0.5);
                let (x, y) = at(middle);
                let (left, _) = at(middle - Vec3::X * 0.08);
                let (right, _) = at(middle + Vec3::X * 0.08);
                let lit = (left..right)
                    .flat_map(|x| (y - 3..=y + 3).map(move |y| (x, y)))
                    .filter(|&(x, y)| {
                        x >= 0 && y >= 0 && (x as u32) < SIZE.0 && (y as u32) < SIZE.1
                    })
                    .filter(|&(x, y)| {
                        let pixel = image.get_pixel(x as u32, y as u32);
                        pixel[0] > 150 && pixel[1] > 150 && pixel[2] > 150
                    })
                    .count();
                assert!(
                    lit > 10,
                    "{:?} line {index} at ({x}, {y}): {lit}",
                    object.key
                );
            }
        }
    }

    #[test]
    fn a_tilted_label_draws_solid_glyphs_over_its_whole_background() {
        const SIZE: u32 = 256;
        let (eye, scene) = tilted();
        let centre = on_label(&scene, 4.0, 4.0);
        let matrix = glam::camera::rh::proj::directx::perspective(0.25, 1.0, 0.05, 50.0)
            * glam::camera::rh::view::look_at_mat4(eye, centre, Vec3::Y);
        crate::test_gpu::with(|gpu| {
            let device = &gpu.device;
            let queue = &gpu.queue;
            let scope = device.push_error_scope(wgpu::ErrorFilter::Validation);
            let format = wgpu::TextureFormat::Rgba8UnormSrgb;
            let texture = device.create_texture(&wgpu::TextureDescriptor {
                label: Some("tilted label test eye"),
                size: wgpu::Extent3d {
                    width: SIZE,
                    height: SIZE,
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
            let mut renderer = crate::graphics::Renderer::new(device, queue, format, SIZE, SIZE);
            let mut capture = |scene: &Scene| {
                renderer.update_arrival(
                    device,
                    queue,
                    &Frame {
                        scene,
                        hovered: None,
                        left_ray: None,
                        ray: None,
                        time: 0.0,
                    },
                );
                renderer.render(
                    device,
                    queue,
                    std::slice::from_ref(&view),
                    &[matrix],
                    glam::Mat4::IDENTITY,
                    &[],
                );
                let buffer = device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("tilted label test readback"),
                    size: u64::from(SIZE * SIZE * 4),
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
                            bytes_per_row: Some(SIZE * 4),
                            rows_per_image: Some(SIZE),
                        },
                    },
                    texture.size(),
                );
                queue.submit([encoder.finish()]);
                let (tx, rx) = std::sync::mpsc::channel();
                buffer
                    .slice(..)
                    .map_async(wgpu::MapMode::Read, move |result| {
                        let _ = tx.send(result);
                    });
                device.poll(wgpu::PollType::wait_indefinitely()).unwrap();
                rx.recv().unwrap().unwrap();
                let data = buffer.slice(..).get_mapped_range().unwrap().to_vec();
                buffer.unmap();
                image::RgbaImage::from_raw(SIZE, SIZE, data).unwrap()
            };
            let image = capture(&scene);
            let bare = capture(&Scene {
                viewer: scene.viewer,
                ..Default::default()
            });
            crate::test_support::preview("TOMORI_TILTED_LABEL_PREVIEW", &image);
            let at = |point: Vec3| {
                let clip = matrix * point.extend(1.0);
                let (x, y) = (clip.x / clip.w, clip.y / clip.w);
                let column = ((x * 0.5 + 0.5) * SIZE as f32) as u32;
                let row = ((0.5 - y * 0.5) * SIZE as f32) as u32;
                (column, row)
            };
            // Down the middle of the first glyph, which fills its row, every screen pixel is lit.
            let gaps: Vec<f32> = (0..64)
                .map(|step| 0.25 + 10.5 * step as f32 / 63.0)
                .filter(|y| {
                    let (column, row) = at(on_label(&scene, 4.0, *y));
                    image.get_pixel(column, row)[0] < 180
                })
                .collect();
            assert!(gaps.is_empty(), "the glyph has gaps at font rows {gaps:?}");
            // Left of it, on the background, where the glyph's quad reaches past its outline, nothing is lit: not even single pixels, as when a join's weight was interpolated to just under its mark and drawn as the boldest of glyphs.
            let lit: Vec<(f32, f32)> = (0..16)
                .flat_map(|x| {
                    (0..64)
                        .map(move |y| (-2.5 + 2.0 * x as f32 / 15.0, 0.25 + 10.5 * y as f32 / 63.0))
                })
                .filter(|(x, y)| {
                    let (column, row) = at(on_label(&scene, *x, *y));
                    image.get_pixel(column, row)[0] >= 180
                })
                .collect();
            assert!(lit.is_empty(), "lit beside the glyph at {lit:?}");
            // Below the row, the background covers what is behind it.
            let y = 11.5;
            let (column, row) = at(on_label(&scene, 4.0, y));
            let covered = image.get_pixel(column, row).0[..3]
                .iter()
                .zip(&bare.get_pixel(column, row).0[..3])
                .map(|(a, b)| u32::from(a.abs_diff(*b)))
                .sum::<u32>();
            assert!(image.get_pixel(column, row)[0] < 180, "{y}: lit");
            assert!(covered > 6, "{y}: no background");
            device.poll(wgpu::PollType::wait_indefinitely()).unwrap();
            assert!(pollster::block_on(scope.pop()).is_none());
        });
    }

    #[test]
    fn labels_built_while_glyphs_waited_are_built_again_until_they_have_them() {
        let mut named = object(Kind::Choice);
        named.label.lines = vec![crate::test_support::undrawn('\u{9e80}', 2)];
        let scene = Scene {
            objects: vec![named],
            ..Default::default()
        };
        crate::test_gpu::with(|gpu| {
            let camera_layout =
                crate::graphics::camera_layout(&gpu.device, crate::multiview::Views::One);
            let glyphs = crate::glyphs::Glyphs::new(&gpu.device);
            let mut labels = ArrivalLabels::new(
                &gpu.device,
                wgpu::TextureFormat::Rgba8UnormSrgb,
                &camera_layout,
                &glyphs,
                1,
            );
            let budget = crate::test_support::Budgeted::spent();
            labels.update(&gpu.device, &gpu.queue, &scene, None);
            assert!(labels.cached.is_none());
            let bare = labels.length;
            drop(budget);
            labels.update(&gpu.device, &gpu.queue, &scene, None);
            assert!(labels.cached.is_some());
            assert_eq!(labels.length, bare + 12);
            let waited = tomori_text::atlas::waited();
            labels.update(&gpu.device, &gpu.queue, &scene, None);
            assert_eq!(tomori_text::atlas::waited(), waited);
        });
    }

    #[test]
    fn a_label_mesh_stops_at_the_vertex_limit_and_its_buffer_grows_to_hold_it() {
        let mut long = object(Kind::Choice);
        long.label.lines = vec!["#".repeat(4_000); 40];
        // An object somewhere that cannot be drawn is left out.
        let mut nowhere = object(Kind::Choice);
        nowhere.position = Vec3::NAN;
        nowhere.label.lines.clear();
        let scene = Scene {
            objects: vec![long, nowhere],
            ..Default::default()
        };
        let mesh = label_mesh(&scene, None);
        assert!(mesh.len() <= MAX_VERTICES && mesh.len() > MAX_VERTICES - 6);
        crate::test_gpu::with(|gpu| {
            let mut renderer = crate::graphics::Renderer::new(
                &gpu.device,
                &gpu.queue,
                wgpu::TextureFormat::Rgba8UnormSrgb,
                64,
                48,
            );
            renderer.update_arrival(
                &gpu.device,
                &gpu.queue,
                &Frame {
                    scene: &scene,
                    hovered: None,
                    left_ray: None,
                    ray: None,
                    time: 0.0,
                },
            );
            assert_eq!(renderer.arrival_label_capacity(), MAX_VERTICES);
        });
    }
}
