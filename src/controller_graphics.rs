// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

//! GPU rendering for two tracked controller poses, with simple fallback meshes.
use crate::{
    controller::{self, Model, Part, Pose, Texture, Vertex},
    hands::{self, Hand},
};
use glam::{Mat4, Quat, Vec3};
use wgpu::util::DeviceExt;

const VERTEX_ATTRIBUTES: [wgpu::VertexAttribute; 4] =
    wgpu::vertex_attr_array![0 => Float32x3, 1 => Float32x3, 2 => Float32x3, 3 => Float32x2];
const INSTANCE_ATTRIBUTES: [wgpu::VertexAttribute; 4] =
    wgpu::vertex_attr_array![4 => Float32x4, 5 => Float32x4, 6 => Float32x4, 7 => Float32x4];

struct Mesh {
    buffer: wgpu::Buffer,
    parts: Vec<Part>,
    _textures: Vec<wgpu::Texture>,
    texture_bindings: Vec<wgpu::BindGroup>,
}

impl Mesh {
    fn new(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        model: &Model,
        layout: &wgpu::BindGroupLayout,
        sampler: &wgpu::Sampler,
    ) -> Self {
        let white = Texture {
            width: 1,
            height: 1,
            rgba: vec![255; 4],
        };
        let mut textures = Vec::new();
        let mut texture_bindings = Vec::new();
        for image in std::iter::once(&white).chain(&model.textures) {
            let size = wgpu::Extent3d {
                width: image.width,
                height: image.height,
                depth_or_array_layers: 1,
            };
            let texture = device.create_texture(&wgpu::TextureDescriptor {
                label: Some("controller base-color texture"),
                size,
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Rgba8UnormSrgb,
                usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                view_formats: &[],
            });
            queue.write_texture(
                texture.as_image_copy(),
                &image.rgba,
                wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(image.width * 4),
                    rows_per_image: Some(image.height),
                },
                size,
            );
            let view = texture.create_view(&Default::default());
            texture_bindings.push(device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("controller texture binding"),
                layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::TextureView(&view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::Sampler(sampler),
                    },
                ],
            }));
            textures.push(texture);
        }
        Self {
            buffer: device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("controller model vertices"),
                contents: bytemuck::cast_slice(&model.vertices),
                usage: wgpu::BufferUsages::VERTEX,
            }),
            parts: model
                .parts
                .iter()
                .map(|p| Part {
                    start: p.start,
                    count: p.count,
                    texture: p.texture,
                })
                .collect(),
            _textures: textures,
            texture_bindings,
        }
    }
}

pub struct Controllers {
    pipeline: wgpu::RenderPipeline,
    instances: wgpu::Buffer,
    texture_layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    meshes: [Mesh; 2],
    bone_mesh: Mesh,
    visible: [bool; 2],
    bone_count: u32,
}

impl Controllers {
    pub fn new(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        format: wgpu::TextureFormat,
        camera_layout: &wgpu::BindGroupLayout,
    ) -> Self {
        let shader = device.create_shader_module(wgpu::include_wgsl!("controllers.wgsl"));
        let texture_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("controller texture layout"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("controller texture sampler"),
            address_mode_u: wgpu::AddressMode::Repeat,
            address_mode_v: wgpu::AddressMode::Repeat,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("controller pipeline layout"),
            bind_group_layouts: &[Some(camera_layout), Some(&texture_layout)],
            immediate_size: 0,
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("controller models"),
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
                        array_stride: 64,
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
        let instances = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("controller transforms"),
            size: ((2 + 2 * hands::BONES.len()) * 64) as u64,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let bundled: [&[u8]; 2] = [
            include_bytes!("../assets/controllers/quest-touch-plus-left.glb"),
            include_bytes!("../assets/controllers/quest-touch-plus-right.glb"),
        ];
        let meshes = std::array::from_fn(|hand| {
            let model = controller::parse_glb(bundled[hand]).unwrap_or_else(|error| {
                log::warn!(
                    "Could not load bundled controller model: {error:#}; using fallback geometry"
                );
                fallback(hand)
            });
            Mesh::new(device, queue, &model, &texture_layout, &sampler)
        });
        let bone_mesh = Mesh::new(device, queue, &fallback_bone(), &texture_layout, &sampler);
        Self {
            pipeline,
            instances,
            texture_layout,
            sampler,
            meshes,
            bone_mesh,
            visible: [false; 2],
            bone_count: 0,
        }
    }

    pub fn set_model(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        hand: usize,
        model: &Model,
    ) {
        if hand < self.meshes.len() && !model.vertices.is_empty() {
            self.meshes[hand] =
                Mesh::new(device, queue, model, &self.texture_layout, &self.sampler);
        }
    }

    pub fn update(
        &mut self,
        queue: &wgpu::Queue,
        poses: [Option<Pose>; 2],
        tracked_hands: [Option<Hand>; 2],
    ) {
        let mut matrices = Vec::with_capacity(2 + 2 * hands::BONES.len());
        for pose in poses {
            matrices.push(
                pose.filter(|p| p.valid())
                    .map_or(Mat4::IDENTITY, |p| p.transform())
                    .to_cols_array(),
            );
        }
        for hand in 0..2 {
            let before = matrices.len();
            if let Some(tracked) = tracked_hands[hand].filter(Hand::valid) {
                for (start, end) in hands::BONES {
                    if let (Some(a), Some(b)) = (tracked.joints[start], tracked.joints[end])
                        && let Some(transform) = bone_transform(a, b)
                    {
                        matrices.push(transform.to_cols_array());
                    }
                }
            }
            // Prefer a tracked hand when available; otherwise show a controller.
            self.visible[hand] = matrices.len() == before && poses[hand].is_some_and(|p| p.valid());
        }
        self.bone_count = (matrices.len() - 2) as u32;
        queue.write_buffer(&self.instances, 0, bytemuck::cast_slice(&matrices));
    }

    pub fn draw<'a>(&'a self, pass: &mut wgpu::RenderPass<'a>) {
        pass.set_pipeline(&self.pipeline);
        pass.set_vertex_buffer(1, self.instances.slice(..));
        for hand in 0..2 {
            if self.visible[hand] {
                pass.set_vertex_buffer(0, self.meshes[hand].buffer.slice(..));
                for part in &self.meshes[hand].parts {
                    let binding = part.texture.map_or(0, |index| index + 1);
                    pass.set_bind_group(1, &self.meshes[hand].texture_bindings[binding], &[]);
                    pass.draw(
                        part.start..part.start + part.count,
                        hand as u32..hand as u32 + 1,
                    );
                }
            }
        }
        if self.bone_count > 0 {
            pass.set_vertex_buffer(0, self.bone_mesh.buffer.slice(..));
            pass.set_bind_group(1, &self.bone_mesh.texture_bindings[0], &[]);
            let part = &self.bone_mesh.parts[0];
            pass.draw(part.start..part.start + part.count, 2..2 + self.bone_count);
        }
    }
}

