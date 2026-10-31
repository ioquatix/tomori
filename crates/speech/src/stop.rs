// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

//! Stopping engine threads when the server shuts down.

use std::{
    sync::{Arc, Condvar, Mutex, PoisonError},
    time::Instant,
};

/// Stops an engine's thread and waits for it to exit, so that no inference is still running when the process exits.
#[derive(Clone)]
pub struct Stopper {
    stop: Arc<dyn Fn() + Send + Sync>,
    exited: Exited,
}

impl Stopper {
    pub(crate) fn new(stop: impl Fn() + Send + Sync + 'static, exited: Exited) -> Self {
        Self {
            stop: Arc::new(stop),
            exited,
        }
    }

    /// Ask the engine to stop. Work in progress is abandoned at the engine's next opportunity, and waiting requests fail with [`crate::Error::Stopped`].
    pub fn stop(&self) {
        (self.stop)();
    }

    /// Wait until the engine's thread has exited, or until `deadline`. Returns whether it exited.
    pub fn wait(&self, deadline: Instant) -> bool {
        self.exited.wait(deadline)
    }
}

/// Whether a thread has exited. The thread holds an [`ExitGuard`], which records its exit when dropped, including by unwinding.
#[derive(Clone, Default)]
pub(crate) struct Exited(Arc<(Mutex<bool>, Condvar)>);

impl Exited {
    /// The guard for the thread to hold. Declare it first in the thread, so that it is dropped last, after the engine is unloaded.
    pub fn guard(&self) -> ExitGuard {
        ExitGuard(self.clone())
    }

    fn wait(&self, deadline: Instant) -> bool {
        let (exited, changed) = &*self.0;
        let mut exited = exited.lock().unwrap_or_else(PoisonError::into_inner);
        while !*exited {
            let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
                return false;
            };
            exited = changed
                .wait_timeout(exited, remaining)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
        true
    }
}

pub(crate) struct ExitGuard(Exited);

impl Drop for ExitGuard {
    fn drop(&mut self) {
        let (exited, changed) = &*(self.0).0;
        *exited.lock().unwrap_or_else(PoisonError::into_inner) = true;
        changed.notify_all();
    }
}

/// The message of a caught panic.
pub(crate) fn panic_message(panic: &(dyn std::any::Any + Send)) -> String {
    if let Some(message) = panic.downcast_ref::<&str>() {
        (*message).to_owned()
    } else if let Some(message) = panic.downcast_ref::<String>() {
        message.clone()
    } else {
        "unknown panic".to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{thread, time::Duration};

    #[test]
    fn waits_for_the_thread_to_exit() {
        let exited = Exited::default();
        let stopper = Stopper::new(|| {}, exited.clone());
        assert!(!stopper.wait(Instant::now() + Duration::from_millis(20)));
        let guard = exited.guard();
        let thread = thread::spawn(move || {
            thread::sleep(Duration::from_millis(20));
            drop(guard);
        });
        assert!(stopper.wait(Instant::now() + Duration::from_secs(5)));
        thread.join().unwrap();
    }
}
