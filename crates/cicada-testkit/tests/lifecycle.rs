//! The full lifecycle over the in-memory fakes: `PlanCalendar` →
//! `ApplyPlan` → `DetectDrift`/`RefreshState`. Op-kind matrix,
//! conflict gating, --force, tag-reconciled resume, per-op journal
//! contents, and honest partial failure (ADR-0001 §4, ADR-0002 §3/§4).
#![allow(clippy::unwrap_used, clippy::expect_used)]

use cicada_core::application::{
    ApplyPlan, ApplyRequest, DetectDrift, DriftRequest, OpResult, PlanCalendar, PlanRequest,
    RefreshRequest, RefreshState,
};
use cicada_core::domain::calendar::Calendar;
use cicada_core::domain::drift::{DriftEntry, DriftKind};
use cicada_core::domain::error::{CicadaError, Result};
use cicada_core::domain::event::{Event, EventBody, EventTime, Transparency};
use cicada_core::domain::ids::{LocalId, RemoteId, ZoneId};
use cicada_core::domain::operation::{OpKind, Operation};
use cicada_core::domain::policy::{SendUpdates, WriteOptions};
use cicada_core::domain::recurrence::{Frequency, RecurrenceRule, StructuredRule};
use cicada_core::domain::state::JournalOutcome;
use cicada_core::domain::time::Time;
use cicada_core::ports::{CalendarProvider, CalendarRef, RemoteEvent, StateStore};
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

