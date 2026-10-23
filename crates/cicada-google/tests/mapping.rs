//! Golden-fixture mapping tests: Google event JSON ↔ `EventBody`
//! per `event_json.rs`'s documented decisions.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use cicada_core::domain::attendee::ResponseStatus;
use cicada_core::domain::event::{Conferencing, EventTime, Transparency};
use cicada_core::domain::recurrence::RecurrenceRule;
use cicada_core::domain::reminder::ReminderKind;
use cicada_core::domain::time::Time;
use cicada_google::event_json::{self, WireEvent};

const EVENT_FULL: &str = include_str!("fixtures/event_full.json");
const EVENT_MINIMAL: &str = include_str!("fixtures/event_minimal.json");

#[test]
fn full_fixture_maps_every_field() {
    let wire: WireEvent = serde_json::from_str(EVENT_FULL).unwrap();
    let remote = event_json::from_wire(&wire).unwrap();

    assert_eq!(remote.remote_id.as_str(), "g-evt-standup");
    assert_eq!(remote.etag.as_deref(), Some("\"3385852702942000\""));
    assert_eq!(
        remote.updated.unwrap().to_string(),
        "2026-10-20T11:12:13.442Z"
    );

    // Tags (ADR-0002 §2); the foreign key isn't read.
    let tags = &remote.tags;
    assert_eq!(tags.local_id.as_ref().unwrap().as_str(), "standup");
    assert!(tags.managed);
    assert_eq!(
        tags.state_hash.as_ref().unwrap().as_str(),
        "sha256:1111111111111111111111111111111111111111111111111111111111111111"
    );

    let body = &remote.body;
    assert_eq!(body.summary, "Daily Standup");
    assert_eq!(
        body.description.as_deref(),
        Some("Fifteen minutes, cameras optional.")
    );
    assert_eq!(body.location.as_deref(), Some("Room 4 / Meet"));

    // Wall time + zone preserved (ADR-0002 §6) — read back as Span.
    let EventTime::Span { start, duration } = &body.time else {
        panic!("expected span, got {:?}", body.time);
    };
    let Time::Wall { local, zone } = start else {
        panic!("expected wall start");
    };
    assert_eq!(local.to_string(), "2026-10-20T09:00:00");
    assert_eq!(zone.as_str(), "America/Sao_Paulo");
    assert_eq!(*duration, jiff::SignedDuration::from_mins(30));

    // RRULE comes back raw and compares semantically — day order
    // normalizes away.
    let Some(rule) = &body.recurrence else {
        panic!("no recurrence")
    };
    assert_eq!(
        *rule,
        RecurrenceRule::Raw("RRULE:FREQ=WEEKLY;BYDAY=MO,TU,WE,TH,FR".into())
    );

    assert_eq!(body.attendees.len(), 2);
    assert_eq!(body.attendees[0].email, "alice@example.com");
    assert!(body.attendees[0].required);
    assert_eq!(body.attendees[0].status, ResponseStatus::Accepted);
    assert_eq!(body.attendees[1].email, "bob@example.com");
    assert!(!body.attendees[1].required);
    assert_eq!(body.attendees[1].status, ResponseStatus::Tentative);

    assert_eq!(body.reminders.len(), 2);
    assert_eq!(body.reminders[0].kind, ReminderKind::Popup);
    assert_eq!(
        body.reminders[0].offset,
        jiff::SignedDuration::from_mins(10)
    );
    assert_eq!(body.reminders[1].kind, ReminderKind::Email);

    assert_eq!(body.conferencing, Some(Conferencing::Meet));
    assert_eq!(body.transparency, Transparency::Transparent);
}

#[test]
fn minimal_fixture_reads_as_instants_without_tags() {
    let wire: WireEvent = serde_json::from_str(EVENT_MINIMAL).unwrap();
    let remote = event_json::from_wire(&wire).unwrap();

    assert_eq!(remote.remote_id.as_str(), "g-evt-lunch");
    assert_eq!(remote.tags.local_id, None);
    assert!(!remote.tags.managed);
    let EventTime::Span { start, .. } = &remote.body.time else {
        panic!("expected span");
    };
    // No timeZone → absolute instant.
    let Time::Instant(ts) = start else {
        panic!("expected instant")
    };
    assert_eq!(ts.to_string(), "2026-10-21T12:00:00Z");
}

