// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

//! The workers' tests: their stations and flocks, where they stand on the default arc and about a registered placement,
//! and pointing at them.

use super::*;

fn node(id: &str, parent: &str) -> Node {
    Node {
        id: id.into(),
        parent_id: Some(parent.into()),
        title: id.into(),
        state: State::Running,
    }
}

fn workspace(nodes: Vec<Node>) -> Workspace {
    Workspace {
        nodes,
        ..Default::default()
    }
}

fn anchors(scene: &Scene) -> BTreeMap<String, [f32; 3]> {
    scene
        .stations()
        .iter()
        .map(|s| (s.id.clone(), s.position))
        .collect()
}

fn settle(scene: &mut Scene, workspace: &Workspace) {
    for _ in 0..150 {
        scene.update(Some("root"), workspace, 1.0 / 60.0);
    }
}

#[test]
fn a_partial_update_adds_no_worker_beyond_the_limit() {
    let mut scene = Scene::new();
    let full = workspace(
        (0..MAX_NODES)
            .map(|i| node(&format!("w{i}"), "root"))
            .collect(),
    );
    settle(&mut scene, &full);
    let partial = Workspace {
        truncated: true,
        ..workspace(vec![node("late", "root")])
    };
    scene.update(Some("root"), &partial, 1.0 / 60.0);
    assert!(!scene.workers.contains_key("late"));
    assert_eq!(scene.stations().len(), MAX_NODES);
}

#[test]
fn a_cube_that_is_not_valid_geometry_is_not_drawn() {
    let mut cubes = Vec::new();
    for position in [Vec3::NAN, Vec3::splat(1000.0)] {
        push_cube(
            &mut cubes,
            position,
            Vec3::splat(0.1),
            Vec3::ONE,
            Quat::IDENTITY,
        );
    }
    assert!(cubes.is_empty());
    push_cube(
        &mut cubes,
        Vec3::ZERO,
        Vec3::splat(0.1),
        Vec3::ONE,
        Quat::IDENTITY,
    );
    assert_eq!(cubes.len(), 1);
}

fn assert_terminal_form(worker: &Worker) {
    assert!(!worker.flying);
    assert!(worker.terminal_motion.is_none());
    for i in 0..PARTICLES {
        let goal = terminal_goal(worker, i);
        assert!(worker.particles[i].distance(goal) < 0.001);
    }
    let center = worker.particles.iter().sum::<Vec3>() / PARTICLES as f32;
    assert!(
        center.distance(worker.anchor) < 0.00001,
        "terminal form drifted"
    );
}

// Used when every worker is visible enough to emit its full geometry.
fn assert_only_worker_flocks(scene: &Scene) {
    let mut cubes = scene.cubes().iter();
    for worker in scene.workers.values() {
        if solid_completed(worker) {
            let cube = cubes.next().unwrap();
            assert_eq!(cube.position, worker.anchor.to_array());
            assert_eq!(cube.scale, [COMPLETED_SIDE * worker.visual_opacity; 3]);
        } else {
            let center = worker.particles.iter().copied().sum::<Vec3>() / PARTICLES as f32;
            for (position, scale) in worker.particles.iter().zip(worker.particle_scales) {
                let cube = cubes.next().unwrap();
                assert_eq!(
                    cube.position,
                    if worker.visual_opacity == 1.0 {
                        position.to_array()
                    } else {
                        (center + (*position - center) * worker.visual_opacity).to_array()
                    }
                );
                assert_eq!(cube.scale, (scale * worker.visual_opacity).to_array());
            }
        }
    }
    assert!(cubes.next().is_none());
}

// Compare rendered volumes across the one-cube / fitted-pieces boundary,
// not just the hidden particle centers. A solid has the same 24 volumes.
fn rendered_volumes(scene: &Scene) -> [(Vec3, Vec3); PARTICLES] {
    match scene.cubes() {
        [solid] => {
            let center = Vec3::from_array(solid.position);
            let side = Vec3::from_array(solid.scale);
            std::array::from_fn(|i| {
                let offset = Quat::from_array(solid.orientation)
                    * terminal_position(Vec3::ZERO, State::Completed, i);
                (
                    center + offset * side / COMPLETED_SIDE,
                    side / Vec3::new(4.0, 3.0, 2.0),
                )
            })
        }
        pieces => {
            assert_eq!(pieces.len(), PARTICLES);
            std::array::from_fn(|i| {
                (
                    Vec3::from_array(pieces[i].position),
                    Vec3::from_array(pieces[i].scale),
                )
            })
        }
    }
}

fn assert_volume_step(
    before: &[(Vec3, Vec3); PARTICLES],
    scene: &Scene,
    position_limit: f32,
    scale_limit: f32,
) {
    for ((position, scale), (next_position, next_scale)) in
        before.iter().zip(rendered_volumes(scene))
    {
        assert!(
            position.distance(next_position) <= position_limit,
            "rendered position jumped: {position:?} -> {next_position:?}"
        );
        assert!(
            scale.distance(next_scale) <= scale_limit,
            "rendered scale jumped: {scale:?} -> {next_scale:?}"
        );
    }
}

fn assert_fitted_partition(pieces: &[Cube], solid: Cube) {
    assert_eq!(pieces.len(), PARTICLES);
    let center = Vec3::from_array(solid.position);
    let side = Vec3::from_array(solid.scale);
    let inverse = Quat::from_array(solid.orientation).inverse();
    let min = -side * 0.5;
    let max = side * 0.5;
    let mut actual_min = Vec3::splat(f32::INFINITY);
    let mut actual_max = Vec3::splat(f32::NEG_INFINITY);
    let mut volume = 0.0;
    for (i, piece) in pieces.iter().enumerate() {
        assert!(piece.valid());
        let position = inverse * (Vec3::from_array(piece.position) - center);
        assert!(
            Quat::from_array(piece.orientation).angle_between(Quat::from_array(solid.orientation))
                < 0.001
        );
        let scale = Vec3::from_array(piece.scale);
        assert!(scale.distance(side / Vec3::new(4.0, 3.0, 2.0)) < 0.000001);
        let lo = position - scale * 0.5;
        let hi = position + scale * 0.5;
        assert!((lo - min).min_element() > -0.000001);
        assert!((max - hi).min_element() > -0.000001);
        actual_min = actual_min.min(lo);
        actual_max = actual_max.max(hi);
        volume += scale.x * scale.y * scale.z;
        for other in &pieces[i + 1..] {
            let other_position = inverse * (Vec3::from_array(other.position) - center);
            let other_scale = Vec3::from_array(other.scale);
            let overlap = hi.min(other_position + other_scale * 0.5)
                - lo.max(other_position - other_scale * 0.5);
            assert!(
                overlap.min_element() < 0.000001,
                "overlapping partition pieces"
            );
        }
    }
    assert!(actual_min.distance(min) < 0.000001);
    assert!(actual_max.distance(max) < 0.000001);
    // Contained, non-overlapping volumes whose sum equals the solid leave
    // no holes. Matching outer bounds alone would miss scale-only shrinkage.
    assert!((volume - side.x * side.y * side.z).abs() < 0.00000001);
}

#[test]
fn rotated_solid_split_and_all_state_changes_preserve_rendered_orientations() {
    for to in [
        State::Completed,
        State::Error,
        State::Running,
        State::Unknown,
    ] {
        let mut scene = Scene::new();
        let mut w = workspace(vec![Node {
            state: State::Completed,
            ..node("a", "root")
        }]);
        // More than a minute of rotation exercises a far-from-identity
        // partition, not just an almost-axis-aligned completed cube.
        for _ in 0..1500 {
            scene.update(Some("root"), &w, 0.05);
        }
        assert_eq!(scene.cubes().len(), 1);
        let solid = scene.cubes()[0];
        let rotation = Quat::from_array(solid.orientation);
        assert!(rotation.angle_between(Quat::IDENTITY) > 0.3);
        let volumes = rendered_volumes(&scene);
        w.nodes[0].state = to;
        scene.update(Some("root"), &w, 0.0);
        assert_volume_step(&volumes, &scene, 0.000001, 0.000001);
        assert!(
            scene
                .cubes()
                .iter()
                .all(|cube| cube.orientation == solid.orientation)
        );
        for _ in 0..120 {
            let before = Quat::from_array(scene.cubes()[0].orientation);
            let volumes = rendered_volumes(&scene);
            scene.update(Some("root"), &w, 1.0 / 60.0);
            assert!(Quat::from_array(scene.cubes()[0].orientation).angle_between(before) < 0.11);
            assert_volume_step(&volumes, &scene, 0.101, 0.025);
        }
    }
    let mut scene = Scene::new();
    let w = workspace(vec![Node {
        state: State::Completed,
        ..node("a", "root")
    }]);
    for _ in 0..1500 {
        scene.update(Some("root"), &w, 0.05);
    }
    let solid = scene.cubes()[0];
    scene.update(Some("root"), &Workspace::default(), 0.0);
    assert_fitted_partition(scene.cubes(), solid);
    assert!(
        scene
            .cubes()
            .iter()
            .all(|c| c.orientation == solid.orientation)
    );
    for _ in 0..10 {
        scene.update(Some("root"), &Workspace::default(), 1.0 / 60.0);
    }
    let before: Vec<_> = scene.cubes().iter().map(|c| c.orientation).collect();
    let volumes = rendered_volumes(&scene);
    scene.update(Some("root"), &w, 0.0);
    assert_volume_step(&volumes, &scene, 0.000001, 0.000001);
    assert_eq!(
        scene
            .cubes()
            .iter()
            .map(|c| c.orientation)
            .collect::<Vec<_>>(),
        before
    );
}

#[test]
fn error_wave_is_small_slow_zero_mean_and_does_not_close_the_fracture() {
    let mut scene = Scene::new();
    let w = workspace(vec![Node {
        state: State::Error,
        ..node("a", "root")
    }]);
    scene.update(Some("root"), &w, 0.0);
    let initial = scene.workers["a"].particles;
    let mut maximum = 0.0_f32;
    for frame in 0..1200 {
        let before = scene.workers["a"].particles;
        let evidence = if frame % 3 == 0 {
            Workspace {
                stale: true,
                ..w.clone()
            }
        } else {
            w.clone()
        };
        scene.update(Some("root"), &evidence, 1.0 / 60.0);
        let worker = &scene.workers["a"];
        assert_terminal_form(worker);
        let radial = Vec3::new(worker.anchor.x, 0.0, worker.anchor.z).normalize();
        let tangent = Vec3::new(-radial.z, 0.0, radial.x);
        for (i, position) in worker.particles.iter().enumerate() {
            let displacement = position.distance(initial[i]);
            maximum = maximum.max(displacement);
            assert!(displacement <= 0.01201);
            assert!(position.distance(before[i]) < 0.00015);
            let x = (*position - worker.anchor).dot(tangent);
            assert!(if i < 12 { x < -0.06 } else { x > 0.06 });
        }
    }
    assert!(maximum > 0.01, "the fracture must actually wave");
}

#[test]
fn cube_quaternions_are_bounded_and_normalized() {
    let upright = Cube {
        position: [0.0, 1.0, -2.0],
        scale: [0.24; 3],
        color: [0.3, 0.8, 0.5],
        orientation: Quat::IDENTITY.to_array(),
    };
    assert!(upright.valid());
    let rotated = Cube {
        orientation: Quat::from_rotation_y(1.2).to_array(),
        ..upright
    };
    assert!(rotated.valid());
    for orientation in [
        [0.0; 4],
        [1.0; 4],
        [f32::NAN, 0.0, 0.0, 1.0],
        [f32::INFINITY, 0.0, 0.0, 1.0],
        [0.0, 0.0, 0.0, 0.9],
        [0.0, 0.0, 0.0, 1.01],
    ] {
        assert!(
            !Cube {
                orientation,
                ..upright
            }
            .valid()
        );
    }
}

