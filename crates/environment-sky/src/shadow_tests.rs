// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

use super::*;

#[test]
fn moon_frustum_contains_all_static_casters_and_receivers() {
    let matrix = light_matrix();
    for vertex in crate::mesh::build() {
        let projected = matrix.project_point3(Vec3::from_array(vertex.position));
        assert!(projected.is_finite());
        assert!(projected.x.abs() < 1.0 && projected.y.abs() < 1.0);
        assert!((0.0..1.0).contains(&projected.z));
    }
}

#[test]
fn moon_shadow_depth_increases_away_from_the_light() {
    let matrix = light_matrix();
    let a = matrix.project_point3(Vec3::new(0.0, 0.0, -2.5));
    let b = matrix.project_point3(Vec3::new(0.0, 0.0, -2.5) + MOON_DIRECTION.normalize());
    assert!((a.x - b.x).abs() < 0.00001 && (a.y - b.y).abs() < 0.00001);
    assert!(b.z < a.z);
    assert_eq!(light_matrix(), matrix, "no camera or clock dependence");
}

#[test]
#[ignore = "requires a Vulkan adapter"]
fn cached_moon_shadows_occlude_scenery_preserve_emission_and_survive_resize() {
    pollster::block_on(async {
        const WIDTH: u32 = 512;
        const HEIGHT: u32 = 512;
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
            backends: wgpu::Backends::VULKAN,
            ..wgpu::InstanceDescriptor::new_without_display_handle()
        });
        let adapter = instance.request_adapter(&Default::default()).await.unwrap();
        let (device, queue) = adapter.request_device(&Default::default()).await.unwrap();
        let scope = device.push_error_scope(wgpu::ErrorFilter::Validation);
        let mut sky = crate::Sky::new(
            &device,
            &queue,
            wgpu::TextureFormat::Rgba8UnormSrgb,
            WIDTH,
            HEIGHT,
        );
        let target = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("Shadow regression color"),
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
        let depth = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("Shadow regression depth"),
            size: target.size(),
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Depth32Float,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            view_formats: &[],
        });
        let view = target.create_view(&Default::default());
        let depth_view = depth.create_view(&Default::default());
        let capture = |sky: &crate::Sky, matrix: Mat4| {
            sky.prepare_eye(&queue, 0, matrix);
            let mut encoder = device.create_command_encoder(&Default::default());
            {
                let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("Shadow regression scene"),
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view: &view,
                        resolve_target: None,
                        depth_slice: None,
                        ops: wgpu::Operations {
                            load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                            store: wgpu::StoreOp::Store,
                        },
                    })],
                    depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                        view: &depth_view,
                        depth_ops: Some(wgpu::Operations {
                            load: wgpu::LoadOp::Clear(1.0),
                            store: wgpu::StoreOp::Discard,
                        }),
                        stencil_ops: None,
                    }),
                    ..Default::default()
                });
                sky.draw(&mut pass, 0);
            }
            let buffer = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("Shadow regression readback"),
                size: u64::from(WIDTH * HEIGHT * 4),
                usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
                mapped_at_creation: false,
            });
            encoder.copy_texture_to_buffer(
                target.as_image_copy(),
                wgpu::TexelCopyBufferInfo {
                    buffer: &buffer,
                    layout: wgpu::TexelCopyBufferLayout {
                        offset: 0,
                        bytes_per_row: Some(WIDTH * 4),
                        rows_per_image: Some(HEIGHT),
                    },
                },
                target.size(),
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
            let pixels = buffer.slice(..).get_mapped_range().unwrap().to_vec();
            buffer.unmap();
            pixels
        };
        let projection = Mat4::orthographic_rh(-6.0, 6.0, -6.0, 6.0, 0.1, 30.0);
        let matrix = projection
            * Mat4::look_at_rh(
                Vec3::new(0.0, 12.0, -2.5),
                Vec3::new(0.0, 0.0, -2.5),
                -Vec3::Z,
            );
        sky.shadows.set_strength(&queue, 0.0);
        let unshadowed = capture(&sky, matrix);
        sky.shadows.set_strength(&queue, 1.0);
        let shadowed = capture(&sky, matrix);
        let mut darkened = 0;
        let mut unchanged = 0;
        for (before, after) in unshadowed
            .as_chunks::<4>()
            .0
            .iter()
            .zip(shadowed.as_chunks::<4>().0)
        {
            assert!(
                after[..3]
                    .iter()
                    .zip(&before[..3])
                    .all(|(a, b)| *a <= b.saturating_add(1))
            );
            if before[0] > after[0].saturating_add(5) {
                darkened += 1;
            }
            if before == after {
                unchanged += 1;
            }
        }
        assert!(
            darkened > 500,
            "actual cast shadows must darken part of the deck: {darkened}"
        );
        assert!(
            unchanged > (WIDTH * HEIGHT / 2) as usize,
            "not a global darkening filter"
        );
        sky.lightmap.set_enabled(&queue, false);
        let without_bake = capture(&sky, matrix);
        sky.lightmap.set_enabled(&queue, true);
        let restored = capture(&sky, matrix);
        assert_eq!(shadowed, restored);
        let mut warm_pool = 0;
        let mut ao_contact = 0;
        for (plain, baked) in without_bake
            .as_chunks::<4>()
            .0
            .iter()
            .zip(restored.as_chunks::<4>().0)
        {
            if baked[0] > plain[0].saturating_add(8)
                && baked[0].saturating_sub(plain[0]) > baked[2].saturating_sub(plain[2])
            {
                warm_pool += 1;
            }
            if plain[0] > baked[0].saturating_add(3) {
                ao_contact += 1;
            }
        }
        assert!(
            warm_pool > 100,
            "lantern lightmap must illuminate nearby geometry: {warm_pool}"
        );
        assert!(
            ao_contact > 20,
            "AO must darken actual contacts: {ao_contact}"
        );
        // Change camera and cloud-target size. Neither invalidates a static,
        // scene-space shadow map; returning to the same view is bit-stable.
        let side = Mat4::perspective_rh(1.1, 1.0, 0.05, 100.0)
            * Mat4::look_at_rh(Vec3::new(4.0, 3.0, 5.0), Vec3::new(0.0, 1.0, -2.5), Vec3::Y);
        // The top-down view sees lantern caps, not their emissive faces. Use a
        // second view to actually exercise emission rather than vacuously pass.
        sky.shadows.set_strength(&queue, 0.0);
        let side_unshadowed = capture(&sky, side);
        sky.shadows.set_strength(&queue, 1.0);
        let side_shadowed = capture(&sky, side);
        let mut emissive = 0;
        for (before, after) in side_unshadowed
            .as_chunks::<4>()
            .0
            .iter()
            .zip(side_shadowed.as_chunks::<4>().0)
        {
            if before[0] > 200 && before[1] > 100 && before[1] < 220 && before[2] < 150 {
                emissive += 1;
                assert_eq!(before, after, "lantern emission must not be shadowed");
            }
        }
        assert!(
            emissive > 10,
            "test must observe emissive lantern pixels: {emissive}"
        );
        sky.resize(&device, 640, 480);
        assert_eq!(shadowed, capture(&sky, matrix));
        if let Ok(path) = std::env::var("TOMORI_SHADOW_PREVIEW") {
            let mut sheet = image::RgbaImage::new(WIDTH * 2, HEIGHT);
            for (i, pixels) in [unshadowed, shadowed].into_iter().enumerate() {
                let image = image::RgbaImage::from_raw(WIDTH, HEIGHT, pixels).unwrap();
                image::imageops::replace(&mut sheet, &image, i as i64 * i64::from(WIDTH), 0);
            }
            sheet.save(path).unwrap();
        }
        assert!(scope.pop().await.is_none());
    });
}
