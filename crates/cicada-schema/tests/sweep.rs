//! Coverage sweep — schema error paths the happy-path tests miss.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use cicada_schema::{
    Document, SchemaError, emit_calendar, parse_calendar, parse_document, parse_manifest,
};

const CAL_HEAD: &str =
    "apiVersion: cicada/v1\nkind: Calendar\nmetadata:\n  name: work\n  timeZone: UTC\nspec:\n";

#[test]
fn kind_mismatches_report_the_expected_kind() {
    let manifest = "apiVersion: cicada/v1\nkind: Manifest\ncalendars: []\n";
    let err = parse_calendar(manifest).unwrap_err();
    assert!(err.to_string().contains("kind: Calendar"), "{err}");
    let cal = format!("{CAL_HEAD}  events: []\n");
    let err = parse_manifest(&cal).unwrap_err();
    assert!(err.to_string().contains("kind: Manifest"), "{err}");
}

#[test]
fn document_level_failures() {
    for source in [
        "",
        "   \n# nothing\n",
        "apiVersion: cicada/v1\nkind: 42\n",
        "---\nkind: x\n---\nkind: y\n",
        // First doc parses — the second document is what errors.
        "apiVersion: cicada/v1\nkind: Manifest\ncalendars: []\n---\nkind: x\n",
        // kind dispatches to ManifestDoc, which then fails to
        // deserialize — the `?` inside the Manifest arm.
        "apiVersion: cicada/v1\nkind: Manifest\ncalendars: bogus\n",
    ] {
        let err = parse_document(source).unwrap_err();
        assert!(!err.to_string().is_empty(), "{source:?}");
    }
    // Multi-doc is a document-level error, not a field path.
    let err = parse_document("apiVersion: cicada/v1\nkind: Manifest\ncalendars: []\n---\nx: 1\n")
        .unwrap_err();
    assert!(err.to_string().contains("multi-document"), "{err}");
    // Document enum exists.
    if let Ok(d) = parse_document("apiVersion: cicada/v1\nkind: Manifest\ncalendars: []\n") {
        let _ = format!("{}", matches!(d, Document::Manifest(_)));
    }
}

#[test]
fn non_string_scalars_describe_their_shape() {
    for (field, value) in [
        ("duration", "30"),
        ("duration", "true"),
        ("duration", "[x]"),
        ("duration", "{a: 1}"),
        ("start", "5"),
        ("start", "[x]"),
        ("start", "{a: 1}"),
        ("start", "true"),
    ] {
        let doc = format!(
            "{CAL_HEAD}  events:\n    - id: e\n      summary: s\n      start: \"2026-01-05T09:00\"\n      {field}: {value}\n"
        );
        let err = parse_calendar(&doc).unwrap_err();
        assert!(!err.to_string().is_empty(), "{field}: {value}");
    }
}

#[test]
fn end_and_duration_are_exclusive_and_required() {
    let base = format!("{CAL_HEAD}  events:\n");
    let both = format!(
        "{base}    - id: e\n      summary: s\n      start: \"2026-01-05T09:00\"\n      end: \"2026-01-05T10:00\"\n      duration: 30m\n"
    );
    assert!(
        parse_calendar(&both)
            .unwrap_err()
            .to_string()
            .contains("not both")
    );
    let neither =
        format!("{base}    - id: e\n      summary: s\n      start: \"2026-01-05T09:00\"\n");
    assert!(
        parse_calendar(&neither)
            .unwrap_err()
            .to_string()
            .contains("end` or `duration")
    );
}

#[test]
fn bare_clock_start_needs_a_supported_rule() {
    let head = format!("{CAL_HEAD}  events:\n");
    // No recurrence at all.
    let doc = format!(
        "{head}    - id: e\n      summary: s\n      start: \"09:00\"\n      duration: 30m\n"
    );
    let err = parse_calendar(&doc).unwrap_err();
    assert!(err.to_string().contains("recurrence"), "{err}");
    // An exotic RRULE can't anchor.
    let doc = format!(
        "{head}    - id: e\n      summary: s\n      start: \"09:00\"\n      duration: 30m\n      recurrence: \"FREQ=MONTHLY;BYSETPOS=-1\"\n"
    );
    let err = parse_calendar(&doc).unwrap_err();
    assert!(err.to_string().contains("exotic"), "{err}");
    // A rule matching nothing can't anchor either.
    let doc = format!(
        "{head}    - id: e\n      summary: s\n      start: \"09:00\"\n      duration: 30m\n      recurrence: {{ freq: YEARLY, byMonth: [2], byMonthDay: [30] }}\n"
    );
    let err = parse_calendar(&doc).unwrap_err();
    assert!(err.to_string().contains("anchor"), "{err}");
}