fn rule(freq: Frequency) -> RecurrenceRule {
    RecurrenceRule::Structured(StructuredRule::new(freq))
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

/// Plan `desired` against provider live + store recorded state.
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

/// Apply `plan` with the given flags.
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

// ---------- the all-op-kinds matrix ----------

#[tokio::test]
async fn apply_every_op_kind_and_converge() {
    let provider = FakeCalendarProvider::new();
    let store = MemoryStateStore::new();
    let clock = clock();

    // v1 seeds the world: standup(weekly), retro, temp, still.
    let mut standup = body("Standup");
    standup.recurrence = Some(rule(Frequency::Weekly));
    let v1 = calendar(vec![
        Event::new("standup", standup),
        Event::new("retro", body("Retro")),
        Event::new("temp", body("Temp")),
        Event::new("still", body("Still")),
    ]);
    let p1 = plan(&v1, &provider, &store, &clock).await;
    assert_eq!(p1.remote_op_count(), 4);
    let r1 = apply(&v1, &p1, &provider, &store, &clock, false, false).await;
    assert_eq!(r1.applied(), 4);
    assert!(!r1.has_failures());
    assert_eq!(provider.events(&primary()).len(), 4);
    let standup_remote = store.snapshot().calendars["work"].events[&LocalId::new("standup")]
        .remote_id
        .clone();

    // v2: Replace (recurrence), Update (summary), Delete (absent),
    // Create (new), NoOp (unchanged) — every kind in one plan.
    let mut standup2 = body("Standup");
    standup2.recurrence = Some(rule(Frequency::Daily));
    let v2 = calendar(vec![
        Event::new("fresh", body("Fresh")),
        Event::new("retro", body("Retro (weekly)")),
        Event::new("standup", standup2),
        Event::new("still", body("Still")),
    ]);
    let p2 = plan(&v2, &provider, &store, &clock).await;
    let kinds: Vec<OpKind> = p2.ops.iter().map(Operation::kind).collect();
    assert_eq!(
        kinds,
        [
            OpKind::Delete,
            OpKind::Replace,
            OpKind::Create,
            OpKind::Update,
            OpKind::NoOp
        ]
    );

    let r2 = apply(&v2, &p2, &provider, &store, &clock, false, false).await;
    assert!(!r2.has_failures());
    assert_eq!(r2.applied(), 4);
    assert_eq!(r2.skipped(), 1); // the noop

    // Remote truth: temp deleted, standup recreated under a new id,
    // retro updated in place, fresh created.
    let live = provider.events(&primary());
    assert_eq!(live.len(), 4);
    let by_tag = |id: &str| {
        live.iter()
            .find(|e| e.tags.local_id.as_ref().map(LocalId::as_str) == Some(id))
            .unwrap()
    };
    assert_eq!(by_tag("retro").body.summary, "Retro (weekly)");
    assert_ne!(by_tag("standup").remote_id, standup_remote);
    assert_eq!(by_tag("fresh").body.summary, "Fresh");
    assert!(live.iter().all(|e| e.tags.managed));

    // Recorded state mirrors it: temp forgotten, new hashes, the
    // binding persisted.
    let state = store.snapshot();
    let work = &state.calendars["work"];
    assert_eq!(work.provider, "fake");
    assert_eq!(work.calendar_id, "primary");
    assert_eq!(work.events.len(), 4);
    assert!(!work.events.contains_key(&"temp".into()));
    assert_eq!(
        work.events[&LocalId::new("standup")].remote_id,
        by_tag("standup").remote_id
    );
    assert_eq!(
        work.events[&LocalId::new("retro")].hash,
        v2.events[&LocalId::new("retro")].content_hash().unwrap()
    );

    // Journal: one entry per remote op (the noop isn't journaled),
    // every entry Applied with the op's idempotency key.
    let journal = store.journal().await.unwrap();
    assert_eq!(journal.len(), 4 + 4); // v1 + v2 applies
    let v2_entries = &journal[4..];
    assert_eq!(v2_entries.len(), 4);
    assert!(
        v2_entries
            .iter()
            .all(|e| matches!(e.outcome, JournalOutcome::Applied { .. }))
    );
    assert_eq!(
        v2_entries[0].key,
        format!("primary|temp|delete|{}", p2.ops[0].hash())
    );

    // Converged: re-plan is all NoOp, re-apply is a clean no-op run.
    let p3 = plan(&v2, &provider, &store, &clock).await;
    assert_eq!(p3.remote_op_count(), 0);
    assert!(p3.ops.iter().all(|op| op.kind() == OpKind::NoOp));
}

// ---------- conflict policy (ADR-0002 §4) ----------

#[tokio::test]
async fn conflicted_op_fails_without_force_and_lands_with_it() {
    let provider = FakeCalendarProvider::new();
    let store = MemoryStateStore::new();
    let clock = clock();

    let v1 = calendar(vec![Event::new("standup", body("Standup"))]);
    let p1 = plan(&v1, &provider, &store, &clock).await;
    apply(&v1, &p1, &provider, &store, &clock, false, false).await;

    // Out-of-band edit: same desired, drifted live → conflicted NoOp.
    let remote_id = store.snapshot().calendars["work"].events[&LocalId::new("standup")]
        .remote_id
        .clone();
    provider.simulate_remote_edit(&primary(), &remote_id, |b| {
        b.summary = "Renamed in the UI".into();
    });

    let p2 = plan(&v1, &provider, &store, &clock).await;
    assert_eq!(p2.drift.len(), 1);
    assert!(p2.ops[0].conflict().is_some());

    // Without --force: the op fails with Conflict, the journal
    // records it, remote keeps the out-of-band edit, recorded state
    // is untouched.
    let r2 = apply(&v1, &p2, &provider, &store, &clock, false, false).await;
    assert!(r2.has_failures());
    assert_eq!(r2.failed(), 1);
    let OpResult::Failed(e) = &r2.outcomes[0].result else {
        panic!("expected failure");
    };
    assert!(matches!(e, CicadaError::Conflict(_)));
    let journal = store.journal().await.unwrap();
    let last = journal.last().unwrap();
    let JournalOutcome::Failed { error_kind, .. } = &last.outcome else {
        panic!("expected Failed journal entry");
    };
    assert_eq!(*error_kind, cicada_core::ErrorKind::Conflict);
    assert_eq!(
        provider
            .get_event(&primary(), &remote_id)
            .await
            .unwrap()
            .unwrap()
            .body
            .summary,
        "Renamed in the UI"
    );

    // With --force on the same plan: desired wins — the drifted NoOp
    // is rewritten over the live edit.
    let r3 = apply(&v1, &p2, &provider, &store, &clock, true, false).await;
    assert!(!r3.has_failures());
    assert_eq!(r3.applied(), 1);
    assert_eq!(
        provider
            .get_event(&primary(), &remote_id)
            .await
            .unwrap()
            .unwrap()
            .body
            .summary,
        "Standup"
    );
    // And drift is gone.
    let drift = drift_now(&provider, &store).await;
    assert!(drift.is_empty());
}

#[tokio::test]
async fn error_plan_is_refused_before_any_write() {
    let store = MemoryStateStore::new();
    let clock = clock();

    let mut too_many = body("All-hands");
    too_many.attendees = vec![
        cicada_core::domain::attendee::Attendee::new("a@x.com"),
        cicada_core::domain::attendee::Attendee::new("b@x.com"),
    ];
    let cal = calendar(vec![Event::new("allhands", too_many)]);
    let mut caps = cicada_core::domain::capabilities::ProviderCapabilities::permissive();
    caps.max_attendees = Some(1);
    let provider = FakeCalendarProvider::with_capabilities(caps);
    let p = plan(&cal, &provider, &store, &clock).await;
    assert!(p.has_errors());

    let err = ApplyPlan::new()
        .run(&ApplyRequest {
            plan: &p,
            desired: &cal,
            provider: &provider,
            state: &store,
            remote_calendar: &primary(),
            provider_name: "fake",
            send_updates: SendUpdates::default(),
            force: false,
            resume: false,
            clock: &clock,
        })
        .await
        .unwrap_err();
    assert_eq!(err.kind(), cicada_core::ErrorKind::Fatal);
    assert!(provider.events(&primary()).is_empty());
    assert!(store.journal().await.unwrap().is_empty());
}

// ---------- resume (ADR-0002 §3) ----------

#[tokio::test]
async fn resume_skips_journaled_ops() {
    let provider = FakeCalendarProvider::new();
    let store = MemoryStateStore::new();
    let clock = clock();

    let v1 = calendar(vec![Event::new("a", body("A")), Event::new("b", body("B"))]);
    let p1 = plan(&v1, &provider, &store, &clock).await;
    apply(&v1, &p1, &provider, &store, &clock, false, false).await;

    // Re-apply the SAME plan with --resume: every op is journaled
    // already → skipped, and no duplicate remote events.
    let r2 = apply(&v1, &p1, &provider, &store, &clock, false, true).await;
    assert_eq!(r2.outcomes.len(), 2);
    assert!(
        r2.outcomes
            .iter()
            .all(|o| o.result == OpResult::SkippedResume)
    );
    assert_eq!(provider.events(&primary()).len(), 2);
    // Journal unchanged — skips don't append.
    assert_eq!(store.journal().await.unwrap().len(), 2);
}

#[tokio::test]
async fn tag_reconcile_adopts_ambiguous_create() {
    // The failure window ADR-0002 §3 names: provider write landed,
    // journal write crashed. Live has the tag; recorded state and the
    // journal are empty.
    let provider = FakeCalendarProvider::new();
    let store = MemoryStateStore::new();
    let clock = clock();
    let seeded = provider.seed_managed(&primary(), &LocalId::new("standup"), body("Standup"));

    let v1 = calendar(vec![Event::new("standup", body("Standup"))]);
    let p1 = plan(&v1, &provider, &store, &clock).await;
    assert_eq!(p1.ops[0].kind(), OpKind::Create);
    // The planner warns about the tagged live event.
    assert!(
        p1.warnings
            .iter()
            .any(|w| w.local_id.as_deref() == Some("standup"))
    );

    let r = apply(&v1, &p1, &provider, &store, &clock, false, true).await;
    assert_eq!(
        r.outcomes[0].result,
        OpResult::AlreadyApplied {
            remote_id: Some(seeded.remote_id.clone())
        }
    );
    // No duplicate was created; the journal says so.
    assert_eq!(provider.events(&primary()).len(), 1);
    let last = store.journal().await.unwrap().last().unwrap().clone();
    assert!(matches!(
        last.outcome,
        JournalOutcome::SkippedAlreadyApplied { .. }
    ));
    // And recorded state adopted the remote id.
    assert_eq!(
        store.snapshot().calendars["work"].events[&LocalId::new("standup")].remote_id,
        seeded.remote_id
    );
}

#[tokio::test]
async fn partial_failure_preserves_state_and_continues() {
    // One conflicted op fails (journaled) while a clean op still
    // lands — apply is per-op honest, not all-or-nothing.
    let provider = FakeCalendarProvider::new();
    let store = MemoryStateStore::new();
    let clock = clock();

    let v1 = calendar(vec![
        Event::new("drifted", body("Old")),
        Event::new("clean", body("Clean")),
    ]);
    let p1 = plan(&v1, &provider, &store, &clock).await;
    apply(&v1, &p1, &provider, &store, &clock, false, false).await;

    // Drift `drifted`, rename `clean`, add `fresh`.
    let drifted_remote = store.snapshot().calendars["work"].events[&LocalId::new("drifted")]
        .remote_id
        .clone();
    provider.simulate_remote_edit(&primary(), &drifted_remote, |b| {
        b.summary = "UI edit".into();
    });
    let v2 = calendar(vec![
        Event::new("clean", body("Clean v2")),
        Event::new("drifted", body("Old")),
        Event::new("fresh", body("Fresh")),
    ]);
    let p2 = plan(&v2, &provider, &store, &clock).await;
    let r2 = apply(&v2, &p2, &provider, &store, &clock, false, false).await;

    assert!(r2.has_failures());
    // update(drifted) failed on conflict; create(fresh) landed;
    // update(clean) landed.
    assert_eq!(r2.applied(), 2);
    assert_eq!(r2.failed(), 1);
    let journal = store.journal().await.unwrap();
    let failed = journal
        .iter()
        .find(|e| matches!(e.outcome, JournalOutcome::Failed { .. }))
        .unwrap();
    assert_eq!(failed.local_id.as_str(), "drifted");
    // Failed op's record is untouched; fresh's record exists.
    let state = store.snapshot();
    assert!(state.calendars["work"].events.contains_key(&"fresh".into()));
    assert_eq!(
        state.calendars["work"].events[&LocalId::new("drifted")].remote_id,
        drifted_remote
    );
    assert_eq!(provider.events(&primary()).len(), 3);
}

/// A fake that fails `update_event`/`delete_event` for chosen remote
/// ids — provider-side errors mid-apply (transient vs conflict).
struct FailingProvider {
    inner: FakeCalendarProvider,
    fail_updates_on: Vec<RemoteId>,
}

impl FailingProvider {
    fn new(inner: FakeCalendarProvider, fail_updates_on: Vec<RemoteId>) -> Self {
        Self {
            inner,
            fail_updates_on,
        }
    }
}

#[async_trait::async_trait]
impl CalendarProvider for FailingProvider {
    fn capabilities(&self) -> cicada_core::domain::capabilities::ProviderCapabilities {
        self.inner.capabilities()
    }
    async fn list_events(
        &self,
        c: &CalendarRef,
        r: Option<cicada_core::domain::time::TimeRange>,
    ) -> Result<Vec<RemoteEvent>> {
        self.inner.list_events(c, r).await
    }
    async fn get_event(&self, c: &CalendarRef, id: &RemoteId) -> Result<Option<RemoteEvent>> {
        self.inner.get_event(c, id).await
    }
    async fn find_by_tag(&self, c: &CalendarRef, id: &LocalId) -> Result<Option<RemoteEvent>> {
        self.inner.find_by_tag(c, id).await
    }
    async fn create_event(
        &self,
        c: &CalendarRef,
        e: &EventBody,
        id: &LocalId,
        o: &WriteOptions,
    ) -> Result<RemoteEvent> {
        self.inner.create_event(c, e, id, o).await
    }
    async fn update_event(
        &self,
        c: &CalendarRef,
        id: &RemoteId,
        e: &EventBody,
        o: &WriteOptions,
    ) -> Result<RemoteEvent> {
        if self.fail_updates_on.contains(id) {
            return Err(CicadaError::transient("provider 503 — try again"));
        }
        self.inner.update_event(c, id, e, o).await
    }
    async fn delete_event(&self, c: &CalendarRef, id: &RemoteId, o: &WriteOptions) -> Result<()> {
        self.inner.delete_event(c, id, o).await
    }
    async fn free_busy(
        &self,
        c: &CalendarRef,
        r: cicada_core::domain::time::TimeRange,
    ) -> Result<Vec<cicada_core::domain::time::TimeRange>> {
        self.inner.free_busy(c, r).await
    }
}

#[tokio::test]
async fn provider_error_is_journaled_with_its_kind() {
    let provider = FakeCalendarProvider::new();
    let store = MemoryStateStore::new();
    let clock = clock();

    let v1 = calendar(vec![Event::new("flaky", body("Old"))]);
    let p1 = plan(&v1, &provider, &store, &clock).await;
    apply(&v1, &p1, &provider, &store, &clock, false, false).await;
    let remote_id = store.snapshot().calendars["work"].events[&LocalId::new("flaky")]
        .remote_id
        .clone();

    // Wrap the provider so the update write 503s.
    let failing = FailingProvider::new(provider, vec![remote_id.clone()]);
    let v2 = calendar(vec![Event::new("flaky", body("New"))]);
    // Plan against the real provider (live is fine — no drift).
    let p2 = plan(&v2, &failing, &store, &clock).await;
    assert!(p2.drift.is_empty());

    let r = apply(&v2, &p2, &failing, &store, &clock, false, false).await;
    assert_eq!(r.failed(), 1);
    let last = store.journal().await.unwrap().last().unwrap().clone();
    let JournalOutcome::Failed { error_kind, error } = &last.outcome else {
        panic!("expected Failed entry");
    };
    assert_eq!(*error_kind, cicada_core::ErrorKind::Transient);
    assert!(error.contains("503"));
    // Recorded state still points at the old applied event — no
    // corruption from the failed write.
    assert_eq!(
        store.snapshot().calendars["work"].events[&LocalId::new("flaky")].remote_id,
        remote_id
    );
}

// ---------- drift + refresh ----------

async fn drift_now(provider: &dyn CalendarProvider, store: &MemoryStateStore) -> Vec<DriftEntry> {
    let state = store.load().await.unwrap();
    let recorded = state.calendar("work").cloned().unwrap_or_default();
    DetectDrift::new()
        .run(&DriftRequest {
            calendar_name: "work",
            recorded: &recorded,
            provider,
            remote_calendar: &primary(),
        })
        .await
        .unwrap()
}

#[tokio::test]
async fn drift_reports_missing_and_modified_and_ignores_foreign() {
    let provider = FakeCalendarProvider::new();
    let store = MemoryStateStore::new();
    let clock = clock();

    let v1 = calendar(vec![Event::new("a", body("A")), Event::new("b", body("B"))]);
    let p1 = plan(&v1, &provider, &store, &clock).await;
    apply(&v1, &p1, &provider, &store, &clock, false, false).await;

    let snapshot = store.snapshot();
    let a_remote = snapshot.calendars["work"].events[&LocalId::new("a")]
        .remote_id
        .clone();
    let b_remote = snapshot.calendars["work"].events[&LocalId::new("b")]
        .remote_id
        .clone();

    provider.simulate_remote_edit(&primary(), &a_remote, |b| {
        b.summary = "A (edited)".into();
    });
    provider.simulate_remote_delete(&primary(), &b_remote);
    provider.seed_foreign(&primary(), body("Human lunch"));

    let drift = drift_now(&provider, &store).await;
    assert_eq!(drift.len(), 2);
    let kinds: Vec<DriftKind> = drift.iter().map(DriftEntry::kind).collect();
    assert!(
        kinds
            .iter()
            .any(|k| matches!(k, DriftKind::Modified { .. }))
    );
    assert!(kinds.contains(&DriftKind::Missing));
    // The foreign event produced no entry.
    assert!(drift.iter().all(|d| d.local_id().as_str() != "human-lunch"));
}

#[tokio::test]
async fn refresh_reconciles_drift_into_state() {
    let provider = FakeCalendarProvider::new();
    let store = MemoryStateStore::new();
    let clock = clock();

    let v1 = calendar(vec![
        Event::new("a", body("A")),
        Event::new("b", body("B")),
        Event::new("c", body("C")),
    ]);
    let p1 = plan(&v1, &provider, &store, &clock).await;
    apply(&v1, &p1, &provider, &store, &clock, false, false).await;

    let snapshot = store.snapshot();
    let a_remote = snapshot.calendars["work"].events[&LocalId::new("a")]
        .remote_id
        .clone();
    let b_remote = snapshot.calendars["work"].events[&LocalId::new("b")]
        .remote_id
        .clone();
    provider.simulate_remote_edit(&primary(), &a_remote, |b| {
        b.summary = "A (edited)".into();
    });
    provider.simulate_remote_delete(&primary(), &b_remote);
    // A cicada-tagged live event recorded state doesn't know (e.g.
    // another repo manages it) — adoption candidate, not adopted.
    provider.seed_managed(&primary(), &LocalId::new("orphan"), body("Orphan"));

    let report = RefreshState::new()
        .run(&RefreshRequest {
            calendar_name: "work",
            provider: &provider,
            remote_calendar: &primary(),
            state: &store,
            clock: &clock,
        })
        .await
        .unwrap();

    assert_eq!(report.reconciled.len(), 2);
    assert_eq!(report.updated, vec![LocalId::new("a")]);
    assert_eq!(report.removed, vec![LocalId::new("b")]);
    assert_eq!(report.unchanged, 1); // c was in sync
    assert_eq!(report.adoption_candidates, vec![LocalId::new("orphan")]);

    // Recorded now mirrors live: `a` carries the live hash/etag,
    // `b` is gone, `orphan` was NOT adopted.
    let state = store.snapshot();
    let work = &state.calendars["work"];
    let a = &work.events[&LocalId::new("a")];
    assert_eq!(a.remote_id, a_remote);
    let live_a = provider
        .get_event(&primary(), &a_remote)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(a.etag, live_a.etag);
    assert_eq!(a.hash, Event::new("a", live_a.body).content_hash().unwrap());
    assert!(!work.events.contains_key(&"b".into()));
    assert!(!work.events.contains_key(&"orphan".into()));

    // Post-refresh: drift is clean, and the next plan proposes
    // exactly the repairs — `a` back to desired, `b` recreated.
    assert!(drift_now(&provider, &store).await.is_empty());
    let p2 = plan(&v1, &provider, &store, &clock).await;
    let kinds: Vec<(OpKind, String)> = p2
        .ops
        .iter()
        .map(|op| (op.kind(), op.local_id().as_str().to_owned()))
        .collect();
    assert!(kinds.contains(&(OpKind::Update, "a".into())));
    assert!(kinds.contains(&(OpKind::Create, "b".into())));
    assert!(kinds.contains(&(OpKind::NoOp, "c".into())));
}

#[tokio::test]
async fn refresh_unknown_calendar_errors() {
    let provider = FakeCalendarProvider::new();
    let store = MemoryStateStore::new();
    let clock = clock();
    let err = RefreshState::new()
        .run(&RefreshRequest {
            calendar_name: "work",
            provider: &provider,
            remote_calendar: &primary(),
            state: &store,
            clock: &clock,
        })
        .await
        .unwrap_err();
    assert_eq!(err.kind(), cicada_core::ErrorKind::Fatal);
}
