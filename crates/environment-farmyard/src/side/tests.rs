// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

use std::sync::Mutex;

use base_scheduler::Cancellation;
use tomori_environment::{Intent as Asked, Outlet};

use super::*;
use crate::behaviour::Doing;
use crate::flock::State;

/// What the side sent, as the server would keep it.
#[derive(Default)]
struct Kept {
    states: Mutex<Vec<String>>,
    events: Mutex<Vec<String>>,
    objects: Mutex<Vec<Vec<Object>>>,
}

impl Outlet for Kept {
    fn share(&self, state: String) -> Result<()> {
        self.states.lock().unwrap().push(state);
        Ok(())
    }

    fn announce(&self, event: String, state: String) -> Result<()> {
        self.events.lock().unwrap().push(event);
        self.states.lock().unwrap().push(state);
        Ok(())
    }

    fn describe(&self, objects: Vec<Object>) -> Result<()> {
        self.objects.lock().unwrap().push(objects);
        Ok(())
    }
}

/// Wait up to a few seconds for `ready`.
fn eventually(ready: impl Fn() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !ready() {
        assert!(Instant::now() < deadline, "not within 10 seconds");
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn the_side_shares_its_flock_describes_it_and_answers_a_notice() {
    let cancellation = Cancellation::new();
    let kept = Arc::new(Kept::default());
    let space = Space::with_cancellation("space-1", "Fleet", "/", cancellation.child())
        .with_outlet(kept.clone());
    let inbox = space.inbox();
    let running = std::thread::spawn(move || FarmyardSide.run(&space));
    // At once, the flock as it stands, and what the agent is told of each chicken.
    eventually(|| {
        !kept.states.lock().unwrap().is_empty() && !kept.objects.lock().unwrap().is_empty()
    });
    let first: State = serde_json::from_str(&kept.states.lock().unwrap()[0]).unwrap();
    let objects = kept.objects.lock().unwrap()[0].clone();
    assert_eq!(objects.len(), first.chickens.len());
    assert_eq!(objects[0].id(), "chicken-0");
    assert!(
        objects[0]
            .description()
            .starts_with(&first.chickens[0].name)
    );
    // Something it cannot read is passed over; a notice confuses the chicken, announced with the state it leaves.
    assert!(inbox.push(Asked::new(7, "not json")));
    assert!(inbox.push(Asked::new(7, r#"{"notice": 0, "toward": [0.0, 0.0]}"#)));
    eventually(|| {
        kept.events.lock().unwrap().iter().any(|event| {
            let event: crate::flock::Event = serde_json::from_str(event).unwrap();
            event.chicken == 0 && matches!(event.behaviour.doing, Doing::Confused { .. })
        })
    });
    let last: State = serde_json::from_str(kept.states.lock().unwrap().last().unwrap()).unwrap();
    assert!(last.clock >= first.clock);
    // It stops with the space.
    cancellation.cancel();
    assert_eq!(running.join().unwrap(), Ok(()));
}

/// An outlet that refuses everything, as one for a space that has gone might.
struct Refusing;

impl Outlet for Refusing {
    fn share(&self, _state: String) -> Result<()> {
        Err(Error::new("gone"))
    }

    fn announce(&self, _event: String, _state: String) -> Result<()> {
        Err(Error::new("gone"))
    }

    fn describe(&self, _objects: Vec<Object>) -> Result<()> {
        Err(Error::new("gone"))
    }
}

#[test]
fn the_side_stops_when_its_space_refuses_what_it_sends() {
    let chicken = Chicken::shared().unwrap();
    let space = Space::new("space-1", "Fleet", "/").with_outlet(Arc::new(Refusing));
    assert_eq!(
        run(&space, Some(chicken.clone()), Instant::now()),
        Err(Error::new("gone"))
    );
    // An outlet that keeps the state but refuses descriptions stops it as it describes.
    struct Sharing;
    impl Outlet for Sharing {
        fn share(&self, _state: String) -> Result<()> {
            Ok(())
        }
        fn announce(&self, _event: String, _state: String) -> Result<()> {
            Err(Error::new("gone"))
        }
        fn describe(&self, _objects: Vec<Object>) -> Result<()> {
            Err(Error::new("described"))
        }
    }
    let space = Space::new("space-1", "Fleet", "/").with_outlet(Arc::new(Sharing));
    assert_eq!(
        run(&space, Some(chicken.clone()), Instant::now()),
        Err(Error::new("described"))
    );
    // One that keeps descriptions but refuses events stops it at the first change, a while in.
    struct Describing;
    impl Outlet for Describing {
        fn share(&self, _state: String) -> Result<()> {
            Ok(())
        }
        fn announce(&self, _event: String, _state: String) -> Result<()> {
            Err(Error::new("announced"))
        }
        fn describe(&self, _objects: Vec<Object>) -> Result<()> {
            Ok(())
        }
    }
    let space = Space::new("space-1", "Fleet", "/").with_outlet(Arc::new(Describing));
    let started = Instant::now() - Duration::from_secs(60);
    assert_eq!(
        run(&space, Some(chicken), started),
        Err(Error::new("announced"))
    );
    // Without its chickens, it cannot run.
    let space = Space::new("space-1", "Fleet", "/");
    assert_eq!(
        run(&space, None, Instant::now()),
        Err(Error::new("The farmyard's chickens could not be loaded."))
    );
    // A cancelled space stops it at once, having shared the flock.
    let cancellation = Cancellation::new();
    cancellation.cancel();
    let kept = Arc::new(Kept::default());
    let space = Space::with_cancellation("space-1", "Fleet", "/", cancellation.child())
        .with_outlet(kept.clone());
    assert_eq!(FarmyardSide.run(&space), Ok(()));
    assert_eq!(kept.states.lock().unwrap().len(), 1);
}
