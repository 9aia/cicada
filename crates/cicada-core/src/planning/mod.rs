//! Planning — pure diff/plan machinery (ADR-0001 §3, ADR-0002 §11).
//!
//! Build step 1 lands the types plus the pieces that are already
//! trivial: [`EventDiffer`] (field diffs) and [`CapabilityChecker`]
//! (feature support checks). [`OperationPlanner`] — the full
//! desired-vs-recorded-vs-live reconciliation — is scaffolded for a
//! later build step.

pub mod capability;
pub mod differ;
pub mod plan;
pub mod planner;

pub use capability::{CapabilityChecker, CapabilityIssue, Severity};
pub use differ::EventDiffer;
pub use plan::{Plan, PlanWarning};
pub use planner::OperationPlanner;