#[test]
fn completed_emits_one_solid_cube_after_history_arrival_or_observed_settle() {
    for discovery in 0..3 {
        let mut scene = Scene::new();
        let mut w = workspace(vec![Node {
            state: State::Completed,
            ..node("a", "root")
        }]);
        if discovery == 1 {
            scene.update(Some("root"), &Workspace::default(), 0.0);
        } else if discovery == 2 {
            w.nodes[0].state = State::Running;
            settle(&mut scene, &w);
            w.nodes[0].state = State::Completed;
        }
        scene.update(Some("root"), &w, 0.0);
        assert_eq!(scene.workers["a"].flying, discovery == 1);
        assert_eq!(scene.workers["a"].terminal_motion.is_some(), discovery == 2);
        settle(&mut scene, &w);
        assert!(solid_completed(&scene.workers["a"]));
        assert_eq!(scene.cubes().len(), 1, "discovery {discovery}");
        let solid = scene.cubes()[0];
        assert_eq!(solid.position, scene.workers["a"].anchor.to_array());
        assert_eq!(solid.scale, [COMPLETED_SIDE; 3]);
        assert!(Vec3::from_array(solid.color).distance(palette(State::Completed)) < 0.000001);
        for _ in 0..120 {
            scene.update(Some("root"), &w, 1.0 / 60.0);
            assert_eq!(scene.cubes().len(), 1);
            assert_eq!(scene.cubes()[0].position, solid.position);
            assert_eq!(scene.cubes()[0].scale, solid.scale);
        }
    }
}

#[test]
fn solid_return_onset_exactly_tiles_full_and_uncertainty_faded_bounds() {
    for faded in [false, true] {
        let mut scene = Scene::new();
        let w = workspace(vec![Node {
            state: State::Completed,
            ..node("a", "root")
        }]);
        settle(&mut scene, &w);
        if faded {
            let stale = Workspace {
                stale: true,
                ..w.clone()
            };
            for _ in 0..20 {
                scene.update(Some("root"), &stale, 0.05);
            }
            assert_eq!(scene.workers["a"].visual_opacity, 0.55);
        }
        assert_eq!(scene.cubes().len(), 1);
        let solid = scene.cubes()[0];
        let before = rendered_volumes(&scene);
        scene.update(Some("root"), &Workspace::default(), 0.0);
        assert_eq!(scene.workers["a"].retired, Some(0.0));
        assert_fitted_partition(scene.cubes(), solid);
        assert_volume_step(&before, &scene, 0.000001, 0.000001);
    }
}

#[test]
fn completed_departure_emits_24_visible_pieces_smoothly_toward_captured_samples() {
    let mut scene = Scene::new();
    let w = workspace(vec![Node {
        state: State::Completed,
        ..node("a", "root")
    }]);
    settle(&mut scene, &w);
    let samples: Vec<_> = (0..PARTICLES)
        .map(|i| (CENTER + Vec3::new(i as f32 * 0.004, 0.0, -0.1)).to_array())
        .collect();
    scene.set_coordinator_samples(&samples);
    let start = scene.workers["a"].particles;
    let before = rendered_volumes(&scene);
    scene.update(Some("root"), &Workspace::default(), 0.0);
    assert_volume_step(&before, &scene, 0.000001, 0.000001);
    let targets = scene.workers["a"].return_end;
    assert!(targets.iter().all(|p| samples.contains(&p.to_array())));
    for (i, target) in targets.iter().enumerate() {
        assert!(!targets[i + 1..].contains(target));
    }
    // Moving coordinator samples do not teleport an in-flight destination.
    scene.set_coordinator_samples(&[[5.0, 5.0, 5.0]]);
    for frame in 1..=60 {
        let before = rendered_volumes(&scene);
        scene.update(Some("root"), &Workspace::default(), 1.0 / 60.0);
        assert_eq!(scene.cubes().len(), PARTICLES, "frame {frame}");
        assert!(scene.cubes().iter().all(Cube::valid));
        assert_volume_step(&before, &scene, 0.101, 0.025);
        assert_eq!(scene.workers["a"].return_end, targets);
        if frame == 30 {
            assert!(
                scene
                    .cubes()
                    .iter()
                    .all(|cube| cube.scale.iter().all(|s| *s >= 0.023))
            );
            let worker = &scene.workers["a"];
            for i in 0..PARTICLES {
                assert!(
                    worker.particles[i].distance(targets[i]) < start[i].distance(targets[i]) * 0.8
                );
                assert_eq!(worker.particle_scales[i], particle_scale(i));
            }
        }
    }
    assert!(
        scene.workers["a"]
            .particles
            .iter()
            .zip(targets)
            .all(|(p, target)| p.distance(target) < 0.4)
    );
    for _ in 0..30 {
        scene.update(Some("root"), &Workspace::default(), 1.0 / 60.0);
    }
    assert!(scene.workers.is_empty());
    assert!(scene.cubes().is_empty());
}

#[test]
fn new_worker_renders_24_pieces_at_actual_samples_without_centroid_collapse() {
    let mut scene = Scene::new();
    scene.update(Some("root"), &Workspace::default(), 0.0);
    let samples: Vec<_> = (0..PARTICLES)
        .map(|i| (CENTER + Vec3::new(i as f32 * 0.03 - 0.35, 0.0, 0.0)).to_array())
        .collect();
    scene.set_coordinator_samples(&samples);
    let w = workspace(vec![node("a", "root")]);
    scene.update(Some("root"), &w, 0.0);
    assert_eq!(scene.cubes().len(), PARTICLES);
    for (i, cube) in scene.cubes().iter().enumerate() {
        assert!(samples.contains(&cube.position));
        assert_eq!(cube.scale, particle_scale(i).to_array());
    }
    for _ in 0..90 {
        let before = rendered_volumes(&scene);
        scene.update(Some("root"), &w, 1.0 / 60.0);
        assert_eq!(scene.cubes().len(), PARTICLES);
        assert_volume_step(&before, &scene, 0.101, 0.000001);
        assert!(scene.cubes().iter().all(Cube::valid));
        // Rendered centers, not only hidden simulation positions, follow
        // the bounded path starting at the actual coordinator samples.
        for (cube, position) in scene.cubes().iter().zip(scene.workers["a"].particles) {
            assert_eq!(cube.position, position.to_array());
        }
    }
}

#[test]
fn rendered_positions_and_scales_are_continuous_across_every_state_pair() {
    let states = [
        State::Running,
        State::Waiting,
        State::Idle,
        State::Completed,
        State::Error,
        State::Stopped,
        State::Unknown,
    ];
    for from in states {
        for to in states {
            let mut scene = Scene::new();
            let mut w = workspace(vec![Node {
                state: from,
                ..node("a", "root")
            }]);
            settle(&mut scene, &w);
            let before = rendered_volumes(&scene);
            w.nodes[0].state = to;
            scene.update(Some("root"), &w, 0.0);
            assert_volume_step(&before, &scene, 0.000001, 0.000001);
            for _ in 0..180 {
                let before = rendered_volumes(&scene);
                scene.update(Some("root"), &w, 1.0 / 60.0);
                assert_volume_step(&before, &scene, 0.101, 0.025);
                assert!(scene.cubes().iter().all(Cube::valid));
            }
            assert_eq!(
                scene.cubes().len(),
                if to == State::Completed { 1 } else { PARTICLES }
            );
            assert_eq!(scene.stations()[0].state, to);
        }
    }
}

#[test]
fn solid_resurrection_retains_rendered_scale_and_position_even_during_late_fade() {
    for return_frames in [0, 12, 60, 68] {
        for resumed_state in [
            State::Completed,
            State::Running,
            State::Error,
            State::Unknown,
        ] {
            let mut scene = Scene::new();
            let mut w = workspace(vec![Node {
                state: State::Completed,
                ..node("a", "root")
            }]);
            settle(&mut scene, &w);
            scene.update(Some("root"), &Workspace::default(), 0.0);
            for _ in 0..return_frames {
                scene.update(Some("root"), &Workspace::default(), 1.0 / 60.0);
            }
            let before = rendered_volumes(&scene);
            w.nodes[0].state = resumed_state;
            scene.update(Some("root"), &w, 0.0);
            assert!(scene.workers["a"].retired.is_none());
            assert!(
                scene.workers["a"]
                    .terminal_motion
                    .is_none_or(|motion| !motion.bounce)
            );
            assert_volume_step(&before, &scene, 0.000001, 0.000001);
            for _ in 0..180 {
                let before = rendered_volumes(&scene);
                scene.update(Some("root"), &w, 1.0 / 60.0);
                // Up to 0.1 m particle motion plus the bounded expansion
                // about the centroid when a faded return is resurrected.
                assert_volume_step(&before, &scene, 0.12, 0.025);
                assert!(scene.cubes().iter().all(Cube::valid));
            }
            assert_eq!(
                scene.cubes().len(),
                if resumed_state == State::Completed {
                    1
                } else {
                    PARTICLES
                }
            );
        }
    }
}

#[test]
fn terminal_history_metadata_and_resumed_evidence_do_not_invent_launches() {
    let mut scene = Scene::new();
    let mut w = workspace(vec![Node {
        state: State::Completed,
        ..node("history", "root")
    }]);
    scene.set_coordinator_samples(&[[0.4, 1.5, -2.4]]);
    scene.update(Some("root"), &w, 0.0);
    assert_eq!(scene.cubes().len(), 1);
    let position = scene.cubes()[0].position;
    w.presentations.push(Presentation {
        id: "historical-card".into(),
        source_id: "history".into(),
        source_message_id: "old-message".into(),
        ..Default::default()
    });
    w.nodes[0].title = "Historical result".into();
    scene.update(Some("root"), &w, 1000.0);
    assert_eq!(scene.cubes().len(), 1);
    assert_eq!(scene.cubes()[0].position, position);
    assert!(!scene.workers["history"].flying);
    assert!(scene.workers["history"].terminal_motion.is_none());
    for uncertain in [
        Workspace {
            stale: true,
            ..w.clone()
        },
        Workspace {
            truncated: true,
            ..Default::default()
        },
        Workspace {
            error: Some("offline".into()),
            ..Default::default()
        },
    ] {
        scene.update(Some("root"), &uncertain, 0.05);
        assert!(scene.workers["history"].retired.is_none());
        w.nodes.push(Node {
            state: State::Completed,
            ..node(&format!("recovered-{}", w.nodes.len()), "root")
        });
        scene.update(Some("root"), &w, 0.0);
        assert_eq!(scene.cubes().len(), w.nodes.len());
        assert!(scene.workers.values().all(|worker| !worker.flying
            && worker.terminal_motion.is_none()
            && worker.retired.is_none()));
    }
    // Only a genuinely new observation after continuous trusted evidence
    // launches 24 particles, each from an actual sampled coordinator cube.
    w.nodes.push(node("new", "root"));
    scene.update(Some("root"), &w, 0.0);
    assert!(scene.workers["new"].flying);
    assert_eq!(
        scene.workers["new"].particles,
        [Vec3::new(0.4, 1.5, -2.4); PARTICLES]
    );
    for _ in 0..12 {
        scene.update(Some("root"), &w, 1.0 / 60.0);
    }
    assert_eq!(scene.cubes().len(), w.nodes.len() - 1 + PARTICLES);
    assert_only_worker_flocks(&scene);
}

#[test]
fn resurrecting_near_the_end_of_split_does_not_snap_scale_at_zero_dt() {
    let mut scene = Scene::new();
    let mut w = workspace(vec![Node {
        state: State::Completed,
        ..node("a", "root")
    }]);
    settle(&mut scene, &w);
    scene.update(Some("root"), &Workspace::default(), 0.0);
    for _ in 0..7 {
        scene.update(
            Some("root"),
            &Workspace::default(),
            (SPLIT_SECONDS - 0.001) / 7.0,
        );
    }
    let before = scene.workers["a"].particle_scales;
    assert!(before[0].distance(particle_scale(0)) > 0.0);
    assert!(before[0].distance(particle_scale(0)) < 0.00001);
    w.nodes[0].state = State::Running;
    scene.update(Some("root"), &w, 0.0);
    assert_eq!(scene.workers["a"].particle_scales, before);
}

