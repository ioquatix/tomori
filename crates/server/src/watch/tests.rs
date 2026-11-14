// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

use super::*;

const MINUTE: u64 = 60_000;
/// A time well after the epoch, from which these tests count.
const T: u64 = 1_800_000_000_000;

fn worker(id: &str, title: &str, state: WorkerState) -> Worker {
    Worker {
        id: id.to_owned(),
        parent: None,
        title: title.to_owned(),
        state,
    }
}

fn active(progressed: u64) -> WorkerActivity {
    WorkerActivity {
        progressed,
        ..WorkerActivity::default()
    }
}

/// The notes told at `now`, after judging `workers` then.
fn told(watch: &mut Watch, now: u64, workers: &[Worker]) -> Vec<String> {
    watch.tick(now, None, workers);
    watch
        .take("opencode", workers, true)
        .into_iter()
        .map(|(_, note)| note)
        .collect()
}

#[test]
fn a_failed_worker_is_told_at_once_with_why() {
    let mut watch = Watch::default();
    let workers = [worker("ses_1", "Deck concepts", WorkerState::Running)];
    assert!(!watch.watching());
    // A worker never seen active since the server started is not judged, whatever its state.
    assert!(
        told(
            &mut watch,
            T,
            &[worker("ses_1", "Deck", WorkerState::Error)]
        )
        .is_empty()
    );
    watch.observe(
        "ses_1".into(),
        WorkerActivity {
            error: Some("BadRequestError: insufficient credit".into()),
            ..active(T)
        },
    );
    assert!(watch.watching());
    assert!(told(&mut watch, T, &workers).is_empty());
    let failed = [worker("ses_1", "Deck concepts", WorkerState::Error)];
    assert!(watch.tick(T + 1, None, &failed));
    assert_eq!(watch.condition("ses_1").unwrap().kind, Kind::Failed);
    // It shows as failed already, so it is shown as it is.
    assert_eq!(watch.shown(&failed[0]).state, WorkerState::Error);
    assert_eq!(
        told(&mut watch, T + 1, &failed),
        [
            "Worker \"Deck concepts\" (opencode session ses_1) failed: BadRequestError: insufficient credit"
        ]
    );
    // Told once, however long it lasts.
    assert!(told(&mut watch, T + 60 * MINUTE, &failed).is_empty());
    // Without a reason, it says so.
    let mut watch = Watch::default();
    watch.observe("ses_1".into(), active(T));
    assert_eq!(
        told(&mut watch, T, &failed),
        ["Worker \"Deck concepts\" (opencode session ses_1) failed"]
    );
}

#[test]
fn a_message_not_answered_for_five_minutes_is_told_with_how_long_ago_it_was_sent() {
    let mut watch = Watch::default();
    let workers = [worker("ses_1", "Deck concepts", WorkerState::Completed)];
    watch.observe(
        "ses_1".into(),
        WorkerActivity {
            message: Some(T),
            unanswered_since: Some(T),
            ..active(T - 30 * MINUTE)
        },
    );
    assert!(!watch.tick(T + 5 * MINUTE - 1, None, &workers));
    assert!(watch.take("opencode", &workers, true).is_empty());
    assert!(watch.tick(T + 5 * MINUTE, None, &workers));
    let shown = watch.shown(&workers[0]);
    assert_eq!(shown.state, WorkerState::Unanswered);
    assert_eq!(
        told(&mut watch, T + 20 * MINUTE, &workers),
        [
            "Worker \"Deck concepts\" (opencode session ses_1) has not answered a message sent 20 min ago"
        ]
    );
    // Waiting for a permission or a question, it is not unanswered.
    watch.observe(
        "ses_1".into(),
        WorkerActivity {
            message: Some(T),
            unanswered_since: Some(T),
            waiting: true,
            ..active(T)
        },
    );
    assert!(watch.judge(&workers));
    assert_eq!(watch.condition("ses_1"), None);
    assert_eq!(watch.shown(&workers[0]).state, WorkerState::Completed);
}

