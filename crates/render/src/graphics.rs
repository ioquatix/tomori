// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

use crate::{
    controller::{Model as ControllerModel, Pose},
    controller_graphics::Controllers,
    environment::Environment,
    hands::Hand,
    interaction::Mode,
    swarm::{AGENT_COUNT, Swarm},
};
use bytemuck::{Pod, Zeroable};
use glam::Mat4;
use tomori_assets::haze::Haze;
use wgpu::util::DeviceExt;

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
pub(crate) struct Vertex {
    pub(crate) position: [f32; 3],
    pub(crate) normal: [f32; 3],
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
pub(crate) struct Instance {
    pub(crate) position: [f32; 3],
    pub(crate) scale: [f32; 3],
    pub(crate) color: [f32; 3],
    pub(crate) basis: [[f32; 3]; 3],
    /// How much of its colour is its own light, unshaded by the soft light from above the rest of Tomori's cubes are
    /// drawn with: the others' swarms glow (see `design/people.md`).
    pub(crate) glow: f32,
}

impl Default for Instance {
    fn default() -> Self {
        Self {
            position: [0.0; 3],
            scale: [1.0; 3],
            color: [1.0; 3],
            basis: glam::Mat3::IDENTITY.to_cols_array_2d(),
            glow: 0.0,
        }
    }
}

/// How many of the others in a space are drawn at most, and how many cubes they are drawn with at most, after the rest
/// of Tomori's cubes, in the same draw. See `design/people.md`.
pub const MOST_PEOPLE: usize = 16;
const PEOPLE_INSTANCES: usize = MOST_PEOPLE * tomori_scene::people::CUBES;
/// How much of the others' cubes' colour is their own light, so that their swarms glow, as a volume of light, rather
/// than stand shaded as solids: nearly all of it, keeping a hint of their faces so that they still read as cubes.
pub const PEOPLE_GLOW: f32 = 0.85;

// Workspace cubes are shown inside a space and arrival cubes outside one, but either may follow the swarm. The grid takes up to twice its lines while an environment grows through it.
const MAX_INSTANCES: usize = AGENT_COUNT
    + 210
    + LANDING_CUBES
    + PAD_CUBES * MOST_PADS
    + if crate::delegation::MAX_CUBES > crate::arrival_graphics::MAX_CUBES {
        crate::delegation::MAX_CUBES
    } else {
        crate::arrival_graphics::MAX_CUBES
    };
/// How many cubes make the ring on the floor at home, its colour, and the share of its brightness it keeps while the
/// agent is away from it: most of it, since the ring marks home, where the workers stand, whether or not the agent is
/// there, and only a little dimmer says it is away.
pub const HOME_RING_CUBES: usize = 40;
pub const HOME_RING_COLOUR: [f32; 3] = [0.08, 0.5, 0.65];
pub const HOME_RING_AWAY: f32 = 0.85;

/// The widest ring on the floor at home drawn, in metres; one wider is drawn beneath the swarm.
pub const MOST_HOME_RING: f32 = 4.0;

/// How far above the floor the ring on the floor stands, the middles of its cubes, in metres: just enough that the
/// ground beneath never shows through it, as the floor's other markings stand.
pub const FLOOR_LIFT: f32 = 0.015;

/// How many cubes make the ring drawn where a teleport being aimed would land.
const LANDING_CUBES: usize = 16;
/// The ring's radius, in metres.
const LANDING_RADIUS: f32 = 0.3;
/// How far above the landing the ring's bars stand, and how far into the surface below it they reach, in metres: tall
/// enough that a surface drawn up to 2 cm above where it is declared, such as a path's stones on the earth, never hides
/// the ring, and sunk enough that one drawn up to 1 cm below never shows a gap under it, so that it sits on what is
/// drawn rather than floating.
pub const LANDING_TOP: f32 = 0.025;
pub const LANDING_SUNK: f32 = 0.01;
/// The most teleport pads drawn: two for each pair a profile uses.
const MOST_PADS: usize = 2 * tomori_scene::locomotion::MOST_PAIRS;
/// How many bars make a pad's ring, and its highlight's.
const PAD_RING: usize = 24;
/// How many squares, each turned a little more, make the plate inside a pad's ring.
const PAD_PLATE: usize = 6;
/// How many bars make each chevron above a pad, and how many chevrons there are.
const PAD_CHEVRON: usize = 4;
const PAD_CHEVRONS: usize = 2;
/// The most cubes one pad is drawn with: its ring, its plate, its chevrons and, pointed at, its highlight's ring.
const PAD_CUBES: usize = 2 * PAD_RING + PAD_PLATE + PAD_CHEVRON * PAD_CHEVRONS;

/// The usage of attachments that live only within one eye's scene pass, cleared at its start and discarded at its end; see [`Renderer::multisampled_views`].
const TRANSIENT: wgpu::TextureUsages =
    wgpu::TextureUsages::RENDER_ATTACHMENT.union(wgpu::TextureUsages::TRANSIENT_ATTACHMENT);
const VERTEX_ATTRIBUTES: [wgpu::VertexAttribute; 2] =
    wgpu::vertex_attr_array![0 => Float32x3, 1 => Float32x3];
const INSTANCE_ATTRIBUTES: [wgpu::VertexAttribute; 7] = wgpu::vertex_attr_array![2 => Float32x3, 3 => Float32x3, 4 => Float32x3, 5 => Float32x3, 6 => Float32x3, 7 => Float32x3, 8 => Float32];

// Keep the shared cube lighting shader, extending its instance transform here.
// The source shader is also used independently by offscreen reference tests.
fn oriented_shader() -> String {
    tomori_assets::haze::shader(include_str!("swarm.wgsl"))
        .replace("    @location(4) color: vec3<f32>,", "    @location(4) color: vec3<f32>,\n    @location(5) basis_x: vec3<f32>,\n    @location(6) basis_y: vec3<f32>,\n    @location(7) basis_z: vec3<f32>,")
        .replace("    var out: VertexOutput;", "    let rotation = mat3x3<f32>(basis_x, basis_y, basis_z);\n    var out: VertexOutput;")
        .replace("position + vertex * drawn", "position + rotation * (vertex * drawn)")
        .replace("out.normal = normal;", "out.normal = rotation * normal;")
}

/// The bytes of an eye's camera, as every pipeline bound to it reads it: its matrix from the scene to its clip space,
/// where the eye is, the scene's haze and, for Tomori's cubes, half its view's height in pixels, padded to a multiple
/// of 16 bytes.
pub(crate) const CAMERA_SIZE: u64 = 112;

/// An eye's camera, as Tomori's own pipelines read it: `scene_to_clip`, the eye it sees from, `haze`, the environment's,
/// which they fade what they draw by, and half of `height`, its view's height in pixels, by which no cube is drawn
/// smaller than a pixel. See `design/rendering.md`.
pub(crate) fn camera(scene_to_clip: Mat4, haze: Haze, height: u32) -> [f32; 28] {
    let mut camera = [0.0; 28];
    camera[..16].copy_from_slice(&scene_to_clip.to_cols_array());
    camera[16..19].copy_from_slice(&crate::scenery::eye(scene_to_clip).to_array());
    camera[20..24].copy_from_slice(&haze.to_array());
    camera[24] = height as f32 * 0.5;
    camera
}

/// The layout of an eye's camera, [`camera`], which Tomori's cubes, its controllers, its signs and arrival's labels are
/// drawn with, read by their fragments too for the haze.
pub(crate) fn camera_layout(device: &wgpu::Device) -> wgpu::BindGroupLayout {
    device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("eye camera layout"),
        entries: &[wgpu::BindGroupLayoutEntry {
            binding: 0,
            visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
            ty: wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Uniform,
                has_dynamic_offset: false,
                min_binding_size: wgpu::BufferSize::new(CAMERA_SIZE),
            },
            count: None,
        }],
    })
}

/// An eye's camera buffer, for [`camera`].
pub(crate) fn camera_buffer(device: &wgpu::Device) -> wgpu::Buffer {
    device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("eye camera"),
        size: CAMERA_SIZE,
        usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    })
}

/// The samples per pixel the headset and desktop window draw with unless told otherwise: 4, for multisample anti-aliasing.
pub const SAMPLES: u32 = 4;

/// `requested` samples per pixel if `adapter` can draw both `format` and the scene's depth with them, or else 1, which draws without multisampling.
///
/// Only 1 and 4 are offered: they are the counts wgpu allows without adapter-specific format features, which the headset's device does not enable.
pub fn supported_samples(
    adapter: &wgpu::Adapter,
    format: wgpu::TextureFormat,
    requested: u32,
) -> u32 {
    let supported = |format: wgpu::TextureFormat| {
        adapter
            .get_texture_format_features(format)
            .flags
            .sample_count_supported(SAMPLES)
    };
    if requested >= SAMPLES && supported(format) && supported(wgpu::TextureFormat::Depth32Float) {
        SAMPLES
    } else {
        1
    }
}

/// The samples per pixel asked for by `value`, such as the headset's `tomori.msaa` intent extra: `4` or `on` for multisample anti-aliasing, `1`, `0` or `off` for none, and [`SAMPLES`] when absent or not understood.
#[cfg_attr(not(target_os = "android"), allow(dead_code))]
pub fn requested_samples(value: Option<&str>) -> u32 {
    let Some(value) = value.map(str::trim).filter(|value| !value.is_empty()) else {
        return SAMPLES;
    };
    match value.to_ascii_lowercase().as_str() {
        "0" | "1" | "off" => 1,
        "4" | "on" => SAMPLES,
        _ => {
            log::warn!(
                "Multisampling {value:?} is not 1, 4, on or off; using {SAMPLES} samples per pixel."
            );
            SAMPLES
        }
    }
}

/// A frame's draws for the eyes, encoded by [`Renderer::encode`] and not yet submitted.
#[must_use = "a frame encoded is drawn only once it is submitted"]
pub struct Encoded {
    commands: wgpu::CommandBuffer,
    profiled: Option<crate::profiler::Frame>,
}

pub struct Renderer {
    environment: Environment,
    /// While a space's environment grows or shrinks, the sphere it fills; the grid is drawn outside it.
    reveal: Option<crate::arrival_motion::Sphere>,
    /// The transform from the environment's own coordinates, in which its scenery draws, into the space's, where home
    /// is the origin facing −Z. See `design/environments.md`.
    carriage: Mat4,
    /// Where a teleport being aimed would land, drawn as a ring; see `design/locomotion.md`.
    landing: Option<glam::Vec3>,
    /// The ring drawn on the floor at home, as the frame step last gave it, or none, when it is drawn beneath the swarm.
    home_ring: Option<tomori_scene::anchors::HomeRing>,
    /// The teleport pads drawn, each a ring with a plate and chevrons above it; see `design/locomotion.md`.
    pads: Vec<tomori_scene::pads::Shown>,
    /// How dark the whole view is drawn, from 0 to 1, as it comes back after a teleport; see `design/locomotion.md`.
    blink: f32,
    /// Darkens the whole view by `blink`, drawn last in each eye.
    blink_pipeline: wgpu::RenderPipeline,
    /// The registered sceneries, each drawn while its environment is.
    sceneries: Vec<(Environment, Box<dyn crate::scenery::Scenery>)>,
    /// What this renderer can draw: the grid, then each scenery's environment.
    environments: Vec<Environment>,
    /// What makes each registered avatar's scenery, by the avatar's identifier, and the scenery of the avatar shown
    /// now, if any, with the avatar and the space it was made for, made when it first shows and dropped, with what it
    /// holds on the GPU, when another avatar, the same in another space, or the swarm shows. See `design/avatars.md`.
    avatar_sceneries: Vec<(&'static str, crate::scenery::Create)>,
    avatar: Option<(String, String, Box<dyn crate::scenery::Scenery>)>,
    /// The registered avatars whose scenery panicked as it was made or given what its avatar shows, which this renderer
    /// never makes again, shared with the conversation, which shows the swarm in their place.
    avatar_failures: tomori_scene::avatar::Failures,
    /// The eyes' targets' size, for an avatar's scenery made later.
    size: (u32, u32),
    pipeline: wgpu::RenderPipeline,
    vertices: wgpu::Buffer,
    instances: wgpu::Buffer,
    cameras: [wgpu::Buffer; 2],
    camera_bindings: [wgpu::BindGroup; 2],
    depth: [wgpu::TextureView; 2],
    /// Samples per pixel in each eye's scene pass; above 1, the pass draws into `multisampled` and resolves into the eye's target. With the format, for resizing, which only desktop windows do.
    #[cfg_attr(target_os = "android", allow(dead_code))]
    samples: u32,
    #[cfg_attr(target_os = "android", allow(dead_code))]
    format: wgpu::TextureFormat,
    multisampled: Option<[wgpu::TextureView; 2]>,
    scene: Vec<Instance>,
    vertex_count: u32,
    controllers: Controllers,
    base_scene_len: usize,
    workspace: crate::workspace_graphics::WorkspaceGraphics,
    arrival: crate::arrival_graphics::ArrivalLabels,
    panels: crate::ui_panel::Pipeline,
    /// Image panels' images, drawn over their faces. See `design/projection.md`.
    images: crate::panel_images::PanelImages,
    glyphs: crate::glyphs::SharedGlyphs,
    /// Whether what neither eye can see is culled, as it is unless a comparison turns it off.
    culling: bool,
    /// What this frame draws, each in its own coordinates, Tomori's cubes and panels and what the scenery and the avatar
    /// submit, and what the eyes may see of it. See `design/rendering.md`.
    world: tomori_world::World,
    submitted: Submitted,
    /// Each registered scenery's coordinates in the world, by its place in `sceneries`, culled while it is drawn.
    scenery_roots: Vec<tomori_world::RootId>,
    /// What the last frame drew in each eye, and how much it culled.
    tally: tomori_assets::bounds::Tally,
    /// The panels' faces the last frame drew, and culled.
    panels_seen: (usize, usize),
    /// The cubes drawn this frame, of [`Self::scene`], those the eyes may see, and whether the cubes' buffer holds only
    /// those rather than the whole scene, as `update` and the rest write it.
    shown: Vec<Instance>,
    cubes_culled: bool,
    /// The GPU's time for each frame, and what each drew; see `design/rendering.md`.
    profiler: crate::profiler::Profiler,
    solids: crate::solids::Solids,
    /// The others in the space, as cubes after [`Self::scene`]'s, each person an item of the world of their own, culled
    /// whole and then cube by cube, with the cubes of [`Self::people`] that are theirs this frame.
    pub(crate) people: Vec<Instance>,
    figures: Vec<(tomori_world::ItemId, std::ops::Range<usize>)>,
}

/// What the renderer itself submits to its world: the scene's coordinates within the space's, the cubes', and the
/// avatar's, which is cleared when its scenery is dropped; and its two items, the cubes and the panels, in the space's
/// coordinates, each instance one cube or panel.
struct Submitted {
    scene: tomori_world::RootId,
    avatar: tomori_world::RootId,
    cubes: tomori_world::ItemId,
    panels: tomori_world::ItemId,
}

impl Submitted {
    /// Its roots, and its items, in `world`, which also holds a root for each of `sceneries`.
    fn new(world: &mut tomori_world::World, sceneries: usize) -> (Self, Vec<tomori_world::RootId>) {
        use tomori_world::{Item, Layer, Mobility};
        let space = world.add_root();
        let scene = world.add_root();
        let avatar = world.add_root();
        let roots = (0..sceneries).map(|_| world.add_root()).collect();
        let cubes = world.add(Item::new(scene, Layer::Opaque, Mobility::Dynamic));
        let panels = world.add(Item::new(space, Layer::Translucent, Mobility::Dynamic));
        (
            Self {
                scene,
                avatar,
                cubes,
                panels,
            },
            roots,
        )
    }
}

/// Where a cube of Tomori's own may be drawn: the unit cube, scaled, turned by its basis and moved to its position.
fn cube_bounds(cube: &Instance) -> tomori_assets::bounds::Aabb {
    let placed = glam::Mat3::from_cols_array_2d(&cube.basis)
        * glam::Mat3::from_diagonal(glam::Vec3::from_array(cube.scale));
    let placed = Mat4::from_cols(
        placed.x_axis.extend(0.0),
        placed.y_axis.extend(0.0),
        placed.z_axis.extend(0.0),
        glam::Vec3::from_array(cube.position).extend(1.0),
    );
    tomori_assets::bounds::Aabb::new(glam::Vec3::splat(-0.5), glam::Vec3::splat(0.5))
        .transformed(placed)
}

impl Renderer {
    /// The headset draws with [`Renderer::with_samples`].
    #[cfg_attr(target_os = "android", allow(dead_code))]
    pub fn new(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        format: wgpu::TextureFormat,
        width: u32,
        height: u32,
    ) -> Self {
        Self::with_samples(device, queue, format, width, height, 1)
    }

    /// As [`Renderer::new`], with `samples` samples per pixel for multisample anti-aliasing, such as 4; 1 draws straight into the targets.
    pub fn with_samples(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        format: wgpu::TextureFormat,
        width: u32,
        height: u32,
        samples: u32,
    ) -> Self {
        Self::with_sceneries(
            device,
            queue,
            format,
            width,
            height,
            samples,
            &crate::scenery::Sceneries::default(),
        )
    }

