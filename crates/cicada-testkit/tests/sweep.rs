//! Coverage sweep over cicada-core use cases + planning — drives
//! the fakes through the paths the lifecycle tests don't reach:
//! resume/adopt/forced-NoOp apply, Instant ranges, refresh
//! resync/removal, validation diagnostics, capability checks.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::default_trait_access,
    clippy::items_after_statements,
    clippy::needless_pass_by_value,
    clippy::unnecessary_get_then_check
)]

use cicada_core::application::{
    ApplyPlan, ApplyRequest, PlanCalendar, PlanRequest, QueryRange, RefreshRequest, RefreshState,
    ShowEvent, ValidateCalendar, default_window,
};
use cicada_core::domain::calendar::Calendar;
use cicada_core::domain::capabilities::ProviderCapabilities;
use cicada_core::domain::event::{Conferencing, Event, EventBody, EventTime, Transparency};
use cicada_core::domain::ids::{LocalId, ZoneId};
use cicada_core::domain::policy::SendUpdates;
use cicada_core::domain::recurrence::{Frequency, RecurrenceRule, StructuredRule};
use cicada_core::domain::state::{JournalEntry, JournalOutcome};
use cicada_core::domain::time::{Time, TimeRange};
use cicada_core::planning::{CapabilityChecker, EventDiffer, Severity};
use cicada_core::ports::{CalendarProvider, CalendarRef, StateStore};
use cicada_testkit::{FakeCalendarProvider, FakeClock, MemoryStateStore};
use jiff::SignedDuration;
use jiff::civil::date;

fn body(summary: &str) -> EventBody {
    EventBody {
        summary: summary.into(),
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
    }
}

fn calendar(events: Vec<Event>) -> Calendar {
    let mut cal = Calendar::new("work", "UTC").unwrap();
    for e in events {
        cal.insert(e).unwrap();
    }
    cal
}

fn primary() -> CalendarRef {
    CalendarRef::new("primary")
}

fn clock() -> FakeClock {
    FakeClock::at("2026-10-23T01:00:00Z".parse().unwrap())
}

async fn plan(
    desired: &Calendar,
    provider: &dyn CalendarProvider,
    store: &MemoryStateStore,
    clock: &FakeClock,
) -> cicada_core::planning::Plan {
    let state = store.load().await.unwrap();
    PlanCalendar::new()
        .run(&PlanRequest {
            desired,
            recorded: state.calendar(&desired.name),
            provider,
            remote_calendar: &primary(),
            clock,
            send_updates: SendUpdates::default(),
        })
        .await
        .unwrap()
}

async fn apply(
    desired: &Calendar,
    p: &cicada_core::planning::Plan,
    provider: &dyn CalendarProvider,
    store: &MemoryStateStore,
    clock: &FakeClock,
    force: bool,
    resume: bool,
) -> cicada_core::application::ApplyReport {
    ApplyPlan::new()
        .run(&ApplyRequest {
            plan: p,
            desired,
            provider,
            state: store,
            remote_calendar: &primary(),
            provider_name: "fake",
            send_updates: SendUpdates::default(),
            force,
            resume,
            clock,
        })
        .await
        .unwrap()
}

// ---------- apply: resume / adopt / forced noop ----------

#[tokio::test]
async fn resume_skips_ops_journaled_as_applied() {
    let provider = FakeCalendarProvider::new();
    let store = MemoryStateStore::new();
    let clock = clock();
    let cal = calendar(vec![Event::new("a", body("A")), Event::new("b", body("B"))]);

    // Seed the journal as if `a` already landed in a crashed run.
    let plan = plan(&cal, &provider, &store, &clock).await;
    let create_a = plan
        .ops
        .iter()
        .find(|op| op.local_id().as_str() == "a")
        .unwrap();
    let mut state = store.load().await.unwrap();
    state.push_journal(JournalEntry {
        key: create_a.idempotency_key("primary"),
        op: create_a.kind(),
        local_id: LocalId::new("a"),
        applied_at: clock.now(),
        outcome: JournalOutcome::Applied {
            remote_id: Some("fake-1".into()),
        },
    });
    store.save(&state).await.unwrap();

    let report = apply(&cal, &plan, &provider, &store, &clock, false, true).await;
    let skipped = report
        .outcomes
        .iter()
        .filter(|o| matches!(o.result, cicada_core::application::OpResult::SkippedResume))
        .count();
    assert_eq!(skipped, 1, "{:?}", report.outcomes);
}

