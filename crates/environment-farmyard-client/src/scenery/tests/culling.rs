// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

//! What culling keeps of the farmyard, from the four viewpoints the rendering proposal measured, through a Quest 3's two
//! eyes: that it takes away nothing either eye would see, and how much it leaves to draw, within its budgets. Set
//! `TOMORI_FARMYARD_CENSUS` to anything to print the census.
use std::f32::consts::{FRAC_PI_2, FRAC_PI_4, PI};

use tomori_scene::scene::headset::{Fov, View, view_projection};

use super::*;
use tomori_render::scenery::Stage;

/// A Quest 3's eyes, about, each its own asymmetric frustum: 52° out, 41° in, 40° up and 50° down, 64 mm apart.
pub(super) fn quest_eyes(middle: Vec3, heading: f32) -> [Mat4; 2] {
    let facing =
        glam::Quat::from_rotation_y(heading) * glam::Quat::from_rotation_x(-10f32.to_radians());
    let across = facing * Vec3::X * 0.032;
    let degrees = |value: f32| value.to_radians();
    [
        (
            middle - across,
            Fov {
                left: degrees(-52.0),
                right: degrees(41.0),
                down: degrees(-50.0),
                up: degrees(40.0),
            },
        ),
        (
            middle + across,
            Fov {
                left: degrees(-41.0),
                right: degrees(52.0),
                down: degrees(-50.0),
                up: degrees(40.0),
            },
        ),
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

/// The heading, about the vertical from −Z, that looks from `from` towards `to`.
fn heading(from: Vec3, to: Vec3) -> f32 {
    let way = to - from;
    (-way.x).atan2(-way.z)
}

/// The proposal's viewpoints, each where the eyes are and the heading of the subject seen from there: the arrival,
/// facing the coop's corner as participants arrive; the yard before the workbench, looking into its corner; station 3,
/// in its clearing, looking along the trail; and the campsite, seated on the lookout's log, looking at the fire.
pub(super) fn viewpoints() -> [(&'static str, Vec3, f32); 4] {
    let arrival = layout::ARRIVAL + Vec3::Y * 1.6;
    let (bench, into) = workbench();
    let station = trail::STATIONS[2];
    let (_, along, _) = trail::frame(station.at);
    let clearing = Vec3::new(station.clearing().x, 1.6, station.clearing().y);
    let (seat, fire) = campsite_view();
    [
        ("Arrival", arrival, heading(arrival, layout::ARRIVAL_FACING)),
        ("Workbench", bench, heading(bench, into)),
        (
            "Trail, station 3",
            clearing,
            heading(clearing, clearing + Vec3::new(along.x, 0.0, along.y)),
        ),
        ("Campsite", seat, heading(seat, fire)),
    ]
}

/// The farmyard, its flock out, as its client side shows it a little after it is made.
pub(super) fn with_hens(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    width: u32,
    height: u32,
) -> Farmyard {
    farmyard_with_hens(
        device,
        queue,
        (width, height),
        tomori_render::multiview::Views::One,
        false,
        Some(CHUNK),
    )
}

/// As [`with_hens`], its models in chunks at most `chunk` across, or whole.
pub(super) fn chunked_with_hens(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    size: (u32, u32),
    chunk: Option<f32>,
) -> Farmyard {
    farmyard_with_hens(
        device,
        queue,
        size,
        tomori_render::multiview::Views::One,
        false,
        chunk,
    )
}

/// As [`with_hens`], drawing `views` a pass, a trail of light painted before the arrival's eyes.
pub(super) fn with_hens_drawing(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    width: u32,
    height: u32,
    views: tomori_render::multiview::Views,
) -> Farmyard {
    farmyard_with_hens(device, queue, (width, height), views, true, Some(CHUNK))
}

/// The farmyard, its flock out, drawing `views` a pass, with a trail of light if `painted`, its models in chunks at
/// most `chunk` across, or whole.
fn farmyard_with_hens(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    (width, height): (u32, u32),
    views: tomori_render::multiview::Views,
    painted: bool,
    chunk: Option<f32>,
) -> Farmyard {
    use tomori_scene::client_side::{ClientSide, Input};
    let target = Target::new(
        device,
        queue,
        wgpu::TextureFormat::Rgba8UnormSrgb,
        width,
        height,
        4,
        tomori_render::glyphs::SharedGlyphs::new(device),
    )
    .with_views(views);
    let mut farmyard = Farmyard::with_chunks(
        &target,
        &Coop::Built,
        Library::shared(),
        Clock::Held(2.0),
        chunk,
    );
    if painted {
        let before = layout::ARRIVAL + Vec3::new(0.0, 1.4, -1.2);
        let path: Vec<Vec3> = (0..20)
            .map(|step| before + Vec3::X * (0.04 * step as f32 - 0.4))
            .collect();
        farmyard.embers_mut().paint(0, &path, 0.02, 1.8);
    }
    let chicken = tomori_environment_farmyard::chicken::Chicken::shared().unwrap();
    let flock = tomori_environment_farmyard::flock::Flock::new(
        tomori_environment_farmyard::flock::seed("space-1"),
        chicken.clone(),
        0,
    );
    let state = tomori_protocol::Payload::new(serde_json::to_value(flock.state(0)).unwrap());
    let epoch = std::time::Instant::now();
    let mut client = crate::client::FarmyardClient::new("space-1", Some(chicken), epoch);
    let output = client.update(Input::new(
        vec![tomori_scene::channel::Update::State(state)],
        epoch + std::time::Duration::from_millis(400),
    ));
    farmyard.animate(device, queue, output.drawn.unwrap().as_ref());
    farmyard
}

/// How far two pictures differ: the pixels whose colour differs at all.
fn differing(a: &[u8], b: &[u8]) -> usize {
    a.chunks(4).zip(b.chunks(4)).filter(|(a, b)| a != b).count()
}

#[test]
fn culling_takes_away_nothing_either_eye_sees_from_the_farmyard_s_viewpoints() {
    test_gpu::with(|test_gpu::Gpu { device, queue, .. }| {
        let scope = device.push_error_scope(wgpu::ErrorFilter::Validation);
        let size = (448, 424);
        let mut farmyard = with_hens(&device, &queue, size.0, size.1);
        let mut stage = Stage::new();
        let mut culled_any = 0;
        for (name, eyes, facing) in viewpoints() {
            for turn in [0.0, FRAC_PI_2, PI] {
                let clips = quest_eyes(eyes, facing + turn);
                let pictures = |farmyard: &Farmyard| {
                    clips.map(|clip| capture_clip(&device, &queue, farmyard, 4, clip, size))
                };
                stage.cull(&mut farmyard, &queue, &clips, false);
                let whole = pictures(&farmyard);
                let tally = stage.cull(&mut farmyard, &queue, &clips, true);
                culled_any += tally.culled;
                let culled = pictures(&farmyard);
                // Nor do the closed models' back faces show anywhere.
                farmyard.set_back_faces_culled(false);
                let two_sided = pictures(&farmyard);
                farmyard.set_back_faces_culled(true);
                for eye in 0..2 {
                    assert!(
                        whole[eye]
                            .chunks(4)
                            .any(|pixel| pixel[..3] != whole[eye][..3]),
                        "{name}, turned {turn}: eye {eye} sees nothing"
                    );
                    assert_eq!(
                        differing(&whole[eye], &culled[eye]),
                        0,
                        "{name}, turned {turn}: culling changed eye {eye}"
                    );
                    // Without its back faces, a closed part's front face wins the depth tie along its silhouette,
                    // where both meet at one depth: a sample or two of a few pixels in each eye, at most one in 2,000,
                    // shows its front face rather than its back, by a little, and nothing else changes.
                    let changed: Vec<u8> = two_sided[eye]
                        .chunks(4)
                        .zip(culled[eye].chunks(4))
                        .filter(|(a, b)| a != b)
                        .map(|(a, b)| a.iter().zip(b).map(|(a, b)| a.abs_diff(*b)).max().unwrap())
                        .collect();
                    assert!(
                        changed.len() * 2_000 <= (size.0 * size.1) as usize
                            && changed.iter().all(|by| *by <= 48),
                        "{name}, turned {turn}: back faces changed eye {eye}: {changed:?}"
                    );
                }
            }
        }
        assert!(culled_any > 0);
        assert!(pollster::block_on(scope.pop()).is_none());
    });
}

#[test]
fn the_census_of_what_the_farmyard_draws_from_its_viewpoints_keeps_its_budgets() {
    test_gpu::with(|test_gpu::Gpu { device, queue, .. }| {
        let scope = device.push_error_scope(wgpu::ErrorFilter::Validation);
        let mut farmyard = with_hens(&device, &queue, 64, 64);
        let mut stage = Stage::new();
        let print = std::env::var_os("TOMORI_FARMYARD_CENSUS").is_some();
        for (name, eyes, facing) in viewpoints() {
            let mut census = |farmyard: &mut Farmyard, heading: f32| {
                let clips = quest_eyes(eyes, heading);
                let all = stage.cull(farmyard, &queue, &clips, false);
                // The render world culls exactly what the scenery culled itself against the eyes' frustum.
                let itself = farmyard.cull(&queue, &Views::new(&clips));
                let seen = stage.cull(farmyard, &queue, &clips, true);
                assert_eq!(itself, seen);
                (all, seen)
            };
            let (all, seen) = census(&mut farmyard, facing);
            let headings: Vec<_> = (0..8)
                .map(|step| census(&mut farmyard, facing + step as f32 * FRAC_PI_4))
                .collect();
            let share = |seen: usize, all: usize| seen as f32 / all as f32;
            let around = |of: fn(&Tally) -> usize| {
                headings
                    .iter()
                    .map(|(all, seen)| share(of(seen), of(all)))
                    .sum::<f32>()
                    / headings.len() as f32
            };
            let pieces = |tally: &Tally| tally.drawn;
            let triangles = |tally: &Tally| tally.triangles;
            let draws = |tally: &Tally| tally.draws;
            if print {
                eprintln!(
                    "{name}: facing, {} of {} pieces ({:.0}%), {} of {} triangles ({:.0}%), {} of {} draws; eight headings, {:.0}% of pieces, {:.0}% of triangles, {:.0}% of draws",
                    seen.drawn,
                    all.drawn,
                    100.0 * share(seen.drawn, all.drawn),
                    seen.triangles,
                    all.triangles,
                    100.0 * share(seen.triangles, all.triangles),
                    seen.draws,
                    all.draws,
                    100.0 * around(pieces),
                    100.0 * around(triangles),
                    100.0 * around(draws),
                );
            }
            // Nothing is culled unless asked, and culling never draws more.
            assert_eq!(all.culled, 0);
            assert_eq!(seen.drawn + seen.culled, all.drawn);
            assert!(seen.draws <= all.draws && seen.triangles <= all.triangles);
            // The budgets: facing its subject, at most 70% of the triangles are submitted, and about the eight headings
            // at most 60% on average, against all of them before culling.
            assert!(
                share(seen.triangles, all.triangles) <= 0.7,
                "{name}: {seen:?} of {all:?}"
            );
            assert!(around(triangles) <= 0.6, "{name}: {}", around(triangles));
        }
        assert!(pollster::block_on(scope.pop()).is_none());
    });
}

#[test]
fn a_hen_is_drawn_within_her_bounds_through_every_animation() {
    use tomori_assets::bounds::Extent;
    let chicken = tomori_environment_farmyard::chicken::Chicken::shared().unwrap();
    let model = &chicken.model;
    assert!(!model.animations.is_empty());
    let placed = Mat4::from_scale_rotation_translation(
        Vec3::splat(1.1),
        glam::Quat::from_rotation_y(2.0),
        Vec3::new(-3.0, 0.0, -6.0),
    );
    for (animation, clip) in model.animations.iter().enumerate() {
        for step in 0..=24 {
            let mut hen = Instance::new(model.clone());
            hen.transform = placed;
            hen.set_pose(tomori_assets::Pose::sampled(
                model,
                animation,
                clip.duration * step as f32 / 24.0,
            ));
            for draw in hen.draws() {
                for primitive in &model.meshes[draw.mesh].primitives {
                    let bounds = Extent::of(primitive)
                        .placed(&draw)
                        .expect("a hen's skin is whole");
                    let joints = draw.joints.as_deref().unwrap_or(&[]);
                    for point in tomori_assets::deformed(primitive, joints, draw.world) {
                        assert!(
                            point.cmpge(bounds.min - 1e-4).all()
                                && point.cmple(bounds.max + 1e-4).all(),
                            "{:?} at {step}: {point} outside {bounds:?}",
                            clip.name
                        );
                    }
                }
            }
        }
    }
}
