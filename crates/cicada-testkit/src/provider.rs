//! `FakeCalendarProvider` — an in-memory [`CalendarProvider`].
//!
//! Honors the tagging contract (ADR-0002 §2): every create writes
//! `cicada:id` + `cicada:managed` + `cicada:state` tags; `find_by_tag`
//! resolves the `cicada:id`; `cicada:state` is the applied event's
//! [`Event::content_hash`], matching what recorded state stores.
//!
//! Test seams:
//!
//! - configurable [`ProviderCapabilities`] for capability-check tests;
//! - idempotency-key dedupe on `create` (resume-safe apply tests);
//! - ETag-based conditional writes (`supports_conditional_writes`);
//! - [`FakeCalendarProvider::simulate_remote_edit`] /
//!   [`FakeCalendarProvider::simulate_remote_delete`] for out-of-band
//!   drift — they bump the etag and leave tags untouched, exactly
//!   like a human edit in the provider UI.

use std::collections::BTreeMap;
use std::sync::Mutex;

use async_trait::async_trait;
use cicada_core::domain::capabilities::ProviderCapabilities;
use cicada_core::domain::error::{CicadaError, Result};
use cicada_core::domain::event::{Event, EventBody, Transparency};
use cicada_core::domain::ids::{LocalId, RemoteId};
use cicada_core::domain::policy::WriteOptions;
use cicada_core::domain::recurrence::RecurrenceExpander;
use cicada_core::domain::time::TimeRange;
use cicada_core::ports::{CalendarProvider, CalendarRef, EventTags, RemoteEvent};
use jiff::Timestamp;
use serde::{Deserialize, Serialize};

/// In-memory provider. One instance can host many remote calendars.
#[derive(Debug)]
pub struct FakeCalendarProvider {
    /// Capabilities reported by `capabilities()`.
    capabilities: ProviderCapabilities,
    /// Mutable store — std `Mutex`; no `.await` is ever held across
    /// a lock.
    inner: Mutex<Inner>,
}

#[derive(Debug, Default)]
struct Inner {
    /// `calendarId → remoteId → event`.
    calendars: BTreeMap<String, BTreeMap<RemoteId, RemoteEvent>>,
    /// `(calendarId, idempotencyKey) → remoteId` — request dedupe so
    /// retried creates converge (ADR-0001 §4).
    idempotency: BTreeMap<(String, String), RemoteId>,
    /// Remote id sequence (`fake-1`, `fake-2`, …).
    next_remote: u64,
    /// ETag sequence (`etag-1`, …) — bumped on every mutation.
    next_etag: u64,
}

impl FakeCalendarProvider {
    /// A fake claiming full [`ProviderCapabilities::permissive`].
    pub fn new() -> Self {
        Self::with_capabilities(ProviderCapabilities::permissive())
    }

    /// A fake reporting `capabilities`.
    pub fn with_capabilities(capabilities: ProviderCapabilities) -> Self {
        Self {
            capabilities,
            inner: Mutex::new(Inner::default()),
        }
    }

    // ---------- test seams ----------

    /// Seed a live event as if `local_id` had been applied:
    /// managed tags + a `cicada:state` hash + a fresh etag.
    pub fn seed_managed(
        &self,
        calendar: &CalendarRef,
        local_id: &LocalId,
        body: EventBody,
    ) -> RemoteEvent {
        let state_hash = Event::new(local_id.clone(), body.clone())
            .content_hash()
            .ok();
        let mut inner = self.lock();
        let remote_id = inner.fresh_remote_id();
        let event = RemoteEvent {
            remote_id: remote_id.clone(),
            tags: EventTags {
                local_id: Some(local_id.clone()),
                managed: true,
                state_hash,
            },
            etag: Some(inner.fresh_etag()),
            updated: Some(Timestamp::now()),
            body,
        };
        inner
            .calendars
            .entry(calendar.as_str().to_owned())
            .or_default()
            .insert(remote_id, event.clone());
        event
    }

    /// Seed an untracked event — a human's event the managed
    /// boundary must never touch (no `cicada:*` tags).
    pub fn seed_foreign(&self, calendar: &CalendarRef, body: EventBody) -> RemoteEvent {
        let mut inner = self.lock();
        let remote_id = inner.fresh_remote_id();
        let event = RemoteEvent {
            remote_id: remote_id.clone(),
            tags: EventTags::default(),
            etag: Some(inner.fresh_etag()),
            updated: Some(Timestamp::now()),
            body,
        };
        inner
            .calendars
            .entry(calendar.as_str().to_owned())
            .or_default()
            .insert(remote_id, event.clone());
        event
    }

