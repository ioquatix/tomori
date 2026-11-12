// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

//! How long the farmyard's scenery takes the GPU to draw, as the headset draws it: two passes, one for each of a Quest
//! 3's eyes, each 1,680 × 1,760 with 4× multisampling resolved in the pass, from the rendering proposal's four
//! viewpoints, by the GPU's timestamps: culled against both eyes as the renderer culls it, each piece lit by the lamps
//! the world lists for it, and, to compare, without culling, with closed models' back faces drawn, with neither, culled
//! with every piece lit by every lamp, with 28 more lanterns about the yard, by each piece's lamps and by every lamp,
//! culled with four burning marshmallows' trails glowing before the eyes, as many as the ring holds; with its models
//! whole rather than in chunks, with its own lamps and with 32; culled with the right eye's pass first; and culled, both
//! eyes drawn in one pass by multiview, as the headset draws them where it can; the twelve taken in turn frame by frame
//! after a few seconds' warming up, so that the GPU's clock changing affects each alike, each the median of 600 frames,
//! with each pass's own where each eye has one. Pass a number for another count, and then a configuration's name, such
//! as `culled`, to time it alone, or several separated by `|`, as a frame that culls what the last culled writes
//! nothing. It times the CPU too: culling, through a render world as the renderer culls, and culling and encoding both
//! eyes. Run by hand on a desktop GPU, not on CI's lavapipe; see `design/rendering.md`.
//!
//! ```sh
//! cargo run --release -p tomori-environment-farmyard-client --example scenery_frames
//! ```
use std::time::Instant;

use glam::{Mat4, Quat, Vec3};
use tomori_assets::Library;
use tomori_environment_farmyard_client::coop::Coop;
use tomori_environment_farmyard_client::embers::{self, Embers};
use tomori_environment_farmyard_client::layout;
use tomori_environment_farmyard_client::scenery::{CHUNK, Clock, Farmyard};
use tomori_environment_farmyard_client::trail;
use tomori_render::multiview::Views as Drawn;
use tomori_render::scenery::{Scenery, Target, Views};
use tomori_render::world::{Light, World};
use tomori_scene::scene::headset::{Fov, View, view_projection};

const SIZE: (u32, u32) = (1680, 1760);
/// The time the scenery is drawn at.
const CLOCK: f32 = 2.0;
/// How long the GPU is kept busy before anything is timed.
const WARM: std::time::Duration = std::time::Duration::from_secs(3);

/// A Quest 3's eyes, about: 52° out, 41° in, 40° up and 50° down, 64 mm apart, looking 10° down.
fn eyes(middle: Vec3, heading: f32) -> [Mat4; 2] {
    let facing = Quat::from_rotation_y(heading) * Quat::from_rotation_x(-10f32.to_radians());
    let across = facing * Vec3::X * 0.032;
    let fov = |left: f32, right: f32| Fov {
        left: left.to_radians(),
        right: right.to_radians(),
        down: -50f32.to_radians(),
        up: 40f32.to_radians(),
    };
    [
        (middle - across, fov(-52.0, 41.0)),
        (middle + across, fov(-41.0, 52.0)),
    ]
    .map(|(position, fov)| {
        view_projection(&View {
            pose: tomori_scene::controller::Pose {
                position: position.to_array(),
                orientation: facing.to_array(),
            },
            fov,
        })
    })
}

/// How the eyes are drawn: each in a pass of its own, the left's first or the right's, or both in one, by multiview.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Eyes {
    LeftFirst,
    RightFirst,
    Both,
}

fn heading(from: Vec3, to: Vec3) -> f32 {
    let way = to - from;
    (-way.x).atan2(-way.z)
}