#[test]
fn the_defaults_are_an_empty_current_workspace_and_an_unknown_state() {
    let empty = Workspace::default();
    assert!(empty.valid());
    assert!(!empty.stale);
    assert_eq!(Node::default().state, State::Unknown);
    assert_eq!(Kind::default(), Kind::Markdown);
}

#[test]
fn validates_ids_cycles_and_missing_parents() {
    assert!(workspace(vec![node("a", "absent")]).valid());
    assert!(!workspace(vec![node("a", "root"), node("a", "root")]).valid());
    assert!(!workspace(vec![node("a", "a")]).valid());
    assert!(!workspace(vec![node("a", "b"), node("b", "a")]).valid());
    assert!(!workspace(vec![node("", "root")]).valid());
    assert!(!workspace(vec![node(&"x".repeat(257), "root")]).valid());
    let mut w = workspace((0..65).map(|i| node(&i.to_string(), "root")).collect());
    assert!(!w.valid());
    w.nodes.pop();
    assert!(w.valid());
    w.nodes[0].title = "🦀".repeat(160);
    assert!(w.valid());
    w.nodes[0].title.push('x');
    assert!(!w.valid());
}

#[test]
fn validates_bounded_rectangular_presentations() {
    let card = Presentation {
        id: "card".into(),
        source_id: "worker".into(),
        kind: Kind::Table,
        columns: vec!["one".into(), "two".into()],
        rows: vec![vec!["a".into(), "b".into()]],
        ..Default::default()
    };
    let mut w = Workspace {
        presentations: vec![card.clone()],
        ..Default::default()
    };
    assert!(w.valid());
    w.presentations[0].rows[0].pop();
    assert!(!w.valid());
    w.presentations[0] = card.clone();
    w.presentations[0].text = "é".repeat(2049);
    assert!(!w.valid());
    w.presentations[0] = card.clone();
    w.presentations[0].columns[0] = "x".repeat(81);
    assert!(!w.valid());
    w.presentations[0] = card.clone();
    w.presentations[0].rows[0][0] = "x".repeat(161);
    assert!(!w.valid());
    w.presentations = vec![card.clone(), card.clone()];
    assert!(!w.valid());
    w.presentations = (0..17)
        .map(|i| Presentation {
            id: i.to_string(),
            ..card.clone()
        })
        .collect();
    assert!(!w.valid());
}

#[test]
fn cube_validation_rejects_bad_geometry() {
    let cube = Cube {
        position: CENTER.to_array(),
        scale: [0.02; 3],
        color: [0.5; 3],
        orientation: Quat::IDENTITY.to_array(),
    };
    assert!(cube.valid());
    for bad in [f32::NAN, f32::INFINITY, -f32::INFINITY, 101.0] {
        assert!(
            !Cube {
                position: [bad, 1.0, 0.0],
                ..cube
            }
            .valid()
        );
    }
    for bad in [f32::NAN, -0.1, 0.0, 1.1] {
        assert!(
            !Cube {
                scale: [bad; 3],
                ..cube
            }
            .valid()
        );
    }
    assert!(
        !Cube {
            color: [1.1; 3],
            ..cube
        }
        .valid()
    );
    assert!(
        !Cube {
            color: [f32::NAN; 3],
            ..cube
        }
        .valid()
    );
}

#[test]
fn station_validation() {
    let station = Station {
        facing: HOME_EYES,
        id: "worker".into(),
        position: [1.0, 2.0, -3.0],
        state: State::Waiting,
        title: "A worker".into(),
        opacity: 0.4,
        scale: 1.0,
        more: 0,
        size: None,
        highlighted: false,
    };
    assert!(station.valid());
    // A compact sign, and one for those gathered into a cluster's "+N more", are kept, within bounds.
    let gathered = Station {
        scale: tomori_protocol::cluster::COMPACT,
        more: 3,
        ..station.clone()
    };
    assert!(gathered.valid());
    for (scale, more) in [(0.05, 0), (f32::NAN, 0), (1.2, 0), (1.0, MAX_WORKERS + 1)] {
        assert!(
            !Station {
                scale,
                more,
                ..station.clone()
            }
            .valid()
        );
    }
    for bad in [f32::NAN, f32::INFINITY, -0.1, 1.1] {
        assert!(
            !Station {
                facing: HOME_EYES,
                opacity: bad,
                ..station.clone()
            }
            .valid()
        );
    }
    assert!(
        !Station {
            facing: HOME_EYES,
            position: [f32::NAN, 0.0, 0.0],
            ..station.clone()
        }
        .valid()
    );
    assert!(
        !Station {
            facing: HOME_EYES,
            id: "x".repeat(161),
            ..station.clone()
        }
        .valid()
    );
    assert!(
        !Station {
            facing: HOME_EYES,
            title: "x".repeat(161),
            ..station
        }
        .valid()
    );
}

#[test]
fn parentless_root_and_unrelated_entries_are_not_workers() {
    let mut scene = Scene::new();
    let w = workspace(vec![
        Node {
            id: "root".into(),
            ..Default::default()
        },
        Node {
            id: "unrelated".into(),
            ..Default::default()
        },
        node("child", "root"),
    ]);
    scene.update(Some("root"), &w, 0.02);
    assert_eq!(scene.stations().len(), 1);
    assert_eq!(scene.stations()[0].id, "child");
    scene.update(None, &w, 0.02);
    assert!(scene.stations().is_empty());
    assert!(scene.cubes().is_empty());
}

#[test]
fn initial_order_and_animation_are_deterministic() {
    let a = workspace(vec![node("c", "b"), node("b", "root"), node("a", "root")]);
    let b = workspace(a.nodes.iter().cloned().rev().collect());
    let mut one = Scene::new();
    let mut two = Scene::default();
    settle(&mut one, &a);
    settle(&mut two, &b);
    assert_eq!(anchors(&one), anchors(&two));
    assert_eq!(format!("{:?}", one.cubes()), format!("{:?}", two.cubes()));
    assert_eq!(one.snapshot().len(), one.cubes().len());
}

#[test]
fn surviving_anchors_do_not_move_on_add_remove_reorder_or_state() {
    let mut w = workspace(vec![node("b", "root"), node("c", "b")]);
    let mut scene = Scene::new();
    settle(&mut scene, &w);
    let before = anchors(&scene);
    w.nodes.reverse();
    w.nodes.push(node("a", "root"));
    w.nodes[0].state = State::Completed;
    scene.update(Some("root"), &w, 1.0 / 60.0);
    for id in ["b", "c"] {
        assert_eq!(anchors(&scene)[id], before[id]);
    }
    w.nodes.retain(|n| n.id != "b");
    settle(&mut scene, &w);
    assert_eq!(anchors(&scene)["c"], before["c"]);
    assert!(!anchors(&scene).contains_key("b"));
    // Archived parent provenance remains cached, rather than using CENTER.
    assert_eq!(slot_position(scene.slots["b"]).to_array(), before["b"]);
    w.nodes.push(node("b", "root"));
    scene.update(Some("root"), &w, 0.0);
    assert_eq!(anchors(&scene)["b"], before["b"]);
}

#[test]
fn archived_parent_anchor_is_reserved_before_new_workers() {
    let mut scene = Scene::new();
    let mut w = workspace(vec![node("parent", "root"), node("child", "parent")]);
    settle(&mut scene, &w);
    let parent_anchor = scene.workers["parent"].anchor;
    let child_anchor = scene.workers["child"].anchor;
    w.nodes.retain(|n| n.id != "parent");
    settle(&mut scene, &w);
    assert!(!scene.workers.contains_key("parent"));
    w.nodes.push(node("new", "root"));
    scene.update(Some("root"), &w, 0.02);
    assert_eq!(slot_position(scene.slots["parent"]), parent_anchor);
    assert_eq!(scene.workers["child"].anchor, child_anchor);
    assert_ne!(scene.workers["new"].anchor, parent_anchor);
}

#[test]
fn root_switch_and_deselect_clear_all_previous_geometry() {
    let w = workspace(vec![node("old", "root")]);
    let mut scene = Scene::new();
    settle(&mut scene, &w);
    let stale = Workspace {
        error: Some("offline".into()),
        stale: true,
        ..w.clone()
    };
    scene.update(Some("new-root"), &stale, 0.02);
    assert!(scene.cubes().is_empty());
    assert!(scene.stations().is_empty());
    assert!(scene.slots.is_empty());
    scene.update(None, &w, 0.02);
    assert!(scene.cubes().is_empty());
    scene.update(Some("root"), &w, 0.02);
    let mut fresh = Scene::new();
    fresh.update(Some("root"), &w, 0.02);
    assert_eq!(anchors(&scene), anchors(&fresh));
}

#[test]
fn switching_between_fresh_root_projections_does_not_retain_old_workers() {
    let mut scene = Scene::new();
    settle(&mut scene, &workspace(vec![node("old", "root")]));
    let next = workspace(vec![node("new", "other-root")]);
    scene.update(Some("other-root"), &next, 0.02);
    assert_eq!(scene.stations().len(), 1);
    assert_eq!(scene.stations()[0].id, "new");
    assert!(!scene.workers.contains_key("old"));
    assert!(!scene.slots.contains_key("old"));
}

#[test]
fn partial_old_root_nodes_cannot_leak_into_new_root() {
    let mut scene = Scene::new();
    settle(&mut scene, &workspace(vec![node("old", "root")]));
    let partial = Workspace {
        nodes: vec![
            node("old", "root"),
            node("nested", "old"),
            node("new", "other-root"),
        ],
        truncated: true,
        ..Default::default()
    };
    scene.update(Some("other-root"), &partial, 0.02);
    assert_eq!(scene.stations().len(), 1);
    assert_eq!(scene.stations()[0].id, "new");
    assert_eq!(scene.stations()[0].state, State::Running);
}

#[test]
fn stale_old_projection_cannot_add_nodes_even_when_new_root_is_a_descendant() {
    let mut scene = Scene::new();
    let old = workspace(vec![node("parent", "root"), node("child", "parent")]);
    settle(&mut scene, &old);
    let stale = Workspace { stale: true, ..old };
    scene.update(Some("parent"), &stale, 0.02);
    assert!(scene.stations().is_empty());
    scene.update(Some("parent"), &stale, 0.02);
    assert!(scene.stations().is_empty());
}

#[test]
fn slots_are_bounded_distinct_without_hidden_radial_overflow() {
    let points: Vec<_> = (0..SLOT_COUNT).map(slot_position).collect();
    for (i, p) in points.iter().enumerate() {
        assert!(p.is_finite());
        assert!((0.2..12.5).contains(&p.y), "{i}: {p}");
        // On the band through the agent's default home, 2.3 m from home, or the arc 3.2 m from home above it.
        let radius = p.x.hypot(p.z);
        assert!((radius - 2.3).abs() < 0.0001 || (radius - 3.2).abs() < 0.0001);
        assert!(p.to_array().iter().all(|v| v.abs() <= 100.0));
        for q in points.iter().skip(i + 1) {
            assert!(p.distance(*q) > 0.54, "{p:?} {q:?}");
        }
    }
    assert!(points[128].y > points[0].y);
}

