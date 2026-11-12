// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

use super::*;
use crate::scenery::{Sceneries, Scenery, Target};
use crate::test_gpu;
use glam::{Mat4, Vec3};

/// What `buffer`, `size` bytes, holds.
fn read_buffer(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    buffer: &wgpu::Buffer,
    size: u64,
) -> Vec<u8> {
    let readback = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("test uniform readback"),
        size,
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let mut encoder = device.create_command_encoder(&Default::default());
    encoder.copy_buffer_to_buffer(buffer, 0, &readback, 0, size);
    queue.submit([encoder.finish()]);
    readback.slice(..).map_async(wgpu::MapMode::Read, |_| {});
    device.poll(wgpu::PollType::wait_indefinitely()).unwrap();
    let data = readback.slice(..).get_mapped_range().unwrap().to_vec();
    readback.unmap();
    data
}

#[test]
fn each_eye_s_uniform_is_its_own_buffer_or_its_place_in_one_holding_both() {
    test_gpu::with(|test_gpu::Gpu { device, queue, .. }| {
        assert_eq!(
            Uniforms::binding_size(64, Views::One).map(|size| size.get()),
            Some(64)
        );
        assert_eq!(
            Uniforms::binding_size(64, Views::Both).map(|size| size.get()),
            Some(128)
        );
        let left = [1u8; 32];
        let right = [2u8; 32];
        // Each eye a buffer of its own, for a pass of its own.
        let readable = wgpu::BufferUsages::COPY_SRC;
        let one = Uniforms::with_usage(&device, "one", 32, Views::One, readable);
        assert_eq!((one.buffers().len(), one.views()), (2, Views::One));
        one.write(&queue, 0, &left);
        one.write(&queue, 1, &right);
        assert_eq!(read_buffer(&device, &queue, one.buffer(0), 32), left);
        assert_eq!(read_buffer(&device, &queue, one.buffer(1), 32), right);
        // Both in one, left then right, which the shaders index by their view.
        let both = Uniforms::with_usage(&device, "both", 32, Views::Both, readable);
        assert_eq!((both.buffers().len(), both.views()), (1, Views::Both));
        both.write(&queue, 1, &right);
        both.write(&queue, 0, &left);
        assert_eq!(
            read_buffer(&device, &queue, both.buffer(0), 64),
            [left, right].concat()
        );
    });
}

#[test]
#[should_panic(expected = "a multiple of 16 bytes")]
fn an_eye_s_uniform_is_a_multiple_of_16_bytes() {
    test_gpu::with(|test_gpu::Gpu { device, .. }| {
        Uniforms::new(&device, "odd", 24, Views::Both);
    })
    .unwrap_or_else(|| panic!("a multiple of 16 bytes: no adapter"));
}

#[test]
fn the_eyes_attachments_are_one_for_each_pass_of_as_many_layers_as_it_draws() {
    test_gpu::with(|test_gpu::Gpu { device, .. }| {
        let made = |views| {
            attachments(
                &device,
                "test",
                wgpu::TextureFormat::Depth32Float,
                (8, 4),
                1,
                wgpu::TextureUsages::RENDER_ATTACHMENT,
                views,
            )
        };
        let one = made(Views::One);
        assert_eq!(one.len(), 2);
        assert_eq!(one[0].texture().depth_or_array_layers(), 1);
        let both = made(Views::Both);
        assert_eq!(both.len(), 1);
        assert_eq!(both[0].texture().depth_or_array_layers(), 2);
        assert_eq!(
            (both[0].texture().width(), both[0].texture().height()),
            (8, 4)
        );
    });
}

#[test]
fn the_eyes_transient_attachments_drawn_at_once_are_one_texture_of_two_layers() {
    test_gpu::with_multiview(
        |test_gpu::Gpu {
             device, adapter, ..
         }| {
            // Transient, multisampled, as the headset's are: both layers in one transient texture.
            let transient = attachments(
                &device,
                "test",
                wgpu::TextureFormat::Rgba8UnormSrgb,
                (8, 4),
                4,
                wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TRANSIENT_ATTACHMENT,
                Views::Both,
            );
            assert_eq!(transient[0].texture().depth_or_array_layers(), 2);
            assert!(
                transient[0]
                    .texture()
                    .usage()
                    .contains(wgpu::TextureUsages::TRANSIENT_ATTACHMENT)
            );
            // Multiview is asked for, for two views where the adapter has it, and the device then has it.
            let limits = limits(&adapter, wgpu::Limits::default());
            assert_eq!(
                limits.max_multiview_view_count,
                adapter.limits().max_multiview_view_count.min(2)
            );
            assert_eq!(
                limits.max_bind_groups,
                wgpu::Limits::default().max_bind_groups
            );
            assert!(supported(&device, 4));
            assert!(supported(&device, 1));
            assert_eq!(usable(&device, Views::Both, 4), Views::Both);
            assert_eq!(usable(&device, Views::One, 4), Views::One);
        },
    );
}

#[test]
fn the_log_says_why_a_device_cannot_draw_both_eyes_at_once() {
    let multiview = wgpu::Features::MULTIVIEW;
    assert_eq!(missing(true, FEATURES, 2, 4), None);
    assert_eq!(missing(true, multiview, 2, 1), None);
    assert_eq!(
        missing(false, FEATURES, 2, 4),
        Some("the device is not Vulkan's")
    );
    assert_eq!(
        missing(true, wgpu::Features::MULTISAMPLE_ARRAY, 2, 4),
        Some("the device has no multiview")
    );
    assert_eq!(
        missing(true, FEATURES, 1, 4),
        Some("the device draws fewer than two views a pass")
    );
    assert_eq!(
        missing(true, multiview, 2, 4),
        Some("the device has no multisampled targets of two layers")
    );
}

