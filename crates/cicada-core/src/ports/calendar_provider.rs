//! `CalendarProvider` — the port every calendar backend implements.
//!
//! Contracts beyond the signatures (ADR-0002):
//!
//! - **Tagging (§2):** every event an adapter writes MUST carry the
//!   `cicada:id` (local id), `cicada:managed = "1"`, and
//!   `cicada:state` (content hash) private tags when
//!   `capabilities().supports_tagging` is claimed — state rebuild,
//!   adoption, and safe resume all depend on it.
//! - **`find_by_tag` (§3):** resume reconciles ambiguous ops by tag,
//!   so this lookup must find events by their `cicada:id` tag.
//! - **Conflict policy (§4):** `Update`/`Delete` receive the recorded
//!   ETag via [`WriteOptions::etag`]; adapters honoring
//!   `supports_conditional_writes` must send it.
//! - **`sendUpdates` (§5):** [`WriteOptions::send_updates`] must be
//!   forwarded; adapters that can't suppress notifications must not
//!   claim `can_suppress_notifications`.

use async_trait::async_trait;
use jiff::Timestamp;
use serde::{Deserialize, Serialize};

use crate::domain::capabilities::ProviderCapabilities;
use crate::domain::error::Result;
use crate::domain::event::EventBody;
use crate::domain::ids::{ContentHash, LocalId, RemoteId};
use crate::domain::policy::WriteOptions;
use crate::domain::time::TimeRange;

/// Address of a concrete remote calendar — the `calendarId` a logical
/// calendar binds to (ADR-0002 §9), e.g. `primary`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct CalendarRef(pub String);

impl From<String> for CalendarRef {
    fn from(s: String) -> Self {
        Self(s)
    }
}

impl From<&str> for CalendarRef {
    fn from(s: &str) -> Self {
        Self(s.to_owned())
    }
}

impl CalendarRef {
    /// Wrap an id.
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    /// Borrow the remote calendar id.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for CalendarRef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// The `cicada:*` private tags on a remote event (ADR-0002 §2).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EventTags {
    /// `cicada:id` — the local id this remote event is managed as.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub local_id: Option<LocalId>,
    /// `cicada:managed = "1"`.
    #[serde(default)]
    pub managed: bool,
    /// `cicada:state` — the recorded content hash at last apply.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state_hash: Option<ContentHash>,
}

/// An event as the provider reports it: the shared [`EventBody`]
/// payload plus remote-only metadata (id, tags, etag).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteEvent {
    /// Provider-assigned id.
    pub remote_id: RemoteId,
    /// `cicada:*` tags — `local_id: None` means unmanaged/foreign;
    /// the managed boundary never touches those (ADR-0002 §1).
    #[serde(default)]
    pub tags: EventTags,
    /// Remote ETag/sequence for conditional writes, when the provider
    /// exposes one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub etag: Option<String>,
    /// Remote `updated` timestamp, when the provider exposes one —
    /// useful for drift diagnostics.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated: Option<Timestamp>,
    /// The event payload.
    pub body: EventBody,
}

/// The provider port — one adapter per backend (`cicada-google`
/// first). All methods take [`CalendarRef`]: a provider instance may
/// serve many remote calendars.
#[async_trait]
pub trait CalendarProvider: Send + Sync {
    /// What this provider can do — consulted at plan time by the
    /// `CapabilityChecker` (ADR-0001 §2). Synchronous: capabilities
    /// are static per adapter.
    fn capabilities(&self) -> ProviderCapabilities;

    /// List events overlapping `range` (or all events when `None`).
    /// The result includes unmanaged events — the caller delimits the
    /// managed boundary via tags/recorded state.
    async fn list_events(
        &self,
        calendar: &CalendarRef,
        range: Option<TimeRange>,
    ) -> Result<Vec<RemoteEvent>>;

    /// Fetch one event by remote id; `Ok(None)` when absent.
    async fn get_event(
        &self,
        calendar: &CalendarRef,
        remote_id: &RemoteId,
    ) -> Result<Option<RemoteEvent>>;

    /// Find the managed event carrying `cicada:id == local_id`
    /// (ADR-0002 §3). Resume calls this before re-executing any
    /// `Create`/`Replace`: a hit means the op already landed.
    async fn find_by_tag(
        &self,
        calendar: &CalendarRef,
        local_id: &LocalId,
    ) -> Result<Option<RemoteEvent>>;

    /// Create `event` remotely. The returned record carries the
    /// assigned [`RemoteId`] and should echo the tags written.
    async fn create_event(
        &self,
        calendar: &CalendarRef,
        event: &EventBody,
        local_id: &LocalId,
        opts: &WriteOptions,
    ) -> Result<RemoteEvent>;

    /// Update the remote event in place.
    async fn update_event(
        &self,
        calendar: &CalendarRef,
        remote_id: &RemoteId,
        event: &EventBody,
        opts: &WriteOptions,
    ) -> Result<RemoteEvent>;

    /// Delete the remote event. Deleting an already-absent id should
    /// succeed — `Delete` is idempotent against remote ids
    /// (ADR-0002 §3).
    async fn delete_event(
        &self,
        calendar: &CalendarRef,
        remote_id: &RemoteId,
        opts: &WriteOptions,
    ) -> Result<()>;

    /// Busy intervals for this calendar in `range` (validation layer
    /// 4 — optional conflict check, ADR-0001 §7). Adapters that can't
    /// answer must return `Fatal`, never a fake empty list.
    async fn free_busy(&self, calendar: &CalendarRef, range: TimeRange) -> Result<Vec<TimeRange>>;
}
