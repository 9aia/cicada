//! `MemoryStateStore` — an in-memory [`StateStore`].
//!
//! Implements the full port contract: `load` never fails on an
//! empty backend (first apply just works), `save` replaces the
//! snapshot, and `append_journal` appends into the state's own
//! journal so `journal()` and `load().journal` stay consistent.

use std::sync::Mutex;

use async_trait::async_trait;
use cicada_core::domain::error::Result;
use cicada_core::domain::state::{JournalEntry, RecordedState};
use cicada_core::ports::StateStore;

/// In-memory recorded state + apply journal.
#[derive(Debug, Default)]
pub struct MemoryStateStore {
    inner: Mutex<RecordedState>,
}

impl MemoryStateStore {
    /// An empty, correctly-versioned store.
    pub fn new() -> Self {
        Self::with_state(RecordedState::new())
    }

    /// A store pre-seeded with `state`.
    pub fn with_state(state: RecordedState) -> Self {
        Self {
            inner: Mutex::new(state),
        }
    }

    /// A snapshot of the current recorded state — assertions in
    /// tests read this rather than round-tripping `load()`.
    pub fn snapshot(&self) -> RecordedState {
        self.inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }
}

#[async_trait]
impl StateStore for MemoryStateStore {
    async fn load(&self) -> Result<RecordedState> {
        Ok(self.snapshot())
    }

    async fn save(&self, state: &RecordedState) -> Result<()> {
        *self
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = state.clone();
        Ok(())
    }

    async fn append_journal(&self, entry: &JournalEntry) -> Result<()> {
        self.inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .journal
            .push(entry.clone());
        Ok(())
    }

    async fn journal(&self) -> Result<Vec<JournalEntry>> {
        Ok(self.snapshot().journal)
    }
}
