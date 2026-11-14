// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

//! The scheduler over every form, daylight saving and month ends, the conditions with first arrival and grace,
//! coalescing, limits and what is kept, with the time and zone given. See `design/timers.md`.
use super::schedule::{Cron, Days, Schedule, time_of_day, time_of_day_to_second};
use super::*;

const LONDON: &str = "Europe/London";

fn london() -> TimeZone {
    TimeZone::get(LONDON).unwrap()
}

/// The instant of `text`, `YYYY-MM-DD HH:MM`, as a London clock shows it, the earlier where it shows it twice.
fn local(text: &str) -> Timestamp {
    let (date, time) = text.split_once(' ').unwrap();
    schedule::resolve(
        &london(),
        date.parse::<jiff::civil::Date>()
            .unwrap()
            .to_datetime(time_of_day(time).unwrap()),
    )
}

/// The instant of `text` in UTC, `YYYY-MM-DD HH:MM`.
fn utc(text: &str) -> Timestamp {
    format!("{}:00Z", text.replace(' ', "T")).parse().unwrap()
}

fn cron(text: &str) -> Schedule {
    Schedule::Cron {
        cron: Cron::parse(text).unwrap(),
    }
}

fn daily(at: &str, days: Days) -> Schedule {
    Schedule::Daily {
        at: at.to_owned(),
        days,
    }
}

#[test]
fn each_form_s_next_occurrence_in_its_zone() {
    let zone = london();
    let monday_thursday = Days::parse(&["mon", "Thursday"]).unwrap();
    let table: Vec<(&str, Schedule, Timestamp, Option<Timestamp>)> = vec![
        // Every hour, aligned to midnight, so on the hour.
        (
            "hourly",
            Schedule::every(60, local("2026-11-11 09:17")),
            local("2026-11-11 09:17"),
            Some(local("2026-11-11 10:00")),
        ),
        // Every quarter, on the quarter.
        (
            "quarterly",
            Schedule::every(15, local("2026-11-11 09:17")),
            local("2026-11-11 09:17"),
            Some(local("2026-11-11 09:30")),
        ),
        // Every 7 minutes, not dividing a day, from when it was set.
        (
            "every 7",
            Schedule::every(7, local("2026-11-11 09:17")),
            local("2026-11-11 09:17"),
            Some(local("2026-11-11 09:24")),
        ),
        // Before its first, its first.
        (
            "before",
            Schedule::every(7, local("2026-11-11 09:17")),
            local("2026-11-11 08:00"),
            Some(local("2026-11-11 09:17")),
        ),
        // Elapsed time across clocks going forward: an hour after 00:00 GMT is 02:00 BST, still on the hour.
        (
            "hourly over the gap",
            Schedule::every(60, local("2026-03-29 00:10")),
            utc("2026-03-29 00:30"),
            Some(local("2026-03-29 02:00")),
        ),
        (
            "daily, later today",
            daily("09:00", Days::EVERY),
            local("2026-11-11 08:59"),
            Some(local("2026-11-11 09:00")),
        ),
        (
            "daily, at its time, tomorrow",
            daily("09:00", Days::EVERY),
            local("2026-11-11 09:00"),
            Some(local("2026-11-12 09:00")),
        ),
        // From Friday after nine, Monday.
        (
            "weekdays",
            daily("09:00", Days::WEEKDAYS),
            local("2026-11-13 10:00"),
            Some(local("2026-11-16 09:00")),
        ),
        (
            "weekends",
            daily("10:30", Days::WEEKENDS),
            local("2026-11-11 10:00"),
            Some(local("2026-11-14 10:30")),
        ),
        (
            "Monday and Thursday",
            daily("09:00", monday_thursday),
            local("2026-11-11 10:00"),
            Some(local("2026-11-12 09:00")),
        ),
        // In the gap, 01:30 does not exist: an hour later, 02:30 BST.
        (
            "daily in the gap",
            daily("01:30", Days::EVERY),
            local("2026-03-28 12:00"),
            Some(utc("2026-03-29 01:30")),
        ),
        // In the overlap, 01:30 happens twice: the first, 00:30 UTC, and the next is the following day's.
        (
            "daily in the overlap",
            daily("01:30", Days::EVERY),
            local("2026-10-24 12:00"),
            Some(utc("2026-10-25 00:30")),
        ),
        (
            "not twice in the overlap",
            daily("01:30", Days::EVERY),
            utc("2026-10-25 00:30"),
            Some(utc("2026-10-26 01:30")),
        ),
        (
            "once",
            Schedule::Once {
                at: local("2026-11-11 14:30").as_second(),
            },
            local("2026-11-11 10:00"),
            Some(local("2026-11-11 14:30")),
        ),
        (
            "once, past",
            Schedule::Once {
                at: local("2026-11-11 14:30").as_second(),
            },
            local("2026-11-11 14:30"),
            None,
        ),
        // Half-hourly in working hours on weekdays: from Friday's last, Monday's first.
        (
            "cron working hours",
            cron("*/30 9-17 * * 1-5"),
            local("2026-11-13 17:30"),
            Some(local("2026-11-16 09:00")),
        ),
        (
            "cron within them",
            cron("*/30 9-17 * * 1-5"),
            local("2026-11-13 12:10"),
            Some(local("2026-11-13 12:30")),
        ),
        // The 31st skips November.
        (
            "cron month end",
            cron("0 9 31 * *"),
            local("2026-11-01 00:00"),
            Some(local("2026-12-31 09:00")),
        ),
        (
            "cron leap day",
            cron("0 9 29 2 *"),
            local("2026-03-01 00:00"),
            Some(local("2028-02-29 09:00")),
        ),
        // Day of the month or day of the week, as in Vixie cron: the 12th, a Thursday, then Friday.
        (
            "cron day or weekday",
            cron("0 9 12 * 5"),
            local("2026-11-11 10:00"),
            Some(local("2026-11-12 09:00")),
        ),
        (
            "cron weekday after",
            cron("0 9 12 * 5"),
            local("2026-11-12 10:00"),
            Some(local("2026-11-13 09:00")),
        ),
        (
            "cron day of the month alone",
            cron("0 9 12 * *"),
            local("2026-11-12 10:00"),
            Some(local("2026-12-12 09:00")),
        ),
        // Sunday is 7 as well as 0.
        (
            "cron Sunday as 7",
            cron("0 8 * * 7"),
            local("2026-11-11 10:00"),
            Some(local("2026-11-15 08:00")),
        ),
        (
            "cron a single value's step",
            cron("5/20 10 * * *"),
            local("2026-11-11 10:06"),
            Some(local("2026-11-11 10:25")),
        ),
        (
            "cron a list",
            cron("0 8,12 * * *"),
            local("2026-11-11 10:00"),
            Some(local("2026-11-11 12:00")),
        ),
        // Every quarter of the hour that does not exist goes past the gap, in order.
        (
            "cron in the gap",
            cron("*/15 1 * * *"),
            utc("2026-03-29 00:59"),
            Some(utc("2026-03-29 01:00")),
        ),
        (
            "cron later in the gap",
            cron("*/15 1 * * *"),
            utc("2026-03-29 01:00"),
            Some(utc("2026-03-29 01:15")),
        ),
        // The 31st of February never comes.
        (
            "cron never",
            cron("0 9 31 2 *"),
            local("2026-11-11 10:00"),
            None,
        ),
    ];
    for (name, schedule, after, expected) in table {
        assert_eq!(schedule.next_after(after, &zone), expected, "{name}");
    }
}