#[test]
fn clock_end_resolution_covers_same_day_overnight_and_instant() {
    let head = format!("{CAL_HEAD}  events:\n");
    // Overnight: end clock < start clock rolls a day.
    let doc = format!(
        "{head}    - id: e\n      summary: s\n      start: \"2026-01-05T23:00\"\n      end: \"01:00\"\n"
    );
    let cal = parse_calendar(&doc).unwrap();
    let ev = cal.events.values().next().unwrap();
    let cicada_core::domain::event::EventTime::Range { end, .. } = &ev.body.time else {
        panic!("expected Range")
    };
    let cicada_core::domain::time::Time::Wall { local, .. } = end else {
        panic!("expected wall end")
    };
    assert!(local.to_string().contains("2026-01-06T01:00"));
    // Same-day clock end.
    let doc = format!(
        "{head}    - id: e\n      summary: s\n      start: \"2026-01-05T09:00\"\n      end: \"10:00\"\n"
    );
    assert!(parse_calendar(&doc).is_ok());
    // Instant end stays an instant.
    let doc = format!(
        "{head}    - id: e\n      summary: s\n      start: \"2026-01-05T09:00\"\n      end: \"2026-01-05T10:00:00Z\"\n"
    );
    assert!(parse_calendar(&doc).is_ok());
    // A clock end against an instant start is refused.
    let doc = format!(
        "{head}    - id: e\n      summary: s\n      start: \"2026-01-05T09:00:00Z\"\n      end: \"10:00\"\n"
    );
    assert!(
        parse_calendar(&doc)
            .unwrap_err()
            .to_string()
            .contains("local `start`")
    );
    // A bare date start works; so does a bare date end.
    let doc = format!(
        "{head}    - id: e\n      summary: s\n      start: \"2026-01-05\"\n      end: \"2026-01-06\"\n"
    );
    assert!(parse_calendar(&doc).is_ok());
}

#[test]
fn reminder_needs_exactly_one_kind() {
    let doc = format!(
        "{CAL_HEAD}  events:\n    - id: e\n      summary: s\n      start: \"2026-01-05T09:00\"\n      duration: 30m\n      reminders: [{{ popup: 10m, email: 1h }}]\n"
    );
    assert!(
        parse_calendar(&doc)
            .unwrap_err()
            .to_string()
            .contains("exactly one")
    );
    let doc = format!(
        "{CAL_HEAD}  events:\n    - id: e\n      summary: s\n      start: \"2026-01-05T09:00\"\n      duration: 30m\n      reminders: [{{}}]\n"
    );
    assert!(parse_calendar(&doc).is_err());
}

#[test]
fn emit_round_trips_rich_events() {
    let doc = format!(
        "{CAL_HEAD}  apply:\n    sendUpdates: all\n  events:\n    - id: a\n      summary: Standup\n      description: daily\n      location: room\n      start: \"09:00\"\n      duration: 30m\n      recurrence: {{ freq: WEEKLY, byDay: [MO,WE] }}\n      attendees:\n        - {{ email: a@x, required: true }}\n        - {{ email: b@x, required: false }}\n      reminders: [{{ popup: 10m }}, {{ email: 1h }}]\n      conferencing: meet\n      transparency: transparent\n    - id: b\n      summary: Range\n      start: \"2026-01-05T09:00\"\n      end: \"2026-01-05T10:00\"\n      recurrence: \"FREQ=DAILY;COUNT=5\"\n    - id: c\n      summary: Instant\n      start: \"2026-01-05T09:00:00Z\"\n      duration: 1h\n    - id: d\n      summary: Other\n      start: \"2026-01-05T09:00\"\n      duration: 1h\n      conferencing: zoom\n"
    );
    let cal = parse_calendar(&doc).unwrap();
    let emitted = emit_calendar(&cal, None).unwrap();
    // Round-trip: the emitted form parses to an equal calendar.
    let back = parse_calendar(&emitted).unwrap();
    assert_eq!(back.events.len(), cal.events.len());
    // Emitting again is stable.
    let again = emit_calendar(&back, None).unwrap();
    assert_eq!(emitted, again);
    // A partial order keeps listed events first.
    let order = vec!["d".into(), "b".into()];
    let reordered = emit_calendar(&cal, Some(&order)).unwrap();
    let pos_d = reordered.find("id: d").unwrap();
    let pos_b = reordered.find("id: b").unwrap();
    let pos_a = reordered.find("id: a").unwrap();
    assert!(pos_d < pos_b && pos_b < pos_a);
}

#[test]
fn schema_error_display_shapes() {
    let e = SchemaError::at("spec.x", "bad").with_file("/tmp/f.yaml");
    assert!(e.to_string().contains("/tmp/f.yaml"), "{e}");
    assert!(e.to_string().contains("spec.x"), "{e}");
    let e = SchemaError::document("boom");
    assert_eq!(e.to_string(), "boom");
    // A syntax error renders line:col.
    let e = parse_calendar("apiVersion: [\n").unwrap_err();
    let s = e.to_string();
    assert!(s.contains(':'), "{s}");
    // Error trait object works.
    let _: &dyn std::error::Error = &e;
}
