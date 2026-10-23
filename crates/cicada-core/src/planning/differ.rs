//! `EventDiffer` — field-level diff between desired and actual
//! event bodies.
//!
//! Semantics that matter to the planner (ADR-0001 §5): recurrence
//! compares **semantically** (normalized rules), so a YAML rewrite
//! that changes nothing produces no diff. A `recurrence` change is
//! what turns an `Update` into a `Replace` — see
//! [`EventDiff::changes`] for `EventField::Recurrence`.

use jiff::SignedDuration;

use crate::domain::attendee::Attendee;
use crate::domain::error::Result;
use crate::domain::event::{Event, EventBody};
use crate::domain::operation::{EventDiff, EventField, FieldChange};
use crate::domain::recurrence::RecurrenceRule;
use crate::domain::reminder::Reminder;
use crate::domain::time::Time;

/// Pure field differ — no state, no I/O.
#[derive(Debug, Clone, Copy, Default)]
pub struct EventDiffer;

impl EventDiffer {
    /// Diff desired vs actual event bodies. `desired`/`actual` are
    /// bodies (not `Event`) so remote records — which may carry no
    /// local id — diff against desired ones directly.
    ///
    /// # Errors
    /// [`CicadaError::Fatal`] when a time can't be rendered for the
    /// diff text (unresolvable zone).
    pub fn diff(&self, desired: &EventBody, actual: &EventBody) -> Result<EventDiff> {
        let mut changes = Vec::new();

        if desired.summary != actual.summary {
            changes.push(FieldChange {
                field: EventField::Summary,
                from: Some(actual.summary.clone()),
                to: Some(desired.summary.clone()),
            });
        }
        push_opt(
            &mut changes,
            EventField::Description,
            actual.description.clone(),
            desired.description.clone(),
        );
        push_opt(
            &mut changes,
            EventField::Location,
            actual.location.clone(),
            desired.location.clone(),
        );

        if desired.time != actual.time {
            changes.push(FieldChange {
                field: EventField::Time,
                from: Some(render_time(&actual.time)?),
                to: Some(render_time(&desired.time)?),
            });
        }

        if desired.recurrence != actual.recurrence {
            changes.push(FieldChange {
                field: EventField::Recurrence,
                from: actual.recurrence.as_ref().map(RecurrenceRule::describe),
                to: desired.recurrence.as_ref().map(RecurrenceRule::describe),
            });
        }

        if !same_attendees(&desired.attendees, &actual.attendees) {
            changes.push(FieldChange {
                field: EventField::Attendees,
                from: Some(render_attendees(&actual.attendees)),
                to: Some(render_attendees(&desired.attendees)),
            });
        }

        if !same_reminders(&desired.reminders, &actual.reminders) {
            changes.push(FieldChange {
                field: EventField::Reminders,
                from: Some(render_reminders(&actual.reminders)),
                to: Some(render_reminders(&desired.reminders)),
            });
        }

        push_opt(
            &mut changes,
            EventField::Conferencing,
            actual.conferencing.as_ref().map(ToString::to_string),
            desired.conferencing.as_ref().map(ToString::to_string),
        );

        if desired.transparency != actual.transparency {
            changes.push(FieldChange {
                field: EventField::Transparency,
                from: Some(actual.transparency.to_string()),
                to: Some(desired.transparency.to_string()),
            });
        }

        Ok(EventDiff { changes })
    }

    /// Diff two desired-side events (recorded-hash comparison isn't
    /// available in memory — callers comparing recorded state should
    /// hash instead). Thin convenience over [`EventDiffer::diff`].
    ///
    /// # Errors
    /// As [`EventDiffer::diff`].
    pub fn diff_events(&self, desired: &Event, actual: &EventBody) -> Result<EventDiff> {
        self.diff(&desired.body, actual)
    }
}

/// Push a change only when `from`/`to` actually differ.
fn push_opt(
    changes: &mut Vec<FieldChange>,
    field: EventField,
    from: Option<String>,
    to: Option<String>,
) {
    if from != to {
        changes.push(FieldChange { field, from, to });
    }
}

/// Attendee lists compare as sets keyed on email (order is noise).
fn same_attendees(a: &[Attendee], b: &[Attendee]) -> bool {
    let mut a: Vec<&Attendee> = a.iter().collect();
    let mut b: Vec<&Attendee> = b.iter().collect();
    a.sort_by(|x, y| x.email.cmp(&y.email));
    b.sort_by(|x, y| x.email.cmp(&y.email));
    a == b
}