#[test]
fn occurrences_are_counted_up_to_a_bound_with_the_latest() {
    let zone = london();
    let hourly = Schedule::every(60, local("2026-11-11 09:00"));
    assert_eq!(
        hourly.occurrences(
            local("2026-11-11 09:00"),
            local("2026-11-11 12:30"),
            &zone,
            999
        ),
        (3, Some(local("2026-11-11 12:00")))
    );
    assert_eq!(
        hourly.occurrences(
            local("2026-11-11 09:00"),
            local("2026-11-11 12:30"),
            &zone,
            2
        ),
        (2, Some(local("2026-11-11 11:00")))
    );
    assert_eq!(
        hourly.occurrences(
            local("2026-11-11 09:00"),
            local("2026-11-11 09:30"),
            &zone,
            999
        ),
        (0, None)
    );
}

#[test]
fn schedules_and_conditions_in_short_words() {
    let zone = london();
    let now = local("2026-11-11 09:00");
    let table = [
        (Schedule::every(60, now), "every hour"),
        (Schedule::every(120, now), "every 2 hours"),
        (Schedule::every(30, now), "every 30 min"),
        (daily("09:00", Days::EVERY), "daily at 09:00"),
        (daily("09:00", Days::WEEKDAYS), "weekdays at 09:00"),
        (daily("09:00", Days::WEEKENDS), "weekends at 09:00"),
        (
            daily("09:00", Days::parse(&["fri", "mon", "wed"]).unwrap()),
            "Mon, Wed, Fri at 09:00",
        ),
        (
            Schedule::Once {
                at: local("2026-11-11 14:30").as_second(),
            },
            "once at 14:30",
        ),
        (cron("0  9 * *   1-5"), "cron 0 9 * * 1-5"),
    ];
    for (schedule, said) in table {
        assert_eq!(schedule.describe(&zone), said);
    }
    assert_eq!(Condition::Always.describe(), "always");
    assert_eq!(Condition::Present.describe(), "when present");
    assert_eq!(
        Condition::PresentOrArrival.describe(),
        "when present or on arrival"
    );
    for (name, condition) in [
        ("always", Some(Condition::Always)),
        ("present", Some(Condition::Present)),
        ("when_present", Some(Condition::Present)),
        ("present_or_arrival", Some(Condition::PresentOrArrival)),
        ("sometimes", None),
    ] {
        assert_eq!(Condition::parse(name), condition);
    }
}

#[test]
fn days_cron_and_times_of_day_are_read_or_refused() {
    assert_eq!(Days::parse(&["weekdays", "sat"]).unwrap(), Days(0b011_1111));
    assert_eq!(Days::parse(&["Daily"]).unwrap(), Days::EVERY);
    assert_eq!(Days::parse(&["every day"]).unwrap(), Days::EVERY);
    assert_eq!(Days::parse(&["weekends"]).unwrap(), Days::WEEKENDS);
    assert!(
        Days::parse(&["mo"])
            .unwrap_err()
            .contains("does not know the day \"mo\"")
    );
    assert!(Days::parse(&["funday"]).is_err());
    assert_eq!(Days::parse(&[]).unwrap_err(), "days names no day.");
    for bad in [
        "0 9 * *",
        "60 9 * * *",
        "0 24 * * *",
        "0 9 0 * *",
        "0 9 * 13 *",
        "0 9 * * 8",
        "*/0 9 * * *",
        "5-3 9 * * *",
        "a 9 * * *",
        "1-b 9 * * *",
        "a-1 9 * * *",
        "*/x 9 * * *",
        "0 9 * JAN *",
    ] {
        assert!(Cron::parse(bad).is_err(), "{bad}");
    }
    assert!(
        Cron::parse("0 9 * *")
            .unwrap_err()
            .starts_with("cron has five fields")
    );
    let refused = Cron::parse("61 9 * * *").unwrap_err();
    assert!(refused.contains("minute field, \"61\""), "{refused}");
    // Kept as its text, and read again from it.
    let kept = serde_json::to_string(&Cron::parse("0 9 * * 1-5").unwrap()).unwrap();
    assert_eq!(kept, "\"0 9 * * 1-5\"");
    assert_eq!(
        serde_json::from_str::<Cron>(&kept).unwrap(),
        Cron::parse("0 9 * * 1-5").unwrap()
    );
    assert!(serde_json::from_str::<Cron>("\"nonsense\"").is_err());
    // The fewest minutes apart, across the hour too.
    assert_eq!(Cron::parse("*/5 * * * *").unwrap().least_gap(), 5);
    assert_eq!(Cron::parse("0,58 9 * * *").unwrap().least_gap(), 2);
    assert_eq!(Cron::parse("0 9 * * *").unwrap().least_gap(), 60);
    for (text, time) in [
        ("09:00", Some((9, 0))),
        ("9:05", Some((9, 5))),
        (" 23:59 ", Some((23, 59))),
        ("24:00", None),
        ("09:60", None),
        ("09:5", None),
        ("0900", None),
        ("aa:00", None),
        ("123:00", None),
        (":00", None),
    ] {
        assert_eq!(
            time_of_day(text).map(|time| (time.hour(), time.minute())),
            time,
            "{text}"
        );
    }
    // A kept time out of range is the nearest a timestamp holds.
    assert_eq!(
        schedule::instant(i64::MAX).as_second(),
        Timestamp::MAX.as_second()
    );
    assert_eq!(
        schedule::instant(i64::MIN).as_second(),
        Timestamp::MIN.as_second()
    );
    // HH:MM:SS, to the second, where a one-off or an end may be; HH:MM as before.
    for (text, time) in [
        ("09:17:45", Some((9, 17, 45))),
        (" 9:05:00 ", Some((9, 5, 0))),
        ("23:59:59", Some((23, 59, 59))),
        ("09:17", Some((9, 17, 0))),
        ("09:17:60", None),
        ("09:17:5", None),
        ("09:17:5a", None),
        ("09:60:00", None),
        ("24:00:00", None),
        ("09:17:", None),
    ] {
        assert_eq!(
            time_of_day_to_second(text).map(|time| (time.hour(), time.minute(), time.second())),
            time,
            "{text}"
        );
    }
    // Gaps are in seconds.
    assert_eq!(
        Schedule::every(60, local("2026-11-11 09:00")).least_gap(),
        Some(3600)
    );
    assert_eq!(
        Schedule::EverySeconds {
            seconds: 90,
            from: 0
        }
        .least_gap(),
        Some(90)
    );
    assert_eq!(cron("*/5 * * * *").least_gap(), Some(300));
    assert_eq!(daily("09:00", Days::EVERY).least_gap(), Some(86_400));
    assert_eq!(Schedule::Once { at: 0 }.least_gap(), None);
}

fn request(note: &str) -> Request {
    Request {
        note: note.to_owned(),
        zone: Some(LONDON.to_owned()),
        ..Request::default()
    }
}