/// The proposal's viewpoints, as the farmyard's census test takes them.
fn viewpoints() -> [(&'static str, Vec3, f32); 4] {
    let at = |point: glam::Vec2, y: f32| Vec3::new(point.x, y, point.y);
    let arrival = layout::ARRIVAL + Vec3::Y * 1.6;
    let bench = at(layout::on_workbench(-1.0, 0.0), 1.6);
    let into = at(
        layout::on_workbench(
            (layout::WORKBENCH_TOP[0] + layout::WORKBENCH_TOP[1]) * 0.5,
            0.0,
        ),
        layout::WORKBENCH_HEIGHT + 0.2,
    );
    let station = trail::STATIONS[2];
    let (_, along, _) = trail::frame(station.at);
    let clearing = at(station.clearing(), 1.6);
    let (middle, seat_along, length, _) = trail::seat();
    let seat = middle + Vec3::new(seat_along.x, 0.0, seat_along.y) * length * 0.35 + Vec3::Y * 1.15;
    let fire = tomori_environment_farmyard::campsite::FIRE;
    [
        ("arrival", arrival, heading(arrival, layout::ARRIVAL_FACING)),
        ("workbench", bench, heading(bench, into)),
        (
            "trail",
            clearing,
            heading(clearing, clearing + Vec3::new(along.x, 0.0, along.y)),
        ),
        (
            "campsite",
            seat,
            heading(seat, Vec3::new(fire.x - 0.5, 0.4, fire.z - 0.05)),
        ),
    ]
}

/// Four burning marshmallows' trails, swung in loops at 2 m/s for as long as a trail glows, a metre and a half before
/// eyes at `middle` facing `heading`: as many quads as the ring holds, all glowing.
fn paint(embers: &mut Embers, middle: Vec3, heading: f32) {
    let facing = Quat::from_rotation_y(heading);
    let ahead = facing * Vec3::NEG_Z;
    let right = facing * Vec3::X;
    embers.clear();
    for index in 0..4 {
        let centre = middle + ahead * 1.5 + right * (index as f32 - 1.5) * 0.3 - Vec3::Y * 0.2;
        let seconds = embers::LIFE * 0.95;
        let steps = 400;
        let path: Vec<Vec3> = (0..=steps)
            .map(|step| {
                // Loops 0.3 m across, about 2 m/s.
                let turn = step as f32 / steps as f32 * seconds * 6.7 + index as f32;
                centre + right * turn.cos() * 0.15 + Vec3::Y * (turn * 1.3).sin() * 0.15
            })
            .collect();
        embers.paint(
            index,
            &path,
            f64::from(seconds) / f64::from(steps),
            f64::from(CLOCK),
        );
    }
}

fn main() {
    let frames: usize = std::env::args()
        .nth(1)
        .and_then(|count| count.parse().ok())
        .unwrap_or(600);
    // Each configuration: whether it culls against the eyes, whether closed parts' back faces are culled, whether four
    // burning marshmallows' trails, as long as the ring holds, glow before the eyes, whether each piece is lit by the
    // lamps listed for it, whether 28 more lanterns light the yard, whether its models are whole, and how the eyes are
    // drawn: each in its own pass, the left's first or the right's, or both in one.
    let only = std::env::args().nth(2);
    let configurations: Vec<_> = [
        (
            "culled",
            true,
            true,
            false,
            true,
            false,
            false,
            Eyes::LeftFirst,
        ),
        (
            "frustum only",
            true,
            false,
            false,
            true,
            false,
            false,
            Eyes::LeftFirst,
        ),
        (
            "back faces only",
            false,
            true,
            false,
            true,
            false,
            false,
            Eyes::LeftFirst,
        ),
        (
            "neither",
            false,
            false,
            false,
            true,
            false,
            false,
            Eyes::LeftFirst,
        ),
        (
            "culled, every lamp",
            true,
            true,
            false,
            false,
            false,
            false,
            Eyes::LeftFirst,
        ),
        (
            "culled, 32 lamps",
            true,
            true,
            false,
            true,
            true,
            false,
            Eyes::LeftFirst,
        ),
        (
            "culled, 32 lamps, every lamp",
            true,
            true,
            false,
            false,
            true,
            false,
            Eyes::LeftFirst,
        ),
        (
            "culled, with trails",
            true,
            true,
            true,
            true,
            false,
            false,
            Eyes::LeftFirst,
        ),
        (
            "culled, whole",
            true,
            true,
            false,
            true,
            false,
            true,
            Eyes::LeftFirst,
        ),
        (
            "culled, 32 lamps, whole",
            true,
            true,
            false,
            true,
            true,
            true,
            Eyes::LeftFirst,
        ),
        (
            "culled, right first",
            true,
            true,
            false,
            true,
            false,
            false,
            Eyes::RightFirst,
        ),
        (
            "culled, multiview",
            true,
            true,
            false,
            true,
            false,
            false,
            Eyes::Both,
        ),
    ]
    .into_iter()
    .filter(|(name, ..)| {
        only.as_deref()
            .is_none_or(|only| only.split('|').any(|only| only == *name))
    })
    .collect();
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
        backends: wgpu::Backends::VULKAN,
        ..wgpu::InstanceDescriptor::new_without_display_handle()
    });
    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::HighPerformance,
        ..Default::default()
    }))
    .expect("an adapter");
    let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        required_features: wgpu::Features::TIMESTAMP_QUERY
            | (adapter.features() & tomori_render::multiview::FEATURES),
        required_limits: tomori_render::multiview::limits(&adapter, wgpu::Limits::default()),
        ..Default::default()
    }))
    .expect("a device with timestamps");
    let multiview = tomori_render::multiview::supported(&device, 4);
    let configurations: Vec<_> = configurations
        .into_iter()
        .filter(|(.., eyes)| multiview || *eyes != Eyes::Both)
        .collect();
    eprintln!("{}", adapter.get_info().name);
    let format = wgpu::TextureFormat::Rgba8UnormSrgb;
    let layered = |format, samples, usage, layers: u32| {
        device
            .create_texture(&wgpu::TextureDescriptor {
                label: Some("Scenery frames target"),
                size: wgpu::Extent3d {
                    width: SIZE.0,
                    height: SIZE.1,
                    depth_or_array_layers: layers,
                },
                mip_level_count: 1,
                sample_count: samples,
                dimension: wgpu::TextureDimension::D2,
                format,
                usage,
                view_formats: &[],
            })
            .create_view(&wgpu::TextureViewDescriptor {
                dimension: Some(if layers == 1 {
                    wgpu::TextureViewDimension::D2
                } else {
                    wgpu::TextureViewDimension::D2Array
                }),
                ..Default::default()
            })
    };
    let texture = |format, samples, usage| layered(format, samples, usage, 1);
    let resolved: [_; 2] =
        std::array::from_fn(|_| texture(format, 1, wgpu::TextureUsages::RENDER_ATTACHMENT));
    let multisampled = texture(format, 4, wgpu::TextureUsages::RENDER_ATTACHMENT);
    let depth = texture(
        wgpu::TextureFormat::Depth32Float,
        4,
        wgpu::TextureUsages::RENDER_ATTACHMENT,
    );
    // Both eyes' targets, a layer each, for both drawn in one pass.
    let both = multiview.then(|| {
        let usage = wgpu::TextureUsages::RENDER_ATTACHMENT;
        (
            layered(format, 1, usage, 2),
            layered(format, 4, usage, 2),
            layered(wgpu::TextureFormat::Depth32Float, 4, usage, 2),
        )
    });
    let made = |views, chunk| {
        let target = Target::new(
            &device,
            &queue,
            format,
            SIZE.0,
            SIZE.1,
            4,
            tomori_render::glyphs::SharedGlyphs::new(&device),
        )
        .with_views(views);
        Farmyard::with_chunks(
            &target,
            &Coop::Built,
            Library::shared(),
            Clock::Held(CLOCK),
            chunk,
        )
    };
    // The farmyard in chunks, as it is drawn, an eye a pass; whole, as it was; and in chunks, both eyes drawn by
    // multiview.
    let mut farmyards = [
        made(Drawn::One, Some(CHUNK)),
        made(Drawn::One, None),
        made(Drawn::Both, Some(CHUNK)),
    ];
    // The flock out, as its client side shows it a little after it is made.
    let drawn = {
        use tomori_scene::client_side::{ClientSide, Input};
        let chicken = tomori_environment_farmyard::chicken::Chicken::shared().expect("the hen");
        let flock = tomori_environment_farmyard::flock::Flock::new(
            tomori_environment_farmyard::flock::seed("space-1"),
            chicken.clone(),
            0,
        );
        let state = tomori_protocol::Payload::new(serde_json::to_value(flock.state(0)).unwrap());
        let epoch = Instant::now();
        let mut client = tomori_environment_farmyard_client::client::FarmyardClient::new(
            "space-1",
            Some(chicken),
            epoch,
        );
        let output = client.update(Input::new(
            vec![tomori_scene::channel::Update::State(state)],
            epoch + std::time::Duration::from_millis(400),
        ));
        output.drawn.unwrap()
    };
    for farmyard in &mut farmyards {
        farmyard.animate(&device, &queue, drawn.as_ref());
    }
    let queries = device.create_query_set(&wgpu::QuerySetDescriptor {
        label: Some("Scenery frames"),
        ty: wgpu::QueryType::Timestamp,
        count: 4,
    });
    let resolve = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("Scenery frames resolve"),
        size: 32,
        usage: wgpu::BufferUsages::QUERY_RESOLVE | wgpu::BufferUsages::COPY_SRC,
        mapped_at_creation: false,
    });
    let readback = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("Scenery frames readback"),
        size: 32,
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let period = f64::from(queue.get_timestamp_period());
    // A render world for each farmyard, as the renderer's, each with 28 more lanterns in rows across the yard and beyond
    // it, 5 m apart and reaching 4 m, dark unless a configuration lights them.
    let mut worlds: [_; 3] = std::array::from_fn(|_| {
        let mut world = World::new();
        let root = world.add_root();
        let lanterns: Vec<_> = (0..28)
            .map(|index| {
                let at = Vec3::new(
                    -15.0 + (index % 7) as f32 * 5.0,
                    2.0,
                    -20.0 + (index / 7) as f32 * 6.0,
                );
                world.add_light(Light::new(root, at, 4.0, Vec3::ZERO))
            })
            .collect::<Vec<_>>();
        (world, root, lanterns)
    });
    for (name, middle, facing) in viewpoints() {
        let clips = eyes(middle, facing);
        let mut gpu = vec![Vec::with_capacity(frames); configurations.len()];
        // Each pass's time, the first's and the second's, where each eye has its own.
        let mut passes =
            vec![[Vec::with_capacity(frames), Vec::with_capacity(frames)]; configurations.len()];
        let mut cpu = vec![Vec::with_capacity(frames); configurations.len()];
        let mut culls = vec![Vec::with_capacity(frames); configurations.len()];
        let mut tallies = vec![tomori_assets::bounds::Tally::default(); configurations.len()];
        let warming = Instant::now();
        let mut frame = 0;
        while gpu.iter().any(|times| times.len() < frames) {
            let which = frame % configurations.len();
            frame += 1;
            let (_, culled, back_faces, trails, listed, lit, whole, order) = configurations[which];
            // The farmyard, and its render world, drawn so: in chunks or whole, an eye a pass or both in one.
            let drawn_by = match (whole, order) {
                (true, _) => 1,
                (false, Eyes::Both) => 2,
                (false, _) => 0,
            };
            let farmyard = &mut farmyards[drawn_by];
            if trails {
                paint(farmyard.embers_mut(), middle, facing);
            } else {
                farmyard.embers_mut().clear();
            }
            farmyard.animate(&device, &queue, drawn.as_ref());
            let started = Instant::now();
            farmyard.set_back_faces_culled(back_faces);
            farmyard.set_lamps_listed(listed);
            // What it submits to the render world, culled there, then drawn as the world saw it, as the renderer does.
            let (world, root, lanterns) = &mut worlds[drawn_by];
            for lantern in lanterns.iter() {
                world.light_mut(*lantern).expect("a lantern").color = if lit {
                    Vec3::new(1.0, 0.7, 0.35)
                } else {
                    Vec3::ZERO
                };
            }
            world.set_views(&clips);
            world.set_culling(culled);
            farmyard.submit(world, *root);
            world.cull();
            tallies[which] = farmyard.cull(&queue, &Views::of(world, *root));
            let culling = started.elapsed();
            let mut encoder = device.create_command_encoder(&Default::default());
            for (eye, clip) in clips.iter().enumerate() {
                farmyard.prepare_eye(&queue, eye, *clip, tomori_render::scenery::eye(*clip));
            }
            let eyes: &[usize] = match order {
                Eyes::LeftFirst => &[0, 1],
                Eyes::RightFirst => &[1, 0],
                Eyes::Both => &[0],
            };
            for (pass, &eye) in eyes.iter().enumerate() {
                let (resolved, multisampled, depth) = match (&both, order) {
                    (Some((resolved, multisampled, depth)), Eyes::Both) => {
                        (resolved, multisampled, depth)
                    }
                    _ => (&resolved[eye], &multisampled, &depth),
                };
                let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("Scenery frames eye"),
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view: multisampled,
                        resolve_target: Some(resolved),
                        depth_slice: None,
                        ops: wgpu::Operations {
                            load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                            store: wgpu::StoreOp::Discard,
                        },
                    })],
                    depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                        view: depth,
                        depth_ops: Some(wgpu::Operations {
                            load: wgpu::LoadOp::Clear(1.0),
                            store: wgpu::StoreOp::Discard,
                        }),
                        stencil_ops: None,
                    }),
                    timestamp_writes: Some(wgpu::RenderPassTimestampWrites {
                        query_set: &queries,
                        beginning_of_pass_write_index: Some(pass as u32 * 2),
                        end_of_pass_write_index: Some(pass as u32 * 2 + 1),
                    }),
                    occlusion_query_set: None,
                    multiview_mask: if order == Eyes::Both {
                        Drawn::Both.mask()
                    } else {
                        None
                    },
                });
                let drawn_as = if order == Eyes::Both { 0 } else { eye };
                farmyard.draw(&mut pass, drawn_as);
                farmyard.draw_translucent(&mut pass, drawn_as);
            }
            let written = eyes.len() as u32 * 2;
            encoder.resolve_query_set(&queries, 0..written, &resolve, 0);
            encoder.copy_buffer_to_buffer(&resolve, 0, &readback, 0, 32);
            queue.submit([encoder.finish()]);
            let encoded = started.elapsed();
            readback.slice(..).map_async(wgpu::MapMode::Read, |_| {});
            device.poll(wgpu::PollType::wait_indefinitely()).unwrap();
            let ticks: Vec<u64> =
                bytemuck::cast_slice(&readback.slice(..).get_mapped_range().unwrap()).to_vec();
            readback.unmap();
            if warming.elapsed() >= WARM && gpu[which].len() < frames {
                let each: Vec<f64> = (0..eyes.len())
                    .map(|pass| (ticks[pass * 2 + 1] - ticks[pass * 2]) as f64 * period / 1e6)
                    .collect();
                gpu[which].push(each.iter().sum());
                for (pass, time) in each.into_iter().enumerate() {
                    passes[which][pass].push(time);
                }
                cpu[which].push(encoded.as_secs_f64() * 1e3);
                culls[which].push(culling.as_secs_f64() * 1e3);
            }
        }
        let median = |times: &mut Vec<f64>| {
            times.sort_by(f64::total_cmp);
            times[times.len() / 2]
        };
        for (which, (configuration, ..)) in configurations.iter().enumerate() {
            let tally = tallies[which];
            let [first, second] = &mut passes[which];
            let each = if second.is_empty() {
                String::new()
            } else {
                format!(
                    " (first pass {:.4}, second {:.4})",
                    median(first),
                    median(second)
                )
            };
            println!(
                "{name}, {configuration}: GPU {:.4} ms for both eyes{each}, CPU {:.4} ms to cull and {:.4} ms to cull and encode; an eye: {} drawn, {} culled, {} draws, {} triangles",
                median(&mut gpu[which]),
                median(&mut culls[which]),
                median(&mut cpu[which]),
                tally.drawn,
                tally.culled,
                tally.draws,
                tally.triangles,
            );
        }
    }
}