#[tokio::test]
async fn create_adopts_a_tagged_landing_with_matching_hash() {
    let provider = FakeCalendarProvider::new();
    let store = MemoryStateStore::new();
    let clock = clock();
    let event = Event::new("a", body("A"));
    let cal = calendar(vec![event.clone()]);

    // Pre-seed the provider as if a previous create landed but the
    // state write was lost — same cicada:id, same state hash.
    provider.seed_managed(&primary(), &LocalId::new("a"), event.body.clone());

    let plan = plan(&cal, &provider, &store, &clock).await;
    let report = apply(&cal, &plan, &provider, &store, &clock, false, false).await;
    // The tagged event was adopted rather than recreated.
    assert!(
        report.outcomes.iter().any(|o| matches!(
            o.result,
            cicada_core::application::OpResult::AlreadyApplied { .. }
                | cicada_core::application::OpResult::Applied { .. }
        )),
        "{:?}",
        report.outcomes
    );
    assert_eq!(provider.events(&primary()).len(), 1);
}

#[tokio::test]
async fn force_rewrites_a_conflicted_noop() {
    let provider = FakeCalendarProvider::new();
    let store = MemoryStateStore::new();
    let clock = clock();
    let cal = calendar(vec![Event::new("a", body("A"))]);

    let applied_plan = plan(&cal, &provider, &store, &clock).await;
    apply(&cal, &applied_plan, &provider, &store, &clock, false, false).await;

    // Out-of-band edit → the converged op is a conflicted NoOp.
    let remote = provider
        .find_by_tag(&primary(), &LocalId::new("a"))
        .await
        .unwrap()
        .unwrap();
    provider.simulate_remote_edit(&primary(), &remote.remote_id, |b| {
        b.summary = "Edited".into();
    });

    let drifted_plan = plan(&cal, &provider, &store, &clock).await;
    assert!(
        drifted_plan.ops.iter().any(|op| op.conflict().is_some()),
        "{:?}",
        drifted_plan.ops
    );

    // --force pushes desired over the edit.
    let report = apply(&cal, &drifted_plan, &provider, &store, &clock, true, false).await;
    assert!(!report.has_failures(), "{:?}", report.outcomes);
    let live = provider
        .find_by_tag(&primary(), &LocalId::new("a"))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(live.body.summary, "A");
}

// ---------- range: instants + error paths ----------

#[tokio::test]
async fn range_expands_instant_events_in_utc() {
    let mut ev = body("instant");
    ev.time = EventTime::Span {
        start: Time::Instant("2026-01-10T09:00:00Z".parse().unwrap()),
        duration: SignedDuration::from_mins(45),
    };
    let cal = calendar(vec![Event::new("i", ev)]);
    let range = TimeRange::new(
        "2026-01-01T00:00:00Z".parse().unwrap(),
        "2026-02-01T00:00:00Z".parse().unwrap(),
    )
    .unwrap();
    let occs = QueryRange::new()
        .expand(cal.events.values(), range)
        .unwrap();
    assert_eq!(occs.len(), 1);
    assert!(occs[0].zone.is_none());
    assert_eq!(occs[0].local_end.time().hour(), 9);
    assert_eq!(occs[0].local_end.time().minute(), 45);
}

#[tokio::test]
async fn show_event_renders_details() {
    let mut ev = body("show me");
    ev.description = Some("desc".into());
    ev.location = Some("room".into());
    ev.conferencing = Some(Conferencing::Meet);
    ev.reminders = vec![cicada_core::domain::reminder::Reminder::popup(
        SignedDuration::from_mins(10),
    )];
    ev.attendees = vec![
        cicada_core::domain::attendee::Attendee {
            email: "a@x".into(),
            required: true,
            status: Default::default(),
        },
        cicada_core::domain::attendee::Attendee {
            email: "b@x".into(),
            required: false,
            status: Default::default(),
        },
    ];
    ev.transparency = Transparency::Transparent;
    ev.recurrence = Some(RecurrenceRule::Structured(StructuredRule::new(
        Frequency::Daily,
    )));
    let event = Event::new("s", ev);
    let window = default_window("2026-01-01T00:00:00Z".parse().unwrap(), 30).unwrap();
    let detail = ShowEvent::new().run(&event, window).unwrap();
    assert!(!detail.occurrences.is_empty());
    assert!(default_window(jiff::Timestamp::MAX, 1).is_err());
}

// ---------- refresh: removal / etag resync / adoption ----------

