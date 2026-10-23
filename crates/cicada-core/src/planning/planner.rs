//! `OperationPlanner` — desired-vs-recorded-vs-live reconciliation.
//!
//! The managed set is *exactly* the recorded state (ADR-0002 §1):
//! `Delete` ops are generated only for local ids recorded but absent
//! from desired; live events unknown to recorded state are never
//! modified or deleted — a human's lunch on a managed calendar is
//! untouchable, and disjoint cicada sources can share one remote
//! calendar.
//!
//! Op selection (ADR-0001 §3/§5):
//!
//! - desired + unrecorded → `Create`.
//! - recorded, hash equal → `NoOp`.
//! - recorded, hash differs → diff desired vs the *live* body:
//!   a `recurrence` change → `Replace`; anything else → `Update`.
//!
//! Drift (ADR-0002 §4): each managed record is compared with its
//! live counterpart — missing live event → `Missing`, changed live
//! event → `Modified`. Drift lands in the plan's `drift` summary and
//! marks the affected op `conflict`ed; apply fails conflicted ops
//! unless `--force`.
//!
//! Ops are sorted `Delete → Replace → Create → Update → NoOp`
//! ([`sort_operations`]) and carry the content hash they were
//! planned against so [`Operation::idempotency_key`] is stable.

use std::collections::BTreeMap;

use crate::domain::calendar::Calendar;
use crate::domain::capabilities::ProviderCapabilities;
use crate::domain::drift::{DriftEntry, DriftKind};
use crate::domain::error::Result;
use crate::domain::event::{Event, EventBody};
use crate::domain::ids::{ContentHash, LocalId};
use crate::domain::operation::{EventDiff, EventField, Operation, sort_operations};
use crate::domain::policy::SendUpdates;
use crate::domain::state::{RecordedCalendar, RecordedEvent};
use crate::planning::capability::{CapabilityChecker, CapabilityIssue};
use crate::planning::differ::EventDiffer;
use crate::planning::plan::{Plan, PlanWarning};
use crate::ports::{Clock, RemoteEvent};

/// Everything the planner needs for one logical calendar. Pure
/// data — the use case (`PlanCalendar`) fetches the live slice and
/// capabilities through the provider port.
pub struct PlanInput<'a> {
    /// Desired state (parsed YAML).
    pub desired: &'a Calendar,
    /// Recorded slice for the calendar; `None` = never applied.
    pub recorded: Option<&'a RecordedCalendar>,
    /// Live events on the bound remote calendar — *includes
    /// unmanaged events*; the planner never generates ops for them
    /// (ADR-0002 §1).
    pub live: &'a [RemoteEvent],
    /// What the provider can do — layer-3 validation.
    pub capabilities: &'a ProviderCapabilities,
    /// Notification policy (`spec.apply.sendUpdates`; default
    /// `ExternalOnly`, ADR-0002 §5).
    pub send_updates: SendUpdates,
    /// Clock for `generatedAt` on the plan document.
    pub clock: &'a dyn Clock,
}

/// Reconciles desired calendars against recorded state into ordered
/// plans. Pure and stateless.
#[derive(Debug, Clone, Copy, Default)]
pub struct OperationPlanner;

