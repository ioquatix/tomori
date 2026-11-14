// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

use tomori_protocol::{Panel, Place, TOMORI_WALL, Wall, WallKind};

use super::*;

fn on(slot: u32) -> Place {
    Place::Wall {
        wall: TOMORI_WALL.into(),
        slot,
    }
}

/// The Tomori wall with an agenda projected `age` seconds ago and notes, at `revision`.
fn projection(revision: u64, age: u64) -> Projection {
    let pose = tomori_protocol::Pose::default();
    Projection::new("s", "sky")
        .with_walls(vec![Wall::new(TOMORI_WALL, WallKind::Tomori, pose)])
        .with_panels(vec![
            Panel::new("agenda", "Agenda", "- [ ] One\n- [ ] Two", on(0))
                .with_version(1)
                .with_age(age),
            Panel::new("notes", "Notes", "Some notes.", on(1)).with_version(1),
        ])
        .with_revision(revision)
}

/// The patch from `before` to `after`.
fn patch(before: &Projection, after: &Projection) -> ProjectionPatch {
    ProjectionPatch::between(before, after)
}

#[test]
fn a_patch_applies_from_the_revision_held_and_a_gap_asks_once_for_the_projection_whole() {
    crate::test_support::log_everything();
    let now = Instant::now();
    let mut held = Held::default();
    // Holding nothing, as before the projection whole arrives, a patch asks for it.
    let first = projection(3, 0);
    assert_eq!(
        held.patch(&patch(&first, &first.clone().with_revision(4)), now),
        Some(0)
    );
    held.whole(first.clone(), now);
    assert_eq!(held.projection(), Some(&first));
    // A patch from the revision held applies.
    let mut second = first.clone().with_revision(4);
    second.panels.remove(1);
    assert_eq!(held.patch(&patch(&first, &second), now), None);
    assert_eq!(held.projection(), Some(&second));
    // One from another, after one was missed, does not, keeps what is held and asks once.
    let mut third = second.clone().with_revision(5);
    third.panels[0].place = Place::Free {
        pose: tomori_protocol::Pose::default(),
    };
    let mut fourth = third.clone().with_revision(6);
    fourth.panels[0].title = "Agenda, revised".into();
    assert_eq!(held.patch(&patch(&third, &fourth), now), Some(4));
    assert_eq!(held.patch(&patch(&third, &fourth), now), None);
    assert_eq!(held.projection(), Some(&second));
    // Sent whole, it patches from there again.
    held.whole(fourth.clone(), now);
    let fifth = fourth.clone().with_revision(9);
    assert_eq!(held.patch(&patch(&fourth, &fifth), now), None);
    assert_eq!(held.projection().unwrap().revision, 9);
    // One naming what it does not hold asks too.
    let missing = ProjectionPatch::new(
        "s",
        "sky",
        9,
        10,
        vec![PatchOp::RemovePanel {
            panel: "gone".into(),
        }],
    );
    assert_eq!(held.patch(&missing, now), Some(9));
    assert_eq!(held.projection().unwrap().revision, 9);
    // One refused partway, after a panel it added and one it removed, keeps what is held, not half patched, and is
    // patched from there once sent whole again; a later patch from the revision held then applies, every panel's age
    // counted.
    let held_now = held.projection().cloned().unwrap();
    let mut partway = fifth.clone().with_revision(10);
    partway.panels.remove(0);
    partway
        .panels
        .push(Panel::new("new", "New", "New.", on(2)).with_version(1));
    let mut refused = patch(&fifth, &partway);
    refused.ops.push(PatchOp::Unknown);
    held.whole(fifth.clone(), now);
    assert_eq!(held.patch(&refused, now), Some(9));
    assert_eq!(held.projection(), Some(&held_now));
    assert_eq!(held.patch(&patch(&fifth, &partway), now), None);
    assert_eq!(held.projection(), Some(&partway));
    held.clear();
    assert_eq!(held.projection(), None);
}

#[test]
fn a_panel_not_sent_again_is_shown_as_old_as_it_now_is() {
    let then = Instant::now();
    let later = then + Duration::from_secs(90);
    let mut held = Held::default();
    let first = projection(1, 60);
    held.whole(first.clone(), then);
    // Ninety seconds on, a patch that sends the notes again but not the agenda: the agenda is shown 150 seconds old,
    // the notes as old as the patch says.
    let mut second = first.clone().with_revision(2);
    second.panels[1].body = "More notes.".into();
    second.panels[1].age = 5;
    held.patch(&patch(&first, &second), later);
    let ages: Vec<u64> = held
        .projection()
        .unwrap()
        .panels
        .iter()
        .map(|panel| panel.age)
        .collect();
    assert_eq!(ages, [150, 5]);
    // An age longer than this device has run is counted from when it arrived.
    assert_eq!(since(then, u64::MAX), then);
}

#[test]
fn a_tick_shown_at_once_is_undone_by_whatever_the_server_sends_next() {
    let now = Instant::now();
    let mut held = Held::default();
    let first = projection(1, 0);
    held.whole(first.clone(), now);
    held.tick("agenda", 0, true);
    held.tick("agenda", 1, true);
    assert_eq!(
        held.projection().unwrap().panels[0].body,
        "- [x] One\n- [x] Two"
    );
    // A tick the server refused is sent again as a patch of nothing, which shows the panel as the server has it.
    let again = first.clone().with_revision(2);
    assert_eq!(held.patch(&patch(&first, &again), now), None);
    assert_eq!(
        held.projection().unwrap().panels[0].body,
        "- [ ] One\n- [ ] Two"
    );
    // One it took is sent as the panel, ticked.
    held.tick("agenda", 0, true);
    let mut ticked = again.clone().with_revision(3);
    ticked.panels[0].body = "- [x] One\n- [ ] Two".into();
    ticked.panels[0].version = 2;
    assert_eq!(held.patch(&patch(&again, &ticked), now), None);
    assert_eq!(held.projection(), Some(&ticked));
    // Sent whole, it is as sent.
    held.tick("agenda", 1, true);
    held.whole(ticked.clone(), now);
    assert_eq!(held.projection(), Some(&ticked));
}

#[test]
fn what_no_server_sends_never_fails_the_client() {
    let now = Instant::now();
    let mut held = Held::default();
    let first = projection(1, 60);
    held.whole(first.clone(), now);
    // A tick kept of a panel no longer held, and a panel held whose projection was never noted, which a server cannot
    // bring about: the patch still applies, the panel keeps the age it was sent with, and the other is counted on.
    held.ticked.insert("gone".into(), "Gone.".into());
    held.projected.remove("agenda");
    let second = first.clone().with_revision(2);
    assert_eq!(
        held.patch(&patch(&first, &second), now + Duration::from_secs(5)),
        None
    );
    let ages: Vec<u64> = held
        .projection()
        .unwrap()
        .panels
        .iter()
        .map(|panel| panel.age)
        .collect();
    assert_eq!(ages, [60, 5]);
}
