//! `ApplyPlan` — execute a [`Plan`]'s ops against the provider,
//! journaling every landed op into recorded state (ADR-0001 §4,
//! ADR-0002 §3/§4).
//!
//! The semantics the caller wires around it:
//!
//! - A plan with error-severity findings is refused outright —
//!   `plan.has_errors()`.
//! - Ops run in plan order (`Delete → Replace → Create → Update`);
//!   each op carries `calendarId + localId + opKind + contentHash` as
//!   its idempotency key (forwarded in [`WriteOptions`]).
//! - A `conflict`ed op (plan-time drift, ADR-0002 §4) fails with a
//!   `Conflict` error unless `force` — under `force` the recorded
//!   etag is dropped so the write lands unconditionally. A drifted
//!   `NoOp` under `force` rewrites live to restore desired.
//! - `--resume`: ops already journaled as applied are skipped;
//!   `Create`/`Replace` additionally reconcile by `cicada:id` tag —
//!   a live event already carrying the id (and, for `Replace`, the
//!   desired hash) means the op landed but the journal write
//!   crashed, so it is journaled `SkippedAlreadyApplied`, not
//!   re-executed. Tag reconciliation runs on every apply, not only
//!   resume: a tagged live event under an unrecorded local id is
//!   always the ambiguity ADR-0002 §3 names.
//! - A failed op is journaled `Failed` with its error kind and does
//!   not abort the apply — the report carries every outcome.
//! - State is saved once per op (calendars + journal in one atomic
//!   document), so a crash mid-apply leaves a valid, resumable file.

use std::collections::BTreeSet;

use jiff::Timestamp;

use crate::domain::calendar::Calendar;
use crate::domain::error::{CicadaError, Result};
use crate::domain::ids::{ContentHash, LocalId, RemoteId};
use crate::domain::operation::{OpKind, Operation};
use crate::domain::policy::{SendUpdates, WriteOptions};
use crate::domain::state::{JournalEntry, JournalOutcome, RecordedCalendar, RecordedState};
use crate::planning::plan::Plan;
use crate::planning::planner::live_content_hash;
use crate::ports::{CalendarProvider, CalendarRef, Clock, RemoteEvent, StateStore};

/// Everything [`ApplyPlan::run`] needs for one logical calendar.
pub struct ApplyRequest<'a> {
    /// The plan to execute — produced by `PlanCalendar` moments ago
    /// (conflict markers are plan-time drift checks, ADR-0002 §4).
    pub plan: &'a Plan,
    /// The desired calendar the plan was computed from — needed to
    /// rewrite a drifted `NoOp` under `--force`.
    pub desired: &'a Calendar,
    /// Provider to write through.
    pub provider: &'a dyn CalendarProvider,
    /// Recorded-state store — journaled per op. The caller takes the
    /// advisory lock first; locking is adapter plumbing (see
    /// `ports::state_store`).
    pub state: &'a dyn StateStore,
    /// Remote calendar this logical calendar binds to (ADR-0002 §9).
    pub remote_calendar: &'a CalendarRef,
    /// Adapter name recorded into the state file (`fake`, `google`).
    pub provider_name: &'a str,
    /// Notification policy for every write (ADR-0002 §5).
    pub send_updates: SendUpdates,
    /// Overwrite out-of-band edits instead of failing on conflict.
    pub force: bool,
    /// Skip ops already journaled as applied.
    pub resume: bool,
    /// Clock for `appliedAt` / journal timestamps.
    pub clock: &'a dyn Clock,
}

/// What happened to one plan op.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpOutcome {
    /// The op kind.
    pub op: OpKind,
    /// Local id reconciled.
    pub local_id: LocalId,
    /// Idempotency key (`calendarId|localId|opKind|hash`).
    pub key: String,
    /// How it went.
    pub result: OpResult,
}

