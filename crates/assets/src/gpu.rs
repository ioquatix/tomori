// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

//! Drawing models with wgpu: a [`Renderer`] uploads each model once, as a [`GpuModel`], and draws a [`Batch`] of its
//! instances in an eye's pass, one draw for every instance of each primitive, skinned on the GPU from each instance's
//! joint matrices, which a storage buffer holds, or placed by its node's matrix, lit by the scenery's [`Lighting`].
//!
//! Once a frame, [`Renderer::cull`] keeps only the instances of each primitive that the eyes' frustum may see, and a
//! closed primitive, [`Primitive::closed`](crate::Primitive::closed), is drawn without its back faces; and
//! [`Renderer::light`] lists the few lamps that light each piece, as a render world assigns them, which its pixels then
//! add up, rather than every lamp. See `design/rendering.md`.
use std::ops::Range;

use bytemuck::{Pod, Zeroable};
use glam::{Mat4, Vec3, Vec4};
use wgpu::util::DeviceExt;

use crate::bounds::{self, Aabb, Extent, Frustum, Tally};
use crate::{INFLUENCES, Instance, Model, Texture};

/// The most lamps a scenery's [`Lighting`] holds.
pub const LAMPS: usize = 64;

/// The most lamps a piece is lit by when [`Renderer::light`] lists its lamps.
pub const PIECE_LAMPS: usize = 8;

/// A piece lit by every lamp, as [`lamps`] packs a list and the shader reads it: every lamp in its mask, more than a
/// list holds.
pub const EVERY_LAMP: [u32; 4] = [u32::MAX; 4];

/// `list`'s lamps, each its index in the lighting's lamps and how much of its light it gives, as the model shader reads
/// them, so that a scenery's own pipelines may light by the same lists: a mask of the lamps, by index, its low and high
/// halves, then how much of its light each gives, a byte each, 255 for all of it, in the mask's order, the first four
/// then the next, and 255 past the last, so that a list none of whose lamps fade has every byte 255, which a pixel
/// tells at once. Of `list`, at most [`PIECE_LAMPS`] are packed, each once, those past [`LAMPS`] and those giving
/// nothing left out. A pixel then adds the light of each lamp in the mask, in the lamps' order, which, as `list` is in
/// the lighting's order, is as it adds every lamp, but for those that reach none of it.
pub fn lamps(list: impl IntoIterator<Item = (u32, f32)>) -> [u32; 4] {
    let mut kept = [(0u32, 0u8); PIECE_LAMPS];
    let mut count = 0;
    let mut mask = 0u64;
    for (lamp, weight) in list.into_iter().take(PIECE_LAMPS) {
        let weight = (weight.clamp(0.0, 1.0) * 255.0).round() as u8;
        if lamp as usize >= LAMPS || weight == 0 || mask & 1 << lamp != 0 {
            continue;
        }
        mask |= 1 << lamp;
        kept[count] = (lamp, weight);
        count += 1;
    }
    // In the mask's order.
    kept[..count].sort_unstable_by_key(|(lamp, _)| *lamp);
    let mut weights = u64::MAX;
    for (at, (_, weight)) in kept[..count].iter().enumerate() {
        weights &= !(0xff << (8 * at)) | u64::from(*weight) << (8 * at);
    }
    [
        mask as u32,
        (mask >> 32) as u32,
        weights as u32,
        (weights >> 32) as u32,
    ]
}

/// A lamp: where it is, how far its light reaches, falling to nothing there, and its colour, as bright as at its
/// centre, linear RGB.
#[derive(Clone, Copy, Debug, PartialEq)]
#[non_exhaustive]
pub struct Lamp {
    pub position: Vec3,
    pub reach: f32,
    pub color: Vec3,
}

impl Lamp {
    pub fn new(position: Vec3, reach: f32, color: Vec3) -> Self {
        Self {
            position,
            reach,
            color,
        }
    }
}

/// How a scenery lights its models: an ambient light from the sky above and the ground below, the sun or moon, up to
/// [`LAMPS`] lamps, each piece lit by those [`Renderer::light`] lists for it, or by every one, and haze that fades
/// distant things into its colour. Colours are linear RGB.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub struct Lighting {
    /// The direction towards the sun or moon.
    pub sun_direction: Vec3,
    pub sun: Vec3,
    pub sky: Vec3,
    pub ground: Vec3,
    pub haze: Vec3,
    /// How thick the haze is: what is `d` metres away fades by `1 - exp(-density d²)`.
    pub haze_density: f32,
    pub lamps: Vec<Lamp>,
}

