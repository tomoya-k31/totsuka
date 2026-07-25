//! [`Clock`] implementations: the real system clock and a manually driven
//! clock for deterministic tests (#174).

use std::sync::Mutex;

use time::{Duration, OffsetDateTime};

use crate::ports::clock::Clock;

/// A [`Clock`] backed by the real system time.
#[derive(Debug, Default, Clone, Copy)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now_utc(&self) -> OffsetDateTime {
        OffsetDateTime::now_utc()
    }
}

/// A [`Clock`] whose time only moves when told to — test utility.
///
/// Public (not `#[cfg(test)]`) so integration tests can inject it; production
/// code always constructs [`SystemClock`]. Interior mutability lets tests
/// advance the clock through the same `Arc` they handed to [`StateDb`] and
/// the engine.
///
/// [`StateDb`]: crate::adapters::state_db::StateDb
pub struct ManualClock {
    now: Mutex<OffsetDateTime>,
}

impl ManualClock {
    /// A clock frozen at `start` until [`set`](Self::set) or
    /// [`advance`](Self::advance) moves it.
    pub fn new(start: OffsetDateTime) -> Self {
        Self {
            now: Mutex::new(start),
        }
    }

    /// Jump the clock to an absolute instant.
    pub fn set(&self, to: OffsetDateTime) {
        *self.now.lock().expect("clock mutex poisoned") = to;
    }

    /// Move the clock forward (or backward, with a negative delta).
    pub fn advance(&self, delta: Duration) {
        *self.now.lock().expect("clock mutex poisoned") += delta;
    }
}

impl Clock for ManualClock {
    fn now_utc(&self) -> OffsetDateTime {
        *self.now.lock().expect("clock mutex poisoned")
    }
}
