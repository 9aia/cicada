//! Plan operations and field diffs.
//!
//! [`Operation`] is what a plan is made of (ADR-0001 §3): `Create |
//! Update | Delete | Replace | NoOp`. Ordering is fixed — `Delete`,
//! then `Replace` (which internally re-creates), then `Create`, then
//! `Update`, then `NoOp` — minimizing conflicts during the
//! non-transactional apply (ADR-0001 §4).
//!
//! Every op carries the [`ContentHash`] it was planned against, so
//! [`Operation::idempotency_key`] can derive
//! `calendarId + localId + opKind + contentHash` deterministically.

use serde::{Deserialize, Serialize};

use super::drift::DriftKind;
use super::event::Event;
use super::ids::{ContentHash, LocalId, RemoteId};

/// Which kind of change an [`Operation`] represents — also used in
/// journal entries and plan output.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum OpKind {
    /// Create a new remote event.
    Create,
    /// Patch an existing remote event in place.
    Update,
    /// Delete a managed remote event.
    Delete,
    /// Delete + recreate (recurrence-rule changes make instances
    /// diverge; ADR-0001 §5).
    Replace,
    /// No change needed.
    NoOp,
}

impl OpKind {
    /// Apply order (ADR-0001 §4: deletes first, then creates, then
    /// updates). `Replace` sorts with deletes since its first
    /// sub-step removes the old event; `NoOp` sorts last and is
    /// filtered from apply anyway.
    fn rank(self) -> u8 {
        match self {
            Self::Delete => 0,
            Self::Replace => 1,
            Self::Create => 2,
            Self::Update => 3,
            Self::NoOp => 4,
        }
    }

    /// Wire/code form (`create`, `update`, …).
    pub fn code(self) -> &'static str {
        match self {
            Self::Create => "create",
            Self::Update => "update",
            Self::Delete => "delete",
            Self::Replace => "replace",
            Self::NoOp => "noop",
        }
    }
}

/// One planned change to reconcile desired → recorded state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    tag = "kind"
)]
pub enum Operation {
    /// Create `desired` remotely and record it under `local_id`.
    Create {
        /// Desired event (carries the local id).
        desired: Box<Event>,
        /// Hash the op was planned against.
        hash: ContentHash,
        /// Drift-conflict marker (ADR-0002 §4) — apply fails the op
        /// unless `--force`. Always `None` on `Create` today (no
        /// recorded state exists to drift against); kept for a
        /// uniform wire shape.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        conflict: Option<DriftKind>,
    },
    /// Patch the remote event in place.
    Update {
        /// Stable local id.
        local_id: LocalId,
        /// Recorded remote id.
        remote_id: RemoteId,
        /// New desired content.
        desired: Box<Event>,
        /// Human-readable field diffs for plan output.
        diff: EventDiff,
        /// Hash the op was planned against.
        hash: ContentHash,
        /// Recorded remote ETag for conditional writes, when the
        /// provider supports them (ADR-0002 §4).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        etag: Option<String>,
        /// Drift-conflict marker (ADR-0002 §4).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        conflict: Option<DriftKind>,
    },
    /// Delete the remote event (local id removed from desired YAML).
    Delete {
        /// Stable local id.
        local_id: LocalId,
        /// Recorded remote id.
        remote_id: RemoteId,
        /// Recorded content hash.
        hash: ContentHash,
        /// Recorded remote ETag, for conditional deletes.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        etag: Option<String>,
        /// Drift-conflict marker (ADR-0002 §4). A missing live event
        /// still marks the op — the record is stale and apply wants
        /// the user to refresh unless forced.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        conflict: Option<DriftKind>,
    },
    /// Recurrence changed — instances diverge, so delete + recreate
    /// as one op (conferencing attach is a sub-step, ADR-0001 §4).
    Replace {
        /// Stable local id.
        local_id: LocalId,
        /// Recorded remote id.
        remote_id: RemoteId,
        /// New desired content.
        desired: Box<Event>,
        /// Human-readable diff of what changed.
        diff: EventDiff,
        /// Hash the op was planned against.
        hash: ContentHash,
        /// Recorded remote ETag for conditional writes.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        etag: Option<String>,
        /// Drift-conflict marker (ADR-0002 §4).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        conflict: Option<DriftKind>,
    },
    /// Recorded and desired agree — no remote call needed.
    NoOp {
        /// Stable local id.
        local_id: LocalId,
        /// Recorded remote id.
        remote_id: RemoteId,
        /// Recorded content hash.
        hash: ContentHash,
        /// Drift-conflict marker — a `NoOp` can still drift (desired
        /// == recorded but live was edited out of band).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        conflict: Option<DriftKind>,
    },
}

