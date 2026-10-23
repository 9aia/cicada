//! Reminders — a kind plus an offset before the event.

use jiff::SignedDuration;
use serde::{Deserialize, Serialize};

/// A reminder `offset` before the event (`{popup: 10m}` in the DSL).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Reminder {
    /// Delivery mechanism.
    pub kind: ReminderKind,
    /// How long before the event it fires.
    pub offset: SignedDuration,
}

impl Reminder {
    /// A popup reminder.
    pub fn popup(offset: SignedDuration) -> Self {
        Self {
            kind: ReminderKind::Popup,
            offset,
        }
    }

    /// An email reminder.
    pub fn email(offset: SignedDuration) -> Self {
        Self {
            kind: ReminderKind::Email,
            offset,
        }
    }
}

/// Reminder delivery kind. Providers differ in which they support —
/// checked against [`super::ProviderCapabilities::reminder_kinds`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ReminderKind {
    /// In-app popup/notification.
    Popup,
    /// Email reminder.
    Email,
}

impl std::fmt::Display for ReminderKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Popup => "popup",
            Self::Email => "email",
        })
    }
}