#[test]
fn a_worker_without_progress_stalls_after_ten_minutes_or_thirty_while_a_tool_runs() {
    let mut watch = Watch::default();
    let workers = [
        worker("ses_1", "Build", WorkerState::Running),
        worker("ses_2", "Review", WorkerState::Running),
    ];
    watch.observe(
        "ses_1".into(),
        WorkerActivity {
            tool: Some(RunningTool {
                name: "shell".into(),
                since: T,
            }),
            ..active(T)
        },
    );
    watch.observe(
        "ses_2".into(),
        WorkerActivity {
            finish: Some("tool-calls".into()),
            ..active(T)
        },
    );
    assert!(told(&mut watch, T + 10 * MINUTE - 1, &workers).is_empty());
    assert_eq!(
        told(&mut watch, T + 10 * MINUTE, &workers),
        [
            "Worker \"Review\" (opencode session ses_2) has shown no progress for 10 min, its last step ended with tool calls"
        ]
    );
    // A long command is not flagged before half an hour.
    assert_eq!(watch.condition("ses_1"), None);
    assert!(told(&mut watch, T + 30 * MINUTE - 1, &workers).is_empty());
    assert_eq!(watch.condition("ses_1"), None);
    watch.tick(T + 30 * MINUTE, None, &workers);
    assert_eq!(watch.shown(&workers[0]).state, WorkerState::Stalled);
    assert_eq!(
        watch.take("opencode", &workers, true)[0].1,
        "Worker \"Build\" (opencode session ses_1) has been running shell for 30 min with no other progress"
    );
    // A finished worker, or one waiting, does not stall.
    let finished = [
        worker("ses_1", "Build", WorkerState::Completed),
        worker("ses_2", "Review", WorkerState::Waiting),
    ];
    watch.tick(T + 60 * MINUTE, None, &finished);
    assert_eq!(watch.condition("ses_1"), None);
    assert_eq!(watch.condition("ses_2"), None);
}

#[test]
fn a_worker_is_told_again_only_after_it_recovers() {
    let mut watch = Watch::default();
    let workers = [worker("ses_1", "Build", WorkerState::Running)];
    watch.observe("ses_1".into(), active(T));
    assert_eq!(told(&mut watch, T + 10 * MINUTE, &workers).len(), 1);
    assert!(told(&mut watch, T + 20 * MINUTE, &workers).is_empty());
    // It makes progress: recovered, as shown at once.
    watch.observe("ses_1".into(), active(T + 21 * MINUTE));
    assert!(watch.judge(&workers));
    assert_eq!(watch.shown(&workers[0]).state, WorkerState::Running);
    assert!(told(&mut watch, T + 25 * MINUTE, &workers).is_empty());
    // And stalls again: a new episode, told again, but only after twice as long.
    assert!(told(&mut watch, T + 31 * MINUTE, &workers).is_empty());
    assert_eq!(
        told(&mut watch, T + 41 * MINUTE, &workers),
        ["Worker \"Build\" (opencode session ses_1) has shown no progress for 20 min"]
    );
    // Its thresholds keep doubling, to eight times, until it completes cleanly.
    for (episode, wait) in [(2, 40), (3, 80), (4, 80)] {
        let at = T + (100 + 100 * episode) * MINUTE;
        watch.observe("ses_1".into(), active(at));
        assert!(told(&mut watch, at + (wait - 1) * MINUTE, &workers).is_empty());
        assert_eq!(
            told(&mut watch, at + wait * MINUTE, &workers).len(),
            1,
            "{episode}"
        );
    }
    watch.tick(
        T + 900 * MINUTE,
        None,
        &[worker("ses_1", "Build", WorkerState::Completed)],
    );
    watch.observe("ses_1".into(), active(T + 900 * MINUTE));
    assert_eq!(told(&mut watch, T + 910 * MINUTE, &workers).len(), 1);
    // A worker that recovers before it is told is not told.
    let lint = [worker("ses_2", "Lint", WorkerState::Running)];
    watch.observe("ses_2".into(), active(T + 920 * MINUTE));
    watch.tick(T + 930 * MINUTE, None, &lint);
    assert!(watch.due());
    watch.observe("ses_2".into(), active(T + 930 * MINUTE));
    watch.judge(&lint);
    assert!(!watch.due());
    assert!(watch.take("opencode", &lint, true).is_empty());
}

