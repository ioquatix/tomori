// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

//! Both eyes drawn in one pass, by multiview, draw what each eye's own pass draws: the swarm, workers and their signs,
//! the Tomori wall and a floating panel, the hand menu and the pill. See `design/rendering.md`.
use super::*;
use crate::multiview::Views;
use tomori_protocol::{Worker, WorkerState};

const WIDTH: u32 = 256;
const HEIGHT: u32 = 256;

/// A space with six workers, the Tomori wall with a panel in a slot and a panel floating ahead.
fn conversation() -> tomori_scene::conversation::Conversation {
    let mut conversation = joined();
    conversation.apply(space(|state| {
        state.workers = (0..6)
            .map(|index| Worker {
                id: format!("ses_{index}"),
                parent: None,
                title: ["Review the pull request", "Run the tests"][index % 2].into(),
                state: [WorkerState::Running, WorkerState::Waiting][index % 2],
            })
            .collect();
    }));
    let ahead = arrival::HOME + Vec3::new(0.3, 0.0, -1.4);
    show_projection(
        &mut conversation,
        Vec::new(),
        vec![
            tomori_protocol::Panel::new(
                "panel-1",
                "Steps to release",
                "1. Bump the version.\n2. Run the tests.\n3. Tag and push.",
                tomori_protocol::Place::Free {
                    pose: tomori_protocol::Pose::upright_facing(
                        ahead.to_array(),
                        arrival::HOME.to_array(),
                    ),
                },
            ),
            tomori_protocol::Panel::new(
                "panel-2",
                "Open PRs",
                "| # | What |\n|---|---|\n| 1 | Multiview |",
                tomori_protocol::Place::Wall {
                    wall: tomori_protocol::TOMORI_WALL.into(),
                    slot: 2,
                },
            ),
        ],
    );
    conversation
}

/// A Quest-like pair of eyes, 64 mm apart, at home, looking a little down towards the agent's home.
fn eyes() -> [Mat4; 2] {
    let lens = glam::camera::rh::proj::directx::perspective(1.6, 1.0, 0.05, 50.0);
    [-0.032, 0.032].map(|x| {
        let eye = arrival::HOME + Vec3::X * x;
        lens * glam::camera::rh::view::look_at_mat4(eye, eye + Vec3::new(0.0, -0.35, -1.0), Vec3::Y)
    })
}

/// Each eye's image of the frame, drawn by a renderer of `views` with `samples` a pixel, and that renderer's views.
fn drawn(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    views: Views,
    samples: u32,
) -> (Vec<image::RgbaImage>, Views) {
    let format = wgpu::TextureFormat::Rgba8UnormSrgb;
    let mut renderer = Renderer::with_views(
        device,
        queue,
        format,
        WIDTH,
        HEIGHT,
        samples,
        &crate::scenery::Sceneries::empty(),
        views,
    );
    renderer.set_environment(tomori_scene::environment::Environment::MINIMAL);
    let mut panels = Panels::new(device, &renderer);
    let mut conversation = conversation();
    let head = Mat4::from_translation(arrival::HOME);
    let senses = Senses {
        focused: true,
        head: Some(head),
        head_pose: Pose::from_transform(head),
        viewer: Some(arrival::HOME),
        space_head: head,
        trigger: Some(0.0),
        left_palm: Some(Palm {
            wrist: arrival::HOME + Vec3::new(-0.06, -0.2, -0.45),
            normal: Vec3::Y,
        }),
        ray: tomori_scene::pointing::Ray::new(arrival::HOME, -Vec3::Y),
        ..Default::default()
    };
    let mut step = Step::default();
    let start = Instant::now();
    step.update(&senses, &mut conversation, &mut felt(), start, 0.01);
    let frame = step.update(&senses, &mut conversation, &mut felt(), start + HOLD, 0.01);
    assert!(frame.arrival.is_some(), "the hand menu shows");
    draw(&mut renderer, device, queue, &mut panels, &frame);
    let layers = renderer.views().count() as u32;
    let textures: Vec<wgpu::Texture> = (0..renderer.views().passes(2))
        .map(|_| {
            device.create_texture(&wgpu::TextureDescriptor {
                label: Some("multiview test eyes"),
                size: wgpu::Extent3d {
                    width: WIDTH,
                    height: HEIGHT,
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
    let targets: Vec<wgpu::TextureView> = textures
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
    renderer.render(
        device,
        queue,
        &targets,
        &eyes(),
        Mat4::IDENTITY,
        &panels.list(Mat4::IDENTITY),
    );
    let images = (0..2)
        .map(|eye| match renderer.views() {
            Views::One => read_back(device, queue, &textures[eye], (WIDTH, HEIGHT)),
            Views::Both => crate::test_gpu::read_layer(
                device,
                queue,
                &textures[0],
                eye as u32,
                (WIDTH, HEIGHT),
            ),
        })
        .collect();
    (images, renderer.views())
}

/// Set `TOMORI_MULTIVIEW_PREVIEW` to a PNG path to save what this renders: each eye drawn in its own pass, then both
/// drawn in one, side by side.
#[test]
fn both_eyes_drawn_in_one_pass_are_what_each_eye_s_own_pass_draws() {
    test_gpu::with_multiview(|test_gpu::Gpu { device, queue, .. }| {
        let scope = device.push_error_scope(wgpu::ErrorFilter::Validation);
        for samples in [1, 4] {
            let (each, one) = drawn(&device, &queue, Views::One, samples);
            let (both, multiview) = drawn(&device, &queue, Views::Both, samples);
            assert_eq!((one, multiview), (Views::One, Views::Both));
            let mut sheet = image::RgbaImage::new(WIDTH * 4, HEIGHT);
            for (index, image) in each.iter().chain(&both).enumerate() {
                image::imageops::replace(&mut sheet, image, i64::from(WIDTH) * index as i64, 0);
            }
            crate::test_support::preview("TOMORI_MULTIVIEW_PREVIEW", &sheet);
            // The eyes see differently, and each sees something.
            assert_ne!(each[0], each[1]);
            for eye in 0..2 {
                let lit = each[eye]
                    .pixels()
                    .filter(|pixel| pixel.0[..3].iter().any(|channel| *channel > 60))
                    .count();
                assert!(lit > 2_000, "eye {eye} draws the scene: {lit}");
                let differing = each[eye]
                    .pixels()
                    .zip(both[eye].pixels())
                    .filter(|(a, b)| a.0.iter().zip(b.0).any(|(a, b)| a.abs_diff(b) > 2))
                    .count();
                assert!(
                    differing <= (WIDTH * HEIGHT / 1000) as usize,
                    "eye {eye} at {samples} samples: {differing} pixels differ"
                );
            }
        }
        assert!(pollster::block_on(scope.pop()).is_none());
    });
}

#[test]
fn a_device_without_multiview_draws_each_eye_in_its_own_pass() {
    test_gpu::with(|test_gpu::Gpu { adapter, .. }| {
        // A device asked for nothing has no multiview.
        let (device, queue) =
            pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default())).unwrap();
        assert!(!crate::multiview::supported(&device, 4));
        let renderer = Renderer::with_views(
            &device,
            &queue,
            wgpu::TextureFormat::Rgba8UnormSrgb,
            8,
            8,
            1,
            &crate::scenery::Sceneries::empty(),
            Views::Both,
        );
        assert_eq!(renderer.views(), Views::One);
        // And says why.
        let why = crate::multiview::unsupported(&device, 1);
        assert!(why.is_some());
        assert_eq!(renderer.single_view(), why);
    });
}