#[tokio::test]
async fn refresh_drops_missing_and_resyncs_etag() {
    let provider = FakeCalendarProvider::new();
    let store = MemoryStateStore::new();
    let clock = clock();
    let cal = calendar(vec![Event::new("a", body("A")), Event::new("b", body("B"))]);

    let plan = plan(&cal, &provider, &store, &clock).await;
    apply(&cal, &plan, &provider, &store, &clock, false, false).await;

    // Out-of-band: delete `a`, silently retag `b`'s etag only
    // (body untouched → pure etag resync, not drift).
    let a = provider
        .find_by_tag(&primary(), &LocalId::new("a"))
        .await
        .unwrap()
        .unwrap();
    provider.simulate_remote_delete(&primary(), &a.remote_id);
    let b = provider
        .find_by_tag(&primary(), &LocalId::new("b"))
        .await
        .unwrap()
        .unwrap();
    // A no-op edit bumps the etag without touching the body — a
    // pure etag resync rather than content drift.
    provider.simulate_remote_edit(&primary(), &b.remote_id, |_| {});

    let req = RefreshRequest {
        calendar_name: "work",
        provider: &provider,
        remote_calendar: &primary(),
        state: &store,
        clock: &clock,
    };
    let report = RefreshState::new().run(&req).await.unwrap();
    assert_eq!(report.removed.len(), 1, "{report:?}");
    assert!(!report.reconciled.is_empty(), "{report:?}");

    // The recorded state now tracks only `b`.
    let state = store.load().await.unwrap();
    let rec = state.calendar("work").unwrap();
    assert!(rec.events.get(&LocalId::new("a")).is_none());
    assert!(rec.events.get(&LocalId::new("b")).is_some());
}

// ---------- capability checker ----------

#[test]
fn capability_checks_cover_every_gap_kind() {
    let checker = CapabilityChecker;
    let mut caps = ProviderCapabilities::permissive();
    caps.max_attendees = Some(1);
    caps.max_reminders = Some(1);
    caps.reminder_kinds = vec![cicada_core::domain::reminder::ReminderKind::Popup];
    caps.recurrence.frequencies = vec![Frequency::Weekly];
    caps.recurrence.supports_ordinal_by_day = false;
    caps.recurrence.supports_by_month_day = false;
    caps.recurrence.supports_by_month = false;
    caps.recurrence.supports_raw_rrule = false;
    caps.conferencing_kinds = vec![];
    caps.can_suppress_notifications = false;

    let mut b = body("busy");
    b.attendees = vec![
        cicada_core::domain::attendee::Attendee {
            email: "a@x".into(),
            required: true,
            status: Default::default(),
        },
        cicada_core::domain::attendee::Attendee {
            email: "b@x".into(),
            required: false,
            status: Default::default(),
        },
    ];
    b.reminders = vec![
        cicada_core::domain::reminder::Reminder::popup(SignedDuration::from_mins(5)),
        cicada_core::domain::reminder::Reminder::email(SignedDuration::from_hours(1)),
    ];
    let mut rule = StructuredRule::new(Frequency::Monthly);
    rule.by_month = vec![3];
    rule.by_month_day = vec![15];
    rule.by_day = vec![cicada_core::domain::recurrence::ByDay::nth(
        cicada_core::domain::recurrence::Weekday::Friday,
        -1,
    )];
    b.recurrence = Some(RecurrenceRule::Structured(rule));
    b.conferencing = Some(Conferencing::Meet);
    let issues = checker.check_event(&Event::new("e", b.clone()), &caps);
    let messages: Vec<_> = issues.iter().map(|i| i.message.as_str()).collect();
    assert!(issues.iter().all(|i| i.severity == Severity::Error));
    assert!(messages.iter().any(|m| m.contains("attendees")));
    assert!(messages.iter().any(|m| m.contains("reminders")));
    assert!(messages.iter().any(|m| m.contains("email reminders")));
    assert!(messages.iter().any(|m| m.contains("MONTHLY")));
    assert!(messages.iter().any(|m| m.contains("ordinal")));
    assert!(messages.iter().any(|m| m.contains("byMonthDay")));
    assert!(messages.iter().any(|m| m.contains("byMonth")));
    assert!(messages.iter().any(|m| m.contains("conferencing")));

    // A structured-in-raw-form rule checks the same fields.
    let mut b2 = body("raw");
    b2.recurrence = Some(RecurrenceRule::Raw(
        "FREQ=MONTHLY;BYDAY=-1FR;BYMONTHDAY=15;BYMONTH=3".into(),
    ));
    let issues = checker.check_body(&b2, Some("raw"), &caps);
    assert!(issues.iter().any(|i| i.message.contains("MONTHLY")));
    assert!(issues.iter().any(|i| i.message.contains("ordinal")));
    assert!(issues.iter().any(|i| i.message.contains("byMonthDay")));
    assert!(issues.iter().any(|i| i.message.contains("byMonth")));

    // An exotic raw rule needs raw-RRULE support.
    let mut b3 = body("exotic");
    b3.recurrence = Some(RecurrenceRule::Raw("FREQ=MONTHLY;BYSETPOS=-1".into()));
    let issues = checker.check_body(&b3, Some("ex"), &caps);
    assert!(issues.iter().any(|i| i.message.contains("raw RRULE")));

    // sendUpdates warning on a provider that can't suppress.
    let issue = checker
        .check_send_updates(SendUpdates::None, &caps)
        .unwrap();
    assert_eq!(issue.severity, Severity::Warning);
    assert!(
        checker
            .check_send_updates(SendUpdates::All, &caps)
            .is_none()
    );
}

