// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

use crate::{
    controller::{Model as ControllerModel, Pose},
    controller_graphics::Controllers,
    environment::Environment,
    hands::Hand,
    interaction::Mode,
    swarm::{AGENT_COUNT, CENTER, Swarm},
};
use bytemuck::{Pod, Zeroable};
use glam::Mat4;
use wgpu::util::DeviceExt;

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct Vertex {
    position: [f32; 3],
    normal: [f32; 3],
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct Instance {
    position: [f32; 3],
    scale: [f32; 3],
    color: [f32; 3],
    basis: [[f32; 3]; 3],
}

impl Default for Instance {
    fn default() -> Self {
        Self {
            position: [0.0; 3],
            scale: [1.0; 3],
            color: [1.0; 3],
            basis: glam::Mat3::IDENTITY.to_cols_array_2d(),
        }
    }
}

const MAX_INSTANCES: usize = AGENT_COUNT + 160 + crate::delegation::MAX_CUBES;
const VERTEX_ATTRIBUTES: [wgpu::VertexAttribute; 2] =
    wgpu::vertex_attr_array![0 => Float32x3, 1 => Float32x3];
const INSTANCE_ATTRIBUTES: [wgpu::VertexAttribute; 6] = wgpu::vertex_attr_array![2 => Float32x3, 3 => Float32x3, 4 => Float32x3, 5 => Float32x3, 6 => Float32x3, 7 => Float32x3];

// Keep the shared cube lighting shader, extending its instance transform here.
// The source shader is also used independently by offscreen reference tests.
fn oriented_shader() -> String {
    include_str!("swarm.wgsl")
        .replace("    @location(4) color: vec3<f32>,", "    @location(4) color: vec3<f32>,\n    @location(5) basis_x: vec3<f32>,\n    @location(6) basis_y: vec3<f32>,\n    @location(7) basis_z: vec3<f32>,")
        .replace("    var out: VertexOutput;", "    let rotation = mat3x3<f32>(basis_x, basis_y, basis_z);\n    var out: VertexOutput;")
        .replace("position + vertex * scale", "position + rotation * (vertex * scale)")
        .replace("out.normal = normal;", "out.normal = rotation * normal;")
}

pub struct Renderer {
    environment: Environment,
    sky: environment_sky::Sky,
    pipeline: wgpu::RenderPipeline,
    vertices: wgpu::Buffer,
    instances: wgpu::Buffer,
    cameras: [wgpu::Buffer; 2],
    camera_bindings: [wgpu::BindGroup; 2],
    depth: [wgpu::TextureView; 2],
    scene: Vec<Instance>,
    vertex_count: u32,
    controllers: Controllers,
    caption_offset: f32,
    base_scene_len: usize,
    workspace: crate::workspace_graphics::WorkspaceGraphics,
}

impl Renderer {
    pub fn new(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        format: wgpu::TextureFormat,
        width: u32,
        height: u32,
    ) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("oriented swarm instances"),
            source: wgpu::ShaderSource::Wgsl(oriented_shader().into()),
        });
        let camera_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("eye camera layout"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: wgpu::BufferSize::new(64),
                },
                count: None,
            }],
        });
        let cameras = std::array::from_fn(|_| {
            device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("eye camera"),
                size: 64,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            })
        });
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
            multisample: Default::default(),
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
            size: (MAX_INSTANCES * size_of::<Instance>()) as u64,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let depth = Self::depth_views(device, width, height);
        let controllers = Controllers::new(device, queue, format, &camera_layout);
        let workspace =
            crate::workspace_graphics::WorkspaceGraphics::new(device, format, &camera_layout);
        Self {
            environment: Environment::initial(),
            sky: environment_sky::Sky::new(device, queue, format, width, height),
            pipeline,
            vertices,
            instances,
            cameras,
            camera_bindings,
            depth,
            scene: Vec::with_capacity(MAX_INSTANCES),
            vertex_count: mesh.len() as u32,
            controllers,
            caption_offset: 0.0,
            base_scene_len: 0,
            workspace,
        }
    }

    fn depth_views(device: &wgpu::Device, width: u32, height: u32) -> [wgpu::TextureView; 2] {
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
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    format: wgpu::TextureFormat::Depth32Float,
                    usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
                    view_formats: &[],
                })
                .create_view(&Default::default())
        })
    }

    #[cfg(any(not(target_os = "android"), test))]
    pub fn resize(&mut self, device: &wgpu::Device, width: u32, height: u32) {
        self.depth = Self::depth_views(device, width, height);
        self.sky.resize(device, width, height);
    }

    pub fn environment(&self) -> Environment {
        self.environment
    }

    /// Does not modify simulation, worker placement, input or conversation state.
    /// The next `update` rebuilds only the presentation instances.
    pub fn set_environment(&mut self, environment: Environment) {
        self.environment = environment;
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
        // Keep the debug console, but move it below the conversational body.
        self.caption_offset = if swarm.visual.mode == Mode::Neutral {
            0.0
        } else {
            -0.4
        };
        swarm_instances(swarm, &mut self.scene);
        if self.environment == Environment::Minimal {
            minimal_floor(&mut self.scene);
        }
        // Keep the coordinator's orientation ring in both environments.
        for n in 0..40 {
            let angle = n as f32 * std::f32::consts::TAU / 40.0;
            self.scene.push(Instance {
                position: [
                    CENTER.x + angle.cos() * 1.25,
                    0.015,
                    CENTER.z + angle.sin() * 1.25,
                ],
                scale: [0.035, 0.015, 0.035],
                color: [0.08, 0.5, 0.65],
                ..Default::default()
            });
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
        self.workspace.update(device, queue, workspace, stations);
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

    pub fn render(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        targets: &[wgpu::TextureView],
        matrices: &[Mat4],
        scene_to_space: Mat4,
        caption: Option<(&crate::caption::Caption, Mat4)>,
    ) -> wgpu::SubmissionIndex {
        assert_eq!(targets.len(), matrices.len());
        assert!((1..=2).contains(&targets.len()));
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("swarm frame"),
        });
        for eye in 0..targets.len() {
            let scene_to_clip = matrices[eye] * scene_to_space;
            if self.environment == Environment::Sky {
                self.sky.prepare_eye(queue, eye, scene_to_clip);
                self.sky.encode_clouds(&mut encoder, eye);
            }
            // Separate buffers are important: two writes to one buffer before
            // submission would make both eyes use the right-eye transform.
            queue.write_buffer(
                &self.cameras[eye],
                0,
                bytemuck::cast_slice(&scene_to_clip.to_cols_array()),
            );
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("eye"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &targets[eye],
                    resolve_target: None,
                    depth_slice: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color {
                            r: 0.006,
                            g: 0.009,
                            b: 0.025,
                            a: 1.0,
                        }),
                        store: wgpu::StoreOp::Store,
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
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            if self.environment == Environment::Sky {
                self.sky.draw(&mut pass, eye);
            }
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &self.camera_bindings[eye], &[]);
            pass.set_vertex_buffer(0, self.vertices.slice(..));
            pass.set_vertex_buffer(1, self.instances.slice(..));
            pass.draw(0..self.vertex_count, 0..self.scene.len() as u32);
            self.controllers.draw(&mut pass);
            if self.environment == Environment::Sky {
                self.sky.draw_clouds(&mut pass, eye);
            }
            self.workspace.draw(
                &mut pass,
                &self.camera_bindings[eye],
                matrices[eye] * scene_to_space,
            );
            drop(pass);
            if let Some((caption, head)) = caption {
                let head =
                    head * Mat4::from_translation(glam::Vec3::new(0.0, self.caption_offset, 0.0));
                caption.draw(&mut encoder, queue, &targets[eye], eye, matrices[eye], head);
            }
        }
        queue.submit([encoder.finish()])
    }
}

