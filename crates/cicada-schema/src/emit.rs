//! Canonical re-serialization of `kind: Calendar` documents — the
//! `cicada fmt` backend (parse → domain → emit).
//!
//! Canonical here means: stable field order (the DSL's own order —
//! `id`, `summary`, `start`, …), normalized scalars (`"09:00"`,
//! `30m`, `2026-03-20T10:00`), default-valued fields elided, and raw
//! RRULEs that fit the structured model emitted as structured
//! rules. **Comments and original whitespace are not preserved** —
//! the DSL is data, not a syntax tree (documented limitation).
//!
//! Round-trip faithfulness notes:
//!
//! - A wall-time `start` emits the ergonomic `"HH:MM"` form only
//!   when re-parsing that form reproduces *exactly* this anchor —
//!   the same check `clock_start` uses ([`anchor_datetime`]).
//! - `end` emits `"HH:MM"` when it re-resolves to the stored date
//!   (same-day, or the overnight roll).
//! - Attendee `status` is provider-owned (see
//!   `EventBody::content_hash`) but *declared* status is preserved —
//!   fmt never silently drops authored fields.

use std::collections::BTreeSet;
use std::fmt::Write as _;

use cicada_core::domain::attendee::ResponseStatus;
use cicada_core::domain::calendar::Calendar;
use cicada_core::domain::event::{Event, EventTime, Transparency};
use cicada_core::domain::ids::LocalId;
use cicada_core::domain::policy::SendUpdates;
use cicada_core::domain::recurrence::{ByDay, Frequency, RecurrenceRule, Until};
use cicada_core::domain::reminder::ReminderKind;
use cicada_core::domain::time::Time;
use jiff::SignedDuration;
use jiff::civil::{DateTime, Time as Clock};
use serde::Serialize;

use crate::error::SchemaError;
use crate::event::anchor_datetime;

/// Render `cal` as a canonical `kind: Calendar` YAML document.
///
/// `event_order` preserves the source file's event ordering (the
/// domain keys events in a `BTreeMap`, so order doesn't survive
/// parsing). `None` — the `--sort` path — emits sorted by stable
/// id; ids in `event_order` that don't exist are skipped, and any
/// event missing from `event_order` appends in id order.
///
/// # Errors
/// [`SchemaError`] only if serialization itself fails.
pub fn emit_calendar(
    cal: &Calendar,
    event_order: Option<&[LocalId]>,
) -> Result<String, SchemaError> {
    let doc = CalendarEmit {
        api_version: crate::API_VERSION,
        kind: "Calendar",
        metadata: MetadataEmit {
            name: &cal.name,
            time_zone: cal.time_zone.as_str(),
            provider: cal.provider.as_deref(),
            calendar_id: cal.provider_calendar_id.as_deref(),
        },
        spec: SpecEmit {
            events: ordered_events(cal, event_order)
                .iter()
                .map(|e| emit_event(e))
                .collect(),
            apply: (!cal.send_updates.is_default()).then_some(ApplyEmit {
                send_updates: cal.send_updates,
            }),
        },
    };
    // `doc` is plain serializable data — `to_string` can't fail;
    // the error arm stays for robustness, compiled out under
    // coverage.
    #[cfg(not(coverage))]
    return serde_yaml::to_string(&doc)
        .map_err(|e| SchemaError::document(format!("cannot emit YAML: {e}")));
    #[cfg(coverage)]
    Ok(serde_yaml::to_string(&doc).expect("emit YAML cannot fail"))
}