    /// As [`Renderer::with_samples`], drawing `sceneries` as well as the built-in grid.
    pub fn with_sceneries(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        format: wgpu::TextureFormat,
        width: u32,
        height: u32,
        samples: u32,
        sceneries: &crate::scenery::Sceneries,
    ) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("oriented swarm instances"),
            source: wgpu::ShaderSource::Wgsl(oriented_shader().into()),
        });
        let camera_layout = camera_layout(device);
        let cameras = std::array::from_fn(|_| camera_buffer(device));
        let camera_bindings = std::array::from_fn(|eye| {
            device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("eye camera binding"),
                layout: &camera_layout,
                entries: &[wgpu::BindGroupEntry {
                    binding: 0,
                    resource: cameras[eye].as_entire_binding(),
                }],
            })
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("swarm pipeline layout"),
            bind_group_layouts: &[Some(&camera_layout)],
            immediate_size: 0,
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("swarm pipeline"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                compilation_options: Default::default(),
                buffers: &[
                    Some(wgpu::VertexBufferLayout {
                        array_stride: size_of::<Vertex>() as u64,
                        step_mode: wgpu::VertexStepMode::Vertex,
                        attributes: &VERTEX_ATTRIBUTES,
                    }),
                    Some(wgpu::VertexBufferLayout {
                        array_stride: size_of::<Instance>() as u64,
                        step_mode: wgpu::VertexStepMode::Instance,
                        attributes: &INSTANCE_ATTRIBUTES,
                    }),
                ],
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: wgpu::PrimitiveState {
                cull_mode: None,
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
            multiview_mask: None,
            cache: None,
        });
        let mesh = cube();
        let vertices = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("unit cube"),
            contents: bytemuck::cast_slice(&mesh),
            usage: wgpu::BufferUsages::VERTEX,
        });
        let instances = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("swarm instances"),
            size: ((MAX_INSTANCES + PEOPLE_INSTANCES) * size_of::<Instance>()) as u64,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let depth = Self::depth_views(device, width, height, samples);
        let multisampled = Self::multisampled_views(device, format, width, height, samples);
        let controllers = Controllers::new(device, queue, format, &camera_layout, samples);
        let glyphs = crate::glyphs::SharedGlyphs::new(device);
        let workspace = crate::workspace_graphics::WorkspaceGraphics::new(
            device,
            format,
            &camera_layout,
            glyphs.glyphs(),
            samples,
        );
        let arrival = crate::arrival_graphics::ArrivalLabels::new(
            device,
            format,
            &camera_layout,
            glyphs.glyphs(),
            samples,
        );
        let panels = crate::ui_panel::Pipeline::shared(device, format, &glyphs, samples);
        let solids = crate::solids::Solids::new(device);
        let images =
            crate::panel_images::PanelImages::new(device, format, samples, panels.transforms());
        let blink_pipeline = blink_pipeline(device, format, samples);
        let target = crate::scenery::Target::new(
            device,
            queue,
            format,
            width,
            height,
            samples,
            glyphs.clone(),
        );
        let created = sceneries.create(&target);
        let mut world = tomori_world::World::new();
        let (submitted, scenery_roots) = Submitted::new(&mut world, created.len());
        Self {
            // Arrival's grid; a headset follows its conversation from here, and the desktop window chooses its own.
            environment: Environment::MINIMAL,
            reveal: None,
            carriage: Mat4::IDENTITY,
            landing: None,
            home_ring: None,
            pads: Vec::new(),
            blink: 0.0,
            blink_pipeline,
            sceneries: created,
            environments: sceneries.environments(),
            avatar_sceneries: sceneries.avatar_sceneries(),
            avatar: None,
            avatar_failures: sceneries.avatar_failures(),
            size: (width, height),
            pipeline,
            vertices,
            instances,
            cameras,
            camera_bindings,
            depth,
            samples,
            format,
            multisampled,
            scene: Vec::with_capacity(MAX_INSTANCES),
            vertex_count: mesh.len() as u32,
            controllers,
            base_scene_len: 0,
            workspace,
            arrival,
            panels,
            images,
            glyphs,
            culling: true,
            world,
            submitted,
            scenery_roots,
            tally: Default::default(),
            panels_seen: (0, 0),
            shown: Vec::with_capacity(MAX_INSTANCES + PEOPLE_INSTANCES),
            cubes_culled: false,
            // Marks within a pass mean little on the headset's tiled GPU, which draws a pass's tiles one by one.
            profiler: crate::profiler::Profiler::new(device, queue, !cfg!(target_os = "android")),
            solids,
            people: Vec::with_capacity(PEOPLE_INSTANCES),
            figures: Vec::with_capacity(MOST_PEOPLE),
        }
    }

    /// Which of the others' cubes each person's item of the world holds, by where they come in the frame.
    #[cfg(test)]
    pub(crate) fn figures(&self) -> Vec<std::ops::Range<usize>> {
        self.figures
            .iter()
            .map(|(_, range)| range.clone())
            .collect()
    }

    /// The others in the space, as the frame step drew them, in the scene: each of the first [`MOST_PEOPLE`] an item of
    /// the world of their own, among Tomori's cubes, in their one draw, hazed and lit as the rest. See `design/people.md`.
    pub fn set_people(&mut self, figures: &[tomori_scene::people::Figure]) {
        use tomori_world::{Item, Layer, Mobility};
        self.people.clear();
        let finite = |values: &[f32]| values.iter().all(|value| value.is_finite());
        for (index, figure) in figures.iter().take(MOST_PEOPLE).enumerate() {
            let start = self.people.len();
            self.people.extend(
                figure
                    .cubes
                    .iter()
                    .take(tomori_scene::people::CUBES)
                    .filter(|cube| {
                        finite(&cube.position)
                            && finite(&cube.scale)
                            && finite(&cube.orientation)
                            && cube.scale.iter().all(|side| *side > 0.0)
                    })
                    .map(|cube| Instance {
                        position: cube.position,
                        scale: cube.scale,
                        color: cube.color,
                        basis: glam::Mat3::from_quat(
                            glam::Quat::from_array(cube.orientation).normalize(),
                        )
                        .to_cols_array_2d(),
                        glow: PEOPLE_GLOW,
                    }),
            );
            let range = start..self.people.len();
            match self.figures.get_mut(index) {
                Some((_, kept)) => *kept = range,
                None => {
                    let item = self.world.add(Item::new(
                        self.submitted.scene,
                        Layer::Opaque,
                        Mobility::Dynamic,
                    ));
                    self.figures.push((item, range));
                }
            }
        }
        let drawn = figures.len().min(MOST_PEOPLE);
        for (_, range) in self.figures.iter_mut().skip(drawn) {
            *range = 0..0;
        }
    }

    /// Cull what neither eye can see, as by default, or draw everything, `false`, to compare.
    pub fn set_culling(&mut self, culling: bool) {
        self.culling = culling;
    }

    /// The world the last frame was culled in.
    #[cfg(test)]
    pub(crate) fn world(&self) -> &tomori_world::World {
        &self.world
    }

    /// What the last frame drew in each eye, and how much it culled: the scenery's and the avatar's, Tomori's cubes and
    /// the panels.
    pub fn tally(&self) -> tomori_assets::bounds::Tally {
        self.tally
    }

    /// How many projected panels' faces the last frame drew, and how many it culled, as neither eye could see them: of
    /// the panels alone, not the walls' boards, labels, cards and other surfaces drawn with them.
    pub fn panels_seen(&self) -> (usize, usize) {
        self.panels_seen
    }

    /// A line summarizing what the GPU took for the frames since the last summary, and what they drew, for the frame log
    /// once a minute; see `design/rendering.md`.
    pub fn gpu_summary(&mut self) -> Option<String> {
        self.profiler.summary()
    }

    /// The frame's volumes, whose solids are drawn among the opaque bodies, within their budget. See
    /// `design/diagrams.md`.
    pub fn update_volumes(
        &mut self,
        queue: &wgpu::Queue,
        volumes: &[tomori_scene::projection::volume::Drawn],
    ) {
        self.solids.update(queue, volumes);
    }

    /// How many triangles the frame's volumes' solids take, and how many volumes it drew without them.
    pub fn volume_counts(&self) -> (usize, usize) {
        self.solids.counts()
    }

    /// This frame's panels show the images `shown`, by hash, each with its PNG: decode and upload them, within the
    /// texture budget. See `design/projection.md`.
    pub fn update_images(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        shown: &[(String, std::sync::Arc<[u8]>)],
    ) {
        self.images.update(device, queue, shown);
    }

    /// The images drawn over image panels, and their textures.
    pub fn images(&mut self) -> &mut crate::panel_images::PanelImages {
        &mut self.images
    }

    /// The atlas its sceneries are given, for tests that check they share it.
    #[cfg(test)]
    pub(crate) fn shared_glyphs(&self) -> &crate::glyphs::SharedGlyphs {
        &self.glyphs
    }

    /// A panel drawn by this renderer's scene pass, holding at most `capacity` vertices.
    pub fn panel(&self, device: &wgpu::Device, capacity: usize) -> crate::ui_panel::Panel {
        crate::ui_panel::Panel::new(device, &self.panels, capacity)
    }

    /// Each eye's multisampled color target, which its scene pass resolves into the eye's own target; none for a single sample.
    ///
    /// Like the depth views, these are cleared at the start of the pass and discarded at its end, so they are transient: a tiling GPU, such as the Quest's, keeps their samples in tile memory and need never back them with memory of their own.
    fn multisampled_views(
        device: &wgpu::Device,
        format: wgpu::TextureFormat,
        width: u32,
        height: u32,
        samples: u32,
    ) -> Option<[wgpu::TextureView; 2]> {
        (samples > 1).then(|| {
            std::array::from_fn(|_| {
                device
                    .create_texture(&wgpu::TextureDescriptor {
                        label: Some("eye multisampled color"),
                        size: wgpu::Extent3d {
                            width,
                            height,
                            depth_or_array_layers: 1,
                        },
                        mip_level_count: 1,
                        sample_count: samples,
                        dimension: wgpu::TextureDimension::D2,
                        format,
                        usage: TRANSIENT,
                        view_formats: &[],
                    })
                    .create_view(&Default::default())
            })
        })
    }

    fn depth_views(
        device: &wgpu::Device,
        width: u32,
        height: u32,
        samples: u32,
    ) -> [wgpu::TextureView; 2] {
        std::array::from_fn(|_| {
            device
                .create_texture(&wgpu::TextureDescriptor {
                    label: Some("eye depth"),
                    size: wgpu::Extent3d {
                        width,
                        height,
                        depth_or_array_layers: 1,
                    },
                    mip_level_count: 1,
                    sample_count: samples,
                    dimension: wgpu::TextureDimension::D2,
                    format: wgpu::TextureFormat::Depth32Float,
                    usage: TRANSIENT,
                    view_formats: &[],
                })
                .create_view(&Default::default())
        })
    }

    #[cfg(any(not(target_os = "android"), test))]
    pub fn resize(&mut self, device: &wgpu::Device, width: u32, height: u32) {
        self.depth = Self::depth_views(device, width, height, self.samples);
        self.multisampled =
            Self::multisampled_views(device, self.format, width, height, self.samples);
        for (_, scenery) in &mut self.sceneries {
            scenery.resize(device, width, height);
        }
        self.size = (width, height);
        if let Some((_, _, scenery)) = &mut self.avatar {
            scenery.resize(device, width, height);
        }
    }

    /// The scenery drawn now, if the environment has one.
    fn scenery(&self) -> Option<&dyn crate::scenery::Scenery> {
        self.sceneries
            .iter()
            .find(|(environment, _)| *environment == self.environment)
            .map(|(_, scenery)| scenery.as_ref())
    }

    /// The haze everything drawn is faded by, for an eye at `eye` in the scene: the environment's, from its scenery; or
    /// none, the grid's, where there is no scenery or while the environment grows out of a space's object with the eye
    /// still outside it, in the grid.
    pub fn haze(&self, eye: glam::Vec3) -> Haze {
        match (self.scenery(), self.reveal) {
            (Some(_), Some(sphere)) if eye.distance(sphere.centre) > sphere.radius => Haze::NONE,
            (Some(scenery), _) => scenery.haze(),
            (None, _) => Haze::NONE,
        }
    }

    /// What this renderer can draw: the grid, then each scenery's environment, as the desktop window's E key cycles through them.
    pub fn environments(&self) -> &[Environment] {
        &self.environments
    }

    #[cfg(any(not(target_os = "android"), test))]
    pub fn environment(&self) -> Environment {
        self.environment
    }

    /// Does not modify simulation, worker placement, input or conversation state.
    /// The next `update` rebuilds only the presentation instances. The headset uses [`Renderer::set_scenery`] instead.
    #[cfg(any(not(target_os = "android"), test))]
    pub fn set_environment(&mut self, environment: Environment) {
        self.environment = environment;
    }

    /// What the current environment's client side shows now, for its scenery to draw. See `design/environments.md`.
    pub fn animate(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        drawn: &(dyn std::any::Any + Send + Sync),
    ) {
        let environment = self.environment;
        if let Some((_, scenery)) = self
            .sceneries
            .iter_mut()
            .find(|(registered, _)| *registered == environment)
        {
            scenery.animate(device, queue, drawn);
        }
    }

    /// Draw the environment's scenery, in its own coordinates, carried into the space's by `carriage`, as for an
    /// environment whose participants arrive elsewhere than its origin, from the next [`Self::set_scenery`]. See
    /// `design/environments.md`.
    pub fn set_carriage(&mut self, carriage: tomori_scene::anchors::Anchor) {
        self.carriage = Mat4::from_rotation_translation(carriage.orientation, carriage.position);
    }

    /// Draw the registered avatar `shown` names from now on, given what it shows, if anything, or none, while the swarm
    /// shows, which [`Self::update`] draws. Its scenery is made when it first shows, and made anew for another space,
    /// so that nothing of one space's shows in the next; it is dropped when the swarm or another avatar shows. An
    /// avatar this renderer has no scenery for draws nothing, and so does one whose scenery panicked as it was made or
    /// given what its avatar shows, from then on; a panic as it is drawn is not caught. See `design/avatars.md`.
    pub fn set_avatar(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        shown: Option<&crate::avatar::Shown<'_>>,
    ) {
        let Some(shown) = shown else {
            self.drop_avatar();
            return;
        };
        let current = self
            .avatar
            .as_ref()
            .is_some_and(|(avatar, space, _)| avatar == shown.avatar && space == shown.space);
        if !current {
            // The previous one's resources go before the next one's are made.
            self.drop_avatar();
            let create = self
                .avatar_sceneries
                .iter()
                .find(|(id, _)| *id == shown.avatar && !self.avatar_failures.contains(id))
                .map(|(_, create)| create.clone());
            if let Some(create) = create {
                let target = crate::scenery::Target::new(
                    device,
                    queue,
                    self.format,
                    self.size.0,
                    self.size.1,
                    self.samples,
                    self.glyphs.clone(),
                );
                match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| create(&target))) {
                    Ok(scenery) => {
                        self.avatar =
                            Some((shown.avatar.to_owned(), shown.space.to_owned(), scenery));
                    }
                    Err(_) => self.give_up_avatar(shown.avatar),
                }
            }
        }
        if let (Some((_, _, scenery)), Some(drawn)) = (&mut self.avatar, &shown.drawn) {
            let animated = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                scenery.animate(device, queue, drawn.as_ref())
            }));
            if animated.is_err() {
                self.drop_avatar();
                self.give_up_avatar(shown.avatar);
            }
        }
    }

    /// Drop the avatar's scenery, with what it holds on the GPU and what it submitted to the world.
    fn drop_avatar(&mut self) {
        self.avatar = None;
        self.world.clear(self.submitted.avatar);
    }

    /// Never make the scenery of the avatar `avatar` again, and tell the conversation, which shows the swarm in its
    /// place from the next frame and says so.
    fn give_up_avatar(&mut self, avatar: &str) {
        self.avatar_failures.report(avatar);
    }

    /// The avatar's scenery drawn now, if any.
    fn avatar(&self) -> Option<&dyn crate::scenery::Scenery> {
        self.avatar.as_ref().map(|(_, _, scenery)| scenery.as_ref())
    }

    /// The environment and, while it grows out of a space's object or shrinks back into it, the sphere it fills, with the grid outside. See `design/arrival.md`.
    pub fn set_scenery(&mut self, scenery: crate::arrival_motion::Scenery) {
        self.environment = scenery.environment;
        self.reveal = scenery.reveal;
        // The scenery reveals its own coordinates: the sphere's centre, in the space's, carried back into them.
        let back = self.carriage.inverse();
        let reveal = scenery
            .reveal
            .map(|sphere| (back.transform_point3(sphere.centre), sphere.radius));
        for (_, scenery) in &mut self.sceneries {
            scenery.set_reveal(reveal);
        }
    }

    /// Draw the ring at `ring`, the agent's home, from the next [`Self::update`], wherever the agent goes: at its height,
    /// on the space's floor however high its home stands or under the home where its environment says so, a little
    /// dimmer while the agent is away, brightening as it comes back. With none, or one that is nonsense, it is drawn
    /// beneath the swarm wherever it stands, as before there were homes. See `design/environments.md`.
    pub fn set_home_ring(&mut self, ring: Option<tomori_scene::anchors::HomeRing>) {
        self.home_ring = ring.filter(|ring| {
            ring.centre.is_finite()
                && ring.towards.is_finite()
                && ring.radius > 0.0
                && ring.radius <= MOST_HOME_RING
                && ring.sweep.is_finite()
                && ring.height.is_finite()
        });
    }

    /// Draw a ring at `landing`, where a teleport being aimed would land, from the next [`Self::update`], or none.
    pub fn set_landing(&mut self, landing: Option<glam::Vec3>) {
        self.landing = landing.filter(|landing| landing.is_finite());
    }

    /// Draw `pads`, the teleport pads of the space's profile, from the next [`Self::update`]: the first [`MOST_PADS`]
    /// that are finite and no wider than a pad may be.
    pub fn set_pads(&mut self, pads: &[tomori_scene::pads::Shown]) {
        self.pads = pads
            .iter()
            .filter(|pad| {
                pad.centre.is_finite()
                    && pad.radius > 0.0
                    && pad.radius <= tomori_scene::locomotion::WIDEST_PAD
            })
            .take(MOST_PADS)
            .copied()
            .collect();
    }

    /// Darken the whole view by `blink`, from 0, not at all, to 1, black, from the next [`Self::render`]: a teleport's blink. Nonsense draws nothing.
    pub fn set_blink(&mut self, blink: f32) {
        self.blink = if blink.is_finite() {
            blink.clamp(0.0, 1.0)
        } else {
            0.0
        };
    }

    pub fn update(
        &mut self,
        queue: &wgpu::Queue,
        swarm: &Swarm,
        controller: Option<glam::Vec3>,
        attracting: bool,
    ) {
        self.scene.clear();
        self.workspace.hide();
        self.arrival.hide();
        swarm_instances(swarm, &mut self.scene);
        if self.environment == Environment::MINIMAL || self.reveal.is_some() {
            minimal_floor(&mut self.scene, self.reveal);
        }
        // The ring at home, the agent's place, which stays there wherever the agent goes, on the space's floor however
        // high its home stands, or under it where its environment says so; or else beneath the swarm, as before there were homes: on the room's
        // floor, or, for a swarm raised above its usual height, on the floor it would stand that high over. A little
        // dimmer while the agent is away, it brightens as it comes back within the ring.
        let swarm_at = swarm.anchor().position;
        let (ring, floor) = match self.home_ring {
            Some(ring) => (ring, ring.height + FLOOR_LIFT),
            None => (
                tomori_scene::anchors::HomeRing {
                    centre: swarm_at,
                    ..tomori_scene::anchors::HomeRing::WHOLE
                },
                (swarm_at.y - crate::swarm::CENTER.y).max(0.0) + FLOOR_LIFT,
            ),
        };
        let (centre, radius) = (ring.centre, ring.radius);
        let away = (swarm_at.x - centre.x).hypot(swarm_at.z - centre.z);
        let faint = ((away - radius / 2.0) / radius).clamp(0.0, 1.0);
        let brightness = 1.0 - (1.0 - HOME_RING_AWAY) * faint;
        // Only as much of it as its workers take, about the way their anchor faces, its cubes standing evenly either
        // side of that way, so that the side it leaves open is centred on it.
        let towards = glam::Vec2::new(ring.towards.x, ring.towards.z)
            .try_normalize()
            .unwrap_or(glam::Vec2::Y)
            .to_angle();
        let step = std::f32::consts::TAU / HOME_RING_CUBES as f32;
        for n in 0..HOME_RING_CUBES {
            // Out from that way, alternately one side and the other, the last straight opposite it.
            let side = if n % 2 == 0 { 1.0 } else { -1.0 };
            let off = side * n.div_ceil(2) as f32 * step;
            if off.abs() > ring.sweep / 2.0 + 1e-4 {
                continue;
            }
            let angle = towards + off;
            self.scene.push(Instance {
                position: [
                    centre.x + angle.cos() * radius,
                    floor,
                    centre.z + angle.sin() * radius,
                ],
                scale: [0.035, 0.015, 0.035],
                color: HOME_RING_COLOUR.map(|channel| channel * brightness),
                ..Default::default()
            });
        }
        if let Some(landing) = self.landing {
            landing_ring(landing, &mut self.scene);
        }
        for pad in &self.pads {
            pad_cubes(pad, &mut self.scene);
        }
        if let Some(position) = controller {
            self.scene.push(Instance {
                position: position.to_array(),
                scale: [0.06; 3],
                color: if attracting {
                    [1.0, 0.45, 0.08]
                } else {
                    [0.35, 0.4, 0.55]
                },
                ..Default::default()
            });
        }
        assert!(self.scene.len() <= MAX_INSTANCES);
        self.base_scene_len = self.scene.len();
        queue.write_buffer(&self.instances, 0, bytemuck::cast_slice(&self.scene));
        self.cubes_culled = false;
    }

    /// The space's timers' dials, whose cubes come among the workers', and whose signs the next `update_workspace` draws.
    /// See `design/timers.md`.
    pub fn set_dials(&mut self, dials: &[tomori_scene::dials::Dial]) {
        self.workspace.set_dials(dials);
    }

    /// Append coordinator-curated workspace visuals without replacing the swarm,
    /// floor, or controller models. Call after `update` for both XR and spectator.
    pub fn update_workspace(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        cubes: &[crate::delegation::Cube],
        stations: &[crate::delegation::Station],
        workspace: &crate::delegation::Workspace,
        viewer: Option<glam::Vec3>,
    ) {
        self.scene.truncate(self.base_scene_len);
        // The scene owns the worker lifecycle: it can retain unknown workers
        // during an outage and fade departures after an authoritative removal.
        for cube in cubes.iter().take(crate::delegation::MAX_CUBES) {
            if self.scene.len() < MAX_INSTANCES
                && let Some(instance) = worker_instance(cube)
            {
                self.scene.push(instance);
            }
        }
        queue.write_buffer(&self.instances, 0, bytemuck::cast_slice(&self.scene));
        self.cubes_culled = false;
        self.workspace
            .update(device, queue, workspace, stations, viewer);
    }

    /// How many vertices the arrival labels' buffer holds now.
    #[cfg(test)]
    pub fn arrival_label_capacity(&self) -> usize {
        self.arrival.capacity()
    }

    /// Append the arrival scene, the pointing ray and the labels anchored to its objects, after the swarm, floor and any workspace cubes, which a space's arrival may still be leaving. Call after `update` and `update_workspace`.
    pub fn update_arrival(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        frame: &crate::arrival_graphics::Frame,
    ) {
        // Unlike workspace cubes, these are made here, so the ray and threads may be longer than a metre.
        let finite = |values: &[f32]| values.iter().all(|v| v.is_finite());
        for cube in crate::arrival_graphics::cubes(frame) {
            if self.scene.len() < MAX_INSTANCES
                && finite(&cube.position)
                && finite(&cube.scale)
                && finite(&cube.orientation)
            {
                self.scene.push(Instance {
                    position: cube.position,
                    scale: cube.scale,
                    color: cube.color,
                    basis: glam::Mat3::from_quat(glam::Quat::from_array(cube.orientation))
                        .to_cols_array_2d(),
                    glow: 0.0,
                });
            }
        }
        queue.write_buffer(&self.instances, 0, bytemuck::cast_slice(&self.scene));
        self.cubes_culled = false;
        self.arrival
            .update(device, queue, frame.scene, frame.hovered);
    }

    /// Zero-based page, applied by the next `update_workspace` call.
    pub fn set_presentation_page(&mut self, page: usize) {
        self.workspace.set_page(page);
    }

    /// Zero-based page within the focused presentation.
    pub fn set_presentation_content_page(&mut self, page: usize) {
        self.workspace.set_content_page(page);
    }

    #[cfg(test)]
    pub fn presentation_page_count(&self) -> usize {
        self.workspace.page_count()
    }

    pub fn update_controllers(
        &mut self,
        queue: &wgpu::Queue,
        poses: [Option<Pose>; 2],
        hands: [Option<Hand>; 2],
    ) {
        self.controllers.update(queue, poses, hands);
    }

    pub fn set_controller_model(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        hand: usize,
        model: &ControllerModel,
    ) {
        self.controllers.set_model(device, queue, hand, model);
    }

    /// Draw the eyes `matrices` see into `targets`: [`Self::encode`], then [`Self::submit`] at once.
    pub fn render(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        targets: &[wgpu::TextureView],
        matrices: &[Mat4],
        scene_to_space: Mat4,
        panels: &[(&crate::ui_panel::Panel, Mat4)],
    ) -> wgpu::SubmissionIndex {
        let encoded = self.encode(device, queue, targets, matrices, scene_to_space, panels);
        self.submit(queue, encoded)
    }

    /// Submit the eyes' draws [`Self::encode`] made, to be drawn after whatever was submitted before, and read their
    /// timestamps back once the GPU has written them.
    pub fn submit(&mut self, queue: &wgpu::Queue, encoded: Encoded) -> wgpu::SubmissionIndex {
        let submitted = queue.submit([encoded.commands]);
        self.profiler.submitted(encoded.profiled);
        submitted
    }

    /// Cull and encode the eyes `matrices` see, drawn into `targets`, for [`Self::submit`]: all of a frame's work for
    /// the CPU but the submission, so that the headset does it while the GPU still draws the frame before, and waits for
    /// that only to submit. What it writes to buffers is written as the next submission starts, after the frames
    /// already submitted have drawn, so the frame before draws as it was. See `design/rendering.md`.
    pub fn encode(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        targets: &[wgpu::TextureView],
        matrices: &[Mat4],
        scene_to_space: Mat4,
        panels: &[(&crate::ui_panel::Panel, Mat4)],
    ) -> Encoded {
        assert_eq!(targets.len(), matrices.len());
        assert!((1..=2).contains(&targets.len()));
        // The glyphs that this frame's meshes were built with, which may be new.
        self.glyphs.glyphs().sync(device, queue);
        // What the eyes see: once, before either is drawn, everything that neither can see is culled, against one frustum
        // enclosing both. See `design/rendering.md`.
        let tally = self.cull(queue, matrices, scene_to_space, panels);
        // The GPU's timestamps of frames it has finished are read back as they are mapped.
        let _ = device.poll(wgpu::PollType::Poll);
        let profiled = self.profiler.begin(targets.len());
        self.profiler.tally(tally);
        self.tally = tally;
        let glyphs = self.glyphs.binding();
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("swarm frame"),
        });
        let multisampled = self.multisampled.as_ref();
        // Which panels either eye may see, in the frame step's order.
        let visible = self.world.seen(self.submitted.panels);
        for eye in 0..targets.len() {
            let scene_to_clip = matrices[eye] * scene_to_space;
            if let Some(scenery) = self.scenery() {
                // The scenery draws in the environment's own coordinates, carried into the space's, and is told where
                // each eye is in them.
                let scenery_to_clip = scene_to_clip * self.carriage;
                let position = crate::scenery::eye(scenery_to_clip);
                scenery.prepare_eye(queue, eye, scenery_to_clip, position);
                scenery.encode(&mut encoder, eye);
            }
            // An avatar draws in the space's.
            if let Some(scenery) = self.avatar() {
                scenery.prepare_eye(
                    queue,
                    eye,
                    scene_to_clip,
                    crate::scenery::eye(scene_to_clip),
                );
                scenery.encode(&mut encoder, eye);
            }
            // Separate buffers are important: two writes to one buffer before
            // submission would make both eyes use the right-eye transform. Everything is hazed as the environment's
            // scenery is, in its coordinates, which its carriage moves without stretching, so that distances are the
            // same in the space's.
            let haze = self.haze(crate::scenery::eye(scene_to_clip));
            queue.write_buffer(
                &self.cameras[eye],
                0,
                bytemuck::cast_slice(&camera(scene_to_clip, haze, self.size.1)),
            );
            // Each panel's model is in the same space as the eyes' matrices.
            let seen = crate::ui_panel::Seen::new(matrices[eye], haze);
            for ((panel, model), _) in panels.iter().zip(visible).filter(|(_, seen)| **seen) {
                panel.prepare_seen(queue, eye, &seen, *model);
            }
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("eye"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: multisampled.map_or(&targets[eye], |views| &views[eye]),
                    resolve_target: multisampled.map(|_| &targets[eye]),
                    depth_slice: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color {
                            r: 0.006,
                            g: 0.009,
                            b: 0.025,
                            a: 1.0,
                        }),
                        // Only the resolved image is needed once the samples are resolved.
                        store: if multisampled.is_some() {
                            wgpu::StoreOp::Discard
                        } else {
                            wgpu::StoreOp::Store
                        },
                    },
                })],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: &self.depth[eye],
                    depth_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Clear(1.0),
                        store: wgpu::StoreOp::Discard,
                    }),
                    stencil_ops: None,
                }),
                timestamp_writes: self.profiler.pass(profiled, eye),
                occlusion_query_set: None,
                multiview_mask: None,
            });
            for scenery in self.scenery().into_iter().chain(self.avatar()) {
                scenery.draw(&mut pass, eye);
            }
            self.profiler.mark(&mut pass, profiled, eye, 0);
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &self.camera_bindings[eye], &[]);
            pass.set_vertex_buffer(0, self.vertices.slice(..));
            pass.set_vertex_buffer(1, self.instances.slice(..));
            pass.draw(0..self.vertex_count, 0..self.shown.len() as u32);
            // Volumes' solids, by the same pipeline, are opaque bodies too.
            self.solids.draw(&mut pass);
            self.controllers.draw(&mut pass);
            self.profiler.mark(&mut pass, profiled, eye, 1);
            for scenery in self.scenery().into_iter().chain(self.avatar()) {
                scenery.draw_translucent(&mut pass, eye);
            }
            self.profiler.mark(&mut pass, profiled, eye, 2);
            // Workers' signs and the space's panels are translucent, so they are drawn after the opaque scene, against its
            // depth, back to front from this eye: each in the order it keeps among its own, the signs' by where they
            // overlap and the panels' as the frame step gave them, farthest first, and between the two whichever is
            // farther, so that a translucent wall's board behind a sign is drawn before it and shows about its rounded
            // corners. Each opaque enough to hide what is behind it then writes its backing's depth where it is itself
            // opaque enough, so that what is behind it is hidden though drawn after it.
            let signs = self.workspace.order(scene_to_clip);
            let ahead = |model: &Mat4| (matrices[eye] * *model).w_axis.w;
            let (mut sign, mut panel) = (0, 0);
            let mut drawing_panels = false;
            while sign < signs.len() || panel < panels.len() {
                // A panel neither eye can see is not drawn, and the rest keep their order.
                if panel < panels.len() && !visible[panel] {
                    panel += 1;
                    continue;
                }
                let next_sign = sign < signs.len()
                    && panels
                        .get(panel)
                        .is_none_or(|(_, model)| signs[sign].1 >= ahead(model));
                if next_sign {
                    self.workspace.draw_panel(
                        &mut pass,
                        &self.camera_bindings[eye],
                        &glyphs,
                        signs[sign].0,
                    );
                    drawing_panels = false;
                    sign += 1;
                    continue;
                }
                if !drawing_panels {
                    self.panels.set(&mut pass, &glyphs);
                    drawing_panels = true;
                }
                let (shown, _) = panels[panel];
                shown.draw(&mut pass, &self.panels, eye);
                // An image panel's image, over its face, before the panels in front of it.
                if self.images.draw(&mut pass, shown, eye) {
                    self.panels.set(&mut pass, &glyphs);
                }
                panel += 1;
            }
            self.profiler.mark(&mut pass, profiled, eye, 3);
            // Arrival's labels are drawn last: in a space they are the hand menu's, by the hand, nearer than the panels, so
            // they blend over those behind them, and are hidden by any in front of them, which has written its depth. Nothing
            // drawn after them needs theirs, so they write none.
            self.arrival
                .draw(&mut pass, &self.camera_bindings[eye], &glyphs);
            // A teleport's blink darkens everything, panels too, so that the view does not jump.
            if self.blink > 0.0 {
                let blink = f64::from(self.blink);
                pass.set_pipeline(&self.blink_pipeline);
                pass.set_blend_constant(wgpu::Color {
                    r: blink,
                    g: blink,
                    b: blink,
                    a: blink,
                });
                pass.draw(0..3, 0..1);
            }
        }
        self.profiler.resolve(&mut encoder, profiled);
        Encoded {
            commands: encoder.finish(),
            profiled,
        }
    }

    /// Cull, against one frustum enclosing all the eyes `matrices` see, what the scenery and the avatar draw, Tomori's
    /// cubes and `panels`, each submitted to the world in its own coordinates, writing the cubes seen for this frame, and
    /// say how much is drawn in each eye and culled.
    fn cull(
        &mut self,
        queue: &wgpu::Queue,
        matrices: &[Mat4],
        scene_to_space: Mat4,
        panels: &[(&crate::ui_panel::Panel, Mat4)],
    ) -> tomori_assets::bounds::Tally {
        use crate::scenery::Views;
        let environment = self.environment;
        let current = self
            .sceneries
            .iter()
            .position(|(registered, _)| *registered == environment);
        let Self {
            world,
            submitted,
            scenery_roots,
            sceneries,
            avatar,
            scene,
            shown,
            people,
            figures,
            ..
        } = self;
        world.set_culling(self.culling);
        world.set_views(matrices);
        // The panels are in the space's coordinates, as the eyes are; the cubes and the avatar in the scene's; the
        // environment's scenery in its own, carried into the scene's.
        world.set_transform(submitted.scene, scene_to_space);
        world.set_transform(submitted.avatar, scene_to_space);
        world.set_active(submitted.avatar, avatar.is_some());
        for (index, root) in scenery_roots.iter().enumerate() {
            world.set_active(*root, Some(index) == current);
        }
        world.place(
            submitted.cubes,
            scene.iter().map(|cube| Some(cube_bounds(cube))),
        );
        for (item, range) in figures.iter() {
            world.place(
                *item,
                people[range.clone()]
                    .iter()
                    .map(|cube| Some(cube_bounds(cube))),
            );
        }
        world.place(
            submitted.panels,
            panels
                .iter()
                .map(|(panel, model)| Some(panel.bounds().transformed(*model))),
        );
        let mut current = current.map(|index| (scenery_roots[index], &mut sceneries[index].1));
        if let Some((root, scenery)) = &mut current {
            world.set_transform(*root, scene_to_space * self.carriage);
            scenery.submit(world, *root);
        }
        if let Some((_, _, scenery)) = avatar {
            scenery.submit(world, submitted.avatar);
        }
        world.cull();
        let mut tally = tomori_assets::bounds::Tally::default();
        if let Some((root, scenery)) = current {
            tally += scenery.cull(queue, &Views::of(world, root));
        }
        if let Some((_, _, scenery)) = avatar {
            tally += scenery.cull(queue, &Views::of(world, submitted.avatar));
        }
        // Tomori's cubes, the swarm, workers, rings, pads and rays among them, in one draw.
        shown.clear();
        shown.extend(
            scene
                .iter()
                .zip(world.seen(submitted.cubes))
                .filter(|(_, seen)| **seen)
                .map(|(cube, _)| *cube),
        );
        let culled = shown.len() != scene.len();
        // The others, after the rest, each person seen whole and then cube by cube.
        let before = shown.len();
        for (item, range) in figures.iter() {
            shown.extend(
                people[range.clone()]
                    .iter()
                    .zip(world.seen(*item))
                    .filter(|(_, seen)| **seen)
                    .map(|(cube, _)| *cube),
            );
        }
        if culled || self.cubes_culled {
            queue.write_buffer(&self.instances, 0, bytemuck::cast_slice(shown));
        } else if shown.len() > before {
            queue.write_buffer(
                &self.instances,
                (before * size_of::<Instance>()) as u64,
                bytemuck::cast_slice(&shown[before..]),
            );
        }
        self.cubes_culled = culled;
        tally += tomori_assets::bounds::Tally {
            drawn: shown.len(),
            culled: scene.len() + people.len() - shown.len(),
            draws: 1,
            triangles: shown.len() * self.vertex_count as usize / 3,
        };
        // The panels, each a draw of its face, and its backing's into depth.
        let seen = world.seen(submitted.panels);
        // Of them, the projected panels' faces, apart from walls' boards, labels, cards and the rest, for the frame log.
        self.panels_seen = panels
            .iter()
            .zip(seen)
            .filter(|((panel, _), _)| panel.panel_face())
            .fold((0, 0), |(drawn, culled), (_, seen)| {
                if *seen {
                    (drawn + 1, culled)
                } else {
                    (drawn, culled + 1)
                }
            });
        for ((panel, _), seen) in panels.iter().zip(seen) {
            tally += if *seen {
                tomori_assets::bounds::Tally {
                    drawn: 1,
                    draws: 1,
                    triangles: panel.triangles(),
                    ..Default::default()
                }
            } else {
                tomori_assets::bounds::Tally {
                    culled: 1,
                    ..Default::default()
                }
            };
        }
        tally
    }
}