impl OperationPlanner {
    /// Compute the plan for `input.desired` against recorded state
    /// and the live provider slice.
    ///
    /// # Errors
    /// [`CicadaError::Fatal`] only for unhashable/unrenderable event
    /// content — never for capability gaps (those land in
    /// `plan.warnings`).
    ///
    /// [`CicadaError::Fatal`]: crate::domain::error::CicadaError::Fatal
    pub fn plan(&self, input: &PlanInput<'_>) -> Result<Plan> {
        let desired = input.desired;
        let mut plan = Plan::new(desired.name.clone(), input.clock.now());

        let checker = CapabilityChecker;
        if let Some(issue) = checker.check_send_updates(input.send_updates, input.capabilities) {
            plan.warnings.push(plan_warning(issue));
        }

        // Live indexes: the recorded boundary looks events up by
        // remote id; the ADR-0002 §3 resume ambiguity is detected by
        // the `cicada:id` tag.
        let live_by_remote: BTreeMap<&str, &RemoteEvent> = input
            .live
            .iter()
            .map(|e| (e.remote_id.as_str(), e))
            .collect();
        let live_by_tag: BTreeMap<&str, &RemoteEvent> = input
            .live
            .iter()
            .filter_map(|e| e.tags.local_id.as_ref().map(|id| (id.as_str(), e)))
            .collect();

        let mut drift: BTreeMap<&LocalId, DriftEntry> = BTreeMap::new();

        for (local_id, event) in &desired.events {
            for issue in checker.check_event(event, input.capabilities) {
                plan.warnings.push(plan_warning(issue));
            }
            let hash = event.content_hash()?;
            let Some(rec) = input.recorded.and_then(|r| r.events.get(local_id)) else {
                // Never applied → Create. A live event already carrying
                // our tag is the resume ambiguity of ADR-0002 §3 —
                // warn so the user isn't surprised when apply adopts
                // it instead of duplicating.
                if live_by_tag.contains_key(local_id.as_str()) {
                    plan.warnings.push(PlanWarning::warning(
                        Some(local_id.as_str().to_owned()),
                        "a live event already carries the cicada:id tag — \
                         apply-time resume reconciliation will adopt it \
                         instead of creating a duplicate (ADR-0002 §3)",
                    ));
                }
                plan.ops.push(Operation::Create {
                    desired: Box::new(event.clone()),
                    hash,
                    conflict: None,
                });
                continue;
            };

            let live = live_by_remote.get(rec.remote_id.as_str()).copied();
            let drift_kind = detect_drift(local_id, rec, live);
            if let Some(kind) = &drift_kind {
                drift.insert(
                    local_id,
                    DriftEntry::new(local_id.clone(), rec.remote_id.clone(), kind.clone()),
                );
            }

            let mut op = if rec.hash == hash {
                Operation::NoOp {
                    local_id: local_id.clone(),
                    remote_id: rec.remote_id.clone(),
                    hash,
                    conflict: None,
                }
            } else {
                let live_body = live.map(|e| &e.body);
                let diff = match live_body {
                    Some(actual) => EventDiffer.diff_events(event, actual)?,
                    // Live is gone — nothing to diff against; the
                    // Missing conflict explains the op's shape.
                    None => EventDiff::default(),
                };
                // A recurrence *rule* change forces Replace — instances
                // diverge (ADR-0001 §5). When the live event is gone the
                // diff can't tell Update from Replace; Replace is the
                // conservative choice since an Update can never repair
                // a rule change but a Replace always rebuilds desired.
                if live_body.is_none()
                    || diff
                        .changes
                        .iter()
                        .any(|c| c.field == EventField::Recurrence)
                {
                    Operation::Replace {
                        local_id: local_id.clone(),
                        remote_id: rec.remote_id.clone(),
                        desired: Box::new(event.clone()),
                        diff,
                        hash,
                        etag: rec.etag.clone(),
                        conflict: None,
                    }
                } else {
                    Operation::Update {
                        local_id: local_id.clone(),
                        remote_id: rec.remote_id.clone(),
                        desired: Box::new(event.clone()),
                        diff,
                        hash,
                        etag: rec.etag.clone(),
                        conflict: None,
                    }
                }
            };
            if let Some(kind) = drift_kind {
                op.set_conflict(kind);
            }
            plan.ops.push(op);
        }

        // Deletes: recorded local ids absent from desired — and only
        // those (ADR-0002 §1).
        if let Some(rec) = input.recorded {
            for (local_id, rec_ev) in &rec.events {
                if desired.events.contains_key(local_id) {
                    continue;
                }
                let live = live_by_remote.get(rec_ev.remote_id.as_str()).copied();
                let drift_kind = detect_drift(local_id, rec_ev, live);
                if let Some(kind) = &drift_kind {
                    drift.insert(
                        local_id,
                        DriftEntry::new(local_id.clone(), rec_ev.remote_id.clone(), kind.clone()),
                    );
                }
                plan.ops.push(Operation::Delete {
                    local_id: local_id.clone(),
                    remote_id: rec_ev.remote_id.clone(),
                    hash: rec_ev.hash.clone(),
                    etag: rec_ev.etag.clone(),
                    conflict: drift_kind,
                });
            }
        }

        plan.drift = drift.into_values().collect();
        sort_operations(&mut plan.ops);
        Ok(plan)
    }
}