impl Lighting {
    /// Light from the sun or moon, `sun`, coming from `sun_direction`, and ambient light from the sky above, `sky`, and
    /// the ground below, `ground`, without haze or lamps.
    pub fn new(sun_direction: Vec3, sun: Vec3, sky: Vec3, ground: Vec3) -> Self {
        Self {
            sun_direction,
            sun,
            sky,
            ground,
            ..Self::default()
        }
    }

    /// This light, with haze of `color`, `density` thick.
    pub fn with_haze(mut self, color: Vec3, density: f32) -> Self {
        self.haze = color;
        self.haze_density = density;
        self
    }

    /// This light, with `lamp` too; past [`LAMPS`], lamps are not drawn.
    pub fn with_lamp(mut self, lamp: Lamp) -> Self {
        self.lamps.push(lamp);
        self
    }
}

impl From<&Lighting> for crate::haze::Haze {
    /// The haze `lighting` lights its models with, which the rest of the scene is hazed by too.
    fn from(lighting: &Lighting) -> Self {
        Self::new(lighting.haze, lighting.haze_density)
    }
}

impl Default for Lighting {
    /// A plain, even light from above, without haze or lamps.
    fn default() -> Self {
        Self {
            sun_direction: Vec3::new(0.3, 0.8, 0.5).normalize(),
            sun: Vec3::splat(0.6),
            sky: Vec3::splat(0.4),
            ground: Vec3::splat(0.2),
            haze: Vec3::ZERO,
            haze_density: 0.0,
            lamps: Vec::new(),
        }
    }
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct CameraUniform {
    view_projection: [f32; 16],
    eye: [f32; 4],
    reveal: [f32; 4],
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct LampUniform {
    position: [f32; 4],
    color: [f32; 4],
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct LightingUniform {
    sun_direction: [f32; 4],
    sun: [f32; 4],
    sky: [f32; 4],
    ground: [f32; 4],
    haze: [f32; 4],
    lamps: [LampUniform; LAMPS],
    count: [u32; 4],
}

impl From<&Lighting> for LightingUniform {
    fn from(lighting: &Lighting) -> Self {
        let mut lamps = [LampUniform::zeroed(); LAMPS];
        for (slot, lamp) in lamps.iter_mut().zip(&lighting.lamps) {
            *slot = LampUniform {
                position: lamp.position.extend(lamp.reach).to_array(),
                color: lamp.color.extend(1.0).to_array(),
            };
        }
        Self {
            sun_direction: lighting
                .sun_direction
                .normalize_or_zero()
                .extend(0.0)
                .to_array(),
            sun: lighting.sun.extend(0.0).to_array(),
            sky: lighting.sky.extend(0.0).to_array(),
            ground: lighting.ground.extend(0.0).to_array(),
            haze: crate::haze::Haze::from(lighting).to_array(),
            lamps,
            count: [lighting.lamps.len().min(LAMPS) as u32, 0, 0, 0],
        }
    }
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct MaterialUniform {
    base_color: [f32; 4],
    emissive: [f32; 4],
}

/// One piece as its shader reads it: where its node is; where its joint matrices begin and whether it is skinned; and
/// its lamps, as [`lamps`] packs them, or every lamp, [`EVERY_LAMP`].
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct Placed {
    world: [f32; 16],
    joints: [u32; 4],
    lamps: [u32; 4],
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct Vertex {
    position: [f32; 3],
    normal: [f32; 3],
    uv: [f32; 2],
    color: [f32; 4],
    joints: [u16; INFLUENCES],
    weights: [f32; INFLUENCES],
}

/// A primitive on the GPU, with how far it reaches: where its vertices and its indices begin in its model's buffers,
/// its triangles in closed parts first, its material hiding what is behind them, then the rest.
struct GpuPrimitive {
    base: i32,
    first: u32,
    count: u32,
    /// How many of its indices are of triangles in closed parts, which may be drawn without their back faces.
    closed: u32,
    material: Option<usize>,
    extent: Extent,
}

/// A model on the GPU: every primitive's vertices in one buffer and indices in another, so that drawing one after
/// another, as the chunks of a scenery's ground are, changes neither; its primitives, by mesh; and its materials with
/// their textures.
pub struct GpuModel {
    vertices: wgpu::Buffer,
    indices: wgpu::Buffer,
    primitives: Vec<GpuPrimitive>,
    meshes: Vec<Range<usize>>,
    materials: Vec<wgpu::BindGroup>,
    plain: wgpu::BindGroup,
}

/// What a batch draws: each primitive with its range of placed instances, and whether the triangles of its closed parts
/// may be drawn without their back faces.
struct Call {
    primitive: usize,
    instances: Range<u32>,
    /// How many of the primitive's indices, first, are of closed parts, drawn without their back faces: none for a
    /// mirrored instance.
    closed: u32,
    /// How many indices the primitive has.
    count: u32,
}

/// One primitive of one instance as placed: where, where in the scene it may be drawn, if that is known, and whether its
/// closed parts may be drawn without their back faces, as they may unless it is mirrored.
struct Piece {
    primitive: usize,
    placed: Placed,
    bounds: Option<Aabb>,
    closed: bool,
}

/// The instances of one model to draw this frame: where each primitive of each is placed, and its joint matrices; and,
/// as last culled, which of them are drawn, and the lamps that light each.
pub struct Batch {
    placed: wgpu::Buffer,
    joints: wgpu::Buffer,
    binding: wgpu::BindGroup,
    calls: Vec<Call>,
    /// Every piece placed, by primitive, those with closed parts drawn without their back faces before the rest of each.
    pieces: Vec<Piece>,
    /// Which pieces the placements' buffer holds now.
    shown: Vec<bool>,
    /// Which pieces a cull would draw, and the placements written, kept between frames so that culling allocates
    /// nothing.
    wanted: Vec<bool>,
    flat: Vec<Placed>,
    /// Where any piece may be drawn, or none if one of them may be drawn anywhere.
    bounds: Option<Aabb>,
    /// Each piece's lamps, as last lit, and as a light would light them, kept so that lighting allocates nothing.
    lamps: Vec<[u32; 4]>,
    next_lamps: Vec<[u32; 4]>,
    /// Whether the lamps changed since the placements were written.
    relit: bool,
}

impl Batch {
    /// Where each piece placed, each primitive of each instance, may be drawn in the scene, or none for anywhere, in the
    /// order [`Renderer::show`] takes which are seen in, as a render world culls them.
    pub fn bounds(&self) -> impl ExactSizeIterator<Item = Option<Aabb>> + '_ {
        self.pieces.iter().map(|piece| piece.bounds)
    }

    /// What it draws in each eye as last placed or culled: its pieces drawn and culled, its draws and their triangles.
    pub fn tally(&self) -> Tally {
        let drawn = self.shown.iter().filter(|shown| **shown).count();
        Tally {
            drawn,
            culled: self.pieces.len() - drawn,
            draws: self
                .calls
                .iter()
                .map(|call| usize::from(call.closed > 0) + usize::from(call.closed < call.count))
                .sum(),
            triangles: self
                .calls
                .iter()
                .map(|call| call.instances.len() * call.count as usize / 3)
                .sum(),
        }
    }
}

/// Draws models in an eye's pass, into targets of the format and sample count it was made for, with a 32-bit float
/// depth buffer.
pub struct Renderer {
    pipeline: wgpu::RenderPipeline,
    /// As `pipeline`, without back faces, for closed primitives.
    closed: wgpu::RenderPipeline,
    revealed: wgpu::RenderPipeline,
    /// Whether closed primitives are drawn without their back faces, as they are unless a comparison turns it off.
    back_faces_culled: bool,
    material_layout: wgpu::BindGroupLayout,
    placed_layout: wgpu::BindGroupLayout,
    /// Each eye's camera: a buffer for each where each eye has a pass of its own, or one holding both.
    cameras: Vec<wgpu::Buffer>,
    lighting: wgpu::Buffer,
    /// The cameras' binding for each pass, with the lighting.
    frames: Vec<wgpu::BindGroup>,
    views: crate::Views,
    sampler: wgpu::Sampler,
    white: wgpu::TextureView,
    reveal: Option<(Vec3, f32)>,
}

fn uniform_entry(binding: u32, visibility: wgpu::ShaderStages) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility,
        ty: wgpu::BindingType::Buffer {
            ty: wgpu::BufferBindingType::Uniform,
            has_dynamic_offset: false,
            min_binding_size: None,
        },
        count: None,
    }
}

fn storage_entry(binding: u32) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::VERTEX,
        ty: wgpu::BindingType::Buffer {
            ty: wgpu::BufferBindingType::Storage { read_only: true },
            has_dynamic_offset: false,
            min_binding_size: None,
        },
        count: None,
    }
}