fn every(minutes: u32) -> Request {
    Request {
        every_minutes: Some(minutes),
        ..request("Give a status update")
    }
}

fn every_seconds(seconds: u32) -> Request {
    Request {
        every_seconds: Some(seconds),
        ..request("Next exercise")
    }
}

fn in_seconds(seconds: u32) -> Request {
    Request {
        name: Some("plank".into()),
        in_seconds: Some(seconds),
        ..request("Thirty seconds of plank are up")
    }
}

/// `at` and `milliseconds` more.
fn later(at: Timestamp, milliseconds: i64) -> Timestamp {
    at + jiff::SignedDuration::from_millis(milliseconds)
}

#[test]
fn a_timer_is_set_only_as_its_rules_allow() {
    let now = local("2026-11-11 09:17");
    let refused = |request: Request| Timer::new(request, now, LONDON, 1).unwrap_err();
    let table: Vec<(Request, &str)> = vec![
        (
            Request {
                note: " ".into(),
                ..every(60)
            },
            "note says what the timer is for",
        ),
        (
            Request {
                note: "x".repeat(NOTE_CHARS + 1),
                ..every(60)
            },
            "note is at most 200",
        ),
        (
            Request {
                name: Some("".into()),
                ..every(60)
            },
            "name is 1 to 40",
        ),
        (
            Request {
                name: Some("n".repeat(41)),
                ..every(60)
            },
            "name is 1 to 40",
        ),
        (
            Request {
                zone: Some("Mars/Olympus".into()),
                ..every(60)
            },
            "does not know the time zone \"Mars/Olympus\"",
        ),
        (request("x"), "Give exactly one schedule"),
        (
            Request {
                in_minutes: Some(5),
                ..every(60)
            },
            "Give exactly one schedule",
        ),
        (every(0), "every_minutes is from 1 to 10080"),
        (every(10081), "every_minutes is from 1 to 10080"),
        // A repeating timer fires at most every minute, whatever its form.
        (
            every_seconds(59),
            "fires when present fires at most every minute, its occurrences at least 60 seconds apart",
        ),
        (
            every_seconds(0),
            "every_seconds is from 60 to 604800, a week.",
        ),
        (
            every_seconds(604_801),
            "every_seconds is from 60 to 604800, a week.",
        ),
        (
            Request {
                condition: Some(Condition::PresentOrArrival),
                ..every_seconds(30)
            },
            "fires when present or on arrival fires at most every minute",
        ),
        (
            Request {
                condition: Some(Condition::Always),
                ..every_seconds(600)
            },
            "fires always fires at most every 15 minutes",
        ),
        (
            Request {
                in_seconds: Some(30),
                ..every_seconds(60)
            },
            "Give exactly one schedule: every_minutes, every_seconds, daily_at (with days), once_at, in_minutes, in_seconds or cron.",
        ),
        (
            Request {
                in_seconds: Some(0),
                ..request("x")
            },
            "in_seconds is from 1 to 31622400.",
        ),
        (
            Request {
                in_seconds: Some(31_622_401),
                ..request("x")
            },
            "in_seconds is from 1 to 31622400.",
        ),
        (
            Request {
                once_at: Some("09:30:60".into()),
                ..request("x")
            },
            "once_at is a time of day, HH:MM or HH:MM:SS",
        ),
        (
            Request {
                once_at: Some("2026-11-11 09:16:59".into()),
                ..request("x")
            },
            "once_at is after now",
        ),
        (
            Request {
                condition: Some(Condition::Always),
                ..every(10)
            },
            "fires always fires at most every 15 minutes",
        ),
        (
            Request {
                daily_at: Some("9am".into()),
                ..request("x")
            },
            "daily_at is a time of day",
        ),
        (
            Request {
                daily_at: Some("09:00".into()),
                days: Some(vec!["someday".into()]),
                ..request("x")
            },
            "does not know the day",
        ),
        (
            Request {
                days: Some(vec!["mon".into()]),
                ..every(60)
            },
            "days goes with daily_at.",
        ),
        (
            Request {
                once_at: Some("tomorrow".into()),
                ..request("x")
            },
            "once_at is a time of day",
        ),
        (
            Request {
                once_at: Some("2026-11-11 08:00".into()),
                ..request("x")
            },
            "once_at is after now",
        ),
        (
            Request {
                once_at: Some("2028-01-01 08:00".into()),
                ..request("x")
            },
            "within 366 days",
        ),
        (
            Request {
                once_at: Some("2026-13-01 08:00".into()),
                ..request("x")
            },
            "once_at is a time of day",
        ),
        (
            Request {
                once_at: Some("2026-12-01 8am".into()),
                ..request("x")
            },
            "once_at is a time of day",
        ),
        (
            Request {
                in_minutes: Some(0),
                ..request("x")
            },
            "in_minutes is from 1 to 527040.",
        ),
        (
            Request {
                cron: Some("0 9 * *".into()),
                ..request("x")
            },
            "cron has five fields",
        ),
        (
            Request {
                cron: Some("*/2 * * * *".into()),
                condition: Some(Condition::Always),
                ..request("x")
            },
            "at most every 15 minutes",
        ),
        (
            Request {
                cron: Some("0 9 31 2 *".into()),
                ..request("x")
            },
            "never fires",
        ),
        (
            Request {
                until: Some("soon".into()),
                ..every(60)
            },
            "until is a time of day",
        ),
        (
            Request {
                until: Some("2026-11-10 08:00".into()),
                ..every(60)
            },
            "until is after now",
        ),
        // An end before its first occurrence leaves it none.
        (
            Request {
                until: Some("09:30".into()),
                ..every(60)
            },
            "never fires",
        ),
        (
            Request {
                count: Some(0),
                ..every(60)
            },
            "count is at least 1.",
        ),
    ];
    for (request, said) in table {
        let refusal = refused(request);
        assert!(refusal.contains(said), "{refusal} does not say {said}");
    }
}