/// Per-op result — mirrors [`JournalOutcome`], plus the
/// non-journaled skips.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OpResult {
    /// The remote write landed.
    Applied {
        /// The provider-assigned/confirmed remote id.
        remote_id: Option<RemoteId>,
    },
    /// The provider already had it (tag reconcile, ADR-0002 §3) —
    /// journaled `SkippedAlreadyApplied`, adopted into state.
    AlreadyApplied {
        /// The remote id found by tag lookup.
        remote_id: Option<RemoteId>,
    },
    /// `--resume` saw a successful journal entry for this key — the
    /// op was not re-executed.
    SkippedResume,
    /// Nothing to do (`NoOp`, unconflicted).
    Noop,
    /// The op failed — journaled with the error kind; apply
    /// continued with the next op.
    Failed(CicadaError),
}

/// The apply's full report — per-op outcomes in plan order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApplyReport {
    /// Logical calendar the plan applied to.
    pub calendar: String,
    /// One outcome per plan op.
    pub outcomes: Vec<OpOutcome>,
}

impl ApplyReport {
    /// `true` when any op failed — the CLI exits non-zero.
    pub fn has_failures(&self) -> bool {
        self.outcomes
            .iter()
            .any(|o| matches!(o.result, OpResult::Failed(_)))
    }

    /// Count of outcomes matching `pred`.
    fn count(&self, pred: impl Fn(&OpResult) -> bool) -> usize {
        self.outcomes.iter().filter(|o| pred(&o.result)).count()
    }

    /// Remote writes that landed (excludes tag-adopted).
    pub fn applied(&self) -> usize {
        self.count(|r| matches!(r, OpResult::Applied { .. }))
    }

    /// Ops needing no remote write (adopted, resume-skipped, noop).
    pub fn skipped(&self) -> usize {
        self.count(|r| {
            matches!(
                r,
                OpResult::AlreadyApplied { .. } | OpResult::SkippedResume | OpResult::Noop
            )
        })
    }

    /// Ops that failed.
    pub fn failed(&self) -> usize {
        self.count(|r| matches!(r, OpResult::Failed(_)))
    }
}

/// `ApplyPlan` — stateless use case; the caller holds the state
/// lock for the whole run.
#[derive(Debug, Clone, Copy, Default)]
pub struct ApplyPlan;

impl ApplyPlan {
    /// A ready-to-use use case.
    pub fn new() -> Self {
        Self
    }

    /// Execute `req.plan`, journaling per op into `req.state`.
    ///
    /// # Errors
    /// [`CicadaError::Fatal`] when the plan carries error-severity
    /// findings (refused whole, before any remote call); whatever
    /// `load`/`save` on the store or `find_by_tag` on the provider
    /// returns — persistence and lookup failures are fatal to the
    /// apply. Per-op *write* failures are journaled and reported,
    /// not returned.
    pub async fn run(&self, req: &ApplyRequest<'_>) -> Result<ApplyReport> {
        if req.plan.has_errors() {
            return Err(CicadaError::fatal(
                "plan contains error-severity findings — refusing to apply",
            ));
        }

        let mut state = req.state.load().await?;
        let applied_keys: BTreeSet<String> = if req.resume {
            state
                .journal
                .iter()
                .filter(|e| {
                    matches!(
                        e.outcome,
                        JournalOutcome::Applied { .. }
                            | JournalOutcome::SkippedAlreadyApplied { .. }
                    )
                })
                .map(|e| e.key.clone())
                .collect()
        } else {
            BTreeSet::new()
        };

        // The recorded slice learns the binding up front so even a
        // first apply persists provider/calendarId (ADR-0002 §9).
        {
            let rec = state.calendar_mut(&req.plan.calendar);
            if rec.provider.is_empty() {
                req.provider_name.clone_into(&mut rec.provider);
            }
            if rec.calendar_id.is_empty() {
                req.remote_calendar
                    .as_str()
                    .clone_into(&mut rec.calendar_id);
            }
        }

        let mut report = ApplyReport {
            calendar: req.plan.calendar.clone(),
            outcomes: Vec::new(),
        };

        for op in &req.plan.ops {
            let key = op.idempotency_key(req.remote_calendar.as_str());
            let local_id = op.local_id().clone();
            let result = self
                .apply_one(req, &mut state, op, &key, &local_id, &applied_keys)
                .await?;
            report.outcomes.push(OpOutcome {
                op: op.kind(),
                local_id,
                key,
                result,
            });
        }

        Ok(report)
    }

