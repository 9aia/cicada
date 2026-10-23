//! Provider capabilities — first-class on `CalendarProvider`
//! (ADR-0001 §2). The planner consults these at plan time and emits
//! warnings or errors for unsupported desired states instead of
//! silently dropping fields.

use serde::{Deserialize, Serialize};

use super::event::Conferencing;
use super::recurrence::Frequency;
use super::reminder::ReminderKind;

/// What a provider can and cannot do. `Option` fields are limits
/// (`None` = unbounded/unknown); `Vec` fields enumerate support.
///
/// Adapters should report conservatively — a capability claimed but
/// not honored produces silent data loss, which is exactly what the
/// model exists to prevent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderCapabilities {
    /// Maximum attendees per event (`None` = no known limit).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_attendees: Option<u32>,
    /// Maximum reminders per event.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_reminders: Option<u32>,
    /// Reminder kinds the provider can deliver.
    #[serde(default)]
    pub reminder_kinds: Vec<ReminderKind>,
    /// Which recurrence features the provider honors.
    #[serde(default)]
    pub recurrence: RecurrenceCapabilities,
    /// Conferencing kinds the provider can attach (`meet`, …).
    #[serde(default)]
    pub conferencing_kinds: Vec<Conferencing>,
    /// Whether the provider can suppress attendee update
    /// notifications (`sendUpdates`, ADR-0002 §5). Providers that
    /// can't produce plan warnings, not silent spam.
    #[serde(default)]
    pub can_suppress_notifications: bool,
    /// Whether `freeBusy` queries are available.
    #[serde(default)]
    pub supports_free_busy: bool,
    /// Whether provider-side private tags (`cicada:id`, …) exist —
    /// required for state rebuild, adoption, and safe resume
    /// (ADR-0002 §2/§3).
    #[serde(default)]
    pub supports_tagging: bool,
    /// Whether conditional writes (ETag/sequence) exist —
    /// optimistic concurrency for `Replace` (ADR-0002 §4).
    #[serde(default)]
    pub supports_conditional_writes: bool,
}

impl ProviderCapabilities {
    /// A provider claiming to support everything — tests and the
    /// fake provider start here.
    pub fn permissive() -> Self {
        Self {
            max_attendees: None,
            max_reminders: None,
            reminder_kinds: vec![ReminderKind::Popup, ReminderKind::Email],
            recurrence: RecurrenceCapabilities::permissive(),
            conferencing_kinds: vec![Conferencing::Meet, Conferencing::Teams],
            can_suppress_notifications: true,
            supports_free_busy: true,
            supports_tagging: true,
            supports_conditional_writes: true,
        }
    }

    /// A provider claiming nothing — capability checks produce
    /// errors for any feature at all.
    pub fn nothing() -> Self {
        Self {
            max_attendees: Some(0),
            max_reminders: Some(0),
            reminder_kinds: vec![],
            recurrence: RecurrenceCapabilities::default(),
            conferencing_kinds: vec![],
            can_suppress_notifications: false,
            supports_free_busy: false,
            supports_tagging: false,
            supports_conditional_writes: false,
        }
    }

    /// Whether `kind` is in `reminder_kinds`.
    pub fn supports_reminder(&self, kind: ReminderKind) -> bool {
        self.reminder_kinds.contains(&kind)
    }

    /// Whether `kind` is in `conferencing_kinds`.
    pub fn supports_conferencing(&self, kind: &Conferencing) -> bool {
        self.conferencing_kinds.contains(kind)
    }
}

/// Recurrence edge cases a provider may or may not honor
/// (ADR-0001 §2).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RecurrenceCapabilities {
    /// Frequencies the provider expands natively.
    #[serde(default)]
    pub frequencies: Vec<Frequency>,
    /// Ordinal `byDay` (`-1FR`, `2TU`) under MONTHLY/YEARLY.
    #[serde(default)]
    pub supports_ordinal_by_day: bool,
    /// `byMonthDay` values (incl. negative-from-end).
    #[serde(default)]
    pub supports_by_month_day: bool,
    /// `byMonth` month selection.
    #[serde(default)]
    pub supports_by_month: bool,
    /// Raw `RRULE:` strings beyond the structured model.
    #[serde(default)]
    pub supports_raw_rrule: bool,
}

impl RecurrenceCapabilities {
    /// Claims everything.
    pub fn permissive() -> Self {
        Self {
            frequencies: vec![
                Frequency::Daily,
                Frequency::Weekly,
                Frequency::Monthly,
                Frequency::Yearly,
            ],
            supports_ordinal_by_day: true,
            supports_by_month_day: true,
            supports_by_month: true,
            supports_raw_rrule: true,
        }
    }

    /// Whether `freq` is listed.
    pub fn supports_frequency(&self, freq: Frequency) -> bool {
        self.frequencies.contains(&freq)
    }
}