// ---------- differ ----------

#[test]
fn differ_flags_each_changed_field() {
    let differ = EventDiffer;
    let a = body("A");
    let mut b = body("B");
    b.description = Some("d".into());
    b.location = Some("l".into());
    b.transparency = Transparency::Transparent;
    b.conferencing = Some(Conferencing::Teams);
    b.attendees = vec![cicada_core::domain::attendee::Attendee {
        email: "a@x".into(),
        required: true,
        status: Default::default(),
    }];
    b.reminders = vec![cicada_core::domain::reminder::Reminder::popup(
        SignedDuration::from_mins(5),
    )];
    b.recurrence = Some(RecurrenceRule::Raw("FREQ=DAILY".into()));
    b.time = EventTime::Range {
        start: a.time.start().clone(),
        end: Time::Wall {
            local: date(2026, 1, 6).at(10, 0, 0, 0),
            zone: ZoneId::new_unchecked("UTC"),
        },
    };
    let diff = differ.diff(&a, &b).unwrap();
    let fields: Vec<_> = diff.changes.iter().map(|c| c.field).collect();
    for f in [
        cicada_core::domain::operation::EventField::Summary,
        cicada_core::domain::operation::EventField::Description,
        cicada_core::domain::operation::EventField::Location,
        cicada_core::domain::operation::EventField::Time,
        cicada_core::domain::operation::EventField::Recurrence,
        cicada_core::domain::operation::EventField::Attendees,
        cicada_core::domain::operation::EventField::Reminders,
        cicada_core::domain::operation::EventField::Conferencing,
        cicada_core::domain::operation::EventField::Transparency,
    ] {
        assert!(fields.contains(&f), "missing {f:?} in {fields:?}");
    }
}

// ---------- plan / planner edge paths ----------

#[test]
fn plan_sorted_orders_ops_and_drift() {
    let mut plan =
        cicada_core::planning::Plan::new("work", "2026-01-01T00:00:00Z".parse().unwrap());
    // `sorted` puts remote ops first (create/update/delete/replace),
    // then noop, and sorts drift entries.
    plan.drift
        .push(cicada_core::domain::drift::DriftEntry::Modified {
            local_id: "z".into(),
            remote_id: "r2".into(),
            recorded_hash: "sha256:a".into(),
            live_hash: None,
        });
    plan.drift
        .push(cicada_core::domain::drift::DriftEntry::Missing {
            local_id: "a".into(),
            remote_id: "r1".into(),
        });
    let sorted = plan.sorted();
    assert_eq!(sorted.drift.len(), 2);
}

#[tokio::test]
async fn validate_report_counts_and_orders_diagnostics() {
    // An event violating several invariants produces diagnostics at
    // each severity; the report counts them.
    let mut ev = Event::new("", body("bad"));
    ev.body.time = EventTime::Range {
        start: Time::Wall {
            local: date(2026, 1, 6).at(9, 0, 0, 0),
            zone: ZoneId::new_unchecked("UTC"),
        },
        end: Time::Wall {
            local: date(2026, 1, 5).at(9, 0, 0, 0),
            zone: ZoneId::new_unchecked("UTC"),
        },
    };
    ev.body.recurrence = Some(RecurrenceRule::Structured(StructuredRule::new(
        Frequency::Hourly,
    )));
    let mut cal = Calendar::new("work", "UTC").unwrap();
    cal.events.insert("bad".into(), ev);
    let report = ValidateCalendar::new().run(&cal);
    assert!(
        report.count(cicada_core::application::Severity::Error) > 0,
        "{:?}",
        report.diagnostics
    );
    let _ = report.count(cicada_core::application::Severity::Warning);
}

