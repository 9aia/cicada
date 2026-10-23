//! The `Plan` document — `cicada/plan-v1`, a public contract
//! (ADR-0002 §11). `cicada plan --out plan.json` is consumed by CI
//! and agents, so the shape is versioned and stable.

use jiff::Timestamp;
use serde::{Deserialize, Serialize};

use crate::domain::drift::DriftEntry;
use crate::domain::operation::Operation;
use crate::planning::capability::Severity;

/// A plan: ordered operations plus warnings and drift, for one
/// logical calendar. `ops` is in apply order
/// (`Operation::kind().rank()`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Plan {
    /// Format version — always [`Plan::API_VERSION`].
    pub api_version: String,
    /// Logical calendar this plan reconciles (`metadata.name`).
    pub calendar: String,
    /// When the plan was computed — the injected clock, so tests and
    /// golden fixtures stay deterministic.
    pub generated_at: Timestamp,
    /// Ordered ops — deletes/replaces first, noops last.
    #[serde(default)]
    pub ops: Vec<Operation>,
    /// Plan-time findings: capability gaps (warnings) and
    /// unsupported desired state (errors). An error-severity entry
    /// blocks apply — the plan is still produced so CI can report
    /// everything at once.
    #[serde(default)]
    pub warnings: Vec<PlanWarning>,
    /// Recorded-vs-live drift found while planning (ADR-0002 §4):
    /// managed events the live provider no longer matches. Each
    /// entry also marks its op `conflict`ed.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub drift: Vec<DriftEntry>,
}

/// A plan-time finding. `Severity::Warning` is a notice;
/// `Severity::Error` blocks apply.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlanWarning {
    /// How bad it is.
    pub severity: Severity,
    /// Local event id the warning concerns, when applicable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub local_id: Option<String>,
    /// Human-readable message.
    pub message: String,
}

impl PlanWarning {
    /// A warning-severity finding.
    pub fn warning(local_id: Option<String>, message: impl Into<String>) -> Self {
        Self {
            severity: Severity::Warning,
            local_id,
            message: message.into(),
        }
    }

    /// An error-severity finding — blocks apply.
    pub fn error(local_id: Option<String>, message: impl Into<String>) -> Self {
        Self {
            severity: Severity::Error,
            local_id,
            message: message.into(),
        }
    }
}

impl Plan {
    /// The only plan format version this core writes.
    pub const API_VERSION: &'static str = "cicada/plan-v1";

    /// An empty plan for `calendar`, stamped at `now`.
    pub fn new(calendar: impl Into<String>, now: Timestamp) -> Self {
        Self {
            api_version: Self::API_VERSION.to_owned(),
            calendar: calendar.into(),
            generated_at: now,
            ops: Vec::new(),
            warnings: Vec::new(),
            drift: Vec::new(),
        }
    }

    /// Sort `ops` into apply order and return `self` (builder style).
    #[must_use]
    pub fn sorted(mut self) -> Self {
        crate::domain::operation::sort_operations(&mut self.ops);
        self
    }

    /// `true` when any finding is `Severity::Error` — apply must
    /// refuse such plans.
    pub fn has_errors(&self) -> bool {
        self.warnings.iter().any(|w| w.severity == Severity::Error)
    }

    /// Count of ops that perform remote calls (excludes `NoOp`).
    pub fn remote_op_count(&self) -> usize {
        self.ops.iter().filter(|op| op.is_remote()).count()
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn plan_serializes_with_version() {
        let p = Plan::new("work", "2026-10-23T01:00:00Z".parse().unwrap());
        let v = serde_json::to_value(&p).unwrap();
        assert_eq!(v["apiVersion"], "cicada/plan-v1");
        assert_eq!(v["calendar"], "work");
        assert_eq!(v["generatedAt"], "2026-10-23T01:00:00Z");
    }

    #[test]
    fn has_errors_tracks_severity() {
        let mut p = Plan::new("work", "2026-10-23T01:00:00Z".parse().unwrap());
        assert!(!p.has_errors());
        p.warnings.push(PlanWarning::warning(None, "note"));
        assert!(!p.has_errors());
        p.warnings
            .push(PlanWarning::error(Some("x".into()), "unsupported"));
        assert!(p.has_errors());
    }
}