fn bone_transform(a: hands::Joint, b: hands::Joint) -> Option<Mat4> {
    if !a.valid() || !b.valid() {
        return None;
    }
    let start = Vec3::from_array(a.position);
    let end = Vec3::from_array(b.position);
    let delta = end - start;
    let length = delta.length();
    if !(0.001..=0.5).contains(&length) {
        return None;
    }
    let width = a.radius.min(b.radius).clamp(0.002, 0.012) * 1.5;
    Some(Mat4::from_scale_rotation_translation(
        Vec3::new(width, length, width),
        Quat::from_rotation_arc(Vec3::Y, delta / length),
        (start + end) * 0.5,
    ))
}

fn fallback_bone() -> Model {
    let mut vertices = Vec::new();
    append_box(&mut vertices, Vec3::ZERO, Vec3::ONE, [0.68, 0.62, 0.54]);
    let count = vertices.len() as u32;
    Model {
        vertices,
        parts: vec![Part {
            start: 0,
            count,
            texture: None,
        }],
        textures: Vec::new(),
    }
}

fn fallback(hand: usize) -> Model {
    let mut vertices = Vec::new();
    let side = if hand == 0 { -1.0 } else { 1.0 };
    append_box(
        &mut vertices,
        Vec3::new(0.0, -0.035, 0.015),
        Vec3::new(0.035, 0.09, 0.035),
        [0.35, 0.4, 0.55],
    );
    append_box(
        &mut vertices,
        Vec3::new(0.0, 0.012, -0.01),
        Vec3::new(0.055, 0.035, 0.07),
        [0.48, 0.54, 0.65],
    );
    append_box(
        &mut vertices,
        Vec3::new(side * 0.018, 0.03, -0.025),
        Vec3::new(0.014, 0.008, 0.014),
        [0.15, 0.18, 0.23],
    );
    let count = vertices.len() as u32;
    Model {
        vertices,
        parts: vec![Part {
            start: 0,
            count,
            texture: None,
        }],
        textures: Vec::new(),
    }
}

fn append_box(out: &mut Vec<Vertex>, center: Vec3, size: Vec3, color: [f32; 3]) {
    for (normal, u, v) in [
        (Vec3::X, Vec3::Y, Vec3::Z),
        (-Vec3::X, Vec3::Y, -Vec3::Z),
        (Vec3::Y, Vec3::Z, Vec3::X),
        (-Vec3::Y, Vec3::Z, -Vec3::X),
        (Vec3::Z, Vec3::X, Vec3::Y),
        (-Vec3::Z, Vec3::X, -Vec3::Y),
    ] {
        for [a, b] in [
            [-1.0, -1.0],
            [1.0, -1.0],
            [1.0, 1.0],
            [-1.0, -1.0],
            [1.0, 1.0],
            [-1.0, 1.0],
        ] {
            let point = (normal + u * a + v * b) * size * 0.5 + center;
            out.push(Vertex {
                position: point.to_array(),
                normal: normal.to_array(),
                color,
                uv: [0.0, 0.0],
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bone_transform_joins_valid_joints_and_rejects_bad_geometry() {
        let a = hands::Joint {
            position: [1.0, 2.0, 3.0],
            radius: 0.01,
        };
        let b = hands::Joint {
            position: [1.1, 2.0, 3.0],
            radius: 0.01,
        };
        let transform = bone_transform(a, b).unwrap();
        assert!(
            transform
                .transform_point3(Vec3::new(0.0, -0.5, 0.0))
                .distance(Vec3::from_array(a.position))
                < 1e-5
        );
        assert!(
            transform
                .transform_point3(Vec3::new(0.0, 0.5, 0.0))
                .distance(Vec3::from_array(b.position))
                < 1e-5
        );
        assert!(bone_transform(a, a).is_none());
        assert!(
            bone_transform(
                a,
                hands::Joint {
                    position: [f32::NAN, 0.0, 0.0],
                    radius: 0.01
                }
            )
            .is_none()
        );
    }
}
