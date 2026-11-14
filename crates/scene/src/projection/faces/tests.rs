// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

//! The faces of projected panels are laid out once for what each shows, and drawn again only as that, its size, its
//! scroll or its frame changes: never as walls move or tilt, the head moves or the ray sweeps.
use std::time::Instant;

use glam::{Mat4, Vec2, Vec3};
use tomori_protocol::{
    Code, Image, Panel, Place, Pose, Projection, Span, TOMORI_WALL, TokenKind, Wall, WallKind,
    WallShape,
};

use super::super::{
    Cells, Controls, FRAME, HIGHLIGHT, Input, NEWEST, PanelLod, Projector, code_columns, editing,
    model, scrolled_face, slot_frames,
};
use super::*;
use crate::{arrival::HOME, pointing::Ray};

/// How many layouts this thread has laid out, and how many faces it has drawn from them.
fn counts() -> (usize, usize) {
    COUNTS.with(std::cell::Cell::get)
}

fn on(wall: &str, slot: u32) -> Place {
    Place::Wall {
        wall: wall.into(),
        slot,
    }
}

const LONG: &str = "## Notes\n\nA paragraph long enough to wrap across the panel's rows many times over, so that the panel has more than it shows at once and scrolls, which it does by a whole row at a time, for whoever scrolls it alone, while everyone else sees it from its top.\n\n- one\n- two\n- three\n- four\n- five\n- six\n- seven\n- eight";

/// Text, code, a table, a checklist, a chart and an image, on the wall `wall-1`, and a text panel floating.
fn panels() -> Vec<Panel> {
    let code_body = "fn main() {\n    let answer = 42;\n    println!(\"{answer}\");\n}";
    let code = Code::new("rust", 4).with_spans(vec![Span::new(0, 2, TokenKind::Keyword)]);
    let chart = crate::chart::fixtures::pressure();
    vec![
        Panel::new("text", "Notes", LONG, on("wall-1", 0)).with_age(30),
        Panel::new("code", "main.rs", code_body, on("wall-1", 1)).with_code(code),
        Panel::new(
            "table",
            "Tests",
            "| A | B |\n|---|---|\n| 1 | 2 |",
            on("wall-1", 2),
        ),
        Panel::new("list", "Agenda", "- [x] one\n- [ ] two", on("wall-1", 3)).with_age(60),
        Panel::new("chart", "Pressure", chart.summary(), on("wall-1", 4)).with_chart(chart),
        Panel::new("image", "Diagram", "A diagram.", on("wall-1", 5)).with_image(Image::new(
            "0123456789abcdef".repeat(4),
            800,
            600,
            1_000,
        )),
        Panel::new(
            "floating",
            "Aside",
            "Floating, it fits its text.",
            Place::Free {
                pose: Pose::upright_facing([0.6, 1.5, -1.2], HOME.to_array()),
            },
        ),
    ]
}

fn wall(pose: Pose) -> Wall {
    Wall::new("wall-1", WallKind::Added, pose)
}

fn projection(panels: Vec<Panel>) -> Projection {
    Projection::new("s", "sky")
        .with_walls(vec![
            Wall::new(
                TOMORI_WALL,
                WallKind::Tomori,
                Pose::upright_facing([0.0, 0.52, -2.3], [0.0, 0.52, 0.0]),
            )
            .with_shape(WallShape::Arc { radius: 2.3 }),
            wall(Pose::upright_facing([0.0, 1.5, -3.0], [0.0, 1.5, 0.0])),
        ])
        .with_panels(panels)
}

fn input(projection: &Projection, viewer: Vec3, ray: Option<Ray>) -> Input<'_> {
    Input {
        projection: Some(projection),
        ray,
        grip: Some(0.0),
        viewer: Some(viewer),
        active: true,
        pointing: true,
        nearer: f32::INFINITY,
        stick: Vec2::ZERO,
        straighten: false,
        free: false,
        controls: Controls::Controllers,
        scroll: None,
        rights: editing::Rights::default(),
        surfaces: None,
        left: crate::projection::Hold::default(),
        now: Instant::now(),
        dt: 1.0 / 72.0,
    }
}

/// The face `panel` showed last, as it was last sized.
fn face(projector: &Projector, panel: &str) -> Arc<Canvas> {
    let face = projector.faces[panel].sizes[0].face.as_ref();
    face.expect("drawn").canvas.clone()
}

/// The faces of every panel, by identity, as they showed last.
fn every_face(projector: &Projector, projection: &Projection) -> Vec<Arc<Canvas>> {
    projection
        .panels
        .iter()
        .map(|panel| face(projector, &panel.id))
        .collect()
}

/// Whether each of `faces` is, as shared, the one before it in `before`.
fn kept(faces: &[Arc<Canvas>], before: &[Arc<Canvas>]) -> Vec<bool> {
    faces
        .iter()
        .zip(before)
        .map(|(face, before)| Arc::ptr_eq(face, before))
        .collect()
}

/// Where the middle of `wall-1`'s `slot` is.
fn slot_middle(projection: &Projection, slot: usize) -> Vec3 {
    let wall = projection.wall("wall-1").unwrap();
    slot_frames(wall, model(&wall.frame()).unwrap())[slot]
        .w_axis
        .truncate()
}

#[test]
fn a_frame_lays_out_and_draws_nothing_unless_something_changed() {
    let shown = projection(panels());
    let mut projector = Projector::default();
    let (laid, drawn) = counts();
    let output = projector.update(input(&shown, HOME, None));
    // Each panel is laid out once, at the size it stands, and its face drawn once.
    assert_eq!(counts(), (laid + 7, drawn + 7));
    let before = every_face(&projector, &shown);
    // The frame hands over each face as it is kept, shared, rather than a copy.
    for face in &before {
        assert!(output.sheets.iter().any(|sheet| {
            sheet
                .canvas
                .as_ref()
                .is_some_and(|canvas| Arc::ptr_eq(canvas, face))
        }));
    }
    // Each face is what drawing it afresh draws.
    for panel in &shown.panels {
        let fit = projector.faces[&panel.id].sizes[0].fit;
        let layout = panel
            .code
            .as_ref()
            .map(|code| CodeLayout::new(panel, code, code_columns(fit)));
        let fresh = scrolled_face(panel, false, panel.id == "image", fit, 0, layout.as_ref());
        assert_eq!(*face(&projector, &panel.id), fresh, "{}", panel.id);
    }
    // Frame after frame, as the head moves, nothing is laid out or drawn again.
    for frame in 0..30 {
        let bob = Vec3::new(
            0.02 * (frame as f32).sin(),
            0.01 * (frame as f32).cos(),
            0.0,
        );
        let output = projector.update(input(&shown, HOME + bob, None));
        assert!(!output.sheets.is_empty());
    }
    assert_eq!(counts(), (laid + 7, drawn + 7));
    assert!(
        kept(&every_face(&projector, &shown), &before)
            .iter()
            .all(|kept| *kept)
    );
}

#[test]
fn moving_or_tilting_a_wall_moves_its_panels_without_laying_them_out() {
    let mut shown = projection(panels());
    let mut projector = Projector::default();
    let first = projector.update(input(&shown, HOME, None));
    let before = every_face(&projector, &shown);
    let counted = counts();
    let models = |output: &crate::projection::Output, face: &Arc<Canvas>| {
        output
            .sheets
            .iter()
            .find(|sheet| {
                sheet
                    .canvas
                    .as_ref()
                    .is_some_and(|canvas| Arc::ptr_eq(canvas, face))
            })
            .and_then(|sheet| sheet.model)
    };
    shown.walls[1] =
        wall(Pose::upright_facing([1.0, 1.4, -2.5], [0.0, 1.5, 0.0])).with_tilt(Some(0.3));
    let moved = projector.update(input(&shown, HOME, None));
    assert_eq!(counts(), counted);
    assert!(
        kept(&every_face(&projector, &shown), &before)
            .iter()
            .all(|kept| *kept)
    );
    // Its panels' faces stand elsewhere, only their models changed.
    assert_ne!(models(&first, &before[0]), models(&moved, &before[0]));
}

#[test]
fn pointing_at_a_panel_recolours_its_frame_without_drawing_it_again() {
    let shown = projection(panels());
    let mut projector = Projector::default();
    projector.update(input(&shown, HOME, None));
    let before = every_face(&projector, &shown);
    let counted = counts();
    // The ray meets the table, the third panel.
    let ray = Ray::new(HOME, slot_middle(&shown, 2) - HOME);
    let output = projector.update(input(&shown, HOME, ray));
    assert_eq!(
        output.pointed.as_ref().map(|(id, ..)| id.as_str()),
        Some("table")
    );
    assert_eq!(counts(), counted, "nothing laid out or drawn again");
    let pointed = every_face(&projector, &shown);
    assert_eq!(
        kept(&pointed, &before),
        [true, true, false, true, true, true, true]
    );
    assert_eq!(pointed[2].border, Some(HIGHLIGHT));
    let mut framed = (*before[2]).clone();
    framed.border = Some(HIGHLIGHT);
    assert_eq!(*pointed[2], framed);
    // Pointed away, it is framed as before, and the newest panel on the wall, the image, in blue.
    projector.update(input(&shown, HOME, None));
    assert_eq!(counts(), counted);
    assert_eq!(face(&projector, "table").border, Some(FRAME));
    assert_eq!(face(&projector, "image").border, Some(NEWEST));
    // A panel newer still takes the blue frame from it, which is only recoloured.
    let mut newer = shown.clone();
    newer
        .panels
        .push(Panel::new("new", "New", "Just now.", on(TOMORI_WALL, 0)));
    newer
        .panels
        .push(Panel::new("newest", "Newest", "Just now.", on("wall-1", 5)).with_age(0));
    newer.panels.retain(|panel| panel.id != "image");
    projector.update(input(&newer, HOME, None));
    assert_eq!(
        counts(),
        (counted.0 + 2, counted.1 + 2),
        "the new panels alone"
    );
    assert_eq!(face(&projector, "newest").border, Some(NEWEST));
    assert!(
        !projector.faces.contains_key("image"),
        "forgotten once removed"
    );
}