#[test]
fn a_timer_is_set_with_its_name_zone_and_first_occurrence() {
    let now = local("2026-11-11 09:17");
    let timer = Timer::new(every(60), now, "UTC", 3).unwrap();
    assert_eq!(timer.name, "timer 3");
    assert_eq!(timer.zone, LONDON);
    assert_eq!(timer.condition, Condition::Present);
    assert_eq!(timer.next, Some(local("2026-11-11 10:00").as_second()));
    assert_eq!(timer.describe(), "every hour, when present");
    // In the default zone without one, its note and name on one line.
    let timer = Timer::new(
        Request {
            name: Some(" Morning \n stand-up ".into()),
            zone: None,
            daily_at: Some("9:00".into()),
            days: Some(vec!["weekdays".into()]),
            condition: Some(Condition::PresentOrArrival),
            ..request("Run the [stand-up]\nnow")
        },
        now,
        "Pacific/Auckland",
        1,
    )
    .unwrap();
    assert_eq!(timer.name, "Morning   stand-up");
    assert_eq!(timer.note, "Run the (stand-up) now");
    assert_eq!(timer.zone, "Pacific/Auckland");
    assert_eq!(
        timer.describe(),
        "weekdays at 09:00, when present or on arrival"
    );
    // Days given empty are every day.
    let timer = Timer::new(
        Request {
            daily_at: Some("09:00".into()),
            days: Some(Vec::new()),
            ..request("x")
        },
        now,
        LONDON,
        1,
    )
    .unwrap();
    assert_eq!(timer.describe(), "daily at 09:00, when present");
    // A time of day already past today is tomorrow's.
    let once = |at: &str| {
        Timer::new(
            Request {
                once_at: Some(at.into()),
                ..request("x")
            },
            now,
            LONDON,
            1,
        )
        .unwrap()
        .next
    };
    assert_eq!(once("14:30"), Some(local("2026-11-11 14:30").as_second()));
    assert_eq!(once("08:00"), Some(local("2026-11-12 08:00").as_second()));
    assert_eq!(
        once("2026-11-20T08:00"),
        Some(local("2026-11-20 08:00").as_second())
    );
    let soon = Timer::new(
        Request {
            in_minutes: Some(20),
            ..request("x")
        },
        now,
        LONDON,
        1,
    )
    .unwrap();
    assert_eq!(soon.next, Some(local("2026-11-11 09:37").as_second()));
    let cron = Timer::new(
        Request {
            cron: Some("0 9 * * 1-5".into()),
            ..request("x")
        },
        now,
        LONDON,
        1,
    )
    .unwrap();
    assert_eq!(cron.next, Some(local("2026-11-12 09:00").as_second()));
    // Always, every quarter of an hour, and with an end and a count.
    let ending = Timer::new(
        Request {
            condition: Some(Condition::Always),
            until: Some("2026-11-12 12:00".into()),
            count: Some(3),
            ..every(15)
        },
        now,
        LONDON,
        1,
    )
    .unwrap();
    assert_eq!(ending.until, Some(local("2026-11-12 12:00").as_second()));
    assert_eq!(ending.count, Some(3));
}

const SAMUEL: &[&str] = &["Samuel"];

fn names(present: &[&str]) -> Vec<String> {
    present.iter().map(|name| (*name).to_owned()).collect()
}

/// A space's timers with one set from `request` at `now`.
fn timers(request: Request, now: Timestamp) -> Timers {
    let mut timers = Timers::default();
    timers.set(request, now, LONDON).unwrap();
    timers
}

fn texts(firings: Vec<Firing>) -> Vec<String> {
    firings.into_iter().map(|firing| firing.text).collect()
}

#[test]
fn when_present_fires_only_while_someone_is_and_says_what_it_skipped() {
    let mut space = timers(
        Request {
            name: Some("status".into()),
            ..every(60)
        },
        local("2026-11-11 09:17"),
    );
    assert!(
        space
            .tick(local("2026-11-11 09:59"), &names(SAMUEL))
            .is_empty()
    );
    assert_eq!(
        texts(space.tick(local("2026-11-11 10:00"), &names(&["Samuel", "Ada"]))),
        [
            "Timer \"status\" (every hour, when present): fired 10:00; present: Samuel, Ada; note: \"Give a status update\""
        ]
    );
    // Gone at 10:30, so 11:00 is skipped, not saved for later.
    assert!(space.tick(local("2026-11-11 10:30"), &[]).is_empty());
    assert!(space.tick(local("2026-11-11 11:00"), &[]).is_empty());
    assert!(
        space
            .tick(local("2026-11-11 11:30"), &names(SAMUEL))
            .is_empty()
    );
    assert_eq!(
        texts(space.tick(local("2026-11-11 12:00"), &names(SAMUEL))),
        [
            "Timer \"status\" (every hour, when present): fired 12:00; last fired 10:00; skipped 1 while nobody was present; present: Samuel; note: \"Give a status update\""
        ]
    );
}

#[test]
fn a_moment_away_within_the_grace_still_counts_as_present() {
    let mut space = timers(every(60), local("2026-11-11 09:17"));
    space.tick(local("2026-11-11 09:59"), &names(SAMUEL));
    // Lifted the headset a minute before, so still attended.
    assert!(space.attended(local("2026-11-11 10:00"), &[]));
    assert_eq!(space.tick(local("2026-11-11 10:00"), &[]).len(), 1);
    // Past the grace, not.
    assert!(!space.attended(local("2026-11-11 10:02"), &[]));
    assert!(space.tick(local("2026-11-11 11:00"), &[]).is_empty());
}

#[test]
fn present_or_arrival_fires_once_as_the_first_participant_arrives_in_its_period() {
    let mut space = timers(
        Request {
            name: Some("morning".into()),
            daily_at: Some("09:00".into()),
            condition: Some(Condition::PresentOrArrival),
            ..request("Summarise overnight")
        },
        local("2026-11-11 08:00"),
    );
    assert!(space.tick(local("2026-11-11 09:00"), &[]).is_empty());
    assert_eq!(
        space.list()[0].waiting.as_ref().unwrap().due,
        local("2026-11-11 09:00").as_second()
    );
    assert_eq!(
        space.views()[0].state,
        tomori_protocol::TimerState::Awaiting
    );
    // Still nobody: it waits on.
    assert!(space.tick(local("2026-11-11 09:30"), &[]).is_empty());
    assert_eq!(
        space.describe("morning").unwrap(),
        "timer \"morning\" (daily at 09:00, when present or on arrival; waiting for someone to arrive)"
    );
    assert_eq!(
        texts(space.tick(local("2026-11-11 10:42"), &names(SAMUEL))),
        [
            "Timer \"morning\" (daily at 09:00, when present or on arrival): fired 10:42 as someone arrived, due 09:00; present: Samuel; note: \"Summarise overnight\""
        ]
    );
    assert_eq!(space.views()[0].state, tomori_protocol::TimerState::Fired);
    // Leaving and coming back the same day fires nothing more.
    assert!(space.tick(local("2026-11-11 12:00"), &[]).is_empty());
    assert!(
        space
            .tick(local("2026-11-11 15:00"), &names(SAMUEL))
            .is_empty()
    );
    // Nobody all the next day: its occurrence waits until the one after, then is skipped as that one waits.
    assert!(space.tick(local("2026-11-11 18:00"), &[]).is_empty());
    assert!(space.tick(local("2026-11-12 09:00"), &[]).is_empty());
    assert!(space.tick(local("2026-11-13 09:00"), &[]).is_empty());
    assert_eq!(space.list()[0].skipped, 1);
    // Someone present at an occurrence: it fires then, saying what was skipped, the 12th's and the 13th's.
    assert_eq!(
        texts(space.tick(local("2026-11-14 09:00"), &names(SAMUEL))),
        [
            "Timer \"morning\" (daily at 09:00, when present or on arrival): fired 09:00; last fired Wed 10:42; skipped 2 while nobody was present; present: Samuel; note: \"Summarise overnight\""
        ]
    );
}