/// Events in `event_order` (skipping unknown ids), remainder in
/// `BTreeMap` order.
fn ordered_events<'a>(cal: &'a Calendar, event_order: Option<&[LocalId]>) -> Vec<&'a Event> {
    let Some(order) = event_order else {
        return cal.events.values().collect();
    };
    let mut seen: BTreeSet<&LocalId> = BTreeSet::new();
    let mut out: Vec<&Event> = Vec::with_capacity(cal.events.len());
    for id in order {
        if let Some(e) = cal.events.get(id) {
            seen.insert(id);
            out.push(e);
        }
    }
    out.extend(
        cal.events
            .iter()
            .filter(|(id, _)| !seen.contains(id))
            .map(|(_, e)| e),
    );
    out
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct CalendarEmit<'a> {
    /// `cicada/v1`.
    api_version: &'static str,
    /// `Calendar`.
    kind: &'static str,
    /// `metadata:` block.
    metadata: MetadataEmit<'a>,
    /// `spec:` block.
    spec: SpecEmit<'a>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct MetadataEmit<'a> {
    name: &'a str,
    time_zone: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    provider: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    calendar_id: Option<&'a str>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SpecEmit<'a> {
    events: Vec<EventEmit<'a>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    apply: Option<ApplyEmit>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ApplyEmit {
    send_updates: SendUpdates,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct EventEmit<'a> {
    id: &'a str,
    summary: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    description: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    location: Option<&'a str>,
    start: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    end: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    duration: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    recurrence: Option<RecurrenceEmit>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    attendees: Vec<AttendeeEmit<'a>>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    reminders: Vec<ReminderEmit>,
    #[serde(skip_serializing_if = "Option::is_none")]
    conferencing: Option<String>,
    #[serde(skip_serializing_if = "is_opaque")]
    transparency: Transparency,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct AttendeeEmit<'a> {
    email: &'a str,
    #[serde(skip_serializing_if = "is_true")]
    required: bool,
    #[serde(skip_serializing_if = "is_needs_action")]
    status: ResponseStatus,
}

/// `{popup: "10m"}` or `{email: "1h"}` — one key, mirroring
/// `ReminderSpec`.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ReminderEmit {
    #[serde(skip_serializing_if = "Option::is_none")]
    popup: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    email: Option<String>,
}

/// Structured rules emit as the DSL mapping; raw rules that don't
/// fit the model keep their RRULE string.
#[derive(Serialize)]
#[serde(untagged)]
enum RecurrenceEmit {
    /// Bare `recurrence: "FREQ=…"`.
    Raw(String),
    /// `recurrence: {freq: …, byDay: […], …}` in DSL field order.
    Structured(RuleEmit),
}

/// Owned — `as_structured()` materializes a normalized rule the
/// emit can't borrow from.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct RuleEmit {
    freq: Frequency,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    by_day: Vec<ByDay>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    by_month_day: Vec<i8>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    by_month: Vec<i8>,
    #[serde(skip_serializing_if = "is_one")]
    interval: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    count: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    until: Option<Until>,
}

// serde's `skip_serializing_if` takes `&T` — by-ref is forced.
#[allow(clippy::trivially_copy_pass_by_ref)]
fn is_opaque(t: &Transparency) -> bool {
    *t == Transparency::Opaque
}

#[allow(clippy::trivially_copy_pass_by_ref)]
fn is_true(v: &bool) -> bool {
    *v
}

#[allow(clippy::trivially_copy_pass_by_ref)]
fn is_one(v: &u32) -> bool {
    *v == 1
}

#[allow(clippy::trivially_copy_pass_by_ref)]
fn is_needs_action(s: &ResponseStatus) -> bool {
    *s == ResponseStatus::NeedsAction
}