#[test]
fn common_worker_counts_flank_coordinator_with_readable_labels() {
    // Direct workers, a deep chain, and independent missing parents must
    // all put the real stations in the first alternating row.
    for count in 1..=8 {
        for hierarchy in 0..3 {
            let nodes = (0..count)
                .map(|i| {
                    let parent = match hierarchy {
                        0 => "root".to_owned(),
                        1 if i == 0 => "root".to_owned(),
                        1 => format!("worker-{:02}", i - 1),
                        _ => format!("absent-{i:02}"),
                    };
                    node(&format!("worker-{i:02}"), &parent)
                })
                .collect();
            let mut scene = Scene::new();
            scene.update(Some("root"), &workspace(nodes), 0.02);
            let stations = scene.stations();
            assert_eq!(stations.len(), count);
            for (i, station) in stations.iter().enumerate() {
                let p = Vec3::from_array(station.position);
                assert_eq!(p, slot_position(i));
                // On the arc through the agent, as far from home as it is, about its height.
                assert!((p.x.hypot(p.z) - (-CENTER.z)).abs() < 0.0001);
                assert!(p.x.abs() > 1.6 && p.z < 0.0);
                assert!((1.1..2.1).contains(&p.y));
                for other in stations.iter().skip(i + 1) {
                    assert!(p.distance(Vec3::from_array(other.position)) >= 0.54);
                }
                // Conservative angular clearance: the coordinator's
                // normal conversational envelope is ~1 m wide in each
                // direction and ~0.42 m deep. Include the label's inner
                // edge, not just the worker anchor, from a viewer at zero.
                let inner_label_edge = p.x.abs() - 0.493;
                assert!(inner_label_edge / -(p.z + 0.1) > 1.0 / -(CENTER.z + 0.42));
            }
            for (i, a) in stations.iter().enumerate() {
                for b in stations.iter().skip(i + 1) {
                    // Current label bounds, projected from the origin;
                    // staggered heights separate adjacent columns too.
                    let interval = |s: &Station| {
                        let distance = -(s.position[2] + 0.1);
                        (
                            (s.position[0] - 0.48 - 3.0 * 0.0038) / distance,
                            (s.position[0] - 0.48 + 256.0 * 0.0038) / distance,
                        )
                    };
                    let (a0, a1) = interval(a);
                    let (b0, b1) = interval(b);
                    assert!(
                        a1 < b0 || b1 < a0 || (a.position[1] - b.position[1]).abs() > 0.13,
                        "{a:?} {b:?}"
                    );
                    // Distinct observer bearings, not world-X distance,
                    // determine clearance for the inward-facing signs.
                    let bearing = |s: &Station| s.position[0].atan2(-s.position[2]);
                    assert!(
                        (bearing(a) - bearing(b)).abs() > 0.3
                            || (a.position[1] - b.position[1]).abs() > 0.54
                    );
                }
            }
        }
    }
}

#[test]
fn arc_is_balanced_curved_and_fixed_in_world_while_view_turns() {
    let w = workspace(
        (0..16)
            .map(|i| node(&format!("w-{i:02}"), "root"))
            .collect(),
    );
    let mut scene = Scene::new();
    scene.update(Some("root"), &w, 0.0);
    let original = anchors(&scene);
    for column in 0..2 {
        let left = slot_position(column * 2);
        let right = slot_position(column * 2 + 1);
        assert!((left.x + right.x).abs() < 0.00001);
        assert_eq!(left.z, right.z);
        if column > 0 {
            let previous = slot_position((column - 1) * 2 + 1);
            assert!(right.x > previous.x && right.z > previous.z);
        }
    }
    // Sixteen workers, all on the band through the agent, 2.3 m from home, which holds eighteen.
    assert!(
        scene
            .workers
            .values()
            .all(|worker| { (worker.anchor.x.hypot(worker.anchor.z) - 2.3).abs() < 0.0001 })
    );
    // Rotation changes the view coordinates, not Scene's world positions.
    let world = Vec3::from_array(original["w-00"]);
    let view = glam::Quat::from_rotation_y(0.7).inverse() * world;
    assert!(view.distance(world) > 0.1);
    scene.update(Some("root"), &w, 0.02);
    assert_eq!(anchors(&scene), original);
}

#[test]
fn first_and_uncertain_discoveries_are_settled_not_launches() {
    let mut scene = Scene::new();
    let mut w = workspace(vec![node("historical", "root")]);
    scene.update(Some("root"), &w, 0.0);
    let worker = &scene.workers["historical"];
    assert_eq!(worker.age, ARRIVAL_SECONDS);
    assert!(
        worker
            .particles
            .iter()
            .all(|p| p.distance(worker.anchor) < 0.25)
    );
    assert_eq!(scene.stations()[0].opacity, 1.0);

    w.nodes.push(node("partial", "root"));
    scene.update(
        Some("root"),
        &Workspace {
            truncated: true,
            ..w.clone()
        },
        0.0,
    );
    assert_eq!(scene.workers["partial"].age, ARRIVAL_SECONDS);
    w.nodes.push(node("after-partial", "root"));
    scene.update(Some("root"), &w, 0.0);
    assert_eq!(scene.workers["after-partial"].age, ARRIVAL_SECONDS);
    scene.update(
        Some("root"),
        &Workspace {
            stale: true,
            ..w.clone()
        },
        0.0,
    );
    w.nodes.push(node("after-stale", "root"));
    scene.update(Some("root"), &w, 0.0);
    assert_eq!(scene.workers["after-stale"].age, ARRIVAL_SECONDS);
    w.nodes.push(node("new", "root"));
    scene.update(Some("root"), &w, 0.0);
    assert_eq!(scene.workers["new"].age, 0.0);
    assert!(scene.workers["new"].particles.iter().all(|p| *p == CENTER));
}

#[test]
fn bounded_actual_samples_drive_launch_and_return_with_center_fallback() {
    let mut scene = Scene::new();
    let mut w = Workspace::default();
    scene.update(Some("root"), &w, 0.0);
    let samples = [
        [f32::NAN, 1.0, -2.0],
        [0.2, 1.3, -2.2],
        [-0.4, 1.5, -2.5],
        [f32::INFINITY, 0.0, 0.0],
        [101.0, 1.4, -2.3],
    ];
    scene.set_coordinator_samples(&samples);
    assert_eq!(scene.coordinator_sample_count, 2);
    w.nodes.push(node("worker", "root"));
    scene.update(Some("root"), &w, 0.0);
    let worker = &scene.workers["worker"];
    assert!(
        worker
            .particles
            .iter()
            .all(|p| *p == Vec3::from_array(samples[1]) || *p == Vec3::from_array(samples[2]))
    );
    assert!(worker.particles.contains(&Vec3::from_array(samples[1])));
    assert!(worker.particles.contains(&Vec3::from_array(samples[2])));
    settle(&mut scene, &w);

    let destination = [0.3, 1.6, -2.4];
    scene.set_coordinator_samples(&[destination]);
    let before = scene.workers["worker"].particles;
    w.nodes.clear();
    scene.update(Some("root"), &w, 0.0);
    assert_eq!(scene.workers["worker"].particles, before);
    assert!(
        scene.workers["worker"]
            .return_end
            .iter()
            .all(|p| *p == Vec3::from_array(destination))
    );
    scene.set_coordinator_samples(&[]);
    for _ in 0..60 {
        scene.update(Some("root"), &w, 1.0 / 60.0);
    }
    let worker = &scene.workers["worker"];
    assert!(worker.retired.unwrap() >= 0.99);
    assert!(
        worker
            .particles
            .iter()
            .all(|p| p.distance(Vec3::from_array(destination)) < 0.4)
    );
    assert!(scene.cubes().iter().all(Cube::valid));
    for _ in 0..20 {
        scene.update(Some("root"), &w, 1.0 / 60.0);
    }
    assert!(!scene.workers.contains_key("worker"));
    w.nodes.push(node("fallback", "root"));
    scene.update(Some("root"), &w, 0.0);
    assert!(
        scene.workers["fallback"]
            .particles
            .iter()
            .all(|p| *p == CENTER)
    );
    w.nodes.clear();
    scene.update(Some("root"), &w, 0.0);
    assert!(
        scene.workers["fallback"]
            .return_end
            .iter()
            .all(|p| *p == CENTER)
    );

    let many = vec![CENTER.to_array(); 1000];
    scene.set_coordinator_samples(&many);
    assert_eq!(scene.coordinator_sample_count, MAX_COORDINATOR_SAMPLES);
}

#[test]
fn distant_finite_coordinator_samples_extend_flight_without_teleports() {
    let mut scene = Scene::new();
    let mut w = Workspace::default();
    scene.update(Some("root"), &w, 0.0);
    let sample = Vec3::new(20.0, 1.4, -2.3);
    scene.set_coordinator_samples(&[sample.to_array()]);
    w.nodes.push(node("distant", "root"));
    scene.update(Some("root"), &w, 0.0);
    assert_eq!(scene.workers["distant"].particles[0], sample);
    assert!(scene.workers["distant"].arrival_seconds > ARRIVAL_SECONDS);
    for _ in 0..200 {
        let before = scene.workers["distant"].particles;
        scene.update(Some("root"), &w, 0.05);
        assert!(
            before
                .iter()
                .zip(scene.workers["distant"].particles)
                .all(|(a, b)| a.distance(b) < 0.301)
        );
        assert!(scene.cubes().iter().all(Cube::valid));
    }
    let worker = &scene.workers["distant"];
    assert!(
        worker
            .particles
            .iter()
            .all(|p| p.distance(worker.anchor) < 0.25)
    );
    w.nodes.clear();
    scene.update(Some("root"), &w, 0.0);
    assert!(scene.workers["distant"].departure_seconds > DEPARTURE_SECONDS);
    for _ in 0..200 {
        let Some(worker) = scene.workers.get("distant") else {
            break;
        };
        let before = worker.particles;
        scene.update(Some("root"), &w, 0.05);
        if let Some(worker) = scene.workers.get("distant") {
            assert!(
                before
                    .iter()
                    .zip(worker.particles)
                    .all(|(a, b)| a.distance(b) < 0.301)
            );
            assert!(scene.cubes().iter().all(Cube::valid));
        }
    }
    assert!(!scene.workers.contains_key("distant"));
}

#[test]
fn return_is_continuous_pauses_on_uncertainty_and_reappearance_resumes_in_place() {
    let mut scene = Scene::new();
    let mut w = workspace(vec![node("gone", "root"), node("healthy", "root")]);
    settle(&mut scene, &w);
    let healthy_anchor = scene.workers["healthy"].anchor;
    let start = scene.workers["gone"].particles;
    w.nodes.retain(|node| node.id != "gone");
    scene.update(Some("root"), &w, 0.0);
    assert_eq!(scene.workers["gone"].particles, start);
    for _ in 0..30 {
        let before = scene.workers["gone"].particles;
        scene.update(Some("root"), &w, 1.0 / 60.0);
        assert!(
            before
                .iter()
                .zip(scene.workers["gone"].particles)
                .all(|(a, b)| a.distance(b) < 0.1)
        );
        assert_eq!(scene.workers["healthy"].anchor, healthy_anchor);
        assert!(scene.workers["healthy"].retired.is_none());
    }
    let midway = scene.workers["gone"].particles;
    let distance =
        |points: &[Vec3; PARTICLES]| points.iter().map(|p| p.distance(CENTER)).sum::<f32>();
    assert!(distance(&midway) < distance(&start));
    for partial in [
        Workspace {
            stale: true,
            ..w.clone()
        },
        Workspace {
            truncated: true,
            ..w.clone()
        },
        Workspace {
            error: Some("offline".into()),
            ..w.clone()
        },
    ] {
        scene.update(Some("root"), &partial, 0.05);
        assert_eq!(scene.workers["gone"].particles, midway);
        assert!(scene.workers["healthy"].retired.is_none());
    }
    w.nodes.push(node("gone", "root"));
    scene.update(Some("root"), &w, 0.0);
    assert_eq!(scene.workers["gone"].particles, midway);
    assert!(scene.workers["gone"].retired.is_none());
    scene.update(Some("root"), &w, 1.0 / 60.0);
    assert!(
        midway
            .iter()
            .zip(scene.workers["gone"].particles)
            .all(|(a, b)| a.distance(b) < 0.101)
    );
    settle(&mut scene, &w);
    assert!(
        scene.workers["gone"]
            .particles
            .iter()
            .all(|p| p.distance(scene.workers["gone"].anchor) < 0.25)
    );
}

