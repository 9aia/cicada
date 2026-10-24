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

impl SendUpdates {
    /// `true` for the ADR-0002 §5 default (`ExternalOnly`).
    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }

    /// Wire form (`none` / `externalOnly` / `all`) — for flag parsing
    /// and diagnostics.
    pub fn code(&self) -> &'static str {
        match self {
            Self::None => "none",
            Self::ExternalOnly => "externalOnly",
            Self::All => "all",
        }
    }

    /// Parse the `spec.apply.sendUpdates` / `--notify` spellings.
    ///
    /// # Errors
    /// [`CicadaError::Fatal`] on an unknown value.
    pub fn parse(s: &str) -> crate::domain::error::Result<Self> {
        match s {
            "none" => Ok(Self::None),
            "externalOnly" | "external-only" | "external_only" => Ok(Self::ExternalOnly),
            "all" => Ok(Self::All),
            other => Err(crate::domain::error::CicadaError::fatal(format!(
                "unknown sendUpdates {other:?} (expected none | externalOnly | all)"
            ))),
        }
    }
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

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn parse_accepts_every_spelling() {
        assert_eq!(SendUpdates::parse("none").unwrap(), SendUpdates::None);
        for spelling in ["externalOnly", "external-only", "external_only"] {
            assert_eq!(
                SendUpdates::parse(spelling).unwrap(),
                SendUpdates::ExternalOnly
            );
        }
        assert_eq!(SendUpdates::parse("all").unwrap(), SendUpdates::All);
        let err = SendUpdates::parse("sometimes").unwrap_err();
        assert!(err.to_string().contains("unknown sendUpdates"));
    }
}