/// A `CapabilityIssue` rendered as a plan warning (severity
/// preserved — errors block apply).
fn plan_warning(issue: CapabilityIssue) -> PlanWarning {
    PlanWarning {
        severity: issue.severity,
        local_id: issue.local_id,
        message: issue.message,
    }
}

/// Compare one managed record with its live counterpart (ADR-0002
/// §4). `None` means recorded == live (no drift).
fn detect_drift(
    local_id: &LocalId,
    rec: &RecordedEvent,
    live: Option<&RemoteEvent>,
) -> Option<DriftKind> {
    let Some(live) = live else {
        return Some(DriftKind::Missing);
    };
    // ETags are authoritative where both sides expose one — a bump
    // means the live event changed even if the content hash would
    // still line up (providers mutate fields we don't model).
    if let (Some(recorded), Some(current)) = (&rec.etag, &live.etag) {
        return (recorded != current).then(|| DriftKind::Modified {
            recorded_hash: rec.hash.clone(),
            live_hash: live_content_hash(local_id, &live.body),
        });
    }
    let live_hash = live_content_hash(local_id, &live.body);
    // A `cicada:state` tag that disagrees with the record means the
    // remote tags were rewritten out of band (or state was rebuilt
    // against a different source).
    let tag_mismatch = live
        .tags
        .state_hash
        .as_ref()
        .is_some_and(|h| *h != rec.hash);
    if live_hash.as_ref() != Some(&rec.hash) || tag_mismatch {
        return Some(DriftKind::Modified {
            recorded_hash: rec.hash.clone(),
            live_hash,
        });
    }
    None
}