    /// One op: noop short-circuit → resume gate → conflict gate →
    /// remote call → record + journal + save. Provider *write*
    /// errors are captured as [`OpResult::Failed`] (journaled, not
    /// propagated); store/lookup errors propagate as fatal to the
    /// apply.
    async fn apply_one(
        &self,
        req: &ApplyRequest<'_>,
        state: &mut RecordedState,
        op: &Operation,
        key: &str,
        local_id: &LocalId,
        applied_keys: &BTreeSet<String>,
    ) -> Result<OpResult> {
        // An unconflicted NoOp needs no remote call and no journal
        // line. A conflicted NoOp runs the full pipeline: refused
        // without --force, rewritten under it.
        if matches!(op, Operation::NoOp { .. }) && op.conflict().is_none() {
            return Ok(OpResult::Noop);
        }

        if req.resume && applied_keys.contains(key) {
            return Ok(OpResult::SkippedResume);
        }

        // Conflict gate (ADR-0002 §4): a drifted op fails without
        // --force. Journaled honestly so resume/drift can see it.
        if !req.force {
            if let Some(kind) = op.conflict() {
                let err = CicadaError::conflict(format!(
                    "drift conflict on {local_id} (live is {}) — run `cicada refresh` \
                     and re-plan, or apply --force",
                    kind.code()
                ));
                self.push_journal(
                    req,
                    state,
                    op,
                    key,
                    local_id,
                    JournalOutcome::Failed {
                        error: err.to_string(),
                        error_kind: err.kind(),
                    },
                )
                .await?;
                return Ok(OpResult::Failed(err));
            }
        }

        let opts = WriteOptions {
            send_updates: req.send_updates,
            idempotency_key: Some(key.to_owned()),
            // Under --force the recorded etag would reject the very
            // overwrite the flag asks for — drop it.
            etag: if req.force { None } else { op_etag(op) },
        };

        match self.execute(req, op, local_id, &opts).await {
            Ok(executed) => {
                let now = req.clock.now();
                let rec = state.calendar_mut(&req.plan.calendar);
                let (outcome, result) = match executed {
                    Executed::Landed { remote, hash } => {
                        record_into(rec, local_id, &remote, hash, now);
                        let remote_id = Some(remote.remote_id.clone());
                        (
                            JournalOutcome::Applied {
                                remote_id: remote_id.clone(),
                            },
                            OpResult::Applied { remote_id },
                        )
                    }
                    Executed::Deleted { remote_id } => {
                        rec.forget(local_id);
                        (
                            JournalOutcome::Applied {
                                remote_id: Some(remote_id.clone()),
                            },
                            OpResult::Applied {
                                remote_id: Some(remote_id),
                            },
                        )
                    }
                    Executed::Adopted { remote } => {
                        adopt_into(rec, local_id, &remote, now);
                        let remote_id = Some(remote.remote_id.clone());
                        (
                            JournalOutcome::SkippedAlreadyApplied {
                                remote_id: remote_id.clone(),
                            },
                            OpResult::AlreadyApplied { remote_id },
                        )
                    }
                };
                self.push_journal(req, state, op, key, local_id, outcome)
                    .await?;
                Ok(result)
            }
            Err(err) => {
                self.push_journal(
                    req,
                    state,
                    op,
                    key,
                    local_id,
                    JournalOutcome::Failed {
                        error: err.to_string(),
                        error_kind: err.kind(),
                    },
                )
                .await?;
                Ok(OpResult::Failed(err))
            }
        }
    }

