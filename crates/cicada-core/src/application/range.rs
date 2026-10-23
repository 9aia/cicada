//! `QueryRange` — expand events into concrete occurrences inside a
//! half-open `[start, end)` window (ADR-0001 §3, §5).
//!
//! Powers `cicada range` and `cicada show`. Two views over the same
//! machinery: **desired** (`Event`s from parsed YAML — the local
//! view) and **live** (`RemoteEvent`s from a provider — the
//! `--provider` view). Wall-time stays wall-time: a "09:00"
//! America/New_York standup expands to 09:00 local across DST, and
//! the materialized instants reflect the offset change (ADR-0002 §6).
//!
//! Inclusion rule: an occurrence is listed when its *start* instant
//! falls inside the range. Pure — no I/O.

use jiff::civil::DateTime;
use jiff::{SignedDuration, Timestamp};
use serde::{Deserialize, Serialize};

use crate::domain::error::{CicadaError, Result};
use crate::domain::event::{Event, EventBody};
use crate::domain::ids::{LocalId, RemoteId, ZoneId};
use crate::domain::recurrence::{Occurrence, RecurrenceExpander};
use crate::domain::time::{Time, TimeRange};
use crate::ports::RemoteEvent;

/// One concrete event occurrence in a range view.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RangeOccurrence {
    /// Stable local id (desired events) or `cicada:id` tag (live).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub local_id: Option<LocalId>,
    /// Remote id (live views only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remote_id: Option<RemoteId>,
    /// Event title.
    pub summary: String,
    /// Wall-time start as declared (`local` + zone civil time).
    pub local_start: DateTime,
    /// Wall-time end (`local_start` + declared duration, civil math).
    pub local_end: DateTime,
    /// Zone the local times are in (`None` = UTC, for
    /// instant-anchored events).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub zone: Option<ZoneId>,
    /// Materialized start instant — after DST resolution.
    pub start: Timestamp,
    /// Materialized end instant.
    pub end: Timestamp,
    /// Whether it came from expanding a recurrence rule.
    pub recurring: bool,
}

/// Expands recurrence into [`RangeOccurrence`]s. Stateless.
#[derive(Debug, Clone, Copy, Default)]
pub struct QueryRange;

impl QueryRange {
    /// A ready-to-use use case.
    pub fn new() -> Self {
        Self
    }

    /// Expand desired events (parsed YAML) over `range`, sorted by
    /// start instant.
    ///
    /// # Errors
    /// [`CicadaError::Fatal`] when a recurrence can't expand
    /// (non-wall start, exotic raw RRULE, sub-daily freq) or a time
    /// can't materialize.
    ///
    /// [`CicadaError::Fatal`]: crate::domain::error::CicadaError
    pub fn expand<'a>(
        &self,
        events: impl IntoIterator<Item = &'a Event>,
        range: TimeRange,
    ) -> Result<Vec<RangeOccurrence>> {
        let mut out = Vec::new();
        for ev in events {
            occurrences_for(&ev.body, Some(&ev.id), None, range, &mut out)?;
        }
        out.sort_by_key(|o| (o.start, o.end));
        Ok(out)
    }

    /// Expand live remote events over `range` — the `--provider`
    /// view. `cicada:id`-tagged events report their local id.
    ///
    /// # Errors
    /// As [`QueryRange::expand`].
    pub fn expand_remote(
        &self,
        events: &[RemoteEvent],
        range: TimeRange,
    ) -> Result<Vec<RangeOccurrence>> {
        let mut out = Vec::new();
        for ev in events {
            occurrences_for(
                &ev.body,
                ev.tags.local_id.as_ref(),
                Some(&ev.remote_id),
                range,
                &mut out,
            )?;
        }
        out.sort_by_key(|o| (o.start, o.end));
        Ok(out)
    }
}

/// All in-range occurrences of one event body.
fn occurrences_for(
    body: &EventBody,
    local_id: Option<&LocalId>,
    remote_id: Option<&RemoteId>,
    range: TimeRange,
    out: &mut Vec<RangeOccurrence>,
) -> Result<()> {
    let Some(rule) = &body.recurrence else {
        let occ = single_occurrence(body, local_id, remote_id)?;
        if range.contains(occ.start) {
            out.push(occ);
        }
        return Ok(());
    };
    let duration = body.time.duration()?;
    for occ in RecurrenceExpander::default().expand(rule, body.time.start(), range)? {
        out.push(render(body, local_id, remote_id, occ, duration, true)?);
    }
    Ok(())
}

/// The one occurrence of a non-recurring event — range-agnostic so
/// `show` can display an event outside its window.
pub(crate) fn single_occurrence(
    body: &EventBody,
    local_id: Option<&LocalId>,
    remote_id: Option<&RemoteId>,
) -> Result<RangeOccurrence> {
    let start = body.time.start();
    let occ = Occurrence {
        local: start.local(),
        instant: start.to_timestamp()?,
    };
    render(body, local_id, remote_id, occ, body.time.duration()?, false)
}

