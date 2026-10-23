//! Planning — pure diff/plan machinery (ADR-0001 §3, ADR-0002 §11).
//!
//! [`EventDiffer`] computes field-level diffs (recurrence compares
//! semantically), [`CapabilityChecker`] validates declared features
//! against the provider, and [`OperationPlanner`] reconciles desired
//! vs recorded vs live into a [`Plan`] — ordered ops, warnings, and
//! a drift summary.

pub mod capability;
pub mod differ;
pub mod plan;
pub mod planner;

pub use capability::{CapabilityChecker, CapabilityIssue, Severity};
pub use differ::EventDiffer;
pub use plan::{Plan, PlanWarning};
pub use planner::{OperationPlanner, PlanInput};
