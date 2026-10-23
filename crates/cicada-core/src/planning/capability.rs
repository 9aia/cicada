//! `CapabilityChecker` — validation layer 3 (ADR-0001 §7): does the
//! provider support what the desired state declares?
//!
//! Unsupported features produce **errors** (never silent drops);
//! degraded-but-harmless behavior produces **warnings** — e.g. a
//! provider that can't suppress notifications warns, because the
//! apply still works, it just emails people (ADR-0002 §5).

use serde::{Deserialize, Serialize};

use crate::domain::capabilities::ProviderCapabilities;
use crate::domain::event::{Event, EventBody};
use crate::domain::policy::SendUpdates;
use crate::domain::recurrence::RecurrenceRule;

/// How bad a capability gap is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Severity {
    /// Plan-time notice; apply still proceeds.
    Warning,
    /// Unsupported desired state — blocks the op.
    Error,
}

/// One capability issue found for an event (or calendar-wide
/// policy).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CapabilityIssue {
    /// Severity.
    pub severity: Severity,
    /// Local event id, when the issue is event-scoped.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub local_id: Option<String>,
    /// Human-readable explanation.
    pub message: String,
}

impl CapabilityIssue {
    fn error(local_id: &str, message: String) -> Self {
        Self {
            severity: Severity::Error,
            local_id: Some(local_id.to_owned()),
            message,
        }
    }
}

/// Checks declared features against [`ProviderCapabilities`]. Pure
/// and stateless.
#[derive(Debug, Clone, Copy, Default)]
pub struct CapabilityChecker;

impl CapabilityChecker {
    /// Check one event against provider capabilities.
    pub fn check_event(&self, event: &Event, caps: &ProviderCapabilities) -> Vec<CapabilityIssue> {
        self.check_body(&event.body, Some(event.id.as_str()), caps)
    }

    /// Check a payload (remote events have no local id to report).
    pub fn check_body(
        &self,
        body: &EventBody,
        local_id: Option<&str>,
        caps: &ProviderCapabilities,
    ) -> Vec<CapabilityIssue> {
        let mut issues = Vec::new();
        let id = local_id.unwrap_or("<unknown>");

        if let Some(max) = caps.max_attendees {
            if body.attendees.len() as u32 > max {
                issues.push(CapabilityIssue::error(
                    id,
                    format!(
                        "declares {} attendees but the provider allows at most {max}",
                        body.attendees.len()
                    ),
                ));
            }
        }
        if let Some(max) = caps.max_reminders {
            if body.reminders.len() as u32 > max {
                issues.push(CapabilityIssue::error(
                    id,
                    format!(
                        "declares {} reminders but the provider allows at most {max}",
                        body.reminders.len()
                    ),
                ));
            }
        }
        for r in &body.reminders {
            if !caps.supports_reminder(r.kind) {
                issues.push(CapabilityIssue::error(
                    id,
                    format!("provider cannot deliver {} reminders", r.kind),
                ));
            }
        }

        if let Some(rule) = &body.recurrence {
            Self::check_recurrence(rule, id, caps, &mut issues);
        }

        if let Some(kind) = &body.conferencing {
            if !caps.supports_conferencing(kind) {
                issues.push(CapabilityIssue::error(
                    id,
                    format!("provider cannot attach {kind} conferencing"),
                ));
            }
        }
        issues
    }

    fn check_recurrence(
        rule: &RecurrenceRule,
        local_id: &str,
        caps: &ProviderCapabilities,
        issues: &mut Vec<CapabilityIssue>,
    ) {
        let mut err = |m: String| {
            issues.push(CapabilityIssue::error(local_id, m));
        };
        match rule {
            RecurrenceRule::Structured(r) => {
                let r = r.clone().normalized();
                if !caps.recurrence.supports_frequency(r.freq) {
                    err(format!("provider does not support {} recurrence", r.freq));
                }
                if r.by_day.iter().any(|b| b.ordinal.is_some())
                    && !caps.recurrence.supports_ordinal_by_day
                {
                    err("provider does not support ordinal byDay (e.g. -1FR)".to_owned());
                }
                if !r.by_month_day.is_empty() && !caps.recurrence.supports_by_month_day {
                    err("provider does not support byMonthDay".to_owned());
                }
                if !r.by_month.is_empty() && !caps.recurrence.supports_by_month {
                    err("provider does not support byMonth".to_owned());
                }
            }
            RecurrenceRule::Raw(raw) => {
                // If it parses into the structured model, check the
                // parsed form; exotic rules need a provider that
                // accepts raw RRULEs verbatim.
                match rule.as_structured() {
                    Some(r) => {
                        if !caps.recurrence.supports_frequency(r.freq) {
                            err(format!("provider does not support {} recurrence", r.freq));
                        }
                        if r.by_day.iter().any(|b| b.ordinal.is_some())
                            && !caps.recurrence.supports_ordinal_by_day
                        {
                            err("provider does not support ordinal byDay".to_owned());
                        }
                        if !r.by_month_day.is_empty() && !caps.recurrence.supports_by_month_day {
                            err("provider does not support byMonthDay".to_owned());
                        }
                        if !r.by_month.is_empty() && !caps.recurrence.supports_by_month {
                            err("provider does not support byMonth".to_owned());
                        }
                    }
                    None => {
                        if !caps.recurrence.supports_raw_rrule {
                            err(format!("provider does not accept raw RRULEs: {raw:?}"));
                        }
                    }
                }
            }
        }
    }

    /// Check the notification policy against the provider: a
    /// non-`All` policy on a provider that can't suppress
    /// notifications degrades to a warning (ADR-0002 §5).
    pub fn check_send_updates(
        &self,
        policy: SendUpdates,
        caps: &ProviderCapabilities,
    ) -> Option<CapabilityIssue> {
        if policy != SendUpdates::All && !caps.can_suppress_notifications {
            return Some(CapabilityIssue {
                severity: Severity::Warning,
                local_id: None,
                message: format!(
                    "provider cannot suppress attendee notifications \
                     (sendUpdates: {policy:?}) — writes will notify everyone"
                ),
            });
        }
        None
    }
}