    /// The remote work for one op — create/update/delete/replace,
    /// tag reconciliation for `Create`/`Replace`, and the
    /// force-rewrite of a drifted `NoOp`.
    async fn execute(
        &self,
        req: &ApplyRequest<'_>,
        op: &Operation,
        local_id: &LocalId,
        opts: &WriteOptions,
    ) -> Result<Executed> {
        match op {
            Operation::Create { desired, .. } => {
                // ADR-0002 §3: a live event already carrying our
                // cicada:id means this create landed without its
                // journal write — adopt it instead of duplicating.
                // Runs on every apply, not only resume.
                if let Some(remote) = req
                    .provider
                    .find_by_tag(req.remote_calendar, local_id)
                    .await?
                {
                    return Ok(Executed::Adopted { remote });
                }
                let remote = req
                    .provider
                    .create_event(req.remote_calendar, &desired.body, local_id, opts)
                    .await?;
                Ok(Executed::Landed {
                    hash: op.hash().clone(),
                    remote,
                })
            }
            Operation::Update {
                remote_id, desired, ..
            } => {
                let remote = req
                    .provider
                    .update_event(req.remote_calendar, remote_id, &desired.body, opts)
                    .await?;
                Ok(Executed::Landed {
                    hash: op.hash().clone(),
                    remote,
                })
            }
            Operation::Delete { remote_id, .. } => {
                req.provider
                    .delete_event(req.remote_calendar, remote_id, opts)
                    .await?;
                Ok(Executed::Deleted {
                    remote_id: remote_id.clone(),
                })
            }
            Operation::Replace {
                remote_id, desired, ..
            } => {
                // Resume ambiguity (ADR-0002 §3): the live event
                // carrying our tag with the *desired* hash means the
                // replace already landed; the same tag with the old
                // hash is the event this replace exists to delete.
                if let Some(remote) = req
                    .provider
                    .find_by_tag(req.remote_calendar, local_id)
                    .await?
                {
                    let landed = remote
                        .tags
                        .state_hash
                        .as_ref()
                        .is_some_and(|h| h == op.hash());
                    if landed {
                        return Ok(Executed::Adopted { remote });
                    }
                }
                req.provider
                    .delete_event(req.remote_calendar, remote_id, opts)
                    .await?;
                let remote = req
                    .provider
                    .create_event(req.remote_calendar, &desired.body, local_id, opts)
                    .await?;
                Ok(Executed::Landed {
                    hash: op.hash().clone(),
                    remote,
                })
            }
            Operation::NoOp { remote_id, .. } => {
                // Reached only under --force with a drift conflict:
                // push desired over the out-of-band edit so recorded,
                // live, and desired agree again.
                let desired = req.desired.get(local_id).ok_or_else(|| {
                    CicadaError::fatal(format!(
                        "forced NoOp rewrite needs desired event {local_id}"
                    ))
                })?;
                let remote = req
                    .provider
                    .update_event(req.remote_calendar, remote_id, &desired.body, opts)
                    .await?;
                Ok(Executed::Landed {
                    hash: op.hash().clone(),
                    remote,
                })
            }
        }
    }

    /// Append the journal entry to the working state and save the
    /// whole document — one atomic write per op keeps a crashed
    /// apply resumable.
    async fn push_journal(
        &self,
        req: &ApplyRequest<'_>,
        state: &mut RecordedState,
        op: &Operation,
        key: &str,
        local_id: &LocalId,
        outcome: JournalOutcome,
    ) -> Result<()> {
        state.push_journal(JournalEntry {
            key: key.to_owned(),
            op: op.kind(),
            local_id: local_id.clone(),
            applied_at: req.clock.now(),
            outcome,
        });
        req.state.save(state).await
    }
}