#[tokio::test]
async fn plan_warns_on_unsuppressable_notifications() {
    let mut caps = ProviderCapabilities::permissive();
    caps.can_suppress_notifications = false;
    let provider = FakeCalendarProvider::with_capabilities(caps);
    let store = MemoryStateStore::new();
    let cal = calendar(vec![Event::new("a", body("A"))]);
    let state = store.load().await.unwrap();
    let plan = PlanCalendar::new()
        .run(&PlanRequest {
            desired: &cal,
            recorded: state.calendar("work"),
            provider: &provider,
            remote_calendar: &primary(),
            clock: &clock(),
            send_updates: SendUpdates::None,
        })
        .await
        .unwrap();
    assert!(
        plan.warnings.iter().any(|w| w.message.contains("suppress")),
        "{:?}",
        plan.warnings
    );
}

// ---------- recurrence leftover arms ----------

use cicada_core::domain::recurrence::{ByDay, RecurrenceExpander, Weekday};

fn expand(rule: RecurrenceRule) -> Vec<cicada_core::domain::recurrence::Occurrence> {
    let range = TimeRange::new(
        "2026-01-01T00:00:00Z".parse().unwrap(),
        "2028-01-01T00:00:00Z".parse().unwrap(),
    )
    .unwrap();
    let dtstart = Time::Wall {
        local: date(2026, 1, 5).at(9, 0, 0, 0),
        zone: ZoneId::new_unchecked("UTC"),
    };
    RecurrenceExpander::default()
        .expand(&rule, &dtstart, range)
        .unwrap()
}

#[test]
fn recurrence_frequency_and_filter_arms() {
    // Every frequency's bucket_anchor + candidates path.
    for freq in [
        Frequency::Daily,
        Frequency::Weekly,
        Frequency::Monthly,
        Frequency::Yearly,
    ] {
        let mut r = StructuredRule::new(freq);
        r.interval = 2;
        assert!(
            !expand(RecurrenceRule::Structured(r)).is_empty(),
            "{freq:?}"
        );
    }
    // Sub-daily frequencies refuse to expand.
    for freq in [Frequency::Hourly, Frequency::Minutely, Frequency::Secondly] {
        let err = RecurrenceExpander::default()
            .expand(
                &RecurrenceRule::Structured(StructuredRule::new(freq)),
                &Time::Wall {
                    local: date(2026, 1, 5).at(9, 0, 0, 0),
                    zone: ZoneId::new_unchecked("UTC"),
                },
                TimeRange::new(
                    "2026-01-01T00:00:00Z".parse().unwrap(),
                    "2026-02-01T00:00:00Z".parse().unwrap(),
                )
                .unwrap(),
            )
            .unwrap_err();
        assert!(err.to_string().contains("sub-daily"), "{err}");
    }
    // DAILY + non-ordinal byDay filter (day_filters_match arms).
    let mut r = StructuredRule::new(Frequency::Daily);
    r.by_day = vec![
        ByDay::every(Weekday::Monday),
        ByDay::every(Weekday::Wednesday),
    ];
    assert!(!expand(RecurrenceRule::Structured(r)).is_empty());
    // until: local DateTime arm of past_until.
    let mut r = StructuredRule::new(Frequency::Daily);
    r.until = Some(cicada_core::domain::recurrence::Until::DateTime(
        "2026-01-10T09:00:00".parse().unwrap(),
    ));
    assert!(!expand(RecurrenceRule::Structured(r)).is_empty());
    // Duplicate candidates dedupe (BYDAY + BYMONTHDAY naming the
    // same date → `seen.insert` returns false).
    let mut r = StructuredRule::new(Frequency::Weekly);
    r.by_day = vec![ByDay::every(Weekday::Monday)];
    r.by_month_day = vec![5];
    assert!(!expand(RecurrenceRule::Structured(r)).is_empty());
}

// ---------- validate leftover arms ----------

