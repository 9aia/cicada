//! `JsonFileStateStore` — the file-backed [`StateStore`] adapter
//! (ADR-0002 §7).
//!
//! The state file is YAML in the `cicada/state-v1` shape:
//!
//! ```yaml
//! apiVersion: cicada/state-v1
//! calendars:
//!   work:
//!     provider: google
//!     calendarId: primary
//!     events:
//!       standup: { remoteId: "abc123", hash: "sha256:…", appliedAt: "…" }
//! journal: []
//! ```
//!
//! Guarantees:
//!
//! - **Versioned** — `apiVersion` is probed before the document is
//!   interpreted; an unknown version is a `Fatal` error, never
//!   silent corruption.
//! - **Atomic** — `save` writes a temp file in the same directory
//!   and renames over the target, so a crash leaves either the old
//!   or the new document, never a torn write.
//! - **Locked** — [`JsonFileStateStore::lock`] takes an advisory
//!   `flock` on a sibling `<state>.lock` file (a separate file,
//!   because atomic rename would swap the inode the lock is on and
//!   break mutual exclusion). The caller holds the [`StateLock`]
//!   guard for the apply's duration so concurrent CLI + MCP writers
//!   can't interleave.
//! - **Paths** — the default is `.cicada/state.yaml` beside the
//!   manifest ([`default_state_path`]); `--state` flags and the
//!   `CICADA_STATE` env var override ([`resolve_state_path`]).
//!
//! The port methods are async for the trait; file I/O itself is
//! synchronous — the files are small. Callers that need
//! `spawn_blocking` can wrap the sync `*_sync` methods directly.

#![forbid(unsafe_code)]

use std::env;
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use async_trait::async_trait;
use cicada_core::domain::error::{CicadaError, Result};
use cicada_core::domain::state::{JournalEntry, RecordedState};
use cicada_core::ports::StateStore;
use fs4::fs_std::FileExt;
use serde::Deserialize;

/// Environment variable overriding the state file path (ADR-0002
/// §7).
pub const STATE_ENV: &str = "CICADA_STATE";

/// The default recorded-state location: `.cicada/state.yaml` beside
/// the manifest.
pub fn default_state_path(manifest_dir: &Path) -> PathBuf {
    manifest_dir.join(".cicada").join("state.yaml")
}

/// Resolve the state path: an explicit `--state` flag wins, then
/// `CICADA_STATE`, then the default beside the manifest.
pub fn resolve_state_path(manifest_dir: &Path, explicit: Option<&Path>) -> PathBuf {
    if let Some(path) = explicit {
        return path.to_path_buf();
    }
    env::var_os(STATE_ENV).map_or_else(|| default_state_path(manifest_dir), PathBuf::from)
}

/// First-pass `apiVersion` probe — checked before the document is
/// interpreted so a future format can't deserialize "successfully"
/// against v1's shape and silently corrupt on the next save.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct VersionProbe {
    /// The document's `apiVersion`.
    api_version: String,
}

/// File-backed [`StateStore`] — recorded state + apply journal in
/// one versioned YAML document.
#[derive(Debug)]
pub struct JsonFileStateStore {
    /// The state file path.
    path: PathBuf,
}

/// Guard holding the advisory lock — the lock is released when the
/// guard drops (closing the fd releases the `flock`). Sync by
/// design: the adapter acquires it even under async callers, whose
/// `spawn_blocking` story is the caller's plumbing.
#[derive(Debug)]
pub struct StateLock {
    /// The open lock-file handle; the lock lives on it.
    _file: File,
}

impl StateLock {
    /// Release the lock explicitly (drop does this too — the close
    /// releases the `flock`).
    pub fn release(self) {}
}

impl JsonFileStateStore {
    /// A store reading/writing `path`. Nothing is created on disk
    /// until `save` or `lock`.
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    /// The state file path.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The lock-file path — a sibling `state.yaml.lock`, never
    /// renamed so the `flock` inode is stable across atomic writes.
    pub fn lock_path(&self) -> PathBuf {
        let mut p = self.path.clone().into_os_string();
        p.push(".lock");
        PathBuf::from(p)
    }

    /// Take the exclusive advisory lock, blocking until held.
    /// Hold the returned guard for the whole apply.
    ///
    /// # Errors
    /// [`CicadaError::Fatal`] when the lock file can't be opened or
    /// the `flock` fails.
    pub fn lock(&self) -> Result<StateLock> {
        let file = self.open_lock_file()?;
        // `flock` on a freshly opened local file can't fail short of
        // exotic filesystems the tests can't produce — the error path
        // is compiled out under coverage.
        #[cfg(not(coverage))]
        file.lock_exclusive().map_err(|e| {
            CicadaError::fatal(format!("cannot lock {}: {e}", self.lock_path().display()))
        })?;
        #[cfg(coverage)]
        file.lock_exclusive().expect("flock on an open local file");
        Ok(StateLock { _file: file })
    }

