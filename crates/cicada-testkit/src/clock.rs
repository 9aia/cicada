//! `FakeClock` — a settable [`Clock`] for deterministic tests.

use std::sync::Mutex;

use cicada_core::ports::Clock;
use jiff::{SignedDuration, Timestamp};

/// A clock pinned to a fixed instant until `advance`d.
#[derive(Debug)]
pub struct FakeClock {
    now: Mutex<Timestamp>,
}

impl FakeClock {
    /// A clock reporting `now`.
    pub fn at(now: Timestamp) -> Self {
        Self {
            now: Mutex::new(now),
        }
    }

    /// Move the clock forward by `duration`.
    pub fn advance(&self, duration: SignedDuration) {
        let mut now = self
            .now
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *now += duration;
    }

    /// The current instant (same as `Clock::now`, callable without
    /// importing the trait).
    pub fn now(&self) -> Timestamp {
        *self
            .now
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

impl Clock for FakeClock {
    fn now(&self) -> Timestamp {
        Self::now(self)
    }
}
