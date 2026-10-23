//! `ShowEvent` — one event's detail plus its expanded occurrences
//! (the `cicada show <id>` view).
//!
//! Recurring events expand inside a caller-supplied window;
//! non-recurring events always report their single occurrence, even
//! when it lands outside the window — `show` should never render an
//! event with no "when".

use jiff::Timestamp;

use crate::application::range::{QueryRange, RangeOccurrence, single_occurrence};
use crate::domain::error::Result;
use crate::domain::event::Event;
use crate::domain::time::TimeRange;

/// Everything `cicada show` prints for one event: the declared
/// content plus concrete occurrences.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EventDetail {
    /// The desired event (id + body).
    pub event: Event,
    /// Expanded occurrences inside `window` — for non-recurring
    /// events, the single occurrence regardless of `window`.
    pub occurrences: Vec<RangeOccurrence>,
}

/// `ShowEvent` — stateless use case.
#[derive(Debug, Clone, Copy, Default)]
pub struct ShowEvent;

impl ShowEvent {
    /// A ready-to-use use case.
    pub fn new() -> Self {
        Self
    }

    /// Detail for `event` with occurrences expanded inside `window`.
    ///
    /// # Errors
    /// [`CicadaError::Fatal`] when the recurrence can't expand or a
    /// time can't materialize.
    ///
    /// [`CicadaError::Fatal`]: crate::domain::error::CicadaError
    pub fn run(&self, event: &Event, window: TimeRange) -> Result<EventDetail> {
        let mut occurrences = QueryRange.expand(std::iter::once(event), window)?;
        if occurrences.is_empty() && event.recurrence.is_none() {
            occurrences.push(single_occurrence(&event.body, Some(&event.id), None)?);
        }
        Ok(EventDetail {
            event: event.clone(),
            occurrences,
        })
    }
}

/// A sensible default `show` window: `now` rounded down to its day
/// start, `days` forward. Interfaces pick the zone; this just builds
/// the [`TimeRange`].
///
/// # Errors
/// [`CicadaError::Fatal`] when the span overflows.
///
/// [`CicadaError::Fatal`]: crate::domain::error::CicadaError
pub fn default_window(now: Timestamp, days: i64) -> Result<TimeRange> {
    let end = now
        .checked_add(jiff::SignedDuration::from_hours(24 * days))
        .map_err(|e| {
            crate::domain::error::CicadaError::fatal(format!("show window overflowed: {e}"))
        })?;
    TimeRange::new(now, end)
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use jiff::SignedDuration;
    use jiff::civil::date;

    use super::*;
    use crate::domain::event::{EventBody, EventTime, Transparency};
    use crate::domain::ids::ZoneId;
    use crate::domain::recurrence::{Frequency, RecurrenceRule, StructuredRule};
    use crate::domain::time::Time;

    fn event(id: &str) -> Event {
        Event::new(
            id,
            EventBody {
                summary: id.into(),
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
                transparency: Transparency::Opaque,
            },
        )
    }

    fn window(a: &str, b: &str) -> TimeRange {
        TimeRange::new(a.parse().unwrap(), b.parse().unwrap()).unwrap()
    }

    #[test]
    fn one_off_outside_window_still_shows() {
        let ev = event("party");
        // Window that misses the event entirely.
        let w = window("2026-02-01T00:00:00Z", "2026-03-01T00:00:00Z");
        let detail = ShowEvent.run(&ev, w).unwrap();
        assert_eq!(detail.occurrences.len(), 1);
        assert_eq!(
            detail.occurrences[0].local_start,
            date(2026, 1, 5).at(9, 0, 0, 0)
        );
    }

    #[test]
    fn recurring_expands_inside_window() {
        let mut ev = event("standup");
        ev.recurrence = Some(RecurrenceRule::Structured(StructuredRule::new(
            Frequency::Daily,
        )));
        let w = window("2026-01-05T00:00:00Z", "2026-01-08T00:00:00Z");
        let detail = ShowEvent.run(&ev, w).unwrap();
        assert_eq!(detail.occurrences.len(), 3);
        assert!(detail.occurrences.iter().all(|o| o.recurring));
    }

    #[test]
    fn default_window_builds_a_range() {
        let now: Timestamp = "2026-10-23T01:00:00Z".parse().unwrap();
        let w = default_window(now, 30).unwrap();
        assert_eq!(w.start, now);
        assert_eq!(w.duration().as_hours(), 30 * 24);
    }
}
