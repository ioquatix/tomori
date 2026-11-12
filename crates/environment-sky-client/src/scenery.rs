// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

//! Sky as a scenery that `tomori-render` draws in each eye's pass, and its registration with a client's sceneries at Sky's identifier and version. See `design/environments.md`.
use glam::{Mat4, Vec3};
use tomori_render::scenery::{Sceneries, Scenery, Target, Views};
use tomori_render::world::{Item, Layer, Mobility, RootId, Tally, World};

use crate::Sky;

impl Scenery for Sky {
    /// Its platform and shrine, one static item, with the box about them, and its clouds, ray-marched in passes of their
    /// own, an item in the sky's layer that is never culled.
    fn submit(&mut self, world: &mut World, root: RootId) {
        if self
            .items
            .is_some_and(|(platform, _)| world.contains(platform))
        {
            return;
        }
        let platform = world.add(Item::new(root, Layer::Opaque, Mobility::Static));
        world.place(platform, [Some(self.bounds)]);
        let clouds = world.add(Item::new(root, Layer::Sky, Mobility::Static).unculled());
        world.place(clouds, [None]);
        self.items = Some((platform, clouds));
    }

    /// Draw the platform and shrine only if the eyes may see them, as the world culled them, or, without a world, against
    /// the eyes' frustum.
    fn cull(&mut self, _queue: &wgpu::Queue, views: &Views<'_>) -> Tally {
        self.shown = match self.items.and_then(|(platform, _)| views.seen(platform)) {
            Some(seen) => seen.iter().any(|seen| *seen),
            None => views.sees(&self.bounds),
        };
        let shown = usize::from(self.shown);
        Tally {
            drawn: shown,
            culled: 1 - shown,
            draws: shown,
            triangles: shown * self.vertex_count as usize / 3,
        }
    }

    fn prepare(&self, queue: &wgpu::Queue, eye: usize, scene_to_clip: Mat4) {
        self.prepare_eye(queue, eye, scene_to_clip);
    }

    fn encode(&self, encoder: &mut wgpu::CommandEncoder, eye: usize) {
        self.encode_clouds(encoder, eye);
    }

    fn draw(&self, pass: &mut wgpu::RenderPass<'_>, eye: usize) {
        Sky::draw(self, pass, eye);
    }

    fn draw_translucent(&self, pass: &mut wgpu::RenderPass<'_>, eye: usize) {
        self.draw_clouds(pass, eye);
    }

    fn set_reveal(&mut self, reveal: Option<(Vec3, f32)>) {
        Sky::set_reveal(self, reveal);
    }

    fn resize(&mut self, device: &wgpu::Device, width: u32, height: u32) {
        Sky::resize(self, device, width, height);
    }

    fn views(&self) -> tomori_render::multiview::Views {
        Sky::views(self)
    }

    fn haze(&self) -> tomori_render::haze::Haze {
        crate::HAZE
    }
}

/// Tomori's moonlit sky, for `target`.
pub fn scenery(target: &Target) -> Box<dyn Scenery> {
    Box::new(Sky::with_views(
        target.device,
        target.queue,
        target.format,
        target.width,
        target.height,
        target.samples,
        target.views,
    ))
}

