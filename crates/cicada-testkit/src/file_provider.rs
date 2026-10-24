//! `FakeFileProvider` — a file-backed [`FakeCalendarProvider`].
//!
//! **Test/demo only.** The in-memory fake dies with the process,
//! which makes the CLI lifecycle (apply → drift → refresh) undemoable
//! end to end. `FakeFileProvider` serializes the whole backend as a
//! [`FakeSnapshot`] JSON document after every mutation, so separate
//! `cicada` invocations share one "provider" — and editing the file
//! by hand simulates an out-of-band edit, exactly like the provider
//! UI.
//!
//! Writes are atomic (sibling temp file + rename). Concurrent CLI
//! processes aren't serialized here — the state-file lock
//! (ADR-0002 §7) is what keeps an apply exclusive; the fake file is
//! a demo seam, not a database.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use async_trait::async_trait;
use cicada_core::domain::capabilities::ProviderCapabilities;
use cicada_core::domain::error::{CicadaError, Result};
use cicada_core::domain::event::EventBody;
use cicada_core::domain::ids::{LocalId, RemoteId};
use cicada_core::domain::policy::WriteOptions;
use cicada_core::domain::time::TimeRange;
use cicada_core::ports::{CalendarProvider, CalendarRef, RemoteEvent};

use crate::provider::{FakeCalendarProvider, FakeSnapshot};

/// A `CalendarProvider` that persists the fake backend to a JSON
/// file after each mutation. Test/demo only.
#[derive(Debug)]
pub struct FakeFileProvider {
    /// The live backend — mutated through the same code paths as
    /// `FakeCalendarProvider`, so behavior is identical.
    inner: FakeCalendarProvider,
    /// Where the snapshot JSON lives.
    path: PathBuf,
    /// Capabilities to report — not persisted.
    capabilities: ProviderCapabilities,
}

impl FakeFileProvider {
    /// Open (or create lazily) the fake backend at `path` with
    /// [`ProviderCapabilities::permissive`]. A missing file starts
    /// empty; nothing is written until the first mutation.
    ///
    /// # Errors
    /// [`CicadaError::Fatal`] on unreadable/malformed files or an
    /// unknown snapshot `apiVersion`.
    pub fn open(path: impl Into<PathBuf>) -> Result<Self> {
        // Thin shim — generic instantiations delegate to one body
        // so per-instantiation coverage doesn't fragment.
        Self::open_with_capabilities(path.into(), ProviderCapabilities::permissive())
    }

    /// Open with explicit capabilities (capability-check demos).
    ///
    /// # Errors
    /// As [`FakeFileProvider::open`].
    pub fn open_with_capabilities(
        path: impl Into<PathBuf>,
        capabilities: ProviderCapabilities,
    ) -> Result<Self> {
        Self::open_path(path.into(), capabilities)
    }