#[test]
fn workers_in_one_condition_are_one_note_and_notes_are_paced() {
    let mut watch = Watch::default();
    let titles = ["A", "B", "C", "D", "E", "F", "G"];
    let workers: Vec<Worker> = titles
        .iter()
        .enumerate()
        .map(|(index, title)| worker(&format!("ses_{index}"), title, WorkerState::Error))
        .collect();
    for worker in &workers {
        watch.observe(
            worker.id.clone(),
            WorkerActivity {
                error: Some("BadRequestError: insufficient credit".into()),
                ..active(T)
            },
        );
    }
    assert_eq!(
        told(&mut watch, T, &workers[..1]),
        ["Worker \"A\" (opencode session ses_0) failed: BadRequestError: insufficient credit"]
    );
    // The rest fail a minute later: a failure is told at once, together, naming five and counting the rest.
    assert_eq!(
        told(&mut watch, T + MINUTE, &workers),
        [
            "Workers \"B\" (opencode session ses_1), \"C\" (opencode session ses_2), \"D\" (opencode session ses_3), \"E\" (opencode session ses_4), \"F\" (opencode session ses_5) and 1 more failed: BadRequestError: insufficient credit"
        ]
    );
    // Different reasons are given with each worker; several conditions are a note each, in one delivery.
    let mut watch = Watch::default();
    let workers = [
        worker("ses_a", "A", WorkerState::Error),
        worker("ses_b", "B", WorkerState::Error),
        worker("ses_c", "C", WorkerState::Running),
        worker("ses_d", "D", WorkerState::Running),
        worker("ses_e", "E", WorkerState::Running),
        worker("ses_f", "F", WorkerState::Running),
    ];
    watch.observe(
        "ses_a".into(),
        WorkerActivity {
            error: Some("Overloaded".into()),
            ..active(T)
        },
    );
    watch.observe("ses_b".into(), active(T));
    for id in ["ses_c", "ses_d"] {
        watch.observe(
            id.into(),
            WorkerActivity {
                message: Some(T),
                unanswered_since: Some(T),
                ..active(T)
            },
        );
    }
    watch.observe(
        "ses_e".into(),
        WorkerActivity {
            tool: Some(RunningTool {
                name: "shell".into(),
                since: T - 20 * MINUTE,
            }),
            ..active(T - 20 * MINUTE)
        },
    );
    watch.observe(
        "ses_f".into(),
        WorkerActivity {
            finish: Some("length".into()),
            ..active(T)
        },
    );
    assert_eq!(
        told(&mut watch, T + 10 * MINUTE, &workers),
        [
            "Workers \"A\" (opencode session ses_a, Overloaded) and \"B\" (opencode session ses_b) failed",
            "Workers \"C\" (opencode session ses_c, sent 10 min ago) and \"D\" (opencode session ses_d, sent 10 min ago) have not answered a message",
            "Workers \"E\" (opencode session ses_e, running shell for 30 min) and \"F\" (opencode session ses_f, no progress for 10 min, its last step ended at its length limit) have shown no progress",
        ]
    );
}

#[test]
fn what_a_note_says_is_bounded_and_cannot_pass_for_anything_else() {
    let mut watch = Watch::default();
    let workers = [
        worker(
            "ses_1",
            &format!("Say \"hi\" [now]{}", "x".repeat(200)),
            WorkerState::Running,
        ),
        worker("ses_2", "Two", WorkerState::Running),
    ];
    watch.observe(
        "ses_1".into(),
        WorkerActivity {
            finish: Some("stop".into()),
            ..active(T)
        },
    );
    watch.observe(
        "ses_2".into(),
        WorkerActivity {
            finish: Some("weird\"one".into()),
            ..active(T)
        },
    );
    let notes = told(&mut watch, T + 10 * MINUTE, &workers);
    assert_eq!(notes.len(), 1);
    assert!(
        notes[0].starts_with("Workers \"Say 'hi' (now)xxx"),
        "{}",
        notes[0]
    );
    assert!(notes[0].contains("its last step ended normally"));
    assert!(notes[0].contains("its last step ended as 'weird'one'"));
    assert!(!notes[0].contains('['));
    assert_eq!(ended("error"), "in an error");
    assert_eq!(listed(&[]), "");
    assert_eq!(listed(&["\"A\"".to_owned()]), "\"A\"");
    assert_eq!(minutes(0), "1 min");
    assert_eq!(Kind::Stalled.name(), "stalled");
    assert_eq!(Kind::Failed.name(), "failed");
    assert_eq!(Kind::Unanswered.name(), "unanswered");
}