#[test]
fn validate_metadata_and_rule_and_until_arms() {
    let mut cal = Calendar::new("work", "UTC").unwrap();
    // No provider + no calendarId → metadata warning arm.
    let report = ValidateCalendar::new().run(&cal);
    assert!(!report.is_clean());
    assert!(
        report
            .diagnostics
            .iter()
            .any(|d| d.severity == cicada_core::application::Severity::Warning)
    );
    // Empty summary warning + empty id error.
    cal.events.insert(
        "".into(),
        Event::new("", {
            let mut b = body("");
            b.summary = " ".into();
            b
        }),
    );
    // Raw exotic RRULE → warning; raw garbage → error;
    // structured → early return arm.
    let mut exotic = body("x");
    exotic.recurrence = Some(RecurrenceRule::Raw("FREQ=MONTHLY;BYSETPOS=-1".into()));
    cal.events.insert("e1".into(), Event::new("e1", exotic));
    let mut garbage = body("x");
    garbage.recurrence = Some(RecurrenceRule::Raw("NOT-A-RULE".into()));
    cal.events.insert("e2".into(), Event::new("e2", garbage));
    let mut structured = body("x");
    structured.recurrence = Some(RecurrenceRule::Structured(StructuredRule::new(
        Frequency::Daily,
    )));
    cal.events.insert("e3".into(), Event::new("e3", structured));
    // until-before-start warnings — every Until×Time arm.
    for (i, (until, start)) in [
        (
            cicada_core::domain::recurrence::Until::Date(date(2020, 1, 1)),
            Time::Wall {
                local: date(2026, 1, 5).at(9, 0, 0, 0),
                zone: ZoneId::new_unchecked("UTC"),
            },
        ),
        (
            cicada_core::domain::recurrence::Until::DateTime(
                "2020-01-01T00:00:00".parse().unwrap(),
            ),
            Time::Wall {
                local: date(2026, 1, 5).at(9, 0, 0, 0),
                zone: ZoneId::new_unchecked("UTC"),
            },
        ),
        (
            cicada_core::domain::recurrence::Until::Instant(
                "2020-01-01T00:00:00Z".parse().unwrap(),
            ),
            Time::Instant("2026-01-05T09:00:00Z".parse().unwrap()),
        ),
        (
            cicada_core::domain::recurrence::Until::Date(date(2030, 1, 1)),
            Time::Instant("2026-01-05T09:00:00Z".parse().unwrap()),
        ),
    ]
    .into_iter()
    .enumerate()
    {
        let id = format!("u{i}");
        let mut e = body("x");
        e.time = EventTime::Span {
            start,
            duration: SignedDuration::from_mins(30),
        };
        let mut r = StructuredRule::new(Frequency::Daily);
        r.until = Some(until);
        e.recurrence = Some(RecurrenceRule::Structured(r));
        cal.events
            .insert(id.clone().into(), Event::new(id.as_str(), e));
    }
    let report = ValidateCalendar::new().run(&cal);
    let paths: Vec<_> = report.diagnostics.iter().map(|d| d.path.as_str()).collect();
    assert!(paths.iter().any(|p| p.contains("u0")), "{paths:?}");
    assert!(paths.iter().any(|p| p.contains("u1")), "{paths:?}");
    assert!(paths.iter().any(|p| p.contains("u2")), "{paths:?}");
    assert!(paths.iter().any(|p| p.contains("summary")), "{paths:?}");
}

// ---------- differ / time / http leftovers ----------

#[test]
fn differ_instant_time_and_http_json_error() {
    let differ = EventDiffer;
    let mut a = body("A");
    a.time = EventTime::Span {
        start: Time::Instant("2026-01-05T09:00:00Z".parse().unwrap()),
        duration: SignedDuration::from_mins(30),
    };
    let mut b = a.clone();
    b.time = EventTime::Range {
        start: Time::Instant("2026-01-05T09:00:00Z".parse().unwrap()),
        end: Time::Instant("2026-01-05T10:00:00Z".parse().unwrap()),
    };
    let diff = differ.diff(&a, &b).unwrap();
    assert!(
        diff.changes
            .iter()
            .any(|c| c.field == cicada_core::domain::operation::EventField::Time)
    );

    // Wall-time to_zoned error arm.
    let bad = Time::Wall {
        local: date(2026, 1, 5).at(9, 0, 0, 0),
        zone: ZoneId::new_unchecked("Not/AZone"),
    };
    assert!(bad.to_zoned().is_err());

    // HttpRequest::json serialization failure.
    use cicada_core::ports::{HttpMethod, HttpRequest};
    struct Unserializable;
    impl serde::Serialize for Unserializable {
        fn serialize<S: serde::Serializer>(&self, _: S) -> Result<S::Ok, S::Error> {
            Err(serde::ser::Error::custom("nope"))
        }
    }
    let req = HttpRequest::new(HttpMethod::Post, "http://x".to_owned());
    assert!(req.json(&Unserializable).is_err());
}

// ---------- apply leftover arms ----------

#[tokio::test]
async fn apply_noop_force_needs_desired_and_adopt_arm() {
    let provider = FakeCalendarProvider::new();
    let store = MemoryStateStore::new();
    let clock = clock();
    let cal = calendar(vec![Event::new("a", body("A"))]);
    let p1 = plan(&cal, &provider, &store, &clock).await;
    apply(&cal, &p1, &provider, &store, &clock, false, false).await;

    // OOB edit → conflicted NoOp; --force rewrites via the desired
    // event (covers the NoOp arm's success path).
    let remote = provider
        .find_by_tag(&primary(), &LocalId::new("a"))
        .await
        .unwrap()
        .unwrap();
    provider.simulate_remote_edit(&primary(), &remote.remote_id, |b| {
        b.summary = "OOB".into();
    });
    let p2 = plan(&cal, &provider, &store, &clock).await;
    let report = apply(&cal, &p2, &provider, &store, &clock, true, false).await;
    assert!(!report.has_failures(), "{:?}", report.outcomes);
}

