// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

use std::collections::VecDeque;
use std::sync::mpsc::{RecvError, RecvTimeoutError, TryRecvError};
use std::sync::{Condvar, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use protocol::ServerMessage;

use crate::Event;

#[derive(Default)]
struct State {
    queue: VecDeque<Event>,
    /// The connection thread has finished: nothing more will arrive.
    finished: bool,
}

/// The events a [`crate::Client`] has observed and the application has not yet taken, in order.
///
/// The queue does not grow without bound when the application falls behind on presence: a [`ServerMessage::Presence`] that has not been taken yet is replaced by the next one for the same source, which moves to the end of the queue so that it never overtakes the events that preceded it. Presence therefore holds at most one event per participant. Every other event is kept until taken; those are control messages and connection state changes, whose rate the server and the reconnection backoff limit.
#[derive(Default)]
pub struct Events {
    state: Mutex<State>,
    ready: Condvar,
}

impl Events {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    pub(crate) fn push(&self, event: Event) {
        let mut state = self.lock();
        if let Event::Message(ServerMessage::Presence { source, .. }) = &event {
            let source = *source;
            if let Some(index) = state.queue.iter().position(|queued| {
                matches!(queued, Event::Message(ServerMessage::Presence { source: queued, .. }) if *queued == source)
            }) {
                state.queue.remove(index);
            }
        }
        state.queue.push_back(event);
        drop(state);
        self.ready.notify_all();
    }

    /// No more events will be pushed: receivers return errors once the queue is empty.
    pub(crate) fn finish(&self) {
        self.lock().finished = true;
        self.ready.notify_all();
    }

    /// The next event, if one is waiting.
    pub fn try_recv(&self) -> Result<Event, TryRecvError> {
        let mut state = self.lock();
        match state.queue.pop_front() {
            Some(event) => Ok(event),
            None if state.finished => Err(TryRecvError::Disconnected),
            None => Err(TryRecvError::Empty),
        }
    }

    /// Wait for the next event. Fails once the client has closed and every event has been taken.
    pub fn recv(&self) -> Result<Event, RecvError> {
        let mut state = self.lock();
        loop {
            if let Some(event) = state.queue.pop_front() {
                return Ok(event);
            }
            if state.finished {
                return Err(RecvError);
            }
            state = self
                .ready
                .wait(state)
                .unwrap_or_else(PoisonError::into_inner);
        }
    }

    /// Wait up to `timeout` for the next event.
    pub fn recv_timeout(&self, timeout: Duration) -> Result<Event, RecvTimeoutError> {
        let deadline = Instant::now() + timeout;
        let mut state = self.lock();
        loop {
            if let Some(event) = state.queue.pop_front() {
                return Ok(event);
            }
            if state.finished {
                return Err(RecvTimeoutError::Disconnected);
            }
            let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
                return Err(RecvTimeoutError::Timeout);
            };
            state = self
                .ready
                .wait_timeout(state, remaining)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
    }

    /// Events waiting to be taken.
    pub fn len(&self) -> usize {
        self.lock().queue.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ConnectionState;
    use protocol::Pose;

    fn presence(source: u32, x: f32) -> Event {
        Event::Message(ServerMessage::Presence {
            source,
            head: Pose {
                position: [x, 0.0, 0.0],
                orientation: [0.0, 0.0, 0.0, 1.0],
            },
            left: None,
            right: None,
        })
    }

    #[test]
    fn a_lagging_application_receives_only_the_latest_presence_per_source() {
        let events = Events::new();
        events.push(presence(1, 0.0));
        events.push(Event::State(ConnectionState::Closed));
        for step in 0..10_000 {
            events.push(presence(1, step as f32));
            events.push(presence(2, step as f32));
        }
        events.push(Event::Message(ServerMessage::Left));
        assert_eq!(events.len(), 4);
        assert_eq!(events.try_recv(), Ok(Event::State(ConnectionState::Closed)));
        assert_eq!(events.try_recv(), Ok(presence(1, 9_999.0)));
        assert_eq!(events.try_recv(), Ok(presence(2, 9_999.0)));
        assert_eq!(events.try_recv(), Ok(Event::Message(ServerMessage::Left)));
        assert_eq!(events.try_recv(), Err(TryRecvError::Empty));
    }

    #[test]
    fn other_events_are_never_dropped() {
        let events = Events::new();
        for _ in 0..1000 {
            events.push(Event::Message(ServerMessage::Left));
        }
        assert_eq!(events.len(), 1000);
    }

    #[test]
    fn receivers_fail_once_finished_and_drained() {
        let events = Events::new();
        assert_eq!(
            events.recv_timeout(Duration::from_millis(1)),
            Err(RecvTimeoutError::Timeout)
        );
        events.push(Event::State(ConnectionState::Closed));
        events.finish();
        assert_eq!(events.recv(), Ok(Event::State(ConnectionState::Closed)));
        assert_eq!(events.recv(), Err(RecvError));
        assert_eq!(events.try_recv(), Err(TryRecvError::Disconnected));
        assert_eq!(
            events.recv_timeout(Duration::from_secs(1)),
            Err(RecvTimeoutError::Disconnected)
        );
    }
}
