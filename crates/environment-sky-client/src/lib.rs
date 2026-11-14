// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

//! Sky's client half: a fixed moonlit cloudscape, independent of Tomori's sessions and simulation, and its registration as a scenery with a headset build, by the identity in `tomori-environment-sky`.
//! Geometry/noise textures are uploaded once; each eye ray-marches a bounded,
//! low-resolution cloud volume and composites it over the opaque scene.
#[cfg(test)]
mod cloud_noise;
mod clouds;
#[cfg(test)]
mod light_bake;
mod lightmap;
mod mesh;
mod scenery;
mod shadows;

pub use scenery::{register, scenery};
pub use tomori_environment_sky::{ID, LABEL, VERSION};

use bytemuck::{Pod, Zeroable};
use glam::{Mat4, Vec3};
use wgpu::util::DeviceExt;

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct Camera {
    view_projection: [f32; 16],
    inverse_view_projection: [f32; 16],
    /// The sphere Sky is drawn inside while it grows or shrinks: its centre, then its radius, which is negative when Sky fills the world.
    reveal: [f32; 4],
    /// Sky's haze, [`HAZE`]: its colour, then its density.
    haze: [f32; 4],
    /// Where the eye is, for the haze.
    eye: [f32; 4],
}

/// Sky's haze, a night blue over the distant hills and clouds, which everything drawn while Sky is shown is faded by.
pub const HAZE: tomori_render::haze::Haze =
    tomori_render::haze::Haze::new(Vec3::new(0.018, 0.027, 0.06), 0.00025);

/// GPU resources for Sky. The caller owns render targets, depth, transforms,
/// selection, and all interaction. No device-local clock drives this scenery.
pub struct Sky {
    background: wgpu::RenderPipeline,
    scenery: wgpu::RenderPipeline,
    /// The scenery, discarded outside the reveal sphere. Separate, so that the scenery keeps early depth testing whenever Sky fills the world.
    revealed_scenery: wgpu::RenderPipeline,
    reveal: Option<(Vec3, f32)>,
    /// Each eye's camera, and their binding for each pass.
    cameras: tomori_render::multiview::Uniforms,
    bindings: Vec<wgpu::BindGroup>,
    views: tomori_render::multiview::Views,
    vertices: wgpu::Buffer,
    vertex_count: u32,
    clouds: clouds::Clouds,
    shadows: shadows::Shadows,
    lightmap: lightmap::Lightmap,
    /// The box about the platform and shrine, the items they and the clouds are in the renderer's world once submitted,
    /// and whether the eyes may see the platform and shrine, as last culled.
    bounds: tomori_render::world::Aabb,
    items: Option<(tomori_render::world::ItemId, tomori_render::world::ItemId)>,
    shown: bool,
}

impl Sky {
    pub fn new(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        format: wgpu::TextureFormat,
        width: u32,
        height: u32,
    ) -> Self {
        Self::with_samples(device, queue, format, width, height, 1)
    }

    /// As [`Sky::new`], for a scene pass with `samples` samples per pixel, such as 4 for multisample anti-aliasing.
    pub fn with_samples(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        format: wgpu::TextureFormat,
        width: u32,
        height: u32,
        samples: u32,
    ) -> Self {
        Self::with_views(
            device,
            queue,
            format,
            width,
            height,
            samples,
            tomori_render::multiview::Views::One,
        )
    }