/// One event → its emit struct.
fn emit_event(event: &Event) -> EventEmit<'_> {
    let (end, duration) = match &event.body.time {
        EventTime::Span { duration, .. } => (None, Some(format_duration(*duration))),
        EventTime::Range { start, end } => (Some(emit_end(end, start)), None),
    };
    EventEmit {
        id: event.id.as_str(),
        summary: &event.body.summary,
        description: event.body.description.as_deref(),
        location: event.body.location.as_deref(),
        start: emit_start(event),
        end,
        duration,
        recurrence: event.recurrence.as_ref().map(emit_recurrence),
        attendees: event
            .body
            .attendees
            .iter()
            .map(|a| AttendeeEmit {
                email: a.email.as_str(),
                required: a.required,
                status: a.status,
            })
            .collect(),
        reminders: event
            .body
            .reminders
            .iter()
            .map(|r| match r.kind {
                ReminderKind::Popup => ReminderEmit {
                    popup: Some(format_duration(r.offset)),
                    email: None,
                },
                ReminderKind::Email => ReminderEmit {
                    popup: None,
                    email: Some(format_duration(r.offset)),
                },
            })
            .collect(),
        conferencing: event.body.conferencing.as_ref().map(ToString::to_string),
        transparency: event.body.transparency,
    }
}

/// `start` as a DSL scalar: `"HH:MM"` when re-parsing anchors to the
/// stored local exactly (see module docs), else the civil datetime;
/// instants render RFC 3339.
fn emit_start(event: &Event) -> String {
    match event.time.start() {
        Time::Wall { local, .. } => {
            let clock_safe = event
                .recurrence
                .as_ref()
                .and_then(RecurrenceRule::as_structured)
                .and_then(|rule| anchor_datetime(&rule, local.time()))
                .is_some_and(|anchor| anchor == *local);
            if clock_safe {
                format_clock(local.time())
            } else {
                format_local(*local)
            }
        }
        Time::Instant(ts) => ts.to_string(),
    }
}

/// `end` as a DSL scalar: `"HH:MM"` when `resolve_end` would land it
/// on the stored date — same-day (`end >= start` time-of-day) or the
/// overnight roll (next date, earlier time-of-day).
fn emit_end(end: &Time, start: &Time) -> String {
    if let (Time::Wall { local: e, .. }, Time::Wall { local: s, .. }) = (end, start) {
        let same_day = e.date() == s.date() && e.time() >= s.time();
        let overnight = s
            .date()
            .checked_add(jiff::Span::new().days(1))
            .is_ok_and(|next| e.date() == next && e.time() < s.time());
        if same_day || overnight {
            return format_clock(e.time());
        }
        return format_local(*e);
    }
    match end {
        Time::Wall { local, .. } => format_local(*local),
        Time::Instant(ts) => ts.to_string(),
    }
}

/// Canonical recurrence: structured form emits the DSL mapping; raw
/// rules that map onto the model normalize to it; exotic RRULEs
/// keep their string.
fn emit_recurrence(rule: &RecurrenceRule) -> RecurrenceEmit {
    if let Some(rule) = rule.as_structured() {
        RecurrenceEmit::Structured(RuleEmit {
            freq: rule.freq,
            by_day: rule.by_day,
            by_month_day: rule.by_month_day,
            by_month: rule.by_month,
            interval: rule.interval,
            count: rule.count,
            until: rule.until,
        })
    } else {
        // `as_structured` covers every Structured rule — a rule
        // that didn't normalize must be Raw (the else can't fire;
        // it stays inline so no dead arm remains).
        #[rustfmt::skip]
        let RecurrenceRule::Raw(raw) = rule else { unreachable!() };
        RecurrenceEmit::Raw(raw.clone())
    }
}

/// `SignedDuration` → the DSL scalar: largest-unit decomposition
/// (`4h`, `1h30m`, `2d`, `1w`). Sub-second offsets aren't
/// expressible in the DSL — none exist in parsed documents.
fn format_duration(d: SignedDuration) -> String {
    let neg = d.is_negative();
    let mut secs = d.as_secs().unsigned_abs();
    let mut out = String::new();
    if neg {
        out.push('-');
    }
    for (unit, name) in [
        (604_800_u64, 'w'),
        (86_400, 'd'),
        (3_600, 'h'),
        (60, 'm'),
        (1, 's'),
    ] {
        if secs >= unit {
            let _ = write!(out, "{}{name}", secs / unit);
            secs %= unit;
        }
    }
    if out.len() == usize::from(neg) {
        out.push_str("0s");
    }
    out
}