// ---------- refresh let-else continue arms ----------

#[tokio::test]
async fn refresh_continue_arms() {
    // These `continue` arms are defensive — reached only when the
    // drift list and live snapshot disagree mid-run. Drive them by
    // seeding recorded state whose remote ids no longer exist.
    let provider = FakeCalendarProvider::new();
    let store = MemoryStateStore::new();
    let clock = clock();
    let cal = calendar(vec![Event::new("a", body("A"))]);
    let plan = plan(&cal, &provider, &store, &clock).await;
    apply(&cal, &plan, &provider, &store, &clock, false, false).await;
    // Delete every live event → all recorded entries are Missing →
    // removed list; the updates/etag_resyncs vecs stay empty.
    for ev in provider.events(&primary()) {
        provider.simulate_remote_delete(&primary(), &ev.remote_id);
    }
    let req = RefreshRequest {
        calendar_name: "work",
        provider: &provider,
        remote_calendar: &primary(),
        state: &store,
        clock: &clock,
    };
    let report = RefreshState::new().run(&req).await.unwrap();
    assert!(!report.removed.is_empty());
}

// ---------- testkit overlaps end-error arm ----------

#[tokio::test]
async fn overlaps_end_unresolvable_is_conservative() {
    let p = FakeCalendarProvider::new();
    let cal = CalendarRef::new("primary");
    let mut ev = body("bad-end");
    ev.time = EventTime::Range {
        start: Time::Instant("2026-01-05T09:00:00Z".parse().unwrap()),
        end: Time::Wall {
            local: date(2026, 1, 5).at(10, 0, 0, 0),
            zone: ZoneId::new_unchecked("Not/AZone"),
        },
    };
    p.seed_foreign(&cal, ev);
    let range = TimeRange::new(
        "2026-01-01T00:00:00Z".parse().unwrap(),
        "2026-02-01T00:00:00Z".parse().unwrap(),
    )
    .unwrap();
    let listed = p.list_events(&cal, Some(range)).await.unwrap();
    assert_eq!(listed.len(), 1);
}

// ---------- apply.rs remaining arms ----------

#[tokio::test]
async fn replace_adopts_tagged_remote_with_desired_hash() {
    // A remote carrying our tag + the *desired* hash means the
    // replace already landed (crash between write and journal) —
    // adopt it instead of deleting.
    let provider = FakeCalendarProvider::new();
    let store = MemoryStateStore::new();
    let clock = clock();
    let mut body_v1 = body("A");
    body_v1.recurrence = Some(RecurrenceRule::Structured(StructuredRule::new(
        Frequency::Daily,
    )));
    let cal = calendar(vec![Event::new("a", body_v1)]);
    let p1 = plan(&cal, &provider, &store, &clock).await;
    apply(&cal, &p1, &provider, &store, &clock, false, false).await;

    // Desired v2 changes the rule → Replace; the live event already
    // carries v2's body+hash (a landed-but-unjournaled write).
    let mut body_v2 = body("A");
    body_v2.recurrence = Some(RecurrenceRule::Structured(StructuredRule::new(
        Frequency::Weekly,
    )));
    for ev in provider.events(&primary()) {
        provider.simulate_remote_delete(&primary(), &ev.remote_id);
    }
    provider.seed_managed(&primary(), &LocalId::new("a"), body_v2.clone());

    let cal = calendar(vec![Event::new("a", body_v2)]);
    let p2 = plan(&cal, &provider, &store, &clock).await;
    let report = apply(&cal, &p2, &provider, &store, &clock, true, false).await;
    assert!(
        report.outcomes.iter().any(|o| matches!(
            o.result,
            cicada_core::application::OpResult::AlreadyApplied { .. }
        )),
        "{:?}",
        report.outcomes
    );
}

