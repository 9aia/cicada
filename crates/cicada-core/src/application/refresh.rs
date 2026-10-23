//! `RefreshState` — pull live events for the managed set and fold
//! them into recorded state (`cicada refresh`, ADR-0002 §4).
//!
//! Refresh is how drift gets resolved: every managed local id is
//! looked up on the live provider by its recorded remote id.
//!
//! - Live event found and unchanged → etag resync, otherwise
//!   untouched.
//! - Live event found but changed → the record is updated to the
//!   *live* truth (hash recomputed over the live body, etag, remote
//!   id) — the next plan diffs desired against what actually
//!   exists and proposes the repair op.
//! - Live event gone → the stale record is dropped; the next plan
//!   proposes `Create` for a still-declared event.
//! - Live events carrying `cicada:*` tags but unknown to recorded
//!   state are reported as adoption candidates — reported, never
//!   auto-adopted (ADR-0002 §2). Untagged events are ignored
//!   entirely (managed boundary, §1).

use std::collections::BTreeMap;

use crate::domain::drift::{DriftEntry, DriftKind};
use crate::domain::error::{CicadaError, Result};
use crate::domain::ids::LocalId;
use crate::planning::planner::{detect_drift, live_content_hash};
use crate::ports::{CalendarProvider, CalendarRef, Clock, StateStore};

/// Everything [`RefreshState::run`] needs for one logical calendar.
pub struct RefreshRequest<'a> {
    /// The logical calendar whose recorded slice is refreshed
    /// (`state.calendars` key).
    pub calendar_name: &'a str,
    /// Provider to read live state from.
    pub provider: &'a dyn CalendarProvider,
    /// The remote binding to list — the recorded `calendarId`
    /// (ADR-0002 §9).
    pub remote_calendar: &'a CalendarRef,
    /// Recorded-state store — loaded, reconciled, saved once.
    /// The caller takes the advisory lock first.
    pub state: &'a dyn StateStore,
    /// Clock — refreshed records keep their original `appliedAt`;
    /// the clock is reserved for a `refreshedAt` field if the state
    /// format grows one. Kept in the request for symmetry with the
    /// other use cases.
    pub clock: &'a dyn Clock,
}

/// What a refresh did to one calendar's recorded slice.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RefreshReport {
    /// Logical calendar refreshed.
    pub calendar: String,
    /// Drift found and reconciled (entries in recorded order).
    pub reconciled: Vec<DriftEntry>,
    /// Records updated to live truth (`Modified` drift).
    pub updated: Vec<LocalId>,
    /// Records dropped — live events are gone (`Missing` drift).
    pub removed: Vec<LocalId>,
    /// Tagged live events recorded state doesn't know — `cicada
    /// adopt` candidates, reported only (ADR-0002 §2).
    pub adoption_candidates: Vec<LocalId>,
    /// Records already in sync.
    pub unchanged: usize,
}

/// `RefreshState` — stateless use case.
#[derive(Debug, Clone, Copy, Default)]
pub struct RefreshState;

impl RefreshState {
    /// A ready-to-use use case.
    pub fn new() -> Self {
        Self
    }

    /// Refresh `req.calendar_name`'s recorded slice against live.
    ///
    /// # Errors
    /// [`CicadaError::Fatal`] when the calendar has no recorded
    /// slice (nothing applied yet — there is nothing to refresh);
    /// whatever `list_events`/`load`/`save` return.
    pub async fn run(&self, req: &RefreshRequest<'_>) -> Result<RefreshReport> {
        let mut state = req.state.load().await?;
        let live = req.provider.list_events(req.remote_calendar, None).await?;
        let live_by_remote: BTreeMap<&str, &crate::ports::RemoteEvent> =
            live.iter().map(|e| (e.remote_id.as_str(), e)).collect();

        let Some(recorded) = state.calendars.get_mut(req.calendar_name) else {
            return Err(CicadaError::fatal(format!(
                "no recorded state for calendar {:?} — nothing applied yet",
                req.calendar_name
            )));
        };

        let mut report = RefreshReport {
            calendar: req.calendar_name.to_owned(),
            ..RefreshReport::default()
        };

        // Collect first — recorded.events can't be mutated while
        // iterated.
        let mut updates: Vec<LocalId> = Vec::new();
        for (local_id, rec) in &recorded.events {
            let live = live_by_remote.get(rec.remote_id.as_str()).copied();
            match detect_drift(local_id, rec, live) {
                Some(kind @ DriftKind::Missing) => {
                    report.reconciled.push(DriftEntry::new(
                        local_id.clone(),
                        rec.remote_id.clone(),
                        kind,
                    ));
                    report.removed.push(local_id.clone());
                }
                Some(kind @ DriftKind::Modified { .. }) => {
                    report.reconciled.push(DriftEntry::new(
                        local_id.clone(),
                        rec.remote_id.clone(),
                        kind,
                    ));
                    if live.is_some() {
                        updates.push(local_id.clone());
                    }
                }
                None => report.unchanged += 1,
            }
        }

        for local_id in &report.removed {
            recorded.events.remove(local_id);
        }
        for local_id in updates {
            let Some(live) = live_by_remote
                .get(recorded.events[&local_id].remote_id.as_str())
                .copied()
            else {
                continue;
            };
            let Some(entry) = recorded.events.get_mut(&local_id) else {
                continue;
            };
            // Live is truth now: hash recomputed over the live body
            // (falls back to the cicada:state tag), freshest etag.
            entry.hash = live_content_hash(&local_id, &live.body)
                .or_else(|| live.tags.state_hash.clone())
                .unwrap_or_else(|| entry.hash.clone());
            entry.etag.clone_from(&live.etag);
            report.updated.push(local_id);
        }

        // Adoption candidates: cicada:*-tagged live events recorded
        // state doesn't know about (ADR-0002 §2 — reported, not
        // auto-adopted). Untagged events are outside the managed
        // boundary entirely.
        report.adoption_candidates = live
            .iter()
            .filter(|e| e.tags.managed)
            .filter_map(|e| e.tags.local_id.clone())
            .filter(|id| !recorded.events.contains_key(id))
            .collect();
        report.adoption_candidates.sort();

        req.state.save(&state).await?;
        Ok(report)
    }
}
