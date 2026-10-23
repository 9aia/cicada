//! The `Plan` document — `cicada/plan-v1`, a public contract
//! (ADR-0002 §11). `cicada plan --out plan.json` is consumed by CI
//! and agents, so the shape is versioned and stable.

use serde::{Deserialize, Serialize};

use crate::domain::operation::Operation;

/// A plan: ordered operations plus warnings, for one logical
/// calendar. `ops` is in apply order (`Operation::kind().rank()`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Plan {
    /// Format version — always [`Plan::API_VERSION`].
    pub api_version: String,
    /// Logical calendar this plan reconciles (`metadata.name`).
    pub calendar: String,
    /// Ordered ops — deletes/replaces first, noops last.
    #[serde(default)]
    pub ops: Vec<Operation>,
    /// Non-fatal issues surfaced at plan time (capability gaps the
    /// provider absorbs, notification policy degradation, …).
    #[serde(default)]
    pub warnings: Vec<PlanWarning>,
}

/// A non-fatal plan-time notice (capability degradation, drift note).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlanWarning {
    /// Local event id the warning concerns, when applicable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub local_id: Option<String>,
    /// Human-readable message.
    pub message: String,
}

impl Plan {
    /// The only plan format version this core writes.
    pub const API_VERSION: &'static str = "cicada/plan-v1";

    /// An empty plan for `calendar`.
    pub fn new(calendar: impl Into<String>) -> Self {
        Self {
            api_version: Self::API_VERSION.to_owned(),
            calendar: calendar.into(),
            ops: Vec::new(),
            warnings: Vec::new(),
        }
    }

    /// Sort `ops` into apply order and return `self` (builder style).
    #[must_use]
    pub fn sorted(mut self) -> Self {
        crate::domain::operation::sort_operations(&mut self.ops);
        self
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
        let p = Plan::new("work");
        let v = serde_json::to_value(&p).unwrap();
        assert_eq!(v["apiVersion"], "cicada/plan-v1");
        assert_eq!(v["calendar"], "work");
    }
}
