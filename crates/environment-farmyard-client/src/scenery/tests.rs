// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

//! The farmyard drawn on the shared test device, on lavapipe in CI: from home, looking into the yard at the coop, as a
//! participant arriving sees it, and growing out of a point. `TOMORI_FARMYARD_PREVIEW`, set to a PNG path, saves the
//! view from home and from beside the coop's window.
use glam::{Mat4, Vec3};
use tomori_render::test_gpu;

use super::*;

const WIDTH: u32 = 320;
const HEIGHT: u32 = 200;

/// What `scenery` draws for an eye at `eye` looking at `target`, read back as RGBA rows.
pub(crate) fn capture(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    scenery: &dyn Scenery,
    samples: u32,
    (eye, target): (Vec3, Vec3),
) -> Vec<u8> {
    let view_projection = Mat4::perspective_rh(1.2, WIDTH as f32 / HEIGHT as f32, 0.05, 200.0)
        * Mat4::look_at_rh(eye, target, Vec3::Y);
    let texture = |format, samples, usage| {
        device.create_texture(&wgpu::TextureDescriptor {
            label: Some("Farmyard test target"),
            size: wgpu::Extent3d {
                width: WIDTH,
                height: HEIGHT,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: samples,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage,
            view_formats: &[],
        })
    };
    let resolved = texture(
        wgpu::TextureFormat::Rgba8UnormSrgb,
        1,
        wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
    );
    let color = texture(
        wgpu::TextureFormat::Rgba8UnormSrgb,
        samples,
        wgpu::TextureUsages::RENDER_ATTACHMENT,
    );
    let depth = texture(
        wgpu::TextureFormat::Depth32Float,
        samples,
        wgpu::TextureUsages::RENDER_ATTACHMENT,
    );
    let (resolved_view, color_view, depth_view) = (
        resolved.create_view(&Default::default()),
        color.create_view(&Default::default()),
        depth.create_view(&Default::default()),
    );
    scenery.prepare(queue, 0, view_projection);
    let mut encoder = device.create_command_encoder(&Default::default());
    scenery.encode(&mut encoder, 0);
    {
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("Farmyard test pass"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: if samples > 1 {
                    &color_view
                } else {
                    &resolved_view
                },
                depth_slice: None,
                resolve_target: (samples > 1).then_some(&resolved_view),
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
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        scenery.draw(&mut pass, 0);
        scenery.draw_translucent(&mut pass, 0);
    }
    let stride = (WIDTH * 4).div_ceil(256) * 256;
    let buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("Farmyard test readback"),
        size: u64::from(stride * HEIGHT),
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    encoder.copy_texture_to_buffer(
        resolved.as_image_copy(),
        wgpu::TexelCopyBufferInfo {
            buffer: &buffer,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(stride),
                rows_per_image: Some(HEIGHT),
            },
        },
        wgpu::Extent3d {
            width: WIDTH,
            height: HEIGHT,
            depth_or_array_layers: 1,
        },
    );
    queue.submit([encoder.finish()]);
    let (sender, receiver) = std::sync::mpsc::channel();
    buffer
        .slice(..)
        .map_async(wgpu::MapMode::Read, move |result| {
            sender.send(result).unwrap();
        });
    device.poll(wgpu::PollType::wait_indefinitely()).unwrap();
    receiver.recv().unwrap().unwrap();
    let data = buffer.slice(..).get_mapped_range().unwrap();
    let pixels = data
        .chunks(stride as usize)
        .flat_map(|row| &row[..(WIDTH * 4) as usize])
        .copied()
        .collect();
    drop(data);
    buffer.unmap();
    pixels
}

/// The average colour of the pixels in the box from `low` to `high`, fractions of the view across and down.
fn average(pixels: &[u8], low: (f32, f32), high: (f32, f32)) -> [f32; 3] {
    let mut sum = [0.0; 3];
    let mut count = 0.0;
    for y in (low.1 * HEIGHT as f32) as u32..(high.1 * HEIGHT as f32) as u32 {
        for x in (low.0 * WIDTH as f32) as u32..(high.0 * WIDTH as f32) as u32 {
            let at = ((y * WIDTH + x) * 4) as usize;
            for channel in 0..3 {
                sum[channel] += f32::from(pixels[at + channel]);
            }
            count += 1.0;
        }
    }
    sum.map(|value| value / count)
}

fn target(device: &wgpu::Device, queue: &wgpu::Queue, samples: u32) -> Farmyard {
    Farmyard::new(
        &Target {
            device,
            queue,
            format: wgpu::TextureFormat::Rgba8UnormSrgb,
            width: WIDTH,
            height: HEIGHT,
            samples,
        },
        &Coop::Built,
        Library::shared(),
    )
}

const HOME: (Vec3, Vec3) = (Vec3::new(0.0, 1.6, 0.0), Vec3::new(-1.2, 0.8, -8.0));
const WINDOW: (Vec3, Vec3) = (Vec3::new(-0.9, 1.25, -8.6), Vec3::new(-2.8, 0.85, -8.6));

#[test]
fn every_shader_is_valid_wgsl() {
    let files = test_gpu::wgsl_files(concat!(env!("CARGO_MANIFEST_DIR"), "/src"));
    let names: Vec<_> = files.iter().map(|(name, _)| name.as_str()).collect();
    assert_eq!(names, ["sky.wgsl"]);
    for (name, source) in files {
        test_gpu::validate_wgsl(&name, &source);
    }
}

#[test]
fn the_eye_is_where_every_ray_of_its_view_begins() {
    let view = Mat4::perspective_rh(1.0, 1.0, 0.1, 10.0)
        * Mat4::look_at_rh(Vec3::new(1.0, 2.0, 3.0), Vec3::ZERO, Vec3::Y);
    assert!(eye(view).distance(Vec3::new(1.0, 2.0, 3.0)) < 1e-3);
    // A view without a point of view, such as an orthographic one, is taken as from the origin.
    assert_eq!(eye(Mat4::IDENTITY), Vec3::ZERO);
}

#[test]
fn the_farmyard_is_an_evening_yard_with_a_warmly_lit_red_coop() {
    test_gpu::with(|test_gpu::Gpu { device, queue, .. }| {
        let scope = device.push_error_scope(wgpu::ErrorFilter::Validation);
        let mut farmyard = target(&device, &queue, 4);
        let home = capture(&device, &queue, &farmyard, 4, HOME);
        // The sky is darker overhead than at the horizon, the ground below is the yard's, and the coop, ahead and left of
        // the gate, is red, lit warmly by its lantern.
        let overhead = average(&home, (0.0, 0.0), (1.0, 0.08));
        let ground = average(&home, (0.3, 0.9), (0.7, 1.0));
        assert!(
            overhead.iter().sum::<f32>() < ground.iter().sum::<f32>(),
            "{overhead:?} {ground:?}"
        );
        let coop = average(&home, (0.35, 0.3), (0.5, 0.55));
        assert!(coop[0] > coop[2] * 1.2, "the coop is red: {coop:?}");
        // Seen from beside its window, its inside is darker than its sunlit, lantern-lit walls.
        let window = capture(&device, &queue, &farmyard, 4, WINDOW);
        if let Ok(path) = std::env::var("TOMORI_FARMYARD_PREVIEW") {
            let mut sheet = image::RgbaImage::new(WIDTH, HEIGHT * 2);
            for (row, pixels) in [&home, &window].into_iter().enumerate() {
                let view = image::RgbaImage::from_raw(WIDTH, HEIGHT, pixels.clone()).unwrap();
                image::imageops::replace(&mut sheet, &view, 0, i64::from(row as u32 * HEIGHT));
            }
            sheet.save(path).unwrap();
        }
        // Growing out of a point in front of home, only what is near it is drawn, and the rest is left as it was.
        farmyard.set_reveal(Some((Vec3::new(0.0, 1.0, -2.0), 0.5)));
        let growing = capture(&device, &queue, &farmyard, 4, HOME);
        let black = growing
            .chunks(4)
            .filter(|pixel| pixel[..3] == [0, 0, 0])
            .count();
        assert!(black > (WIDTH * HEIGHT) as usize / 2, "{black}");
        farmyard.set_reveal(Some((Vec3::NAN, 1.0)));
        assert!(farmyard.reveal.is_none());
        assert_eq!(capture(&device, &queue, &farmyard, 4, HOME), home);
        // A view that cannot be inverted draws the sky as if from the origin, rather than failing.
        farmyard.prepare(&queue, 1, Mat4::ZERO);
        // Registered, the build's sceneries make it for their targets.
        let mut sceneries = tomori_render::scenery::Sceneries::empty();
        crate::register(&mut sceneries);
        let made = sceneries.create(&Target {
            device: &device,
            queue: &queue,
            format: wgpu::TextureFormat::Rgba8UnormSrgb,
            width: WIDTH,
            height: HEIGHT,
            samples: 4,
        });
        assert_eq!(made.len(), 1);
        assert_eq!(capture(&device, &queue, made[0].1.as_ref(), 4, HOME), home);
        assert!(pollster::block_on(scope.pop()).is_none());
    });
}