#[test]
fn scrolling_draws_the_face_again_from_its_layout_without_laying_it_out() {
    let shown = projection(panels());
    let mut projector = Projector::default();
    projector.update(input(&shown, HOME, None));
    let before = face(&projector, "text");
    let (laid, drawn) = counts();
    let mut scrolled = |rows: f32| {
        projector.update(Input {
            scroll: Some(("text", rows)),
            ..input(&shown, HOME, None)
        });
    };
    // Less than a row: nothing changes.
    scrolled(0.3);
    assert_eq!(counts(), (laid, drawn));
    scrolled(1.0);
    assert_eq!(counts(), (laid, drawn + 1));
    assert_eq!(projector.scroll("text"), 1);
    let after = face(&projector, "text");
    assert_ne!(*after, *before);
    assert_eq!(
        *after,
        scrolled_face(
            &shown.panels[0],
            false,
            false,
            projector.faces["text"].sizes[0].fit,
            1,
            None
        )
    );
}

#[test]
fn what_a_panel_shows_or_its_size_changing_lays_it_out_again() {
    let mut shown = projection(panels());
    let mut projector = Projector::default();
    projector.update(input(&shown, HOME, None));
    let before = every_face(&projector, &shown);
    let (laid, drawn) = counts();
    // The server's patch upserts the text panel with another body: it alone is laid out again.
    shown.panels[0].body.push_str("\n- nine");
    projector.update(input(&shown, HOME, None));
    assert_eq!(counts(), (laid + 1, drawn + 1));
    assert_eq!(
        kept(&every_face(&projector, &shown), &before),
        [false, true, true, true, true, true, true]
    );
    // Only its chart's data changing, the chart panel is drawn again, but not laid out.
    let mut plotted = shown.clone();
    plotted.panels[4].chart = Some(Box::new(crate::chart::fixtures::provisioning()));
    projector.update(input(&plotted, HOME, None));
    assert_eq!(counts(), (laid + 1, drawn + 2));
    assert_ne!(face(&projector, "chart"), before[4]);
    // Its summary changing with its data, it is laid out again too.
    plotted.panels[4].body = "Provisioning.".into();
    projector.update(input(&plotted, HOME, None));
    assert_eq!(counts(), (laid + 2, drawn + 3));
    // Its wall's slots made taller, each of its panels is laid out at their size.
    plotted.walls[1] = plotted.walls[1].clone().with_aspect(Some(0.5));
    projector.update(input(&plotted, HOME, None));
    assert_eq!(counts(), (laid + 8, drawn + 9));
}

#[test]
fn a_wall_s_content_scale_lays_its_panels_out_again_at_their_cells_drawn_at_its_text_s_size_and_kept()
 {
    let mut shown = projection(panels());
    let mut projector = Projector::default();
    projector.update(input(&shown, HOME, None));
    let standard = face(&projector, "text");
    let (laid, drawn) = counts();
    let model_of = |output: &crate::projection::Output, face: &Arc<Canvas>| {
        output
            .sheets
            .iter()
            .find(|sheet| {
                sheet
                    .canvas
                    .as_ref()
                    .is_some_and(|canvas| Arc::ptr_eq(canvas, face))
            })
            .and_then(|sheet| sheet.model)
            .expect("drawn")
    };
    // Its text drawn at half, each of the wall's six panels is laid out again, once, at twice as many columns and
    // rows, and drawn at half the size, filling the same slot.
    shown.walls[1] = shown.walls[1].clone().with_content_scale(Some(0.5));
    let output = projector.update(input(&shown, HOME, None));
    assert_eq!(counts(), (laid + 6, drawn + 6));
    let cells = Cells::of(&shown.walls[1]);
    assert_eq!(projector.faces["text"].sizes[0].fit, Fit::Slot(cells));
    assert!(cells.columns >= 2 * Cells::STANDARD.columns, "{cells:?}");
    assert!(
        cells.code_columns >= 2 * Cells::STANDARD.code_columns,
        "{cells:?}"
    );
    let smaller = face(&projector, "text");
    let model = model_of(&output, &smaller);
    assert!((model.x_axis.truncate().length() - 0.5).abs() < 1e-4);
    let (slot_width, slot_height) = shown.walls[1].slot_size();
    let (width, height) = smaller.size();
    assert!(
        width * 0.5 <= slot_width + 1e-4 && height * 0.5 <= slot_height + 1e-4,
        "{width} {height}"
    );
    assert!(
        width * 0.5 > slot_width * 0.95,
        "it fills its slot: {width}"
    );
    // The same again lays out and draws nothing, and back as large as its slots give it, its faces at that size,
    // kept, are shown again as they were.
    projector.update(input(&shown, HOME, None));
    assert_eq!(counts(), (laid + 6, drawn + 6));
    shown.walls[1] = shown.walls[1].clone().with_content_scale(None);
    projector.update(input(&shown, HOME, None));
    assert_eq!(counts(), (laid + 6, drawn + 6));
    assert!(Arc::ptr_eq(&face(&projector, "text"), &standard));
    // Drawn larger, at twice, each slot holds fewer cells than a standard one, but always a few words and some code.
    shown.walls[1] = shown.walls[1].clone().with_content_scale(Some(2.0));
    projector.update(input(&shown, HOME, None));
    let larger = Cells::of(&shown.walls[1]);
    assert_eq!(projector.faces["text"].sizes[0].fit, Fit::Slot(larger));
    assert!(larger.columns < Cells::STANDARD.columns && larger.body < Cells::STANDARD.body);
    assert!(larger.code_columns > super::super::GUTTER + super::super::CODE_ROOM - 1);
}

/// A patch counts on the age of every panel held, in seconds, but a panel shows its age in minutes, hours or days: one
/// whose age shows the same is not laid out again, and one whose age passes another minute is.
#[test]
fn an_age_counted_on_lays_a_panel_out_again_only_where_it_shows_otherwise() {
    let mut shown = projection(panels());
    let mut projector = Projector::default();
    projector.update(input(&shown, HOME, None));
    let before = every_face(&projector, &shown);
    let (laid, drawn) = counts();
    // A few seconds on, as when the next patch arrives, every panel shows the same age.
    for panel in &mut shown.panels {
        panel.age += 5;
    }
    projector.update(input(&shown, HOME, None));
    assert_eq!(counts(), (laid, drawn));
    assert!(
        kept(&every_face(&projector, &shown), &before)
            .iter()
            .all(|kept| *kept)
    );
    // The checklist, a minute old, passes to two minutes: it alone is laid out again, and shows it.
    shown.panels[3].age = 120;
    projector.update(input(&shown, HOME, None));
    assert_eq!(counts(), (laid + 1, drawn + 1));
    assert_eq!(
        kept(&every_face(&projector, &shown), &before),
        [true, true, true, false, true, true, true]
    );
    assert!(face(&projector, "list").plain().contains("2m"));
}

#[test]
fn a_panel_keeps_its_faces_at_a_few_sizes_the_latest_first() {
    let panel = Panel::new("text", "Notes", LONG, on("wall-1", 0));
    let mut faces = Faces::new(&panel);
    let sizes: Vec<Fit> = [34, 40, 46, 52]
        .into_iter()
        .map(|columns| {
            Fit::Slot(super::super::Cells {
                columns,
                ..super::super::Cells::STANDARD
            })
        })
        .collect();
    let (laid, _) = counts();
    for fit in &sizes {
        faces.at(*fit);
    }
    assert_eq!(counts().0, laid + 4);
    assert_eq!(faces.sizes.len(), SIZES);
    // The first is forgotten, the others kept: going back to one kept lays nothing out, and puts it first.
    faces.at(sizes[1]);
    assert_eq!(counts().0, laid + 4);
    assert_eq!(faces.sizes[0].fit, sizes[1]);
    faces.at(sizes[0]);
    assert_eq!(counts().0, laid + 5);
    // A code panel floating is as tall as its code, and points at no item.
    let code = Panel::new("code", "main.rs", "fn main() {}", on("wall-1", 1))
        .with_code(Code::new("rust", 1));
    let mut faces = Faces::new(&code);
    let floating = faces.at(Fit::Text);
    assert_eq!(floating.size, super::super::fitted_size(&code, Fit::Text));
    assert_eq!(floating.item(Mat4::IDENTITY, 0, Vec3::ZERO), None);
}