#[test]
fn activity_is_bounded_and_forgotten_with_its_worker() {
    let mut watch = Watch::default();
    for index in 0..MOST + 1 {
        watch.observe(format!("ses_{index}"), active(T));
    }
    assert_eq!(watch.activity.len(), MOST);
    watch.observe("ses_0".into(), active(T + 1));
    assert_eq!(watch.activity["ses_0"].progressed, T + 1);
    watch.retain(&[worker("ses_0", "Kept", WorkerState::Running)]);
    assert_eq!(watch.activity.len(), 1);
    // The clock never goes back.
    watch.tick(T + MINUTE, None, &[]);
    watch.tick(T, None, &[]);
    assert_eq!(watch.now, T + MINUTE);
}

#[test]
fn stalls_are_paced_but_a_failure_is_told_at_once() {
    let mut watch = Watch::default();
    let workers = [
        worker("ses_1", "One", WorkerState::Running),
        worker("ses_2", "Two", WorkerState::Running),
        worker("ses_3", "Three", WorkerState::Running),
    ];
    watch.observe("ses_1".into(), active(T));
    watch.observe("ses_2".into(), active(T + MINUTE));
    watch.observe("ses_3".into(), active(T + MINUTE));
    assert_eq!(told(&mut watch, T + 10 * MINUTE, &workers).len(), 1);
    // The second stalls a minute later: it waits for the pace.
    assert!(told(&mut watch, T + 11 * MINUTE, &workers[..2]).is_empty());
    assert!(!watch.due());
    // The third fails meanwhile: it is told at once, and the stall still waits.
    let failing = [
        workers[0].clone(),
        workers[1].clone(),
        worker("ses_3", "Three", WorkerState::Error),
    ];
    assert_eq!(
        told(&mut watch, T + 11 * MINUTE, &failing),
        ["Worker \"Three\" (opencode session ses_3) failed"]
    );
    assert_eq!(
        told(&mut watch, T + 12 * MINUTE, &failing),
        ["Worker \"Two\" (opencode session ses_2) has shown no progress for 11 min"]
    );
}

#[test]
fn notes_are_capped_in_a_day_and_while_nobody_is_there() {
    crate::test_log::record();
    let mut watch = Watch::default();
    let workers = [worker("ses_1", "One", WorkerState::Running)];
    // The worker stalls from `at`, after completing cleanly, which keeps its plain threshold: the notes told.
    let stall = |watch: &mut Watch, at: &mut u64, present: bool| {
        watch.observe("ses_1".into(), active(*at));
        *at += 10 * MINUTE;
        watch.tick(
            *at - MINUTE,
            None,
            &[worker("ses_1", "One", WorkerState::Completed)],
        );
        watch.tick(*at, None, &workers);
        watch.take("opencode", &workers, present).len()
    };
    let mut at = T;
    // While nobody is in the space, twelve are told, and the rest only counted.
    let unattended: usize = (0..UNATTENDED)
        .map(|_| stall(&mut watch, &mut at, false))
        .sum();
    assert_eq!(unattended, UNATTENDED);
    // Then the agent is told, once, that notes are paused, and until when, and its backoff does not grow.
    watch.observe("ses_1".into(), active(at));
    at += 10 * MINUTE;
    watch.tick(
        at - MINUTE,
        None,
        &[worker("ses_1", "One", WorkerState::Completed)],
    );
    watch.tick(at, None, &workers);
    let paused = watch.take("opencode", &workers, false);
    assert_eq!(paused.len(), 1);
    assert_eq!(paused[0].0, "paused");
    assert!(
        paused[0]
            .1
            .starts_with(&format!("Notes about workers are paused until {}, having reached the most while nobody is in the space, 12", clock_time(T + 10 * MINUTE + DAY))),
        "{}",
        paused[0].1
    );
    assert!(watch.take("opencode", &workers, false).is_empty());
    assert_eq!(watch.episodes.get("ses_1"), None);
    // With someone there, what waited is told, up to the day's most, and then the pause once.
    let attended: usize = (0..DAILY).map(|_| stall(&mut watch, &mut at, true)).sum();
    assert_eq!(attended, DAILY - UNATTENDED + 1);
    assert!(
        watch.take("opencode", &workers, true).is_empty(),
        "paused already said"
    );
    // A day after the first, they are told again.
    let mut at = T + DAY + 10 * MINUTE * (UNATTENDED as u64 + 2);
    assert_eq!(stall(&mut watch, &mut at, true), 1);
    assert!(clock_time(T).contains(':'));
    assert_eq!(clock_time(u64::MAX), "tomorrow");
}