/// `texture`'s mipmaps, from its own size down to one texel, each averaging up to four texels of the last in linear
/// light, so that a distant model does not shimmer.
pub(crate) fn mipmaps(texture: &Texture) -> Vec<(u32, u32, Vec<u8>)> {
    let to_linear = |value: u8| (f32::from(value) / 255.0).powf(2.2);
    let mut levels = vec![(texture.width, texture.height, texture.pixels.clone())];
    let mut level: Vec<f32> = texture
        .pixels
        .iter()
        .enumerate()
        .map(|(index, value)| {
            if index % 4 == 3 {
                f32::from(*value) / 255.0
            } else {
                to_linear(*value)
            }
        })
        .collect();
    let (mut width, mut height) = (texture.width as usize, texture.height as usize);
    while width > 1 || height > 1 {
        let (next_width, next_height) = ((width / 2).max(1), (height / 2).max(1));
        let mut next = vec![0.0; next_width * next_height * 4];
        for y in 0..next_height {
            for x in 0..next_width {
                for channel in 0..4 {
                    let mut sum = 0.0;
                    for (dx, dy) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
                        let sx = (x * 2 + dx).min(width - 1);
                        let sy = (y * 2 + dy).min(height - 1);
                        sum += level[(sy * width + sx) * 4 + channel];
                    }
                    next[(y * next_width + x) * 4 + channel] = sum / 4.0;
                }
            }
        }
        let bytes = next
            .iter()
            .enumerate()
            .map(|(index, value)| {
                let value = if index % 4 == 3 {
                    *value
                } else {
                    value.powf(1.0 / 2.2)
                };
                (value * 255.0).round() as u8
            })
            .collect();
        levels.push((next_width as u32, next_height as u32, bytes));
        level = next;
        (width, height) = (next_width, next_height);
    }
    levels
}