/// Recompute the content hash of a live body *as the event the
/// record describes* — the recorded hash includes the local id
/// ([`Event::content_hash`]), so the live body is hashed under the
/// same id.
fn live_content_hash(local_id: &LocalId, body: &EventBody) -> Option<ContentHash> {
    Event::new(local_id.clone(), body.clone())
        .content_hash()
        .ok()
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use jiff::SignedDuration;
    use jiff::civil::date;

    use super::*;
    use crate::domain::capabilities::ProviderCapabilities;
    use crate::domain::event::{EventTime, Transparency};
    use crate::domain::ids::ZoneId;
    use crate::domain::operation::OpKind;
    use crate::domain::recurrence::{Frequency, RecurrenceRule, StructuredRule};
    use crate::domain::time::Time;
    use crate::planning::capability::Severity;
    use crate::ports::EventTags;

    /// Deterministic clock for the plan's `generatedAt`.
    struct FixedClock;

    impl Clock for FixedClock {
        fn now(&self) -> jiff::Timestamp {
            "2026-10-23T01:00:00Z".parse().unwrap()
        }
    }

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

    fn event(id: &str, body: EventBody) -> Event {
        Event::new(id, body)
    }

    fn calendar(events: Vec<Event>) -> Calendar {
        let mut cal = Calendar::new("work", "UTC").unwrap();
        for e in events {
            cal.insert(e).unwrap();
        }
        cal
    }

    /// A recorded entry for `event` — the hash an apply would have
    /// written (`Event::content_hash`).
    fn record_for(event: &Event) -> (RecordedEvent, ContentHash) {
        let hash = event.content_hash().unwrap();
        (
            RecordedEvent {
                remote_id: format!("remote-{}", event.id.as_str()).into(),
                hash: hash.clone(),
                applied_at: "2026-01-01T00:00:00Z".parse().unwrap(),
                etag: Some("etag-1".into()),
            },
            hash,
        )
    }

    /// The live event as it looked right after `event` was applied:
    /// matching tags, matching etag, matching body.
    fn live_for(event: &Event, rec: &RecordedEvent) -> RemoteEvent {
        RemoteEvent {
            remote_id: rec.remote_id.clone(),
            tags: EventTags {
                local_id: Some(event.id.clone()),
                managed: true,
                state_hash: Some(rec.hash.clone()),
            },
            etag: rec.etag.clone(),
            updated: None,
            body: event.body.clone(),
        }
    }

    fn input<'a>(
        desired: &'a Calendar,
        recorded: Option<&'a RecordedCalendar>,
        live: &'a [RemoteEvent],
        caps: &'a ProviderCapabilities,
    ) -> PlanInput<'a> {
        PlanInput {
            desired,
            recorded,
            live,
            capabilities: caps,
            send_updates: SendUpdates::default(),
            clock: &FixedClock,
        }
    }

    fn kinds(plan: &Plan) -> Vec<(OpKind, String)> {
        plan.ops
            .iter()
            .map(|op| (op.kind(), op.local_id().as_str().to_owned()))
            .collect()
    }

    #[test]
    fn fresh_apply_is_all_create() {
        let cal = calendar(vec![event("a", body("A")), event("b", body("B"))]);
        let caps = ProviderCapabilities::permissive();
        let plan = OperationPlanner
            .plan(&input(&cal, None, &[], &caps))
            .unwrap();
        assert_eq!(
            kinds(&plan),
            [(OpKind::Create, "a".into()), (OpKind::Create, "b".into())]
        );
        assert!(plan.drift.is_empty());
        assert!(plan.warnings.is_empty());
        assert_eq!(plan.api_version, "cicada/plan-v1");
    }

    #[test]
    fn unchanged_desired_is_noop() {
        let ev = event("standup", body("Standup"));
        let (rec, _) = record_for(&ev);
        let mut recorded = RecordedCalendar::default();
        recorded.events.insert("standup".into(), rec.clone());
        let live = vec![live_for(&ev, &rec)];
        let cal = calendar(vec![ev]);
        let caps = ProviderCapabilities::permissive();
        let plan = OperationPlanner
            .plan(&input(&cal, Some(&recorded), &live, &caps))
            .unwrap();
        assert_eq!(kinds(&plan), [(OpKind::NoOp, "standup".into())]);
        assert!(plan.drift.is_empty());
    }

    #[test]
    fn summary_change_is_update_with_diff() {
        let applied = event("standup", body("Standup"));
        let (rec, _) = record_for(&applied);
        let mut recorded = RecordedCalendar::default();
        recorded.events.insert("standup".into(), rec.clone());
        let live = vec![live_for(&applied, &rec)];

        let desired = event("standup", body("Daily Standup"));
        let cal = calendar(vec![desired]);
        let caps = ProviderCapabilities::permissive();
        let plan = OperationPlanner
            .plan(&input(&cal, Some(&recorded), &live, &caps))
            .unwrap();
        let [op] = &plan.ops[..] else {
            panic!("one op expected");
        };
        assert_eq!(op.kind(), OpKind::Update);
        let Operation::Update { diff, etag, .. } = op else {
            panic!("update expected");
        };
        assert_eq!(diff.changes.len(), 1);
        assert_eq!(diff.changes[0].field, EventField::Summary);
        assert_eq!(diff.changes[0].to.as_deref(), Some("Daily Standup"));
        assert_eq!(etag.as_deref(), Some("etag-1"));
    }

    #[test]
    fn recurrence_rule_change_is_replace() {
        let mut applied_body = body("Standup");
        applied_body.recurrence = Some(RecurrenceRule::Structured(StructuredRule::new(
            Frequency::Weekly,
        )));
        let applied = event("standup", applied_body);
        let (rec, _) = record_for(&applied);
        let mut recorded = RecordedCalendar::default();
        recorded.events.insert("standup".into(), rec.clone());
        let live = vec![live_for(&applied, &rec)];

        let mut desired_body = body("Standup");
        desired_body.recurrence = Some(RecurrenceRule::Structured(StructuredRule::new(
            Frequency::Daily,
        )));
        let cal = calendar(vec![event("standup", desired_body)]);
        let caps = ProviderCapabilities::permissive();
        let plan = OperationPlanner
            .plan(&input(&cal, Some(&recorded), &live, &caps))
            .unwrap();
        let [op] = &plan.ops[..] else {
            panic!("one op expected");
        };
        assert_eq!(op.kind(), OpKind::Replace);
        let Operation::Replace { diff, .. } = op else {
            panic!("replace expected");
        };
        assert!(
            diff.changes
                .iter()
                .any(|c| c.field == EventField::Recurrence)
        );
    }

    #[test]
    fn removed_from_desired_is_delete() {
        let applied = event("old", body("Old"));
        let (rec, _) = record_for(&applied);
        let mut recorded = RecordedCalendar::default();
        recorded.events.insert("old".into(), rec.clone());
        let live = vec![live_for(&applied, &rec)];

        let cal = calendar(vec![]); // nothing declared
        let caps = ProviderCapabilities::permissive();
        let plan = OperationPlanner
            .plan(&input(&cal, Some(&recorded), &live, &caps))
            .unwrap();
        let [op] = &plan.ops[..] else {
            panic!("one op expected");
        };
        assert_eq!(op.kind(), OpKind::Delete);
        assert_eq!(op.local_id().as_str(), "old");
        assert!(op.conflict().is_none());
    }

    #[test]
    fn unknown_live_events_are_never_touched() {
        // A foreign live event (human's lunch, another source's event)
        // produces no op and no drift.
        let foreign = RemoteEvent {
            remote_id: "foreign-1".into(),
            tags: EventTags::default(),
            etag: Some("etag-f".into()),
            updated: None,
            body: body("Lunch with Sam"),
        };
        let cal = calendar(vec![event("a", body("A"))]);
        let caps = ProviderCapabilities::permissive();
        let plan = OperationPlanner
            .plan(&input(&cal, None, &[foreign], &caps))
            .unwrap();
        assert_eq!(kinds(&plan), [(OpKind::Create, "a".into())]);
        assert!(plan.drift.is_empty());
    }

    #[test]
    fn live_missing_marks_op_conflicted_and_drifts() {
        let applied = event("standup", body("Standup"));
        let (rec, _) = record_for(&applied);
        let mut recorded = RecordedCalendar::default();
        recorded.events.insert("standup".into(), rec.clone());
        let live: Vec<RemoteEvent> = vec![]; // deleted out of band

        let cal = calendar(vec![applied]);
        let caps = ProviderCapabilities::permissive();
        let plan = OperationPlanner
            .plan(&input(&cal, Some(&recorded), &live, &caps))
            .unwrap();
        assert_eq!(plan.drift.len(), 1);
        assert_eq!(plan.drift[0].kind(), DriftKind::Missing);
        let [op] = &plan.ops[..] else {
            panic!("one op expected");
        };
        // desired == recorded (NoOp) but the record is stale.
        assert_eq!(op.kind(), OpKind::NoOp);
        assert_eq!(op.conflict(), Some(&DriftKind::Missing));
    }

    #[test]
    fn live_edit_marks_op_conflicted() {
        let applied = event("standup", body("Standup"));
        let (rec, _) = record_for(&applied);
        let mut recorded = RecordedCalendar::default();
        recorded.events.insert("standup".into(), rec.clone());

        // Out-of-band rename: etag bumped, tags untouched.
        let mut live = live_for(&applied, &rec);
        live.body.summary = "Renamed in the UI".into();
        live.etag = Some("etag-2".into());

        let cal = calendar(vec![applied]);
        let caps = ProviderCapabilities::permissive();
        let plan = OperationPlanner
            .plan(&input(&cal, Some(&recorded), &[live], &caps))
            .unwrap();
        assert_eq!(plan.drift.len(), 1);
        let DriftKind::Modified {
            recorded_hash,
            live_hash,
        } = plan.drift[0].kind()
        else {
            panic!("modified drift expected");
        };
        assert_ne!(Some(&recorded_hash), live_hash.as_ref());
        // desired == recorded → NoOp, but conflicted.
        assert_eq!(plan.ops[0].kind(), OpKind::NoOp);
        assert!(matches!(
            plan.ops[0].conflict(),
            Some(DriftKind::Modified { .. })
        ));
    }

    #[test]
    fn live_edit_detected_without_etags() {
        let applied = event("standup", body("Standup"));
        let (mut rec, _) = record_for(&applied);
        rec.etag = None;
        let mut recorded = RecordedCalendar::default();
        recorded.events.insert("standup".into(), rec.clone());
        let mut live = live_for(&applied, &rec);
        live.etag = None;
        live.body.summary = "Changed".into();
        live.tags.state_hash = Some(rec.hash.clone()); // tag untouched

        let cal = calendar(vec![applied]);
        let caps = ProviderCapabilities::permissive();
        let plan = OperationPlanner
            .plan(&input(&cal, Some(&recorded), &[live], &caps))
            .unwrap();
        assert_eq!(plan.drift.len(), 1);
        assert!(matches!(plan.drift[0].kind(), DriftKind::Modified { .. }));
    }

    #[test]
    fn etag_equality_means_no_drift_even_when_body_differs() {
        // Providers can bump/normalize fields we don't hash; a
        // matching etag is the authoritative "unchanged" signal.
        let applied = event("standup", body("Standup"));
        let (rec, _) = record_for(&applied);
        let mut recorded = RecordedCalendar::default();
        recorded.events.insert("standup".into(), rec.clone());
        let mut live = live_for(&applied, &rec);
        live.body.summary = "Provider-normalized".into();

        let cal = calendar(vec![applied]);
        let caps = ProviderCapabilities::permissive();
        let plan = OperationPlanner
            .plan(&input(&cal, Some(&recorded), &[live], &caps))
            .unwrap();
        assert!(plan.drift.is_empty());
    }

    #[test]
    fn capability_gaps_surface_as_errors() {
        let mut b = body("All-hands");
        b.attendees = vec![
            crate::domain::attendee::Attendee::new("a@x.com"),
            crate::domain::attendee::Attendee::new("b@x.com"),
        ];
        let cal = calendar(vec![event("allhands", b)]);
        let mut caps = ProviderCapabilities::permissive();
        caps.max_attendees = Some(1);
        let plan = OperationPlanner
            .plan(&input(&cal, None, &[], &caps))
            .unwrap();
        assert!(plan.has_errors());
        assert!(plan.warnings.iter().any(|w| {
            w.severity == Severity::Error && w.local_id.as_deref() == Some("allhands")
        }));
        // The op is still produced — the plan is a report; apply
        // refuses error plans.
        assert_eq!(plan.ops.len(), 1);
    }

    #[test]
    fn send_updates_degrades_to_warning() {
        let cal = calendar(vec![event("a", body("A"))]);
        let mut caps = ProviderCapabilities::permissive();
        caps.can_suppress_notifications = false;
        let plan = OperationPlanner
            .plan(&input(&cal, None, &[], &caps))
            .unwrap();
        assert!(!plan.has_errors());
        assert!(
            plan.warnings
                .iter()
                .any(|w| w.message.contains("notifications"))
        );
    }

    #[test]
    fn ops_sort_into_apply_order() {
        // One of each op kind in one plan.
        let gone = event("gone", body("Gone"));
        let (rec_gone, _) = record_for(&gone);
        let replaced_old = {
            let mut b = body("Old");
            b.recurrence = Some(RecurrenceRule::Structured(StructuredRule::new(
                Frequency::Weekly,
            )));
            event("replace-me", b)
        };
        let (rec_repl, _) = record_for(&replaced_old);
        let updated_old = event("update-me", body("Old summary"));
        let (rec_upd, _) = record_for(&updated_old);
        let noop_ev = event("noop", body("Same"));
        let (rec_noop, _) = record_for(&noop_ev);

        let mut recorded = RecordedCalendar::default();
        recorded.events.insert("gone".into(), rec_gone.clone());
        recorded
            .events
            .insert("replace-me".into(), rec_repl.clone());
        recorded.events.insert("update-me".into(), rec_upd.clone());
        recorded.events.insert("noop".into(), rec_noop.clone());

        let mut new_rule = body("New");
        new_rule.recurrence = Some(RecurrenceRule::Structured(StructuredRule::new(
            Frequency::Daily,
        )));
        let cal = calendar(vec![
            event("create-me", body("Fresh")),
            event("replace-me", new_rule),
            event("update-me", body("New summary")),
            event("noop", body("Same")),
        ]);
        let live = vec![
            live_for(&gone, &rec_gone),
            live_for(&replaced_old, &rec_repl),
            live_for(&updated_old, &rec_upd),
            live_for(&noop_ev, &rec_noop),
        ];
        let caps = ProviderCapabilities::permissive();
        let plan = OperationPlanner
            .plan(&input(&cal, Some(&recorded), &live, &caps))
            .unwrap();
        assert_eq!(
            plan.ops.iter().map(Operation::kind).collect::<Vec<_>>(),
            [
                OpKind::Delete,
                OpKind::Replace,
                OpKind::Create,
                OpKind::Update,
                OpKind::NoOp
            ]
        );
    }

    #[test]
    fn idempotency_keys_are_stable_across_runs() {
        let cal = calendar(vec![event("standup", body("Standup"))]);
        let caps = ProviderCapabilities::permissive();
        let p1 = OperationPlanner
            .plan(&input(&cal, None, &[], &caps))
            .unwrap();
        let p2 = OperationPlanner
            .plan(&input(&cal, None, &[], &caps))
            .unwrap();
        assert_eq!(
            p1.ops[0].idempotency_key("primary"),
            p2.ops[0].idempotency_key("primary")
        );
        assert_eq!(
            p1.ops[0].idempotency_key("primary"),
            format!("primary|standup|create|{}", p1.ops[0].hash().as_str())
        );
    }

    #[test]
    fn tagged_live_event_warns_on_create() {
        // Resume ambiguity (ADR-0002 §3): live already carries our
        // cicada:id but recorded state doesn't know it.
        let mut tagged = live_for(
            &event("standup", body("Standup")),
            &RecordedEvent {
                remote_id: "remote-standup".into(),
                hash: "sha256:x".into(),
                applied_at: "2026-01-01T00:00:00Z".parse().unwrap(),
                etag: None,
            },
        );
        tagged.remote_id = "live-9".into();
        let cal = calendar(vec![event("standup", body("Standup"))]);
        let caps = ProviderCapabilities::permissive();
        let plan = OperationPlanner
            .plan(&input(&cal, None, &[tagged], &caps))
            .unwrap();
        assert_eq!(plan.ops[0].kind(), OpKind::Create);
        assert!(
            plan.warnings
                .iter()
                .any(|w| w.local_id.as_deref() == Some("standup"))
        );
    }
}