/// Materialize one [`RangeOccurrence`]: civil end for wall-time
/// events (`09:00 + 30m` is `09:30` local), absolute end for
/// instant-anchored ones.
fn render(
    body: &EventBody,
    local_id: Option<&LocalId>,
    remote_id: Option<&RemoteId>,
    occ: Occurrence,
    duration: SignedDuration,
    recurring: bool,
) -> Result<RangeOccurrence> {
    let (local_end, end, zone) = match body.time.start() {
        Time::Wall { zone, .. } => {
            let local_end = occ.local.checked_add(duration).map_err(|e| {
                CicadaError::fatal(format!(
                    "occurrence end overflowed for {:?}: {e}",
                    body.summary
                ))
            })?;
            let tz = zone.timezone()?;
            let end = local_end.to_zoned(tz).map_err(|e| {
                CicadaError::fatal(format!("{local_end} in {zone} does not resolve: {e}"))
            })?;
            (local_end, end.timestamp(), Some(zone.clone()))
        }
        Time::Instant(_) => {
            let end = occ.instant + duration;
            (end.to_zoned(jiff::tz::TimeZone::UTC).datetime(), end, None)
        }
    };
    Ok(RangeOccurrence {
        local_id: local_id.cloned(),
        remote_id: remote_id.cloned(),
        summary: body.summary.clone(),
        local_start: occ.local,
        local_end,
        zone,
        start: occ.instant,
        end,
        recurring,
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use jiff::civil::date;

    use super::*;
    use crate::domain::event::EventTime;
    use crate::domain::recurrence::{Frequency, RecurrenceRule, StructuredRule};
    use crate::ports::EventTags;

    fn wall(d: jiff::civil::Date, h: i8, zone: &str) -> Time {
        Time::Wall {
            local: d.at(h, 0, 0, 0),
            zone: ZoneId::new_unchecked(zone),
        }
    }

    fn body(start: Time) -> EventBody {
        EventBody {
            summary: "Standup".into(),
            description: None,
            location: None,
            time: EventTime::Span {
                start,
                duration: SignedDuration::from_mins(30),
            },
            recurrence: None,
            attendees: vec![],
            reminders: vec![],
            conferencing: None,
            transparency: crate::domain::event::Transparency::Opaque,
        }
    }

    fn range(a: &str, b: &str) -> TimeRange {
        TimeRange::new(a.parse().unwrap(), b.parse().unwrap()).unwrap()
    }

    #[test]
    fn non_recurring_in_range_only() {
        let inside = Event::new("in", body(wall(date(2026, 1, 6), 9, "UTC")));
        let outside = Event::new("out", body(wall(date(2026, 2, 1), 9, "UTC")));
        let r = range("2026-01-05T00:00:00Z", "2026-01-12T00:00:00Z");
        let occs = QueryRange.expand([&inside, &outside], r).unwrap();
        assert_eq!(occs.len(), 1);
        assert_eq!(occs[0].local_id.as_ref().map(LocalId::as_str), Some("in"));
        assert!(!occs[0].recurring);
        assert_eq!(occs[0].local_end, date(2026, 1, 6).at(9, 30, 0, 0));
    }

    #[test]
    fn recurring_expands_and_stays_wall_time_across_dst() {
        // Weekly Monday 09:00 America/New_York across the 2026-03-08
        // spring-forward: local stays 09:00; the instant shifts an
        // hour earlier in UTC.
        let mut b = body(wall(date(2026, 3, 2), 9, "America/New_York"));
        b.recurrence = Some(RecurrenceRule::Structured(StructuredRule::new(
            Frequency::Weekly,
        )));
        let ev = Event::new("standup", b);
        let r = range("2026-03-01T00:00:00Z", "2026-03-22T00:00:00Z");
        let occs = QueryRange.expand([&ev], r).unwrap();
        assert_eq!(occs.len(), 3);
        assert!(occs.iter().all(|o| o.recurring));
        // March 2 + 9 are EST (UTC-5) → 14:00Z.
        assert_eq!(occs[0].start.to_string(), "2026-03-02T14:00:00Z");
        assert_eq!(occs[1].start.to_string(), "2026-03-09T13:00:00Z");
        // March 16 is EDT (UTC-4) → 13:00Z, still 09:00 local.
        assert_eq!(occs[2].start.to_string(), "2026-03-16T13:00:00Z");
        assert!(occs.iter().all(|o| o.local_start.time().hour() == 9));
    }

    #[test]
    fn remote_events_expand_their_rules() {
        let mut b = body(wall(date(2026, 1, 5), 9, "UTC"));
        b.recurrence = Some(RecurrenceRule::Raw("FREQ=DAILY;COUNT=3".into()));
        let remote = RemoteEvent {
            remote_id: "r-1".into(),
            tags: EventTags {
                local_id: Some("standup".into()),
                managed: true,
                state_hash: None,
            },
            etag: None,
            updated: None,
            body: b,
        };
        let r = range("2026-01-01T00:00:00Z", "2026-02-01T00:00:00Z");
        let occs = QueryRange.expand_remote(&[remote], r).unwrap();
        assert_eq!(occs.len(), 3);
        assert_eq!(
            occs[0].remote_id.as_ref().map(RemoteId::as_str),
            Some("r-1")
        );
        assert_eq!(
            occs[0].local_id.as_ref().map(LocalId::as_str),
            Some("standup")
        );
    }

    #[test]
    fn instant_events_materialize_in_utc() {
        let mut b = body(Time::Instant("2026-01-05T09:00:00Z".parse().unwrap()));
        b.recurrence = None;
        let ev = Event::new("one-off", b);
        let r = range("2026-01-01T00:00:00Z", "2026-02-01T00:00:00Z");
        let occs = QueryRange.expand([&ev], r).unwrap();
        assert_eq!(occs.len(), 1);
        assert_eq!(occs[0].start.to_string(), "2026-01-05T09:00:00Z");
        assert_eq!(occs[0].local_start, date(2026, 1, 5).at(9, 0, 0, 0));
        assert_eq!(occs[0].end.to_string(), "2026-01-05T09:30:00Z");
    }
}
