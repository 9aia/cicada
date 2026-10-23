//! DSL → domain: the README example, manifest parsing, and every
//! error class the schema layer owns.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::needless_raw_string_hashes
)]

use cicada_core::domain::event::{Conferencing, EventTime};
use cicada_core::domain::recurrence::{Frequency, RecurrenceRule};
use cicada_core::domain::reminder::ReminderKind;
use cicada_core::domain::time::Time;
use cicada_schema::{Document, parse_calendar, parse_document, parse_manifest};

/// The example from README.md — this document must always parse.
const README: &str = r#"
apiVersion: cicada/v1
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

#[test]
fn readme_example_parses() {
    let cal = parse_calendar(README).unwrap();
    assert_eq!(cal.name, "work");
    assert_eq!(cal.time_zone.as_str(), "America/Sao_Paulo");
    assert_eq!(cal.len(), 1);

    let e = &cal.get(&"standup".into()).unwrap();
    assert_eq!(e.summary, "Daily Standup");
    // Bare clock start anchored at the epoch (1970-01-01, Thursday —
    // inside MO..FR), in the calendar zone.
    let EventTime::Span { start, duration } = &e.time else {
        panic!("expected start+duration");
    };
    assert_eq!(duration.as_secs(), 30 * 60);
    let Time::Wall { local, zone } = start else {
        panic!("expected wall start");
    };
    assert_eq!(local.to_string(), "1970-01-01T09:00:00");
    assert_eq!(zone.as_str(), "America/Sao_Paulo");

    let Some(RecurrenceRule::Structured(rule)) = &e.recurrence else {
        panic!("expected structured recurrence");
    };
    assert_eq!(rule.freq, Frequency::Weekly);
    assert_eq!(rule.by_day.len(), 5);

    assert_eq!(e.attendees.len(), 1);
    assert_eq!(e.attendees[0].email, "alice@x.com");
    assert!(e.attendees[0].required);
    assert_eq!(e.conferencing, Some(Conferencing::Meet));
    assert_eq!(e.reminders.len(), 1);
    assert_eq!(e.reminders[0].kind, ReminderKind::Popup);
    assert_eq!(e.reminders[0].offset.as_secs(), 600);
}

#[test]
fn manifest_parses_calendar_paths() {
    let doc = parse_document(
        r#"
apiVersion: cicada/v1
kind: Manifest
calendars: [calendars/work.yaml, calendars/personal.yaml]
"#,
    )
    .unwrap();
    let Document::Manifest(m) = doc else {
        panic!("expected manifest");
    };
    assert_eq!(m.calendars.len(), 2);
    assert_eq!(m.calendars[0].to_str().unwrap(), "calendars/work.yaml");

    let m = parse_manifest("apiVersion: cicada/v1\nkind: Manifest\ncalendars: [a.yaml]\n").unwrap();
    assert_eq!(m.calendars.len(), 1);
}

#[test]
fn full_datetime_forms() {
    let cal = parse_calendar(
        r#"
apiVersion: cicada/v1
kind: Calendar
metadata: { name: p, timeZone: Europe/Lisbon, provider: google, calendarId: primary }
spec:
  events:
    - id: dated
      summary: Dated wall event
      start: "2026-08-19T09:00"
      end: "2026-08-19T10:30"
    - id: instant
      summary: Absolute event
      start: "2026-08-19T09:00:00Z"
      duration: 45m
    - id: overnight
      summary: Late one
      start: "2026-08-19T23:00"
      end: "01:00"
"#,
    )
    .unwrap();
    assert_eq!(cal.provider.as_deref(), Some("google"));
    assert_eq!(cal.provider_calendar_id.as_deref(), Some("primary"));

    let dated = &cal.get(&"dated".into()).unwrap();
    let EventTime::Range { start, end } = &dated.time else {
        panic!("expected start+end");
    };
    let Time::Wall { local, .. } = start else {
        panic!("wall");
    };
    assert_eq!(local.to_string(), "2026-08-19T09:00:00");
    let Time::Wall { local, .. } = end else {
        panic!("wall");
    };
    assert_eq!(local.to_string(), "2026-08-19T10:30:00");

    let instant = &cal.get(&"instant".into()).unwrap();
    let EventTime::Span { start, .. } = &instant.time else {
        panic!("span");
    };
    assert!(matches!(start, Time::Instant(_)));

    // Overnight: bare-clock end rolls into the next day.
    let on = &cal.get(&"overnight".into()).unwrap();
    let EventTime::Range { end, .. } = &on.time else {
        panic!("range");
    };
    let Time::Wall { local, .. } = end else {
        panic!("wall");
    };
    assert_eq!(local.to_string(), "2026-08-20T01:00:00");
}