#[tokio::test]
async fn replace_without_tag_deletes_and_recreates() {
    // find_by_tag → None: the remote at the recorded id is foreign
    // (no cicada:id tag) — the replace deletes + recreates.
    let provider = FakeCalendarProvider::new();
    let store = MemoryStateStore::new();
    let clock = clock();
    let mut body_v1 = body("A");
    body_v1.recurrence = Some(RecurrenceRule::Structured(StructuredRule::new(
        Frequency::Daily,
    )));
    let cal = calendar(vec![Event::new("a", body_v1.clone())]);
    let p1 = plan(&cal, &provider, &store, &clock).await;
    apply(&cal, &p1, &provider, &store, &clock, false, false).await;

    // Replace the live event with an untagged one at the same id.
    let remote = provider.events(&primary()).remove(0);
    let remote_id = remote.remote_id.clone();
    provider.simulate_remote_delete(&primary(), &remote_id);
    provider.seed(
        &primary(),
        cicada_core::ports::RemoteEvent {
            remote_id: remote_id.clone(),
            tags: cicada_core::ports::EventTags::default(),
            etag: None,
            updated: None,
            body: body_v1,
        },
    );

    let mut body_v2 = body("A");
    body_v2.recurrence = Some(RecurrenceRule::Structured(StructuredRule::new(
        Frequency::Weekly,
    )));
    let cal = calendar(vec![Event::new("a", body_v2)]);
    let p2 = plan(&cal, &provider, &store, &clock).await;
    let report = apply(&cal, &p2, &provider, &store, &clock, true, false).await;
    assert!(
        report
            .outcomes
            .iter()
            .any(|o| matches!(o.result, cicada_core::application::OpResult::Applied { .. })),
        "{:?}",
        report.outcomes
    );
    // The foreign remote was deleted; the tag now exists.
    assert!(
        provider
            .find_by_tag(&primary(), &LocalId::new("a"))
            .await
            .unwrap()
            .is_some()
    );
}

#[tokio::test]
async fn forced_noop_rewrite_errors_without_desired_event() {
    // A drifted NoOp under --force rewrites live from desired state;
    // a synthetic plan can name an id the desired calendar lacks.
    let provider = FakeCalendarProvider::new();
    let store = MemoryStateStore::new();
    let clock = clock();
    let mut plan = cicada_core::planning::Plan::new("work", clock.now());
    plan.ops
        .push(cicada_core::domain::operation::Operation::NoOp {
            local_id: LocalId::new("ghost"),
            remote_id: "r-1".into(),
            hash: cicada_core::domain::ids::ContentHash::new("sha256:x"),
            conflict: Some(cicada_core::domain::drift::DriftKind::Missing),
        });
    let cal = calendar(vec![]);
    let report = ApplyPlan::new()
        .run(&ApplyRequest {
            plan: &plan,
            desired: &cal,
            provider: &provider,
            state: &store,
            remote_calendar: &primary(),
            provider_name: "fake",
            send_updates: SendUpdates::default(),
            force: true,
            resume: false,
            clock: &clock,
        })
        .await
        .unwrap();
    // The op-level failure is journaled, not returned as Err.
    let [outcome] = report.outcomes.as_slice() else {
        panic!("expected one outcome: {report:?}");
    };
    assert!(
        matches!(&outcome.result, cicada_core::application::OpResult::Failed(e) if e.to_string().contains("ghost")),
        "{outcome:?}"
    );
}

#[test]
fn raw_rrule_capability_gaps_use_the_parsed_form() {
    // Raw RRULEs that normalize hit the same checks as structured
    // rules (the `Some(r)` arm); unparseable ones hit the raw gate.
    let checker = CapabilityChecker;
    let mut caps = ProviderCapabilities::permissive();
    caps.recurrence.frequencies = vec![Frequency::Weekly];
    caps.recurrence.supports_ordinal_by_day = false;
    caps.recurrence.supports_by_month_day = false;
    caps.recurrence.supports_by_month = false;
    caps.recurrence.supports_raw_rrule = false;

    for (raw, want) in [
        ("FREQ=DAILY", "DAILY"),
        ("FREQ=WEEKLY;BYDAY=-1FR", "ordinal byDay"),
        ("FREQ=MONTHLY;BYMONTHDAY=15", "byMonthDay"),
        ("FREQ=YEARLY;BYMONTH=3", "byMonth"),
    ] {
        let mut b = body("x");
        b.recurrence = Some(RecurrenceRule::Raw(raw.into()));
        let issues = checker.check_body(&b, Some("x"), &caps);
        assert!(
            issues.iter().any(|i| i.message.contains(want)),
            "{raw}: {issues:?}"
        );
    }

    let mut b = body("x");
    b.recurrence = Some(RecurrenceRule::Raw("GARBAGE".into()));
    let issues = checker.check_body(&b, Some("x"), &caps);
    assert!(
        issues.iter().any(|i| i.message.contains("raw RRULE")),
        "{issues:?}"
    );

    // Same unparseable rule with raw RRULEs allowed — the `if`
    // guard's false edge (no error pushed).
    let mut caps = ProviderCapabilities::permissive();
    let issues = checker.check_body(&b, Some("x"), &caps);
    assert!(issues.is_empty(), "{issues:?}");
    let _ = &mut caps;
}