    /// As [`Sky::with_samples`], for scene passes drawing `views`: with both, both eyes in one pass, by multiview, and
    /// their clouds in one pass of their own. See `tomori_render::multiview`.
    pub fn with_views(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        format: wgpu::TextureFormat,
        width: u32,
        height: u32,
        samples: u32,
        views: tomori_render::multiview::Views,
    ) -> Self {
        use tomori_render::multiview::Uniforms;
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("Sky camera layout"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: Uniforms::binding_size(size_of::<Camera>() as u64, views),
                },
                count: None,
            }],
        });
        let cameras = Uniforms::new(device, "Sky eye camera", size_of::<Camera>() as u64, views);
        let bindings = cameras
            .buffers()
            .iter()
            .map(|buffer| {
                device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some("Sky eye binding"),
                    layout: &layout,
                    entries: &[wgpu::BindGroupEntry {
                        binding: 0,
                        resource: buffer.as_entire_binding(),
                    }],
                })
            })
            .collect();
        let mesh = mesh::build();
        let bounds = tomori_render::world::Aabb::from_points(
            mesh.iter().map(|vertex| Vec3::from_array(vertex.position)),
        );
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
        let shader =
            tomori_render::multiview::shader(device, "sky.wgsl", include_str!("sky.wgsl"), views);
        let attributes = wgpu::vertex_attr_array![0 => Float32x3, 1 => Float32x3, 2 => Float32x3, 3 => Float32, 4 => Float32x2];
        let vertex_layout = [Some(wgpu::VertexBufferLayout {
            array_stride: size_of::<mesh::Vertex>() as u64,
            step_mode: wgpu::VertexStepMode::Vertex,
            attributes: &attributes,
        })];
        let pipeline = |scenery, fragment: &str| {
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
                    entry_point: Some(fragment),
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
                multisample: wgpu::MultisampleState {
                    count: samples,
                    ..Default::default()
                },
                multiview_mask: views.mask(),
                cache: None,
            })
        };
        Self {
            background: pipeline(false, "sky_fragment"),
            scenery: pipeline(true, "scenery_fragment"),
            revealed_scenery: pipeline(true, "revealed_scenery_fragment"),
            reveal: None,
            cameras,
            bindings,
            views,
            vertices,
            vertex_count: mesh.len() as u32,
            clouds: clouds::Clouds::new(
                device, queue, format, &layout, width, height, samples, views,
            ),
            shadows,
            lightmap,
            bounds,
            items: None,
            shown: true,
        }
    }

    /// Draw Sky only inside a sphere, `(centre, radius)` in scene coordinates, as it grows out of a point or shrinks back into it; `None` fills the world. Applied by the next [`Sky::prepare_eye`]. Through the sphere, from outside it, the sky and clouds show as through a window; the platform and shrine show only where they are inside it.
    pub fn set_reveal(&mut self, reveal: Option<(Vec3, f32)>) {
        self.reveal = reveal.filter(|(centre, radius)| centre.is_finite() && radius.is_finite());
    }

    pub fn resize(&mut self, device: &wgpu::Device, width: u32, height: u32) {
        self.clouds.resize(device, width, height);
    }

    /// How many views each pass it draws in draws.
    pub fn views(&self) -> tomori_render::multiview::Views {
        self.views
    }

    /// Record the low-resolution density integration before beginning the main
    /// scene pass. Call after preparing this eye's camera. Drawing both eyes in one pass, eye 0's integrates both, in
    /// one pass of its own, and eye 1's nothing.
    pub fn encode_clouds(&self, encoder: &mut wgpu::CommandEncoder, eye: usize) {
        if let Some(camera) = self.bindings.get(eye) {
            self.clouds.encode(encoder, camera, eye);
        }
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
            reveal: match self.reveal {
                Some((centre, radius)) => [centre.x, centre.y, centre.z, radius.max(0.0)],
                None => [0.0, 0.0, 0.0, -1.0],
            },
            haze: HAZE.to_array(),
            eye: tomori_render::scenery::eye(view_projection)
                .extend(1.0)
                .to_array(),
        };
        self.cameras.write(queue, eye, bytemuck::bytes_of(&camera));
    }

    /// Draw before application content, into a pass with a cleared depth buffer.
    /// The distant sky never writes depth; platform and shrine do. Volumetric
    /// clouds are composited separately after the application's opaque content.
    pub fn draw(&self, pass: &mut wgpu::RenderPass<'_>, eye: usize) {
        pass.set_bind_group(0, &self.bindings[eye], &[]);
        // The platform and shrine, unless neither eye may see them.
        if self.shown {
            pass.set_pipeline(if self.reveal.is_some() {
                &self.revealed_scenery
            } else {
                &self.scenery
            });
            pass.set_bind_group(1, &self.shadows.binding, &[]);
            pass.set_bind_group(2, &self.lightmap.binding, &[]);
            pass.set_vertex_buffer(0, self.vertices.slice(..));
            pass.draw(0..self.vertex_count, 0..1);
        }
        // The sky after the platform and shrine, at the far plane, comparing `LessEqual` without writing depth, so that
        // it is shaded only where they do not cover it.
        pass.set_pipeline(&self.background);
        pass.draw(0..3, 0..1);
    }
}

#[cfg(test)]
mod tests {
    use tomori_render::test_gpu;

    #[test]
    fn every_shader_is_valid_wgsl() {
        let files = test_gpu::wgsl_files(concat!(env!("CARGO_MANIFEST_DIR"), "/src"));
        let names: Vec<_> = files.iter().map(|(name, _)| name.as_str()).collect();
        // A new shader is added here, with how it is compiled.
        assert_eq!(
            names,
            [
                "cloud_common.wgsl",
                "cloud_composite.wgsl",
                "cloud_volume.wgsl",
                "shadow.wgsl",
                "sky.wgsl"
            ]
        );
        for (name, source) in files {
            // The cloud passes are compiled after `cloud_common.wgsl`, which is also valid alone.
            let source = match name.as_str() {
                "cloud_volume.wgsl" | "cloud_composite.wgsl" => {
                    crate::clouds::shader_source(&source)
                }
                _ => source,
            };
            test_gpu::validate_wgsl(&name, &source);
        }
    }
}