#[test]
fn recurrence_forms() {
    let cal = parse_calendar(
        r#"
apiVersion: cicada/v1
kind: Calendar
metadata: { name: w, timeZone: UTC }
spec:
  events:
    - id: a
      summary: A
      start: "09:00"
      duration: 30m
      recurrence: { freq: MONTHLY, byDay: [-1FR], count: 12 }
    - id: b
      summary: B
      start: "2026-08-19T09:00"
      duration: 30m
      recurrence: { rrule: "FREQ=YEARLY;BYMONTH=8;BYMONTHDAY=19" }
    - id: c
      summary: C
      start: "2026-08-19T09:00"
      duration: 30m
      recurrence: "FREQ=DAILY;COUNT=5"
    - id: d
      summary: D
      start: "2026-08-19T09:00"
      duration: 30m
      recurrence: { freq: WEEKLY, until: 2027-01-01 }
"#,
    )
    .unwrap();
    assert_eq!(cal.len(), 4);
    let Some(RecurrenceRule::Structured(r)) = &cal.get(&"a".into()).unwrap().recurrence else {
        panic!("structured");
    };
    assert_eq!(r.count, Some(12));
    let Some(RecurrenceRule::Raw(_)) = &cal.get(&"b".into()).unwrap().recurrence else {
        panic!("raw via rrule key");
    };
    let Some(RecurrenceRule::Raw(_)) = &cal.get(&"c".into()).unwrap().recurrence else {
        panic!("raw via bare string");
    };
    let Some(RecurrenceRule::Structured(r)) = &cal.get(&"d".into()).unwrap().recurrence else {
        panic!("structured");
    };
    assert!(r.until.is_some());
}

#[test]
fn error_paths_point_at_fields() {
    // Missing required `id`.
    let err = parse_calendar(
        r#"
apiVersion: cicada/v1
kind: Calendar
metadata: { name: w, timeZone: UTC }
spec:
  events:
    - summary: No id
      start: "2026-08-19T09:00"
      duration: 30m
"#,
    )
    .unwrap_err();
    // Missing-field errors point at the enclosing entry.
    assert_eq!(err.path.as_deref(), Some("spec.events[0]"));
    assert!(err.message.contains("missing field `id`"));

    // Bad scalar: duration must carry a unit.
    let err = parse_calendar(
        r#"
apiVersion: cicada/v1
kind: Calendar
metadata: { name: w, timeZone: UTC }
spec:
  events:
    - { id: x, summary: X, start: "2026-08-19T09:00", duration: "30" }
"#,
    )
    .unwrap_err();
    assert_eq!(err.path.as_deref(), Some("spec.events[0].duration"));
    assert!(err.message.contains("invalid duration"));

    // Unknown field (typo guard).
    let err = parse_calendar(
        r#"
apiVersion: cicada/v1
kind: Calendar
metadata: { name: w, timeZone: UTC }
spec:
  events:
    - { id: x, summery: X, start: "2026-08-19T09:00", duration: 30m }
"#,
    )
    .unwrap_err();
    assert_eq!(
        err.path.as_deref(),
        Some("spec.events[0].summery"),
        "unexpected: {err}"
    );
    assert!(err.message.contains("unknown field"));
}

#[test]
fn envelope_errors() {
    // Unsupported version.
    let err = parse_document("apiVersion: cicada/v2\nkind: Calendar\n").unwrap_err();
    assert!(err.message.contains("unsupported apiVersion"));

    // Unknown kind.
    let err = parse_document("apiVersion: cicada/v1\nkind: Widget\n").unwrap_err();
    assert!(err.message.contains("Widget"));

    // Missing kind.
    let err = parse_document("apiVersion: cicada/v1\n").unwrap_err();
    assert!(err.message.contains("kind"));

    // Not YAML at all — scanner position rides in the message.
    let err = parse_document("{{{{").unwrap_err();
    assert!(err.message.contains("line 1"), "{err}");

    // Empty.
    assert!(parse_document("").is_err());
    // Wrong kind for a typed parse.
    let err = parse_calendar("apiVersion: cicada/v1\nkind: Manifest\n").unwrap_err();
    assert!(err.message.contains("expected kind: Calendar"));
}

