//! Plan-digest gate — the engine-side replacement for cicada-mcp's
//! session state (T-019).
//!
//! `cicada plan` previews desired vs recorded vs live and issues a
//! `planDigest` (`sha256:` over the serialized plan set, `generatedAt`
//! stripped — timestamps always differ, everything else must match).
//! The digest is persisted beside the state file (`.cicada/
//! plan-digest`, one digest per line) so the gate survives process
//! boundaries: CLI invocations, `cli.*` wire ops, and generated
//! surfaces are all separate processes sharing only the filesystem.
//!
//! `cicada apply --plan-digest <d>` enforces the contract the MCP
//! server used to hold in a `Mutex<BTreeSet>`:
//!
//! 1. `<d>` must have been issued by a `plan` run and not yet spent —
//!    proves a preview happened.
//! 2. Under the state lock, apply re-plans and re-derives the digest —
//!    YAML, live, or recorded drift since preview renders it stale
//!    and the apply refuses.
//! 3. A verified digest is consumed — a second apply must re-preview.
//!
//! Providing `--plan-digest` *is* the confirmation (it can only exist
//! after a human/agent saw the preview), so it skips the interactive
//! prompt — the non-interactive equivalent of MCP's `confirm: true`.

use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};

use cicada_core::planning::Plan;
use sha2::{Digest, Sha256};

/// The issued-digest file: a sibling of the state file, so `--state`
/// / `CICADA_STATE` overrides relocate it with the state they guard.
pub(crate) fn digest_path(state_path: &Path) -> PathBuf {
    state_path.with_file_name("plan-digest")
}

/// `sha256:` digest over the plan set, `generatedAt` stripped — the
/// preview → apply re-plan must produce identical digests when
/// nothing changed, and timestamps always differ.
pub(crate) fn plans_digest(plans: &[&Plan]) -> String {
    // `Plan` serializes infallibly (string-keyed fields only); a
    // hypothetical failure digests the empty value, which is still a
    // stable, comparable token.
    let mut value = serde_json::to_value(plans).unwrap_or_default();
    for plan in value.as_array_mut().into_iter().flatten() {
        plan.as_object_mut().map(|obj| obj.remove("generatedAt"));
    }
    let bytes = serde_json::to_vec(&value).unwrap_or_default();
    let hash = Sha256::digest(&bytes);
    let hex = hash.iter().fold(String::new(), |mut s, b| {
        let _ = write!(s, "{b:02x}");
        s
    });
    format!("sha256:{hex}")
}

/// Record `digest` as issued — appends (deduped), atomically. The
/// file is newline-separated plain text: one `sha256:…` per line.
/// I/O failures are reported, never fatal to the preview itself —
/// apply will simply refuse the unpersisted digest.
pub(crate) fn issue(state_path: &Path, digest: &str) -> std::io::Result<()> {
    let path = digest_path(state_path);
    let mut digests = read_digests(&path);
    if !digests.iter().any(|d| d == digest) {
        digests.push(digest.to_owned());
    }
    write_digests(&path, &digests)
}

/// The issued, unspent digests. Missing/unreadable file → empty set
/// (apply then refuses every `--plan-digest` — fail closed).
fn read_digests(path: &Path) -> Vec<String> {
    fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(str::to_owned)
        .collect()
}

/// Atomically rewrite the digest file (temp + rename, same dir).
fn write_digests(path: &Path, digests: &[String]) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("tmp");
    let mut text = digests.join("\n");
    if !text.is_empty() {
        text.push('\n');
    }
    fs::write(&tmp, text)?;
    fs::rename(&tmp, path)
}

/// Is `digest` currently issued (previewed, not yet spent)?
pub(crate) fn is_issued(state_path: &Path, digest: &str) -> bool {
    read_digests(&digest_path(state_path))
        .iter()
        .any(|d| d == digest)
}

/// Spend `digest` — remove it from the issued set so a second apply
/// must re-preview. Missing file/digest is not an error.
pub(crate) fn consume(state_path: &Path, digest: &str) {
    let path = digest_path(state_path);
    let mut digests = read_digests(&path);
    digests.retain(|d| d != digest);
    // Best-effort: the apply already verified under the state lock;
    // a failed rewrite only means the digest stays spendable once
    // more, which the freshness re-check still gates.
    let _ = write_digests(&path, &digests);
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn digest_ignores_generated_at() {
        let a = Plan::new("work", "2026-01-01T00:00:00Z".parse().unwrap());
        let mut b = Plan::new("work", "2026-01-02T00:00:00Z".parse().unwrap());
        // Same content, different timestamps → same digest.
        b.generated_at = a.generated_at;
        let da = plans_digest(&[&a]);
        let db = plans_digest(&[&b]);
        assert_eq!(da, db);
        assert!(da.starts_with("sha256:"));
    }

    #[test]
    fn issue_is_issued_consume_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let state = dir.path().join(".cicada/state.yaml");
        assert!(!is_issued(&state, "sha256:x"));

        issue(&state, "sha256:x").unwrap();
        issue(&state, "sha256:y").unwrap();
        issue(&state, "sha256:x").unwrap(); // deduped
        assert!(is_issued(&state, "sha256:x"));
        assert!(is_issued(&state, "sha256:y"));

        consume(&state, "sha256:x");
        assert!(!is_issued(&state, "sha256:x"));
        assert!(is_issued(&state, "sha256:y"));

        // Digest file lives beside the state file.
        assert!(digest_path(&state).ends_with("plan-digest"));
        assert_eq!(
            digest_path(&state).parent().unwrap(),
            state.parent().unwrap()
        );
    }
}