#[test]
fn an_arrival_wait_lapses_at_its_period_s_end() {
    let mut space = timers(
        Request {
            once_at: Some("10:00".into()),
            condition: Some(Condition::PresentOrArrival),
            ..request("Check the deploy")
        },
        local("2026-11-11 09:00"),
    );
    assert!(space.tick(local("2026-11-11 10:00"), &[]).is_empty());
    // A one-off's period is a day.
    assert_eq!(
        space.list()[0].waiting.as_ref().unwrap().ends,
        local("2026-11-12 10:00").as_second()
    );
    assert!(space.tick(local("2026-11-12 10:00"), &[]).is_empty());
    assert!(
        space.list().is_empty(),
        "lapsed with nothing ahead, it is gone"
    );
}

#[test]
fn always_fires_with_nobody_present_within_its_own_limit() {
    let start = local("2026-11-11 00:00");
    let mut space = timers(
        Request {
            condition: Some(Condition::Always),
            name: Some("watch".into()),
            ..every(15)
        },
        start,
    );
    assert_eq!(
        texts(space.tick(local("2026-11-11 00:15"), &[])),
        [
            "Timer \"watch\" (every 15 min, always): fired 00:15; present: nobody; note: \"Give a status update\""
        ]
    );
    assert_eq!(space.views()[0].state, tomori_protocol::TimerState::Fired);
    let mut fired = 1;
    for quarter in 2..40 {
        fired += space.tick(start + minutes(15 * quarter), &[]).len();
    }
    assert_eq!(fired, UNATTENDED_FIRINGS);
    // Someone present: it fires again, saying how many were held back.
    let text = texts(space.tick(start + minutes(15 * 40), &names(SAMUEL))).remove(0);
    assert!(
        text.contains("held back 15 by the space's daily limit"),
        "{text}"
    );
}

#[test]
fn the_space_s_daily_limit_holds_back_firings_and_arrivals() {
    let start = local("2026-11-11 00:00");
    let mut space = timers(every(5), start);
    let mut fired = 0;
    for step in 1..=100 {
        fired += space.tick(start + minutes(5 * step), &names(SAMUEL)).len();
    }
    assert_eq!(fired, DAILY_FIRINGS);
    assert_eq!(space.list()[0].limited, 4);
    // An arrival past the limit fires nothing, and is counted.
    space
        .set(
            Request {
                name: Some("arrival".into()),
                daily_at: Some("09:00".into()),
                condition: Some(Condition::PresentOrArrival),
                ..request("x")
            },
            start + minutes(5 * 100),
            LONDON,
        )
        .unwrap();
    assert!(space.tick(local("2026-11-11 09:00"), &[]).is_empty());
    assert!(
        space
            .tick(local("2026-11-11 09:10"), &names(SAMUEL))
            .is_empty()
    );
    let arrival = space
        .list()
        .iter()
        .find(|timer| timer.name == "arrival")
        .unwrap();
    assert_eq!((arrival.limited, arrival.waiting.is_none()), (1, true));
}

#[test]
fn occurrences_missed_while_stopped_coalesce_into_one_catch_up() {
    let mut space = timers(every(60), local("2026-11-11 09:17"));
    assert_eq!(
        space.tick(local("2026-11-11 10:00"), &names(SAMUEL)).len(),
        1
    );
    // Nothing looked at it until 13:05: one firing, for 13:00, counting 11:00 and 12:00.
    let text = texts(space.tick(local("2026-11-11 13:05"), &names(SAMUEL))).remove(0);
    assert!(
        text.contains("fired 13:00; last fired 10:00; missed 2 while"),
        "{text}"
    );
    // A week stopped counts at most a bound.
    let text = texts(space.tick(local("2026-12-30 13:05"), &names(SAMUEL))).remove(0);
    assert!(text.contains("missed at least 999"), "{text}");
}

#[test]
fn a_count_an_end_or_a_one_off_finishes_a_timer() {
    let start = local("2026-11-11 09:17");
    let mut space = timers(
        Request {
            count: Some(2),
            name: Some("twice".into()),
            ..every(60)
        },
        start,
    );
    space.tick(local("2026-11-11 10:00"), &names(SAMUEL));
    let text = texts(space.tick(local("2026-11-11 11:00"), &names(SAMUEL))).remove(0);
    assert!(text.contains("; its last firing; note:"), "{text}");
    assert!(space.list().is_empty());
    let mut space = timers(
        Request {
            until: Some("10:30".into()),
            ..every(60)
        },
        start,
    );
    assert_eq!(
        space.tick(local("2026-11-11 10:00"), &names(SAMUEL)).len(),
        1
    );
    assert!(space.list().is_empty(), "nothing before its end is left");
    let mut space = timers(
        Request {
            in_minutes: Some(20),
            ..request("Tea")
        },
        start,
    );
    assert_eq!(
        space.tick(local("2026-11-11 09:37"), &names(SAMUEL)).len(),
        1
    );
    assert!(space.list().is_empty());
}

#[test]
fn how_a_timer_looks_and_is_described_as_it_is_pointed_at() {
    let mut space = timers(
        Request {
            name: Some("status".into()),
            ..every(60)
        },
        local("2026-11-11 09:17"),
    );
    space.tick(local("2026-11-11 09:48"), &[]);
    let view = &space.views()[0];
    assert_eq!(
        (
            view.name.as_str(),
            view.schedule.as_str(),
            view.condition.as_str(),
            view.next,
            view.state
        ),
        (
            "status",
            "every hour",
            "when present",
            Some(local("2026-11-11 10:00").as_second() as u64),
            tomori_protocol::TimerState::Paused
        )
    );
    assert_eq!(
        space.describe("status").unwrap(),
        "timer \"status\" (every hour, when present; next in 12 min)"
    );
    assert_eq!(space.describe("other"), None);
    space.tick(local("2026-11-11 09:49"), &names(SAMUEL));
    assert_eq!(space.views()[0].state, tomori_protocol::TimerState::Waiting);
    for (seconds, said) in [
        (0, "0 s"),
        (30, "30 s"),
        (60, "1 min"),
        (125, "2 min 5 s"),
        (600, "10 min"),
        (10_800, "3 h"),
        (300_000, "3 days"),
    ] {
        let now = local("2026-11-11 09:00");
        assert_eq!(
            until(now + jiff::SignedDuration::from_secs(seconds), now),
            said
        );
    }
    // Rounded up, so that it says nothing is left only once it is due.
    let now = local("2026-11-11 09:00");
    assert_eq!(until(later(now, 26_200), now), "27 s");
    assert_eq!(until(later(now, 400), now), "1 s");
}

#[test]
fn timers_are_named_replaced_bounded_and_cancelled() {
    let now = local("2026-11-11 09:17");
    let mut space = Timers::default();
    let (first, replaced) = space.set(every(60), now, LONDON).unwrap();
    assert_eq!((first.name.as_str(), replaced), ("timer 1", false));
    assert_eq!(space.set(every(30), now, LONDON).unwrap().0.name, "timer 2");
    // The same name, in any case, replaces it.
    let (again, replaced) = space
        .set(
            Request {
                name: Some("TIMER 1".into()),
                ..every(120)
            },
            now,
            LONDON,
        )
        .unwrap();
    assert!(replaced);
    assert_eq!(again.describe(), "every 2 hours, when present");
    assert_eq!(space.list().len(), 2);
    for index in 3..=MOST {
        space
            .set(
                Request {
                    name: Some(format!("t{index}")),
                    ..every(60)
                },
                now,
                LONDON,
            )
            .unwrap();
    }
    assert!(
        space
            .set(every(60), now, LONDON)
            .unwrap_err()
            .contains("has 8 timers")
    );
    assert_eq!(space.cancel(" t3 ").unwrap().name, "t3");
    assert_eq!(space.cancel("t3"), None);
    // A refused timer changes nothing.
    assert!(space.set(every_seconds(30), now, LONDON).is_err());
    assert_eq!(space.list().len(), MOST - 1);
}