/// One triangle over the whole view, black, which the blend constant mixes in: what is drawn is kept by one less the constant.
const BLINK_SHADER: &str = r"
@vertex
fn vs_main(@builtin(vertex_index) index: u32) -> @builtin(position) vec4<f32> {
    let x = f32((index << 1u) & 2u) * 2.0 - 1.0;
    let y = f32(index & 2u) * 2.0 - 1.0;
    return vec4<f32>(x, y, 0.0, 1.0);
}

@fragment
fn fs_main() -> @location(0) vec4<f32> {
    return vec4<f32>(0.0, 0.0, 0.0, 1.0);
}
";

/// The pipeline that darkens the whole view by the blend constant, over whatever was drawn, ignoring depth.
fn blink_pipeline(
    device: &wgpu::Device,
    format: wgpu::TextureFormat,
    samples: u32,
) -> wgpu::RenderPipeline {
    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("blink"),
        source: wgpu::ShaderSource::Wgsl(BLINK_SHADER.into()),
    });
    let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("blink pipeline layout"),
        bind_group_layouts: &[],
        immediate_size: 0,
    });
    let darken = wgpu::BlendComponent {
        src_factor: wgpu::BlendFactor::Zero,
        dst_factor: wgpu::BlendFactor::OneMinusConstant,
        operation: wgpu::BlendOperation::Add,
    };
    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("blink pipeline"),
        layout: Some(&layout),
        vertex: wgpu::VertexState {
            module: &shader,
            entry_point: Some("vs_main"),
            compilation_options: Default::default(),
            buffers: &[],
        },
        fragment: Some(wgpu::FragmentState {
            module: &shader,
            entry_point: Some("fs_main"),
            compilation_options: Default::default(),
            targets: &[Some(wgpu::ColorTargetState {
                format,
                blend: Some(wgpu::BlendState {
                    color: darken,
                    alpha: darken,
                }),
                write_mask: wgpu::ColorWrites::ALL,
            })],
        }),
        primitive: wgpu::PrimitiveState::default(),
        depth_stencil: Some(wgpu::DepthStencilState {
            format: wgpu::TextureFormat::Depth32Float,
            depth_write_enabled: Some(false),
            depth_compare: Some(wgpu::CompareFunction::Always),
            stencil: Default::default(),
            bias: Default::default(),
        }),
        multisample: wgpu::MultisampleState {
            count: samples,
            ..Default::default()
        },
        multiview_mask: None,
        cache: None,
    })
}

