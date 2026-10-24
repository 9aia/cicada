//! Drift — recorded-state vs live disagreement (ADR-0002 §4).
//!
//! A managed event *drifts* when its live record no longer matches
//! what recorded state says it should be: the recorded remote id is
//! gone ([`DriftKind::Missing`]) or the live event changed since the
//! apply that wrote the record ([`DriftKind::Modified`]). Drift marks
//! plan ops conflicted — apply fails them unless `--force` — and is
//! listed in the plan's drift summary (ADR-0002 §11).

use serde::{Deserialize, Serialize};

use super::ids::{ContentHash, LocalId, RemoteId};

/// Why a managed event drifted — the payload of [`DriftEntry`] and
/// of an operation's `conflict` marker.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    tag = "kind"
)]
pub enum DriftKind {
    /// The recorded remote id no longer exists live — deleted or
    /// moved out of band.
    Missing,
    /// The live event changed since the recorded apply — an ETag
    /// bump where the provider exposes them, a content-hash
    /// mismatch, or a stale `cicada:state` tag otherwise.
    Modified {
        /// The hash recorded at last apply.
        recorded_hash: ContentHash,
        /// Hash recomputed over the live body, when it could be
        /// computed (helps humans see *what* drifted).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        live_hash: Option<ContentHash>,
    },
}

impl DriftKind {
    /// Stable machine-readable code (`missing` / `modified`).
    pub fn code(&self) -> &'static str {
        match self {
            Self::Missing => "missing",
            Self::Modified { .. } => "modified",
        }
    }
}

impl std::fmt::Display for DriftKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.code())
    }
}

/// One managed event's drift — `localId → remoteId` plus the kind,
/// serialized flat (`{localId, remoteId, kind, …}`) in the plan's
/// drift summary.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    tag = "kind"
)]
pub enum DriftEntry {
    /// The recorded remote event is gone live.
    Missing {
        /// Stable local id of the managed event.
        local_id: LocalId,
        /// The remote id recorded state points at.
        remote_id: RemoteId,
    },
    /// The live event changed since the recorded apply.
    Modified {
        /// Stable local id of the managed event.
        local_id: LocalId,
        /// The remote id recorded state points at.
        remote_id: RemoteId,
        /// The hash recorded at last apply.
        recorded_hash: ContentHash,
        /// Hash recomputed over the live body, when available.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        live_hash: Option<ContentHash>,
    },
}

impl DriftEntry {
    /// Wrap a [`DriftKind`] with the ids it belongs to.
    pub fn new(local_id: LocalId, remote_id: RemoteId, kind: DriftKind) -> Self {
        match kind {
            DriftKind::Missing => Self::Missing {
                local_id,
                remote_id,
            },
            DriftKind::Modified {
                recorded_hash,
                live_hash,
            } => Self::Modified {
                local_id,
                remote_id,
                recorded_hash,
                live_hash,
            },
        }
    }

    /// The managed event's local id.
    pub fn local_id(&self) -> &LocalId {
        match self {
            Self::Missing { local_id, .. } | Self::Modified { local_id, .. } => local_id,
        }
    }

    /// The remote id recorded state points at.
    pub fn remote_id(&self) -> &RemoteId {
        match self {
            Self::Missing { remote_id, .. } | Self::Modified { remote_id, .. } => remote_id,
        }
    }

    /// The [`DriftKind`] without the ids.
    pub fn kind(&self) -> DriftKind {
        match self {
            Self::Missing { .. } => DriftKind::Missing,
            Self::Modified {
                recorded_hash,
                live_hash,
                ..
            } => DriftKind::Modified {
                recorded_hash: recorded_hash.clone(),
                live_hash: live_hash.clone(),
            },
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn drift_entry_serializes_flat() {
        let e = DriftEntry::Modified {
            local_id: "standup".into(),
            remote_id: "abc".into(),
            recorded_hash: "sha256:a".into(),
            live_hash: Some("sha256:b".into()),
        };
        let v = serde_json::to_value(&e).unwrap();
        assert_eq!(v["localId"], "standup");
        assert_eq!(v["remoteId"], "abc");
        assert_eq!(v["kind"], "modified");
        assert_eq!(v["recordedHash"], "sha256:a");
        assert_eq!(v["liveHash"], "sha256:b");

        let m = DriftEntry::Missing {
            local_id: "gone".into(),
            remote_id: "x".into(),
        };
        let v = serde_json::to_value(&m).unwrap();
        assert_eq!(v["kind"], "missing");
        assert_eq!(v["localId"], "gone");

        // Round-trips.
        let back: DriftEntry = serde_json::from_value(v).unwrap();
        assert_eq!(back, m);
    }

    #[test]
    fn entry_wraps_kind() {
        let e = DriftEntry::new("a".into(), "r".into(), DriftKind::Missing);
        assert_eq!(e.kind(), DriftKind::Missing);
        assert_eq!(e.local_id().as_str(), "a");
        assert_eq!(e.remote_id().as_str(), "r");
    }

    #[test]
    fn kind_codes_cover_both_variants() {
        assert_eq!(DriftKind::Missing.code(), "missing");
        let modified = DriftKind::Modified {
            recorded_hash: "sha256:a".into(),
            live_hash: None,
        };
        assert_eq!(modified.code(), "modified");
        assert_eq!(DriftKind::Missing.to_string(), "missing");
        assert_eq!(modified.to_string(), "modified");
    }
}