/// A scenery that draws nothing, drawing the views its target asked for, or one, and counting what it was drawn as.
#[derive(Clone, Default)]
struct Viewed {
    drawn: std::sync::Arc<std::sync::Mutex<Vec<usize>>>,
    prepared: std::sync::Arc<std::sync::Mutex<Vec<usize>>>,
    views: Views,
}

impl Scenery for Viewed {
    fn prepare(&self, _queue: &wgpu::Queue, eye: usize, _scene_to_clip: Mat4) {
        self.prepared.lock().unwrap().push(eye);
    }

    fn draw(&self, _pass: &mut wgpu::RenderPass<'_>, eye: usize) {
        self.drawn.lock().unwrap().push(eye);
    }

    fn set_reveal(&mut self, _reveal: Option<(Vec3, f32)>) {}

    fn views(&self) -> Views {
        self.views
    }
}

/// Sceneries of `floor`, made for its target's views if `follows`, and otherwise for one, as every scenery made before
/// multiview was.
fn sceneries(floor: &Viewed, follows: bool) -> Sceneries {
    let mut sceneries = Sceneries::empty();
    let floor = floor.clone();
    sceneries.register("floor", move |target: &Target| {
        Box::new(Viewed {
            views: if follows { target.views } else { Views::One },
            ..floor.clone()
        }) as Box<dyn Scenery>
    });
    sceneries
}

#[test]
fn a_renderer_draws_both_eyes_at_once_only_where_every_scenery_does() {
    use crate::graphics::Renderer;
    test_gpu::with_multiview(|test_gpu::Gpu { device, queue, .. }| {
        let format = wgpu::TextureFormat::Rgba8UnormSrgb;
        let renderer = |sceneries: &Sceneries| {
            Renderer::with_views(&device, &queue, format, 8, 8, 1, sceneries, Views::Both)
        };
        // A scenery that draws one view, as every scenery did before multiview, has each eye drawn in its own pass.
        let plain = Viewed::default();
        let single = renderer(&sceneries(&plain, false));
        assert_eq!(single.views(), Views::One);
        // And the log says which.
        assert_eq!(
            single.single_view(),
            Some("the scenery of floor draws one view")
        );
        // One that draws the views its target asks for is drawn once, in the pass drawing both, prepared for each eye.
        let floor = Viewed::default();
        let mut both = renderer(&sceneries(&floor, true));
        assert_eq!((both.views(), both.single_view()), (Views::Both, None));
        both.set_environment(crate::environment::Environment::new("floor"));
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("multiview scenery eyes"),
            size: wgpu::Extent3d {
                width: 8,
                height: 8,
                depth_or_array_layers: 2,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor {
            dimension: Some(wgpu::TextureViewDimension::D2Array),
            ..Default::default()
        });
        let scope = device.push_error_scope(wgpu::ErrorFilter::Validation);
        both.render(
            &device,
            &queue,
            std::slice::from_ref(&view),
            &[Mat4::IDENTITY; 2],
            Mat4::IDENTITY,
            &[],
        );
        assert_eq!(*floor.prepared.lock().unwrap(), [0, 1]);
        assert_eq!(*floor.drawn.lock().unwrap(), [0]);
        // Its frames are timed as one pass, with no time for each eye, once their timestamps are back.
        for _ in 0..3 {
            device.poll(wgpu::PollType::wait_indefinitely()).unwrap();
            both.render(
                &device,
                &queue,
                std::slice::from_ref(&view),
                &[Mat4::IDENTITY; 2],
                Mat4::IDENTITY,
                &[],
            );
        }
        assert!(pollster::block_on(scope.pop()).is_none());
        let summary = both.gpu_summary().unwrap();
        assert!(
            summary.contains(" timed, ms mean/p95/max: eyes "),
            "{summary}"
        );
        assert!(!summary.contains("left"), "{summary}");
    });
}

#[test]
fn an_avatar_that_cannot_draw_the_renderer_s_views_gives_way_to_the_swarm() {
    use crate::graphics::Renderer;
    test_gpu::with_multiview(|test_gpu::Gpu { device, queue, .. }| {
        let mut sceneries = Sceneries::empty();
        sceneries.set_avatar(
            "fox",
            |_| Box::new(Still) as Box<dyn tomori_scene::avatar::Avatar>,
            |_| Box::new(Viewed::default()) as Box<dyn Scenery>,
        );
        let failures = sceneries.avatar_failures();
        let mut renderer = Renderer::with_views(
            &device,
            &queue,
            wgpu::TextureFormat::Rgba8UnormSrgb,
            8,
            8,
            1,
            &sceneries,
            Views::Both,
        );
        assert_eq!(renderer.views(), Views::Both);
        renderer.set_avatar(
            &device,
            &queue,
            Some(&tomori_scene::avatar::Shown {
                avatar: "fox",
                space: "space-1",
                drawn: None,
            }),
        );
        assert!(failures.contains("fox"));
    });
}

/// An avatar that stands still at the default place.
struct Still;

impl tomori_scene::avatar::Avatar for Still {
    fn update(&mut self, _input: &tomori_scene::avatar::Input) {}

    fn voices(&self, voices: &mut Vec<[f32; 3]>) {
        voices.push(tomori_scene::swarm::CENTER.to_array());
    }

    fn departures(&self, departures: &mut Vec<[f32; 3]>) {
        self.voices(departures);
    }

    fn bounds(&self) -> crate::arrival_motion::Sphere {
        crate::arrival_motion::Sphere {
            centre: tomori_scene::swarm::CENTER,
            radius: 0.5,
        }
    }
}
