//! The fakes are the point: `FakeCalendarProvider` must honor the
//! tagging contract, ETag conditional writes, and idempotent
//! creates; `MemoryStateStore`/`FakeClock` round-trip trivially.
//! `PlanCalendar` gets its end-to-end here — core can't see the
//! fakes (testkit depends on core, not vice versa).
#![allow(clippy::unwrap_used, clippy::expect_used)]

use cicada_core::application::{PlanCalendar, PlanRequest};
use cicada_core::domain::calendar::Calendar;
use cicada_core::domain::drift::DriftKind;
use cicada_core::domain::error::CicadaError;
use cicada_core::domain::event::{Event, EventBody, EventTime, Transparency};
use cicada_core::domain::ids::{LocalId, RemoteId, ZoneId};
use cicada_core::domain::operation::OpKind;
use cicada_core::domain::policy::{SendUpdates, WriteOptions};
use cicada_core::domain::state::{JournalEntry, JournalOutcome, RecordedState};
use cicada_core::domain::time::Time;
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

// ---------- FakeCalendarProvider ----------

#[tokio::test]
async fn create_tags_events_and_find_by_tag_resolves() {
    let provider = FakeCalendarProvider::new();
    let created = provider
        .create_event(
            &primary(),
            &body("Standup"),
            &LocalId::new("standup"),
            &WriteOptions::default(),
        )
        .await
        .unwrap();
    assert!(created.tags.managed);
    assert_eq!(
        created.tags.local_id.as_ref().map(LocalId::as_str),
        Some("standup")
    );
    assert_eq!(
        created
            .tags
            .state_hash
            .as_ref()
            .map(cicada_core::domain::ids::ContentHash::as_str),
        Some(
            Event::new("standup", body("Standup"))
                .content_hash()
                .unwrap()
                .as_str()
        )
    );

    let found = provider
        .find_by_tag(&primary(), &LocalId::new("standup"))
        .await
        .unwrap();
    assert_eq!(found.unwrap().remote_id, created.remote_id);
    assert!(
        provider
            .find_by_tag(&primary(), &LocalId::new("nope"))
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn create_dedupes_on_idempotency_key() {
    let provider = FakeCalendarProvider::new();
    let opts = WriteOptions {
        idempotency_key: Some("primary|standup|create|sha256:x".into()),
        ..WriteOptions::default()
    };
    let first = provider
        .create_event(&primary(), &body("A"), &LocalId::new("a"), &opts)
        .await
        .unwrap();
    let second = provider
        .create_event(&primary(), &body("A"), &LocalId::new("a"), &opts)
        .await
        .unwrap();
    assert_eq!(first.remote_id, second.remote_id);
    assert_eq!(provider.events(&primary()).len(), 1);
}

#[tokio::test]
async fn update_and_delete_honor_etags() {
    let provider = FakeCalendarProvider::new();
    let created = provider
        .create_event(
            &primary(),
            &body("A"),
            &LocalId::new("a"),
            &WriteOptions::default(),
        )
        .await
        .unwrap();

    // Stale etag → conflict.
    let stale = WriteOptions {
        etag: Some("etag-999".into()),
        ..WriteOptions::default()
    };
    let err = provider
        .update_event(&primary(), &created.remote_id, &body("B"), &stale)
        .await
        .unwrap_err();
    assert!(matches!(err, CicadaError::Conflict(_)));

    // Right etag → lands, bumps the etag.
    let fresh = WriteOptions {
        etag: created.etag.clone(),
        ..WriteOptions::default()
    };
    let updated = provider
        .update_event(&primary(), &created.remote_id, &body("B"), &fresh)
        .await
        .unwrap();
    assert_eq!(updated.body.summary, "B");
    assert_ne!(updated.etag, created.etag);
    assert_eq!(
        updated.tags.local_id.as_ref().map(LocalId::as_str),
        Some("a")
    );

    // Missing remote → conflict on update, silent success on delete.
    let err = provider
        .update_event(
            &primary(),
            &RemoteId::new("gone"),
            &body("B"),
            &WriteOptions::default(),
        )
        .await
        .unwrap_err();
    assert!(matches!(err, CicadaError::Conflict(_)));
    provider
        .delete_event(&primary(), &RemoteId::new("gone"), &WriteOptions::default())
        .await
        .unwrap();

    provider
        .delete_event(&primary(), &created.remote_id, &WriteOptions::default())
        .await
        .unwrap();
    assert!(provider.events(&primary()).is_empty());
}

#[tokio::test]
async fn simulated_drift_is_visible_but_tagged() {
    let provider = FakeCalendarProvider::new();
    let seeded = provider.seed_managed(&primary(), &LocalId::new("a"), body("A"));
    assert!(
        provider.simulate_remote_edit(&primary(), &seeded.remote_id, |b| {
            b.summary = "Edited in the UI".into();
        })
    );
    let after = provider
        .get_event(&primary(), &seeded.remote_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(after.body.summary, "Edited in the UI");
    assert_ne!(after.etag, seeded.etag);
    // Tags are untouched — that's what makes it *drift* detectable.
    assert_eq!(after.tags.state_hash, seeded.tags.state_hash);

    assert!(provider.simulate_remote_delete(&primary(), &seeded.remote_id));
    assert!(!provider.simulate_remote_delete(&primary(), &seeded.remote_id));
}

#[tokio::test]
async fn free_busy_reports_opaque_events() {
    let provider = FakeCalendarProvider::new();
    provider.seed_managed(&primary(), &LocalId::new("a"), body("Busy"));
    let mut free = body("Not busy");
    free.transparency = Transparency::Transparent;
    provider.seed_foreign(&primary(), free);

    let range = cicada_core::domain::time::TimeRange::new(
        "2026-01-05T00:00:00Z".parse().unwrap(),
        "2026-01-06T00:00:00Z".parse().unwrap(),
    )
    .unwrap();
    let busy = provider.free_busy(&primary(), range).await.unwrap();
    assert_eq!(busy.len(), 1);
    assert_eq!(busy[0].start.to_string(), "2026-01-05T09:00:00Z");

    let quiet = FakeCalendarProvider::with_capabilities(
        cicada_core::domain::capabilities::ProviderCapabilities::nothing(),
    );
    assert!(quiet.free_busy(&primary(), range).await.is_err());
}

// ---------- MemoryStateStore / FakeClock ----------

#[tokio::test]
async fn memory_state_store_round_trips() {
    let store = MemoryStateStore::new();
    assert_eq!(store.load().await.unwrap(), RecordedState::new());

    let mut state = RecordedState::new();
    state.calendar_mut("work").record(
        "standup".into(),
        "fake-1".into(),
        "sha256:x".into(),
        "2026-01-01T00:00:00Z".parse().unwrap(),
    );
    store.save(&state).await.unwrap();
    assert_eq!(store.load().await.unwrap(), state);

    let entry = JournalEntry {
        key: "primary|standup|create|sha256:x".into(),
        op: OpKind::Create,
        local_id: "standup".into(),
        applied_at: "2026-01-01T00:00:00Z".parse().unwrap(),
        outcome: JournalOutcome::Applied {
            remote_id: Some("fake-1".into()),
        },
    };
    store.append_journal(&entry).await.unwrap();
    assert_eq!(store.journal().await.unwrap(), vec![entry]);
    // The journal lives inside the state document too.
    assert_eq!(store.snapshot().journal.len(), 1);
}

#[test]
fn fake_clock_advances() {
    let clock = FakeClock::at("2026-01-01T00:00:00Z".parse().unwrap());
    assert_eq!(clock.now().to_string(), "2026-01-01T00:00:00Z");
    clock.advance(SignedDuration::from_hours(2));
    assert_eq!(clock.now().to_string(), "2026-01-01T02:00:00Z");
}

// ---------- PlanCalendar end-to-end ----------

#[tokio::test]
async fn plan_calendar_fetches_live_and_plans() {
    let provider = FakeCalendarProvider::new();
    let clock = FakeClock::at("2026-10-23T01:00:00Z".parse().unwrap());
    let desired = calendar(vec![
        Event::new("standup", body("Standup")),
        Event::new("retro", body("Retro")),
    ]);

    // `retro` was applied before; `standup` is new. Seed live with a
    // stale copy + an out-of-band edit on `retro`.
    let applied_retro = Event::new("retro", body("Retro"));
    let seeded = provider.seed_managed(
        &primary(),
        &LocalId::new("retro"),
        applied_retro.body.clone(),
    );
    provider.simulate_remote_edit(&primary(), &seeded.remote_id, |b| {
        b.summary = "Retro (moved)".into();
    });
    provider.seed_foreign(&primary(), body("Someone's lunch"));

    let mut recorded = cicada_core::domain::state::RecordedCalendar::default();
    recorded.record(
        "retro".into(),
        seeded.remote_id.clone(),
        applied_retro.content_hash().unwrap(),
        "2026-01-01T00:00:00Z".parse().unwrap(),
    );
    recorded
        .events
        .get_mut(&LocalId::new("retro"))
        .unwrap()
        .etag = seeded.etag.clone();

    let req = PlanRequest {
        desired: &desired,
        recorded: Some(&recorded),
        provider: &provider,
        remote_calendar: &primary(),
        clock: &clock,
        send_updates: SendUpdates::default(),
    };
    let plan = PlanCalendar::new().run(&req).await.unwrap();

    assert_eq!(plan.api_version, "cicada/plan-v1");
    assert_eq!(plan.generated_at.to_string(), "2026-10-23T01:00:00Z");
    // standup → Create; retro → NoOp but drifted (etag bumped).
    assert_eq!(plan.ops.len(), 2);
    assert_eq!(plan.ops[0].kind(), OpKind::Create); // creates sort first
    assert_eq!(plan.ops[1].kind(), OpKind::NoOp);
    assert_eq!(plan.drift.len(), 1);
    assert!(matches!(plan.drift[0].kind(), DriftKind::Modified { .. }));
    assert!(plan.ops[1].conflict().is_some());
}