/// The remote side of a successful op.
enum Executed {
    /// A write landed; record `remote` under the planned hash —
    /// desired truth for apply-landed ops.
    Landed {
        /// The hash to record (the op's planned content hash).
        hash: ContentHash,
        /// The provider's post-write view.
        remote: RemoteEvent,
    },
    /// A `Delete` landed; forget the record.
    Deleted {
        /// The deleted remote id.
        remote_id: RemoteId,
    },
    /// Tag reconcile found the op already applied — adopt the live
    /// record (honest hash, not the plan's).
    Adopted {
        /// The live event found by `cicada:id`.
        remote: RemoteEvent,
    },
}

/// The op's recorded etag, for conditional writes.
fn op_etag(op: &Operation) -> Option<String> {
    match op {
        Operation::Update { etag, .. }
        | Operation::Delete { etag, .. }
        | Operation::Replace { etag, .. } => etag.clone(),
        Operation::Create { .. } | Operation::NoOp { .. } => None,
    }
}

/// Record a landed write: remote id + planned hash + fresh etag.
fn record_into(
    rec: &mut RecordedCalendar,
    local_id: &LocalId,
    remote: &RemoteEvent,
    hash: ContentHash,
    now: Timestamp,
) {
    rec.record(local_id.clone(), remote.remote_id.clone(), hash, now);
    if let Some(recorded) = rec.events.get_mut(local_id) {
        recorded.etag.clone_from(&remote.etag);
    }
}

/// Record an adopted event: the remote id cicada didn't create this
/// run plus the *live* truth hash — next plan then diffs desired
/// against what actually exists, not what we wished for.
fn adopt_into(
    rec: &mut RecordedCalendar,
    local_id: &LocalId,
    remote: &RemoteEvent,
    now: Timestamp,
) {
    let hash = live_content_hash(local_id, &remote.body)
        .or_else(|| remote.tags.state_hash.clone())
        .unwrap_or_else(|| ContentHash::new("sha256:unknown"));
    rec.record(local_id.clone(), remote.remote_id.clone(), hash, now);
    if let Some(recorded) = rec.events.get_mut(local_id) {
        recorded.etag.clone_from(&remote.etag);
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    // The full ApplyPlan matrix (provider + store fakes) lives in
    // cicada-testkit — core can't see the fakes.
    use super::*;

    #[test]
    fn report_counts_outcomes() {
        let report = ApplyReport {
            calendar: "work".into(),
            outcomes: vec![
                OpOutcome {
                    op: OpKind::Create,
                    local_id: "a".into(),
                    key: "k1".into(),
                    result: OpResult::Applied {
                        remote_id: Some("r".into()),
                    },
                },
                OpOutcome {
                    op: OpKind::NoOp,
                    local_id: "b".into(),
                    key: "k2".into(),
                    result: OpResult::Noop,
                },
                OpOutcome {
                    op: OpKind::Update,
                    local_id: "c".into(),
                    key: "k3".into(),
                    result: OpResult::Failed(CicadaError::conflict("drift")),
                },
            ],
        };
        assert!(report.has_failures());
        assert_eq!(report.applied(), 1);
        assert_eq!(report.skipped(), 1);
        assert_eq!(report.failed(), 1);
    }

    #[test]
    fn live_hash_matches_event_content_hash() {
        // adopt_into hashes the live body under the local id —
        // Event::content_hash is the same function the fake tags
        // with, keeping the seam honest.
        let body = crate::domain::event::EventBody {
            summary: "X".into(),
            description: None,
            location: None,
            time: crate::domain::event::EventTime::Span {
                start: crate::domain::time::Time::Wall {
                    local: jiff::civil::date(2026, 1, 5).at(9, 0, 0, 0),
                    zone: crate::domain::ids::ZoneId::new_unchecked("UTC"),
                },
                duration: jiff::SignedDuration::from_mins(30),
            },
            recurrence: None,
            attendees: vec![],
            reminders: vec![],
            conferencing: None,
            transparency: crate::domain::event::Transparency::Opaque,
        };
        let live = live_content_hash(&"a".into(), &body).unwrap();
        let planned = crate::domain::event::Event::new("a", body)
            .content_hash()
            .unwrap();
        assert_eq!(live, planned);
    }
}
