//! A fixed moonlit cloudscape, independent of Tomori's sessions and simulation.
//! Geometry/noise textures are uploaded once; each eye ray-marches a bounded,
//! low-resolution cloud volume and composites it over the opaque scene.
#[cfg(test)]
mod cloud_noise;
mod clouds;
#[cfg(test)]
mod light_bake;
mod lightmap;
mod mesh;
mod shadows;

use bytemuck::{Pod, Zeroable};
use glam::Mat4;
use wgpu::util::DeviceExt;

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct Camera {
    view_projection: [f32; 16],
    inverse_view_projection: [f32; 16],
}

/// GPU resources for Sky. The caller owns render targets, depth, transforms,
/// selection, and all interaction. No device-local clock drives this scenery.
pub struct Sky {
    background: wgpu::RenderPipeline,
    scenery: wgpu::RenderPipeline,
    cameras: [wgpu::Buffer; 2],
    bindings: [wgpu::BindGroup; 2],
    vertices: wgpu::Buffer,
    vertex_count: u32,
    clouds: clouds::Clouds,
    shadows: shadows::Shadows,
    lightmap: lightmap::Lightmap,
}

impl Sky {
    pub fn new(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        format: wgpu::TextureFormat,
        width: u32,
        height: u32,
    ) -> Self {
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("Sky camera layout"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: wgpu::BufferSize::new(size_of::<Camera>() as u64),
                },
                count: None,
            }],
        });
        let cameras = std::array::from_fn(|_| {
            device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("Sky eye camera"),
                size: size_of::<Camera>() as u64,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            })
        });
        let bindings = std::array::from_fn(|eye| {
            device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("Sky eye binding"),
                layout: &layout,
                entries: &[wgpu::BindGroupEntry {
                    binding: 0,
                    resource: cameras[eye].as_entire_binding(),
                }],
            })
        });
        let mesh = mesh::build();
        let vertices = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("Sky static scenery"),
            contents: bytemuck::cast_slice(&mesh),
            usage: wgpu::BufferUsages::VERTEX,
        });
        let shadows = shadows::Shadows::new(device, queue, &vertices, mesh.len() as u32);
        let lightmap = lightmap::Lightmap::new(device, queue);
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("Sky pipeline layout"),
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
        let scenery_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("Shadowed Sky scenery layout"),
            bind_group_layouts: &[Some(&layout), Some(&shadows.layout), Some(&lightmap.layout)],
            immediate_size: 0,
        });
        let shader = device.create_shader_module(wgpu::include_wgsl!("sky.wgsl"));
        let attributes = wgpu::vertex_attr_array![0 => Float32x3, 1 => Float32x3, 2 => Float32x3, 3 => Float32, 4 => Float32x2];
        let vertex_layout = [Some(wgpu::VertexBufferLayout {
            array_stride: size_of::<mesh::Vertex>() as u64,
            step_mode: wgpu::VertexStepMode::Vertex,
            attributes: &attributes,
        })];
        let pipeline = |scenery| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(if scenery {
                    "Sky scenery"
                } else {
                    "Sky background"
                }),
                layout: Some(if scenery {
                    &scenery_layout
                } else {
                    &pipeline_layout
                }),
                vertex: wgpu::VertexState {
                    module: &shader,
                    entry_point: Some(if scenery {
                        "scenery_vertex"
                    } else {
                        "sky_vertex"
                    }),
                    compilation_options: Default::default(),
                    buffers: if scenery { &vertex_layout } else { &[] },
                },
                fragment: Some(wgpu::FragmentState {
                    module: &shader,
                    entry_point: Some(if scenery {
                        "scenery_fragment"
                    } else {
                        "sky_fragment"
                    }),
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
                    depth_write_enabled: Some(scenery),
                    depth_compare: Some(if scenery {
                        wgpu::CompareFunction::Less
                    } else {
                        wgpu::CompareFunction::LessEqual
                    }),
                    stencil: Default::default(),
                    bias: Default::default(),
                }),
                multisample: Default::default(),
                multiview_mask: None,
                cache: None,
            })
        };
        Self {
            background: pipeline(false),
            scenery: pipeline(true),
            cameras,
            bindings,
            vertices,
            vertex_count: mesh.len() as u32,
            clouds: clouds::Clouds::new(device, queue, format, &layout, width, height),
            shadows,
            lightmap,
        }
    }

    pub fn resize(&mut self, device: &wgpu::Device, width: u32, height: u32) {
        self.clouds.resize(device, width, height);
    }

    /// Record the low-resolution density integration before beginning the main
    /// scene pass. Call after preparing this eye's camera.
    pub fn encode_clouds(&self, encoder: &mut wgpu::CommandEncoder, eye: usize) {
        self.clouds.encode(encoder, &self.bindings[eye], eye);
    }

    /// Premultiplied-alpha composition after opaque content, before transparent
    /// labels. Depth-tested without depth writes, using the cloud slab's entry.
    pub fn draw_clouds(&self, pass: &mut wgpu::RenderPass<'_>, eye: usize) {
        self.clouds.draw(pass, &self.bindings[eye], eye);
    }

    /// Use the complete scene-to-clip transform, including the scene's recenter
    /// anchor. Separate eye buffers prevent one eye's upload replacing the other.
    pub fn prepare_eye(&self, queue: &wgpu::Queue, eye: usize, view_projection: Mat4) {
        let inverse = view_projection.inverse();
        let camera = Camera {
            view_projection: view_projection.to_cols_array(),
            inverse_view_projection: if inverse.is_finite() {
                inverse
            } else {
                Mat4::IDENTITY
            }
            .to_cols_array(),
        };
        queue.write_buffer(&self.cameras[eye], 0, bytemuck::bytes_of(&camera));
    }

    /// Draw before application content, into a pass with a cleared depth buffer.
    /// The distant sky never writes depth; platform and shrine do. Volumetric
    /// clouds are composited separately after the application's opaque content.
    pub fn draw(&self, pass: &mut wgpu::RenderPass<'_>, eye: usize) {
        pass.set_bind_group(0, &self.bindings[eye], &[]);
        pass.set_pipeline(&self.background);
        pass.draw(0..3, 0..1);
        pass.set_pipeline(&self.scenery);
        pass.set_bind_group(1, &self.shadows.binding, &[]);
        pass.set_bind_group(2, &self.lightmap.binding, &[]);
        pass.set_vertex_buffer(0, self.vertices.slice(..));
        pass.draw(0..self.vertex_count, 0..1);
    }
}