#[test]
fn a_sleep_or_a_jump_of_the_clock_is_not_time_without_progress() {
    crate::test_log::record();
    let mut watch = Watch::default();
    let workers = [
        worker("ses_1", "One", WorkerState::Running),
        worker("ses_2", "Two", WorkerState::Running),
    ];
    let start = std::time::Instant::now();
    // The second stalled, and was told, before the sleep.
    watch.observe("ses_2".into(), active(T - 20 * MINUTE));
    watch.observe(
        "ses_1".into(),
        WorkerActivity {
            message: Some(T),
            unanswered_since: Some(T),
            tool: Some(RunningTool {
                name: "shell".into(),
                since: T,
            }),
            ..active(T)
        },
    );
    watch.tick(T + MINUTE, Some(start), &workers);
    assert_eq!(watch.take("opencode", &workers, true).len(), 1);
    // Time passing as both clocks agree is not a jump.
    let start = start + std::time::Duration::from_secs(60);
    watch.tick(T + 2 * MINUTE, Some(start), &workers);
    assert!(watch.jumps.is_empty());
    // The laptop sleeps eight hours: the wall clock moved, the monotonic one did not.
    let woke = start + std::time::Duration::from_secs(1);
    watch.tick(T + 2 * MINUTE + 8 * 60 * MINUTE, Some(woke), &workers);
    assert_eq!(watch.condition("ses_1"), None);
    // The second, stalled before it, still is, and is not told again.
    assert!(watch.take("opencode", &workers, true).is_empty());
    assert_eq!(
        watch.activity["ses_1"].progressed,
        T + 8 * 60 * MINUTE - 1_000
    );
    // A report from before the sleep is moved likewise; one from after it is not.
    watch.observe("ses_1".into(), active(T + 30_000));
    assert_eq!(
        watch.activity["ses_1"].progressed,
        T + 8 * 60 * MINUTE + 29_000
    );
    let after = T + 9 * 60 * MINUTE;
    watch.observe("ses_1".into(), active(after));
    assert_eq!(watch.activity["ses_1"].progressed, after);
    // The clock set back an hour moves them back.
    let later = woke + std::time::Duration::from_secs(60);
    watch.tick(T + 8 * 60 * MINUTE - 60 * MINUTE, Some(later), &workers);
    assert!(watch.activity["ses_1"].progressed < T + 8 * 60 * MINUTE);
    // Many jumps are remembered only so far.
    let mut now = T + 8 * 60 * MINUTE;
    let mut steady = later;
    for _ in 0..JUMPS + 2 {
        now += 10 * MINUTE;
        steady += std::time::Duration::from_secs(1);
        watch.tick(now, Some(steady), &workers);
    }
    assert_eq!(watch.jumps.len(), JUMPS);
    assert_eq!(moved(5, -10), 0);
}