#[test]
fn later_tiers_favor_nearby_parents_without_skipping_tiers() {
    let mut nodes: Vec<_> = (0..8)
        .map(|i| node(&format!("worker-{i:02}"), "root"))
        .collect();
    nodes.push(node("child", "worker-06"));
    let mut scene = Scene::new();
    scene.update(Some("root"), &workspace(nodes), 0.02);
    let parent = scene.workers["worker-06"].anchor;
    let child = &scene.workers["child"];
    assert_eq!(child.origin, parent);
    assert!((8..16).contains(&child.slot));
    assert!((8..16).all(|slot| {
        child.anchor.distance_squared(parent)
            <= slot_position(slot).distance_squared(parent) + 0.0001
    }));
}

#[test]
fn over_sixty_four_workers_are_rejected_without_displacing_live_stations() {
    let mut scene = Scene::new();
    let mut w = workspace(
        (0..64)
            .map(|i| node(&format!("w-{i:02}"), "root"))
            .collect(),
    );
    scene.update(Some("root"), &w, 0.02);
    let before = anchors(&scene);
    w.nodes.push(node("overflow", "root"));
    assert!(!w.valid());
    scene.update(Some("root"), &w, 0.02);
    assert_eq!(anchors(&scene), before);
    assert!(scene.stations().iter().all(|s| s.state == State::Unknown));
}

#[test]
fn ordinary_state_changes_never_teleport_particles_or_stations() {
    let mut w = workspace(vec![node("a", "root")]);
    let mut scene = Scene::new();
    settle(&mut scene, &w);
    let anchor = scene.stations()[0].position;
    for state in [
        State::Waiting,
        State::Completed,
        State::Error,
        State::Unknown,
        State::Running,
    ] {
        let before = scene.workers["a"].particles;
        w.nodes[0].state = state;
        scene.update(Some("root"), &w, 1.0 / 72.0);
        for (a, b) in before.iter().zip(scene.workers["a"].particles) {
            assert!(a.distance(b) < 0.035);
        }
        assert_eq!(scene.stations()[0].position, anchor);
    }
    assert_eq!(scene.stations()[0].state, State::Running);
}

#[test]
fn historical_terminal_forms_are_distinct_deterministic_and_gently_ambient() {
    for state in [State::Completed, State::Error] {
        let w = workspace(vec![Node {
            state,
            ..node("a", "root")
        }]);
        let mut first = Scene::new();
        let mut second = Scene::new();
        first.update(Some("root"), &w, 0.0);
        second.update(Some("root"), &w, 0.0);
        let worker = &first.workers["a"];
        let positions = worker.particles;
        assert!(worker.terminal_motion.is_none());
        assert!(!worker.flying);
        assert_eq!(positions, second.workers["a"].particles);
        for (i, p) in positions.iter().enumerate() {
            assert_eq!(*p, terminal_position(worker.anchor, state, i));
            for other in positions.iter().skip(i + 1) {
                assert!(p.distance(*other) > 0.059, "{p:?} {other:?}");
            }
        }
        for _ in 0..180 {
            let before = first.workers["a"].particles;
            first.update(Some("root"), &w, 1.0 / 60.0);
            second.update(Some("root"), &w, 1.0 / 60.0);
            assert_eq!(first.workers["a"].particles, second.workers["a"].particles);
            assert!(
                before
                    .iter()
                    .zip(first.workers["a"].particles)
                    .all(|(a, b)| a.distance(b) < 0.0004)
            );
            assert_terminal_form(&first.workers["a"]);
            assert_eq!(
                first.cubes().len(),
                if state == State::Completed {
                    1
                } else {
                    PARTICLES
                }
            );
        }
        assert_ne!(first.workers["a"].particles, positions);
        assert!(first.cubes().iter().all(Cube::valid));
    }

    // The error form has an open split in station-tangential coordinates.
    let anchor = slot_position(0);
    // A worker placed straight above or below home faces home's front.
    let radial = Vec3::new(anchor.x, 0.0, anchor.z)
        .try_normalize()
        .unwrap_or(Vec3::NEG_Z);
    let tangent = Vec3::new(-radial.z, 0.0, radial.x);
    let local_x = |state, i| (terminal_position(anchor, state, i) - anchor).dot(tangent);
    assert!((0..PARTICLES).all(|i| local_x(State::Error, i).abs() > 0.06));
    assert!((0..12).all(|i| local_x(State::Error, i) < 0.0));
    assert!((12..24).all(|i| local_x(State::Error, i) > 0.0));
}

#[test]
fn terminal_bounce_occurs_once_then_resumes_and_replays_on_new_transition() {
    let mut scene = Scene::new();
    let mut w = workspace(vec![node("a", "root")]);
    settle(&mut scene, &w);
    let initial = scene.workers["a"].particles;
    w.nodes[0].state = State::Completed;
    scene.update(Some("root"), &w, 0.0);
    assert_eq!(scene.workers["a"].particles, initial);
    assert!(scene.workers["a"].terminal_motion.unwrap().bounce);
    for _ in 0..12 {
        let before = scene.workers["a"].particles;
        scene.update(Some("root"), &w, 1.0 / 60.0);
        assert!(
            before
                .iter()
                .zip(scene.workers["a"].particles)
                .all(|(a, b)| a.distance(b) < 0.035)
        );
    }
    let worker = &scene.workers["a"];
    let motion = worker.terminal_motion.unwrap();
    let t = motion.elapsed / motion.duration;
    let baseline = worker.flight_start[0].lerp(
        terminal_position(worker.anchor, State::Completed, 0),
        smooth(t),
    );
    assert!(worker.particles[0].y - baseline.y > 0.03);
    settle(&mut scene, &w);
    assert!(scene.workers["a"].terminal_motion.is_none());
    for _ in 0..120 {
        let before = scene.workers["a"].particles;
        scene.update(Some("root"), &w, 1.0 / 60.0);
        assert!(
            before
                .iter()
                .zip(scene.workers["a"].particles)
                .all(|(a, b)| a.distance(b) < 0.0004)
        );
        assert!(scene.workers["a"].terminal_motion.is_none());
    }

    let settled = scene.workers["a"].particles;
    w.nodes[0].state = State::Running;
    scene.update(Some("root"), &w, 0.0);
    assert_eq!(scene.workers["a"].particles, settled);
    settle(&mut scene, &w);
    assert_ne!(scene.workers["a"].particles, settled);
    w.nodes[0].state = State::Completed;
    scene.update(Some("root"), &w, 0.0);
    assert!(scene.workers["a"].terminal_motion.unwrap().bounce);
}

#[test]
fn newly_observed_terminal_worker_arrives_without_a_fabricated_bounce() {
    for state in [State::Completed, State::Error] {
        let mut scene = Scene::new();
        scene.update(Some("root"), &Workspace::default(), 0.0);
        let w = workspace(vec![Node {
            state,
            ..node("a", "root")
        }]);
        scene.update(Some("root"), &w, 0.0);
        assert!(scene.workers["a"].flying);
        assert!(scene.workers["a"].terminal_motion.is_none());
        assert!(scene.workers["a"].particles.iter().all(|p| *p == CENTER));
        for _ in 0..180 {
            let before = scene.workers["a"].particles;
            let before_scales = scene.workers["a"].particle_scales;
            scene.update(Some("root"), &w, 1.0 / 60.0);
            assert!(
                before
                    .iter()
                    .zip(scene.workers["a"].particles)
                    .all(|(a, b)| a.distance(b) <= 0.101)
            );
            assert!(scene.workers["a"].terminal_motion.is_none());
            assert!(
                before_scales
                    .iter()
                    .zip(scene.workers["a"].particle_scales)
                    .all(|(a, b)| a.distance(b) < 0.025)
            );
        }
        let worker = &scene.workers["a"];
        assert!(!worker.flying);
        assert_terminal_form(worker);
        for _ in 0..60 {
            let before = scene.workers["a"].particles;
            scene.update(Some("root"), &w, 1.0 / 60.0);
            assert!(
                before
                    .iter()
                    .zip(scene.workers["a"].particles)
                    .all(|(a, b)| a.distance(b) < 0.0004)
            );
            assert_terminal_form(&scene.workers["a"]);
        }
    }
}

#[test]
fn nonterminal_running_waiting_and_unknown_keep_moving() {
    for state in [State::Running, State::Waiting, State::Unknown] {
        let mut scene = Scene::new();
        let w = workspace(vec![Node {
            state,
            ..node("a", "root")
        }]);
        settle(&mut scene, &w);
        let before = scene.workers["a"].particles;
        for _ in 0..60 {
            scene.update(Some("root"), &w, 1.0 / 60.0);
        }
        assert_ne!(scene.workers["a"].particles, before);
    }
}

#[test]
fn stale_partial_unknown_and_recovery_do_not_replay_terminal_bounce() {
    for state in [State::Completed, State::Error] {
        let mut scene = Scene::new();
        let mut w = workspace(vec![node("a", "root")]);
        settle(&mut scene, &w);
        w.nodes[0].state = state;
        scene.update(Some("root"), &w, 0.02);
        assert!(scene.workers["a"].terminal_motion.unwrap().bounce);
        let elapsed = scene.workers["a"].terminal_motion.unwrap().elapsed;
        scene.update(
            Some("root"),
            &Workspace {
                stale: true,
                ..w.clone()
            },
            0.02,
        );
        assert!(scene.workers["a"].terminal_motion.unwrap().elapsed > elapsed);
        scene.update(Some("root"), &w, 0.02);
        assert!(scene.workers["a"].terminal_motion.unwrap().elapsed > elapsed + 0.02);
        settle(&mut scene, &w);
        for uncertain in [
            Workspace {
                stale: true,
                ..w.clone()
            },
            Workspace {
                truncated: true,
                ..Default::default()
            },
            Workspace {
                error: Some("offline".into()),
                ..Default::default()
            },
        ] {
            for _ in 0..20 {
                scene.update(Some("root"), &uncertain, 0.05);
            }
            assert_terminal_form(&scene.workers["a"]);
            assert!(scene.workers["a"].terminal_motion.is_none());
            scene.update(Some("root"), &w, 0.02);
            assert_terminal_form(&scene.workers["a"]);
            assert!(scene.workers["a"].terminal_motion.is_none());
        }
        let unknown = workspace(vec![Node {
            state: State::Unknown,
            ..node("a", "root")
        }]);
        let positions = scene.workers["a"].particles;
        scene.update(Some("root"), &unknown, 0.0);
        assert_eq!(scene.workers["a"].particles, positions);
        for _ in 0..60 {
            let before = scene.workers["a"].particles;
            scene.update(Some("root"), &unknown, 1.0 / 60.0);
            assert!(
                before
                    .iter()
                    .zip(scene.workers["a"].particles)
                    .all(|(a, b)| a.distance(b) <= 0.101)
            );
        }
        assert_ne!(scene.workers["a"].particles, positions);
        let before = scene.workers["a"].particles;
        scene.update(Some("root"), &w, 0.0);
        assert_eq!(scene.workers["a"].particles, before);
        assert!(!scene.workers["a"].terminal_motion.unwrap().bounce);
        settle(&mut scene, &w);
        assert_terminal_form(&scene.workers["a"]);
    }

    let mut scene = Scene::new();
    let mut w = workspace(vec![node("a", "root")]);
    settle(&mut scene, &w);
    scene.update(
        Some("root"),
        &Workspace {
            stale: true,
            ..w.clone()
        },
        0.02,
    );
    w.nodes[0].state = State::Completed;
    scene.update(Some("root"), &w, 0.0);
    assert!(!scene.workers["a"].terminal_motion.unwrap().bounce);
    settle(&mut scene, &w);
    assert!(scene.workers["a"].terminal_motion.is_none());
}

