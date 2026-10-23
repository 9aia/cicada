//! Application layer — the use cases interfaces drive (ADR-0001 §1).
//!
//! Use cases take domain data (never paths or handles), depend only
//! on port traits for I/O, and return structured results interfaces
//! render. `cicada validate` → [`ValidateCalendar`]; `plan`, `apply`,
//! `refresh`, `drift`, `range`, `show`, `fmt` land in later build
//! steps against the same shape.
//!
//! Nothing in this module performs I/O: the CLI/MCP shells read files
//! and call providers; use cases see only parsed, portable values.
//!
//! `cicada validate` → [`ValidateCalendar`]; `plan` →
//! [`PlanCalendar`]; `apply` → [`ApplyPlan`]; `refresh` →
//! [`RefreshState`]; `drift` → [`DetectDrift`]; `range`/`show` →
//! [`QueryRange`]/[`ShowEvent`]; `fmt` lands in a later build step.

pub mod apply;
pub mod drift;
pub mod plan;
pub mod range;
pub mod refresh;
pub mod show;
pub mod validate;

pub use apply::{ApplyPlan, ApplyReport, ApplyRequest, OpOutcome, OpResult};
pub use drift::{DetectDrift, DriftRequest};
pub use plan::{PlanCalendar, PlanRequest};
pub use range::{QueryRange, RangeOccurrence};
pub use refresh::{RefreshReport, RefreshRequest, RefreshState};
pub use show::{EventDetail, ShowEvent, default_window};
pub use validate::{Diagnostic, Severity, ValidateCalendar, ValidationReport};