#[test]
fn timers_are_kept_and_restored_and_catch_up_after_a_restart() {
    crate::test_log::record();
    let mut space = timers(
        Request {
            name: Some("status".into()),
            ..every(60)
        },
        local("2026-11-11 09:17"),
    );
    space.tick(local("2026-11-11 10:00"), &names(SAMUEL));
    let kept = space.kept();
    assert_eq!(kept["version"], VERSION);
    let written = serde_json::to_string(&kept).unwrap();
    let read: serde_json::Value = serde_json::from_str(&written).unwrap();
    assert_eq!(read, kept);
    // Stopped from 10:30 to 13:10: once someone is present, one catch-up.
    let mut restored = Timers::restore(&read, "space-1");
    assert_eq!(restored.list(), space.list());
    assert_eq!(restored.kept()["fired"], kept["fired"]);
    let text = texts(restored.tick(local("2026-11-11 13:10"), &names(SAMUEL))).remove(0);
    assert!(
        text.contains("fired 13:00; last fired 10:00; missed 2"),
        "{text}"
    );
    // Nothing set, nothing kept.
    assert!(Timers::default().kept().is_null());
    assert!(
        Timers::restore(&serde_json::Value::Null, "space-1")
            .list()
            .is_empty()
    );
    // A record of more than the most is cut to it.
    let mut many = kept.clone();
    many["timers"] = serde_json::Value::Array(vec![kept["timers"][0].clone(); MOST + 2]);
    assert_eq!(Timers::restore(&many, "space-1").list().len(), MOST);
    // An unknown zone kept goes by UTC.
    let mut odd = space.list()[0].clone();
    odd.zone = "Nowhere/Here".into();
    assert_eq!(odd.time_zone(), TimeZone::UTC);
    let mut unknown = kept.clone();
    unknown["timers"][0]["zone"] = "Nowhere/Here".into();
    assert_eq!(
        Timers::restore(&unknown, "space-1").list()[0].zone,
        "Nowhere/Here"
    );
}

#[test]
fn a_record_this_server_cannot_read_leaves_out_only_what_it_cannot_read() {
    crate::test_log::record();
    let mut space = Timers::default();
    let now = local("2026-11-11 09:17");
    space
        .set(
            Request {
                name: Some("one".into()),
                ..every(60)
            },
            now,
            LONDON,
        )
        .unwrap();
    space
        .set(
            Request {
                name: Some("two".into()),
                ..every(30)
            },
            now,
            LONDON,
        )
        .unwrap();
    let kept = space.kept();
    let names = |record: &serde_json::Value| -> Vec<String> {
        Timers::restore(record, "space-1")
            .list()
            .iter()
            .map(|timer| timer.name.clone())
            .collect()
    };
    assert_eq!(names(&kept), ["one", "two"]);
    // A future version leaves every timer out; so does a record with none.
    let mut future = kept.clone();
    future["version"] = 2.into();
    assert!(names(&future).is_empty());
    assert!(names(&serde_json::json!({"timers": []})).is_empty());
    // A schedule a newer server knows, an invalid cron and a field edited by hand each leave out only that timer.
    for (field, value) in [
        (
            "schedule",
            serde_json::json!({"kind": "lunar", "phase": "full"}),
        ),
        (
            "schedule",
            serde_json::json!({"kind": "cron", "cron": "0 9 L * *"}),
        ),
        ("created", serde_json::json!("yesterday")),
    ] {
        let mut odd = kept.clone();
        odd["timers"][0][field] = value;
        assert_eq!(names(&odd), ["two"], "{field}");
    }
    let mut nameless = kept.clone();
    nameless["timers"][1] = serde_json::json!({"note": "x"});
    assert_eq!(names(&nameless), ["one"]);
    // Limits' times and the number not readable are none.
    let mut odd = kept.clone();
    odd["fired"] = "lots".into();
    odd["numbered"] = "two".into();
    let restored = Timers::restore(&odd, "space-1");
    assert_eq!(
        (restored.list().len(), restored.kept()["numbered"].clone()),
        (2, 0.into())
    );
    // A kept interval of no minutes is every minute rather than a failure.
    let every_none = Schedule::Every {
        minutes: 0,
        from: 0,
    };
    assert_eq!(
        every_none.next_after(local("2026-11-11 09:17"), &london()),
        Some(local("2026-11-11 09:18"))
    );
    assert!(!crate::test_log::logged("which this server cannot read").is_empty());
}

#[test]
fn the_system_zone_is_named() {
    assert!(!system_zone().is_empty());
    assert!(zone(&system_zone()).is_ok());
}

#[test]
fn cron_s_day_fields_follow_vixie_cron_steps_of_all_included() {
    let zone = london();
    let table = [
        // A step of all the days of the month keeps to odd days, whatever the weekday.
        ("0 9 */2 * *", "2026-11-11 10:00", "2026-11-13 09:00"),
        ("0 9 */2 * *", "2026-11-29 10:00", "2026-12-01 09:00"),
        ("0 9 */2 * *", "2026-12-31 10:00", "2027-01-01 09:00"),
        // A step of all the weekdays keeps to Sunday, Tuesday, Thursday and Saturday, whatever the day.
        ("0 9 * * */2", "2026-11-11 10:00", "2026-11-12 09:00"),
        ("0 9 * * */2", "2026-11-12 10:00", "2026-11-14 09:00"),
        // A step of all the days and a weekday: an odd day that is a Monday, not either.
        ("0 9 */2 * 1", "2026-11-11 10:00", "2026-11-23 09:00"),
        // A step of one is all of it: every Monday, and the 1st alone.
        ("0 9 */1 * 1", "2026-11-11 10:00", "2026-11-16 09:00"),
        ("0 9 1 * */1", "2026-11-11 10:00", "2026-12-01 09:00"),
        // Both restricted otherwise, either: the 1st, the 15th or any Monday.
        ("0 9 1,15 * 1", "2026-11-11 10:00", "2026-11-15 09:00"),
        ("0 9 1,15 * 1", "2026-11-15 10:00", "2026-11-16 09:00"),
        // Every 13th and every Friday, not only Friday the 13th.
        ("0 0 13 * 5", "2026-11-11 10:00", "2026-11-13 00:00"),
        ("0 0 13 * 5", "2026-11-13 10:00", "2026-11-20 00:00"),
        ("0 0 13 * 5", "2026-12-04 10:00", "2026-12-11 00:00"),
    ];
    for (text, after, next) in table {
        assert_eq!(
            cron(text).next_after(local(after), &zone),
            Some(local(next)),
            "{text} after {after}"
        );
    }
}

