//! `StateStore` — persistence for recorded state plus the per-op
//! apply journal.
//!
//! First adapter: `JsonFileStateStore` (`cicada-state`), which also
//! takes the advisory lock ADR-0002 §7 requires — locking is an
//! adapter concern (the lock's lifetime spans an apply run, which
//! doesn't fit an async port method cleanly); use cases take the
//! store already locked by the caller's plumbing.

use async_trait::async_trait;

use crate::domain::error::Result;
use crate::domain::state::{JournalEntry, RecordedState};

/// Load/save recorded state and append/read the apply journal.
#[async_trait]
pub trait StateStore: Send + Sync {
    /// Load the recorded state. A missing/empty backend returns
    /// `Ok(RecordedState::new())`, never an error — first apply must
    /// just work. An `apiVersion` the caller doesn't understand is a
    /// `Fatal`, not silent data loss.
    async fn load(&self) -> Result<RecordedState>;

    /// Persist the whole state document (calendars + journal
    /// snapshot). Implementations should write atomically
    /// (temp + rename on file adapters).
    async fn save(&self, state: &RecordedState) -> Result<()>;

    /// Append one journal entry as an op lands — the crash-safe
    /// record that makes `apply --resume` work (ADR-0001 §4).
    async fn append_journal(&self, entry: &JournalEntry) -> Result<()>;

    /// Read the journal back (resume checks keys; `drift` reports).
    async fn journal(&self) -> Result<Vec<JournalEntry>>;
}