fn upload_texture(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    texture: &Texture,
) -> wgpu::TextureView {
    let levels = mipmaps(texture);
    let gpu = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("Model texture"),
        size: wgpu::Extent3d {
            width: texture.width,
            height: texture.height,
            depth_or_array_layers: 1,
        },
        mip_level_count: levels.len() as u32,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8UnormSrgb,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    for (level, (width, height, pixels)) in levels.iter().enumerate() {
        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &gpu,
                mip_level: level as u32,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            pixels,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(width * 4),
                rows_per_image: Some(*height),
            },
            wgpu::Extent3d {
                width: *width,
                height: *height,
                depth_or_array_layers: 1,
            },
        );
    }
    gpu.create_view(&Default::default())
}

impl Renderer {
    /// A renderer for targets of `format` with `samples` samples a pixel, each eye drawn in a pass of its own.
    pub fn new(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        format: wgpu::TextureFormat,
        samples: u32,
    ) -> Self {
        Self::with_views(device, queue, format, samples, crate::Views::One)
    }

    /// As [`Self::new`], drawing `views` a pass: with [`Views::Both`](crate::Views::Both), both eyes at once by
    /// multiview. See `design/rendering.md`.
    pub fn with_views(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        format: wgpu::TextureFormat,
        samples: u32,
        views: crate::Views,
    ) -> Self {
        let both = wgpu::ShaderStages::VERTEX_FRAGMENT;
        let frame_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("Model frame layout"),
            entries: &[
                uniform_entry(0, both),
                uniform_entry(1, wgpu::ShaderStages::FRAGMENT),
            ],
        });
        let material_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("Model material layout"),
            entries: &[
                uniform_entry(0, wgpu::ShaderStages::FRAGMENT),
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });
        let placed_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("Model placement layout"),
            entries: &[storage_entry(0), storage_entry(1)],
        });
        let cameras: Vec<_> = (0..views.passes(2))
            .map(|_| {
                device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("Model eye camera"),
                    size: (size_of::<CameraUniform>() * views.count()) as u64,
                    usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                })
            })
            .collect();
        let lighting = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("Model lighting"),
            contents: bytemuck::bytes_of(&LightingUniform::from(&Lighting::default())),
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        });
        let frames = (0..cameras.len())
            .map(|eye| {
                device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some("Model frame"),
                    layout: &frame_layout,
                    entries: &[
                        wgpu::BindGroupEntry {
                            binding: 0,
                            resource: cameras[eye].as_entire_binding(),
                        },
                        wgpu::BindGroupEntry {
                            binding: 1,
                            resource: lighting.as_entire_binding(),
                        },
                    ],
                })
            })
            .collect();
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("model.wgsl"),
            source: wgpu::ShaderSource::Wgsl(
                views
                    .shader(&crate::haze::shader(include_str!("model.wgsl")))
                    .into(),
            ),
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("Model pipeline layout"),
            bind_group_layouts: &[
                Some(&frame_layout),
                Some(&material_layout),
                Some(&placed_layout),
            ],
            immediate_size: 0,
        });
        let attributes = wgpu::vertex_attr_array![
            0 => Float32x3, 1 => Float32x3, 2 => Float32x2, 3 => Float32x4,
            4 => Uint16x4, 5 => Uint16x4, 6 => Float32x4, 7 => Float32x4
        ];
        let pipeline = |fragment: &str, cull_mode: Option<wgpu::Face>| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("Model pipeline"),
                layout: Some(&layout),
                vertex: wgpu::VertexState {
                    module: &shader,
                    entry_point: Some("model_vertex"),
                    compilation_options: Default::default(),
                    buffers: &[Some(wgpu::VertexBufferLayout {
                        array_stride: size_of::<Vertex>() as u64,
                        step_mode: wgpu::VertexStepMode::Vertex,
                        attributes: &attributes,
                    })],
                },
                fragment: Some(wgpu::FragmentState {
                    module: &shader,
                    entry_point: Some(fragment),
                    compilation_options: Default::default(),
                    targets: &[Some(wgpu::ColorTargetState {
                        format,
                        blend: None,
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                }),
                primitive: wgpu::PrimitiveState {
                    cull_mode,
                    ..Default::default()
                },
                depth_stencil: Some(wgpu::DepthStencilState {
                    format: wgpu::TextureFormat::Depth32Float,
                    depth_write_enabled: Some(true),
                    depth_compare: Some(wgpu::CompareFunction::Less),
                    stencil: Default::default(),
                    bias: Default::default(),
                }),
                multisample: wgpu::MultisampleState {
                    count: samples,
                    ..Default::default()
                },
                multiview_mask: views.mask(),
                cache: None,
            })
        };
        let white = upload_texture(
            device,
            queue,
            &Texture {
                width: 1,
                height: 1,
                pixels: vec![255; 4],
            },
        );
        Self {
            pipeline: pipeline("model_fragment", None),
            closed: pipeline("model_fragment", Some(wgpu::Face::Back)),
            // Cut off by the reveal sphere, a closed primitive is open: what is inside it shows.
            revealed: pipeline("revealed_model_fragment", None),
            back_faces_culled: true,
            material_layout,
            placed_layout,
            cameras,
            lighting,
            frames,
            views,
            sampler: device.create_sampler(&wgpu::SamplerDescriptor {
                label: Some("Model sampler"),
                address_mode_u: wgpu::AddressMode::Repeat,
                address_mode_v: wgpu::AddressMode::Repeat,
                mag_filter: wgpu::FilterMode::Linear,
                min_filter: wgpu::FilterMode::Linear,
                mipmap_filter: wgpu::MipmapFilterMode::Linear,
                ..Default::default()
            }),
            white,
            reveal: None,
        }
    }

    fn material(
        &self,
        device: &wgpu::Device,
        uniform: MaterialUniform,
        texture: &wgpu::TextureView,
    ) -> wgpu::BindGroup {
        let buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("Model material"),
            contents: bytemuck::bytes_of(&uniform),
            usage: wgpu::BufferUsages::UNIFORM,
        });
        device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("Model material"),
            layout: &self.material_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(texture),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::Sampler(&self.sampler),
                },
            ],
        })
    }

    /// Upload `model`, once, to draw any number of its instances.
    pub fn upload(&self, device: &wgpu::Device, queue: &wgpu::Queue, model: &Model) -> GpuModel {
        let textures: Vec<_> = model
            .textures
            .iter()
            .map(|texture| upload_texture(device, queue, texture))
            .collect();
        let materials = model
            .materials
            .iter()
            .map(|material| {
                let uniform = MaterialUniform {
                    base_color: material.base_color.to_array(),
                    emissive: material
                        .emissive
                        .extend(f32::from(u8::from(material.masked)))
                        .to_array(),
                };
                let texture = material
                    .texture
                    .map_or(&self.white, |texture| &textures[texture]);
                self.material(device, uniform, texture)
            })
            .collect();
        let mut primitives = Vec::new();
        let (mut all_vertices, mut all_indices): (Vec<Vertex>, Vec<u32>) = (Vec::new(), Vec::new());
        let meshes = model
            .meshes
            .iter()
            .map(|mesh| {
                let start = primitives.len();
                for primitive in &mesh.primitives {
                    let vertices: Vec<Vertex> = (0..primitive.positions.len())
                        .map(|index| Vertex {
                            position: primitive.positions[index].to_array(),
                            normal: primitive
                                .normals
                                .get(index)
                                .copied()
                                .unwrap_or(Vec3::ZERO)
                                .to_array(),
                            uv: primitive
                                .uvs
                                .get(index)
                                .copied()
                                .unwrap_or_default()
                                .to_array(),
                            color: primitive
                                .colors
                                .get(index)
                                .copied()
                                .unwrap_or(Vec4::ONE)
                                .to_array(),
                            joints: primitive.joints.get(index).copied().unwrap_or_default(),
                            weights: primitive.weights.get(index).copied().unwrap_or_default(),
                        })
                        .collect();
                    // The triangles of its closed parts first, if its material hides what is behind them.
                    let closed = if bounds::opaque(model, primitive.material) {
                        primitive.closed_triangles()
                    } else {
                        vec![false; primitive.indices.len() / 3]
                    };
                    let (inside, outside): (Vec<_>, Vec<_>) = primitive
                        .indices
                        .as_chunks::<3>()
                        .0
                        .iter()
                        .zip(&closed)
                        .partition(|(_, closed)| **closed);
                    let indices: Vec<u32> = inside
                        .iter()
                        .chain(&outside)
                        .flat_map(|(triangle, _)| triangle.iter().copied())
                        .collect();
                    primitives.push(GpuPrimitive {
                        extent: Extent::of(primitive),
                        closed: inside.len() as u32 * 3,
                        base: all_vertices.len() as i32,
                        first: all_indices.len() as u32,
                        count: primitive.indices.len() as u32,
                        material: primitive.material,
                    });
                    all_vertices.extend(vertices);
                    all_indices.extend(indices);
                }
                start..primitives.len()
            })
            .collect();
        let plain = MaterialUniform {
            base_color: [1.0; 4],
            emissive: [0.0; 4],
        };
        // Never empty, as a buffer bound must not be.
        all_vertices.resize(all_vertices.len().max(1), Vertex::zeroed());
        all_indices.resize(all_indices.len().max(1), 0);
        GpuModel {
            vertices: device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("Model vertices"),
                contents: bytemuck::cast_slice(&all_vertices),
                usage: wgpu::BufferUsages::VERTEX,
            }),
            indices: device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("Model indices"),
                contents: bytemuck::cast_slice(&all_indices),
                usage: wgpu::BufferUsages::INDEX,
            }),
            primitives,
            meshes,
            materials,
            plain: self.material(device, plain, &self.white),
        }
    }

    /// A batch to draw instances with.
    pub fn batch(&self, device: &wgpu::Device) -> Batch {
        self.sized_batch(device, 1, 1)
    }

    fn sized_batch(&self, device: &wgpu::Device, placed: usize, joints: usize) -> Batch {
        let buffer = |label, bytes: usize| {
            device.create_buffer(&wgpu::BufferDescriptor {
                label: Some(label),
                size: bytes as u64,
                usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            })
        };
        let placed = buffer("Model placements", placed * size_of::<Placed>());
        let joints = buffer("Model joints", joints * size_of::<[f32; 16]>());
        let binding = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("Model placements"),
            layout: &self.placed_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: placed.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: joints.as_entire_binding(),
                },
            ],
        });
        Batch {
            placed,
            joints,
            binding,
            calls: Vec::new(),
            pieces: Vec::new(),
            shown: Vec::new(),
            wanted: Vec::new(),
            flat: Vec::new(),
            bounds: Some(Aabb::EMPTY),
            lamps: Vec::new(),
            next_lamps: Vec::new(),
            relit: false,
        }
    }

    /// Place `instances`, each of `model`, in `batch`, to be drawn as they are now, each lit by every lamp until
    /// [`Self::light`] lists its lamps, growing its buffers if need be.
    ///
    /// # Panics
    ///
    /// If an instance is not of the model `model` was uploaded from.
    pub fn place(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        model: &GpuModel,
        batch: &mut Batch,
        instances: &[Instance],
    ) {
        // Each primitive's pieces, the closed apart from the rest, so that each draws with its own pipeline.
        let mut pieces: Vec<[Vec<Piece>; 2]> = (0..model.primitives.len())
            .map(|_| [Vec::new(), Vec::new()])
            .collect();
        let mut joints: Vec<[f32; 16]> = Vec::new();
        for instance in instances {
            for draw in instance.draws() {
                let first = joints.len() as u32;
                let skinned = draw.joints.as_ref().map_or(0, |matrices| {
                    joints.extend(matrices.iter().map(Mat4::to_cols_array));
                    1
                });
                let mirrored = bounds::mirrored(&draw);
                for primitive in model.meshes[draw.mesh].clone() {
                    let gpu = &model.primitives[primitive];
                    let closed = gpu.closed > 0 && !mirrored;
                    pieces[primitive][usize::from(!closed)].push(Piece {
                        primitive,
                        placed: Placed {
                            world: draw.world.to_cols_array(),
                            joints: [first, skinned, 0, 0],
                            lamps: EVERY_LAMP,
                        },
                        bounds: gpu.extent.placed(&draw),
                        closed,
                    });
                }
            }
        }
        let pieces: Vec<Piece> = pieces.into_iter().flatten().flatten().collect();
        let placed_bytes = (pieces.len().max(1) * size_of::<Placed>()) as u64;
        let joint_bytes = (joints.len().max(1) * size_of::<[f32; 16]>()) as u64;
        if placed_bytes > batch.placed.size() || joint_bytes > batch.joints.size() {
            *batch = self.sized_batch(
                device,
                pieces.len().next_power_of_two(),
                joints.len().max(1).next_power_of_two(),
            );
        }
        queue.write_buffer(&batch.joints, 0, bytemuck::cast_slice(&joints));
        batch.bounds = pieces.iter().try_fold(Aabb::EMPTY, |bounds, piece| {
            Some(bounds.union(piece.bounds?))
        });
        batch.pieces = pieces;
        batch.wanted.clear();
        batch.wanted.resize(batch.pieces.len(), true);
        batch.lamps.clear();
        batch.lamps.resize(batch.pieces.len(), EVERY_LAMP);
        self.write(queue, model, batch);
    }

    /// Light each piece of `batch` by the lamps of the lighting `lamps` lists for it, each by its index and how much of
    /// its light it gives, as [`lamps`] packs them, in the order [`Batch::bounds`] says the pieces, as a render world
    /// lights them, from the next [`Self::cull`] or [`Self::show`]; or each by every lamp, should `lamps` not list as
    /// many as it placed. Lighting as before writes nothing.
    pub fn light<L>(&self, batch: &mut Batch, lamps: impl IntoIterator<Item = L>)
    where
        L: IntoIterator<Item = (u32, f32)>,
    {
        let Batch {
            pieces,
            lamps: lit,
            next_lamps,
            relit,
            ..
        } = batch;
        next_lamps.clear();
        next_lamps.extend(lamps.into_iter().take(pieces.len() + 1).map(self::lamps));
        if next_lamps.len() != pieces.len() {
            next_lamps.clear();
            next_lamps.resize(pieces.len(), EVERY_LAMP);
        }
        if next_lamps != lit {
            std::mem::swap(lit, next_lamps);
            *relit = true;
        }
    }

    /// Write the pieces of `batch` that it wants drawn into its placements' buffer, each primitive's together, with their
    /// lamps, and draw those.
    fn write(&self, queue: &wgpu::Queue, model: &GpuModel, batch: &mut Batch) {
        let Batch {
            calls,
            flat,
            pieces,
            wanted,
            shown,
            lamps,
            relit,
            ..
        } = batch;
        calls.clear();
        flat.clear();
        for ((piece, _), lamps) in pieces
            .iter()
            .zip(wanted.iter())
            .zip(lamps.iter())
            .filter(|((_, wanted), _)| **wanted)
        {
            let at = flat.len() as u32;
            flat.push(Placed {
                lamps: *lamps,
                ..piece.placed
            });
            match calls.last_mut() {
                Some(call)
                    if call.primitive == piece.primitive && (call.closed > 0) == piece.closed =>
                {
                    call.instances.end = at + 1;
                }
                _ => {
                    let primitive = &model.primitives[piece.primitive];
                    calls.push(Call {
                        primitive: piece.primitive,
                        instances: at..at + 1,
                        closed: if piece.closed { primitive.closed } else { 0 },
                        count: primitive.count,
                    });
                }
            }
        }
        if !flat.is_empty() {
            queue.write_buffer(&batch.placed, 0, bytemuck::cast_slice(flat));
        }
        shown.clone_from(wanted);
        *relit = false;
    }

    /// Draw only the instances of `batch`'s primitives that `frustum` may see, from the next pass, writing their
    /// placements only when which are seen changes; with none, every one. What it then draws in each eye is its
    /// [`Batch::tally`].
    pub fn cull(
        &self,
        queue: &wgpu::Queue,
        model: &GpuModel,
        batch: &mut Batch,
        frustum: Option<&Frustum>,
    ) -> Tally {
        let Batch {
            pieces,
            wanted,
            bounds,
            ..
        } = batch;
        wanted.clear();
        match frustum {
            // Nothing of it is in view at all.
            Some(frustum) if bounds.is_some_and(|bounds| !frustum.sees(&bounds)) => {
                wanted.resize(pieces.len(), false);
            }
            Some(frustum) => wanted.extend(
                pieces
                    .iter()
                    .map(|piece| piece.bounds.is_none_or(|bounds| frustum.sees(&bounds))),
            ),
            None => wanted.resize(pieces.len(), true),
        }
        self.redraw(queue, model, batch)
    }

    /// Draw only the pieces of `batch` that `seen` says, as a render world culled what [`Batch::bounds`] said of them,
    /// from the next pass, writing their placements only when which are seen changes; or every one, should `seen` not
    /// say of as many as it placed. What it then draws in each eye is its [`Batch::tally`].
    pub fn show(
        &self,
        queue: &wgpu::Queue,
        model: &GpuModel,
        batch: &mut Batch,
        seen: &[bool],
    ) -> Tally {
        batch.wanted.clear();
        if seen.len() == batch.pieces.len() {
            batch.wanted.extend_from_slice(seen);
        } else {
            batch.wanted.resize(batch.pieces.len(), true);
        }
        self.redraw(queue, model, batch)
    }

    /// Write `batch`'s placements again if it wants other pieces drawn than it does, or they are lit by other lamps, and
    /// say what it then draws.
    fn redraw(&self, queue: &wgpu::Queue, model: &GpuModel, batch: &mut Batch) -> Tally {
        if batch.wanted != batch.shown || batch.relit {
            self.write(queue, model, batch);
        }
        batch.tally()
    }

    /// Draw closed primitives without their back faces, as by default, or with them, `false`, to compare.
    pub fn set_back_faces_culled(&mut self, culled: bool) {
        self.back_faces_culled = culled;
    }

    /// Light every model as `lighting` says, from the next pass.
    pub fn set_lighting(&self, queue: &wgpu::Queue, lighting: &Lighting) {
        queue.write_buffer(
            &self.lighting,
            0,
            bytemuck::bytes_of(&LightingUniform::from(lighting)),
        );
    }

    /// Draw models only inside a sphere, `(centre, radius)` in the scene, as a scenery grows out of a point, or
    /// everywhere, with `None`; applied by the next [`Self::prepare`].
    pub fn set_reveal(&mut self, reveal: Option<(Vec3, f32)>) {
        self.reveal = reveal.filter(|(centre, radius)| centre.is_finite() && radius.is_finite());
    }

    /// Before eye `eye`'s pass, or the pass that draws both: its view and projection, and where the eye is.
    pub fn prepare(&self, queue: &wgpu::Queue, eye: usize, view_projection: Mat4, position: Vec3) {
        let camera = CameraUniform {
            view_projection: view_projection.to_cols_array(),
            eye: position.extend(1.0).to_array(),
            reveal: match self.reveal {
                Some((centre, radius)) => centre.extend(radius.max(0.0)).to_array(),
                None => [0.0, 0.0, 0.0, -1.0],
            },
        };
        let (buffer, offset) = match self.views {
            crate::Views::One => (eye, 0),
            crate::Views::Both => (0, (eye * size_of::<CameraUniform>()) as u64),
        };
        queue.write_buffer(&self.cameras[buffer], offset, bytemuck::bytes_of(&camera));
    }

    /// In eye `eye`'s pass, or with [`Views::Both`](crate::Views::Both) the pass that draws both, as `eye` 0: draw
    /// `batch`'s instances of `model`.
    pub fn draw(
        &self,
        pass: &mut wgpu::RenderPass<'_>,
        eye: usize,
        model: &GpuModel,
        batch: &Batch,
    ) {
        if batch.calls.is_empty() {
            return;
        }
        pass.set_bind_group(0, &self.frames[eye], &[]);
        pass.set_bind_group(2, &batch.binding, &[]);
        pass.set_vertex_buffer(0, model.vertices.slice(..));
        pass.set_index_buffer(model.indices.slice(..), wgpu::IndexFormat::Uint32);
        // Cut off by the reveal sphere, a closed part is open: what is inside it shows.
        let culled = self.back_faces_culled && self.reveal.is_none();
        // Each material bound only as it changes, which it does not between the chunks of a primitive.
        let mut bound = None;
        let mut draw = |pass: &mut wgpu::RenderPass<'_>, call: &Call, indices: Range<u32>| {
            let primitive = &model.primitives[call.primitive];
            if bound != Some(primitive.material) {
                let material = primitive
                    .material
                    .map_or(&model.plain, |material| &model.materials[material]);
                pass.set_bind_group(1, material, &[]);
                bound = Some(primitive.material);
            }
            pass.draw_indexed(
                primitive.first + indices.start..primitive.first + indices.end,
                primitive.base,
                call.instances.clone(),
            );
        };
        // The closed parts without their back faces, then the rest with them.
        let closed = |call: &Call| if culled { call.closed } else { 0 };
        if batch.calls.iter().any(|call| closed(call) > 0) {
            pass.set_pipeline(&self.closed);
            for call in batch.calls.iter().filter(|call| closed(call) > 0) {
                draw(pass, call, 0..closed(call));
            }
        }
        pass.set_pipeline(if self.reveal.is_some() {
            &self.revealed
        } else {
            &self.pipeline
        });
        for call in batch.calls.iter().filter(|call| closed(call) < call.count) {
            draw(pass, call, closed(call)..call.count);
        }
    }
}

#[cfg(test)]
mod tests;
