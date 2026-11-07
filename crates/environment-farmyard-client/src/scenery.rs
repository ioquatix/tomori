// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

//! The farmyard's scenery, as Tomori's clients draw it in each eye's pass: the evening sky behind everything, then the
//! yard and the coop, models drawn with `tomori-assets`, lit by the moon and the lanterns' warm light, all of it growing
//! out of the space through the reveal sphere as the space's environment does.
use std::sync::Arc;

use bytemuck::{Pod, Zeroable};
use glam::{Mat4, Vec3};
use tomori_assets::gpu::{Batch, GpuModel, Lamp, Lighting, Renderer};
use tomori_assets::{Instance, Library, Model};
use tomori_render::scenery::{Scenery, Target};

use tomori_environment_farmyard::coop::{self, Coop};
use tomori_environment_farmyard::layout;
use tomori_environment_farmyard::props::{self, palette};

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct SkyCamera {
    inverse_view_projection: [f32; 16],
    reveal: [f32; 4],
}

/// How the farmyard is lit: a cool moon low in the east, the sky's blue overhead and the earth's brown below, the
/// lanterns' warm light by the coop's door and on the gate, and a dusky haze over the far meadow.
pub fn lighting() -> Lighting {
    Lighting::new(
        Vec3::new(0.62, 0.36, -0.7).normalize(),
        Vec3::new(0.30, 0.34, 0.46),
        Vec3::new(0.10, 0.12, 0.2),
        Vec3::new(0.06, 0.05, 0.04),
    )
    .with_haze(Vec3::new(0.03, 0.033, 0.06), 0.0008)
    .with_lamp(Lamp::new(layout::LANTERN, 6.0, palette::GLOW * 1.4))
    .with_lamp(Lamp::new(layout::GATE_LANTERN, 4.5, palette::GLOW * 1.1))
}

/// Where the eye is, from its view and projection: the point every ray of the view begins from.
pub fn eye(scene_to_clip: Mat4) -> Vec3 {
    let origin = scene_to_clip.inverse() * glam::Vec4::new(0.0, 0.0, 1.0, 0.0);
    if origin.w.abs() > f32::EPSILON {
        origin.truncate() / origin.w
    } else {
        Vec3::ZERO
    }
}

/// One model the farmyard draws, uploaded once, with its instances placed.
struct Drawn {
    model: GpuModel,
    batch: Batch,
}

impl Drawn {
    fn new(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        renderer: &Renderer,
        model: &Arc<Model>,
        placement: Mat4,
    ) -> Self {
        let gpu = renderer.upload(device, queue, model);
        let mut batch = renderer.batch(device);
        let mut instance = Instance::new(model.clone());
        instance.transform = placement;
        renderer.place(device, queue, &gpu, &mut batch, &[instance]);
        Self { model: gpu, batch }
    }
}

/// The farmyard's scenery.
pub struct Farmyard {
    sky: wgpu::RenderPipeline,
    cameras: [wgpu::Buffer; 2],
    bindings: [wgpu::BindGroup; 2],
    models: Renderer,
    yard: Drawn,
    coop: Drawn,
    /// The chickens, uploaded once their client side first shows them, placed as it last did.
    chickens: Option<Drawn>,
    reveal: Option<(Vec3, f32)>,
}

impl Farmyard {
    /// The farmyard for `target`, with `coop`, whose asset, if it has one, comes from `library`.
    pub fn new(target: &Target, coop: &Coop, library: &Library) -> Self {
        let device = target.device;
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("Farmyard sky layout"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            }],
        });
        let cameras: [wgpu::Buffer; 2] = std::array::from_fn(|_| {
            device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("Farmyard sky camera"),
                size: size_of::<SkyCamera>() as u64,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            })
        });
        let bindings = std::array::from_fn(|eye| {
            device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("Farmyard sky camera"),
                layout: &layout,
                entries: &[wgpu::BindGroupEntry {
                    binding: 0,
                    resource: cameras[eye].as_entire_binding(),
                }],
            })
        });
        let shader = device.create_shader_module(wgpu::include_wgsl!("sky.wgsl"));
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("Farmyard sky"),
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
        let sky = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("Farmyard sky"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("sky_vertex"),
                compilation_options: Default::default(),
                buffers: &[],
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("sky_fragment"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: target.format,
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: Default::default(),
            depth_stencil: Some(wgpu::DepthStencilState {
                format: wgpu::TextureFormat::Depth32Float,
                depth_write_enabled: Some(false),
                depth_compare: Some(wgpu::CompareFunction::LessEqual),
                stencil: Default::default(),
                bias: Default::default(),
            }),
            multisample: wgpu::MultisampleState {
                count: target.samples,
                ..Default::default()
            },
            multiview_mask: None,
            cache: None,
        });
        let models = Renderer::new(device, target.queue, target.format, target.samples);
        models.set_lighting(target.queue, &lighting());
        let yard = Drawn::new(
            device,
            target.queue,
            &models,
            &Arc::new(props::model()),
            Mat4::IDENTITY,
        );
        let coop = Drawn::new(
            device,
            target.queue,
            &models,
            &coop.model(library),
            coop::placement(),
        );
        Self {
            sky,
            cameras,
            bindings,
            models,
            yard,
            coop,
            chickens: None,
            reveal: None,
        }
    }
}

impl Scenery for Farmyard {
    fn animate(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        drawn: &(dyn std::any::Any + Send + Sync),
    ) {
        let Some(drawn) = drawn.downcast_ref::<crate::client::Drawn>() else {
            return;
        };
        let Some(first) = drawn.chickens.first() else {
            return;
        };
        let models = &self.models;
        let chickens = self.chickens.get_or_insert_with(|| Drawn {
            model: models.upload(device, queue, first.model()),
            batch: models.batch(device),
        });
        models.place(
            device,
            queue,
            &chickens.model,
            &mut chickens.batch,
            &drawn.chickens,
        );
    }

    fn prepare(&self, queue: &wgpu::Queue, eye: usize, scene_to_clip: Mat4) {
        self.models
            .prepare(queue, eye, scene_to_clip, self::eye(scene_to_clip));
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
        };
        queue.write_buffer(&self.cameras[eye], 0, bytemuck::bytes_of(&camera));
    }

    fn draw(&self, pass: &mut wgpu::RenderPass<'_>, eye: usize) {
        pass.set_pipeline(&self.sky);
        pass.set_bind_group(0, &self.bindings[eye], &[]);
        pass.draw(0..3, 0..1);
        for drawn in [Some(&self.yard), Some(&self.coop), self.chickens.as_ref()]
            .into_iter()
            .flatten()
        {
            self.models.draw(pass, eye, &drawn.model, &drawn.batch);
        }
    }

    fn set_reveal(&mut self, reveal: Option<(Vec3, f32)>) {
        self.reveal = reveal.filter(|(centre, radius)| centre.is_finite() && radius.is_finite());
        self.models.set_reveal(self.reveal);
    }
}

#[cfg(test)]
mod tests;