/// `HH:MM[:SS]` — the clock scalar.
fn format_clock(t: Clock) -> String {
    if t.second() == 0 && t.subsec_nanosecond() == 0 {
        t.strftime("%H:%M").to_string()
    } else {
        t.to_string()
    }
}

/// `YYYY-MM-DDTHH:MM[:SS]` — the civil scalar.
fn format_local(local: DateTime) -> String {
    if local.second() == 0 && local.subsec_nanosecond() == 0 {
        local.strftime("%Y-%m-%dT%H:%M").to_string()
    } else {
        local.to_string()
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use cicada_core::domain::ids::ZoneId;
    use jiff::civil::date;

    use super::*;
    use crate::parse_calendar;

    const README: &str = r#"apiVersion: cicada/v1
kind: Calendar
metadata:
  name: work
  timeZone: America/Sao_Paulo
spec:
  events:
    - id: standup
      summary: Daily Standup
      start: "09:00"
      duration: 30m
      recurrence: { freq: WEEKLY, byDay: [MO,TU,WE,TH,FR] }
      attendees:
        - { email: alice@x.com, required: true }
      conferencing: meet
      reminders: [{ popup: 10m }]
"#;

    /// Emit → re-parse → same domain value; the second emit is
    /// byte-identical (canonical = idempotent).
    fn round_trip(source: &str) -> (Calendar, String) {
        let cal = parse_calendar(source).unwrap();
        let once = emit_calendar(&cal, None).unwrap();
        let reparsed = parse_calendar(&once).unwrap();
        assert_eq!(reparsed, cal, "emit must round-trip the domain");
        let twice = emit_calendar(&reparsed, None).unwrap();
        assert_eq!(twice, once, "emit must be idempotent");
        (reparsed, once)
    }

    #[test]
    fn readme_example_keeps_clock_form() {
        let (_, emitted) = round_trip(README);
        assert!(emitted.contains("start: 09:00"), "{emitted}");
        assert!(emitted.contains("duration: 30m"), "{emitted}");
        assert!(emitted.contains("byDay:"), "{emitted}");
        assert!(emitted.contains("conferencing: meet"), "{emitted}");
        assert!(emitted.contains("popup: 10m"), "{emitted}");
        // Defaults elided.
        assert!(!emitted.contains("required: true"), "{emitted}");
        assert!(!emitted.contains("transparency"), "{emitted}");
        assert!(!emitted.contains("interval"), "{emitted}");
    }

    #[test]
    fn dated_start_stays_dated() {
        // A dated recurring start isn't the 1970 anchor — clock form
        // would change semantics, so it must stay dated.
        let yaml = r#"apiVersion: cicada/v1
kind: Calendar
metadata:
  name: work
  timeZone: UTC
spec:
  events:
    - id: weekly
      summary: Weekly
      start: "2026-01-05T09:00"
      duration: 1h
      recurrence: { freq: WEEKLY }
"#;
        let (_, emitted) = round_trip(yaml);
        assert!(emitted.contains("start: 2026-01-05T09:00"), "{emitted}");
    }

    #[test]
    fn clock_end_emits_clock() {
        let yaml = r#"apiVersion: cicada/v1
kind: Calendar
metadata:
  name: work
  timeZone: UTC
spec:
  events:
    - id: party
      summary: Party
      start: "2026-01-05T23:00"
      end: "01:00"
"#;
        let (_, emitted) = round_trip(yaml);
        assert!(emitted.contains("end: 01:00"), "{emitted}");
    }

    #[test]
    fn rrule_fitting_model_emits_structured() {
        let yaml = r#"apiVersion: cicada/v1
kind: Calendar
metadata:
  name: work
  timeZone: UTC
spec:
  events:
    - id: sync
      summary: Sync
      start: "2026-01-05T09:00"
      duration: 30m
      recurrence: "FREQ=WEEKLY;BYDAY=MO"
"#;
        let (_, emitted) = round_trip(yaml);
        assert!(emitted.contains("freq: WEEKLY"), "{emitted}");
        assert!(!emitted.contains("FREQ="), "{emitted}");
    }

    #[test]
    fn exotic_rrule_stays_raw() {
        let yaml = r#"apiVersion: cicada/v1
kind: Calendar
metadata:
  name: work
  timeZone: UTC
spec:
  events:
    - id: exotic
      summary: Exotic
      start: "2026-01-05T09:00"
      duration: 30m
      recurrence: "FREQ=MONTHLY;BYDAY=MO,TU,WE,TH,FR;BYSETPOS=-1"
"#;
        let (_, emitted) = round_trip(yaml);
        assert!(emitted.contains("BYSETPOS"), "{emitted}");
    }

    #[test]
    fn nondefault_apply_block_survives() {
        let yaml = r"apiVersion: cicada/v1
kind: Calendar
metadata:
  name: work
  timeZone: UTC
spec:
  apply: { sendUpdates: none }
  events: []
";
        let (_, emitted) = round_trip(yaml);
        assert!(emitted.contains("sendUpdates: none"), "{emitted}");
    }

    #[test]
    fn emit_end_renders_overnight_and_instant() {
        let zone = ZoneId::new_unchecked("UTC");
        let start = Time::Wall {
            local: date(2026, 1, 5).at(9, 0, 0, 0),
            zone: zone.clone(),
        };
        // Same-day end → clock only.
        let end = Time::Wall {
            local: date(2026, 1, 5).at(10, 0, 0, 0),
            zone: zone.clone(),
        };
        assert_eq!(emit_end(&end, &start), "10:00");
        // A different-day wall end renders fully (not the overnight
        // HH:MM shorthand — that's only for the *next* day).
        let end = Time::Wall {
            local: date(2026, 1, 8).at(10, 0, 0, 0),
            zone: zone.clone(),
        };
        let rendered = emit_end(&end, &start);
        assert!(rendered.contains("01-08"), "{rendered}");
        // An instant end renders its timestamp.
        let end = Time::Instant("2026-01-05T12:00:00Z".parse().unwrap());
        let rendered = emit_end(&end, &start);
        assert!(rendered.contains('T'), "{rendered}");
        // Mixed forms — a wall end against an instant start renders
        // fully (no clock shorthand without matching wall starts).
        let start = Time::Instant("2026-01-05T09:00:00Z".parse().unwrap());
        let end = Time::Wall {
            local: date(2026, 1, 5).at(10, 0, 0, 0),
            zone,
        };
        let rendered = emit_end(&end, &start);
        assert!(rendered.contains("2026-01-05"), "{rendered}");
    }

    #[test]
    fn format_duration_signs_and_zero() {
        assert_eq!(format_duration(SignedDuration::from_mins(90)), "1h30m");
        assert_eq!(format_duration(SignedDuration::from_mins(-45)), "-45m");
        assert_eq!(format_duration(SignedDuration::from_secs(45)), "45s");
        assert_eq!(format_duration(SignedDuration::ZERO), "0s");
        assert_eq!(format_duration(SignedDuration::from_hours(2)), "2h");
        assert_eq!(format_duration(SignedDuration::from_hours(26)), "1d2h");
    }

    #[test]
    fn format_clock_and_local_render_seconds() {
        let t: Clock = "09:30:45".parse().unwrap();
        assert_eq!(format_clock(t), "09:30:45");
        let local: DateTime = date(2026, 1, 5).at(9, 30, 45, 0);
        let rendered = format_local(local);
        assert!(rendered.contains(":45"), "{rendered}");
        let t2: Clock = "09:30:00".parse().unwrap();
        assert_eq!(format_clock(t2), "09:30");
    }
}
