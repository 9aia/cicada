//! `Clock` — injectable `now()` so `appliedAt`, drift windows, and
//! TTLs are deterministic under test.

use jiff::Timestamp;

/// The wall clock. Synchronous — `now()` never blocks.
pub trait Clock: Send + Sync {
    /// The current instant.
    fn now(&self) -> Timestamp;
}

/// The real clock — adapters embed this by default.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> Timestamp {
        Timestamp::now()
    }
}