#[test]
fn nothing_is_judged_while_activity_is_unknown_and_what_was_told_stays_told() {
    let mut watch = Watch::default();
    let workers = [worker("ses_1", "One", WorkerState::Running)];
    // A worker whose last progress is not known is not judged.
    watch.observe("ses_1".into(), WorkerActivity::default());
    watch.tick(T, None, &workers);
    assert_eq!(watch.condition("ses_1"), None);
    watch.observe("ses_1".into(), active(T));
    assert_eq!(told(&mut watch, T + 10 * MINUTE, &workers).len(), 1);
    // The backend loses its connection: nothing is judged, and once it reports the worker again, still stalled, it is
    // not told again.
    watch.unknown();
    assert!(!watch.watching());
    watch.tick(T + 20 * MINUTE, None, &workers);
    assert_eq!(watch.condition("ses_1"), None);
    watch.observe("ses_1".into(), active(T));
    assert!(told(&mut watch, T + 21 * MINUTE, &workers).is_empty());
    assert_eq!(watch.condition("ses_1").unwrap().kind, Kind::Stalled);
}

#[test]
fn a_message_given_while_a_tool_runs_has_as_long_as_a_stall_and_says_when_it_was_sent() {
    let mut watch = Watch::default();
    let workers = [worker("ses_1", "Build", WorkerState::Running)];
    watch.observe(
        "ses_1".into(),
        WorkerActivity {
            message: Some(T - 3 * MINUTE),
            unanswered_since: Some(T),
            tool: Some(RunningTool {
                name: "shell".into(),
                since: T - MINUTE,
            }),
            ..active(T)
        },
    );
    assert!(told(&mut watch, T + 29 * MINUTE, &workers).is_empty());
    assert_eq!(
        told(&mut watch, T + 30 * MINUTE, &workers),
        ["Worker \"Build\" (opencode session ses_1) has not answered a message sent 33 min ago"]
    );
}

/// A follow-up queued while a worker runs does not stop it being judged: a worker that then hangs, mid-step or
/// mid-tool, stalls as any does, and the note says the message is still waiting.
#[test]
fn a_worker_that_hangs_with_a_follow_up_queued_still_stalls() {
    let workers = [
        worker("ses_1", "Deck", WorkerState::Running),
        worker("ses_2", "Build", WorkerState::Running),
    ];
    let mut watch = Watch::default();
    // Queued behind the run, not yet the worker's to answer.
    watch.observe(
        "ses_1".into(),
        WorkerActivity {
            message: Some(T + 2 * MINUTE),
            ..active(T)
        },
    );
    watch.observe(
        "ses_2".into(),
        WorkerActivity {
            message: Some(T + 2 * MINUTE),
            tool: Some(RunningTool {
                name: "shell".into(),
                since: T,
            }),
            ..active(T)
        },
    );
    assert!(told(&mut watch, T + 10 * MINUTE - 1, &workers).is_empty());
    assert_eq!(
        told(&mut watch, T + 10 * MINUTE, &workers),
        [
            "Worker \"Deck\" (opencode session ses_1) has shown no progress for 10 min; a message sent 8 min ago is still waiting"
        ]
    );
    // Running a tool, at half an hour.
    assert!(told(&mut watch, T + 30 * MINUTE - 1, &workers).is_empty());
    assert_eq!(
        told(&mut watch, T + 30 * MINUTE, &workers),
        [
            "Worker \"Build\" (opencode session ses_2) has been running shell for 30 min with no other progress; a message sent 28 min ago is still waiting"
        ]
    );
    // Several together say it of each.
    let mut watch = Watch::default();
    for id in ["ses_1", "ses_2"] {
        watch.observe(
            id.into(),
            WorkerActivity {
                message: Some(T),
                ..active(T)
            },
        );
    }
    assert_eq!(
        told(&mut watch, T + 10 * MINUTE, &workers),
        [
            "Workers \"Deck\" (opencode session ses_1, no progress for 10 min, a message sent 10 min ago is still waiting) and \"Build\" (opencode session ses_2, no progress for 10 min, a message sent 10 min ago is still waiting) have shown no progress"
        ]
    );
}