    /// Insert a fully-formed [`RemoteEvent`] verbatim — for cases
    /// the seeds don't shape (stale tags, missing etags).
    pub fn seed(&self, calendar: &CalendarRef, event: RemoteEvent) {
        self.lock()
            .calendars
            .entry(calendar.as_str().to_owned())
            .or_default()
            .insert(event.remote_id.clone(), event);
    }

    /// Out-of-band edit: mutate the body, bump the etag, leave the
    /// `cicada:*` tags alone — exactly what the provider UI does.
    /// `false` when the remote id doesn't exist.
    pub fn simulate_remote_edit(
        &self,
        calendar: &CalendarRef,
        remote_id: &RemoteId,
        edit: impl FnOnce(&mut EventBody),
    ) -> bool {
        let mut inner = self.lock();
        let exists = inner
            .calendars
            .get(calendar.as_str())
            .is_some_and(|events| events.contains_key(remote_id));
        if !exists {
            return false;
        }
        let etag = inner.fresh_etag();
        // `exists` was verified above — `get_mut` can't miss; the
        // `if let` else arm is compiled out under coverage.
        #[cfg(not(coverage))]
        if let Some(event) = inner
            .calendars
            .get_mut(calendar.as_str())
            .and_then(|events| events.get_mut(remote_id))
        {
            edit(&mut event.body);
            event.etag = Some(etag);
            event.updated = Some(Timestamp::now());
        }
        #[cfg(coverage)]
        {
            let event = inner
                .calendars
                .get_mut(calendar.as_str())
                .and_then(|events| events.get_mut(remote_id))
                .expect("exists checked above");
            edit(&mut event.body);
            event.etag = Some(etag);
            event.updated = Some(Timestamp::now());
        }
        true
    }

    /// Out-of-band delete. `false` when the remote id doesn't exist.
    pub fn simulate_remote_delete(&self, calendar: &CalendarRef, remote_id: &RemoteId) -> bool {
        self.lock()
            .calendars
            .get_mut(calendar.as_str())
            .is_some_and(|events| events.remove(remote_id).is_some())
    }

    /// Snapshot of every event on `calendar` (empty for unknown ids).
    pub fn events(&self, calendar: &CalendarRef) -> Vec<RemoteEvent> {
        self.lock()
            .calendars
            .get(calendar.as_str())
            .map_or_else(Vec::new, |events| events.values().cloned().collect())
    }

    /// Export the whole backend as a serializable [`FakeSnapshot`] —
    /// `FakeFileProvider` persists this between CLI invocations.
    pub fn snapshot(&self) -> FakeSnapshot {
        let inner = self.lock();
        let mut idempotency: BTreeMap<String, BTreeMap<String, RemoteId>> = BTreeMap::new();
        for ((calendar, key), remote_id) in &inner.idempotency {
            idempotency
                .entry(calendar.clone())
                .or_default()
                .insert(key.clone(), remote_id.clone());
        }
        FakeSnapshot {
            api_version: FakeSnapshot::API_VERSION.to_owned(),
            calendars: inner.calendars.clone(),
            idempotency,
            next_remote: inner.next_remote,
            next_etag: inner.next_etag,
        }
    }