/// Reminder lists compare as (kind, offset) sets.
fn same_reminders(a: &[Reminder], b: &[Reminder]) -> bool {
    let mut a: Vec<(crate::domain::reminder::ReminderKind, i128)> =
        a.iter().map(|r| (r.kind, r.offset.as_nanos())).collect();
    let mut b: Vec<(crate::domain::reminder::ReminderKind, i128)> =
        b.iter().map(|r| (r.kind, r.offset.as_nanos())).collect();
    a.sort();
    b.sort();
    a == b
}

fn render_time(t: &crate::domain::event::EventTime) -> Result<String> {
    let start = match t.start() {
        Time::Wall { local, zone } => format!("{local} [{zone}]"),
        Time::Instant(ts) => ts.to_string(),
    };
    let end = t.end()?;
    let end = match &end {
        Time::Wall { local, zone } => format!("{local} [{zone}]"),
        Time::Instant(ts) => ts.to_string(),
    };
    let dur: SignedDuration = t.duration()?;
    Ok(format!("{start} → {end} ({dur:?})"))
}

fn render_attendees(attendees: &[Attendee]) -> String {
    let mut emails: Vec<String> = attendees
        .iter()
        .map(|a| {
            if a.required {
                a.email.clone()
            } else {
                format!("{} (optional)", a.email)
            }
        })
        .collect();
    emails.sort();
    emails.join(", ")
}

fn render_reminders(reminders: &[Reminder]) -> String {
    let mut items: Vec<String> = reminders
        .iter()
        .map(|r| format!("{} {:?}", r.kind, r.offset))
        .collect();
    items.sort();
    items.join(", ")
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use jiff::civil::date;

    use super::*;
    use crate::domain::attendee::ResponseStatus;
    use crate::domain::event::EventTime;
    use crate::domain::ids::ZoneId;
    use crate::domain::recurrence::{Frequency, StructuredRule};

    fn body() -> EventBody {
        EventBody {
            summary: "Standup".into(),
            description: None,
            location: None,
            time: EventTime::Span {
                start: Time::Wall {
                    local: date(2026, 1, 5).at(9, 0, 0, 0),
                    zone: ZoneId::new_unchecked("UTC"),
                },
                duration: SignedDuration::from_mins(30),
            },
            recurrence: None,
            attendees: vec![],
            reminders: vec![],
            conferencing: None,
            transparency: crate::domain::event::Transparency::Opaque,
        }
    }

    #[test]
    fn identical_bodies_diff_empty() {
        let d = EventDiffer.diff(&body(), &body()).unwrap();
        assert!(d.is_empty());
    }

    #[test]
    fn summary_change_is_a_diff() {
        let mut b = body();
        b.summary = "Sync".into();
        let d = EventDiffer.diff(&b, &body()).unwrap();
        assert_eq!(d.changes.len(), 1);
        assert_eq!(d.changes[0].field, EventField::Summary);
        assert_eq!(d.changes[0].from.as_deref(), Some("Standup"));
        assert_eq!(d.changes[0].to.as_deref(), Some("Sync"));
    }

    #[test]
    fn reordered_attendees_are_not_a_diff() {
        let a1 = Attendee {
            email: "a@x".into(),
            required: true,
            status: ResponseStatus::NeedsAction,
        };
        let a2 = Attendee {
            email: "b@x".into(),
            required: false,
            status: ResponseStatus::NeedsAction,
        };
        let mut x = body();
        let mut y = body();
        x.attendees = vec![a1.clone(), a2.clone()];
        y.attendees = vec![a2, a1];
        assert!(EventDiffer.diff(&x, &y).unwrap().is_empty());
        y.attendees.pop();
        let d = EventDiffer.diff(&x, &y).unwrap();
        assert_eq!(d.changes[0].field, EventField::Attendees);
    }

    #[test]
    fn semantically_equal_recurrence_is_not_a_diff() {
        let mut x = body();
        let mut y = body();
        let mut s = StructuredRule::new(Frequency::Weekly);
        s.by_day = vec![
            crate::domain::recurrence::ByDay::every(crate::domain::recurrence::Weekday::Friday),
            crate::domain::recurrence::ByDay::every(crate::domain::recurrence::Weekday::Monday),
        ];
        x.recurrence = Some(RecurrenceRule::Structured(s));
        y.recurrence = Some(RecurrenceRule::Raw("FREQ=WEEKLY;BYDAY=MO,FR".into()));
        assert!(EventDiffer.diff(&x, &y).unwrap().is_empty());
    }

    #[test]
    fn recurrence_change_is_flagged() {
        let mut x = body();
        let mut y = body();
        x.recurrence = Some(RecurrenceRule::Raw("FREQ=WEEKLY".into()));
        y.recurrence = Some(RecurrenceRule::Raw("FREQ=DAILY".into()));
        let d = EventDiffer.diff(&x, &y).unwrap();
        assert_eq!(d.changes[0].field, EventField::Recurrence);
    }
}