#[test]
fn daylight_saving_in_zones_that_shift_by_an_hour_half_an_hour_or_off_the_hour() {
    let start = utc("2026-01-01 00:00");
    let table: Vec<(&str, &str, Schedule, Timestamp, Timestamp)> = vec![
        // Auckland's clocks go from 02:00 to 03:00 on 27 September: 02:30 is 03:30 NZDT, and an hour after 01:00 is
        // 03:00, then 04:00.
        (
            "Auckland gap, daily",
            "Pacific/Auckland",
            daily("02:30", Days::EVERY),
            utc("2026-09-26 12:00"),
            utc("2026-09-26 14:30"),
        ),
        (
            "Auckland gap, hourly",
            "Pacific/Auckland",
            Schedule::every(60, start),
            utc("2026-09-26 13:30"),
            utc("2026-09-26 14:00"),
        ),
        (
            "Auckland gap, hourly after",
            "Pacific/Auckland",
            Schedule::every(60, start),
            utc("2026-09-26 14:00"),
            utc("2026-09-26 15:00"),
        ),
        // They go back from 03:00 to 02:00 on 5 April: 02:30 fires at its first, 13:30 UTC, and not again that day.
        (
            "Auckland overlap, daily",
            "Pacific/Auckland",
            daily("02:30", Days::EVERY),
            utc("2026-04-04 12:00"),
            utc("2026-04-04 13:30"),
        ),
        (
            "Auckland overlap, daily once",
            "Pacific/Auckland",
            daily("02:30", Days::EVERY),
            utc("2026-04-04 13:30"),
            utc("2026-04-05 14:30"),
        ),
        // Hourly on the clock: 02:00 at its first, then 03:00 NZST, the repeated hour not again.
        (
            "Auckland overlap, hourly",
            "Pacific/Auckland",
            Schedule::every(60, start),
            utc("2026-04-04 12:30"),
            utc("2026-04-04 13:00"),
        ),
        (
            "Auckland overlap, hourly after",
            "Pacific/Auckland",
            Schedule::every(60, start),
            utc("2026-04-04 13:00"),
            utc("2026-04-04 15:00"),
        ),
        // Lord Howe Island moves by half an hour, 02:00 to 02:30 on 4 October: 02:15 is 02:45, at UTC+11.
        (
            "Lord Howe gap",
            "Australia/Lord_Howe",
            daily("02:15", Days::EVERY),
            utc("2026-10-03 12:00"),
            utc("2026-10-03 15:45"),
        ),
        // And back, 02:00 to 01:30 on 5 April: 01:45 at its first, at UTC+11.
        (
            "Lord Howe overlap",
            "Australia/Lord_Howe",
            daily("01:45", Days::EVERY),
            utc("2026-04-04 12:00"),
            utc("2026-04-04 14:45"),
        ),
        // Chatham is 45 minutes off the hour, 02:45 to 03:45 on 27 September: 03:00 is 04:00 at UTC+13:45.
        (
            "Chatham gap",
            "Pacific/Chatham",
            daily("03:00", Days::EVERY),
            utc("2026-09-26 12:00"),
            utc("2026-09-26 14:15"),
        ),
        (
            "Chatham hourly",
            "Pacific/Chatham",
            Schedule::every(60, start),
            utc("2026-09-26 13:15"),
            utc("2026-09-26 14:15"),
        ),
    ];
    for (name, zone, schedule, after, next) in table {
        assert_eq!(
            schedule.next_after(after, &TimeZone::get(zone).unwrap()),
            Some(next),
            "{name}"
        );
    }
}

#[test]
fn intervals_that_divide_a_day_stay_on_the_clock_through_daylight_saving() {
    let zone = london();
    let set = local("2026-03-20 09:17");
    // Every two hours, on the even hours, before and after clocks go forward and back.
    let two = Schedule::every(120, set);
    assert_eq!(
        two.next_after(local("2026-03-20 09:17"), &zone),
        Some(local("2026-03-20 10:00"))
    );
    assert_eq!(
        two.next_after(local("2026-03-29 03:30"), &zone),
        Some(local("2026-03-29 04:00"))
    );
    assert_eq!(
        two.next_after(local("2026-10-25 03:30"), &zone),
        Some(local("2026-10-25 04:00"))
    );
    // Every day, at midnight, either side of each.
    let day = Schedule::every(1440, set);
    assert_eq!(
        day.next_after(local("2026-03-29 00:30"), &zone),
        Some(local("2026-03-30 00:00"))
    );
    assert_eq!(
        day.next_after(local("2026-10-25 12:00"), &zone),
        Some(local("2026-10-26 00:00"))
    );
    // Every 90 minutes, from midnight: 10:30 then 12:00, on the clock after the change too.
    let ninety = Schedule::every(90, set);
    assert_eq!(
        ninety.next_after(local("2026-03-30 09:17"), &zone),
        Some(local("2026-03-30 10:30"))
    );
    // Every 7 minutes does not divide a day: elapsed time from when it was set.
    let seven = Schedule::every(7, set);
    assert_eq!(
        seven
            .next_after(local("2026-03-29 03:00"), &zone)
            .map(|at| (at.as_second() - set.as_second()) % 420),
        Some(0)
    );
}

#[test]
fn after_a_restart_a_catch_up_waits_the_grace_for_participants_to_return() {
    let mut space = timers(every(60), local("2026-11-11 09:17"));
    space.tick(local("2026-11-11 10:00"), &names(SAMUEL));
    let kept = space.kept();
    // Bound again at 13:05, before the headsets reconnect: the catch-up waits.
    let mut restored = Timers::restore(&kept, "space-1");
    assert!(restored.tick(local("2026-11-11 13:05"), &[]).is_empty());
    assert_eq!(restored.list()[0].skipped, 0, "not skipped while held");
    // Samuel is back a minute later: it fires, counting what was missed.
    let text = texts(restored.tick(local("2026-11-11 13:06"), &names(SAMUEL))).remove(0);
    assert!(text.contains("missed 2"), "{text}");
    // Nobody back within the grace: skipped once it has passed.
    let mut restored = Timers::restore(&kept, "space-1");
    assert!(restored.tick(local("2026-11-11 13:05"), &[]).is_empty());
    assert!(restored.tick(local("2026-11-11 13:07"), &[]).is_empty());
    assert_eq!(restored.list()[0].skipped, 3);
}

#[test]
fn names_match_in_any_case_and_default_names_never_replace_a_timer() {
    let now = local("2026-11-11 09:17");
    let mut space = Timers::default();
    space
        .set(
            Request {
                name: Some("Café".into()),
                ..every(60)
            },
            now,
            LONDON,
        )
        .unwrap();
    space.tick(now, &[]);
    assert!(space.describe("CAFÉ").is_some());
    let (_, replaced) = space
        .set(
            Request {
                name: Some(" CAFÉ ".into()),
                ..every(30)
            },
            now,
            LONDON,
        )
        .unwrap();
    assert!(replaced);
    assert!(same_name("Ünïcode", "üNÏCODE"));
    // A timer named as a default would be is skipped by the next default.
    space
        .set(
            Request {
                name: Some("Timer 1".into()),
                ..every(60)
            },
            now,
            LONDON,
        )
        .unwrap();
    space
        .set(
            Request {
                name: Some("timer 2".into()),
                ..every(60)
            },
            now,
            LONDON,
        )
        .unwrap();
    let (named, replaced) = space.set(every(60), now, LONDON).unwrap();
    assert_eq!((named.name.as_str(), replaced), ("timer 3", false));
    assert_eq!(space.list().len(), 4);
    assert_eq!(space.cancel("café").unwrap().name, "CAFÉ");
}