#[test]
fn status_changes_mid_flight_and_terminal_return_never_teleport() {
    for state in [State::Completed, State::Error] {
        let mut scene = Scene::new();
        let mut w = workspace(vec![node("a", "root")]);
        scene.update(Some("root"), &Workspace::default(), 0.0);
        scene.update(Some("root"), &w, 0.0);
        for _ in 0..25 {
            scene.update(Some("root"), &w, 1.0 / 60.0);
        }
        let before = scene.workers["a"].particles;
        let before_scales = scene.workers["a"].particle_scales;
        w.nodes[0].state = state;
        scene.update(Some("root"), &w, 0.0);
        assert_eq!(scene.workers["a"].particles, before);
        assert_eq!(scene.workers["a"].particle_scales, before_scales);
        assert!(scene.workers["a"].terminal_motion.unwrap().bounce);
        for _ in 0..180 {
            let before = scene.workers["a"].particles;
            let before_scales = scene.workers["a"].particle_scales;
            scene.update(Some("root"), &w, 1.0 / 60.0);
            assert!(
                before
                    .iter()
                    .zip(scene.workers["a"].particles)
                    .all(|(a, b)| a.distance(b) <= 0.101)
            );
            assert!(scene.cubes().iter().all(Cube::valid));
            assert!(
                before_scales
                    .iter()
                    .zip(scene.workers["a"].particle_scales)
                    .all(|(a, b)| a.distance(b) < 0.025)
            );
        }
        let shape = scene.workers["a"].particles;
        assert!(scene.workers["a"].terminal_motion.is_none());
        let empty = Workspace::default();
        scene.update(Some("root"), &empty, 0.0);
        assert_eq!(scene.workers["a"].particles, shape);
        for _ in 0..30 {
            let before = scene.workers["a"].particles;
            scene.update(Some("root"), &empty, 1.0 / 60.0);
            assert!(
                before
                    .iter()
                    .zip(scene.workers["a"].particles)
                    .all(|(a, b)| a.distance(b) <= 0.101)
            );
        }
        let returning = scene.workers["a"].particles;
        scene.update(Some("root"), &w, 0.0);
        assert_eq!(scene.workers["a"].particles, returning);
        assert!(!scene.workers["a"].terminal_motion.unwrap().bounce);
        settle(&mut scene, &w);
        assert_terminal_form(&scene.workers["a"]);
    }
}

#[test]
fn nested_workers_depart_from_actual_parent_even_when_missing() {
    let mut scene = Scene::new();
    let w = workspace(vec![node("child", "parent"), node("parent", "root")]);
    scene.update(Some("root"), &w, 0.0);
    assert_eq!(scene.workers["parent"].origin, CENTER);
    assert_eq!(
        scene.workers["child"].origin,
        scene.workers["parent"].anchor
    );
    let mut missing = Scene::new();
    missing.update(
        Some("root"),
        &workspace(vec![node("child", "archived")]),
        0.02,
    );
    assert_eq!(
        missing.workers["child"].origin,
        slot_position(missing.slots["archived"])
    );
    assert_ne!(missing.workers["child"].origin, CENTER);
    assert_eq!(missing.stations().len(), 1);
}

#[test]
fn a_nested_worker_returns_to_its_parent_unless_the_parent_leaves_too() {
    let mut scene = Scene::new();
    let coordinator = Vec3::new(0.3, 1.6, -2.4);
    scene.set_coordinator_samples(&[coordinator.to_array()]);
    let mut w = workspace(vec![
        node("lead", "root"),
        node("helper", "lead"),
        node("second", "lead"),
    ]);
    settle(&mut scene, &w);
    let lead = scene.workers["lead"].anchor;

    // Deleting the helper sends it back to the lead's station, not to the coordinator.
    w.nodes.retain(|node| node.id != "helper");
    scene.update(Some("root"), &w, 0.0);
    assert!(
        scene.workers["helper"]
            .return_end
            .iter()
            .all(|p| *p == lead)
    );
    let departure = scene.workers["helper"].departure_seconds;
    let mut elapsed = 0.0;
    while elapsed + 1.0 / 60.0 < departure {
        scene.update(Some("root"), &w, 1.0 / 60.0);
        elapsed += 1.0 / 60.0;
    }
    let helper = &scene.workers["helper"];
    assert!(helper.particles.iter().all(|p| p.distance(lead) < 0.05));

    // A parent leaving with its child is no destination: both return to the coordinator.
    w.nodes.clear();
    scene.update(Some("root"), &w, 0.0);
    for id in ["lead", "second"] {
        assert!(
            scene.workers[id]
                .return_end
                .iter()
                .all(|p| *p == coordinator),
            "{id}"
        );
    }
}

/// A placement that stands each worker on a line ahead of its origin, by its slot, a metre further for each worker
/// already standing, and records what it was told.
#[derive(Default)]
struct Line {
    told: std::sync::Mutex<Vec<Told>>,
}

/// What a placement was told: the worker, its parent, its state, its slot and the workers already standing.
type Told = (String, Option<String>, State, usize, Vec<String>);

impl crate::anchors::WorkerPlacement for Line {
    fn place(&self, origin: Anchor, worker: &Placing<'_>) -> Vec3 {
        self.told.lock().unwrap().push((
            worker.id.to_owned(),
            worker.parent.map(str::to_owned),
            worker.state,
            worker.slot,
            worker
                .workers
                .iter()
                .map(|placed| placed.id.to_owned())
                .collect(),
        ));
        origin.place(Vec3::new(
            worker.slot as f32,
            1.5,
            -2.0 - worker.workers.len() as f32,
        ))
    }
}

fn turned(position: Vec3) -> Anchor {
    Anchor {
        position,
        orientation: Quat::from_rotation_y(0.4),
    }
}

#[test]
fn the_default_arc_stands_at_the_workers_anchor_turned_with_it() {
    let origin = turned(Vec3::new(1.0, 0.0, -6.0));
    let mut scene = Scene::new();
    // Arranged the same way as by default, nothing changes.
    scene.arrange(Anchor::AGENT, crate::anchors::EYES, None, None);
    let w = workspace(vec![node("a", "root"), node("b", "root")]);
    scene.update(Some("root"), &w, 0.0);
    for (id, position) in anchors(&scene) {
        assert_eq!(position, slot_position(scene.slots[&id]).to_array());
    }
    // Moved and turned, every live worker is placed again, about the anchor, glides there, its sign with it, and
    // points at it there.
    scene.arrange(origin, crate::anchors::EYES, None, None);
    scene.update(Some("root"), &w, 0.0);
    let before = anchors(&scene);
    for (id, position) in &before {
        assert_eq!(*position, slot_position(scene.slots[id]).to_array());
    }
    scene.update(Some("root"), &w, 0.1);
    for (id, position) in anchors(&scene) {
        let expected = crate::anchors::HomeArc::at(origin, crate::anchors::EYES, scene.slots[&id]);
        let from = Vec3::from_array(before[&id]);
        let moved = Vec3::from_array(position).distance(from);
        // Easing out, no faster than its greatest speed.
        assert!(moved > 0.01 && moved <= GLIDE_SPEED * 0.1 + 1e-4, "{moved}");
        assert!(Vec3::from_array(position).distance(expected) < from.distance(expected));
    }
    for _ in 0..400 {
        scene.update(Some("root"), &w, 1.0 / 60.0);
    }
    for (id, position) in anchors(&scene) {
        let expected = crate::anchors::HomeArc::at(origin, crate::anchors::EYES, scene.slots[&id]);
        assert_eq!(Vec3::from_array(position), expected);
        assert!(scene.targets().iter().any(|target| target.key == id
            && target.shape
                == Shape::Sphere {
                    center: expected,
                    radius: POINTING_RADIUS
                }));
    }
    // A worker arriving afterwards stands on the moved arc too.
    let more = workspace(vec![
        node("a", "root"),
        node("b", "root"),
        node("c", "root"),
    ]);
    scene.update(Some("root"), &more, 0.0);
    assert_eq!(
        scene.workers["c"].anchor,
        crate::anchors::HomeArc::at(origin, crate::anchors::EYES, scene.slots["c"])
    );
}

#[test]
fn a_placement_places_arrivals_and_never_moves_live_workers_under_churn() {
    let line = std::sync::Arc::new(Line::default());
    let placement: Placement = line.clone();
    let origin = Anchor {
        position: Vec3::new(0.0, 0.0, -1.0),
        orientation: Quat::IDENTITY,
    };
    let mut scene = Scene::new();
    scene.arrange(origin, crate::anchors::EYES, None, Some(placement.clone()));
    let mut w = workspace(vec![node("a", "root"), node("b", "a")]);
    settle(&mut scene, &w);
    // Each was told of the workers already standing, its parent, but not the root, its state and its slot.
    let told = line.told.lock().unwrap().clone();
    assert_eq!(told.len(), 2);
    assert_eq!(told[0].0, "a");
    assert_eq!((told[0].1.as_deref(), told[0].2), (None, State::Running));
    assert_eq!(told[1].0, "b");
    assert_eq!(told[1].1.as_deref(), Some("a"));
    assert_eq!(told[1].4, ["a"]);
    let a = scene.workers["a"].anchor;
    assert_eq!(a, Vec3::new(told[0].3 as f32, 1.5, -3.0));
    let before = anchors(&scene);
    // Titles, states, order and membership change; live workers stay where they were placed, and the same
    // arrangement given again places nothing.
    w.nodes.reverse();
    w.nodes[0].title = "renamed".into();
    w.nodes[1].state = State::Waiting;
    w.nodes.push(node("c", "root"));
    for _ in 0..3 {
        scene.arrange(origin, crate::anchors::EYES, None, Some(placement.clone()));
        scene.update(Some("root"), &w, 1.0 / 60.0);
    }
    w.nodes.retain(|node| node.id != "b");
    scene.arrange(origin, crate::anchors::EYES, None, Some(placement.clone()));
    scene.update(Some("root"), &w, 1.0 / 60.0);
    let after = anchors(&scene);
    assert_eq!(after["a"], before["a"]);
    // The ray points at each placed worker where it stands, through the churn.
    for (id, position) in &after {
        let center = Vec3::from_array(*position);
        if id == "b" {
            continue;
        }
        assert!(scene.targets().iter().any(|target| &target.key == id
            && target.shape
                == Shape::Sphere {
                    center,
                    radius: POINTING_RADIUS
                }));
        let ray = Ray::new(Vec3::new(0.0, 1.6, 0.0), center - Vec3::new(0.0, 1.6, 0.0));
        assert_eq!(scene.point(ray), Some(id.as_str()));
    }
    assert_eq!(line.told.lock().unwrap().len(), 3);
    assert_eq!(scene.workers["c"].anchor.z, -1.0 - 2.0 - 2.0);
    // The worker leaving returns to its parent where it was placed.
    assert!(scene.workers["b"].return_end.iter().all(|p| *p == a));
    // Another arrangement places every live worker again, in the order of their slots.
    scene.arrange(Anchor::HOME, crate::anchors::EYES, None, Some(placement));
    let replaced: Vec<String> = line.told.lock().unwrap()[3..]
        .iter()
        .map(|told| told.0.clone())
        .collect();
    assert_eq!(replaced, ["a", "c"]);
    assert_eq!(
        scene.workers["c"].place,
        Vec3::new(scene.slots["c"] as f32, 1.5, -3.0)
    );
    // Back to the default, the arc again.
    scene.arrange(Anchor::AGENT, crate::anchors::EYES, None, None);
    assert_eq!(scene.workers["a"].place, slot_position(scene.slots["a"]));
}

#[test]
fn a_child_arriving_with_its_parent_launches_from_where_the_parent_is_placed() {
    let placement: Placement = std::sync::Arc::new(Line::default());
    let mut scene = Scene::new();
    scene.arrange(Anchor::AGENT, crate::anchors::EYES, None, Some(placement));
    // Trusted twice, so that the next arrivals launch.
    let empty = workspace(Vec::new());
    scene.update(Some("root"), &empty, 0.0);
    scene.update(Some("root"), &empty, 0.0);
    // The child's identity sorts before its parent's, and its slot after.
    let w = workspace(vec![node("zz", "root"), node("aa", "zz")]);
    scene.update(Some("root"), &w, 0.0);
    assert!(scene.workers["aa"].flying);
    assert_eq!(scene.workers["aa"].origin, scene.workers["zz"].anchor);
    assert!(
        scene.workers["aa"]
            .particles
            .iter()
            .all(|p| *p == scene.workers["zz"].anchor)
    );
}