/// From a server that versions panels, a panel's version and data seq are its faces' keys: a new version lays it out
/// again, and a chart's new data seq draws its face again from its new points without laying it out, while what the
/// client changes itself, a tick shown at once, is still followed.
#[test]
fn a_versioned_panel_is_laid_out_again_for_its_version_and_drawn_again_for_its_data() {
    let mut shown = projection(
        panels()
            .into_iter()
            .map(|panel| {
                let seq = if panel.chart.is_some() { 5 } else { 0 };
                panel.with_version(5).with_data_seq(seq)
            })
            .collect(),
    );
    let mut projector = Projector::default();
    projector.update(input(&shown, HOME, None));
    let before = every_face(&projector, &shown);
    let (laid, drawn) = counts();
    // The chart's points and summary move on at a new data seq, its version as it was: its face is drawn again from
    // them, and nothing laid out.
    let chart = crate::chart::fixtures::provisioning();
    shown.panels[4].body = chart.summary();
    shown.panels[4].chart = Some(Box::new(chart));
    shown.panels[4].data_seq = 6;
    projector.update(input(&shown, HOME, None));
    assert_eq!(counts(), (laid, drawn + 1));
    assert_eq!(
        kept(&every_face(&projector, &shown), &before),
        [true, true, true, true, false, true, true]
    );
    // The checklist ticked at once, its version as the server sent it, is laid out again, as it then is at its next
    // version.
    shown.panels[3].body = "- [x] one\n- [x] two".into();
    projector.update(input(&shown, HOME, None));
    assert_eq!(counts(), (laid + 1, drawn + 2));
    shown.panels[3].version = 7;
    projector.update(input(&shown, HOME, None));
    assert_eq!(counts(), (laid + 2, drawn + 3));
    // At the same version, body and title, nothing else is looked at: its version says it is as it was.
    shown.panels[0].meta.push(tomori_protocol::Meta::new(
        "source",
        "as no server sends without a version",
    ));
    projector.update(input(&shown, HOME, None));
    assert_eq!(counts(), (laid + 2, drawn + 3));
    // A new version lays it out again.
    shown.panels[0].version = 8;
    projector.update(input(&shown, HOME, None));
    assert_eq!(counts(), (laid + 3, drawn + 4));
    // Its title is followed at the same version, since a server before versions counted no change its environment
    // made of a panel.
    shown.panels[0].title = "Renamed".into();
    projector.update(input(&shown, HOME, None));
    assert_eq!(counts(), (laid + 4, drawn + 5));
    assert!(face(&projector, "text").plain().contains("Renamed"));
}

/// From a server before versions, which counted each panel's changes as its revision but not its environment's changes
/// of it, a chart panel with a revision and no data seq is followed as it shows, its points moving on drawn again.
#[test]
fn a_chart_panel_from_a_server_before_versions_is_followed_by_its_points() {
    let mut shown = projection(
        panels()
            .into_iter()
            .map(|panel| panel.with_version(1))
            .collect(),
    );
    let mut projector = Projector::default();
    projector.update(input(&shown, HOME, None));
    let (laid, drawn) = counts();
    // Its points move on with its summary as it was, at the same revision.
    let mut chart = shown.panels[4].chart.clone().unwrap();
    chart.series[0].values.rotate_left(1);
    shown.panels[4].chart = Some(chart);
    projector.update(input(&shown, HOME, None));
    assert_eq!(counts(), (laid, drawn + 1));
}

/// As many charts as an environment may publish, 32, on two walls of 16 slots, versioned, each at the data seq `seq`
/// with a latest point of its own for it, and a text panel floating.
fn charts(seq: u64) -> Projection {
    let chart = |index: usize| {
        let mut chart = crate::chart::fixtures::pressure()
            .with_time(tomori_protocol::chart::TimeAxis::Live { step_ms: 5_000 });
        *chart.series[0].values.last_mut().unwrap() =
            Some(0.5 + 0.01 * (seq as usize + index) as f32);
        chart
    };
    let mut panels: Vec<Panel> = (0..32)
        .map(|index| {
            let chart = chart(index);
            let place = on(&format!("wall-{}", 1 + index / 16), index as u32 % 16);
            Panel::new(format!("chart-{index}"), "Pressure", chart.summary(), place)
                .with_chart(chart)
                .with_version(5)
                .with_data_seq(seq)
        })
        .collect();
    let aside = Place::Free {
        pose: Pose::upright_facing([0.6, 1.5, -1.2], HOME.to_array()),
    };
    panels.push(Panel::new("aside", "Aside", "Floating.", aside).with_version(5));
    // One above the other, the lower nearer, every slot near enough to read.
    let wall = |id: &str, y: f32| {
        Wall::new(
            id,
            WallKind::Added,
            Pose::upright_facing([0.0, y, -3.0], [0.0, y, 0.0]),
        )
        .with_grid(tomori_protocol::Grid::from((8, 2)))
    };
    Projection::new("s", "sky")
        .with_walls(vec![wall("wall-1", 1.2), wall("wall-2", 2.4)])
        .with_panels(panels)
}

/// As many panels as an environment may publish, 32, each a list, on two walls of 16 slots, versioned `version`, their
/// rows saying so.
fn lists(version: u64) -> Projection {
    let mut shown = charts(5);
    shown.panels.truncate(32);
    for (index, panel) in shown.panels.iter_mut().enumerate() {
        panel.title = format!("Queue {index}");
        panel.body = format!("- [ ] first at {version}\n- [ ] second");
        panel.chart = None;
        panel.data_seq = 0;
        panel.version = version;
    }
    shown
}

/// Whether each panel's face is the one a projector that lays every panel out at once shows.
fn current(projector: &Projector, shown: &Projection) -> Vec<bool> {
    let mut fresh = Projector::default();
    fresh.update(input(shown, HOME, None));
    shown
        .panels
        .iter()
        .map(|panel| {
            projector.faces.contains_key(&panel.id)
                && *face(projector, &panel.id) == *face(&fresh, &panel.id)
        })
        .collect()
}

/// Paced, as the frame step's projector is, panels that arrive together, as on entering a space or as an environment
/// puts up its 32, are laid out a few a frame, [`ARRIVALS`], in the projection's order, each standing from the frame it
/// is laid out in, rather than all in one frame, which would cost that frame several times what it has.
#[test]
fn panels_arriving_together_are_laid_out_a_few_a_frame_in_the_projection_s_order() {
    let shown = lists(5);
    let mut projector = Projector::paced();
    let (laid, drawn) = counts();
    for frame in 1..=32 / ARRIVALS {
        let output = projector.update(input(&shown, HOME, None));
        // Each frame lays out and draws so many, no more: the next in order.
        assert_eq!(
            counts(),
            (laid + frame * ARRIVALS, drawn + frame * ARRIVALS)
        );
        let standing: Vec<bool> = shown
            .panels
            .iter()
            .map(|panel| projector.faces.contains_key(&panel.id))
            .collect();
        assert!(standing[..frame * ARRIVALS].iter().all(|stands| *stands));
        assert!(standing[frame * ARRIVALS..].iter().all(|stands| !stands));
        // Those laid out are drawn, with both walls; the rest are not yet.
        let faces = output.sheets.iter().filter(|sheet| sheet.canvas.is_some());
        assert_eq!(faces.count(), 2 + frame * ARRIVALS);
    }
    // Nothing more is laid out, and every one stands, as a projector that lays them out at once shows them.
    for _ in 0..3 {
        projector.update(input(&shown, HOME, None));
    }
    assert_eq!(counts(), (laid + 32, drawn + 32));
    assert!(current(&projector, &shown).iter().all(|current| *current));
}

/// Paced, as many panels as a space shows a client that takes them all, 900, stand within a bounded number of frames
/// of entering it, 113, about a second and a half, where two a frame took 450: panels that show nothing yet are laid
/// out [`ARRIVALS`] a frame, and no frame lays out more than that and the [`LAYOUTS`] that are laid out again of those
/// that already show something, however many of those the server changes meanwhile.
#[test]
fn every_panel_of_the_fullest_space_stands_within_a_bounded_number_of_frames() {
    let panels: Vec<Panel> = (0..900)
        .map(|index| {
            let (column, row) = ((index % 30) as f32, (index / 30) as f32);
            let at = [column * 0.7 - 10.0, 0.5 + row * 0.4, -6.0];
            let pose = Pose::upright_facing(at, [at[0], at[1], 0.0]);
            Panel::new(
                format!("panel-{index}"),
                "Note",
                format!("Note {index}."),
                Place::Free { pose },
            )
            .with_version(5)
        })
        .collect();
    let mut shown = Projection::new("s", "sky").with_panels(panels);
    let mut projector = Projector::paced();
    let (laid, _) = counts();
    let mut frames = 0;
    while projector.faces.len() < 900 {
        // Halfway, the server changes every panel that stands: they are laid out again two a frame, as ever, while the
        // rest go on arriving eight a frame.
        if frames == 50 {
            for panel in &mut shown.panels {
                panel.body.push_str(" Changed.");
                panel.version = 6;
            }
        }
        let before = (counts().0, projector.faces.len());
        let output = projector.update(input(&shown, HOME, None));
        frames += 1;
        let arrived = projector.faces.len() - before.1;
        let again = counts().0 - before.0 - arrived;
        assert_eq!(arrived, ARRIVALS.min(900 - before.1));
        assert_eq!(again, if frames > 50 { LAYOUTS } else { 0 });
        let standing = output.sheets.iter().filter(|sheet| sheet.canvas.is_some());
        assert_eq!(standing.count(), projector.faces.len());
        assert!(frames <= 900_usize.div_ceil(ARRIVALS));
    }
    assert_eq!(frames, 900_usize.div_ceil(ARRIVALS));
    // Every one stands, in the projection's order as it arrived; those changed are then laid out again in turn.
    let again = counts().0 - laid - 900;
    assert_eq!(again, (frames - 50) * LAYOUTS);
    assert!(face(&projector, "panel-899").plain().contains("Changed"));
}

