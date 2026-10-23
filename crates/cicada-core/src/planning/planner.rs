//! `OperationPlanner` — desired-vs-recorded reconciliation.
//!
//! **Build step 1 scaffolding:** types and signatures land now; the
//! full algorithm is build step 3 (`core/planning + PlanCalendar`),
//! so `plan` currently returns `Fatal("not implemented")`.
//!
//! ## Semantics the implementation must honor (ADR-0001 §3/§4,
//! ADR-0002 §1/§4)
//!
//! - The managed set is *exactly* the recorded state — deletes are
//!   generated only for local ids recorded but absent from desired.
//! - Desired event with no recorded entry → `Create`. Recorded +
//!   desired + hash equal → `NoOp`. Hash differs → diff:
//!   `recurrence` change → `Replace`; anything else → `Update`.
//! - Output ops are sorted into apply order
//!   ([`crate::domain::operation::sort_operations`]) and every op
//!   carries the content hash it was planned against so
//!   [`crate::domain::operation::Operation::idempotency_key`] is
//!   stable.
//! - Live-vs-recorded disagreement on a touched event is a
//!   `Conflict` (drift), not an overwrite — `--force` is an apply
//!   flag, not a planning one.

use crate::domain::calendar::Calendar;
use crate::domain::error::{CicadaError, Result};
use crate::domain::state::RecordedCalendar;
use crate::planning::plan::Plan;

/// Reconciles desired calendars against recorded state into ordered
/// plans. Pure and stateless — the live provider is read through
/// `CalendarProvider` by the calling use case, which passes in the
/// recorded slice; drift detection (recorded vs live) is likewise
/// performed by the caller and surfaced via plan warnings.
#[derive(Debug, Clone, Copy, Default)]
pub struct OperationPlanner;

impl OperationPlanner {
    /// Compute the plan for `desired` against `recorded`.
    ///
    /// Full reconciliation is build step 3 — see the module docs for
    /// the contract this will implement.
    ///
    /// # Errors
    /// [`CicadaError::Fatal`] until implemented.
    pub fn plan(&self, desired: &Calendar, recorded: Option<&RecordedCalendar>) -> Result<Plan> {
        let _ = (desired, recorded);
        Err(CicadaError::fatal(
            "OperationPlanner::plan is not implemented yet (build step 3)",
        ))
    }
}
