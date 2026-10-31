// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

//! Half-resolution bounded volumetric clouds, with no temporal history or motion.
use wgpu::util::DeviceExt;

const NOISE_SIZE: u32 = 64;
const NOISE: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/cloud-noise.rgba"));
const _: () = assert!(NOISE.len() == (NOISE_SIZE * NOISE_SIZE * NOISE_SIZE * 4) as usize);

struct Target {
    _texture: wgpu::Texture,
    view: wgpu::TextureView,
    binding: wgpu::BindGroup,
}

fn resolution(width: u32, height: u32) -> (u32, u32) {
    let divisor = 2.max(width.div_ceil(960)).max(height.div_ceil(960));
    (
        width.div_ceil(divisor).max(1),
        height.div_ceil(divisor).max(1),
    )
}

pub(super) struct Clouds {
    volume: wgpu::RenderPipeline,
    composite: wgpu::RenderPipeline,
    noise: wgpu::BindGroup,
    image_layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    targets: [Target; 2],
}

impl Clouds {
    pub fn new(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        format: wgpu::TextureFormat,
        camera_layout: &wgpu::BindGroupLayout,
        width: u32,
        height: u32,
    ) -> Self {
        let texture_layout = |dimension| {
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("Cloud texture layout"),
                entries: &[
                    wgpu::BindGroupLayoutEntry {
                        binding: 0,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Texture {
                            sample_type: wgpu::TextureSampleType::Float { filterable: true },
                            view_dimension: dimension,
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
            })
        };
        let noise_layout = texture_layout(wgpu::TextureViewDimension::D3);
        let image_layout = texture_layout(wgpu::TextureViewDimension::D2);
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("Cloud linear upsampling"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        let noise_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("Cloud periodic density sampler"),
            address_mode_u: wgpu::AddressMode::Repeat,
            address_mode_v: wgpu::AddressMode::Repeat,
            address_mode_w: wgpu::AddressMode::Repeat,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        let noise_texture = device.create_texture_with_data(
            queue,
            &wgpu::TextureDescriptor {
                label: Some("Cloud baked shape/detail noise"),
                size: wgpu::Extent3d {
                    width: NOISE_SIZE,
                    height: NOISE_SIZE,
                    depth_or_array_layers: NOISE_SIZE,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D3,
                format: wgpu::TextureFormat::Rgba8Unorm,
                usage: wgpu::TextureUsages::TEXTURE_BINDING,
                view_formats: &[],
            },
            wgpu::util::TextureDataOrder::LayerMajor,
            NOISE,
        );
        let noise = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("Cloud noise binding"),
            layout: &noise_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(
                        &noise_texture.create_view(&Default::default()),
                    ),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&noise_sampler),
                },
            ],
        });
        let pipeline =
            |fragment: &str, source: &str, textures: &wgpu::BindGroupLayout, composite: bool| {
                let source = format!("{}\n{source}", include_str!("cloud_common.wgsl"));
                let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
                    label: Some(fragment),
                    source: wgpu::ShaderSource::Wgsl(source.into()),
                });
                let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                    label: Some(fragment),
                    bind_group_layouts: &[Some(camera_layout), Some(textures)],
                    immediate_size: 0,
                });
                device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                    label: Some(fragment),
                    layout: Some(&layout),
                    vertex: wgpu::VertexState {
                        module: &shader,
                        entry_point: Some("ray_vertex"),
                        compilation_options: Default::default(),
                        buffers: &[],
                    },
                    fragment: Some(wgpu::FragmentState {
                        module: &shader,
                        entry_point: Some(fragment),
                        compilation_options: Default::default(),
                        targets: &[Some(wgpu::ColorTargetState {
                            format: if composite {
                                format
                            } else {
                                wgpu::TextureFormat::Rgba8Unorm
                            },
                            blend: composite
                                .then_some(wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING),
                            write_mask: wgpu::ColorWrites::ALL,
                        })],
                    }),
                    primitive: wgpu::PrimitiveState::default(),
                    depth_stencil: composite.then_some(wgpu::DepthStencilState {
                        format: wgpu::TextureFormat::Depth32Float,
                        depth_write_enabled: Some(false),
                        depth_compare: Some(wgpu::CompareFunction::LessEqual),
                        stencil: Default::default(),
                        bias: Default::default(),
                    }),
                    multisample: Default::default(),
                    multiview_mask: None,
                    cache: None,
                })
            };
        Self {
            volume: pipeline(
                "volume_fragment",
                include_str!("cloud_volume.wgsl"),
                &noise_layout,
                false,
            ),
            composite: pipeline(
                "composite_fragment",
                include_str!("cloud_composite.wgsl"),
                &image_layout,
                true,
            ),
            targets: Self::targets(device, &image_layout, &sampler, width, height),
            noise,
            image_layout,
            sampler,
        }
    }

    fn targets(
        device: &wgpu::Device,
        layout: &wgpu::BindGroupLayout,
        sampler: &wgpu::Sampler,
        width: u32,
        height: u32,
    ) -> [Target; 2] {
        // Half resolution normally, capped at 960 pixels per axis for 4K spectators.
        let (width, height) = resolution(width, height);
        std::array::from_fn(|_| {
            let texture = device.create_texture(&wgpu::TextureDescriptor {
                label: Some("Cloud low-resolution eye"),
                size: wgpu::Extent3d {
                    width,
                    height,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Rgba8Unorm,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                    | wgpu::TextureUsages::TEXTURE_BINDING
                    | wgpu::TextureUsages::COPY_SRC,
                view_formats: &[],
            });
            let view = texture.create_view(&Default::default());
            let binding = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("Cloud image binding"),
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
            });
            Target {
                _texture: texture,
                view,
                binding,
            }
        })
    }

    pub fn resize(&mut self, device: &wgpu::Device, width: u32, height: u32) {
        self.targets = Self::targets(device, &self.image_layout, &self.sampler, width, height);
    }

    pub fn encode(&self, encoder: &mut wgpu::CommandEncoder, camera: &wgpu::BindGroup, eye: usize) {
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("Cloud density integration"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &self.targets[eye].view,
                resolve_target: None,
                depth_slice: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                    store: wgpu::StoreOp::Store,
                },
            })],
            ..Default::default()
        });
        pass.set_pipeline(&self.volume);
        pass.set_bind_group(0, camera, &[]);
        pass.set_bind_group(1, &self.noise, &[]);
        pass.draw(0..3, 0..1);
    }

    pub fn draw(&self, pass: &mut wgpu::RenderPass<'_>, camera: &wgpu::BindGroup, eye: usize) {
        pass.set_pipeline(&self.composite);
        pass.set_bind_group(0, camera, &[]);
        pass.set_bind_group(1, &self.targets[eye].binding, &[]);
        pass.draw(0..3, 0..1);
    }
}

#[cfg(test)]
#[path = "cloud_tests.rs"]
mod tests;
