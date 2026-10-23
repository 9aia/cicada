//! `DetectDrift` — recorded-vs-live comparison (`cicada drift`,
//! ADR-0002 §4).
//!
//! Produces the same [`DriftEntry`] set the planner emits — drift is
//! defined once (`planning::planner::detect_drift`) so `plan`,
//! `drift`, and `refresh` never disagree. Untagged live events are
//! ignored (managed boundary, ADR-0002 §1).

use std::collections::BTreeMap;

use crate::domain::drift::DriftEntry;
use crate::domain::error::Result;
use crate::domain::state::RecordedCalendar;
use crate::planning::planner::detect_drift;
use crate::ports::{CalendarProvider, CalendarRef, RemoteEvent};

/// Everything [`DetectDrift::run`] needs for one logical calendar.
pub struct DriftRequest<'a> {
    /// The logical calendar whose recorded slice is checked
    /// (`state.calendars` key) — for labeling only.
    pub calendar_name: &'a str,
    /// The recorded slice to compare against live.
    pub recorded: &'a RecordedCalendar,
    /// Provider to read live state from.
    pub provider: &'a dyn CalendarProvider,
    /// The remote binding to list — the recorded `calendarId`.
    pub remote_calendar: &'a CalendarRef,
}

/// `DetectDrift` — stateless use case.
#[derive(Debug, Clone, Copy, Default)]
pub struct DetectDrift;

impl DetectDrift {
    /// A ready-to-use use case.
    pub fn new() -> Self {
        Self
    }

    /// Compare `req.recorded` with the live provider slice.
    ///
    /// # Errors
    /// Whatever `list_events` returns.
    pub async fn run(&self, req: &DriftRequest<'_>) -> Result<Vec<DriftEntry>> {
        let live = req.provider.list_events(req.remote_calendar, None).await?;
        let live_by_remote: BTreeMap<&str, &RemoteEvent> =
            live.iter().map(|e| (e.remote_id.as_str(), e)).collect();
        Ok(req
            .recorded
            .events
            .iter()
            .filter_map(|(local_id, rec)| {
                let live = live_by_remote.get(rec.remote_id.as_str()).copied();
                detect_drift(local_id, rec, live)
                    .map(|kind| DriftEntry::new(local_id.clone(), rec.remote_id.clone(), kind))
            })
            .collect())
    }
}