#[test]
fn firings_later_than_now_as_after_the_clock_went_back_count_toward_no_limit() {
    let mut space = timers(every(60), local("2026-11-11 09:17"));
    space.tick(local("2026-11-11 10:00"), &names(SAMUEL));
    assert_eq!(space.kept()["fired"].as_array().unwrap().len(), 1);
    // The clock is set back a day: the firing it remembers is in the future, and forgotten.
    space.tick(local("2026-11-10 10:30"), &names(SAMUEL));
    assert!(space.kept().get("fired").is_none());
}

#[test]
fn a_timer_set_in_seconds_fires_at_its_second() {
    // Set 400 ms into 09:17, it counts from the nearest second, so that it fires within half a second of 30 s from now.
    let set = later(local("2026-11-11 09:17"), 400);
    let mut space = timers(in_seconds(30), set);
    let timer = &space.list()[0];
    assert_eq!(timer.next, Some(local("2026-11-11 09:17").as_second() + 30));
    assert_eq!(timer.describe(), "once at 09:17:30, when present");
    assert_eq!(space.next_due(), timer.next);
    // A moment before, nothing; at its second, it fires, its times said to the second, and it is done.
    assert!(space.tick(later(set, 29_500), &names(SAMUEL)).is_empty());
    assert_eq!(
        space.describe("plank").unwrap(),
        "timer \"plank\" (once at 09:17:30, when present; next in 1 s)"
    );
    let fired = space.tick(later(set, 29_600), &names(SAMUEL));
    // A one-off is told on time, without the gap since the last prompt of notes alone.
    assert!(fired.iter().all(|firing| firing.on_time));
    assert_eq!(
        texts(fired),
        [
            "Timer \"plank\" (once at 09:17:30, when present): fired 09:17:30; present: Samuel; its last firing; note: \"Thirty seconds of plank are up\""
        ]
    );
    assert!(space.list().is_empty());
    assert_eq!(space.next_due(), None);
    // Set 600 ms into 09:17, it counts from 09:17:01; a single firing may be a second away.
    let space = timers(in_seconds(1), later(local("2026-11-11 09:17"), 600));
    assert_eq!(
        space.list()[0].next,
        Some(local("2026-11-11 09:17").as_second() + 2)
    );
}

#[test]
fn once_at_takes_a_time_to_the_second() {
    let now = local("2026-11-11 09:17");
    let at = |text: &str| {
        Timer::new(
            Request {
                once_at: Some(text.into()),
                ..request("Stretch")
            },
            now,
            LONDON,
            1,
        )
        .unwrap()
    };
    // Later today, tomorrow once it has passed, or on a date.
    let today = at("09:17:45");
    assert_eq!(today.next, Some(now.as_second() + 45));
    assert_eq!(today.describe(), "once at 09:17:45, when present");
    assert_eq!(
        at("09:16:59").next,
        Some(local("2026-11-12 09:16").as_second() + 59)
    );
    assert_eq!(
        at("2026-11-12 08:00:30").next,
        Some(local("2026-11-12 08:00").as_second() + 30)
    );
    // On a minute, it reads as HH:MM, as before.
    assert_eq!(at("10:00:00").describe(), "once at 10:00, when present");
    // An end may be to the second too.
    let ends = Timer::new(
        Request {
            until: Some("09:20:30".into()),
            ..every_seconds(60)
        },
        now,
        LONDON,
        1,
    )
    .unwrap();
    assert_eq!(ends.until, Some(local("2026-11-11 09:20").as_second() + 30));
}

#[test]
fn every_so_many_seconds_counts_from_when_it_was_set() {
    let set = later(local("2026-11-11 09:17"), 300);
    let mut space = timers(
        Request {
            name: Some("intervals".into()),
            count: Some(3),
            ..every_seconds(90)
        },
        set,
    );
    let start = local("2026-11-11 09:17").as_second();
    assert_eq!(space.list()[0].next, Some(start + 90));
    assert_eq!(space.list()[0].describe(), "every 90 s, when present");
    let present = names(SAMUEL);
    let fired = space.tick(instant(start + 90), &present);
    // A repeating timer waits for the gap, as other notes do.
    assert!(fired.iter().all(|firing| !firing.on_time));
    assert_eq!(
        texts(fired),
        [
            "Timer \"intervals\" (every 90 s, when present): fired 09:18:30; present: Samuel; note: \"Next exercise\""
        ]
    );
    // It looks just fired for half its interval, rather than a minute, so that it counts down again before the next.
    assert_eq!(space.views()[0].state, tomori_protocol::TimerState::Fired);
    space.tick(instant(start + 134), &present);
    assert_eq!(space.views()[0].state, tomori_protocol::TimerState::Fired);
    space.tick(instant(start + 135), &present);
    assert_eq!(space.views()[0].state, tomori_protocol::TimerState::Waiting);
    assert_eq!(space.views()[0].next, Some((start + 180) as u64));
    assert_eq!(
        texts(space.tick(instant(start + 180), &present)),
        [
            "Timer \"intervals\" (every 90 s, when present): fired 09:20:00; last fired 09:18:30; present: Samuel; note: \"Next exercise\""
        ]
    );
    // Its third firing is its last.
    assert!(texts(space.tick(instant(start + 270), &present))[0].contains("its last firing"));
    assert!(space.list().is_empty());
    // Every 60 s is the least, said in minutes; a timer every so many minutes may now be every minute.
    let every_minute = timers(every_seconds(60), set);
    assert_eq!(
        every_minute.list()[0].describe(),
        "every 1 min, when present"
    );
    assert_eq!(every_minute.list()[0].next, Some(start + 60));
    let mut aligned = timers(every(1), set);
    assert_eq!(aligned.list()[0].next, Some(start + 60));
    assert_eq!(aligned.tick(instant(start + 60), &present).len(), 1);
    // An hourly timer still looks just fired for a minute.
    let mut hourly = timers(every(60), set);
    hourly.tick(local("2026-11-11 10:00"), &present);
    hourly.tick(later(local("2026-11-11 10:00"), 59_000), &present);
    assert_eq!(hourly.views()[0].state, tomori_protocol::TimerState::Fired);
    // A kept record of none, as edited by hand, is every second rather than a division by zero.
    let none = Schedule::EverySeconds {
        seconds: 0,
        from: start,
    };
    assert_eq!(
        none.next_after(instant(start), &london()),
        Some(instant(start + 1))
    );
    // It is kept and read again.
    let kept = timers(every_seconds(90), set).kept();
    assert_eq!(kept["timers"][0]["schedule"]["kind"], "every_seconds");
    let restored = Timers::restore(&kept, "space");
    assert_eq!(
        restored.list()[0].schedule,
        timers(every_seconds(90), set).list()[0].schedule
    );
}