#[test]
fn write_shape_is_google_json() {
    use cicada_core::domain::event::EventBody;
    use jiff::civil::date;
    use std::collections::BTreeMap;

    let body = EventBody {
        summary: "Standup".into(),
        description: None,
        location: Some("Meet".into()),
        time: EventTime::Span {
            start: Time::wall(date(2026, 10, 20).at(9, 0, 0, 0), "America/Sao_Paulo").unwrap(),
            duration: jiff::SignedDuration::from_mins(30),
        },
        recurrence: Some(RecurrenceRule::Raw("FREQ=DAILY;COUNT=5".into())),
        attendees: vec![cicada_core::domain::Attendee::new("a@x.com")],
        reminders: vec![cicada_core::domain::Reminder::popup(
            jiff::SignedDuration::from_mins(10),
        )],
        conferencing: Some(Conferencing::Meet),
        transparency: Transparency::Transparent,
    };
    let tags = cicada_core::ports::EventTags {
        local_id: Some("standup".into()),
        managed: true,
        state_hash: Some("sha256:abc".into()),
    };
    let wire = event_json::to_wire(&body, &tags, &BTreeMap::new(), Some("req-42")).unwrap();
    let json = serde_json::to_value(&wire).unwrap();

    assert_eq!(json["summary"], "Standup");
    // None fields serialize as explicit null so PATCH clears them.
    assert!(json["description"].is_null());
    assert_eq!(json["start"]["timeZone"], "America/Sao_Paulo");
    assert_eq!(json["start"]["dateTime"], "2026-10-20T09:00:00-03:00");
    assert_eq!(json["end"]["dateTime"], "2026-10-20T09:30:00-03:00");
    assert_eq!(
        json["recurrence"],
        serde_json::json!(["RRULE:FREQ=DAILY;COUNT=5"])
    );
    assert_eq!(json["attendees"][0]["email"], "a@x.com");
    assert_eq!(json["attendees"][0]["optional"], false);
    assert_eq!(json["reminders"]["useDefault"], false);
    assert_eq!(json["reminders"]["overrides"][0]["method"], "popup");
    assert_eq!(json["reminders"]["overrides"][0]["minutes"], 10);
    assert_eq!(json["transparency"], "transparent");
    assert_eq!(
        json["conferenceData"]["createRequest"]["conferenceSolutionKey"]["type"],
        "hangoutsMeet"
    );
    assert_eq!(
        json["conferenceData"]["createRequest"]["requestId"],
        "req-42"
    );
    let private = &json["extendedProperties"]["private"];
    assert_eq!(private["cicada:id"], "standup");
    assert_eq!(private["cicada:managed"], "1");
    assert_eq!(private["cicada:state"], "sha256:abc");
    // responseStatus is never written — RSVP state is server-owned.
    assert!(json["attendees"][0].get("responseStatus").is_none());
}

#[test]
fn empty_reminders_write_as_use_default() {
    use cicada_core::domain::event::EventBody;
    use std::collections::BTreeMap;

    let body = EventBody {
        summary: "x".into(),
        description: None,
        location: None,
        time: EventTime::Span {
            start: Time::Instant("2026-10-20T12:00:00Z".parse().unwrap()),
            duration: jiff::SignedDuration::from_mins(10),
        },
        recurrence: None,
        attendees: vec![],
        reminders: vec![],
        conferencing: None,
        transparency: Transparency::Opaque,
    };
    let wire = event_json::to_wire(
        &body,
        &cicada_core::ports::EventTags::default(),
        &BTreeMap::new(),
        None,
    )
    .unwrap();
    let json = serde_json::to_value(&wire).unwrap();
    assert_eq!(json["reminders"]["useDefault"], true);
    // Absent attendees means "clear" on PATCH.
    assert_eq!(json["attendees"], serde_json::json!([]));
}

#[test]
fn declared_range_reads_back_as_span() {
    // Documented limitation: `start + end` declares `Range`; Google
    // stores start/end, so read-back is the equivalent `Span`.
    use cicada_core::domain::event::EventBody;
    use cicada_core::domain::ids::ZoneId;
    use jiff::civil::date;
    use std::collections::BTreeMap;

    let start = Time::Wall {
        local: date(2026, 10, 20).at(9, 0, 0, 0),
        zone: ZoneId::new_unchecked("UTC"),
    };
    let end = Time::Wall {
        local: date(2026, 10, 20).at(9, 30, 0, 0),
        zone: ZoneId::new_unchecked("UTC"),
    };
    let body = EventBody {
        summary: "x".into(),
        description: None,
        location: None,
        time: EventTime::Range { start, end },
        recurrence: None,
        attendees: vec![],
        reminders: vec![],
        conferencing: None,
        transparency: Transparency::Opaque,
    };
    let mut wire = event_json::to_wire(
        &body,
        &cicada_core::ports::EventTags::default(),
        &BTreeMap::new(),
        None,
    )
    .unwrap();
    wire.id = Some("evt".into());
    let remote = event_json::from_wire(&wire).unwrap();
    assert!(matches!(remote.body.time, EventTime::Span { .. }));
    // The span is the same, so `to_timestamp` agrees — only the
    // enum shape (and therefore hash text) differs.
    assert_eq!(
        remote.body.time.start().to_timestamp().unwrap(),
        body.time.start().to_timestamp().unwrap()
    );
    assert_eq!(
        remote.body.time.duration().unwrap(),
        body.time.duration().unwrap()
    );
}
