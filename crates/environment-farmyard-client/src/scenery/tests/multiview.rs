// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

//! The farmyard drawn in one pass for both eyes, by multiview, as the headset draws it, draws what each eye's own pass
//! draws: its models and hens, ferns, water, sky, ranges, fire and plaques. See `tomori_render::multiview`.
use tomori_render::multiview;
use tomori_render::scenery::Stage;

use super::culling::{quest_eyes, viewpoints, with_hens_drawing};
use super::*;

/// What `scenery`, made for both views, draws through both eyes `clips` in one pass, `width` by `height` with `samples`
/// a pixel, each eye's layer read back as RGBA rows.
fn capture_both(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    scenery: &dyn Scenery,
    samples: u32,
    clips: [Mat4; 2],
    (width, height): (u32, u32),
) -> [Vec<u8>; 2] {
    let texture = |format, samples, usage| {
        device.create_texture(&wgpu::TextureDescriptor {
            label: Some("Farmyard multiview test target"),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 2,
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
    let layers = |texture: &wgpu::Texture| {
        texture.create_view(&wgpu::TextureViewDescriptor {
            dimension: Some(wgpu::TextureViewDimension::D2Array),
            ..Default::default()
        })
    };
    let (resolved_view, color_view, depth_view) =
        (layers(&resolved), layers(&color), layers(&depth));
    for (eye, clip) in clips.iter().enumerate() {
        scenery.prepare(queue, eye, *clip);
    }
    let mut encoder = device.create_command_encoder(&Default::default());
    for eye in 0..2 {
        scenery.encode(&mut encoder, eye);
    }
    {
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("Farmyard multiview test pass"),
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
            multiview_mask: multiview::Views::Both.mask(),
        });
        scenery.draw(&mut pass, 0);
        scenery.draw_translucent(&mut pass, 0);
    }
    queue.submit([encoder.finish()]);
    [0, 1].map(|layer| {
        tomori_render::test_gpu::read_layer(device, queue, &resolved, layer, (width, height))
            .into_raw()
    })
}

#[test]
fn the_farmyard_drawn_for_both_eyes_in_one_pass_is_what_each_eye_s_own_pass_draws() {
    test_gpu::with_multiview(|test_gpu::Gpu { device, queue, .. }| {
        let scope = device.push_error_scope(wgpu::ErrorFilter::Validation);
        let size = (256, 256);
        // Each with its hens, and a trail of light painted by a burning marshmallow glowing in the air.
        let mut each = with_hens_drawing(&device, &queue, size.0, size.1, multiview::Views::One);
        let mut both = with_hens_drawing(&device, &queue, size.0, size.1, multiview::Views::Both);
        assert_eq!(
            (each.views(), both.views()),
            (multiview::Views::One, multiview::Views::Both)
        );
        // Each culled in a render world of its own, as the renderer culls it, each piece and fern lit by the lamps the
        // world lists for it, so that both eyes are lit alike in one pass as in their own.
        let mut stages = [Stage::new(), Stage::new()];
        for (name, eyes, facing) in viewpoints() {
            let clips = quest_eyes(eyes, facing);
            for (farmyard, stage) in [&mut each, &mut both].into_iter().zip(&mut stages) {
                stage.cull(farmyard, &queue, &clips, true);
            }
            let alone = clips.map(|clip| capture_clip(&device, &queue, &each, 4, clip, size));
            let together = capture_both(&device, &queue, &both, 4, clips, size);
            for eye in 0..2 {
                let differing = alone[eye]
                    .chunks(4)
                    .zip(together[eye].chunks(4))
                    .filter(|(a, b)| a.iter().zip(*b).any(|(a, b)| a.abs_diff(*b) > 2))
                    .count();
                assert!(
                    differing * 1_000 <= (size.0 * size.1) as usize,
                    "{name}: eye {eye}, {differing} pixels differ"
                );
                assert!(
                    alone[eye]
                        .chunks(4)
                        .any(|pixel| pixel[..3] != alone[eye][..3])
                );
            }
        }
        assert!(pollster::block_on(scope.pop()).is_none());
    });
}