/// The ring where a teleport would land: on the surface at `landing`, [`LANDING_TOP`] above it and [`LANDING_SUNK`]
/// into it, in the hover highlight's colour.
fn landing_ring(landing: glam::Vec3, scene: &mut Vec<Instance>) {
    for n in 0..LANDING_CUBES {
        let angle = n as f32 * std::f32::consts::TAU / LANDING_CUBES as f32;
        scene.push(Instance {
            position: [
                landing.x + angle.cos() * LANDING_RADIUS,
                landing.y + (LANDING_TOP - LANDING_SUNK) * 0.5,
                landing.z + angle.sin() * LANDING_RADIUS,
            ],
            scale: [0.04, LANDING_TOP + LANDING_SUNK, 0.04],
            color: [0.45, 0.95, 1.0],
            ..Default::default()
        });
    }
}

/// A teleport pad as Tomori draws it for every environment: a ring of bars flat on the floor, about a dim plate, and,
/// over it, two chevrons pointing up or down where the pad paired with it stands a storey higher or lower, or a
/// diamond where it is level; pointed at, the ring brightens to the hover highlight's colour and a second ring stands
/// about it, as the ring where a teleport would land does.
fn pad_cubes(pad: &tomori_scene::pads::Shown, scene: &mut Vec<Instance>) {
    use glam::{Mat3, Quat, Vec3};
    let (centre, radius) = (pad.centre, pad.radius);
    let (ring, plate, chevron) = if pad.highlighted {
        ([0.45, 0.95, 1.0], [0.05, 0.22, 0.28], [0.6, 1.0, 1.0])
    } else {
        ([0.12, 0.6, 0.75], [0.025, 0.1, 0.14], [0.25, 0.75, 0.9])
    };
    let turned = |angle: f32| Mat3::from_quat(Quat::from_rotation_y(angle)).to_cols_array_2d();
    // A ring of bars, each along the circle, `across` thick.
    let mut circle = |radius: f32, across: f32, color: [f32; 3]| {
        let chord = std::f32::consts::TAU * radius / PAD_RING as f32;
        for n in 0..PAD_RING {
            let angle = n as f32 * std::f32::consts::TAU / PAD_RING as f32;
            scene.push(Instance {
                position: (centre + Vec3::new(angle.cos() * radius, 0.012, angle.sin() * radius))
                    .to_array(),
                scale: [across, 0.012, chord * 0.8],
                color,
                basis: turned(-angle),
                glow: 0.0,
            });
        }
    };
    circle(radius, 0.04, ring);
    if pad.highlighted {
        circle(radius + 0.08, 0.025, ring);
    }
    // Squares turned in steps make a plate within the ring whose corners reach just inside it.
    for n in 0..PAD_PLATE {
        let side = radius * 0.96 * std::f32::consts::SQRT_2;
        scene.push(Instance {
            position: (centre + Vec3::Y * 0.004).to_array(),
            scale: [side, 0.006, side],
            color: plate,
            basis: turned(n as f32 * std::f32::consts::FRAC_PI_2 / PAD_PLATE as f32),
            glow: 0.0,
        });
    }
    // Chevrons pointing the way the pad paired with it goes, each of four bars rising, or falling, in to its middle from
    // four sides, so that it reads from anywhere around it; or, level, a diamond.
    let size = radius.min(0.5) * 0.5;
    let Some(up) = pad.up else {
        scene.push(Instance {
            position: (centre + Vec3::Y * 0.4).to_array(),
            scale: [0.1; 3],
            color: chevron,
            basis: Mat3::from_quat(
                Quat::from_rotation_z(std::f32::consts::FRAC_PI_4)
                    * Quat::from_rotation_x(std::f32::consts::FRAC_PI_4),
            )
            .to_cols_array_2d(),
            glow: 0.0,
        });
        return;
    };
    // Turned about its length's middle, a bar along +X rises at its outer end for a positive lean.
    let lean = if up { -0.75_f32 } else { 0.75 };
    for step in 0..PAD_CHEVRONS {
        let height = 0.3 + step as f32 * 0.2;
        for side in 0..PAD_CHEVRON {
            let around =
                Quat::from_rotation_y(side as f32 * std::f32::consts::TAU / PAD_CHEVRON as f32);
            let outward = around * Vec3::X * (size * 0.5 * lean.cos());
            scene.push(Instance {
                position: (centre + Vec3::Y * height + outward).to_array(),
                scale: [size, 0.035, 0.035],
                color: chevron,
                basis: Mat3::from_quat(around * Quat::from_rotation_z(lean)).to_cols_array_2d(),
                glow: 0.0,
            });
        }
    }
}

/// The parts of the line from `start` to `end` outside the gap from `centre - half` to `centre + half`.
fn outside(start: f32, end: f32, gap: Option<(f32, f32)>) -> impl Iterator<Item = (f32, f32)> {
    let parts = match gap {
        Some((centre, half)) => [
            (start, (centre - half).min(end)),
            ((centre + half).max(start), end),
        ],
        None => [(start, end), (end, end)],
    };
    parts.into_iter().filter(|(a, b)| b - a > 1e-4)
}

/// The grid, without the part inside `reveal`, where a space's environment is growing. The sphere meets the floor in a circle, which each line skips by its chord, so this costs nothing on the GPU.
fn minimal_floor(scene: &mut Vec<Instance>, reveal: Option<crate::arrival_motion::Sphere>) {
    // Original stationary grid retained as a regression/comfort baseline.
    let circle = reveal.and_then(|sphere| {
        let squared = sphere.radius * sphere.radius - sphere.centre.y * sphere.centre.y;
        (squared > 0.0).then_some((sphere.centre.x, sphere.centre.z, squared))
    });
    // The gap a line `offset` from the circle's centre crosses, as its centre and half chord along the line.
    let gap = |offset: f32, along: f32| {
        circle.and_then(|(_, _, squared)| {
            let half = squared - offset * offset;
            (half > 0.0).then(|| (along, half.sqrt()))
        })
    };
    let (cx, cz) = circle.map_or((0.0, 0.0), |(x, z, _)| (x, z));
    for n in -10..=10 {
        let f = n as f32 * 0.5;
        let color = if n == 0 {
            [0.08, 0.22, 0.3]
        } else {
            [0.025, 0.065, 0.09]
        };
        for (a, b) in outside(-7.5, 2.5, gap(f - cx, cz)) {
            scene.push(Instance {
                position: [f, 0.0, (a + b) * 0.5],
                scale: [0.008, 0.004, b - a],
                color,
                ..Default::default()
            });
        }
        for (a, b) in outside(-5.0, 5.0, gap(f - 2.5 - cz, cx)) {
            scene.push(Instance {
                position: [(a + b) * 0.5, 0.0, f - 2.5],
                scale: [b - a, 0.004, 0.008],
                color,
                ..Default::default()
            });
        }
    }
}

fn valid_workspace_cube(cube: &crate::delegation::Cube) -> bool {
    cube.valid()
}

fn worker_instance(cube: &crate::delegation::Cube) -> Option<Instance> {
    valid_workspace_cube(cube).then(|| Instance {
        position: cube.position,
        scale: cube.scale,
        color: cube.color,
        basis: glam::Mat3::from_quat(glam::Quat::from_array(cube.orientation).normalize())
            .to_cols_array_2d(),
        glow: 0.0,
    })
}

fn swarm_instances(swarm: &Swarm, scene: &mut Vec<Instance>) {
    let visual = swarm.visual;
    let base = visual.color();
    for (i, agent) in swarm.agents.iter().enumerate() {
        let t = i as f32 / AGENT_COUNT as f32;
        let wave = ((t * std::f32::consts::TAU * 3.0 - visual.time * 5.0).sin() * 0.5 + 0.5)
            * visual.amplitude();
        let (scale, color) = if visual.mode == Mode::Neutral {
            (0.028, [0.12 + 0.5 * t, 0.85 - 0.25 * t, 1.0])
        } else {
            let gain = if visual.mode == Mode::Speaking {
                0.65 + wave * 0.35
            } else {
                0.75 + t * 0.25
            };
            (
                0.028
                    + if visual.mode == Mode::Speaking {
                        wave * 0.024
                    } else {
                        0.0
                    },
                base.map(|v| v * gain),
            )
        };
        scene.push(Instance {
            position: agent.position.to_array(),
            scale: [scale; 3],
            color,
            ..Default::default()
        });
    }
    if visual.mode == Mode::Neutral || swarm.agents.is_empty() {
        return;
    }
    // A steady complete halo means ready. Gaps/motion distinguish other states
    // even without reading color. Keep this in scene coordinates, not head-locked.
    for i in 0..48 {
        let visible = match visual.mode {
            Mode::Ready | Mode::Speaking | Mode::Listening => true,
            Mode::Waiting => i % 24 < 17,
            Mode::Unknown | Mode::Unavailable => i % 12 < 3,
            _ => i % 12 < 8,
        };
        if !visible {
            continue;
        }
        let moving = matches!(
            visual.mode,
            Mode::Working | Mode::Preparing | Mode::Transcribing
        );
        let angle =
            i as f32 * std::f32::consts::TAU / 48.0 + if moving { visual.time * 0.45 } else { 0.0 };
        let pulse = if visual.mode == Mode::Waiting {
            0.78 + 0.22 * (visual.time * 2.0).sin()
        } else {
            1.0
        };
        // When speech and observed session work overlap, a short violet arc
        // remains active. It does NOT claim visibility into child sessions.
        let work_arc = visual.working
            && visual.mode != Mode::Working
            && ((i as f32 - visual.time * 7.0).rem_euclid(48.0) < 6.0);
        let color = if work_arc {
            [0.45, 0.35, 1.0]
        } else {
            base.map(|v| v * pulse)
        };
        scene.push(Instance {
            // Behind the swarm's middle, as it faces, wherever it stands.
            position: swarm
                .at(glam::Vec3::new(
                    angle.cos() * 1.13,
                    angle.sin() * 0.70,
                    -0.28,
                ))
                .to_array(),
            scale: [0.025, 0.025, 0.025],
            color,
            ..Default::default()
        });
    }
}

pub(crate) fn cube() -> Vec<Vertex> {
    let mut vertices = Vec::with_capacity(36);
    for (normal, u, v) in [
        (glam::Vec3::X, glam::Vec3::Y, glam::Vec3::Z),
        (-glam::Vec3::X, glam::Vec3::Y, -glam::Vec3::Z),
        (glam::Vec3::Y, glam::Vec3::Z, glam::Vec3::X),
        (-glam::Vec3::Y, glam::Vec3::Z, -glam::Vec3::X),
        (glam::Vec3::Z, glam::Vec3::X, glam::Vec3::Y),
        (-glam::Vec3::Z, glam::Vec3::X, -glam::Vec3::Y),
    ] {
        for [a, b] in [
            [-1.0, -1.0],
            [1.0, -1.0],
            [1.0, 1.0],
            [-1.0, -1.0],
            [1.0, 1.0],
            [-1.0, 1.0],
        ] {
            vertices.push(Vertex {
                position: ((normal + u * a + v * b) * 0.5).to_array(),
                normal: normal.to_array(),
            });
        }
    }
    vertices
}

