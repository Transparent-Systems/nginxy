//! Cooperative shutdown flag.
//!
//! Replaces Python's `_handle_sigint()` / `KeyboardInterrupt` pair: the signal
//! handler only sets a flag and wakes sleepers, so a second Ctrl-C cannot land
//! inside shutdown code (the Python comment at `main.py:556-575` explains why
//! that mattered). Every wait in the sync loop goes through [`Stop::sleep`],
//! which is why Ctrl-C during a 30 s period still exits immediately.

use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

#[derive(Clone, Debug, Default)]
pub struct Stop {
    inner: Arc<(Mutex<bool>, Condvar)>,
}

impl Stop {
    pub fn new() -> Self {
        Self::default()
    }

    /// Requests shutdown. Safe to call from a signal handler and repeatedly.
    pub fn request(&self) {
        let (lock, cvar) = &*self.inner;
        let mut stopped = lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        if !*stopped {
            *stopped = true;
            cvar.notify_all();
        }
    }

    pub fn is_stopped(&self) -> bool {
        let (lock, _) = &*self.inner;
        *lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Sleeps for `duration`.
    ///
    /// Returns `true` when shutdown was requested instead of the time elapsing,
    /// which is the caller's cue to stop the loop. A zero duration only reports
    /// the current state.
    pub fn sleep(&self, duration: Duration) -> bool {
        if duration.is_zero() {
            return self.is_stopped();
        }

        let (lock, cvar) = &*self.inner;
        let guard = lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let (guard, _) = cvar
            .wait_timeout_while(guard, duration, |stopped| !*stopped)
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        *guard
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    #[test]
    fn sleep_returns_false_when_nothing_happens() {
        let stop = Stop::new();
        assert!(!stop.sleep(Duration::from_millis(20)));
        assert!(!stop.is_stopped());
    }

    #[test]
    fn sleep_wakes_up_when_requested() {
        let stop = Stop::new();
        let waker = stop.clone();
        let started = Instant::now();
        let handle = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(30));
            waker.request();
        });

        // A five-second sleep must return as soon as the flag is set.
        assert!(stop.sleep(Duration::from_secs(5)));
        handle.join().unwrap();
        assert!(started.elapsed() < Duration::from_secs(2));
        assert!(stop.is_stopped());
    }

    #[test]
    fn repeated_requests_are_harmless() {
        let stop = Stop::new();
        stop.request();
        stop.request();
        assert!(stop.is_stopped());
        assert!(stop.sleep(Duration::from_millis(1)));
    }
}