#[test]
fn domain_refusals_surface_with_paths() {
    // Unknown IANA zone.
    let err = parse_calendar(
        r#"
apiVersion: cicada/v1
kind: Calendar
metadata: { name: w, timeZone: Not/AZone }
spec: { events: [] }
"#,
    )
    .unwrap_err();
    assert_eq!(err.path.as_deref(), Some("metadata.timeZone"));

    // Duplicate ids.
    let err = parse_calendar(
        r#"
apiVersion: cicada/v1
kind: Calendar
metadata: { name: w, timeZone: UTC }
spec:
  events:
    - { id: dup, summary: A, start: "2026-08-19T09:00", duration: 30m }
    - { id: dup, summary: B, start: "2026-08-19T10:00", duration: 30m }
"#,
    )
    .unwrap_err();
    assert_eq!(err.path.as_deref(), Some("spec.events[1].id"));
    assert!(err.message.contains("duplicate"));

    // Bare clock without recurrence.
    let err = parse_calendar(
        r#"
apiVersion: cicada/v1
kind: Calendar
metadata: { name: w, timeZone: UTC }
spec:
  events:
    - { id: x, summary: X, start: "09:00", duration: 30m }
"#,
    )
    .unwrap_err();
    assert_eq!(err.path.as_deref(), Some("spec.events[0].start"));
    assert!(err.message.contains("recurrence"));

    // Both end and duration.
    let err = parse_calendar(
        r#"
apiVersion: cicada/v1
kind: Calendar
metadata: { name: w, timeZone: UTC }
spec:
  events:
    - { id: x, summary: X, start: "2026-08-19T09:00", end: "2026-08-19T10:00", duration: 30m }
"#,
    )
    .unwrap_err();
    assert!(err.message.contains("not both"));

    // Neither end nor duration.
    let err = parse_calendar(
        r#"
apiVersion: cicada/v1
kind: Calendar
metadata: { name: w, timeZone: UTC }
spec:
  events:
    - { id: x, summary: X, start: "2026-08-19T09:00" }
"#,
    )
    .unwrap_err();
    assert!(err.message.contains("`end` or `duration`"));

    // Reminder with both keys.
    let err = parse_calendar(
        r#"
apiVersion: cicada/v1
kind: Calendar
metadata: { name: w, timeZone: UTC }
spec:
  events:
    - id: x
      summary: X
      start: "2026-08-19T09:00"
      duration: 30m
      reminders: [{ popup: 10m, email: 1h }]
"#,
    )
    .unwrap_err();
    assert_eq!(err.path.as_deref(), Some("spec.events[0].reminders[0]"));
    assert!(err.message.contains("exactly one"));
}

#[test]
fn schema_error_maps_to_fatal() {
    let err = parse_document("bogus").unwrap_err();
    let core: cicada_core::CicadaError = err.into();
    assert_eq!(core.kind(), cicada_core::ErrorKind::Fatal);
}

#[test]
fn durations_everywhere() {
    let cal = parse_calendar(
        r#"
apiVersion: cicada/v1
kind: Calendar
metadata: { name: w, timeZone: UTC }
spec:
  events:
    - id: a
      summary: A
      start: "2026-08-19T09:00"
      duration: 1h30m
      reminders: [{ popup: 10m }, { email: "1h" }]
"#,
    )
    .unwrap();
    let e = &cal.get(&"a".into()).unwrap();
    let EventTime::Span { duration, .. } = &e.time else {
        panic!("span");
    };
    assert_eq!(duration.as_secs(), 90 * 60);
    assert_eq!(e.reminders.len(), 2);
    assert_eq!(e.reminders[1].kind, ReminderKind::Email);
    assert_eq!(e.reminders[1].offset.as_secs(), 3600);
}

#[test]
fn spec_apply_send_updates_parses_and_defaults() {
    // Default is externalOnly (ADR-0002 §5).
    let cal = parse_calendar(
        r#"
apiVersion: cicada/v1
kind: Calendar
metadata: { name: w, timeZone: UTC }
spec:
  events: []
"#,
    )
    .unwrap();
    assert_eq!(
        cal.send_updates,
        cicada_core::domain::policy::SendUpdates::ExternalOnly
    );

    let cal = parse_calendar(
        r#"
apiVersion: cicada/v1
kind: Calendar
metadata: { name: w, timeZone: UTC }
spec:
  apply:
    sendUpdates: none
  events: []
"#,
    )
    .unwrap();
    assert_eq!(
        cal.send_updates,
        cicada_core::domain::policy::SendUpdates::None
    );

    // Unknown values and unknown apply keys fail at their paths.
    let err = parse_calendar(
        r#"
apiVersion: cicada/v1
kind: Calendar
metadata: { name: w, timeZone: UTC }
spec:
  apply:
    sendUpdates: everybody
  events: []
"#,
    )
    .unwrap_err();
    assert!(
        err.path.as_deref().unwrap_or("").contains("sendUpdates"),
        "{err:?}"
    );
    let err = parse_calendar(
        r#"
apiVersion: cicada/v1
kind: Calendar
metadata: { name: w, timeZone: UTC }
spec:
  apply:
    bogus: true
  events: []
"#,
    )
    .unwrap_err();
    assert!(err.message.contains("bogus"), "{err:?}");
}