/// Paced, panels the server changed together, as an environment's lists on one tick, are laid out again a few a frame,
/// each in turn, showing what they showed until then; what this client changed itself is laid out in its frame.
#[test]
fn panels_the_server_changed_together_are_laid_out_a_few_a_frame_each_in_turn() {
    let mut shown = lists(5);
    let mut projector = Projector::paced();
    for _ in 0..32 / LAYOUTS {
        projector.update(input(&shown, HOME, None));
    }
    let (laid, drawn) = counts();
    let first = every_face(&projector, &shown);
    // Every list changes in one patch, at a new version: the frame lays out only so many, the first in order, and the
    // rest show what they showed, the very faces.
    shown = lists(6);
    projector.update(input(&shown, HOME, None));
    assert_eq!(counts(), (laid + LAYOUTS, drawn + LAYOUTS));
    let second = every_face(&projector, &shown);
    let again = kept(&second, &first);
    assert!(again[..LAYOUTS].iter().all(|kept| !kept));
    assert!(again[LAYOUTS..].iter().all(|kept| *kept));
    assert!(face(&projector, "chart-0").plain().contains("first at 6"));
    assert!(face(&projector, "chart-31").plain().contains("first at 5"));
    // They change again before the rest are laid out: the next frame goes on from where the last ended, rather than
    // with the first again, so that none waits behind the same ones every frame, and lays them out as they now are.
    shown = lists(7);
    projector.update(input(&shown, HOME, None));
    assert_eq!(counts(), (laid + 2 * LAYOUTS, drawn + 2 * LAYOUTS));
    let third = every_face(&projector, &shown);
    let again = kept(&third, &second);
    assert!(again[..LAYOUTS].iter().all(|kept| *kept));
    assert!(again[LAYOUTS..2 * LAYOUTS].iter().all(|kept| !kept));
    assert!(again[2 * LAYOUTS..].iter().all(|kept| *kept));
    assert!(face(&projector, "chart-0").plain().contains("first at 6"));
    assert!(face(&projector, "chart-2").plain().contains("first at 7"));
    // A tick this participant makes, shown at once at the version the panel has, is laid out in its frame, whatever
    // waits, and takes no other panel's turn; an age that shows otherwise waits its turn as the server's changes do.
    shown.panels[3].body = "- [x] first at 7\n- [ ] second".into();
    shown.panels[2].age = 3600;
    projector.update(input(&shown, HOME, None));
    assert_eq!(counts(), (laid + 3 * LAYOUTS + 1, drawn + 3 * LAYOUTS + 1));
    assert!(face(&projector, "chart-3").plain().contains("☑ first at 7"));
    let now = current(&projector, &shown);
    assert_eq!(now[..3 * LAYOUTS], [false, false, false, true, true, true]);
    // Frame after frame, each in turn, the first ones last, none starved: within as many frames as lay every one out
    // once, each shows what laying it out afresh shows, and then nothing more is laid out.
    let mut frames = 0;
    let waiting = 32 - 2 * LAYOUTS + 1;
    while current(&projector, &shown).contains(&false) {
        let before = counts();
        projector.update(input(&shown, HOME, None));
        let laid = counts().0 - before.0;
        assert!((1..=LAYOUTS).contains(&laid) && counts().1 - before.1 == laid);
        frames += 1;
        assert!(frames <= waiting.div_ceil(LAYOUTS));
    }
    assert_eq!(frames, waiting.div_ceil(LAYOUTS));
    assert!(face(&projector, "chart-2").plain().contains("1h"));
    let settled = counts();
    for _ in 0..3 {
        projector.update(input(&shown, HOME, None));
    }
    assert_eq!(counts(), settled);
}

/// Paced, charts whose live data changed together are drawn again a few a frame, each in turn, without being laid
/// out, on the same turns as panels laid out.
#[test]
fn charts_whose_data_changed_together_are_drawn_again_a_few_a_frame_each_in_turn() {
    let mut shown = charts(5);
    let mut projector = Projector::paced();
    while current(&projector, &shown).contains(&false) {
        projector.update(input(&shown, HOME, None));
    }
    let (laid, drawn) = counts();
    let first = every_face(&projector, &shown);
    // Every chart's next point arrives in one patch: the frame draws only so many again, the first in order, and lays
    // nothing out; the rest show what they showed.
    shown = charts(6);
    projector.update(input(&shown, HOME, None));
    assert_eq!(counts(), (laid, drawn + LAYOUTS));
    let second = every_face(&projector, &shown);
    let again = kept(&second, &first);
    assert!(again[..LAYOUTS].iter().all(|kept| !kept));
    assert!(again[LAYOUTS..].iter().all(|kept| *kept));
    // A text panel's change at a new version takes a turn among them, laid out as its turn comes.
    shown.panels[32].body = "Floating, changed.".into();
    shown.panels[32].version = 8;
    for frame in 2..=32 / LAYOUTS {
        projector.update(input(&shown, HOME, None));
        assert_eq!(counts(), (laid, drawn + frame * LAYOUTS));
    }
    projector.update(input(&shown, HOME, None));
    assert_eq!(counts(), (laid + 1, drawn + 33));
    assert!(face(&projector, "aside").plain().contains("changed"));
    for _ in 0..3 {
        projector.update(input(&shown, HOME, None));
    }
    assert_eq!(counts(), (laid + 1, drawn + 33));
    assert!(current(&projector, &shown).iter().all(|current| *current));
}

/// While a panel's faces are behind the projection's panel, its turn not yet come, none of its items is pointed at,
/// since the lines it shows are no longer the panel's, so that no tick names another item than the one shown.
#[test]
fn a_panel_waiting_its_turn_points_at_no_item() {
    let panel = lists(5).panels.remove(0);
    let mut faces = Faces::new(&panel);
    let size = faces.at(Fit::SLOT).size;
    // The middle of the row of its first item, below its title, its age and the rule.
    let y = size.1 / 2.0
        - (super::super::PADDING.1 as f32 + 3.5 * CELL_HEIGHT as f32) * super::super::PIXEL;
    let item =
        |faces: &mut Faces| faces.item(Fit::SLOT, Mat4::IDENTITY, 0, Vec3::new(-0.05, y, 0.0));
    assert_eq!(item(&mut faces), Some(0));
    // The server changed it, and its turn has not come: it shows what it showed, and points at nothing.
    let changed = lists(6).panels.remove(0);
    assert!(!faces.follow(&changed, false, 0.0));
    assert_eq!(item(&mut faces), None);
    assert!(
        faces
            .face(Fit::SLOT, (0, &Default::default()), FRAME)
            .plain()
            .contains("first at 5")
    );
    // Its turn come, it shows the panel as it is, and points at its items again.
    assert!(faces.follow(&changed, true, 0.0));
    assert_eq!(item(&mut faces), Some(0));
    assert!(
        faces
            .face(Fit::SLOT, (0, &Default::default()), FRAME)
            .plain()
            .contains("first at 6")
    );
    assert!(!faces.follow(&changed, false, 0.0));
    assert_eq!(item(&mut faces), Some(0));
}

/// A chart panel sent as its summary, as a server sends it where a client's frame cannot hold its chart, at the version
/// and data seq it had, is laid out again as the text panel it then is, and as a chart once it is sent whole again.
#[test]
fn a_chart_panel_sent_as_its_summary_is_laid_out_again_as_text_and_as_a_chart_again() {
    let mut shown = charts(5);
    let mut projector = Projector::default();
    projector.update(input(&shown, HOME, None));
    let (laid, drawn) = counts();
    let chart = shown.panels[0].chart.take();
    projector.update(input(&shown, HOME, None));
    assert_eq!(counts(), (laid + 1, drawn + 1));
    assert!(face(&projector, "chart-0").shapes.is_empty());
    assert!(face(&projector, "chart-0").plain().contains("mean: "));
    // Whole again, with a point newer, it is a chart again, laid out as one rather than only drawn from its data.
    shown.panels[0].chart = chart;
    shown.panels[0].data_seq = 6;
    projector.update(input(&shown, HOME, None));
    assert_eq!(counts(), (laid + 2, drawn + 2));
    assert!(!face(&projector, "chart-0").shapes.is_empty());
}