/// Draw Sky for spaces bound to it, at [`crate::ID`] and [`crate::VERSION`], which a server registering Sky by the same identity offers it at, in place of any scenery registered for it before. It declares no locomotion.
///
/// ```
/// let mut sceneries = tomori_render::scenery::Sceneries::empty();
/// tomori_environment_sky_client::register(&mut sceneries);
/// assert!(sceneries.contains(tomori_environment_sky_client::ID));
/// ```
pub fn register(sceneries: &mut Sceneries) {
    sceneries.register_versioned(crate::ID, crate::VERSION, scenery);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_haze_is_the_one_sky_s_scenery_and_clouds_are_hazed_by() {
        // Each is hazed by the camera's haze, which is Sky's, `HAZE`, with the shared haze's function.
        for shader in [include_str!("sky.wgsl"), include_str!("cloud_volume.wgsl")] {
            assert!(shader.contains("hazed(") && shader.contains("camera.haze"));
        }
    }

    #[test]
    fn sky_is_registered_at_its_identity_and_declares_no_locomotion() {
        let mut sceneries = Sceneries::empty();
        register(&mut sceneries);
        let environments: Vec<(&str, u32)> = sceneries
            .environments()
            .into_iter()
            .map(|environment| (environment.id(), environment.version()))
            .collect();
        assert_eq!(
            environments,
            [
                (tomori_protocol::GRID, tomori_protocol::GRID_VERSION),
                ("sky", 1)
            ]
        );
        assert!(sceneries.locomotion().is_empty());
    }

    #[test]
    fn a_build_may_declare_teleport_pads_beside_sky() {
        use tomori_scene::locomotion::{Locomotion, Pad, Surface};
        // A floor, racks 4.5 m below it, and a pair of pads between them, declared in Sky's own coordinates, as a build
        // of another project declares them for its own environment.
        let floors = Locomotion::fly(
            vec![
                Surface::rectangle(Vec3::ZERO, glam::Vec2::new(12.0, 12.0)),
                Surface::rectangle(Vec3::new(0.0, -4.5, 0.0), glam::Vec2::new(12.0, 12.0)),
            ],
            6.0,
        )
        .with_pads([[
            Pad::at(Vec3::new(2.0, 0.0, -3.0)).named("Main floor"),
            Pad::at(Vec3::new(2.0, -4.5, -3.0))
                .named("Racks")
                .facing(Vec3::NEG_Z),
        ]]);
        let mut sceneries = Sceneries::empty();
        register(&mut sceneries);
        sceneries.set_locomotion(crate::ID, floors.clone());
        let declared = sceneries.locomotion();
        assert_eq!(declared.len(), 1);
        assert_eq!(declared[0].0.id(), crate::ID);
        let [floor, racks] = declared[0].1.pads().next().expect("the pair is used");
        assert_eq!(
            (floor.name(), racks.name()),
            (Some("Main floor"), Some("Racks"))
        );
        assert_eq!(declared[0].1, floors);
    }

    #[test]
    fn sky_s_platform_is_drawn_only_where_the_eyes_may_see_it() {
        tomori_render::test_gpu::with(|gpu| {
            let mut sky = Sky::new(
                &gpu.device,
                &gpu.queue,
                wgpu::TextureFormat::Rgba8UnormSrgb,
                16,
                16,
            );
            let projection = glam::camera::rh::proj::directx::perspective(1.4, 1.0, 0.05, 2_000.0);
            let looking = |from: Vec3, to: Vec3| {
                projection * glam::camera::rh::view::look_at_mat4(from, to, Vec3::Y)
            };
            let towards = [looking(Vec3::new(0.0, 1.6, 6.0), Vec3::ZERO)];
            let away = [looking(
                Vec3::new(0.0, 1.6, 200.0),
                Vec3::new(0.0, 1.6, 400.0),
            )];
            let mut stage = tomori_render::scenery::Stage::new();
            let seen = stage.cull(&mut sky, &gpu.queue, &towards, true);
            assert_eq!((seen.drawn, seen.culled, seen.draws), (1, 0, 1));
            assert_eq!(seen.triangles, sky.vertex_count as usize / 3);
            // Looking away, it is culled, and the clouds, which are never culled, are still drawn.
            let culled = stage.cull(&mut sky, &gpu.queue, &away, true);
            assert_eq!(
                (culled.drawn, culled.culled, culled.draws, culled.triangles),
                (0, 1, 0, 0)
            );
            assert!(!sky.shown);
            // It is drawn so, the sky without the platform.
            let texture = |format, usage| {
                gpu.device
                    .create_texture(&wgpu::TextureDescriptor {
                        label: Some("Sky culled eye"),
                        size: wgpu::Extent3d {
                            width: 16,
                            height: 16,
                            depth_or_array_layers: 1,
                        },
                        mip_level_count: 1,
                        sample_count: 1,
                        dimension: wgpu::TextureDimension::D2,
                        format,
                        usage,
                        view_formats: &[],
                    })
                    .create_view(&Default::default())
            };
            let color = texture(
                wgpu::TextureFormat::Rgba8UnormSrgb,
                wgpu::TextureUsages::RENDER_ATTACHMENT,
            );
            let depth = texture(
                wgpu::TextureFormat::Depth32Float,
                wgpu::TextureUsages::RENDER_ATTACHMENT,
            );
            Scenery::prepare(&sky, &gpu.queue, 0, away[0]);
            let mut encoder = gpu
                .device
                .create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
            {
                let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("Sky culled eye"),
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view: &color,
                        resolve_target: None,
                        depth_slice: None,
                        ops: wgpu::Operations {
                            load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                            store: wgpu::StoreOp::Store,
                        },
                    })],
                    depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                        view: &depth,
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
                Scenery::draw(&sky, &mut pass, 0);
            }
            gpu.queue.submit([encoder.finish()]);
            let (world, root) = stage.world();
            assert_eq!(world.tally(root).drawn, 1);
            // Unculled it is drawn, and given views without a world it culls itself alike.
            assert_eq!(stage.cull(&mut sky, &gpu.queue, &away, false), seen);
            assert_eq!(sky.cull(&gpu.queue, &Views::new(&away)), culled);
            assert_eq!(sky.cull(&gpu.queue, &Views::new(&towards)), seen);
        });
    }

    #[test]
    fn sky_is_drawn_by_a_renderer_given_it() {
        tomori_render::test_gpu::with(|gpu| {
            let mut sceneries = Sceneries::empty();
            register(&mut sceneries);
            let created = sceneries.create(&Target::new(
                &gpu.device,
                &gpu.queue,
                wgpu::TextureFormat::Rgba8UnormSrgb,
                32,
                32,
                1,
                tomori_render::glyphs::SharedGlyphs::new(&gpu.device),
            ));
            assert_eq!(created.len(), 1);
            let (environment, mut sky) = created.into_iter().next().unwrap();
            assert_eq!(environment.id(), crate::ID);
            assert_eq!(sky.haze(), crate::HAZE);
            sky.set_reveal(Some((Vec3::ZERO, 1.0)));
            sky.resize(&gpu.device, 16, 16);
            sky.prepare(&gpu.queue, 0, Mat4::IDENTITY);
            let mut encoder = gpu
                .device
                .create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
            sky.encode(&mut encoder, 0);
            gpu.queue.submit([encoder.finish()]);
        });
    }

    /// Each eye's picture of Sky, its platform, shrine, sky and clouds, as a renderer of `views` draws them with 4
    /// samples a pixel, from the platform looking out over the clouds.
    fn drawn(
        gpu: &tomori_render::test_gpu::Gpu,
        views: tomori_render::multiview::Views,
    ) -> Vec<image::RgbaImage> {
        use tomori_render::graphics::Renderer;
        const SIZE: u32 = 128;
        let format = wgpu::TextureFormat::Rgba8UnormSrgb;
        let mut sceneries = Sceneries::empty();
        register(&mut sceneries);
        let mut renderer = Renderer::with_views(
            &gpu.device,
            &gpu.queue,
            format,
            SIZE,
            SIZE,
            4,
            &sceneries,
            views,
        );
        assert_eq!(renderer.views(), views);
        renderer.set_scenery(tomori_scene::arrival_motion::Scenery {
            environment: tomori_scene::environment::Environment::new(crate::ID)
                .with_version(crate::VERSION),
            reveal: None,
        });
        let layers = views.count() as u32;
        let textures: Vec<_> = (0..views.passes(2))
            .map(|_| {
                gpu.device.create_texture(&wgpu::TextureDescriptor {
                    label: Some("Sky multiview test eyes"),
                    size: wgpu::Extent3d {
                        width: SIZE,
                        height: SIZE,
                        depth_or_array_layers: layers,
                    },
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    format,
                    usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
                    view_formats: &[],
                })
            })
            .collect();
        let targets: Vec<_> = textures
            .iter()
            .map(|texture| {
                texture.create_view(&wgpu::TextureViewDescriptor {
                    dimension: Some(if layers == 2 {
                        wgpu::TextureViewDimension::D2Array
                    } else {
                        wgpu::TextureViewDimension::D2
                    }),
                    ..Default::default()
                })
            })
            .collect();
        let lens = glam::camera::rh::proj::directx::perspective(1.6, 1.0, 0.05, 2000.0);
        let eyes = [-0.032, 0.032].map(|x| {
            let eye = Vec3::new(x, 1.6, 0.0);
            lens * glam::camera::rh::view::look_at_mat4(
                eye,
                eye + Vec3::new(0.3, -0.4, -1.0),
                Vec3::Y,
            )
        });
        renderer.render(
            &gpu.device,
            &gpu.queue,
            &targets,
            &eyes,
            Mat4::IDENTITY,
            &[],
        );
        (0..2)
            .map(|eye| {
                let (texture, layer) = match views {
                    tomori_render::multiview::Views::One => (&textures[eye], 0),
                    tomori_render::multiview::Views::Both => (&textures[0], eye as u32),
                };
                tomori_render::test_gpu::read_layer(
                    &gpu.device,
                    &gpu.queue,
                    texture,
                    layer,
                    (SIZE, SIZE),
                )
            })
            .collect()
    }

    #[test]
    fn sky_drawn_for_both_eyes_in_one_pass_is_what_each_eye_s_own_pass_draws() {
        use tomori_render::multiview::Views;
        tomori_render::test_gpu::with_multiview(|gpu| {
            let scope = gpu.device.push_error_scope(wgpu::ErrorFilter::Validation);
            let each = drawn(&gpu, Views::One);
            let both = drawn(&gpu, Views::Both);
            assert_ne!(each[0], each[1]);
            for eye in 0..2 {
                let differing = each[eye]
                    .pixels()
                    .zip(both[eye].pixels())
                    .filter(|(a, b)| a.0.iter().zip(b.0).any(|(a, b)| a.abs_diff(b) > 2))
                    .count();
                assert!(
                    differing * 1_000 <= 128 * 128,
                    "eye {eye}: {differing} pixels differ"
                );
            }
            assert!(pollster::block_on(scope.pop()).is_none());
        });
    }
}