#[cfg(test)]
mod distance;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_gpu;

    #[test]
    fn every_shader_is_valid_wgsl() {
        let files = test_gpu::wgsl_files(concat!(env!("CARGO_MANIFEST_DIR"), "/src"));
        let names: Vec<_> = files.iter().map(|(name, _)| name.as_str()).collect();
        // A new shader is added here, with how it is compiled.
        assert_eq!(
            names,
            [
                "caption.wgsl",
                "controllers.wgsl",
                "panel_image.wgsl",
                "swarm.wgsl",
                "ui_panel.wgsl",
                "workspace.wgsl"
            ]
        );
        for (name, source) in &files {
            test_gpu::validate_wgsl(name, source);
        }
        // The swarm and workers are drawn with each instance's basis spliced into `swarm.wgsl`.
        test_gpu::validate_wgsl("swarm.wgsl, oriented", &oriented_shader());
    }

    #[test]
    fn activity_geometry_is_bounded_and_ready_halo_is_persistent() {
        let mut swarm = Swarm::default();
        for mode in [
            Mode::Neutral,
            Mode::Ready,
            Mode::Working,
            Mode::Preparing,
            Mode::Speaking,
            Mode::Waiting,
            Mode::Listening,
            Mode::Transcribing,
            Mode::Unknown,
            Mode::Unavailable,
        ] {
            swarm.visual = crate::interaction::Visual {
                mode,
                level: 0.8,
                time: 2.0,
                working: true,
            };
            let mut scene = Vec::new();
            swarm_instances(&swarm, &mut scene);
            assert!(scene.len() + 83 <= MAX_INSTANCES);
            assert!(scene.iter().all(|instance| {
                instance
                    .position
                    .iter()
                    .chain(&instance.scale)
                    .chain(&instance.color)
                    .all(|v| v.is_finite())
            }));
            if mode == Mode::Ready {
                assert_eq!(scene.len(), AGENT_COUNT + 48);
                let halo = scene[AGENT_COUNT..]
                    .iter()
                    .map(|v| (v.position, v.scale))
                    .collect::<Vec<_>>();
                swarm.visual.time = 12.0;
                let mut later = Vec::new();
                swarm_instances(&swarm, &mut later);
                assert_eq!(
                    halo,
                    later[AGENT_COUNT..]
                        .iter()
                        .map(|v| (v.position, v.scale))
                        .collect::<Vec<_>>()
                );
            }
        }
    }

    #[test]
    fn conversational_states_render_distinct_images() {
        test_gpu::with(|test_gpu::Gpu { device, queue, .. }| {
            pollster::block_on(async move {
                let scope = device.push_error_scope(wgpu::ErrorFilter::Validation);
                let texture = device.create_texture(&wgpu::TextureDescriptor {
                    label: Some("conversation signal test"),
                    size: wgpu::Extent3d {
                        width: 512,
                        height: 384,
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
                let mut renderer = Renderer::new(
                    &device,
                    &queue,
                    wgpu::TextureFormat::Rgba8UnormSrgb,
                    512,
                    384,
                );
                let camera =
                    glam::camera::rh::proj::directx::perspective(1.1, 512.0 / 384.0, 0.05, 50.0)
                        * Mat4::from_translation(glam::Vec3::new(0.0, -1.6, 0.0));
                let mut images = Vec::new();
                for mode in [
                    Mode::Working,
                    Mode::Preparing,
                    Mode::Speaking,
                    Mode::Ready,
                    Mode::Waiting,
                    Mode::Listening,
                ] {
                    let mut swarm = Swarm::default();
                    for _ in 0..240 {
                        swarm.set_visual(
                            crate::interaction::Visual {
                                mode,
                                level: 0.8,
                                ..Default::default()
                            },
                            1.0 / 72.0,
                        );
                        swarm.step(1.0 / 72.0, None);
                    }
                    renderer.update(&queue, &swarm, None, false);
                    renderer.render(
                        &device,
                        &queue,
                        std::slice::from_ref(&view),
                        &[camera],
                        Mat4::IDENTITY,
                        &[],
                    );
                    let buffer = device.create_buffer(&wgpu::BufferDescriptor {
                        label: Some("signal readback"),
                        size: 512 * 384 * 4,
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
                                bytes_per_row: Some(512 * 4),
                                rows_per_image: Some(384),
                            },
                        },
                        wgpu::Extent3d {
                            width: 512,
                            height: 384,
                            depth_or_array_layers: 1,
                        },
                    );
                    queue.submit([encoder.finish()]);
                    let (tx, rx) = std::sync::mpsc::channel();
                    buffer
                        .slice(..)
                        .map_async(wgpu::MapMode::Read, move |result| {
                            tx.send(result).unwrap();
                        });
                    device.poll(wgpu::PollType::wait_indefinitely()).unwrap();
                    rx.recv().unwrap().unwrap();
                    let image = buffer.slice(..).get_mapped_range().unwrap().to_vec();
                    buffer.unmap();
                    assert!(
                        images.iter().all(|previous| previous != &image),
                        "{mode:?} must be visually distinct"
                    );
                    images.push(image);
                }
                {
                    let mut sheet = image::RgbaImage::new(512 * 3, 384 * 2);
                    for (i, bytes) in images.into_iter().enumerate() {
                        let image = image::RgbaImage::from_raw(512, 384, bytes).unwrap();
                        image::imageops::replace(
                            &mut sheet,
                            &image,
                            (i % 3) as i64 * 512,
                            (i / 3) as i64 * 384,
                        );
                    }
                    crate::test_support::preview("TOMORI_SIGNAL_PREVIEW", &sheet);
                }
                assert!(scope.pop().await.is_none());
            })
        });
    }

    #[test]
    fn both_controller_poses_change_the_offscreen_image() {
        test_gpu::with(|test_gpu::Gpu { device, queue, .. }| {
            pollster::block_on(async move {
                let error_scope = device.push_error_scope(wgpu::ErrorFilter::Validation);
                let texture = device.create_texture(&wgpu::TextureDescriptor {
                    label: Some("controller test target"),
                    size: wgpu::Extent3d {
                        width: 128,
                        height: 128,
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
                let mut renderer = Renderer::new(
                    &device,
                    &queue,
                    wgpu::TextureFormat::Rgba8UnormSrgb,
                    128,
                    128,
                );
                let mut swarm = Swarm::default();
                swarm.agents.clear();
                renderer.update(&queue, &swarm, None, false);
                let capture = |renderer: &mut Renderer| {
                    renderer.render(
                        &device,
                        &queue,
                        std::slice::from_ref(&view),
                        &[Mat4::IDENTITY],
                        Mat4::IDENTITY,
                        &[],
                    );
                    let buffer = device.create_buffer(&wgpu::BufferDescriptor {
                        label: Some("controller test readback"),
                        size: 128 * 128 * 4,
                        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
                        mapped_at_creation: false,
                    });
                    let mut encoder = device.create_command_encoder(&Default::default());
                    encoder.copy_texture_to_buffer(
                        texture.as_image_copy(),
                        wgpu::TexelCopyBufferInfo {
                            buffer: &buffer,
                            layout: wgpu::TexelCopyBufferLayout {
                                offset: 0,
                                bytes_per_row: Some(128 * 4),
                                rows_per_image: Some(128),
                            },
                        },
                        wgpu::Extent3d {
                            width: 128,
                            height: 128,
                            depth_or_array_layers: 1,
                        },
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
                    data
                };
                let empty = capture(&mut renderer);
                renderer.update_controllers(
                    &queue,
                    [
                        Some(crate::controller::Pose {
                            position: [-0.45, 0.0, 0.5],
                            orientation: [0.0, 0.0, 0.0, 1.0],
                        }),
                        Some(crate::controller::Pose {
                            position: [0.45, 0.0, 0.5],
                            orientation: [0.0, 0.0, 0.0, 1.0],
                        }),
                    ],
                    [None, None],
                );
                let with_controllers = capture(&mut renderer);
                let changed = |start: usize, end: usize| {
                    (0..128).any(|y| {
                        (start..end).any(|x| {
                            let offset = (y * 128 + x) * 4;
                            empty[offset..offset + 4] != with_controllers[offset..offset + 4]
                        })
                    })
                };
                assert!(changed(0, 64), "left controller should render");
                assert!(changed(64, 128), "right controller should render");
                let make_hand = |x| {
                    let mut hand = crate::hands::Hand {
                        joints: [None; crate::hands::JOINT_COUNT],
                    };
                    hand.joints[1] = Some(crate::hands::Joint {
                        position: [x, -0.1, 0.5],
                        radius: 0.01,
                    });
                    hand.joints[2] = Some(crate::hands::Joint {
                        position: [x, 0.1, 0.5],
                        radius: 0.01,
                    });
                    hand
                };
                renderer.update_controllers(
                    &queue,
                    [None, None],
                    [Some(make_hand(-0.45)), Some(make_hand(0.45))],
                );
                let with_hands = capture(&mut renderer);
                let hand_changed = |start: usize, end: usize| {
                    (0..128).any(|y| {
                        (start..end).any(|x| {
                            let offset = (y * 128 + x) * 4;
                            empty[offset..offset + 4] != with_hands[offset..offset + 4]
                        })
                    })
                };
                assert!(hand_changed(0, 64), "left hand should render");
                assert!(hand_changed(64, 128), "right hand should render");
                // A model the runtime provides replaces the bundled one, on its own hand only; an empty one is ignored.
                let pose = |x| {
                    Some(crate::controller::Pose {
                        position: [x, 0.0, 0.5],
                        orientation: [0.0, 0.0, 0.0, 1.0],
                    })
                };
                renderer.update_controllers(&queue, [pose(-0.45), pose(0.45)], [None, None]);
                assert_eq!(capture(&mut renderer), with_controllers);
                let vertex = |x, y| crate::controller::Vertex {
                    position: [x, y, 0.0],
                    normal: [0.0, 0.0, -1.0],
                    color: [1.0, 0.1, 0.1],
                    uv: [0.0, 0.0],
                };
                let triangle = ControllerModel {
                    vertices: vec![vertex(-0.3, -0.3), vertex(0.3, -0.3), vertex(0.0, 0.3)],
                    parts: vec![crate::controller::Part {
                        start: 0,
                        count: 3,
                        texture: None,
                    }],
                    textures: Vec::new(),
                };
                renderer.set_controller_model(&device, &queue, 0, &triangle);
                let replaced = capture(&mut renderer);
                let differs = |a: &[u8], b: &[u8], start: usize, end: usize| {
                    (0..128).any(|y| {
                        (start..end).any(|x| {
                            let offset = (y * 128 + x) * 4;
                            a[offset..offset + 4] != b[offset..offset + 4]
                        })
                    })
                };
                assert!(
                    differs(&replaced, &with_controllers, 0, 64),
                    "the left model should change"
                );
                assert!(
                    !differs(&replaced, &with_controllers, 64, 128),
                    "the right model should not change"
                );
                let empty_model = ControllerModel {
                    vertices: Vec::new(),
                    parts: Vec::new(),
                    textures: Vec::new(),
                };
                renderer.set_controller_model(&device, &queue, 1, &empty_model);
                renderer.set_controller_model(&device, &queue, 2, &triangle);
                assert_eq!(
                    capture(&mut renderer),
                    replaced,
                    "an empty model or unknown hand is ignored"
                );

                // Without tracked controllers, the desktop's pointer is a cube that turns orange while it attracts the swarm.
                renderer.update_controllers(&queue, [None, None], [None, None]);
                renderer.update(&queue, &swarm, Some(glam::Vec3::new(0.0, 0.0, 0.5)), false);
                let pointer = capture(&mut renderer);
                renderer.update(&queue, &swarm, Some(glam::Vec3::new(0.0, 0.0, 0.5)), true);
                let attracting = capture(&mut renderer);
                assert_ne!(pointer, empty, "the pointer should render");
                assert_ne!(
                    pointer, attracting,
                    "attracting should change the pointer's colour"
                );
                assert!(error_scope.pop().await.is_none());
            })
        });
    }

    #[test]
    fn workspace_pages_render_stereo_recenter_and_clear() {
        test_gpu::with(|test_gpu::Gpu { device, queue, .. }| {
            pollster::block_on(async move {
                let scope = device.push_error_scope(wgpu::ErrorFilter::Validation);
                let textures: [_; 2] = std::array::from_fn(|_| {
                    device.create_texture(&wgpu::TextureDescriptor {
                        label: Some("workspace test eye"),
                        size: wgpu::Extent3d {
                            width: 256,
                            height: 256,
                            depth_or_array_layers: 1,
                        },
                        mip_level_count: 1,
                        sample_count: 1,
                        dimension: wgpu::TextureDimension::D2,
                        format: wgpu::TextureFormat::Rgba8UnormSrgb,
                        usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                            | wgpu::TextureUsages::COPY_SRC,
                        view_formats: &[],
                    })
                });
                let views: [_; 2] =
                    std::array::from_fn(|i| textures[i].create_view(&Default::default()));
                let mut renderer = Renderer::new(
                    &device,
                    &queue,
                    wgpu::TextureFormat::Rgba8UnormSrgb,
                    256,
                    256,
                );
                let mut swarm = Swarm::default();
                swarm.agents.clear();
                renderer.update(&queue, &swarm, None, false);
                let projection = glam::camera::rh::proj::directx::perspective(1.0, 1.0, 0.05, 50.0);
                let direction = (glam::Vec3::new(-3.5, 2.4, -3.35)
                    - glam::Vec3::new(0.0, 1.6, 0.0))
                .normalize();
                let matrices = [-0.032, 0.032].map(|x| {
                    let eye = glam::Vec3::new(x, 1.6, 0.0);
                    projection
                        * glam::camera::rh::view::look_at_mat4(eye, eye + direction, glam::Vec3::Y)
                });
                let capture =
                    |renderer: &mut Renderer, matrices: [Mat4; 2], scene_to_space: Mat4| {
                        renderer.render(&device, &queue, &views, &matrices, scene_to_space, &[]);
                        let buffers: [_; 2] = std::array::from_fn(|_| {
                            device.create_buffer(&wgpu::BufferDescriptor {
                                label: Some("workspace test readback"),
                                size: 256 * 256 * 4,
                                usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
                                mapped_at_creation: false,
                            })
                        });
                        let mut encoder = device.create_command_encoder(&Default::default());
                        for i in 0..2 {
                            encoder.copy_texture_to_buffer(
                                textures[i].as_image_copy(),
                                wgpu::TexelCopyBufferInfo {
                                    buffer: &buffers[i],
                                    layout: wgpu::TexelCopyBufferLayout {
                                        offset: 0,
                                        bytes_per_row: Some(256 * 4),
                                        rows_per_image: Some(256),
                                    },
                                },
                                wgpu::Extent3d {
                                    width: 256,
                                    height: 256,
                                    depth_or_array_layers: 1,
                                },
                            );
                        }
                        queue.submit([encoder.finish()]);
                        buffers
                            .iter()
                            .map(|buffer| {
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
                                data
                            })
                            .collect::<Vec<_>>()
                    };
                let empty = capture(&mut renderer, matrices, Mat4::IDENTITY);
                let workspace = crate::delegation::Workspace {
                    presentations: (0..5)
                        .map(|i| crate::delegation::Presentation {
                            id: format!("card-{i}"),
                            title: format!("Synthetic result {i}"),
                            source_id: "worker".into(),
                            source_message_id: "message".into(),
                            text: if i == 0 {
                                format!("{}{}", "A".repeat(44 * 14), "B".repeat(44 * 14))
                            } else {
                                format!("Example content for page entry {i}")
                            },
                            ..Default::default()
                        })
                        .collect(),
                    ..Default::default()
                };
                renderer.update_workspace(&device, &queue, &[], &[], &workspace, None);
                assert_eq!(renderer.presentation_page_count(), 5);
                let page0 = capture(&mut renderer, matrices, Mat4::IDENTITY);
                assert_ne!(page0[0], empty[0]);
                assert_ne!(page0[1], empty[1]);
                assert_ne!(page0[0], page0[1], "each eye uses its own matrix");
                renderer.set_presentation_content_page(1);
                renderer.update_workspace(&device, &queue, &[], &[], &workspace, None);
                let content_page1 = capture(&mut renderer, matrices, Mat4::IDENTITY);
                for eye in 0..2 {
                    assert_ne!(page0[eye], content_page1[eye], "content page in eye {eye}");
                }
                renderer.set_presentation_content_page(0);
                renderer.update_workspace(&device, &queue, &[], &[], &workspace, None);
                let recenter = Mat4::from_translation(glam::Vec3::new(0.5, 0.0, 0.0));
                let compensated = capture(
                    &mut renderer,
                    matrices.map(|m| m * recenter.inverse()),
                    recenter,
                );
                for eye in 0..2 {
                    let changed = page0[eye]
                        .as_chunks::<4>()
                        .0
                        .iter()
                        .zip(compensated[eye].as_chunks::<4>().0.iter())
                        .filter(|(a, b)| a != b)
                        .count();
                    assert!(
                        changed < 256,
                        "recenter should preserve world-relative rendering: {changed} pixels"
                    );
                }
                renderer.set_presentation_page(1);
                renderer.update_workspace(&device, &queue, &[], &[], &workspace, None);
                let page1 = capture(&mut renderer, matrices, Mat4::IDENTITY);
                assert_ne!(page0[0], page1[0]);
                assert_ne!(page1[0], empty[0]);

                // Empty snapshots must not hide an independently retained/fading cube.
                let cube = crate::delegation::Cube {
                    position: [-3.5, 1.8, -3.0],
                    scale: [0.1; 3],
                    color: [0.5; 3],
                    orientation: glam::Quat::IDENTITY.to_array(),
                };
                let invalid = crate::delegation::Cube {
                    scale: [f32::NAN; 3],
                    ..cube
                };
                renderer.update_workspace(
                    &device,
                    &queue,
                    &[cube, invalid],
                    &[],
                    &Default::default(),
                    None,
                );
                assert_eq!(renderer.scene.len(), renderer.base_scene_len + 1);
                let unrotated = capture(&mut renderer, matrices, Mat4::IDENTITY);
                let rotated = crate::delegation::Cube {
                    orientation: glam::Quat::from_euler(glam::EulerRot::YXZ, 0.6, 0.35, 0.15)
                        .to_array(),
                    ..cube
                };
                renderer.update_workspace(
                    &device,
                    &queue,
                    &[rotated],
                    &[],
                    &Default::default(),
                    None,
                );
                assert_ne!(
                    unrotated,
                    capture(&mut renderer, matrices, Mat4::IDENTITY),
                    "orientation must reach both the vertex transform and lighting shader"
                );
                let invalid_rotation = crate::delegation::Cube {
                    orientation: [1.0; 4],
                    ..cube
                };
                renderer.update_workspace(
                    &device,
                    &queue,
                    &[invalid_rotation],
                    &[],
                    &Default::default(),
                    None,
                );
                assert_eq!(renderer.scene.len(), renderer.base_scene_len);
                renderer.update_workspace(&device, &queue, &[], &[], &Default::default(), None);
                assert_eq!(renderer.scene.len(), renderer.base_scene_len);
                assert_eq!(renderer.presentation_page_count(), 0);
                assert_eq!(capture(&mut renderer, matrices, Mat4::IDENTITY), empty);

                let label_workspace = crate::delegation::Workspace {
                    nodes: vec![crate::delegation::Node {
                        id: "worker".into(),
                        parent_id: Some("root".into()),
                        title: "Synthetic task".into(),
                        state: crate::delegation::State::Running,
                    }],
                    ..Default::default()
                };
                let mut workers = crate::delegation::Scene::new();
                for _ in 0..30 {
                    workers.update(Some("root"), &label_workspace, 0.05);
                }
                let label_target = glam::Vec3::from_array(workers.stations()[0].position)
                    + glam::Vec3::new(0.0, 0.35, 0.0);
                let direction = (label_target - glam::Vec3::new(0.0, 1.6, 0.0)).normalize();
                let label_matrices = [-0.032, 0.032].map(|x| {
                    let eye = glam::Vec3::new(x, 1.6, 0.0);
                    projection
                        * glam::camera::rh::view::look_at_mat4(eye, eye + direction, glam::Vec3::Y)
                });
                let without_label = capture(&mut renderer, label_matrices, Mat4::IDENTITY);
                renderer.update_workspace(
                    &device,
                    &queue,
                    &[],
                    workers.stations(),
                    &label_workspace,
                    None,
                );
                let with_label = capture(&mut renderer, label_matrices, Mat4::IDENTITY);
                for eye in 0..2 {
                    assert_ne!(
                        without_label[eye], with_label[eye],
                        "worker label in eye {eye}"
                    );
                }
                let mut faded = workers.stations().to_vec();
                faded[0].opacity = 0.4;
                renderer.update_workspace(&device, &queue, &[], &faded, &label_workspace, None);
                let faded_label = capture(&mut renderer, label_matrices, Mat4::IDENTITY);
                for eye in 0..2 {
                    assert_ne!(
                        with_label[eye], faded_label[eye],
                        "label transition in eye {eye}"
                    );
                }
                let mut sheet = image::RgbaImage::new(768, 512);
                for (row, state) in [
                    crate::delegation::State::Completed,
                    crate::delegation::State::Error,
                ]
                .into_iter()
                .enumerate()
                {
                    let workspace = crate::delegation::Workspace {
                        nodes: vec![crate::delegation::Node {
                            id: "synthetic-terminal".into(),
                            parent_id: Some("root".into()),
                            title: "Synthetic terminal".into(),
                            state,
                        }],
                        ..Default::default()
                    };
                    let mut scene = crate::delegation::Scene::new();
                    scene.update(Some("root"), &workspace, 0.0);
                    let anchor = glam::Vec3::from_array(scene.stations()[0].position);
                    let focus = glam::camera::rh::proj::directx::perspective(0.22, 1.0, 0.05, 50.0);
                    let matrices = [-0.032, 0.032].map(|x| {
                        focus
                            * glam::camera::rh::view::look_at_mat4(
                                glam::Vec3::new(x, 1.6, 0.0),
                                anchor,
                                glam::Vec3::Y,
                            )
                    });
                    let mut previous = None;
                    for column in 0..3 {
                        if column > 0 {
                            for _ in 0..120 {
                                scene.update(Some("root"), &workspace, 1.0 / 60.0);
                            }
                        }
                        assert_eq!(
                            scene.cubes().len(),
                            if state == crate::delegation::State::Completed {
                                1
                            } else {
                                24
                            }
                        );
                        renderer.update_workspace(
                            &device,
                            &queue,
                            scene.cubes(),
                            &[],
                            &Default::default(),
                            None,
                        );
                        assert_eq!(
                            renderer.scene.len(),
                            renderer.base_scene_len + scene.cubes().len()
                        );
                        let image = capture(&mut renderer, matrices, Mat4::IDENTITY);
                        if let Some(previous) = &previous {
                            assert_ne!(
                                *previous, image,
                                "actual terminal ambient motion must reach the renderer"
                            );
                        }
                        let tile = image::RgbaImage::from_raw(256, 256, image[0].clone()).unwrap();
                        image::imageops::replace(&mut sheet, &tile, column * 256, row as i64 * 256);
                        previous = Some(image);
                    }
                }
                crate::test_support::preview("TOMORI_TERMINAL_PREVIEW", &sheet);
                device.poll(wgpu::PollType::wait_indefinitely()).unwrap();
                assert!(scope.pop().await.is_none());
            })
        });
    }

    /// Set `TOMORI_JAPANESE_PREVIEW` to a PNG path to save what this renders: a result card and a worker's sign with Japanese titles, close up, and the sign from where it stands.
    #[test]
    fn japanese_signs_and_cards_draw_their_titles_whole() {
        const SIZE: u32 = 512;
        test_gpu::with(|test_gpu::Gpu { device, queue, .. }| {
            let scope = device.push_error_scope(wgpu::ErrorFilter::Validation);
            let texture = device.create_texture(&wgpu::TextureDescriptor {
                label: Some("japanese test eye"),
                size: wgpu::Extent3d {
                    width: SIZE,
                    height: SIZE,
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
            let mut renderer = Renderer::new(
                &device,
                &queue,
                wgpu::TextureFormat::Rgba8UnormSrgb,
                SIZE,
                SIZE,
            );
            let mut swarm = Swarm::default();
            swarm.agents.clear();
            renderer.update(&queue, &swarm, None, false);
            let capture = |renderer: &mut Renderer, matrix: Mat4| {
                renderer.render(
                    &device,
                    &queue,
                    std::slice::from_ref(&view),
                    &[matrix],
                    Mat4::IDENTITY,
                    &[],
                );
                let buffer = device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("japanese test readback"),
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
                buffer.slice(..).map_async(wgpu::MapMode::Read, |_| {});
                device.poll(wgpu::PollType::wait_indefinitely()).unwrap();
                let data = buffer.slice(..).get_mapped_range().unwrap().to_vec();
                image::RgbaImage::from_raw(SIZE, SIZE, data).unwrap()
            };
            let eye = glam::Vec3::new(0.0, 1.6, 0.0);
            let look = |target: glam::Vec3, fov: f32| {
                glam::camera::rh::proj::directx::perspective(fov, 1.0, 0.05, 50.0)
                    * glam::camera::rh::view::look_at_mat4(eye, target, glam::Vec3::Y)
            };
            let workspace = crate::delegation::Workspace {
                nodes: vec![crate::delegation::Node {
                    id: "worker".into(),
                    parent_id: Some("root".into()),
                    title:
                        "東京の天気を調べて、週末の予定を立てる。Tomoriは日本語も表示できます 🎉"
                            .into(),
                    state: crate::delegation::State::Completed,
                }],
                presentations: vec![crate::delegation::Presentation {
                    id: "card".into(),
                    title: "検索結果：東京の天気と週末の予定について、詳しくまとめました".into(),
                    source_id: "ワーカー".into(),
                    source_message_id: "message".into(),
                    text: "Sunny on Saturday, rain on Sunday.".into(),
                    ..Default::default()
                }],
                ..Default::default()
            };
            let mut scene = crate::delegation::Scene::new();
            for _ in 0..30 {
                scene.update(Some("root"), &workspace, 0.05);
            }
            renderer.update_workspace(&device, &queue, &[], scene.stations(), &workspace, None);
            let station = glam::Vec3::from_array(scene.stations()[0].position);
            let card = look(glam::Vec3::new(-3.4, 2.45, -3.35), 0.5);
            let sign = look(station + glam::Vec3::new(0.0, 0.32, 0.0), 0.32);
            let far = look(station + glam::Vec3::new(0.0, 0.32, 0.0), 1.0);
            let tiles = [
                capture(&mut renderer, card),
                capture(&mut renderer, sign),
                capture(&mut renderer, far),
            ];
            // Text that lights pixels in each: the card's and the sign's titles and the sign seen from afar.
            for (index, tile) in tiles.iter().enumerate() {
                let lit = tile.pixels().filter(|pixel| pixel[1] > 150).count();
                assert!(lit > 40, "{index}: {lit}");
            }
            let mut sheet = image::RgbaImage::new(SIZE * 3, SIZE);
            for (index, tile) in tiles.iter().enumerate() {
                image::imageops::replace(&mut sheet, tile, i64::from(SIZE) * index as i64, 0);
            }
            crate::test_support::preview("TOMORI_JAPANESE_PREVIEW", &sheet);
            device.poll(wgpu::PollType::wait_indefinitely()).unwrap();
            assert!(pollster::block_on(scope.pop()).is_none());
        });
    }

    /// Set `TOMORI_ARRIVAL_PREVIEW` to a PNG path to save what this renders.
    #[test]
    fn arrival_renders_objects_ray_and_world_labels() {
        use crate::arrival::{Item, Key, Kind, Ring, Scene, Step, View};
        const WIDTH: u32 = 640;
        const HEIGHT: u32 = 480;
        test_gpu::with(|test_gpu::Gpu { device, queue, .. }| {
            pollster::block_on(async move {
                let scope = device.push_error_scope(wgpu::ErrorFilter::Validation);
                let texture = device.create_texture(&wgpu::TextureDescriptor {
                    label: Some("arrival test eye"),
                    size: wgpu::Extent3d {
                        width: WIDTH,
                        height: HEIGHT,
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
                let mut renderer = Renderer::new(
                    &device,
                    &queue,
                    wgpu::TextureFormat::Rgba8UnormSrgb,
                    WIDTH,
                    HEIGHT,
                );
                renderer.set_environment(Environment::MINIMAL);
                let swarm = Swarm::default();
                let eye = glam::Vec3::new(0.0, 1.6, 0.0);
                let matrix = glam::camera::rh::proj::directx::perspective(
                    1.4,
                    WIDTH as f32 / HEIGHT as f32,
                    0.05,
                    50.0,
                ) * glam::camera::rh::view::look_at_mat4(
                    eye,
                    crate::arrival::CENTRE,
                    glam::Vec3::Y,
                );
                // A closer look at the rings, as the headset's resolution would show them.
                let close = glam::camera::rh::proj::directx::perspective(
                    0.75,
                    WIDTH as f32 / HEIGHT as f32,
                    0.05,
                    50.0,
                ) * glam::camera::rh::view::look_at_mat4(
                    eye,
                    crate::arrival::CREATE,
                    glam::Vec3::Y,
                );
                let capture = |renderer: &mut Renderer, matrix: Mat4| {
                    renderer.render(
                        &device,
                        &queue,
                        std::slice::from_ref(&view),
                        &[matrix],
                        Mat4::IDENTITY,
                        &[],
                    );
                    let buffer = device.create_buffer(&wgpu::BufferDescriptor {
                        label: Some("arrival test readback"),
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
                    data
                };
                let space = |id: &str, title: &str, activity, participants| Item {
                    key: Key::Space(id.into()),
                    kind: Kind::Space {
                        activity,
                        participants,
                        hue: crate::arrival::hue(id),
                        attention: None,
                    },
                    label: title.into(),
                    detail: Some(format!("{activity:?}").to_lowercase()),
                    selected: false,
                };
                let create = |open| Item {
                    key: Key::Create,
                    kind: Kind::Create { open },
                    label: "New space".into(),
                    detail: None,
                    selected: false,
                };
                let connected = Some(Item {
                    key: Key::Connected("studio.local:8765".into()),
                    kind: Kind::Connected,
                    label: "studio.local".into(),
                    detail: Some("connected".into()),
                    selected: false,
                });
                let spaces = vec![
                    space("a1", "Tomori", tomori_protocol::Activity::Running, 2),
                    space("b2", "Fantail", tomori_protocol::Activity::Idle, 0),
                    space("c3", "Notes", tomori_protocol::Activity::Waiting, 1),
                    space("d4", "Old review", tomori_protocol::Activity::Closed, 0),
                ];
                let choosing = View {
                    heading: "Spaces on studio.local".into(),
                    connected: connected.clone(),
                    items: spaces.iter().cloned().chain([create(false)]).collect(),
                    ..Default::default()
                };
                let backend = |name: &str| Item {
                    key: Key::Backend(name.into()),
                    kind: Kind::Choice,
                    label: name.into(),
                    detail: None,
                    selected: name == "opencode",
                };
                let creating = View {
                    heading: "Choose a backend".into(),
                    connected,
                    items: spaces.into_iter().chain([create(true)]).collect(),
                    ring: Some(Ring {
                        step: Step::Backend,
                        items: ["opencode", "codex", "echo"].map(backend).into(),
                    }),
                    ..Default::default()
                };
                renderer.update(&queue, &swarm, None, false);
                let bare = capture(&mut renderer, matrix);
                let mut sheet = image::RgbaImage::new(WIDTH, HEIGHT * 3);
                for (row, (view, hovered, matrix)) in [
                    (&choosing, Key::Space("a1".into()), matrix),
                    (&creating, Key::Backend("opencode".into()), matrix),
                    (&creating, Key::Backend("opencode".into()), close),
                ]
                .into_iter()
                .enumerate()
                {
                    let scene = Scene::layout(view);
                    let target = scene.object(&hovered).unwrap().position;
                    let origin = glam::Vec3::new(0.22, 1.1, -0.35);
                    renderer.update(&queue, &swarm, None, false);
                    renderer.update_arrival(
                        &device,
                        &queue,
                        &crate::arrival_graphics::Frame {
                            scene: &scene,
                            hovered: Some(&hovered),
                            left_ray: None,
                            ray: Some(crate::arrival_graphics::Beam {
                                origin,
                                end: target,
                                hit: true,
                            }),
                            time: 1.0,
                        },
                    );
                    let image = capture(&mut renderer, matrix);
                    assert_ne!(image, bare, "arrival must draw over the bare scene");
                    image::imageops::replace(
                        &mut sheet,
                        &image::RgbaImage::from_raw(WIDTH, HEIGHT, image).unwrap(),
                        0,
                        (row as u32 * HEIGHT).into(),
                    );
                }
                {
                    crate::test_support::preview("TOMORI_ARRIVAL_PREVIEW", &sheet);
                }
                device.poll(wgpu::PollType::wait_indefinitely()).unwrap();
                assert!(scope.pop().await.is_none());
            })
        });
    }

    #[test]
    fn multisampling_smooths_edges_on_the_grid() {
        use crate::arrival::{Item, Key, Kind, Scene, View};
        const WIDTH: u32 = 320;
        const HEIGHT: u32 = 200;
        test_gpu::with(|test_gpu::Gpu { device, queue, .. }| {
            pollster::block_on(async move {
                let format = wgpu::TextureFormat::Rgba8UnormSrgb;
                let scope = device.push_error_scope(wgpu::ErrorFilter::Validation);
                let texture = device.create_texture(&wgpu::TextureDescriptor {
                    label: Some("multisample test eye"),
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
                let eye = glam::Vec3::new(0.0, 1.6, 0.0);
                let matrix = glam::camera::rh::proj::directx::perspective(
                    1.0,
                    WIDTH as f32 / HEIGHT as f32,
                    0.05,
                    50.0,
                ) * glam::camera::rh::view::look_at_mat4(
                    eye,
                    crate::arrival::CENTRE,
                    glam::Vec3::Y,
                );
                let scene = Scene::layout(&View {
                    heading: "Spaces on studio.local".into(),
                    items: vec![Item {
                        key: Key::Create,
                        kind: Kind::Create { open: false },
                        label: "New space".into(),
                        detail: None,
                        selected: false,
                    }],
                    ..Default::default()
                });
                let pill = tomori_scene::pill::rows(&tomori_scene::pill::Status {
                    activity: tomori_protocol::Activity::Running,
                    agent: None,
                    speaking: None,
                    waiting: false,
                    pulse: 0,
                    mode: Some("build".into()),
                    title: "Hello from the desktop window.".into(),
                    replaying: false,
                    need: Some(tomori_scene::pill::Need::Approval),
                });
                let mut swarm = Swarm::default();
                for _ in 0..120 {
                    swarm.step(1.0 / 72.0, None);
                }
                // Draw everything the scene pass draws, so that each pipeline's sample count must match the pass's.
                let render = |samples: u32, environment: Environment| {
                    let mut renderer =
                        Renderer::with_samples(&device, &queue, format, WIDTH, HEIGHT, samples);
                    renderer.set_environment(environment);
                    renderer.update(&queue, &swarm, None, false);
                    renderer.update_controllers(
                        &queue,
                        [
                            Some(crate::controller::Pose {
                                position: [-0.2, 1.3, -0.6],
                                orientation: [0.0, 0.0, 0.0, 1.0],
                            }),
                            None,
                        ],
                        [None, None],
                    );
                    renderer.update_arrival(
                        &device,
                        &queue,
                        &crate::arrival_graphics::Frame {
                            scene: &scene,
                            hovered: Some(&Key::Create),
                            left_ray: None,
                            ray: Some(crate::arrival_graphics::Beam {
                                origin: glam::Vec3::new(0.2, 1.1, -0.3),
                                end: crate::arrival::CREATE,
                                hit: true,
                            }),
                            time: 1.0,
                        },
                    );
                    let mut panel = renderer.panel(&device, 24_000);
                    let mut canvas = crate::ui_panel::Canvas::new(
                        tomori_scene::pill::COLUMNS,
                        tomori_scene::pill::PIXEL,
                    );
                    canvas.rows = pill.clone();
                    panel.set(&queue, &canvas);
                    renderer.render(
                        &device,
                        &queue,
                        std::slice::from_ref(&view),
                        &[matrix],
                        Mat4::IDENTITY,
                        &[(
                            &panel,
                            Mat4::from_translation(eye)
                                * Mat4::from_translation(crate::controller_ui::PILL_OFFSET),
                        )],
                    );
                    let buffer = device.create_buffer(&wgpu::BufferDescriptor {
                        label: Some("multisample test readback"),
                        size: u64::from(WIDTH * HEIGHT * 4),
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
                    let (tx, rx) = std::sync::mpsc::channel();
                    buffer
                        .slice(..)
                        .map_async(wgpu::MapMode::Read, move |result| tx.send(result).unwrap());
                    device.poll(wgpu::PollType::wait_indefinitely()).unwrap();
                    rx.recv().unwrap().unwrap();
                    let data = buffer.slice(..).get_mapped_range().unwrap().to_vec();
                    image::RgbaImage::from_raw(WIDTH, HEIGHT, data).unwrap()
                };
                // Edges drawn with several samples blend into their background, which a single sample never does.
                let colours = |image: &image::RgbaImage| {
                    image
                        .pixels()
                        .map(|pixel| pixel.0)
                        .collect::<std::collections::HashSet<_>>()
                        .len()
                };
                // An environment's crate checks its own scenery's multisampling.
                let mut sheet = image::RgbaImage::new(WIDTH * 2, HEIGHT);
                for (row, environment) in [Environment::MINIMAL].into_iter().enumerate() {
                    let single = render(1, environment);
                    let multi = render(4, environment);
                    assert!(
                        colours(&multi) > colours(&single),
                        "{environment:?}: multisampling blends more colours"
                    );
                    let y = (row as u32 * HEIGHT).into();
                    image::imageops::replace(&mut sheet, &single, 0, y);
                    image::imageops::replace(&mut sheet, &multi, WIDTH.into(), y);
                }
                {
                    crate::test_support::preview("TOMORI_MULTISAMPLE_PREVIEW", &sheet);
                }
                assert!(scope.pop().await.is_none());
            })
        });
    }

    #[test]
    fn multisampling_is_asked_for_by_count_or_switch() {
        log::set_max_level(log::LevelFilter::Trace);
        assert_eq!(requested_samples(None), SAMPLES);
        assert_eq!(requested_samples(Some("")), SAMPLES);
        for on in ["4", "on", " ON "] {
            assert_eq!(requested_samples(Some(on)), 4, "{on:?}");
        }
        for off in ["0", "1", "off", "Off"] {
            assert_eq!(requested_samples(Some(off)), 1, "{off:?}");
        }
        // Counts wgpu doesn't allow without adapter-specific features, and nonsense, keep the default.
        for other in ["2", "8", "lots"] {
            assert_eq!(requested_samples(Some(other)), SAMPLES, "{other:?}");
        }
    }

    /// Draws as the headset does, into the two layers of one swapchain-like image, with every pass the headset draws in the scene pass, and the caption after it.
    #[test]
    fn headset_layers_multisample_every_pass() {
        use crate::arrival::{Item, Key, Kind, Scene, View};
        // Rows of whole multiples of 256 bytes, as copying to a buffer needs.
        const WIDTH: u32 = 256;
        const HEIGHT: u32 = 256;
        test_gpu::with(
            |test_gpu::Gpu {
                 adapter,
                 device,
                 queue,
             }| {
                pollster::block_on(async move {
                    let format = wgpu::TextureFormat::Rgba8UnormSrgb;
                    assert_eq!(supported_samples(&adapter, format, SAMPLES), 4);
                    assert_eq!(supported_samples(&adapter, format, 1), 1);
                    let scope = device.push_error_scope(wgpu::ErrorFilter::Validation);
                    let texture = device.create_texture(&wgpu::TextureDescriptor {
                        label: Some("headset test swapchain image"),
                        size: wgpu::Extent3d {
                            width: WIDTH,
                            height: HEIGHT,
                            depth_or_array_layers: 2,
                        },
                        mip_level_count: 1,
                        sample_count: 1,
                        dimension: wgpu::TextureDimension::D2,
                        format,
                        usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                            | wgpu::TextureUsages::COPY_SRC,
                        view_formats: &[],
                    });
                    let views: [_; 2] = std::array::from_fn(|eye| {
                        texture.create_view(&wgpu::TextureViewDescriptor {
                            dimension: Some(wgpu::TextureViewDimension::D2),
                            base_array_layer: eye as u32,
                            array_layer_count: Some(1),
                            ..Default::default()
                        })
                    });
                    let projection = glam::camera::rh::proj::directx::perspective(
                        1.4,
                        WIDTH as f32 / HEIGHT as f32,
                        0.05,
                        50.0,
                    );
                    let matrices = [-0.032, 0.032].map(|x| {
                        let eye = glam::Vec3::new(x, 1.6, 0.0);
                        projection
                            * glam::camera::rh::view::look_at_mat4(
                                eye,
                                crate::arrival::CENTRE,
                                glam::Vec3::Y,
                            )
                    });
                    let arrival = Scene::layout(&View {
                        heading: "Spaces on studio.local".into(),
                        items: vec![Item {
                            key: Key::Create,
                            kind: Kind::Create { open: false },
                            label: "New space".into(),
                            detail: None,
                            selected: false,
                        }],
                        ..Default::default()
                    });
                    let workspace = crate::delegation::Workspace {
                        nodes: vec![crate::delegation::Node {
                            id: "worker".into(),
                            parent_id: Some("root".into()),
                            title: "Synthetic task".into(),
                            state: crate::delegation::State::Running,
                        }],
                        presentations: vec![crate::delegation::Presentation {
                            id: "card".into(),
                            title: "Synthetic result".into(),
                            source_id: "worker".into(),
                            source_message_id: "message".into(),
                            text: "Example content".into(),
                            ..Default::default()
                        }],
                        ..Default::default()
                    };
                    let mut workers = crate::delegation::Scene::new();
                    for _ in 0..30 {
                        workers.update(Some("root"), &workspace, 0.05);
                    }
                    let indicator = tomori_scene::recording::canvas(
                        &tomori_scene::recording::Phase::Recording {
                            elapsed: std::time::Duration::from_secs(15),
                            heard: "Hello from the headset.".into(),
                            answering: None,
                            about: None,
                        },
                        Some(std::time::Duration::from_secs(60)),
                        0.5,
                    );
                    let mut swarm = Swarm::default();
                    for _ in 0..120 {
                        swarm.step(1.0 / 72.0, None);
                    }
                    let hand = {
                        let mut hand = crate::hands::Hand {
                            joints: [None; crate::hands::JOINT_COUNT],
                        };
                        for (joint, y) in [(1, 1.2), (2, 1.3)] {
                            hand.joints[joint] = Some(crate::hands::Joint {
                                position: [0.2, y, -0.6],
                                radius: 0.01,
                            });
                        }
                        hand
                    };
                    let render = |samples: u32, environment: Environment| {
                        let mut renderer =
                            Renderer::with_samples(&device, &queue, format, WIDTH, HEIGHT, samples);
                        renderer.set_environment(environment);
                        renderer.update(&queue, &swarm, None, false);
                        renderer.update_controllers(
                            &queue,
                            [
                                Some(crate::controller::Pose {
                                    position: [-0.2, 1.3, -0.6],
                                    orientation: [0.0, 0.0, 0.0, 1.0],
                                }),
                                None,
                            ],
                            [None, Some(hand)],
                        );
                        renderer.update_workspace(
                            &device,
                            &queue,
                            workers.cubes(),
                            workers.stations(),
                            &workspace,
                            None,
                        );
                        renderer.update_arrival(
                            &device,
                            &queue,
                            &crate::arrival_graphics::Frame {
                                scene: &arrival,
                                hovered: Some(&Key::Create),
                                left_ray: None,
                                ray: Some(crate::arrival_graphics::Beam {
                                    origin: glam::Vec3::new(0.2, 1.1, -0.3),
                                    end: crate::arrival::CREATE,
                                    hit: true,
                                }),
                                time: 1.0,
                            },
                        );
                        let mut pill = renderer.panel(&device, 24_000);
                        let mut canvas = crate::ui_panel::Canvas::new(
                            tomori_scene::pill::COLUMNS,
                            tomori_scene::pill::PIXEL,
                        );
                        canvas.rows = vec![crate::ui_text::Row::text(
                            "Hello from the headset.",
                            crate::ui_text::Style::plain([1.0; 4]),
                            tomori_scene::pill::COLUMNS,
                        )];
                        pill.set(&queue, &canvas);
                        let mut card = renderer.panel(&device, 24_000);
                        card.set(&queue, &indicator);
                        let mut panels = vec![(
                            &pill,
                            Mat4::from_translation(glam::Vec3::new(0.0, 1.1, -1.5)),
                        )];
                        panels.push((
                            &card,
                            tomori_scene::recording::facing(
                                glam::Vec3::new(-0.2, 1.4, -0.6),
                                glam::Vec3::new(0.0, 1.6, 0.0),
                                1.0,
                            ),
                        ));
                        renderer.render(
                            &device,
                            &queue,
                            &views,
                            &matrices,
                            Mat4::IDENTITY,
                            &panels,
                        );
                        let layer = u64::from(WIDTH * HEIGHT * 4);
                        let buffer = device.create_buffer(&wgpu::BufferDescriptor {
                            label: Some("headset test readback"),
                            size: layer * 2,
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
                        let (tx, rx) = std::sync::mpsc::channel();
                        buffer
                            .slice(..)
                            .map_async(wgpu::MapMode::Read, move |result| tx.send(result).unwrap());
                        device.poll(wgpu::PollType::wait_indefinitely()).unwrap();
                        rx.recv().unwrap().unwrap();
                        let data = buffer.slice(..).get_mapped_range().unwrap().to_vec();
                        let (left, right) = data.split_at(layer as usize);
                        [left, right].map(|eye| {
                            image::RgbaImage::from_raw(WIDTH, HEIGHT, eye.to_vec()).unwrap()
                        })
                    };
                    let colours = |image: &image::RgbaImage| {
                        image
                            .pixels()
                            .map(|pixel| pixel.0)
                            .collect::<std::collections::HashSet<_>>()
                            .len()
                    };
                    // An environment's crate checks its own scenery's multisampling.
                    let mut sheet = image::RgbaImage::new(WIDTH * 4, HEIGHT);
                    for (row, environment) in [Environment::MINIMAL].into_iter().enumerate() {
                        let single = render(1, environment);
                        let multi = render(4, environment);
                        assert_ne!(
                            multi[0], multi[1],
                            "{environment:?}: each eye has its own layer"
                        );
                        for eye in 0..2 {
                            assert!(
                                colours(&multi[eye]) > colours(&single[eye]),
                                "{environment:?}, eye {eye}: multisampling blends more colours"
                            );
                        }
                        let y = (row as u32 * HEIGHT).into();
                        for (column, image) in single.iter().chain(&multi).enumerate() {
                            image::imageops::replace(
                                &mut sheet,
                                image,
                                column as i64 * i64::from(WIDTH),
                                y,
                            );
                        }
                    }
                    {
                        crate::test_support::preview("TOMORI_HEADSET_MULTISAMPLE_PREVIEW", &sheet);
                    }
                    assert!(scope.pop().await.is_none());
                })
            },
        );
    }

    #[test]
    fn renders_stereo_offscreen() {
        test_gpu::with(|test_gpu::Gpu { device, queue, .. }| {
            pollster::block_on(async move {
                let error_scope = device.push_error_scope(wgpu::ErrorFilter::Validation);
                let targets: [wgpu::TextureView; 2] = std::array::from_fn(|_| {
                    device
                        .create_texture(&wgpu::TextureDescriptor {
                            label: Some("test eye"),
                            size: wgpu::Extent3d {
                                width: 128,
                                height: 128,
                                depth_or_array_layers: 1,
                            },
                            mip_level_count: 1,
                            sample_count: 1,
                            dimension: wgpu::TextureDimension::D2,
                            format: wgpu::TextureFormat::Rgba8UnormSrgb,
                            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
                            view_formats: &[],
                        })
                        .create_view(&Default::default())
                });
                let mut renderer = Renderer::new(
                    &device,
                    &queue,
                    wgpu::TextureFormat::Rgba8UnormSrgb,
                    128,
                    128,
                );
                renderer.update(&queue, &Swarm::default(), None, false);
                renderer.update_controllers(
                    &queue,
                    [
                        Some(crate::controller::Pose {
                            position: [-0.2, 1.3, -1.0],
                            orientation: [0.0, 0.0, 0.0, 1.0],
                        }),
                        Some(crate::controller::Pose {
                            position: [0.2, 1.3, -1.0],
                            orientation: [0.0, 0.0, 0.0, 1.0],
                        }),
                    ],
                    [None, None],
                );
                let projection = glam::camera::rh::proj::directx::perspective(1.2, 1.0, 0.05, 50.0);
                let matrices = [-0.032, 0.032]
                    .map(|x| projection * Mat4::from_translation(glam::Vec3::new(-x, -1.6, 0.0)));
                let mut pill = renderer.panel(&device, 24_000);
                let mut canvas = crate::ui_panel::Canvas::new(
                    tomori_scene::pill::COLUMNS,
                    tomori_scene::pill::PIXEL,
                );
                canvas.rows = vec![crate::ui_text::Row::text(
                    "Hello from the Quest microphone!",
                    crate::ui_text::Style::plain([1.0; 4]),
                    tomori_scene::pill::COLUMNS,
                )];
                pill.set(&queue, &canvas);
                // Exercise the recording indicator through its states, beside the panel, and drawing no panel at all.
                let mut indicator = renderer.panel(&device, 24_000);
                let limit = Some(std::time::Duration::from_secs(60));
                for elapsed in [Some(0), Some(15), Some(50), Some(55), Some(60), None] {
                    let mut panels = vec![(
                        &pill,
                        Mat4::from_translation(glam::Vec3::new(0.0, 1.1, -1.5)),
                    )];
                    if let Some(elapsed) = elapsed {
                        indicator.set(
                            &queue,
                            &tomori_scene::recording::canvas(
                                &tomori_scene::recording::Phase::Recording {
                                    elapsed: std::time::Duration::from_secs(elapsed),
                                    heard: "Hello from the Quest microphone!".into(),
                                    answering: None,
                                    about: None,
                                },
                                limit,
                                0.5,
                            ),
                        );
                        panels.push((
                            &indicator,
                            tomori_scene::recording::facing(
                                glam::Vec3::new(-0.2, 1.2, -0.4),
                                glam::Vec3::new(0.0, 1.6, 0.0),
                                1.0,
                            ),
                        ));
                    }
                    renderer.render(
                        &device,
                        &queue,
                        &targets,
                        &matrices,
                        Mat4::IDENTITY,
                        &panels,
                    );
                }
                // Exercise every activity shape/halo through both eye targets.
                let mut swarm = Swarm::default();
                for mode in [
                    Mode::Ready,
                    Mode::Working,
                    Mode::Preparing,
                    Mode::Speaking,
                    Mode::Waiting,
                    Mode::Listening,
                    Mode::Transcribing,
                    Mode::Unknown,
                    Mode::Unavailable,
                ] {
                    swarm.visual = crate::interaction::Visual {
                        mode,
                        level: 0.8,
                        time: 2.0,
                        working: true,
                    };
                    renderer.update(&queue, &swarm, None, false);
                    renderer.render(&device, &queue, &targets, &matrices, Mat4::IDENTITY, &[]);
                }
                // Exercise the same single-view + resized-depth path as desktop.
                renderer.resize(&device, 128, 128);
                renderer.render(
                    &device,
                    &queue,
                    &targets[..1],
                    &matrices[..1],
                    Mat4::IDENTITY,
                    &[],
                );
                device.poll(wgpu::PollType::wait_indefinitely()).unwrap();
                assert!(error_scope.pop().await.is_none());
            })
        });
    }
    #[test]
    fn a_ring_is_drawn_where_a_teleport_would_land() {
        test_gpu::with(|test_gpu::Gpu { device, queue, .. }| {
            let mut renderer =
                Renderer::new(&device, &queue, wgpu::TextureFormat::Rgba8UnormSrgb, 8, 8);
            renderer.update(&queue, &Swarm::default(), None, false);
            let without = renderer.scene.len();
            let landing = glam::Vec3::new(2.0, 3.0, -4.0);
            renderer.set_landing(Some(landing));
            renderer.update(&queue, &Swarm::default(), None, false);
            assert_eq!(renderer.scene.len(), without + LANDING_CUBES);
            for instance in &renderer.scene[without..] {
                let at = glam::Vec3::from_array(instance.position);
                // Each bar stands 2.5 cm above the landing and reaches 1 cm into the surface beneath it.
                let half = instance.scale[1] * 0.5;
                assert!((at.y + half - landing.y - 0.025).abs() < 1e-5);
                assert!((at.y - half - landing.y + 0.01).abs() < 1e-5);
                let across = glam::Vec2::new(at.x - landing.x, at.z - landing.z).length();
                assert!((across - LANDING_RADIUS).abs() < 1e-5);
            }
            // Nonsense, or none, draws no ring.
            renderer.set_landing(Some(glam::Vec3::NAN));
            renderer.update(&queue, &Swarm::default(), None, false);
            assert_eq!(renderer.scene.len(), without);
            renderer.set_landing(None);
            renderer.update(&queue, &Swarm::default(), None, false);
            assert_eq!(renderer.scene.len(), without);
        });
    }

    #[test]
    fn a_pad_is_drawn_where_it_is_with_its_highlight_and_which_way_it_goes() {
        use tomori_scene::pads::Shown;
        test_gpu::with(|test_gpu::Gpu { device, queue, .. }| {
            let mut renderer =
                Renderer::new(&device, &queue, wgpu::TextureFormat::Rgba8UnormSrgb, 8, 8);
            renderer.update(&queue, &Swarm::default(), None, false);
            let without = renderer.scene.len();
            let pad =
                |up, highlighted| Shown::new(glam::Vec3::new(1.0, -4.5, 2.0), 0.5, up, highlighted);
            let count = |renderer: &mut Renderer, pads: &[Shown]| {
                renderer.set_pads(pads);
                renderer.update(&queue, &Swarm::default(), None, false);
                renderer.scene.len() - without
            };
            // A ring, a plate and two chevrons; pointed at, a second ring; level, a diamond in place of the chevrons.
            let plain = PAD_RING + PAD_PLATE + PAD_CHEVRON * PAD_CHEVRONS;
            assert_eq!(count(&mut renderer, &[pad(Some(false), false)]), plain);
            assert_eq!(
                count(&mut renderer, &[pad(Some(true), true)]),
                plain + PAD_RING
            );
            assert_eq!(
                count(&mut renderer, &[pad(None, false)]),
                PAD_RING + PAD_PLATE + 1
            );
            // Every cube stands over the pad, within its ring and the highlight's.
            let top = |renderer: &Renderer| {
                renderer.scene[without..]
                    .iter()
                    .map(|instance| {
                        let at =
                            glam::Vec3::from_array(instance.position) - pad(None, false).centre;
                        assert!(
                            glam::Vec2::new(at.x, at.z).length() <= 0.5 + 0.08 + 1e-4,
                            "{at}"
                        );
                        // How high the cube reaches, turned as it is.
                        let half: f32 = (0..3)
                            .map(|axis| 0.5 * instance.basis[axis][1].abs() * instance.scale[axis])
                            .sum();
                        assert!(at.y - half >= -1e-4, "{at}");
                        at.y + half
                    })
                    .fold(0.0, f32::max)
            };
            // The column the ray points at is no taller than what is drawn, and as tall, to a centimetre, wherever the
            // pad's partner stands.
            for up in [Some(true), Some(false)] {
                count(&mut renderer, &[pad(up, true)]);
                let drawn = top(&renderer);
                assert!(
                    (drawn - tomori_scene::pads::COLUMN).abs() < 0.01,
                    "{up:?} {drawn}"
                );
            }
            count(&mut renderer, &[pad(None, true)]);
            assert!(top(&renderer) <= tomori_scene::pads::COLUMN);
            // Nonsense, too wide, or more than a profile uses, is not drawn.
            let mut nonsense = pad(None, false);
            nonsense.centre = glam::Vec3::NAN;
            let mut wide = pad(None, false);
            wide.radius = 3.0;
            assert_eq!(count(&mut renderer, &[nonsense, wide]), 0);
            let many = vec![pad(Some(true), true); MOST_PADS + 3];
            assert_eq!(count(&mut renderer, &many), MOST_PADS * PAD_CUBES);
            assert_eq!(count(&mut renderer, &[]), 0);
        });
    }

    /// Set `TOMORI_PADS_PREVIEW` to a PNG path to save what this draws: teleport pads on the grid, seen from home, one
    /// pointed at, whose partner is below, one whose partner is above and one level with its partner, and, beside it,
    /// the one pointed at from nearer.
    #[test]
    fn pads_read_on_the_floor_from_where_participants_arrive() {
        use tomori_scene::pads::Shown;
        const WIDTH: u32 = 512;
        const HEIGHT: u32 = 320;
        test_gpu::with(|test_gpu::Gpu { device, queue, .. }| {
            let scope = device.push_error_scope(wgpu::ErrorFilter::Validation);
            let format = wgpu::TextureFormat::Rgba8UnormSrgb;
            let texture = device.create_texture(&wgpu::TextureDescriptor {
                label: Some("pads test eye"),
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
            let pads = [
                Shown::new(glam::Vec3::new(1.6, 0.0, -2.4), 0.5, Some(false), true),
                Shown::new(glam::Vec3::new(3.4, 0.0, -3.2), 0.5, Some(true), false),
                Shown::new(glam::Vec3::new(2.2, 0.0, -4.6), 0.5, None, false),
            ];
            let mut renderer = Renderer::with_samples(&device, &queue, format, WIDTH, HEIGHT, 4);
            let draw =
                |renderer: &mut Renderer, eye: glam::Vec3, at: glam::Vec3, shown: &[Shown]| {
                    renderer.set_pads(shown);
                    renderer.update(&queue, &Swarm::default(), None, false);
                    let matrix = glam::camera::rh::proj::directx::perspective(
                        1.0,
                        WIDTH as f32 / HEIGHT as f32,
                        0.05,
                        50.0,
                    ) * glam::camera::rh::view::look_at_mat4(eye, at, glam::Vec3::Y);
                    renderer.render(
                        &device,
                        &queue,
                        std::slice::from_ref(&view),
                        &[matrix],
                        Mat4::IDENTITY,
                        &[],
                    );
                    (
                        crate::test_support::read_back(&device, &queue, &texture, (WIDTH, HEIGHT)),
                        matrix,
                    )
                };
            let home = glam::Vec3::new(0.0, 1.6, 0.0);
            let towards = glam::Vec3::new(2.4, 0.4, -3.4);
            let (bare, _) = draw(&mut renderer, home, towards, &[]);
            let (seen, matrix) = draw(&mut renderer, home, towards, &pads);
            let near = glam::Vec3::new(1.0, 1.6, -0.6);
            let (close, _) = draw(
                &mut renderer,
                near,
                pads[0].centre + glam::Vec3::Y * 0.4,
                &pads,
            );
            let mut sheet = image::RgbaImage::new(WIDTH * 2, HEIGHT);
            image::imageops::replace(&mut sheet, &seen, 0, 0);
            image::imageops::replace(&mut sheet, &close, WIDTH.into(), 0);
            crate::test_support::preview("TOMORI_PADS_PREVIEW", &sheet);
            assert!(pollster::block_on(scope.pop()).is_none());
            // Where each pad's ring passes nearest home, it is drawn cyan over the grid, brighter for the one pointed at.
            let brightest = |image: &image::RgbaImage, point: glam::Vec3| {
                let clip = matrix * point.extend(1.0);
                let (x, y) = (
                    ((clip.x / clip.w + 1.0) * 0.5 * WIDTH as f32) as i64,
                    ((1.0 - clip.y / clip.w) * 0.5 * HEIGHT as f32) as i64,
                );
                (-3..=3)
                    .flat_map(|dx| (-3..=3).map(move |dy| (x + dx, y + dy)))
                    .map(|(x, y)| image.get_pixel(x as u32, y as u32).0)
                    .max_by_key(|pixel| u32::from(pixel[1]) + u32::from(pixel[2]))
                    .unwrap()
            };
            let rings: Vec<_> = pads
                .iter()
                .map(|pad| {
                    let towards_home = (home - pad.centre).with_y(0.0).normalize() * pad.radius;
                    let point = pad.centre + towards_home + glam::Vec3::Y * 0.012;
                    (brightest(&bare, point), brightest(&seen, point))
                })
                .collect();
            for (bare, seen) in &rings {
                assert!(seen[2] > seen[0] && seen[1] > seen[0], "{seen:?}");
                assert!(
                    u32::from(seen[2]) > u32::from(bare[2]) + 40,
                    "{bare:?} {seen:?}"
                );
            }
            assert!(rings[0].1[0] > rings[1].1[0], "{rings:?}");
        });
    }

    #[test]
    fn the_swarm_s_rings_stand_about_it_wherever_it_stands() {
        test_gpu::with(|test_gpu::Gpu { device, queue, .. }| {
            let mut renderer =
                Renderer::new(&device, &queue, wgpu::TextureFormat::Rgba8UnormSrgb, 8, 8);
            let mut swarm = Swarm::default();
            swarm.visual.mode = Mode::Ready;
            let anchor = tomori_scene::anchors::Anchor {
                position: glam::Vec3::new(3.0, 1.8, -5.0),
                orientation: glam::Quat::from_rotation_y(std::f32::consts::FRAC_PI_2),
            };
            swarm.set_anchor(anchor);
            renderer.update(&queue, &swarm, None, false);
            // The halo stands 0.28 m behind the swarm's middle as it faces, and the floor ring 1.25 m about beneath it.
            let halo: Vec<glam::Vec3> = renderer.scene[AGENT_COUNT..AGENT_COUNT + 48]
                .iter()
                .map(|instance| glam::Vec3::from_array(instance.position))
                .collect();
            for at in &halo {
                assert!((at.x - (anchor.position.x - 0.28)).abs() < 1e-4, "{at}");
            }
            // Raised 0.4 m above the swarm's usual height, the ring is on the floor 0.4 m up.
            let floor = renderer
                .scene
                .iter()
                .filter(|instance| (instance.position[1] - 0.415).abs() < 1e-5)
                .collect::<Vec<_>>();
            assert_eq!(floor.len(), 40);
            for instance in floor {
                let across = glam::Vec2::new(
                    instance.position[0] - anchor.position.x,
                    instance.position[2] - anchor.position.z,
                )
                .length();
                assert!((across - 1.25).abs() < 1e-4);
            }
            // Lower than its usual height, the ring stays on the room's floor.
            swarm.set_anchor(tomori_scene::anchors::Anchor {
                position: glam::Vec3::new(3.0, 0.6, -5.0),
                ..anchor
            });
            renderer.update(&queue, &swarm, None, false);
            assert_eq!(
                renderer
                    .scene
                    .iter()
                    .filter(|instance| instance.position[1] == 0.015)
                    .count(),
                40
            );
        });
    }

    #[test]
    fn the_ring_on_the_floor_stays_at_home_a_little_dimmer_while_the_agent_is_away() {
        use tomori_scene::anchors::HomeRing;
        test_gpu::with(|test_gpu::Gpu { device, queue, .. }| {
            let mut renderer =
                Renderer::new(&device, &queue, wgpu::TextureFormat::Rgba8UnormSrgb, 8, 8);
            let mut swarm = Swarm::default();
            let home = glam::Vec3::new(-1.0, 1.5, -4.0);
            let ring = HomeRing {
                centre: home,
                radius: tomori_protocol::cluster::RING,
                towards: glam::Vec3::Z,
                sweep: std::f32::consts::TAU,
                height: tomori_protocol::cluster::FLOOR,
            };
            renderer.set_home_ring(Some(ring));
            // The ring's cubes, on the space's floor beneath home, though home stands 0.1 m above the swarm's usual
            // height; or `raised` above it, beneath the swarm.
            let on = |renderer: &Renderer, raised: f32| -> Vec<Instance> {
                renderer
                    .scene
                    .iter()
                    .filter(|instance| (instance.position[1] - (FLOOR_LIFT + raised)).abs() < 1e-5)
                    .copied()
                    .collect()
            };
            let floor = |renderer: &Renderer| on(renderer, 0.0);
            let at = |anchor: glam::Vec3| tomori_scene::anchors::Anchor {
                position: anchor,
                orientation: glam::Quat::IDENTITY,
            };
            swarm.set_anchor(at(home));
            renderer.update(&queue, &swarm, None, false);
            let at_home = floor(&renderer);
            assert_eq!(at_home.len(), HOME_RING_CUBES);
            for instance in &at_home {
                let across =
                    glam::Vec2::new(instance.position[0] - home.x, instance.position[2] - home.z)
                        .length();
                assert!((across - ring.radius).abs() < 1e-4);
                assert_eq!(instance.color, HOME_RING_COLOUR);
            }
            // The agent gone to someone 4 m away, the ring stays where it was, whole and still plainly seen, a little dimmer.
            swarm.set_anchor(at(glam::Vec3::new(2.0, 1.5, -1.0)));
            renderer.update(&queue, &swarm, None, false);
            let away = floor(&renderer);
            assert_eq!(
                away.iter()
                    .map(|instance| instance.position)
                    .collect::<Vec<_>>(),
                at_home
                    .iter()
                    .map(|instance| instance.position)
                    .collect::<Vec<_>>()
            );
            let faint = HOME_RING_COLOUR.map(|channel| channel * HOME_RING_AWAY);
            assert!(away.iter().all(|instance| {
                (0..3).all(|channel| (instance.color[channel] - faint[channel]).abs() < 1e-6)
            }));
            // Home is marked whether or not the agent is there: the ring keeps most of its brightness.
            assert_eq!(away.len(), HOME_RING_CUBES);
            assert!(faint[1] >= 0.8 * HOME_RING_COLOUR[1], "{faint:?}");
            // Coming back within the ring, it brightens again.
            swarm.set_anchor(at(home + glam::Vec3::new(0.9, 0.0, 0.0)));
            renderer.update(&queue, &swarm, None, false);
            let nearly = floor(&renderer)[0].color[1];
            assert!(
                nearly > faint[1] && nearly < HOME_RING_COLOUR[1],
                "{nearly}"
            );
            // Two thirds of a ring, about the way to the arrival: the side behind it is not drawn.
            renderer.set_home_ring(Some(HomeRing {
                sweep: 240f32.to_radians(),
                ..ring
            }));
            renderer.update(&queue, &swarm, None, false);
            let part = floor(&renderer);
            assert!(part.len() < HOME_RING_CUBES * 3 / 4 && part.len() > HOME_RING_CUBES / 2);
            assert!(
                part.iter()
                    .all(|instance| instance.position[2] - home.z > -0.71)
            );
            // However high home stands, and whichever way it opens, the ring is on the floor, its cubes either side of
            // the way it faces alike, so that the side it leaves open is centred on the way straight behind it.
            for (height, yaw, sweep) in [
                (0.6, 0.3, 300.0),
                (1.5, -1.15f32.to_radians(), 300.0),
                (2.4, 2.0, 170.0),
                (1.4, 0.0, 360.0),
            ] {
                let centre = home.with_y(height);
                let towards = glam::Vec3::new(yaw.sin(), 0.0, yaw.cos());
                renderer.set_home_ring(Some(HomeRing {
                    centre,
                    towards,
                    sweep: f32::to_radians(sweep),
                    ..ring
                }));
                swarm.set_anchor(at(centre));
                renderer.update(&queue, &swarm, None, false);
                let drawn = floor(&renderer);
                assert!(!drawn.is_empty() && drawn.len() <= HOME_RING_CUBES);
                let ahead = glam::Vec2::new(towards.x, towards.z);
                let mut sides = [0.0f32; 2];
                let mut middle = glam::Vec2::ZERO;
                for instance in &drawn {
                    let out = glam::Vec2::new(
                        instance.position[0] - centre.x,
                        instance.position[2] - centre.z,
                    );
                    assert!((out.length() - ring.radius).abs() < 1e-4);
                    let off = ahead.angle_to(out);
                    assert!(off.abs() <= f32::to_radians(sweep) / 2.0 + 1e-3, "{off}");
                    // The cube straight opposite, of a whole ring, is on neither side.
                    if off.abs() < std::f32::consts::PI - 1e-3 {
                        sides[usize::from(off > 0.0)] += off.abs();
                    }
                    middle += out;
                }
                assert!((sides[0] - sides[1]).abs() < 1e-3, "{height} {sides:?}");
                assert!(
                    middle.perp_dot(ahead).abs() < 1e-3
                        && (sweep == 360.0 || middle.dot(ahead) > 0.0),
                    "{height} {middle}"
                );
            }
            // Under a home floating high, where its environment asks, the ring lies at its height, whole, about home.
            let high = home.with_y(5.3);
            renderer.set_home_ring(Some(HomeRing {
                centre: high,
                height: 3.9,
                ..ring
            }));
            swarm.set_anchor(at(high));
            renderer.update(&queue, &swarm, None, false);
            assert!(floor(&renderer).is_empty());
            let under = on(&renderer, 3.9);
            assert_eq!(under.len(), HOME_RING_CUBES);
            assert!(under.iter().all(|instance| {
                let across =
                    glam::Vec2::new(instance.position[0] - home.x, instance.position[2] - home.z);
                (across.length() - ring.radius).abs() < 1e-4
            }));
            swarm.set_anchor(at(home + glam::Vec3::new(0.9, 0.0, 0.0)));
            // Nonsense, or none, draws it beneath the swarm, as before there were homes.
            for nonsense in [
                HomeRing {
                    radius: 9.0,
                    ..ring
                },
                HomeRing {
                    centre: glam::Vec3::NAN,
                    ..ring
                },
                HomeRing {
                    height: f32::INFINITY,
                    ..ring
                },
            ] {
                renderer.set_home_ring(Some(nonsense));
                renderer.update(&queue, &swarm, None, false);
                // Beneath the swarm, raised as it is, 0.1 m above its usual height.
                assert!(floor(&renderer).is_empty());
                let beneath = on(&renderer, 0.1);
                assert_eq!(beneath.len(), HOME_RING_CUBES);
                let middle = beneath
                    .iter()
                    .map(|instance| glam::Vec3::from_array(instance.position))
                    .sum::<glam::Vec3>()
                    / beneath.len() as f32;
                assert!((middle.x - (home.x + 0.9)).abs() < 1e-3, "{middle}");
            }
        });
    }

    #[test]
    fn a_blink_darkens_the_whole_view_to_black_and_back() {
        const SIZE: u32 = 64;
        test_gpu::with(|test_gpu::Gpu { device, queue, .. }| {
            let scope = device.push_error_scope(wgpu::ErrorFilter::Validation);
            let format = wgpu::TextureFormat::Rgba8UnormSrgb;
            let texture = device.create_texture(&wgpu::TextureDescriptor {
                label: Some("blink test eye"),
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
            // From standing height at home, looking at the swarm's ring on the grid.
            let matrix = glam::camera::rh::proj::directx::perspective(1.2, 1.0, 0.05, 50.0)
                * glam::camera::rh::view::look_at_mat4(
                    glam::Vec3::new(0.0, 1.6, 1.0),
                    crate::swarm::CENTER,
                    glam::Vec3::Y,
                );
            let capture = |renderer: &mut Renderer| -> Vec<u8> {
                renderer.render(
                    &device,
                    &queue,
                    std::slice::from_ref(&view),
                    &[matrix],
                    Mat4::IDENTITY,
                    &[],
                );
                let buffer = device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("blink test readback"),
                    size: u64::from(SIZE * SIZE * 4),
                    usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
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
                buffer.slice(..).map_async(wgpu::MapMode::Read, |_| {});
                device.poll(wgpu::PollType::wait_indefinitely()).unwrap();
                let data = buffer.slice(..).get_mapped_range().unwrap().to_vec();
                buffer.unmap();
                data
            };
            let light = |pixels: &[u8]| -> u64 {
                pixels
                    .chunks(4)
                    .map(|pixel| pixel[..3].iter().map(|&v| u64::from(v)).sum::<u64>())
                    .sum()
            };
            for samples in [1, 4] {
                let mut renderer =
                    Renderer::with_samples(&device, &queue, format, SIZE, SIZE, samples);
                renderer.update(&queue, &Swarm::default(), None, false);
                let open = capture(&mut renderer);
                assert!(light(&open) > 0, "something is seen");
                renderer.set_blink(0.5);
                let half = capture(&mut renderer);
                assert!(light(&half) > 0 && light(&half) < light(&open), "{samples}");
                // Shut, nothing is seen, not even the background.
                renderer.set_blink(1.0);
                assert_eq!(light(&capture(&mut renderer)), 0, "{samples}");
                renderer.set_blink(2.0);
                assert_eq!(light(&capture(&mut renderer)), 0, "{samples}");
                // Open again, or given nonsense, it draws as before.
                for open_again in [0.0, -1.0, f32::NAN] {
                    renderer.set_blink(open_again);
                    assert_eq!(capture(&mut renderer), open, "{samples} {open_again}");
                }
            }
            assert!(pollster::block_on(scope.pop()).is_none());
        });
    }

    #[test]
    fn the_grid_gives_way_inside_a_growing_environment() {
        use crate::arrival_motion::Sphere;
        let mut whole = Vec::new();
        minimal_floor(&mut whole, None);
        assert_eq!(whole.len(), 42);
        // A sphere that doesn't reach the floor leaves it whole.
        let mut grid = Vec::new();
        let high = Sphere {
            centre: glam::Vec3::new(0.0, 1.4, -0.8),
            radius: 1.0,
        };
        minimal_floor(&mut grid, Some(high));
        assert_eq!(grid.len(), 42);
        let wide = Sphere {
            radius: 3.0,
            ..high
        };
        let mut grid = Vec::new();
        minimal_floor(&mut grid, Some(wide));
        assert!(grid.len() > 42 && grid.len() <= 84);
        let floor = (wide.radius * wide.radius - wide.centre.y * wide.centre.y).sqrt();
        for instance in &grid {
            let centre = glam::Vec3::from_array(instance.position);
            let half = glam::Vec3::from_array(instance.scale) * 0.5;
            // Neither end of a segment lies inside the circle where the sphere meets the floor.
            for end in [centre - half, centre + half] {
                let offset = glam::Vec2::new(end.x - wide.centre.x, end.z - wide.centre.z);
                assert!(offset.length() > floor - 0.01, "{end}");
            }
        }
        // Once the sphere covers the grid, none of it is left.
        let mut grid = Vec::new();
        minimal_floor(
            &mut grid,
            Some(Sphere {
                radius: 20.0,
                ..high
            }),
        );
        assert!(grid.is_empty());
    }

    #[test]
    fn worker_instance_rotates_scaled_vertices_and_normals_and_rejects_bad_quaternions() {
        let rotation = glam::Quat::from_euler(glam::EulerRot::YXZ, 0.7, 0.2, -0.15);
        let cube = crate::delegation::Cube {
            position: [1.0, 2.0, -3.0],
            scale: [0.06, 0.08, 0.12],
            color: [0.5; 3],
            orientation: rotation.to_array(),
        };
        let instance = worker_instance(&cube).unwrap();
        let basis = glam::Mat3::from_cols_array_2d(&instance.basis);
        let model = glam::Mat4::from_scale_rotation_translation(
            glam::Vec3::from_array(cube.scale),
            rotation,
            glam::Vec3::from_array(cube.position),
        );
        for vertex in super::cube() {
            let local = glam::Vec3::from_array(vertex.position);
            let actual = glam::Vec3::from_array(instance.position)
                + basis * (local * glam::Vec3::from_array(instance.scale));
            assert!(actual.distance(model.transform_point3(local)) < 0.000001);
            let normal = basis * glam::Vec3::from_array(vertex.normal);
            assert!((normal.length() - 1.0).abs() < 0.000001);
            assert!(normal.distance(rotation * glam::Vec3::from_array(vertex.normal)) < 0.000001);
        }
        for orientation in [[0.0; 4], [1.0; 4], [f32::NAN; 4], [f32::INFINITY; 4]] {
            assert!(
                worker_instance(&crate::delegation::Cube {
                    orientation,
                    ..cube
                })
                .is_none()
            );
        }
        let shader = oriented_shader();
        assert!(shader.contains("rotation * (vertex * drawn)"));
        assert!(shader.contains("out.normal = rotation * normal;"));
        assert!(shader.contains("@location(7) basis_z"));
    }

    #[test]
    fn what_neither_eye_sees_is_culled_and_the_rest_drawn_as_before() {
        test_gpu::with(|test_gpu::Gpu { device, queue, .. }| {
            let scope = device.push_error_scope(wgpu::ErrorFilter::Validation);
            let size = (128, 128);
            let textures: [wgpu::Texture; 2] = std::array::from_fn(|_| {
                device.create_texture(&wgpu::TextureDescriptor {
                    label: Some("culling test eye"),
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
                })
            });
            let targets = textures
                .each_ref()
                .map(|texture| texture.create_view(&Default::default()));
            let mut renderer = Renderer::with_samples(
                &device,
                &queue,
                wgpu::TextureFormat::Rgba8UnormSrgb,
                size.0,
                size.1,
                4,
            );
            // The swarm ahead, and a teleport's ring behind the eyes, which neither sees.
            renderer.set_landing(Some(glam::Vec3::new(0.0, 0.0, 4.0)));
            renderer.update(&queue, &Swarm::default(), None, false);
            let projection = glam::camera::rh::proj::directx::perspective(1.2, 1.0, 0.05, 50.0);
            let matrices = [-0.032, 0.032]
                .map(|x| projection * Mat4::from_translation(glam::Vec3::new(-x, -1.6, 0.0)));
            // A panel before the eyes, and one behind them, and behind them too another surface, such as a wall's board,
            // the same but for what it is drawn as.
            let mut canvas = crate::ui_panel::Canvas::new(20, 0.004);
            canvas.rows = vec![crate::ui_text::Row::text(
                "Seen",
                crate::ui_text::Style::plain([1.0; 4]),
                20,
            )];
            let mut board = renderer.panel(&device, 6_000);
            board.set(&queue, &canvas);
            canvas.kind = tomori_scene::canvas::Kind::PanelFace;
            let mut ahead = renderer.panel(&device, 6_000);
            ahead.set(&queue, &canvas);
            let mut behind = renderer.panel(&device, 6_000);
            behind.set(&queue, &canvas);
            assert!(!ahead.bounds().is_empty() && ahead.triangles() > 0);
            assert!(ahead.panel_face() && !board.panel_face());
            // Shown the same canvas as a panel's face, a surface is counted as one.
            let mut surface = renderer.panel(&device, 6_000);
            surface.set(&queue, &crate::ui_panel::Canvas::new(20, 0.004));
            assert!(!surface.panel_face());
            let mut retold = crate::ui_panel::Canvas::new(20, 0.004);
            retold.kind = tomori_scene::canvas::Kind::PanelFace;
            surface.set(&queue, &retold);
            assert!(surface.panel_face());
            let panels = [
                (
                    &behind,
                    Mat4::from_translation(glam::Vec3::new(0.0, 1.6, 2.0)),
                ),
                (
                    &board,
                    Mat4::from_translation(glam::Vec3::new(0.5, 1.6, 2.0)),
                ),
                (
                    &ahead,
                    Mat4::from_translation(glam::Vec3::new(0.0, 1.5, -1.5)),
                ),
            ];
            let draw = |renderer: &mut Renderer| {
                renderer.render(
                    &device,
                    &queue,
                    &targets,
                    &matrices,
                    Mat4::IDENTITY,
                    &panels,
                );
                textures.each_ref().map(|texture| {
                    crate::test_support::read_back(&device, &queue, texture, size).into_raw()
                })
            };
            let culled = draw(&mut renderer);
            let tally = renderer.tally();
            // The ring's cubes and the panel and the surface behind are culled, and the rest drawn.
            let cubes = renderer.scene.len();
            assert!(renderer.shown.len() <= cubes - LANDING_CUBES);
            assert_eq!(tally.culled, cubes - renderer.shown.len() + 2, "{tally:?}");
            assert_eq!(tally.drawn, renderer.shown.len() + 1);
            assert_eq!(tally.draws, 2);
            assert_eq!(
                tally.triangles,
                renderer.shown.len() * 12 + ahead.triangles()
            );
            // Of the panels' faces alone, the one ahead drawn and the one behind culled, as the frame log counts them, the
            // surface behind not among them.
            assert_eq!(renderer.panels_seen(), (1, 1));
            // Drawn without culling, both eyes look the same, and nothing is culled.
            renderer.set_culling(false);
            let whole = draw(&mut renderer);
            assert_eq!(renderer.tally().culled, 0);
            assert_eq!(renderer.panels_seen(), (2, 0));
            assert_eq!(renderer.shown.len(), cubes);
            assert!(whole == culled, "culling changed what the eyes see");
            renderer.set_culling(true);
            assert!(draw(&mut renderer) == culled);
            // The frames' GPU time, where the device has timestamps, and what they drew, a frame.
            device.poll(wgpu::PollType::wait_indefinitely()).unwrap();
            draw(&mut renderer);
            let summary = renderer.gpu_summary().unwrap();
            assert!(summary.starts_with("GPU over 4 frames, "), "{summary}");
            assert_eq!(
                summary.contains(" timed, ms mean/p95/max: eyes "),
                device.features().contains(wgpu::Features::TIMESTAMP_QUERY),
                "{summary}"
            );
            assert_eq!(renderer.gpu_summary(), None);
            // Each cube's box holds the cube, scaled, turned and moved.
            let cube = Instance {
                position: [1.0, 2.0, 3.0],
                scale: [0.2, 0.4, 0.6],
                basis: glam::Mat3::from_rotation_y(std::f32::consts::FRAC_PI_2).to_cols_array_2d(),
                ..Default::default()
            };
            let bounds = cube_bounds(&cube);
            assert!(bounds.min.distance(glam::Vec3::new(0.7, 1.8, 2.9)) < 1e-5);
            assert!(bounds.max.distance(glam::Vec3::new(1.3, 2.2, 3.1)) < 1e-5);
            // A ray, a cube stretched along its length and turned along it, is held whole, its ends and edges.
            let along = glam::Vec3::new(1.0, -0.4, -2.0).normalize();
            let turn = glam::Quat::from_rotation_arc(glam::Vec3::Z, along);
            let ray = Instance {
                position: (glam::Vec3::new(0.2, 1.2, -0.3) + along * 1.5).to_array(),
                scale: [0.004, 0.004, 3.0],
                basis: glam::Mat3::from_quat(turn).to_cols_array_2d(),
                ..Default::default()
            };
            let bounds = cube_bounds(&ray);
            for corner in super::cube() {
                let corner = glam::Vec3::from_array(ray.position)
                    + turn
                        * (glam::Vec3::from_array(corner.position)
                            * glam::Vec3::from_array(ray.scale));
                assert!(
                    corner.cmpge(bounds.min - 1e-5).all() && corner.cmple(bounds.max + 1e-5).all()
                );
            }
            assert!((bounds.max - bounds.min).length() < 3.1);
            let error = pollster::block_on(scope.pop());
            assert!(error.is_none(), "{error:?}");
        });
    }
}