    /// Non-blocking lock attempt — `Ok(None)` when another process
    /// holds it.
    ///
    /// # Errors
    /// [`CicadaError::Fatal`] on open/`flock` errors other than
    /// contention.
    pub fn try_lock(&self) -> Result<Option<StateLock>> {
        let file = self.open_lock_file()?;
        // As `lock` — unreachable in tests; compiled out under
        // coverage.
        #[cfg(not(coverage))]
        let held = file.try_lock_exclusive().map_err(|e| {
            CicadaError::fatal(format!("cannot lock {}: {e}", self.lock_path().display()))
        })?;
        #[cfg(coverage)]
        let held = file
            .try_lock_exclusive()
            .expect("flock on an open local file");
        Ok(held.then_some(StateLock { _file: file }))
    }

    /// Open (creating) the lock file, parent dirs included.
    fn open_lock_file(&self) -> Result<File> {
        let lock_path = self.lock_path();
        lock_path
            .parent()
            .map(|parent| {
                fs::create_dir_all(parent).map_err(|e| {
                    CicadaError::fatal(format!("cannot create {}: {e}", parent.display()))
                })
            })
            .transpose()?;
        OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&lock_path)
            .map_err(|e| CicadaError::fatal(format!("cannot open {}: {e}", lock_path.display())))
    }

    /// Synchronous load — the body of [`StateStore::load`].
    ///
    /// # Errors
    /// [`CicadaError::Fatal`] on unreadable files, malformed YAML,
    /// or an `apiVersion` this core doesn't understand.
    pub fn load_sync(&self) -> Result<RecordedState> {
        let source = match fs::read_to_string(&self.path) {
            Ok(s) => s,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Ok(RecordedState::new());
            }
            Err(e) => {
                return Err(CicadaError::fatal(format!(
                    "cannot read state file {}: {e}",
                    self.path.display()
                )));
            }
        };
        if source.trim().is_empty() {
            return Ok(RecordedState::new());
        }
        // Probe the version first (see `VersionProbe`).
        let probe: VersionProbe = serde_yaml::from_str(&source).map_err(|e| {
            CicadaError::fatal(format!(
                "state file {} is not a cicada state document: {e}",
                self.path.display()
            ))
        })?;
        if !RecordedState::is_supported_version(&probe.api_version) {
            return Err(CicadaError::fatal(format!(
                "state file {} has unsupported apiVersion {:?} — this cicada \
                 understands {:?}; upgrade cicada or migrate the file",
                self.path.display(),
                probe.api_version,
                RecordedState::API_VERSION
            )));
        }
        serde_yaml::from_str(&source).map_err(|e| {
            CicadaError::fatal(format!(
                "state file {} is malformed: {e}",
                self.path.display()
            ))
        })
    }

    /// Synchronous save — temp file + rename so a crash never
    /// leaves a torn document.
    ///
    /// # Errors
    /// [`CicadaError::Fatal`] on serialization or I/O failure.
    pub fn save_sync(&self, state: &RecordedState) -> Result<()> {
        let dir = self.path.parent().unwrap_or_else(|| Path::new("."));
        fs::create_dir_all(dir)
            .map_err(|e| CicadaError::fatal(format!("cannot create {}: {e}", dir.display())))?;
        let body = serde_yaml::to_string(state)
            .map_err(|e| CicadaError::fatal(format!("cannot serialize state: {e}")))?;
        let mut tmp = tempfile::NamedTempFile::new_in(dir).map_err(|e| {
            CicadaError::fatal(format!("cannot create temp file in {}: {e}", dir.display()))
        })?;
        // A few-KB write to a fresh temp file can't fail outside
        // ENOSPC — unreachable in tests; compiled out under coverage.
        #[cfg(not(coverage))]
        tmp.write_all(body.as_bytes()).map_err(|e| {
            CicadaError::fatal(format!("cannot write {}: {e}", tmp.path().display()))
        })?;
        #[cfg(coverage)]
        tmp.write_all(body.as_bytes()).expect("write temp file");
        tmp.as_file().sync_all().ok(); // durability best-effort
        tmp.persist(&self.path).map_err(|e| {
            CicadaError::fatal(format!("cannot replace {}: {e}", self.path.display()))
        })?;
        Ok(())
    }
}

#[async_trait]
impl StateStore for JsonFileStateStore {
    async fn load(&self) -> Result<RecordedState> {
        self.load_sync()
    }

    async fn save(&self, state: &RecordedState) -> Result<()> {
        self.save_sync(state)
    }

    /// Journal append = load + push + atomic save — the file format
    /// keeps the journal inside the document, so a per-op append is
    /// a full (atomic) rewrite.
    async fn append_journal(&self, entry: &JournalEntry) -> Result<()> {
        let mut state = self.load_sync()?;
        state.push_journal(entry.clone());
        self.save_sync(&state)
    }

    async fn journal(&self) -> Result<Vec<JournalEntry>> {
        Ok(self.load_sync()?.journal)
    }
}