#[test]
fn scanning_a_chart_s_plot_reads_it_over_its_face_without_drawing_the_face_again() {
    let shown = projection(panels());
    let mut projector = Projector::default();
    projector.update(input(&shown, HOME, None));
    let walls: Vec<(&Wall, Mat4)> = shown
        .walls
        .iter()
        .filter_map(|wall| model(&wall.frame()).map(|frame| (wall, frame)))
        .collect();
    let (panel_model, _) = projector
        .stands(shown.panel("chart").unwrap(), &walls)
        .unwrap();
    let canvas = face(&projector, "chart");
    let plot = projector.faces["chart"].sizes[0]
        .face
        .as_ref()
        .and_then(|face| face.plot.clone())
        .expect("a chart's face keeps where its plot is");
    // The ray from home through a place on the face, in font pixels inside its padding.
    let towards = |[x, y]: [f32; 2]| {
        let (width, height) = canvas.size();
        let local = Vec3::new(
            (x + canvas.padding.0 as f32) * canvas.pixel - width / 2.0,
            height / 2.0 - (y + canvas.padding.1 as f32) * canvas.pixel,
            0.0,
        );
        Ray::new(HOME, panel_model.transform_point3(local) - HOME)
    };
    let middle = plot.top + plot.height / 2.0;
    let counted = counts();
    // Scanned from left to right, the readout follows, later at each step, drawn over the face, which is the same face.
    let mut before = 0;
    for step in 1..10 {
        let x = plot.left + plot.width * step as f32 / 10.0;
        let output = projector.update(input(&shown, HOME, towards([x, middle])));
        let across = output.across.expect("on the plot");
        assert!(across > before, "{across} after {before}");
        assert!(
            (f32::from(across) - 1000.0 * step as f32).abs() < 20.0,
            "{across}"
        );
        before = across;
        let readout = output.readout.expect("drawn while on the plot");
        let overlay = readout.canvas.as_ref().unwrap();
        assert_eq!(overlay.size(), canvas.size());
        assert_eq!(
            readout.model,
            Some(panel_model * Mat4::from_translation(Vec3::Z * super::super::READOUT_LIFT))
        );
    }
    assert_eq!(counts(), counted, "nothing laid out or drawn again");
    // While the ray is still, the readout is the one drawn, handed over again.
    let still = towards([plot.left + plot.width / 2.0, middle]);
    let first = projector
        .update(input(&shown, HOME, still))
        .readout
        .unwrap();
    let again = projector
        .update(input(&shown, HOME, still))
        .readout
        .unwrap();
    assert!(Arc::ptr_eq(
        first.canvas.as_ref().unwrap(),
        again.canvas.as_ref().unwrap()
    ));
    // So is it while the hand is nearly still, within a font pixel, but not once it moves a pixel on.
    let pixel = (plot.left + plot.width / 2.0).round();
    let mut near = |x: f32| {
        projector
            .update(input(&shown, HOME, towards([x, middle])))
            .readout
            .unwrap()
            .canvas
            .unwrap()
    };
    let a = near(pixel - 0.3);
    let b = near(pixel + 0.3);
    let c = near(pixel + 1.0);
    assert!(Arc::ptr_eq(&a, &b));
    assert!(!Arc::ptr_eq(&b, &c));
    // Off its plot, on its title or its axis's labels, there is none, though the panel is pointed at.
    for off in [[plot.left, 3.0], [plot.left - 4.0, middle]] {
        let output = projector.update(input(&shown, HOME, towards(off)));
        assert_eq!(
            output.pointed.as_ref().map(|(id, ..)| id.as_str()),
            Some("chart")
        );
        assert_eq!((output.readout, output.across), (None, None));
    }
    // A panel of no chart has none.
    let ray = Ray::new(HOME, slot_middle(&shown, 2) - HOME);
    let output = projector.update(input(&shown, HOME, ray));
    assert_eq!((output.readout, output.across), (None, None));
}

/// Charts that share the time of a chart pointed at, on its wall where its axis counts back from now, or anywhere where
/// it says clock times, show a readout linked to it at the same time, drawn over their faces without laying anything
/// out; charts that do not cover the time, on another wall, carried or of the other kind show none.
#[test]
fn pointing_at_a_chart_links_a_readout_on_every_chart_sharing_its_time() {
    use tomori_protocol::chart::{AxisLabel, TimeAxis};
    // Labels are only shown: charts link by their time axes.
    let timed = |labels: Vec<AxisLabel>, time: Option<TimeAxis>| {
        let mut chart = crate::chart::fixtures::pressure();
        chart.labels = labels;
        chart.time = time;
        chart
    };
    let live = |first: &str, step_ms: u32| {
        timed(
            vec![AxisLabel::new(0, first), AxisLabel::new(180, "now")],
            Some(TimeAxis::Live { step_ms }),
        )
    };
    // 12 November 2026, 00:00 UTC, and a minute of it.
    const DAY: i64 = 1_794_441_600_000;
    let at = |hours: i64, minutes: i64| DAY + hours * 3_600_000 + minutes * 60_000;
    let fixed = |from: &str, to: &str, from_ms: i64, to_ms: i64| {
        timed(
            vec![AxisLabel::new(0, from), AxisLabel::new(180, to)],
            Some(TimeAxis::Fixed { from_ms, to_ms }),
        )
    };
    let chart = |id: &str, chart: tomori_protocol::Chart, place: Place| {
        Panel::new(id, id, chart.summary(), place).with_chart(chart)
    };
    let floating = |x: f32| Place::Free {
        pose: Pose::upright_facing([x, 2.9, -1.5], HOME.to_array()),
    };
    let mut shown = projection(vec![
        chart("pressure", live("-15 min", 5_000), on("wall-1", 0)),
        // An hour's chart covers ten minutes ago; five minutes' does not.
        chart("hour", live("-1 h", 20_000), on("wall-1", 1)),
        chart("five", live("-5 min", 1_667), on("wall-1", 2)),
        Panel::new("notes", "Notes", "Text.", on("wall-1", 3)),
        // Labels that count back from now, on a chart that says nothing of its time, link nothing.
        chart(
            "untimed",
            timed(
                vec![AxisLabel::new(0, "-1 h"), AxisLabel::new(180, "now")],
                None,
            ),
            on("wall-1", 4),
        ),
        // On another wall, the same live window is not shared.
        chart("elsewhere", live("-15 min", 5_000), on("wall-2", 0)),
        // Frozen windows, overlapping by ten minutes, one that does not overlap, and the same times a day before.
        chart(
            "frozen",
            fixed("02:20", "02:50", at(2, 20), at(2, 50)),
            floating(-0.8),
        ),
        chart(
            "overlapping",
            fixed("02:40", "03:10", at(2, 40), at(3, 10)),
            floating(0.8),
        ),
        chart(
            "later",
            fixed("05:00", "05:30", at(5, 0), at(5, 30)),
            floating(0.0),
        ),
        chart(
            "yesterday",
            fixed(
                "02:40",
                "03:10",
                at(2, 40) - 86_400_000,
                at(3, 10) - 86_400_000,
            ),
            floating(1.6),
        ),
    ]);
    shown.walls.push(Wall::new(
        "wall-2",
        WallKind::Added,
        Pose::upright_facing([4.0, 1.5, -3.0], [4.0, 1.5, 0.0]),
    ));
    let mut projector = Projector::default();
    projector.update(input(&shown, HOME, None));
    let walls: Vec<(&Wall, Mat4)> = shown
        .walls
        .iter()
        .filter_map(|wall| model(&wall.frame()).map(|frame| (wall, frame)))
        .collect();
    // The ray from home at `at`, a share of the way across the plot of `panel`, halfway down it.
    let towards = |projector: &Projector, panel: &str, share: f32| {
        let (panel_model, _) = projector
            .stands(shown.panel(panel).unwrap(), &walls)
            .unwrap();
        let canvas = face(projector, panel);
        let plot = projector.faces[panel].sizes[0]
            .face
            .as_ref()
            .and_then(|face| face.plot.clone())
            .unwrap();
        let (width, height) = canvas.size();
        let [x, y] = [plot.left + plot.width * share, plot.top + plot.height / 2.0];
        let local = Vec3::new(
            (x + canvas.padding.0 as f32) * canvas.pixel - width / 2.0,
            height / 2.0 - (y + canvas.padding.1 as f32) * canvas.pixel,
            0.0,
        );
        Ray::new(HOME, panel_model.transform_point3(local) - HOME)
    };
    let linked = |projector: &Projector| {
        let mut linked: Vec<(String, String)> = projector
            .linked
            .iter()
            .map(|(id, (_, readout, _))| (id.clone(), readout.lines[0].0.clone()))
            .collect();
        linked.sort();
        linked
    };
    let counted = counts();
    // Ten minutes ago on the pressure chart is ten minutes ago on the hour's, on its wall alone.
    let ray = towards(&projector, "pressure", 1.0 / 3.0);
    let output = projector.update(input(&shown, HOME, ray));
    assert!(output.readout.is_some());
    assert_eq!(output.linked.len(), 1);
    assert_eq!(
        linked(&projector),
        [("hour".to_owned(), "-10 min".to_owned())]
    );
    let (_, readout, _) = &projector.linked["hour"];
    assert!(readout.linked);
    let (hour_model, _) = projector
        .stands(shown.panel("hour").unwrap(), &walls)
        .unwrap();
    assert_eq!(
        output.linked[0].model,
        Some(hour_model * Mat4::from_translation(Vec3::Z * super::super::READOUT_LIFT))
    );
    // Still, it is handed over again; nothing was laid out or drawn again.
    let again = projector.update(input(&shown, HOME, ray));
    assert!(Arc::ptr_eq(
        output.linked[0].canvas.as_ref().unwrap(),
        again.linked[0].canvas.as_ref().unwrap()
    ));
    assert_eq!(counts(), counted);
    // Two minutes ago, the five minutes' chart covers it too.
    let ray = towards(&projector, "pressure", 13.0 / 15.0);
    let output = projector.update(input(&shown, HOME, ray));
    assert_eq!(output.linked.len(), 2);
    assert_eq!(
        linked(&projector)
            .iter()
            .map(|(id, _)| id.as_str())
            .collect::<Vec<_>>(),
        ["five", "hour"]
    );
    // The hour's chart carried in the left hand is left out, while its wall's other charts still link.
    let hour = towards(&projector, "hour", 0.5);
    let left = |grip: f32| crate::projection::Hold {
        ray: hour,
        grip: Some(grip),
        active: true,
        pointing: true,
        ..Default::default()
    };
    let carrying = |grip: f32| Input {
        left: left(grip),
        ..input(&shown, HOME, ray)
    };
    projector.update(carrying(0.0));
    projector.update(carrying(1.0));
    assert_eq!(projector.held_in(crate::sides::Side::Left), Some("hour"));
    let output = projector.update(carrying(1.0));
    assert_eq!(output.linked.len(), 1);
    assert_eq!(linked(&projector)[0].0, "five");
    projector.update(carrying(0.0));
    // A frozen chart links to the one overlapping it, by the instant, wherever it stands, and to nothing live, nor to
    // the same times of day a day before.
    let ray = towards(&projector, "frozen", 5.0 / 6.0);
    let output = projector.update(input(&shown, HOME, ray));
    assert!(output.readout.is_some());
    let frozen = linked(&projector);
    assert_eq!(frozen.len(), 1);
    assert_eq!(frozen[0].0, "overlapping");
    // About 02:45, to the second, as its points are ten seconds apart, where the ray meets the plot to the pixel.
    assert!(frozen[0].1.starts_with("02:4"), "{}", frozen[0].1);
    // Where they do not overlap, nothing.
    let ray = towards(&projector, "frozen", 0.1);
    assert!(projector.update(input(&shown, HOME, ray)).linked.is_empty());
    // Off the plot, or pointing at nothing, none.
    assert!(
        projector
            .update(input(&shown, HOME, None))
            .linked
            .is_empty()
    );
    assert!(projector.linked.is_empty());
}