/// A placement that records the slots of the workers it is told stand already, and stands each worker as `Line`.
#[derive(Default)]
struct Slots(std::sync::Mutex<Vec<Vec<usize>>>);

impl crate::anchors::WorkerPlacement for Slots {
    fn place(&self, origin: Anchor, worker: &Placing<'_>) -> Vec3 {
        self.0
            .lock()
            .unwrap()
            .push(worker.workers.iter().map(|placed| placed.slot).collect());
        origin.place(Vec3::new(worker.slot as f32, 1.5, -2.0))
    }
}

#[test]
fn a_placement_is_told_of_the_workers_standing_in_the_order_of_their_slots_on_both_paths() {
    let slots = std::sync::Arc::new(Slots::default());
    let placement: Placement = slots.clone();
    let mut scene = Scene::new();
    scene.arrange(
        Anchor::AGENT,
        crate::anchors::EYES,
        None,
        Some(placement.clone()),
    );
    // Identities that sort against their slots: "z" takes slot 0, "a" slot 1, "m" slot 2.
    let first = workspace(vec![node("z", "root"), node("a", "z")]);
    scene.update(Some("root"), &first, 0.0);
    let mut second = workspace(vec![node("z", "root"), node("a", "z"), node("m", "a")]);
    second.nodes.push(node("b", "root"));
    scene.update(Some("root"), &second, 0.0);
    let told = slots.0.lock().unwrap().clone();
    for workers in &told {
        assert!(workers.is_sorted(), "{told:?}");
    }
    // Arriving: those standing, in slot order, with each just placed among them.
    assert_eq!(told[3].len(), 3);
    // Placed again: the same order.
    scene.arrange(
        turned(Vec3::ZERO),
        crate::anchors::EYES,
        None,
        Some(placement),
    );
    let again = slots.0.lock().unwrap()[4..].to_vec();
    assert_eq!(again.last().unwrap().len(), 3);
    for workers in &again {
        assert!(workers.is_sorted(), "{again:?}");
    }
}

/// A placement that panics, counting how often it is called.
#[derive(Default)]
struct Panics(std::sync::atomic::AtomicUsize);

impl crate::anchors::WorkerPlacement for Panics {
    fn place(&self, _origin: Anchor, worker: &Placing<'_>) -> Vec3 {
        self.0.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        panic!("no place for {}", worker.id)
    }
}

#[test]
fn a_placement_that_panics_is_never_called_again_and_its_workers_stand_on_the_arc() {
    crate::test_support::log_everything();
    use std::sync::atomic::Ordering;
    // Arriving: two workers at once, then one more.
    let panics = std::sync::Arc::new(Panics::default());
    let placement: Placement = panics.clone();
    let mut scene = Scene::new();
    scene.arrange(
        Anchor::AGENT,
        crate::anchors::EYES,
        None,
        Some(placement.clone()),
    );
    let w = workspace(vec![node("a", "root"), node("b", "root")]);
    scene.update(Some("root"), &w, 0.0);
    assert_eq!(panics.0.load(Ordering::Relaxed), 1);
    let more = workspace(vec![
        node("a", "root"),
        node("b", "root"),
        node("c", "root"),
    ]);
    for _ in 0..3 {
        scene.arrange(
            Anchor::AGENT,
            crate::anchors::EYES,
            None,
            Some(placement.clone()),
        );
        scene.update(Some("root"), &more, 0.0);
    }
    // Moved elsewhere with it, still not called.
    let away = turned(Vec3::new(0.0, 0.0, -3.0));
    scene.arrange(away, crate::anchors::EYES, None, Some(placement.clone()));
    assert_eq!(panics.0.load(Ordering::Relaxed), 1);
    for id in ["a", "b", "c"] {
        assert_eq!(
            scene.workers[id].place,
            crate::anchors::HomeArc::at(away, crate::anchors::EYES, scene.slots[id])
        );
    }
    // Placing again: the first call panics, and the rest stand on the arc.
    let again = std::sync::Arc::new(Panics::default());
    let fresh: Placement = again.clone();
    scene.arrange(Anchor::AGENT, crate::anchors::EYES, None, Some(fresh));
    assert_eq!(again.0.load(Ordering::Relaxed), 1);
    for id in ["a", "b", "c"] {
        assert_eq!(scene.workers[id].anchor, slot_position(scene.slots[id]));
    }
    // Neither is called again.
    scene.arrange(away, crate::anchors::EYES, None, Some(placement));
    assert_eq!(panics.0.load(Ordering::Relaxed), 1);
    assert_eq!(scene.poisoned.len(), 2);
}

#[test]
fn a_child_arriving_later_launches_from_where_its_parent_was_placed() {
    let placement: Placement = std::sync::Arc::new(Line::default());
    let mut scene = Scene::new();
    scene.arrange(Anchor::AGENT, crate::anchors::EYES, None, Some(placement));
    let parent = workspace(vec![node("lead", "root")]);
    scene.update(Some("root"), &parent, 0.0);
    scene.update(Some("root"), &parent, 0.0);
    let lead = scene.workers["lead"].anchor;
    assert_ne!(lead, slot_position(scene.slots["lead"]));
    let both = workspace(vec![node("lead", "root"), node("helper", "lead")]);
    scene.update(Some("root"), &both, 0.0);
    assert!(scene.workers["helper"].flying);
    assert_eq!(scene.workers["helper"].origin, lead);
    assert!(scene.workers["helper"].particles.iter().all(|p| *p == lead));
}

#[test]
fn after_a_rearrangement_workers_glide_to_their_new_places_at_a_bounded_speed() {
    let mut scene = Scene::new();
    let w = workspace(vec![node("a", "root"), node("b", "root")]);
    settle(&mut scene, &w);
    let away = Anchor {
        position: Vec3::new(0.0, 0.0, -6.0),
        orientation: Quat::IDENTITY,
    };
    scene.arrange(away, crate::anchors::EYES, None, None);
    const DT: f32 = 1.0 / 60.0;
    let mut previous: Vec<Vec3> = scene
        .cubes()
        .iter()
        .map(|cube| Vec3::from_array(cube.position))
        .collect();
    for _ in 0..300 {
        scene.update(Some("root"), &w, DT);
        let now: Vec<Vec3> = scene
            .cubes()
            .iter()
            .map(|cube| Vec3::from_array(cube.position))
            .collect();
        for (before, after) in previous.iter().zip(&now) {
            // The particles' bounded speed, 6 m/s, and a little for the flock's own motion.
            assert!(before.distance(*after) <= 6.0 * DT + 0.01);
        }
        previous = now;
    }
    // Arrived, each flock is about its new place.
    for id in ["a", "b"] {
        let worker = &scene.workers[id];
        let centre = worker.particles.iter().copied().sum::<Vec3>() / PARTICLES as f32;
        assert!(
            centre.distance(crate::anchors::HomeArc::at(
                away,
                crate::anchors::EYES,
                worker.slot
            )) < 0.3,
            "{id}"
        );
    }
}

#[test]
fn a_placement_standing_a_worker_out_of_bounds_or_above_home_stays_finite() {
    struct At(Vec3);
    impl crate::anchors::WorkerPlacement for At {
        fn place(&self, _origin: Anchor, _worker: &Placing<'_>) -> Vec3 {
            self.0
        }
    }
    let mut scene = Scene::new();
    scene.arrange(
        Anchor::AGENT,
        crate::anchors::EYES,
        None,
        Some(std::sync::Arc::new(At(Vec3::splat(f32::NAN)))),
    );
    let w = workspace(vec![node("a", "root")]);
    scene.update(Some("root"), &w, 0.0);
    assert_eq!(scene.workers["a"].anchor, slot_position(scene.slots["a"]));
    // Straight above home, its terminal forms still face somewhere.
    let mut above = Scene::new();
    above.arrange(
        Anchor::AGENT,
        crate::anchors::EYES,
        None,
        Some(std::sync::Arc::new(At(Vec3::new(0.0, 2.0, 0.0)))),
    );
    let mut done = node("a", "root");
    done.state = State::Error;
    settle(&mut above, &workspace(vec![done]));
    assert!(above.cubes().iter().all(Cube::valid));
    assert_eq!(above.stations()[0].position, [0.0, 2.0, 0.0]);
}

#[test]
fn a_partial_projection_keeps_observed_children_of_a_parent_it_omits() {
    let mut scene = Scene::new();
    let complete = workspace(vec![node("parent", "root"), node("child", "parent")]);
    settle(&mut scene, &complete);

    // The snapshot budget left out the parent but kept its child, which completed and started a worker of its
    // own. Both are observed; the omitted parent is only unknown.
    let partial = Workspace {
        nodes: vec![
            Node {
                state: State::Completed,
                ..node("child", "parent")
            },
            node("grandchild", "child"),
            node("stranger", "elsewhere"),
        ],
        truncated: true,
        ..Default::default()
    };
    scene.update(Some("root"), &partial, 0.02);
    assert_eq!(scene.workers["parent"].state, State::Unknown);
    assert_eq!(scene.workers["child"].state, State::Completed);
    assert_eq!(scene.workers["grandchild"].state, State::Running);
    assert_eq!(
        scene.workers["grandchild"].parent_id.as_deref(),
        Some("child")
    );
    // A worker under a parent this space never showed is not taken into it.
    assert!(!scene.workers.contains_key("stranger"));

    let recovered = workspace(vec![
        node("parent", "root"),
        Node {
            state: State::Completed,
            ..node("child", "parent")
        },
        node("grandchild", "child"),
    ]);
    scene.update(Some("root"), &recovered, 0.02);
    let states: BTreeMap<_, _> = scene
        .stations()
        .iter()
        .map(|station| (station.id.as_str(), station.state))
        .collect();
    assert_eq!(
        states,
        BTreeMap::from([
            ("child", State::Completed),
            ("grandchild", State::Running),
            ("parent", State::Running),
        ])
    );
    assert!(
        scene
            .workers
            .values()
            .all(|worker| worker.retired.is_none())
    );
}

#[test]
fn new_child_without_an_observed_parent_does_not_launch_from_a_fictional_station() {
    let mut scene = Scene::new();
    scene.update(Some("root"), &Workspace::default(), 0.0);
    let w = workspace(vec![node("child", "missing")]);
    scene.update(Some("root"), &w, 0.0);
    let child = &scene.workers["child"];
    assert_eq!(child.origin, slot_position(scene.slots["missing"]));
    assert_eq!(child.age, child.arrival_seconds);
    assert!(
        child
            .particles
            .iter()
            .all(|p| p.distance(child.anchor) < 0.25)
    );
    assert_eq!(scene.stations().len(), 1);
}

#[test]
fn snapshots_and_changes_render_only_flocks_not_inferred_traffic() {
    let mut scene = Scene::new();
    let mut w = workspace(vec![
        node("parent", "root"),
        node("child", "parent"),
        node("orphan", "absent"),
    ]);
    w.presentations.push(Presentation {
        id: "historical-card".into(),
        source_id: "parent".into(),
        source_message_id: "historical-message".into(),
        ..Default::default()
    });
    // An initial snapshot, including historical messages, is not a new
    // interaction. Even the first visible frame contains only flocks.
    scene.update(Some("root"), &w, 0.02);
    assert_only_worker_flocks(&scene);
    settle(&mut scene, &w);
    let original_anchors = anchors(&scene);
    let before = scene.workers["parent"].particles;
    for _ in 0..30 {
        scene.update(Some("root"), &w, 1.0 / 60.0);
        assert_only_worker_flocks(&scene);
    }
    // Natural status motion continues without a connector stream.
    assert_ne!(before, scene.workers["parent"].particles);

    w.nodes[0].state = State::Completed;
    w.nodes[1].title = "Updated title".into();
    w.presentations[0].source_message_id = "another-message".into();
    scene.update(Some("root"), &w, 0.02);
    assert_only_worker_flocks(&scene);
    assert_eq!(anchors(&scene), original_anchors);

    for partial in [
        Workspace {
            stale: true,
            ..w.clone()
        },
        Workspace {
            error: Some("unavailable".into()),
            ..w.clone()
        },
        Workspace {
            truncated: true,
            ..w.clone()
        },
    ] {
        scene.update(Some("root"), &partial, 0.02);
        assert_only_worker_flocks(&scene);
        assert_eq!(anchors(&scene), original_anchors);
    }

    // After the parent departs, neither its remembered slot nor the
    // independently absent parent produces a broken-end beacon.
    w.nodes.retain(|node| node.id != "parent");
    settle(&mut scene, &w);
    assert!(!scene.workers.contains_key("parent"));
    assert_eq!(
        slot_position(scene.slots["parent"]).to_array(),
        original_anchors["parent"]
    );
    assert_eq!(anchors(&scene)["child"], original_anchors["child"]);
    assert_only_worker_flocks(&scene);
}

