//! Apply-time policy knobs (ADR-0002 §5) and per-write options.

use serde::{Deserialize, Serialize};

/// Who providers notify when cicada writes an event.
/// `spec.apply.sendUpdates` in the DSL; also `--notify` and the
/// `plan.apply` tool parameter. Default per ADR: `ExternalOnly`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SendUpdates {
    /// Send no attendee notifications at all.
    None,
    /// Notify only attendees outside the provider's domain.
    /// The default — drift fixes shouldn't spam the team.
    #[default]
    ExternalOnly,
    /// Notify every attendee.
    All,
}

/// Per-write options threaded through `CalendarProvider` mutations.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WriteOptions {
    /// Notification policy for this write.
    #[serde(default)]
    pub send_updates: SendUpdates,
    /// Idempotency key
    /// (`calendarId + localId + opKind + contentHash`, ADR-0001 §4).
    /// Providers that support request ids use it; others ignore it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub idempotency_key: Option<String>,
    /// Recorded remote ETag/sequence for conditional writes
    /// (optimistic concurrency where available, ADR-0002 §4).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub etag: Option<String>,
}