/// A panel's faces give no linked readout before its face is drawn, nor for a panel of no chart.
#[test]
fn a_face_not_yet_drawn_or_of_no_chart_links_no_readout() {
    let chart = crate::chart::fixtures::pressure();
    let charted =
        Panel::new("chart", "Pressure", chart.summary(), on("wall-1", 0)).with_chart(chart);
    assert!(Faces::new(&charted).linked(Fit::SLOT, 60.0).is_none());
    let mut text = Faces::new(&Panel::new("text", "Notes", "Text.", on("wall-1", 1)));
    text.face(Fit::SLOT, (0, &RowMarks::default()), FRAME);
    assert!(text.linked(Fit::SLOT, 60.0).is_none());
    let mut drawn = Faces::new(&charted);
    drawn.face(Fit::SLOT, (0, &RowMarks::default()), FRAME);
    let (_, readout) = drawn.linked(Fit::SLOT, 60.0).expect("a chart drawn links");
    assert!(readout.linked);
    assert_eq!(readout.lines[0].0, "-10 min");
}

/// Of the fifteen charts on the wall of a chart pointed at, every one of them sharing its time, at most eight link.
#[test]
fn at_most_eight_charts_link_to_one_pointed_at() {
    let shown = charts(5);
    let mut projector = Projector::default();
    projector.update(input(&shown, HOME, None));
    let walls: Vec<(&Wall, Mat4)> = shown
        .walls
        .iter()
        .filter_map(|wall| model(&wall.frame()).map(|frame| (wall, frame)))
        .collect();
    let (panel_model, _) = projector
        .stands(shown.panel("chart-0").unwrap(), &walls)
        .unwrap();
    let canvas = face(&projector, "chart-0");
    let plot = projector.faces["chart-0"].sizes[0]
        .face
        .as_ref()
        .and_then(|face| face.plot.clone())
        .unwrap();
    let (width, height) = canvas.size();
    let [x, y] = [plot.left + plot.width * 0.9, plot.top + plot.height / 2.0];
    let local = Vec3::new(
        (x + canvas.padding.0 as f32) * canvas.pixel - width / 2.0,
        height / 2.0 - (y + canvas.padding.1 as f32) * canvas.pixel,
        0.0,
    );
    let ray = Ray::new(HOME, panel_model.transform_point3(local) - HOME);
    let output = projector.update(input(&shown, HOME, ray));
    assert!(output.readout.is_some());
    assert_eq!(output.linked.len(), super::super::LINKED);
    assert!(projector.linked.keys().all(|id| {
        let index: usize = id.strip_prefix("chart-").unwrap().parse().unwrap();
        (1..16).contains(&index)
    }));
}

/// A panel's and a wall's context shows as chips, drawn with their faces, which pointing at a chip names by its
/// parameter, as the ray moves over the face, without laying anything out or drawing it again.
#[test]
fn chips_show_a_panel_s_and_a_wall_s_context_and_the_ray_names_the_chip_it_meets() {
    use tomori_protocol::context::{ContextChip, ContextSetting, ContextValue};
    let region = |value: &str, setting| {
        ContextChip::new(
            "region",
            "Region",
            ContextValue::Text { text: value.into() },
            setting,
        )
    };
    let time = ContextChip::new(
        "time",
        "Time",
        ContextValue::Live { minutes: 15 },
        ContextSetting::Follows,
    );
    let chart = crate::chart::fixtures::pressure();
    let mut shown = projection(vec![
        Panel::new("pods", "Pods", "- pod-7", on("wall-1", 0))
            .with_context(vec![region("EU-WEST1", ContextSetting::Fixed)]),
        Panel::new("chart", "Pressure", chart.summary(), on("wall-1", 1))
            .with_chart(chart)
            .with_context(vec![
                region("EU-WEST1", ContextSetting::Inherited),
                time.clone(),
            ]),
        Panel::new("plain", "Plain", "x", on("wall-1", 2)),
        // A code panel, as only the agent shows, which a server never gives a context: were it given one, it draws no
        // chips and none is named.
        Panel::new("code", "main.rs", "fn main() {}", on("wall-1", 3))
            .with_code(Code::new("rust", 1))
            .with_context(vec![region("EU-WEST1", ContextSetting::Fixed)]),
    ]);
    shown.walls[1] = shown.walls[1]
        .clone()
        .with_context(vec![region("EU-WEST1", ContextSetting::Fixed)]);
    shown.walls[0] = shown.walls[0].clone().with_context(vec![time]);
    let mut projector = Projector::default();
    let output = projector.update(input(&shown, HOME, None));
    let row = |projector: &Projector, panel: &str| projector.faces[panel].sizes[0].chips.row;
    // Below its title, its chips before its metadata where both fit, or else on a row of their own below it.
    let pods = face(&projector, "pods");
    let pods_row = row(&projector, "pods");
    assert!(
        pods.rows[pods_row].plain().starts_with(" Region EU-WEST1 "),
        "{:?}",
        pods.rows[pods_row].plain()
    );
    assert!(matches!(pods.shapes[0], crate::shapes::Shape::Rect { .. }));
    let chart = face(&projector, "chart");
    let at = row(&projector, "chart");
    assert!(
        chart.rows[at].plain().contains("EU-WEST1"),
        "{}",
        chart.rows[at].plain()
    );
    assert!(!face(&projector, "plain").rows[1].plain().starts_with(' '));
    assert!(projector.faces["plain"].sizes[0].chips.spans.is_empty());
    assert!(projector.faces["code"].sizes[0].chips.spans.is_empty());
    assert!(!face(&projector, "code").plain().contains("EU-WEST1"));
    // The flat wall's board after its label, and the curved Tomori wall's label.
    let rows: Vec<String> = output
        .sheets
        .iter()
        .filter_map(|sheet| sheet.canvas.as_deref())
        .filter(|canvas| !canvas.rows.is_empty())
        .map(|canvas| canvas.rows[0].plain())
        .collect();
    assert!(
        rows.iter()
            .any(|row| row.contains("WALL   Region EU-WEST1")),
        "{rows:?}"
    );
    assert!(
        rows.iter()
            .any(|row| row.starts_with("TOMORI   Time last 15 min")),
        "{rows:?}"
    );
    // The ray from home through a place on the chart's face, in font pixels inside its padding.
    let walls: Vec<(&Wall, Mat4)> = shown
        .walls
        .iter()
        .filter_map(|wall| model(&wall.frame()).map(|frame| (wall, frame)))
        .collect();
    let towards = |projector: &Projector, panel: &str, [x, y]: [f32; 2]| {
        let (panel_model, _) = projector
            .stands(shown.panel(panel).unwrap(), &walls)
            .unwrap();
        let canvas = face(projector, panel);
        let (width, height) = canvas.size();
        let local = Vec3::new(
            (x + canvas.padding.0 as f32) * canvas.pixel - width / 2.0,
            height / 2.0 - (y + canvas.padding.1 as f32) * canvas.pixel,
            0.0,
        );
        Ray::new(HOME, panel_model.transform_point3(local) - HOME)
    };
    let cell = |column: f32, row: f32| [column * CELL_WIDTH as f32, row * CELL_HEIGHT as f32];
    let counted = counts();
    let chip = |projector: &mut Projector, panel: &str, at: [f32; 2]| {
        let ray = towards(projector, panel, at);
        projector.update(input(&shown, HOME, ray)).chip
    };
    let on = at as f32 + 0.5;
    let spans = projector.faces["chart"].sizes[0].chips.spans.clone();
    let middle = |index: usize| (spans[index].1 + spans[index].2) as f32 / 2.0;
    assert_eq!(
        chip(&mut projector, "chart", cell(middle(0), on)),
        Some("region".to_owned())
    );
    assert_eq!(
        chip(&mut projector, "chart", cell(middle(1), on)),
        Some("time".to_owned())
    );
    // Between them, on the title, below them and on a panel without chips, none.
    assert_eq!(
        chip(&mut projector, "chart", cell(spans[0].2 as f32 + 0.5, on)),
        None
    );
    assert_eq!(chip(&mut projector, "chart", cell(middle(0), 0.5)), None);
    assert_eq!(
        chip(&mut projector, "chart", cell(middle(0), on + 1.0)),
        None
    );
    assert_eq!(chip(&mut projector, "plain", cell(1.0, 1.5)), None);
    assert_eq!(chip(&mut projector, "code", cell(2.0, 1.5)), None);
    assert_eq!(chip(&mut projector, "code", cell(2.0, 2.5)), None);
    let pods_on = pods_row as f32 + 0.5;
    assert_eq!(
        chip(&mut projector, "pods", cell(2.0, pods_on)),
        Some("region".to_owned())
    );
    assert_eq!(counts(), counted, "nothing laid out or drawn again");
}

