//! Golden: the README example parsed → `plan` vs empty state → the
//! full `cicada/plan-v1` JSON document, compared against the
//! committed fixture. Frozen output — regenerate deliberately with
//! `CICADA_BLESS=1` when the wire format legitimately changes.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::PathBuf;

use cicada_core::application::{PlanCalendar, PlanRequest};
use cicada_core::domain::policy::SendUpdates;
use cicada_core::ports::CalendarRef;
use cicada_schema::parse_calendar;
use cicada_testkit::{FakeCalendarProvider, FakeClock};

/// The README example verbatim (`README.md` YAML block).
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

fn fixture_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("fixtures")
        .join(name)
}

/// `CICADA_BLESS=1` rewrites the fixture with current output.
fn assert_golden(path: &std::path::Path, actual: &str) {
    if std::env::var_os("CICADA_BLESS").is_some() {
        std::fs::write(path, actual).expect("bless fixture");
    }
    let expected = std::fs::read_to_string(path).unwrap_or_else(|e| {
        panic!(
            "cannot read golden {}: {e} — create it with CICADA_BLESS=1",
            path.display()
        )
    });
    assert_eq!(actual, expected, "golden {} drifted", path.display());
}

#[tokio::test]
async fn readme_example_plan_is_golden() {
    let cal = parse_calendar(README).expect("README example parses");
    let provider = FakeCalendarProvider::new();
    let clock = FakeClock::at("2026-10-23T01:00:00Z".parse().unwrap());
    let remote = CalendarRef::new("primary");
    let plan = PlanCalendar::new()
        .run(&PlanRequest {
            desired: &cal,
            recorded: None,
            provider: &provider,
            remote_calendar: &remote,
            clock: &clock,
            send_updates: SendUpdates::default(),
        })
        .await
        .expect("plan");
    let json = format!("{}\n", serde_json::to_string_pretty(&plan).unwrap());
    assert_golden(&fixture_path("readme-plan.golden.json"), &json);
}
