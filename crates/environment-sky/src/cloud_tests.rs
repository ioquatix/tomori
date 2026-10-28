use super::*;
use glam::{Mat4, Vec3};

#[test]
fn cloud_target_size_is_bounded_for_headsets_spectators_and_tiny_windows() {
    assert_eq!(resolution(1680, 1760), (840, 880));
    assert_eq!(resolution(3840, 2160), (960, 540));
    assert_eq!(resolution(0, 0), (1, 1));
    assert_eq!(resolution(1, 1), (1, 1));
}

fn read_image(device: &wgpu::Device, queue: &wgpu::Queue, texture: &wgpu::Texture) -> Vec<u8> {
    let width = texture.width();
    let height = texture.height();
    let stride = (width * 4).div_ceil(256) * 256;
    let buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("Cloud test readback"),
        size: u64::from(stride * height),
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
                bytes_per_row: Some(stride),
                rows_per_image: Some(height),
            },
        },
        wgpu::Extent3d {
            width,
            height,
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
    let data = buffer.slice(..).get_mapped_range().unwrap();
    let pixels = data
        .chunks(stride as usize)
        .flat_map(|row| &row[..(width * 4) as usize])
        .copied()
        .collect();
    drop(data);
    buffer.unmap();
    pixels
}

#[test]
#[ignore = "requires a Vulkan adapter"]
fn clouds_are_translucent_stereo_stable_and_occluded_by_foreground_depth() {
    pollster::block_on(async {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
            backends: wgpu::Backends::VULKAN,
            ..wgpu::InstanceDescriptor::new_without_display_handle()
        });
        let adapter = instance.request_adapter(&Default::default()).await.unwrap();
        let (device, queue) = adapter.request_device(&Default::default()).await.unwrap();
        let scope = device.push_error_scope(wgpu::ErrorFilter::Validation);
        let mut sky = crate::Sky::new(&device, &queue, wgpu::TextureFormat::Rgba8Unorm, 512, 384);
        let matrices = [-0.032, 0.032].map(|x| {
            let eye = Vec3::new(x, 1.6, 0.0);
            Mat4::perspective_rh(1.1, 512.0 / 384.0, 0.05, 100.0)
                * Mat4::look_at_rh(eye, eye + Vec3::new(0.0, -0.3, -1.0), Vec3::Y)
        });
        let mut encoder = device.create_command_encoder(&Default::default());
        for (eye, matrix) in matrices.into_iter().enumerate() {
            sky.prepare_eye(&queue, eye, matrix);
            sky.encode_clouds(&mut encoder, eye);
        }
        queue.submit([encoder.finish()]);
        let pixels = read_image(&device, &queue, &sky.clouds.targets[0]._texture);
        let transparent = pixels
            .as_chunks::<4>()
            .0
            .iter()
            .filter(|p| p[3] == 0)
            .count();
        let translucent = pixels
            .as_chunks::<4>()
            .0
            .iter()
            .filter(|p| p[3] > 16 && p[3] < 240)
            .count();
        assert!(transparent > 100, "empty sky must stay transparent");
        assert!(
            translucent > 1000,
            "cloud edges/body must have real partial alpha: {translucent}"
        );
        assert!(
            pixels
                .as_chunks::<4>()
                .0
                .iter()
                .all(|p| p[..3].iter().all(|c| *c <= p[3]))
        );
        assert_ne!(
            pixels,
            read_image(&device, &queue, &sky.clouds.targets[1]._texture),
            "world-space volume needs stereo parallax"
        );
        let mut encoder = device.create_command_encoder(&Default::default());
        sky.encode_clouds(&mut encoder, 0);
        queue.submit([encoder.finish()]);
        assert_eq!(
            pixels,
            read_image(&device, &queue, &sky.clouds.targets[0]._texture),
            "no temporal jitter"
        );

        let color = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("Cloud foreground test"),
            size: wgpu::Extent3d {
                width: 512,
                height: 384,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let depth = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("Cloud test depth"),
            size: color.size(),
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Depth32Float,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            view_formats: &[],
        });
        let view = color.create_view(&Default::default());
        let depth_view = depth.create_view(&Default::default());
        for depth_value in [0.0, 1.0] {
            let mut encoder = device.create_command_encoder(&Default::default());
            {
                let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("Cloud opaque depth test"),
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view: &view,
                        resolve_target: None,
                        depth_slice: None,
                        ops: wgpu::Operations {
                            load: wgpu::LoadOp::Clear(wgpu::Color::GREEN),
                            store: wgpu::StoreOp::Store,
                        },
                    })],
                    depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                        view: &depth_view,
                        depth_ops: Some(wgpu::Operations {
                            load: wgpu::LoadOp::Clear(depth_value),
                            store: wgpu::StoreOp::Discard,
                        }),
                        stencil_ops: None,
                    }),
                    ..Default::default()
                });
                sky.draw_clouds(&mut pass, 0);
            }
            queue.submit([encoder.finish()]);
            let result = read_image(&device, &queue, &color);
            let changed = result
                .as_chunks::<4>()
                .0
                .iter()
                .filter(|p| **p != [0, 255, 0, 255])
                .count();
            if depth_value == 0.0 {
                assert_eq!(changed, 0, "foreground must occlude clouds");
            } else {
                assert!(
                    changed > 1000,
                    "clouds must blend over unoccluded background"
                );
            }
        }
        sky.resize(&device, 1, 1);
        assert_eq!(
            sky.clouds.targets[0]._texture.size(),
            wgpu::Extent3d {
                width: 1,
                height: 1,
                depth_or_array_layers: 1
            }
        );
        assert!(scope.pop().await.is_none());
    });
}