/// A curved wall's chips, and a wall's of a group, never stand beyond the wall, however long they are.
#[test]
fn a_curved_wall_s_chips_stand_within_its_row_of_slots() {
    use tomori_protocol::context::{ContextChip, ContextSetting, ContextValue};
    let long = |parameter: &str| {
        ContextChip::new(
            parameter,
            "A long label",
            ContextValue::Text {
                text: "A value as long as one may be".into(),
            },
            ContextSetting::Fixed,
        )
    };
    let mut shown = projection(Vec::new());
    shown.walls[0] = shown.walls[0]
        .clone()
        .with_context(vec![long("a"), long("b"), long("c")]);
    let mut projector = Projector::default();
    let output = projector.update(input(&shown, HOME, None));
    let label = output
        .sheets
        .iter()
        .filter_map(|sheet| sheet.canvas.as_deref())
        .find(|canvas| !canvas.rows.is_empty() && canvas.rows[0].plain().starts_with("TOMORI"))
        .expect("the Tomori wall is labelled");
    let most = super::super::arc_columns(&shown.walls[0]);
    assert!(label.columns <= most, "{} columns of {most}", label.columns);
    assert!(
        label.rows[0].plain().contains("A long"),
        "{}",
        label.rows[0].plain()
    );
}

/// A wall of a list, code, a chart, a volume and two more lists, `away` metres ahead of home, versioned `version`, and
/// a text panel floating far away beside it.
fn far_wall(away: f32, version: u64) -> Projection {
    use tomori_protocol::diagram::{Diagram, Node};
    let mut diagram = Diagram::default();
    diagram.nodes.push(Node::new("a", "A"));
    diagram.extent = [300, 300, 200];
    let code = Code::new("rust", 1).with_spans(vec![Span::new(0, 2, TokenKind::Keyword)]);
    let chart = crate::chart::fixtures::pressure();
    let list = |index: u32| {
        Panel::new(
            format!("list-{index}"),
            format!("Queue {index}"),
            format!("- [ ] first at {version}\n- [ ] second"),
            on("wall-1", index),
        )
    };
    let far = Place::Free {
        pose: Pose::upright_facing([3.0, 1.5, -away], HOME.to_array()),
    };
    let panels = vec![
        list(0),
        Panel::new("code", "main.rs", "fn main() {}", on("wall-1", 1)).with_code(code),
        Panel::new("chart", "Pressure", chart.summary(), on("wall-1", 2)).with_chart(chart),
        Panel::new("volume", "Pair", "- A", on("wall-1", 3)).with_diagram(diagram),
        list(4),
        list(5),
        Panel::new("aside", "Aside", "Floating far away.", far),
    ];
    Projection::new("s", "sky")
        .with_walls(vec![wall(Pose::upright_facing(
            [0.0, 1.5, -away],
            [0.0, 1.5, 0.0],
        ))])
        .with_panels(
            panels
                .into_iter()
                .map(|panel| panel.with_version(version))
                .collect(),
        )
}

/// Paced, as the frame step's projector is, a panel in a wall's slot too far away for its text to be read is drawn as
/// its title alone, larger, at its slot's size, laid out not at all and drawn again only as its title or frame
/// changes; one floating, and a volume, are drawn whole however far; and coming near, each is laid out to be drawn
/// whole a few a frame, the nearest first, and not again where it stays within the margin.
#[test]
fn a_panel_too_far_away_to_read_is_drawn_as_its_title_alone_until_it_comes_near() {
    let shown = far_wall(40.0, 5);
    // What the volume and the floating panel cost alone, laid out and drawn whole.
    let mut whole = shown.clone();
    whole
        .panels
        .retain(|panel| ["volume", "aside"].contains(&panel.id.as_str()));
    let before = counts();
    Projector::paced().update(input(&whole, HOME, None));
    let alone = (counts().0 - before.0, counts().1 - before.1);
    let mut projector = Projector::paced();
    let (laid, drawn) = counts();
    let output = projector.update(input(&shown, HOME, None));
    // The others are laid out not at all, and drawn as their titles.
    assert_eq!(counts(), (laid + alone.0, drawn + alone.1 + 5));
    let far: Vec<&str> = ["list-0", "code", "chart", "list-4", "list-5"]
        .into_iter()
        .filter(|id| projector.faces[*id].far)
        .collect();
    assert_eq!(far, ["list-0", "code", "chart", "list-4", "list-5"]);
    assert!(!projector.faces["volume"].far && !projector.faces["aside"].far);
    let title = output
        .sheets
        .iter()
        .filter_map(|sheet| sheet.canvas.as_ref())
        .find(|face| face.plain().contains("Queue 0"))
        .expect("drawn as its title");
    assert!(!title.plain().contains("first at"));
    assert!(title.pixel > 2.0 * PIXEL);
    // At its slot's size, to within half a font pixel, as its face drawn whole would be.
    let (width, height) = title.size();
    let (columns, rows) = super::super::fit(&shown.panels[0], Fit::SLOT);
    let whole = sized(columns, rows);
    assert!((width - whole.0).abs() < 1e-4, "{width} {whole:?}");
    assert!(
        (height - whole.1).abs() <= title.pixel / 2.0 + 1e-4,
        "{height} {whole:?}"
    );
    assert!(!projector.scrollable("list-0"));
    // Nothing of it is pointed at, nor its chart read.
    let model = Mat4::IDENTITY;
    let list = projector.faces.get_mut("list-0").unwrap();
    assert_eq!(list.item(Fit::SLOT, model, 0, Vec3::ZERO), None);
    assert_eq!(list.chip(Fit::SLOT, model, Vec3::ZERO), None);
    let chart = projector.faces.get_mut("chart").unwrap();
    assert!(chart.readout(Fit::SLOT, model, Vec3::ZERO).is_none());
    assert!(chart.linked(Fit::SLOT, 3.0).is_none());
    // What they show changing, at a new version, draws and lays out nothing, and takes no turn; a title changing draws
    // that one again.
    let mut changed = far_wall(40.0, 6);
    for (panel, was) in changed.panels.iter_mut().zip(&shown.panels) {
        if ["volume", "aside"].contains(&panel.id.as_str()) {
            panel.version = was.version;
        }
    }
    let before = counts();
    projector.update(input(&changed, HOME, None));
    assert_eq!(counts(), before);
    let mut retitled = changed.clone();
    retitled.panels[0].title = "Queue zero".into();
    projector.update(input(&retitled, HOME, None));
    assert_eq!(counts(), (before.0, before.1 + 1));
    // Between where a list is drawn far, beyond about 14.5 m, and whole again, within about 12.1 m, nothing changes,
    // and the code, of a smaller font pixel, is drawn far still.
    let between = HOME - Vec3::Z * (40.0 - 13.3);
    let before = counts();
    projector.update(input(&retitled, between, None));
    assert!(projector.faces["list-0"].far);
    assert_eq!(counts(), before);
    // Near, a few a frame are laid out to be drawn whole, the nearest first, until every one is, as they are now.
    let near = HOME - Vec3::Z * (40.0 - 2.5);
    let mut frames = 0;
    while ["list-0", "code", "chart", "list-4", "list-5"]
        .iter()
        .any(|id| projector.faces[*id].far)
    {
        let before = counts();
        projector.update(input(&retitled, near, None));
        frames += 1;
        assert!(counts().0 - before.0 <= NEARING);
    }
    assert_eq!(frames, 5_usize.div_ceil(NEARING));
    assert!(face(&projector, "list-0").plain().contains("first at 6"));
    // Far again, each is drawn as its title, forgetting what was laid out of it.
    projector.update(input(&retitled, HOME, None));
    assert!(projector.faces["list-4"].far && projector.faces["list-4"].sizes.is_empty());
}

/// A wall of one list in its first slot, `id`, of `scale` times a standard slot's size, `away` metres ahead of home and
/// `aside` to its right.
fn scaled_wall(id: &str, scale: f32, away: f32, aside: f32) -> (Wall, Panel) {
    let pose = Pose::upright_facing([aside, 1.6, -away], [aside, 1.6, 0.0]);
    let wall = Wall::new(id, WallKind::Added, pose).with_scale(scale);
    let panel = Panel::new(
        format!("{id}-list"),
        "Queue",
        "- [ ] first\n- [ ] second",
        on(id, 0),
    );
    (wall, panel)
}

/// Whether each of `projection`'s panels is drawn as its title, by identity.
fn titles(projector: &Projector, projection: &Projection) -> Vec<(String, bool)> {
    projection
        .panels
        .iter()
        .map(|panel| (panel.id.clone(), projector.faces[&panel.id].far))
        .collect()
}