impl Operation {
    /// The [`OpKind`] of this operation.
    pub fn kind(&self) -> OpKind {
        match self {
            Self::Create { .. } => OpKind::Create,
            Self::Update { .. } => OpKind::Update,
            Self::Delete { .. } => OpKind::Delete,
            Self::Replace { .. } => OpKind::Replace,
            Self::NoOp { .. } => OpKind::NoOp,
        }
    }

    /// The local id this op reconciles.
    pub fn local_id(&self) -> &LocalId {
        match self {
            Self::Create { desired, .. } => &desired.id,
            Self::Update { local_id, .. }
            | Self::Delete { local_id, .. }
            | Self::Replace { local_id, .. }
            | Self::NoOp { local_id, .. } => local_id,
        }
    }

    /// The content hash the op was planned against.
    pub fn hash(&self) -> &ContentHash {
        match self {
            Self::Create { hash, .. }
            | Self::Update { hash, .. }
            | Self::Delete { hash, .. }
            | Self::Replace { hash, .. }
            | Self::NoOp { hash, .. } => hash,
        }
    }

    /// The drift-conflict marker the planner set, if any (ADR-0002
    /// §4): live disagrees with recorded state for the managed event
    /// this op touches. Apply fails conflicted ops unless `--force`.
    pub fn conflict(&self) -> Option<&DriftKind> {
        match self {
            Self::Create { conflict, .. }
            | Self::Update { conflict, .. }
            | Self::Delete { conflict, .. }
            | Self::Replace { conflict, .. }
            | Self::NoOp { conflict, .. } => conflict.as_ref(),
        }
    }

    /// Mark this op conflicted by `drift` — planner-facing; called
    /// once per op after construction.
    pub fn set_conflict(&mut self, drift: DriftKind) {
        let slot = match self {
            Self::Create { conflict, .. }
            | Self::Update { conflict, .. }
            | Self::Delete { conflict, .. }
            | Self::Replace { conflict, .. }
            | Self::NoOp { conflict, .. } => conflict,
        };
        *slot = Some(drift);
    }

    /// `calendarId + localId + opKind + contentHash` (ADR-0001 §4) —
    /// deterministic per (plan, event, op), so a retried or resumed
    /// apply re-derives the same key and providers can dedupe.
    pub fn idempotency_key(&self, calendar_id: &str) -> String {
        let local = self.local_id();
        let kind = self.kind().code();
        let hash = self.hash();
        format!("{calendar_id}|{local}|{kind}|{hash}")
    }

    /// Whether this op performs remote calls when applied.
    pub fn is_remote(&self) -> bool {
        !matches!(self, Self::NoOp { .. })
    }
}

/// Sort `ops` into apply order: Delete → Replace → Create → Update →
/// NoOp. Stable within a kind (BTreeMap iteration order from the
/// desired calendar is preserved).
pub fn sort_operations(ops: &mut [Operation]) {
    ops.sort_by_key(|op| op.kind().rank());
}

/// A field-level diff between desired and actual — the
/// human-readable payload of `Update`/`Replace` ops and the `changes`
/// list in `plan.json` (ADR-0002 §11).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EventDiff {
    /// Per-field changes; empty means no diff.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub changes: Vec<FieldChange>,
}

impl EventDiff {
    /// `true` when nothing changed.
    pub fn is_empty(&self) -> bool {
        self.changes.is_empty()
    }
}

/// One changed field.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FieldChange {
    /// Which field.
    pub field: EventField,
    /// Rendered old value (`None` = unset before).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from: Option<String>,
    /// Rendered new value (`None` = now unset).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to: Option<String>,
}

