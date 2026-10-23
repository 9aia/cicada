//! Attendees and RSVP state.

use serde::{Deserialize, Serialize};

/// A person on an event. `email` is the stable identity cicada
/// matches on when diffing attendee lists.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Attendee {
    /// Email address — the attendee's identity.
    pub email: String,
    /// `true` = required attendee, `false` = optional (DSL `required`).
    #[serde(default = "default_required")]
    pub required: bool,
    /// Last-known RSVP status; mostly a provider-side concern, kept
    /// here so remote state round-trips faithfully.
    #[serde(default)]
    pub status: ResponseStatus,
}

fn default_required() -> bool {
    true
}

impl Attendee {
    /// A required attendee who hasn't responded yet.
    pub fn new(email: impl Into<String>) -> Self {
        Self {
            email: email.into(),
            required: true,
            status: ResponseStatus::NeedsAction,
        }
    }
}

/// RSVP response status (RFC 5545 PARTSTAT).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ResponseStatus {
    /// No response yet.
    #[default]
    NeedsAction,
    /// Accepted.
    Accepted,
    /// Declined.
    Declined,
    /// Tentative ("maybe").
    Tentative,
}