/// Whether a panel is drawn as its title is decided by the angle a row of its text subtends, its slot's scale included:
/// at 12 m, where until November 2026 every standard slot was drawn as its title, a standard slot's text is drawn
/// whole; at 20 m it is drawn as its title, while a slot 2.26 times as large is drawn whole to beyond 30 m.
#[test]
fn a_larger_panel_is_drawn_whole_further_away_than_a_standard_one() {
    let lists = |away: f32| {
        let (standard, one) = scaled_wall("standard", 1.0, away, -6.0);
        let (large, two) = scaled_wall("large", 2.26, away, 6.0);
        Projection::new("s", "sky")
            .with_walls(vec![standard, large])
            .with_panels(vec![one, two])
    };
    let far = |away: f32| {
        let projection = lists(away);
        let mut projector = Projector::paced();
        projector.update(input(&projection, HOME, None));
        titles(&projector, &projection)
            .into_iter()
            .map(|(_, far)| far)
            .collect::<Vec<_>>()
    };
    assert_eq!(far(12.0), [false, false]);
    assert_eq!(far(20.0), [true, false]);
    assert_eq!(far(30.0), [true, false]);
    assert_eq!(far(36.0), [true, true]);
    // A row of a standard slot's text, 12 font pixels of 1.9 mm, is drawn as its title beyond about 14.5 m, and one
    // 2.26 times as large beyond about 33 m.
    let row = CELL_HEIGHT as f32 * PIXEL;
    assert!((row / FAR - 14.5).abs() < 0.1, "{}", row / FAR);
    assert!(
        (2.26 * row / FAR - 32.8).abs() < 0.2,
        "{}",
        2.26 * row / FAR
    );
}

/// A wall's content scale sizes its text, and so the angle a row of it subtends: at a content scale of 2, a slot's text
/// is drawn whole at about 21 m, where at 1 it is drawn as its title; at a half, as its title at about 12 m, where at 1
/// it is drawn whole.
#[test]
fn a_wall_s_content_scale_changes_how_far_its_panels_are_drawn_whole() {
    let far = |away: f32, content: f32| {
        let (standard, one) = scaled_wall("standard", 1.0, away, -6.0);
        let (scaled, two) = scaled_wall("scaled", 1.0, away, 6.0);
        let projection = Projection::new("s", "sky")
            .with_walls(vec![standard, scaled.with_content_scale(Some(content))])
            .with_panels(vec![one, two]);
        assert_eq!(projection.walls[1].text_scale(), content);
        let mut projector = Projector::paced();
        projector.update(input(&projection, HOME, None));
        titles(&projector, &projection)
            .into_iter()
            .map(|(_, far)| far)
            .collect::<Vec<_>>()
    };
    assert_eq!(far(20.0, 2.0), [true, false]);
    assert_eq!(far(10.0, 0.5), [false, true]);
}

/// A panel's face, drawn whole or as its title, is drawn as a panel's face, which the renderer counts apart from every
/// other surface, as a wall's board is.
#[test]
fn a_panel_s_faces_are_drawn_as_panel_faces() {
    use crate::canvas::Kind;
    let face_of = |away: f32| {
        let (wall, panel) = scaled_wall("wall-1", 1.0, away, 0.0);
        let projection = Projection::new("s", "sky")
            .with_walls(vec![wall])
            .with_panels(vec![panel]);
        let output = Projector::paced().update(input(&projection, HOME, None));
        let kinds: Vec<(bool, bool)> = output
            .sheets
            .iter()
            .filter_map(|sheet| sheet.canvas.as_ref())
            .map(|canvas| {
                (
                    canvas.plain().contains("Queue"),
                    matches!(canvas.kind, Kind::PanelFace),
                )
            })
            .collect();
        kinds
    };
    for away in [3.0, 30.0] {
        let kinds = face_of(away);
        assert!(kinds.contains(&(true, true)), "{away}: {kinds:?}");
        assert!(
            kinds.iter().all(|(queue, panel)| queue == panel),
            "{away}: {kinds:?}"
        );
    }
}

/// A panel drawn as its title is drawn whole again only once a row of its text subtends a fifth more than the angle it
/// went at, so that a head swaying about the threshold does not switch it back and forth.
#[test]
fn a_panel_drawn_as_its_title_is_drawn_whole_again_only_past_the_margin() {
    let (wall, panel) = scaled_wall("wall-1", 1.0, 30.0, 0.0);
    let projection = Projection::new("s", "sky")
        .with_walls(vec![wall])
        .with_panels(vec![panel]);
    let mut projector = Projector::paced();
    let mut far_at = |distance: f32| {
        let viewer = Vec3::new(0.0, 1.6, -30.0 + distance);
        projector.update(input(&projection, viewer, None));
        projector.faces["wall-1-list"].far
    };
    // Whole until beyond 14.5 m; then drawn as its title until within 12.1 m; then whole until beyond 14.5 m again.
    assert!(!far_at(14.0));
    assert!(far_at(15.0));
    assert!(far_at(13.0));
    assert!(far_at(12.3));
    assert!(!far_at(11.8));
    assert!(!far_at(13.0));
    assert!(!far_at(14.3));
    assert!(far_at(14.8));
    assert!((NEAR / FAR - 1.2).abs() < 1e-6);
}

/// Turned off, as `tomori.panel_lod off` turns it off, no panel is drawn as its title however far, and turned off while
/// some are, every one is drawn whole again at once, not a few a frame; turned on again, they are drawn as their titles.
#[test]
fn with_the_level_of_detail_off_every_panel_is_drawn_whole() {
    let shown = far_wall(40.0, 1);
    let mut projector = Projector::paced();
    assert_eq!(projector.lod(), PanelLod::Angle);
    projector.update(input(&shown, HOME, None));
    let far = || {
        ["list-0", "code", "chart", "list-4", "list-5"]
            .into_iter()
            .map(|id| projector.faces[id].far)
            .collect::<Vec<_>>()
    };
    assert_eq!(far(), [true; 5]);
    projector.set_lod(PanelLod::Off);
    assert_eq!(projector.lod(), PanelLod::Off);
    let output = projector.update(input(&shown, HOME, None));
    let far = || {
        ["list-0", "code", "chart", "list-4", "list-5"]
            .into_iter()
            .map(|id| projector.faces[id].far)
            .collect::<Vec<_>>()
    };
    assert_eq!(far(), [false; 5]);
    assert!(output.sheets.iter().any(|sheet| {
        sheet
            .canvas
            .as_ref()
            .is_some_and(|face| face.plain().contains("first at 1"))
    }));
    let mut fresh = Projector::paced();
    fresh.set_lod(PanelLod::Off);
    fresh.update(input(&shown, HOME, None));
    assert!(fresh.faces.values().all(|faces| !faces.far));
    projector.set_lod(PanelLod::Angle);
    projector.update(input(&shown, HOME, None));
    assert!(projector.faces["list-0"].far);
}

/// The frame log counts the panels handed over as their titles each frame.
#[test]
fn the_panels_drawn_as_titles_are_counted_for_the_frame_log() {
    let shown = far_wall(40.0, 1);
    let mut projector = Projector::paced();
    crate::frame_timing::take();
    projector.update(input(&shown, HOME, None));
    assert_eq!(crate::frame_timing::take().titles, 5);
    projector.set_lod(PanelLod::Off);
    projector.update(input(&shown, HOME, None));
    assert_eq!(crate::frame_timing::take().titles, 0);
}

/// The level of detail is read from a client's setting, `off` or `angle`, and said as it starts.
#[test]
fn the_level_of_detail_is_parsed_and_described() {
    crate::test_support::log_everything();
    for (value, lod) in [
        (None, PanelLod::Angle),
        (Some("angle"), PanelLod::Angle),
        (Some(" ON "), PanelLod::Angle),
        (Some("off"), PanelLod::Off),
        (Some("None"), PanelLod::Off),
        (Some("0"), PanelLod::Off),
        (Some("sideways"), PanelLod::Angle),
    ] {
        assert_eq!(PanelLod::parse(value), lod, "{value:?}");
    }
    assert_eq!(PanelLod::Off.to_string(), "off");
    assert_eq!(PanelLod::Angle.to_string(), "angle");
    assert!(
        PanelLod::Off
            .describe()
            .ends_with("off, every panel drawn whole")
    );
    assert!(
        PanelLod::Angle
            .describe()
            .contains("under 0.090°, whole again over 0.108°")
    );
}

/// Paced, of the panels that arrive together, more than a frame lays out, those nearest stand first: the nearest
/// wall's, then those floating as near, then the farther wall's, and last any on a wall not shown or in a place this
/// client does not know.
#[test]
fn panels_arriving_together_stand_the_nearest_first() {
    let mut shown = lists(5);
    // The far wall first in the projection's order, then the near one.
    shown.walls[0].pose = Pose::upright_facing([0.0, 1.2, -4.0], [0.0, 1.2, 0.0]);
    shown.walls[1].pose = Pose::upright_facing([0.0, 2.4, -3.0], [0.0, 2.4, 0.0]);
    shown
        .panels
        .push(Panel::new("lost", "Lost", "Elsewhere.", on("gone", 0)).with_version(5));
    shown
        .panels
        .push(Panel::new("odd", "Odd", "Unknown.", Place::Unknown).with_version(5));
    let mut projector = Projector::paced();
    projector.update(input(&shown, HOME, None));
    let standing: Vec<&str> = shown
        .panels
        .iter()
        .filter(|panel| projector.faces.contains_key(&panel.id))
        .map(|panel| panel.id.as_str())
        .collect();
    // The near wall's, chart-16 to chart-23.
    let near: Vec<String> = (16..16 + ARRIVALS)
        .map(|index| format!("chart-{index}"))
        .collect();
    assert_eq!(standing, near);
    // In turn, every one stands, those nowhere last.
    for _ in 0..(shown.panels.len().div_ceil(ARRIVALS)) {
        projector.update(input(&shown, HOME, None));
    }
    assert!(
        shown
            .panels
            .iter()
            .all(|panel| projector.faces.contains_key(&panel.id))
    );
}