#[test]
fn flocks_form_smoothly_from_coordinator_and_parent_in_arrival_time() {
    let mut scene = Scene::new();
    let w = workspace(vec![node("parent", "root"), node("child", "parent")]);
    scene.update(Some("root"), &Workspace::default(), 0.0);
    scene.update(Some("root"), &w, 0.0);
    let parent_anchor = scene.workers["parent"].anchor;
    assert_eq!(scene.workers["parent"].origin, CENTER);
    assert_eq!(scene.workers["child"].origin, parent_anchor);
    for worker in scene.workers.values() {
        assert!(
            worker
                .particles
                .iter()
                .all(|p| p.distance(worker.origin) < 0.05)
        );
    }
    let station_anchors = anchors(&scene);
    let dt = 1.0 / 60.0;
    // Include the first frame after the 1.5-second arrival boundary.
    for frame in 1..=91 {
        let before: BTreeMap<_, _> = scene
            .workers
            .iter()
            .map(|(id, worker)| (id.clone(), worker.particles))
            .collect();
        scene.update(Some("root"), &w, dt);
        assert_only_worker_flocks(&scene);
        assert_eq!(anchors(&scene), station_anchors);
        for (id, worker) in &scene.workers {
            for (previous, current) in before[id].iter().zip(worker.particles) {
                assert!(previous.distance(current) <= 6.0 * dt + 0.00001);
            }
            if frame == 45 {
                let centroid = worker.particles.iter().copied().sum::<Vec3>() / PARTICLES as f32;
                let path = worker.anchor - worker.origin;
                let progress = (centroid - worker.origin).dot(path) / path.length_squared();
                assert!((0.3..0.7).contains(&progress), "{id}: {progress}");
            }
            if frame == 90 {
                assert!((worker.age - ARRIVAL_SECONDS).abs() < 0.00001);
                assert!(
                    worker
                        .particles
                        .iter()
                        .all(|p| p.distance(worker.anchor) < 0.25)
                );
            }
        }
    }
}

#[test]
fn arrival_departure_and_completion_are_not_fake_progress() {
    let mut scene = Scene::new();
    let mut w = workspace(vec![node("a", "root")]);
    scene.update(Some("root"), &Workspace::default(), 0.0);
    scene.update(Some("root"), &w, 0.0);
    assert!(scene.workers["a"].particles[0].distance(CENTER) < 0.05);
    settle(&mut scene, &w);
    assert!(scene.workers["a"].particles[0].distance(scene.workers["a"].anchor) < 0.25);
    w.nodes[0].state = State::Completed;
    settle(&mut scene, &w);
    assert_eq!(scene.stations()[0].state, State::Completed);
    assert_eq!(scene.stations()[0].opacity, 1.0);
    scene.update(Some("root"), &Workspace::default(), 0.02);
    assert!(!scene.cubes().is_empty());
    assert!(scene.stations()[0].opacity < 1.0);
    settle(&mut scene, &Workspace::default());
    assert!(scene.stations().is_empty());
    assert!(scene.cubes().is_empty());
}

#[test]
fn stale_partial_and_invalid_data_preserve_omitted_workers_as_unknown() {
    let w = workspace(vec![Node {
        state: State::Completed,
        ..node("a", "root")
    }]);
    let mut scene = Scene::new();
    settle(&mut scene, &w);
    for bad in [
        Workspace {
            error: Some("offline".into()),
            ..Default::default()
        },
        Workspace {
            truncated: true,
            ..Default::default()
        },
        Workspace {
            stale: true,
            ..w.clone()
        },
        workspace(vec![node("bad", "bad")]),
    ] {
        scene.update(Some("root"), &bad, 0.02);
        assert_eq!(scene.stations().len(), 1);
        assert_eq!(scene.stations()[0].state, State::Unknown);
        assert!(scene.cubes().iter().all(Cube::valid));
    }
    scene.update(Some("root"), &w, 0.02);
    assert_eq!(scene.stations()[0].state, State::Completed);
}

#[test]
fn fresh_partial_tree_retains_omissions_as_unknown_and_observed_states() {
    let mut scene = Scene::new();
    let partial = Workspace {
        nodes: vec![
            Node {
                state: State::Unknown,
                ..node("a", "root")
            },
            Node {
                state: State::Completed,
                ..node("b", "root")
            },
        ],
        truncated: true,
        ..Default::default()
    };
    scene.update(Some("root"), &partial, 0.02);
    assert_eq!(scene.stations().len(), 2);
    assert_eq!(scene.workers["a"].state, State::Unknown);
    assert_eq!(scene.workers["b"].state, State::Completed);
    assert_eq!(scene.stations()[1].state, State::Completed);
    scene.update(
        Some("root"),
        &Workspace {
            nodes: vec![node("c", "root")],
            error: Some("unavailable".into()),
            ..Default::default()
        },
        0.02,
    );
    assert_eq!(scene.stations().len(), 3);
    assert_eq!(scene.stations()[0].state, State::Unknown);
    assert_eq!(scene.stations()[1].state, State::Unknown);
    assert_eq!(scene.stations()[2].state, State::Running);
    assert!(
        scene
            .workers
            .values()
            .all(|worker| worker.retired.is_none())
    );
}

#[test]
fn truncated_sixty_four_workers_keep_observed_states() {
    let mut scene = Scene::new();
    let partial = Workspace {
        nodes: (0..64)
            .map(|i| Node {
                state: if i == 7 {
                    State::Unknown
                } else {
                    State::Completed
                },
                ..node(&format!("worker-{i:02}"), "root")
            })
            .collect(),
        truncated: true,
        ..Default::default()
    };
    scene.update(Some("root"), &partial, 0.02);
    assert_eq!(scene.stations().len(), 64);
    assert_eq!(scene.stations()[7].state, State::Unknown);
    assert!(scene.stations().iter().enumerate().all(|(i, s)| s.state
        == if i == 7 {
            State::Unknown
        } else {
            State::Completed
        }));
}

#[test]
fn store_error_keeps_status_but_cannot_authorize_removals() {
    let mut scene = Scene::new();
    settle(
        &mut scene,
        &workspace(vec![node("a", "root"), node("b", "root")]),
    );
    let partial = Workspace {
        nodes: vec![Node {
            state: State::Completed,
            ..node("a", "root")
        }],
        error: Some("Presentation store unavailable".into()),
        ..Default::default()
    };
    settle(&mut scene, &partial);
    assert_eq!(scene.stations().len(), 2);
    assert_eq!(scene.stations()[0].state, State::Completed);
    assert_eq!(scene.stations()[1].state, State::Unknown);
    assert!(
        scene
            .workers
            .values()
            .all(|worker| worker.retired.is_none())
    );
    scene.update(
        Some("root"),
        &Workspace {
            stale: true,
            ..partial
        },
        0.02,
    );
    assert!(scene.stations().iter().all(|s| s.state == State::Unknown));
}

#[test]
fn sixty_four_workers_and_rapid_churn_stay_within_budget() {
    let mut scene = Scene::new();
    for generation in 0..8 {
        let w = workspace(
            (0..64)
                .map(|i| Node {
                    state: match i % 3 {
                        0 => State::Completed,
                        1 => State::Error,
                        _ => State::Running,
                    },
                    ..node(
                        &format!("worker-{generation}-{i}"),
                        &format!("missing-{generation}-{i}"),
                    )
                })
                .collect(),
        );
        for _ in 0..3 {
            scene.update(Some("root"), &w, 0.02);
            assert!(scene.cubes().len() <= MAX_CUBES);
            assert!(scene.cubes().iter().all(Cube::valid));
            assert_only_worker_flocks(&scene);
            assert_eq!(
                scene.cubes().len(),
                scene
                    .workers
                    .values()
                    .map(|worker| if solid_completed(worker) {
                        1
                    } else {
                        PARTICLES
                    })
                    .sum::<usize>()
            );
            assert!(scene.workers.len() <= MAX_WORKERS);
            assert!(scene.slots.len() <= SLOT_COUNT);
            assert!(scene.stations().len() <= MAX_WORKERS);
            assert!(scene.stations().iter().all(Station::valid));
            assert_eq!(
                scene
                    .workers
                    .values()
                    .filter(|w| w.retired.is_none())
                    .count(),
                64
            );
            assert!(
                scene
                    .stations()
                    .iter()
                    .take(MAX_NODES)
                    .all(|station| scene.workers[&station.id].retired.is_none())
            );
            let live: BTreeSet<_> = scene
                .workers
                .values()
                .filter(|w| w.retired.is_none())
                .map(|w| w.slot)
                .collect();
            assert_eq!(live.len(), 64);
            let required: BTreeSet<_> = w
                .nodes
                .iter()
                .flat_map(|node| [node.id.as_str(), node.parent_id.as_deref().unwrap()])
                .map(|id| scene.slots[id])
                .collect();
            assert_eq!(required.len(), 128);
            if generation > 0 {
                assert!(required.iter().any(|&slot| slot >= 128));
            }
        }
    }
}

#[test]
fn nonfinite_negative_zero_and_resume_dt_are_safe() {
    let w = workspace(vec![node("a", "root")]);
    let mut scene = Scene::new();
    settle(&mut scene, &w);
    for dt in [f32::NAN, f32::INFINITY, -1.0, 0.0] {
        let before = scene.workers["a"].particles;
        scene.update(Some("root"), &w, dt);
        assert_eq!(before, scene.workers["a"].particles);
    }
    let before = scene.workers["a"].particles;
    scene.update(Some("root"), &w, 1000.0);
    for (a, b) in before.iter().zip(scene.workers["a"].particles) {
        assert!(a.distance(b) <= 0.301);
    }
    assert!(scene.cubes().iter().all(Cube::valid));
}

#[test]
fn a_worker_further_off_is_as_easy_to_point_at_as_one_3_m_away() {
    // Within 3 m, the sphere is as it was; beyond, it keeps its angle.
    assert_eq!(
        pointing_radius(Vec3::ZERO, Vec3::new(0.0, 0.0, -2.0)),
        POINTING_RADIUS
    );
    assert!((pointing_radius(Vec3::ZERO, Vec3::new(0.0, 0.0, -9.0)) - 0.9).abs() < 1e-5);
    let origin = Anchor {
        position: crate::swarm::CENTER + Vec3::new(0.0, 0.0, -6.0),
        orientation: Quat::IDENTITY,
    };
    let mut scene = Scene::new();
    scene.arrange(origin, crate::anchors::EYES, None, None);
    let w = workspace(vec![node("a", "root")]);
    scene.update(Some("root"), &w, 0.0);
    let anchor = scene.workers["a"].anchor;
    let eyes = Vec3::new(0.0, 1.6, 0.0);
    let distance = eyes.distance(anchor);
    assert!(distance > 7.0, "{distance}");
    // A ray passing 0.5 m beside it, 4° off at that distance, points at it, as 0.25 m does at 3.6 m.
    let aside = (anchor - eyes).cross(Vec3::Y).normalize();
    let at = |offset: f32| Ray {
        origin: eyes,
        direction: (anchor + aside * offset - eyes).normalize(),
    };
    assert_eq!(scene.point(Some(at(0.5))), Some("a"));
    assert_eq!(scene.point(Some(at(1.2))), None);
}