    /// Rebuild a fake from a [`FakeSnapshot`] — `capabilities` are
    /// not persisted (they're adapter claims, not state).
    pub fn restore(capabilities: ProviderCapabilities, snapshot: FakeSnapshot) -> Self {
        let mut idempotency = BTreeMap::new();
        for (calendar, entries) in snapshot.idempotency {
            for (key, remote_id) in entries {
                idempotency.insert((calendar.clone(), key), remote_id);
            }
        }
        Self {
            capabilities,
            inner: Mutex::new(Inner {
                calendars: snapshot.calendars,
                idempotency,
                next_remote: snapshot.next_remote,
                next_etag: snapshot.next_etag,
            }),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        // Poisoning means a test panicked mid-mutation; recovering
        // the data is still the most useful behavior for a fake.
        self.inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

impl Default for FakeCalendarProvider {
    fn default() -> Self {
        Self::new()
    }
}

/// The whole fake backend, serializable — `FakeFileProvider`
/// persists this to a JSON file so the CLI's apply/drift/refresh
/// lifecycle survives across processes (test/demo only).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FakeSnapshot {
    /// Format version — always [`FakeSnapshot::API_VERSION`].
    pub api_version: String,
    /// `calendarId → remoteId → event`.
    #[serde(default)]
    pub calendars: BTreeMap<String, BTreeMap<RemoteId, RemoteEvent>>,
    /// `calendarId → idempotencyKey → remoteId` — the request dedupe
    /// map (tuple keys don't serialize as JSON object keys, so it
    /// nests instead).
    #[serde(default)]
    pub idempotency: BTreeMap<String, BTreeMap<String, RemoteId>>,
    /// Remote id counter (`fake-N`).
    #[serde(default)]
    pub next_remote: u64,
    /// ETag counter (`etag-N`).
    #[serde(default)]
    pub next_etag: u64,
}

impl FakeSnapshot {
    /// The only snapshot format version written.
    pub const API_VERSION: &'static str = "cicada/fake-provider-v1";

    /// An empty, correctly-versioned snapshot.
    pub fn new() -> Self {
        Self {
            api_version: Self::API_VERSION.to_owned(),
            calendars: BTreeMap::new(),
            idempotency: BTreeMap::new(),
            next_remote: 0,
            next_etag: 0,
        }
    }
}

impl Default for FakeSnapshot {
    fn default() -> Self {
        Self::new()
    }
}

impl Inner {
    fn fresh_remote_id(&mut self) -> RemoteId {
        self.next_remote += 1;
        RemoteId::new(format!("fake-{}", self.next_remote))
    }

    fn fresh_etag(&mut self) -> String {
        self.next_etag += 1;
        format!("etag-{}", self.next_etag)
    }

    fn events(&self, calendar: &CalendarRef) -> impl Iterator<Item = &RemoteEvent> {
        self.calendars
            .get(calendar.as_str())
            .into_iter()
            .flat_map(BTreeMap::values)
    }
}

/// Does `body` overlap `range`? Non-recurring events compare their
/// materialized span; recurring events expand (unexpandable rules
/// conservatively count as overlapping).
fn overlaps(body: &EventBody, range: &TimeRange) -> bool {
    if let Some(rule) = &body.recurrence {
        return match RecurrenceExpander::default().expand(rule, body.time.start(), *range) {
            Ok(occs) => !occs.is_empty(),
            Err(_) => true,
        };
    }
    let Ok(start) = body.time.start().to_timestamp() else {
        return true;
    };
    let Ok(end) = body.time.end().and_then(|t| t.to_timestamp()) else {
        return true;
    };
    start < range.end && end > range.start
}

#[async_trait]
impl CalendarProvider for FakeCalendarProvider {
    fn capabilities(&self) -> ProviderCapabilities {
        self.capabilities.clone()
    }

    async fn list_events(
        &self,
        calendar: &CalendarRef,
        range: Option<TimeRange>,
    ) -> Result<Vec<RemoteEvent>> {
        Ok(self
            .lock()
            .events(calendar)
            .filter(|e| range.is_none_or(|r| overlaps(&e.body, &r)))
            .cloned()
            .collect())
    }

    async fn get_event(
        &self,
        calendar: &CalendarRef,
        remote_id: &RemoteId,
    ) -> Result<Option<RemoteEvent>> {
        Ok(self
            .lock()
            .events(calendar)
            .find(|e| e.remote_id == *remote_id)
            .cloned())
    }

    async fn find_by_tag(
        &self,
        calendar: &CalendarRef,
        local_id: &LocalId,
    ) -> Result<Option<RemoteEvent>> {
        Ok(self
            .lock()
            .events(calendar)
            .find(|e| e.tags.local_id.as_ref() == Some(local_id))
            .cloned())
    }

    async fn create_event(
        &self,
        calendar: &CalendarRef,
        event: &EventBody,
        local_id: &LocalId,
        opts: &WriteOptions,
    ) -> Result<RemoteEvent> {
        let mut inner = self.lock();
        if let Some(key) = &opts.idempotency_key {
            let seen = inner
                .idempotency
                .get(&(calendar.as_str().to_owned(), key.clone()))
                .and_then(|id| {
                    inner
                        .calendars
                        .get(calendar.as_str())
                        .and_then(|events| events.get(id))
                })
                .cloned();
            if let Some(existing) = seen {
                return Ok(existing);
            }
        }
        let remote_id = inner.fresh_remote_id();
        let state_hash = Event::new(local_id.clone(), event.clone())
            .content_hash()
            .ok();
        let created = RemoteEvent {
            remote_id: remote_id.clone(),
            tags: EventTags {
                local_id: Some(local_id.clone()),
                managed: true,
                state_hash,
            },
            etag: Some(inner.fresh_etag()),
            updated: Some(Timestamp::now()),
            body: event.clone(),
        };
        inner
            .calendars
            .entry(calendar.as_str().to_owned())
            .or_default()
            .insert(remote_id.clone(), created.clone());
        if let Some(key) = &opts.idempotency_key {
            inner
                .idempotency
                .insert((calendar.as_str().to_owned(), key.clone()), remote_id);
        }
        Ok(created)
    }

    async fn update_event(
        &self,
        calendar: &CalendarRef,
        remote_id: &RemoteId,
        event: &EventBody,
        opts: &WriteOptions,
    ) -> Result<RemoteEvent> {
        let mut inner = self.lock();
        let Some(existing) = inner
            .calendars
            .get(calendar.as_str())
            .and_then(|events| events.get(remote_id))
            .cloned()
        else {
            return Err(CicadaError::conflict(format!(
                "remote event {remote_id} no longer exists — refresh and re-plan"
            )));
        };
        if let (Some(expected), Some(current)) = (&opts.etag, &existing.etag) {
            if expected != current {
                return Err(CicadaError::conflict(format!(
                    "etag mismatch on {remote_id}: expected {expected}, live is {current}"
                )));
            }
        }
        // cicada rewrites the managed tags it owns; local_id can't
        // change on update (it isn't a parameter).
        let state_hash = existing
            .tags
            .local_id
            .as_ref()
            .and_then(|id| Event::new(id.clone(), event.clone()).content_hash().ok());
        let updated = RemoteEvent {
            tags: EventTags {
                state_hash: state_hash.or(existing.tags.state_hash.clone()),
                ..existing.tags.clone()
            },
            etag: Some(inner.fresh_etag()),
            updated: Some(Timestamp::now()),
            body: event.clone(),
            ..existing
        };
        inner
            .calendars
            .entry(calendar.as_str().to_owned())
            .or_default()
            .insert(remote_id.clone(), updated.clone());
        Ok(updated)
    }

    async fn delete_event(
        &self,
        calendar: &CalendarRef,
        remote_id: &RemoteId,
        opts: &WriteOptions,
    ) -> Result<()> {
        let mut inner = self.lock();
        let existing = inner
            .calendars
            .get(calendar.as_str())
            .and_then(|events| events.get(remote_id));
        let Some(existing) = existing else {
            return Ok(()); // deletes are idempotent (ADR-0002 §3)
        };
        if let (Some(expected), Some(current)) = (&opts.etag, &existing.etag) {
            if expected != current {
                return Err(CicadaError::conflict(format!(
                    "etag mismatch on {remote_id}: expected {expected}, live is {current}"
                )));
            }
        }
        if let Some(events) = inner.calendars.get_mut(calendar.as_str()) {
            events.remove(remote_id);
        }
        Ok(())
    }

    async fn free_busy(&self, calendar: &CalendarRef, range: TimeRange) -> Result<Vec<TimeRange>> {
        if !self.capabilities.supports_free_busy {
            return Err(CicadaError::fatal(
                "the fake provider has freeBusy disabled (supports_free_busy = false)",
            ));
        }
        let mut busy = Vec::new();
        for event in self.events(calendar) {
            if event.body.transparency != Transparency::Opaque {
                continue;
            }
            let Ok(duration) = event.body.time.duration() else {
                continue;
            };
            if let Some(rule) = &event.body.recurrence {
                let Ok(occs) =
                    RecurrenceExpander::default().expand(rule, event.body.time.start(), range)
                else {
                    continue;
                };
                for occ in occs {
                    if let Ok(span) = TimeRange::new(occ.instant, occ.instant + duration) {
                        busy.push(span);
                    }
                }
            } else if let (Ok(s), Ok(e)) = (
                event.body.time.start().to_timestamp(),
                event.body.time.end().and_then(|t| t.to_timestamp()),
            ) {
                if let Ok(span) = TimeRange::new(s, e) {
                    if span.overlaps(range) {
                        busy.push(span);
                    }
                }
            }
        }
        busy.sort_by_key(|r| r.start);
        Ok(busy)
    }
}
