//! `PlanCalendar` — the use case behind `cicada plan` and the MCP
//! `plan.preview` tool (ADR-0001 §3, ADR-0002 §11).
//!
//! Orchestration only: fetch the live slice and capabilities through
//! the [`CalendarProvider`] port, then hand everything to the pure
//! [`OperationPlanner`]. The planner decides ops, warnings, and
//! drift; this use case decides *what to read*.

use crate::domain::calendar::Calendar;
use crate::domain::error::Result;
use crate::domain::policy::SendUpdates;
use crate::domain::state::RecordedCalendar;
use crate::planning::plan::Plan;
use crate::planning::planner::{OperationPlanner, PlanInput};
use crate::ports::{CalendarProvider, CalendarRef, Clock};

/// Everything [`PlanCalendar::run`] needs for one logical calendar.
pub struct PlanRequest<'a> {
    /// Desired state (parsed YAML).
    pub desired: &'a Calendar,
    /// Recorded slice for this calendar (`None` = never applied).
    pub recorded: Option<&'a RecordedCalendar>,
    /// The provider to read live state and capabilities from.
    pub provider: &'a dyn CalendarProvider,
    /// The remote calendar this logical calendar binds to
    /// (ADR-0002 §9) — from `metadata.calendarId` or recorded state.
    pub remote_calendar: &'a CalendarRef,
    /// Clock for `generatedAt` on the plan document.
    pub clock: &'a dyn Clock,
    /// Notification policy for apply (`spec.apply.sendUpdates`,
    /// default `ExternalOnly`).
    pub send_updates: SendUpdates,
}

/// `PlanCalendar` — stateless use case; construct cheaply per call
/// site.
#[derive(Debug, Clone, Copy, Default)]
pub struct PlanCalendar;

impl PlanCalendar {
    /// A ready-to-use use case.
    pub fn new() -> Self {
        Self
    }

    /// Fetch live events + capabilities and produce the [`Plan`].
    ///
    /// The live listing is intentionally unfiltered — unmanaged
    /// events ride along untouched so the planner can delimit the
    /// managed boundary by recorded state (ADR-0002 §1).
    ///
    /// # Errors
    /// Whatever the provider's `list_events` returns (transient,
    /// auth, …) or the planner's `Fatal` on unrenderable content.
    pub async fn run(&self, req: &PlanRequest<'_>) -> Result<Plan> {
        let live = req.provider.list_events(req.remote_calendar, None).await?;
        let capabilities = req.provider.capabilities();
        OperationPlanner.plan(&PlanInput {
            desired: req.desired,
            recorded: req.recorded,
            live: &live,
            capabilities: &capabilities,
            send_updates: req.send_updates,
            clock: req.clock,
        })
    }
}