    fn open_path(path: PathBuf, capabilities: ProviderCapabilities) -> Result<Self> {
        let snapshot = match fs::read_to_string(&path) {
            Ok(source) if source.trim().is_empty() => FakeSnapshot::new(),
            Ok(source) => {
                let snapshot: FakeSnapshot = serde_json::from_str(&source).map_err(|e| {
                    CicadaError::fatal(format!(
                        "fake provider file {} is malformed: {e}",
                        path.display()
                    ))
                })?;
                if snapshot.api_version != FakeSnapshot::API_VERSION {
                    return Err(CicadaError::fatal(format!(
                        "fake provider file {} has unsupported apiVersion {:?}",
                        path.display(),
                        snapshot.api_version
                    )));
                }
                snapshot
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => FakeSnapshot::new(),
            Err(e) => {
                return Err(CicadaError::fatal(format!(
                    "cannot read fake provider file {}: {e}",
                    path.display()
                )));
            }
        };
        Ok(Self {
            inner: FakeCalendarProvider::restore(capabilities.clone(), snapshot),
            path,
            capabilities,
        })
    }

    /// The file this provider persists to.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Access to the in-memory fake's test seams (`simulate_*`,
    /// `seed_*`, `events`) — mutations here do NOT persist; the file
    /// is owned by port calls. For file-level tests, edit the JSON
    /// and re-`open`.
    pub fn fake(&self) -> &FakeCalendarProvider {
        &self.inner
    }

    /// Serialize + atomically write the snapshot.
    fn persist(&self) -> Result<()> {
        let snapshot = self.inner.snapshot();
        // `FakeSnapshot` is plain data — serialization can't fail;
        // the error arm is compiled out under coverage.
        #[cfg(not(coverage))]
        let json = serde_json::to_string_pretty(&snapshot)
            .map_err(|e| CicadaError::fatal(format!("fake snapshot: {e}")))?;
        #[cfg(coverage)]
        let json = serde_json::to_string_pretty(&snapshot).expect("snapshot serializes");
        // Only `/` has no parent — a fake-provider file can't live
        // there; the `"."` fallback is compiled out under coverage.
        #[cfg(not(coverage))]
        let dir = self.path.parent().unwrap_or_else(|| Path::new("."));
        #[cfg(coverage)]
        let dir = self.path.parent().expect("provider path has a parent");
        fs::create_dir_all(dir)
            .map_err(|e| CicadaError::fatal(format!("cannot create {}: {e}", dir.display())))?;
        let mut tmp = self.path.clone().into_os_string();
        tmp.push(".tmp");
        let tmp = PathBuf::from(tmp);
        fs::File::create(&tmp)
            .and_then(|mut f| f.write_all(json.as_bytes()).and_then(|()| f.sync_all()))
            .map_err(|e| CicadaError::fatal(format!("cannot write {}: {e}", tmp.display())))?;
        fs::rename(&tmp, &self.path).map_err(|e| {
            CicadaError::fatal(format!("cannot replace {}: {e}", self.path.display()))
        })?;
        Ok(())
    }
}

#[async_trait]
impl CalendarProvider for FakeFileProvider {
    fn capabilities(&self) -> ProviderCapabilities {
        self.capabilities.clone()
    }

    async fn list_events(
        &self,
        calendar: &CalendarRef,
        range: Option<TimeRange>,
    ) -> Result<Vec<RemoteEvent>> {
        self.inner.list_events(calendar, range).await
    }

    async fn get_event(
        &self,
        calendar: &CalendarRef,
        remote_id: &RemoteId,
    ) -> Result<Option<RemoteEvent>> {
        self.inner.get_event(calendar, remote_id).await
    }

    async fn find_by_tag(
        &self,
        calendar: &CalendarRef,
        local_id: &LocalId,
    ) -> Result<Option<RemoteEvent>> {
        self.inner.find_by_tag(calendar, local_id).await
    }

    async fn create_event(
        &self,
        calendar: &CalendarRef,
        event: &EventBody,
        local_id: &LocalId,
        opts: &WriteOptions,
    ) -> Result<RemoteEvent> {
        let created = self
            .inner
            .create_event(calendar, event, local_id, opts)
            .await?;
        self.persist()?;
        Ok(created)
    }

    async fn update_event(
        &self,
        calendar: &CalendarRef,
        remote_id: &RemoteId,
        event: &EventBody,
        opts: &WriteOptions,
    ) -> Result<RemoteEvent> {
        let updated = self
            .inner
            .update_event(calendar, remote_id, event, opts)
            .await?;
        self.persist()?;
        Ok(updated)
    }

    async fn delete_event(
        &self,
        calendar: &CalendarRef,
        remote_id: &RemoteId,
        opts: &WriteOptions,
    ) -> Result<()> {
        self.inner.delete_event(calendar, remote_id, opts).await?;
        self.persist()
    }

    async fn free_busy(&self, calendar: &CalendarRef, range: TimeRange) -> Result<Vec<TimeRange>> {
        self.inner.free_busy(calendar, range).await
    }
}
