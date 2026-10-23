//! Recorded state — the `cicada/state-v1` contract (ADR-0002 §7).
//!
//! ```yaml
//! apiVersion: cicada/state-v1
//! calendars:
//!   work:
//!     provider: google
//!     calendarId: primary
//!     events:
//!       standup: { remoteId: "abc123", hash: "sha256:…", appliedAt: "…" }
//! journal: []   # per-op entries, appended as ops land
//! ```
//!
//! Recorded state is the *managed boundary* (ADR-0002 §1): cicada
//! only deletes or overwrites what this file records. `journal` is
//! the append-only per-op record that makes apply resumable and
//! crash-safe (ADR-0001 §4).

use std::collections::BTreeMap;

use jiff::Timestamp;
use serde::{Deserialize, Serialize};

use super::ids::{ContentHash, LocalId, RemoteId};
use super::operation::OpKind;

/// The whole recorded-state document.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RecordedState {
    /// Format version — always [`RecordedState::API_VERSION`];
    /// migrations are explicit and tested.
    pub api_version: String,
    /// Per-calendar recorded slices, keyed by logical calendar name.
    #[serde(default)]
    pub calendars: BTreeMap<String, RecordedCalendar>,
    /// Append-only apply journal — per-op entries in land order.
    #[serde(default)]
    pub journal: Vec<JournalEntry>,
}

impl RecordedState {
    /// The only state format version this core writes.
    pub const API_VERSION: &'static str = "cicada/state-v1";

    /// An empty, correctly-versioned state.
    pub fn new() -> Self {
        Self {
            api_version: Self::API_VERSION.to_owned(),
            calendars: BTreeMap::new(),
            journal: Vec::new(),
        }
    }

    /// `true` when `v` is a version this core understands.
    pub fn is_supported_version(v: &str) -> bool {
        v == Self::API_VERSION
    }

    /// Get-or-create the recorded slice for a logical calendar.
    pub fn calendar_mut(&mut self, name: &str) -> &mut RecordedCalendar {
        self.calendars.entry(name.to_owned()).or_default()
    }

    /// The recorded slice for a calendar, if present.
    pub fn calendar(&self, name: &str) -> Option<&RecordedCalendar> {
        self.calendars.get(name)
    }

    /// Append a journal entry.
    pub fn push_journal(&mut self, entry: JournalEntry) {
        self.journal.push(entry);
    }

    /// Find a journal entry by idempotency key — `apply --resume`
    /// skips ops already journaled (ADR-0001 §4).
    pub fn journal_entry(&self, key: &str) -> Option<&JournalEntry> {
        self.journal.iter().find(|e| e.key == key)
    }
}

impl Default for RecordedState {
    fn default() -> Self {
        Self::new()
    }
}

/// Recorded slice of one logical calendar.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RecordedCalendar {
    /// Provider that owns the binding (`google`).
    #[serde(default)]
    pub provider: String,
    /// Remote calendar id (`primary`).
    #[serde(default)]
    pub calendar_id: String,
    /// localId → recorded remote event.
    #[serde(default)]
    pub events: BTreeMap<LocalId, RecordedEvent>,
}

impl RecordedCalendar {
    /// Record (or overwrite) the remote record for a local id.
    pub fn record(
        &mut self,
        local_id: LocalId,
        remote_id: RemoteId,
        hash: ContentHash,
        applied_at: Timestamp,
    ) {
        self.events.insert(
            local_id,
            RecordedEvent {
                remote_id,
                hash,
                applied_at,
                etag: None,
            },
        );
    }

    /// Drop the record for a local id (after a `Delete` lands).
    pub fn forget(&mut self, local_id: &LocalId) -> Option<RecordedEvent> {
        self.events.remove(local_id)
    }
}

/// One recorded remote event — the last reconciled state cicada saw
/// for this local id.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RecordedEvent {
    /// Provider-assigned remote id.
    pub remote_id: RemoteId,
    /// Content hash of the applied desired event.
    pub hash: ContentHash,
    /// When the op landed.
    pub applied_at: Timestamp,
    /// Last-known remote ETag/sequence for conditional writes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub etag: Option<String>,
}

/// One journaled op — written as the op lands, so a crash mid-apply
/// leaves a valid, resumable state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JournalEntry {
    /// Idempotency key (`calendarId|localId|opKind|hash`).
    pub key: String,
    /// What was attempted.
    pub op: OpKind,
    /// Local id the op reconciled.
    pub local_id: LocalId,
    /// When the op was journaled.
    pub applied_at: Timestamp,
    /// How it went.
    pub outcome: JournalOutcome,
}

/// Per-op apply outcome.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    tag = "status"
)]
pub enum JournalOutcome {
    /// The remote write landed.
    Applied {
        /// The remote id the provider assigned/confirmed.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        remote_id: Option<RemoteId>,
    },
    /// The op failed — recorded honestly so `resume` and `drift` can
    /// see it instead of corrupting state (ADR-0001 §4).
    Failed {
        /// Error message.
        error: String,
        /// Error class (`transient`/`conflict`/…), for resume logic.
        error_kind: super::error::ErrorKind,
    },
    /// On resume the provider already had the op (found by
    /// `cicada:id` tag, ADR-0002 §3) — journaled as applied-without-
    /// re-executing.
    SkippedAlreadyApplied {
        /// The remote id found by tag lookup.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        remote_id: Option<RemoteId>,
    },
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn new_state_has_v1_version() {
        let s = RecordedState::new();
        assert_eq!(s.api_version, "cicada/state-v1");
        assert!(RecordedState::is_supported_version(&s.api_version));
    }

    #[test]
    fn state_yaml_shape_matches_the_adr() {
        let mut s = RecordedState::new();
        let cal = s.calendar_mut("work");
        cal.provider = "google".into();
        cal.calendar_id = "primary".into();
        cal.record(
            "standup".into(),
            "abc123".into(),
            "sha256:deadbeef".into(),
            "2026-10-23T01:00:00Z".parse().unwrap(),
        );
        let yaml = serde_yaml::to_string(&s).unwrap();
        let value: serde_yaml::Value = serde_yaml::from_str(&yaml).unwrap();
        assert_eq!(value["apiVersion"], "cicada/state-v1");
        assert_eq!(value["calendars"]["work"]["provider"], "google");
        assert_eq!(value["calendars"]["work"]["calendarId"], "primary");
        assert_eq!(
            value["calendars"]["work"]["events"]["standup"]["remoteId"],
            "abc123"
        );
        assert_eq!(
            value["calendars"]["work"]["events"]["standup"]["hash"],
            "sha256:deadbeef"
        );
        assert_eq!(value["journal"], serde_yaml::Value::Sequence(vec![]));
        // Round-trips.
        let back: RecordedState = serde_yaml::from_str(&yaml).unwrap();
        assert_eq!(back, s);
    }

    #[test]
    fn journal_lookup_by_key() {
        let mut s = RecordedState::new();
        s.push_journal(JournalEntry {
            key: "primary|standup|create|sha256:abc".into(),
            op: OpKind::Create,
            local_id: "standup".into(),
            applied_at: "2026-10-23T01:00:00Z".parse().unwrap(),
            outcome: JournalOutcome::Applied {
                remote_id: Some("abc123".into()),
            },
        });
        assert!(
            s.journal_entry("primary|standup|create|sha256:abc")
                .is_some()
        );
        assert!(s.journal_entry("nope").is_none());
    }
}