fn minimal_floor(scene: &mut Vec<Instance>) {
    // Original stationary grid retained as a regression/comfort baseline.
    for n in -10..=10 {
        let f = n as f32 * 0.5;
        let color = if n == 0 {
            [0.08, 0.22, 0.3]
        } else {
            [0.025, 0.065, 0.09]
        };
        scene.push(Instance {
            position: [f, 0.0, -2.5],
            scale: [0.008, 0.004, 10.0],
            color,
            ..Default::default()
        });
        scene.push(Instance {
            position: [0.0, 0.0, f - 2.5],
            scale: [10.0, 0.004, 0.008],
            color,
            ..Default::default()
        });
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
            position: [
                CENTER.x + angle.cos() * 1.13,
                CENTER.y + angle.sin() * 0.70,
                CENTER.z - 0.28,
            ],
            scale: [0.025, 0.025, 0.025],
            color,
            ..Default::default()
        });
    }
}

fn cube() -> Vec<Vertex> {
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
mod tests {
    use super::*;

    #[test]
    #[ignore = "requires a Vulkan adapter"]
    fn sky_environment_renders_stereo_switches_and_preserves_recenter() {
        pollster::block_on(async {
            const WIDTH: u32 = 768;
            const HEIGHT: u32 = 512;
            let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
                backends: wgpu::Backends::VULKAN,
                ..wgpu::InstanceDescriptor::new_without_display_handle()
            });
            let adapter = instance.request_adapter(&Default::default()).await.unwrap();
            let (device, queue) = adapter.request_device(&Default::default()).await.unwrap();
            let scope = device.push_error_scope(wgpu::ErrorFilter::Validation);
            let textures: [_; 2] = std::array::from_fn(|_| {
                device.create_texture(&wgpu::TextureDescriptor {
                    label: Some("Sky test eye"),
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
                })
            });
            let views = textures
                .each_ref()
                .map(|t| t.create_view(&Default::default()));
            let mut renderer = Renderer::new(
                &device,
                &queue,
                wgpu::TextureFormat::Rgba8UnormSrgb,
                WIDTH,
                HEIGHT,
            );
            let mut swarm = Swarm::new();
            for _ in 0..240 {
                swarm.set_visual(
                    crate::interaction::Visual {
                        mode: Mode::Ready,
                        ..Default::default()
                    },
                    1.0 / 72.0,
                );
                swarm.step(1.0 / 72.0, None);
            }
            let positions: Vec<_> = swarm.agents.iter().map(|a| a.position).collect();
            let projection = Mat4::perspective_rh(1.2, WIDTH as f32 / HEIGHT as f32, 0.05, 100.0);
            let matrices = [-0.032, 0.032].map(|x| {
                let eye = glam::Vec3::new(x, 1.6, 0.0);
                projection
                    * Mat4::look_at_rh(eye, eye + glam::Vec3::new(0.0, 0.15, -1.0), glam::Vec3::Y)
            });
            let capture = |renderer: &Renderer, matrices: [Mat4; 2], anchor: Mat4| {
                renderer.render(&device, &queue, &views, &matrices, anchor, None);
                let buffers: [_; 2] = std::array::from_fn(|_| {
                    device.create_buffer(&wgpu::BufferDescriptor {
                        label: Some("Sky test readback"),
                        size: u64::from(WIDTH * HEIGHT * 4),
                        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
                        mapped_at_creation: false,
                    })
                });
                let mut encoder = device.create_command_encoder(&Default::default());
                for eye in 0..2 {
                    encoder.copy_texture_to_buffer(
                        textures[eye].as_image_copy(),
                        wgpu::TexelCopyBufferInfo {
                            buffer: &buffers[eye],
                            layout: wgpu::TexelCopyBufferLayout {
                                offset: 0,
                                bytes_per_row: Some(WIDTH * 4),
                                rows_per_image: Some(HEIGHT),
                            },
                        },
                        wgpu::Extent3d {
                            width: WIDTH,
                            height: HEIGHT,
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
                                tx.send(result).unwrap();
                            });
                        device.poll(wgpu::PollType::wait_indefinitely()).unwrap();
                        rx.recv().unwrap().unwrap();
                        let data = buffer.slice(..).get_mapped_range().unwrap().to_vec();
                        buffer.unmap();
                        data
                    })
                    .collect::<Vec<_>>()
            };
            renderer.set_environment(Environment::Minimal);
            renderer.update(&queue, &swarm, None, false);
            let minimal = capture(&renderer, matrices, Mat4::IDENTITY);
            renderer.set_environment(Environment::Sky);
            renderer.update(&queue, &swarm, None, false);
            let sky = capture(&renderer, matrices, Mat4::IDENTITY);
            assert_ne!(sky[0], minimal[0]);
            assert_ne!(sky[0], sky[1], "scenery must have stereo parallax");
            // Return to the original preset without touching the simulation.
            renderer.set_environment(Environment::Minimal);
            renderer.update(&queue, &swarm, None, false);
            assert_eq!(minimal, capture(&renderer, matrices, Mat4::IDENTITY));
            assert_eq!(
                positions,
                swarm.agents.iter().map(|a| a.position).collect::<Vec<_>>()
            );
            renderer.set_environment(Environment::Sky);
            renderer.update(&queue, &swarm, None, false);
            let anchor = Mat4::from_rotation_translation(
                glam::Quat::from_rotation_y(0.35),
                glam::Vec3::new(1.0, 0.0, -0.5),
            );
            let compensated = capture(&renderer, matrices.map(|m| m * anchor.inverse()), anchor);
            for eye in 0..2 {
                let changed = sky[eye]
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .zip(compensated[eye].as_chunks::<4>().0)
                    .filter(|(a, b)| a != b)
                    .count();
                assert!(
                    changed < 2000,
                    "recenter changed {changed} pixels in eye {eye}"
                );
            }
            // Distant sky follows orientation, not the viewer's translation.
            let translated = capture(
                &renderer,
                matrices.map(|m| m * Mat4::from_translation(glam::Vec3::new(-0.4, 0.0, 0.0))),
                Mat4::IDENTITY,
            );
            let top_bytes = (WIDTH * 100 * 4) as usize;
            let sky_changes = sky[0][..top_bytes]
                .as_chunks::<4>()
                .0
                .iter()
                .zip(translated[0][..top_bytes].as_chunks::<4>().0)
                .filter(|(a, b)| a != b)
                .count();
            assert!(
                sky_changes < 800,
                "translation must not drag the sky: {sky_changes}"
            );
            let overview = projection
                * Mat4::look_at_rh(
                    glam::Vec3::new(6.0, 5.0, 8.0),
                    glam::Vec3::new(0.0, 0.7, -2.5),
                    glam::Vec3::Y,
                );
            let overview = capture(&renderer, [overview; 2], Mat4::IDENTITY);
            if let Ok(path) = std::env::var("TOMORI_SKY_PREVIEW") {
                let mut sheet = image::RgbaImage::new(WIDTH * 2, HEIGHT * 2);
                for (i, bytes) in [&sky[0], &sky[1], &minimal[0], &overview[0]]
                    .into_iter()
                    .enumerate()
                {
                    let image = image::RgbaImage::from_raw(WIDTH, HEIGHT, bytes.clone()).unwrap();
                    image::imageops::replace(
                        &mut sheet,
                        &image,
                        (i % 2) as i64 * i64::from(WIDTH),
                        (i / 2) as i64 * i64::from(HEIGHT),
                    );
                }
                sheet.save(path).unwrap();
            }
            assert!(scope.pop().await.is_none());
        });
    }

    #[test]
    fn activity_geometry_is_bounded_and_ready_halo_is_persistent() {
        let mut swarm = Swarm::new();
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
    #[ignore = "requires a Vulkan adapter; run cargo test -- --ignored"]
    fn conversational_states_render_distinct_images() {
        pollster::block_on(async {
            let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
                backends: wgpu::Backends::VULKAN,
                ..wgpu::InstanceDescriptor::new_without_display_handle()
            });
            let adapter = instance.request_adapter(&Default::default()).await.unwrap();
            let (device, queue) = adapter.request_device(&Default::default()).await.unwrap();
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
            let camera = Mat4::perspective_rh(1.1, 512.0 / 384.0, 0.05, 50.0)
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
                let mut swarm = Swarm::new();
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
                    None,
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
            if let Ok(path) = std::env::var("TOMORI_SIGNAL_PREVIEW") {
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
                sheet.save(path).unwrap();
            }
            assert!(scope.pop().await.is_none());
        });
    }

    #[test]
    #[ignore = "requires a Vulkan adapter; run cargo test -- --ignored"]
    fn both_controller_poses_change_the_offscreen_image() {
        pollster::block_on(async {
            let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
                backends: wgpu::Backends::VULKAN,
                ..wgpu::InstanceDescriptor::new_without_display_handle()
            });
            let adapter = instance.request_adapter(&Default::default()).await.unwrap();
            let (device, queue) = adapter.request_device(&Default::default()).await.unwrap();
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
            let mut swarm = Swarm::new();
            swarm.agents.clear();
            renderer.update(&queue, &swarm, None, false);
            let capture = |renderer: &Renderer| {
                renderer.render(
                    &device,
                    &queue,
                    std::slice::from_ref(&view),
                    &[Mat4::IDENTITY],
                    Mat4::IDENTITY,
                    None,
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
            let empty = capture(&renderer);
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
            let with_controllers = capture(&renderer);
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
            let with_hands = capture(&renderer);
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
            assert!(error_scope.pop().await.is_none());
        });
    }

    #[test]
    #[ignore = "requires a Vulkan adapter; run cargo test -- --ignored"]
    fn workspace_pages_render_stereo_recenter_and_clear() {
        pollster::block_on(async {
            let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
                backends: wgpu::Backends::VULKAN,
                ..wgpu::InstanceDescriptor::new_without_display_handle()
            });
            let adapter = instance.request_adapter(&Default::default()).await.unwrap();
            let (device, queue) = adapter.request_device(&Default::default()).await.unwrap();
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
                    usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
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
            let mut swarm = Swarm::new();
            swarm.agents.clear();
            renderer.update(&queue, &swarm, None, false);
            let projection = Mat4::perspective_rh(1.0, 1.0, 0.05, 50.0);
            let direction =
                (glam::Vec3::new(-3.5, 2.4, -3.35) - glam::Vec3::new(0.0, 1.6, 0.0)).normalize();
            let matrices = [-0.032, 0.032].map(|x| {
                let eye = glam::Vec3::new(x, 1.6, 0.0);
                projection * Mat4::look_at_rh(eye, eye + direction, glam::Vec3::Y)
            });
            let capture = |renderer: &Renderer, matrices: [Mat4; 2], scene_to_space: Mat4| {
                renderer.render(&device, &queue, &views, &matrices, scene_to_space, None);
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
            let empty = capture(&renderer, matrices, Mat4::IDENTITY);
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
            renderer.update_workspace(&device, &queue, &[], &[], &workspace);
            assert_eq!(renderer.presentation_page_count(), 5);
            let page0 = capture(&renderer, matrices, Mat4::IDENTITY);
            assert_ne!(page0[0], empty[0]);
            assert_ne!(page0[1], empty[1]);
            assert_ne!(page0[0], page0[1], "each eye uses its own matrix");
            renderer.set_presentation_content_page(1);
            renderer.update_workspace(&device, &queue, &[], &[], &workspace);
            let content_page1 = capture(&renderer, matrices, Mat4::IDENTITY);
            for eye in 0..2 {
                assert_ne!(page0[eye], content_page1[eye], "content page in eye {eye}");
            }
            renderer.set_presentation_content_page(0);
            renderer.update_workspace(&device, &queue, &[], &[], &workspace);
            let recenter = Mat4::from_translation(glam::Vec3::new(0.5, 0.0, 0.0));
            let compensated = capture(
                &renderer,
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
            renderer.update_workspace(&device, &queue, &[], &[], &workspace);
            let page1 = capture(&renderer, matrices, Mat4::IDENTITY);
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
            renderer.update_workspace(&device, &queue, &[cube, invalid], &[], &Default::default());
            assert_eq!(renderer.scene.len(), renderer.base_scene_len + 1);
            let unrotated = capture(&renderer, matrices, Mat4::IDENTITY);
            let rotated = crate::delegation::Cube {
                orientation: glam::Quat::from_euler(glam::EulerRot::YXZ, 0.6, 0.35, 0.15)
                    .to_array(),
                ..cube
            };
            renderer.update_workspace(&device, &queue, &[rotated], &[], &Default::default());
            assert_ne!(
                unrotated,
                capture(&renderer, matrices, Mat4::IDENTITY),
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
            );
            assert_eq!(renderer.scene.len(), renderer.base_scene_len);
            renderer.update_workspace(&device, &queue, &[], &[], &Default::default());
            assert_eq!(renderer.scene.len(), renderer.base_scene_len);
            assert_eq!(renderer.presentation_page_count(), 0);
            assert_eq!(capture(&renderer, matrices, Mat4::IDENTITY), empty);

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
                projection * Mat4::look_at_rh(eye, eye + direction, glam::Vec3::Y)
            });
            let without_label = capture(&renderer, label_matrices, Mat4::IDENTITY);
            renderer.update_workspace(&device, &queue, &[], workers.stations(), &label_workspace);
            let with_label = capture(&renderer, label_matrices, Mat4::IDENTITY);
            for eye in 0..2 {
                assert_ne!(
                    without_label[eye], with_label[eye],
                    "worker label in eye {eye}"
                );
            }
            let mut faded = workers.stations().to_vec();
            faded[0].opacity = 0.4;
            renderer.update_workspace(&device, &queue, &[], &faded, &label_workspace);
            let faded_label = capture(&renderer, label_matrices, Mat4::IDENTITY);
            for eye in 0..2 {
                assert_ne!(
                    with_label[eye], faded_label[eye],
                    "label transition in eye {eye}"
                );
            }
            let preview_path = std::env::var("TOMORI_TERMINAL_PREVIEW").ok();
            let mut sheet = preview_path
                .as_ref()
                .map(|_| image::RgbaImage::new(768, 512));
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
                let focus = Mat4::perspective_rh(0.22, 1.0, 0.05, 50.0);
                let matrices = [-0.032, 0.032].map(|x| {
                    focus * Mat4::look_at_rh(glam::Vec3::new(x, 1.6, 0.0), anchor, glam::Vec3::Y)
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
                    );
                    assert_eq!(
                        renderer.scene.len(),
                        renderer.base_scene_len + scene.cubes().len()
                    );
                    let image = capture(&renderer, matrices, Mat4::IDENTITY);
                    if let Some(previous) = &previous {
                        assert_ne!(
                            *previous, image,
                            "actual terminal ambient motion must reach the renderer"
                        );
                    }
                    if let Some(sheet) = &mut sheet {
                        let tile = image::RgbaImage::from_raw(256, 256, image[0].clone()).unwrap();
                        image::imageops::replace(sheet, &tile, column * 256, row as i64 * 256);
                    }
                    previous = Some(image);
                }
            }
            if let (Some(sheet), Some(path)) = (sheet, preview_path) {
                sheet.save(path).unwrap();
            }
            device.poll(wgpu::PollType::wait_indefinitely()).unwrap();
            assert!(scope.pop().await.is_none());
        });
    }

    #[test]
    #[ignore = "requires a Vulkan adapter; run cargo test -- --ignored"]
    fn renders_stereo_offscreen() {
        pollster::block_on(async {
            let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
                backends: wgpu::Backends::VULKAN,
                ..wgpu::InstanceDescriptor::new_without_display_handle()
            });
            let adapter = instance.request_adapter(&Default::default()).await.unwrap();
            let (device, queue) = adapter.request_device(&Default::default()).await.unwrap();
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
            renderer.update(&queue, &Swarm::new(), None, false);
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
            let projection = Mat4::perspective_rh(1.2, 1.0, 0.05, 50.0);
            let matrices = [-0.032, 0.032]
                .map(|x| projection * Mat4::from_translation(glam::Vec3::new(-x, -1.6, 0.0)));
            let mut caption =
                crate::caption::Caption::new(&device, wgpu::TextureFormat::Rgba8UnormSrgb);
            caption.set_text(
                &queue,
                "VOICE TO TEXT\nReady.\n\nHello from the Quest microphone!",
            );
            // Exercise empty/green/amber/red/full recording bars and hiding
            // the overlay again without replacing the conversation text.
            for elapsed in [Some(0), Some(15), Some(50), Some(55), Some(60), None] {
                caption.set_recording_progress(
                    &queue,
                    elapsed.map(std::time::Duration::from_secs),
                    std::time::Duration::from_secs(60),
                );
                renderer.render(
                    &device,
                    &queue,
                    &targets,
                    &matrices,
                    Mat4::IDENTITY,
                    Some((
                        &caption,
                        Mat4::from_translation(glam::Vec3::new(0.0, 1.6, 0.0)),
                    )),
                );
            }
            // Exercise every activity shape/halo through both eye targets.
            let mut swarm = Swarm::new();
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
                renderer.render(&device, &queue, &targets, &matrices, Mat4::IDENTITY, None);
            }
            // Exercise the same single-view + resized-depth path as desktop.
            renderer.resize(&device, 128, 128);
            renderer.render(
                &device,
                &queue,
                &targets[..1],
                &matrices[..1],
                Mat4::IDENTITY,
                None,
            );
            device.poll(wgpu::PollType::wait_indefinitely()).unwrap();
            assert!(error_scope.pop().await.is_none());
        });
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
        assert!(shader.contains("rotation * (vertex * scale)"));
        assert!(shader.contains("out.normal = rotation * normal;"));
        assert!(shader.contains("@location(7) basis_z"));
    }
}