/// Diffable event fields.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum EventField {
    /// `summary`.
    Summary,
    /// `description`.
    Description,
    /// `location`.
    Location,
    /// `start`/`end`/`duration`.
    Time,
    /// `recurrence` — a change here produces `Replace`, not `Update`.
    Recurrence,
    /// `attendees`.
    Attendees,
    /// `reminders`.
    Reminders,
    /// `conferencing`.
    Conferencing,
    /// `transparency`.
    Transparency,
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use jiff::SignedDuration;
    use jiff::civil::date;

    use super::*;
    use crate::domain::event::{EventBody, EventTime, Transparency};
    use crate::domain::ids::ZoneId;
    use crate::domain::time::Time;

    fn event(id: &str) -> Event {
        Event::new(
            id,
            EventBody {
                summary: id.into(),
                description: None,
                location: None,
                time: EventTime::Span {
                    start: Time::Wall {
                        local: date(2026, 1, 5).at(9, 0, 0, 0),
                        zone: ZoneId::new_unchecked("UTC"),
                    },
                    duration: SignedDuration::from_mins(30),
                },
                recurrence: None,
                attendees: vec![],
                reminders: vec![],
                conferencing: None,
                transparency: Transparency::Opaque,
            },
        )
    }

    fn hash(s: &str) -> ContentHash {
        ContentHash::new(format!("sha256:{s}"))
    }

    #[test]
    fn operations_sort_into_apply_order() {
        let mut ops = vec![
            Operation::NoOp {
                local_id: "n".into(),
                remote_id: "r".into(),
                hash: hash("a"),
                conflict: None,
            },
            Operation::Update {
                local_id: "u".into(),
                remote_id: "r".into(),
                desired: Box::new(event("u")),
                diff: EventDiff::default(),
                hash: hash("b"),
                etag: None,
                conflict: None,
            },
            Operation::Create {
                desired: Box::new(event("c")),
                hash: hash("c"),
                conflict: None,
            },
            Operation::Replace {
                local_id: "rep".into(),
                remote_id: "r".into(),
                desired: Box::new(event("rep")),
                diff: EventDiff::default(),
                hash: hash("d"),
                etag: None,
                conflict: None,
            },
            Operation::Delete {
                local_id: "d".into(),
                remote_id: "r".into(),
                hash: hash("e"),
                etag: None,
                conflict: None,
            },
        ];
        sort_operations(&mut ops);
        let kinds: Vec<OpKind> = ops.iter().map(Operation::kind).collect();
        assert_eq!(
            kinds,
            [
                OpKind::Delete,
                OpKind::Replace,
                OpKind::Create,
                OpKind::Update,
                OpKind::NoOp
            ]
        );
    }

    #[test]
    fn idempotency_key_is_deterministic() {
        let op = Operation::Create {
            desired: Box::new(event("standup")),
            hash: hash("abc"),
            conflict: None,
        };
        assert_eq!(
            op.idempotency_key("primary"),
            "primary|standup|create|sha256:abc"
        );
        assert_eq!(op.idempotency_key("primary"), op.idempotency_key("primary"));
        assert_ne!(op.idempotency_key("other"), op.idempotency_key("primary"));
    }

    #[test]
    fn local_id_comes_from_the_right_place() {
        let create = Operation::Create {
            desired: Box::new(event("from-event")),
            hash: hash("x"),
            conflict: None,
        };
        assert_eq!(create.local_id().as_str(), "from-event");
        let delete = Operation::Delete {
            local_id: "gone".into(),
            remote_id: "r".into(),
            hash: hash("y"),
            etag: None,
            conflict: None,
        };
        assert_eq!(delete.local_id().as_str(), "gone");
        assert!(delete.is_remote());
    }

    #[test]
    fn conflict_marks_every_variant() {
        let mut op = Operation::NoOp {
            local_id: "x".into(),
            remote_id: "r".into(),
            hash: hash("a"),
            conflict: None,
        };
        assert!(op.conflict().is_none());
        assert!(!serde_json::to_string(&op).unwrap().contains("conflict"));
        op.set_conflict(crate::domain::drift::DriftKind::Missing);
        assert_eq!(
            op.conflict(),
            Some(&crate::domain::drift::DriftKind::Missing)
        );
        let v = serde_json::to_value(&op).unwrap();
        assert_eq!(v["conflict"]["kind"], "missing");
    }

    #[test]
    fn set_conflict_covers_every_variant() {
        let desired = || Box::new(event("x"));
        let mut ops = vec![
            Operation::Create {
                desired: desired(),
                hash: hash("c"),
                conflict: None,
            },
            Operation::Update {
                local_id: "x".into(),
                remote_id: "r".into(),
                desired: desired(),
                diff: EventDiff::default(),
                hash: hash("u"),
                etag: None,
                conflict: None,
            },
            Operation::Delete {
                local_id: "x".into(),
                remote_id: "r".into(),
                hash: hash("d"),
                etag: None,
                conflict: None,
            },
            Operation::Replace {
                local_id: "x".into(),
                remote_id: "r".into(),
                desired: desired(),
                diff: EventDiff::default(),
                hash: hash("rp"),
                etag: None,
                conflict: None,
            },
            Operation::NoOp {
                local_id: "x".into(),
                remote_id: "r".into(),
                hash: hash("n"),
                conflict: None,
            },
        ];
        for op in &mut ops {
            op.set_conflict(crate::domain::drift::DriftKind::Missing);
            assert_eq!(
                op.conflict(),
                Some(&crate::domain::drift::DriftKind::Missing)
            );
        }
    }
}
