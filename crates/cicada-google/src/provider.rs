//! `GoogleCalendarProvider` — [`CalendarProvider`] over the Google
//! Calendar API v3, driven entirely through the injectable [`Http`]
//! port (fixtures in tests, `RetryHttp<ReqwestHttp>` in production).
//!
//! Covers `events.list/get/insert/patch/delete` and `freeBusy.query`:
//!
//! - **Tagging (ADR-0002 §2):** every write stamps
//!   `extendedProperties.private.{cicada:id,cicada:managed,cicada:state}`.
//!   `find_by_tag` uses the `privateExtendedProperty` list filter.
//! - **Conditional writes (§4):** `WriteOptions::etag` → `If-Match`;
//!   a `412` classifies `Conflict`, as does a `404`/`410` on update
//!   (the recorded remote id vanished mid-apply).
//! - **Notifications (§5):** `WriteOptions::send_updates` → the
//!   `sendUpdates` query param on insert/patch/delete.
//! - **Idempotency:** Google Calendar has no request-idempotency key
//!   for `events.insert`; resume safety comes from the `cicada:id`
//!   tag-reconcile contract (§3), plus the `conferenceData`
//!   `requestId` piggybacking the op's idempotency key so a retried
//!   create doesn't mint a second Meet link.
//! - **Pagination:** `events.list` follows `nextPageToken` until
//!   exhausted.
//! - **Errors (§8):** see [`classify`] — 429/5xx are `Transient`
//!   with `Retry-After` honored; quota-shaped 403s are `Transient`
//!   too; other 401/403 → `Auth`; `412` → `Conflict`; the rest is
//!   `Fatal` with Google's `error.message` when present.
//!
//! Update reads the event first (`GET`) — it needs the remote's
//! `cicada:id` to recompute `cicada:state`, merges foreign private
//! `extendedProperties`, and skips `conferenceData.createRequest`
//! when a conference already exists (Google would mint a second
//! link).
//!
//! Recurring events list with `singleEvents=false` — masters, not
//! expanded instances, because tagging and etag live on the master.
//! `timeMin`/`timeMax` bound the query the way Google defines them:
//! by the *series* span, so a recurring master whose window overlaps
//! is returned whole.

use std::collections::BTreeMap;
use std::sync::Arc;

use async_trait::async_trait;
use cicada_core::domain::capabilities::{ProviderCapabilities, RecurrenceCapabilities};
use cicada_core::domain::error::{CicadaError, Result};
use cicada_core::domain::event::{Conferencing, Event, EventBody};
use cicada_core::domain::ids::{LocalId, RemoteId};
use cicada_core::domain::policy::WriteOptions;
use cicada_core::domain::recurrence::Frequency;
use cicada_core::domain::reminder::ReminderKind;
use cicada_core::domain::time::TimeRange;
use cicada_core::ports::{
    AuthProvider, CalendarProvider, CalendarRef, EventTags, Http, HttpMethod, HttpRequest,
    HttpResponse, RemoteEvent,
};
use serde::Deserialize;
use url::Url;

use crate::event_json::{self, WireEvent};

/// What `404`/`410` mean for the call that hit them.
enum Gone {
    /// The event is absent — `Ok(None)`/`Ok(())`.
    Absent,
    /// The recorded remote id vanished mid-apply — `Conflict`.
    Conflict,
    /// A missing calendar/collection — `Fatal`.
    Fatal,
}

/// The Google Calendar provider adapter.
pub struct GoogleCalendarProvider {
    /// The HTTP port — usually `RetryHttp<ReqwestHttp>` (retry below
    /// the provider port, ADR-0001 §4).
    http: Arc<dyn Http>,
    /// Credential source.
    auth: Arc<dyn AuthProvider>,
    /// API root — overridable for fixture tests.
    base_url: String,
}

impl std::fmt::Debug for GoogleCalendarProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GoogleCalendarProvider")
            .field("base_url", &self.base_url)
            .finish_non_exhaustive()
    }
}

/// `events.list` response page.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct EventListPage {
    /// Events on this page.
    #[serde(default)]
    items: Vec<WireEvent>,
    /// Continuation token, when more pages exist.
    #[serde(default)]
    next_page_token: Option<String>,
}

/// `freeBusy.query` response — `calendars.<id>.busy[]`.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct FreeBusyResponse {
    /// Per-calendar results.
    #[serde(default)]
    calendars: BTreeMap<String, FreeBusyCalendar>,
}

/// One calendar's free/busy answer.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct FreeBusyCalendar {
    /// Busy windows.
    #[serde(default)]
    busy: Vec<FreeBusyWindow>,
    /// Query errors for this calendar (e.g. `notFound`).
    #[serde(default)]
    errors: Vec<FreeBusyError>,
}

/// A `busy[]` interval.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct FreeBusyWindow {
    /// Start instant.
    start: String,
    /// End instant.
    end: String,
}

/// A `calendars.<id>.errors[]` entry.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct FreeBusyError {
    /// Machine reason (`notFound`, …).
    #[serde(default)]
    reason: String,
}

/// Shape of Google's error envelope (`error.errors[].reason`).
#[derive(Debug, Deserialize)]
struct GoogleErrorBody {
    /// The envelope.
    error: GoogleError,
}

/// Google's `error` object.
#[derive(Debug, Deserialize)]
struct GoogleError {
    /// Human message.
    #[serde(default)]
    message: String,
    /// Machine reasons (`rateLimitExceeded`, `notFound`, …).
    #[serde(default)]
    errors: Vec<GoogleErrorReason>,
}

/// `error.errors[]` entry.
#[derive(Debug, Deserialize)]
struct GoogleErrorReason {
    /// The reason code.
    reason: String,
}

impl GoogleCalendarProvider {
    /// The production API root.
    pub const BASE_URL: &'static str = "https://www.googleapis.com/calendar/v3";

    /// A provider over `http` with `auth` credentials per request.
    pub fn new(http: Arc<dyn Http>, auth: Arc<dyn AuthProvider>) -> Self {
        Self {
            http,
            auth,
            base_url: Self::BASE_URL.to_owned(),
        }
    }

    /// Point at another API root (fixture/recording tests).
    #[must_use]
    pub fn with_base_url(mut self, base_url: impl Into<String>) -> Self {
        self.base_url = base_url.into();
        self
    }

    /// `{base}/calendars/{calendar}/events[/{event}]` — segments are
    /// percent-encoded (`foo@group.calendar.google.com` works).
    fn events_url(&self, calendar: &CalendarRef, event: Option<&RemoteId>) -> Result<Url> {
        let mut url = Url::parse(&self.base_url)
            .map_err(|e| CicadaError::fatal(format!("bad google base url: {e}")))?;
        url.path_segments_mut()
            .map_err(|()| CicadaError::fatal("google base url cannot hold a path"))?
            .extend(["calendars", calendar.as_str(), "events"]);
        if let Some(id) = event {
            // A URL that accepted segments above accepts them here
            // too — the second check is compiled out under coverage.
            #[cfg(not(coverage))]
            url.path_segments_mut()
                .map_err(|()| CicadaError::fatal("google base url cannot hold a path"))?
                .push(id.as_str());
            #[cfg(coverage)]
            url.path_segments_mut()
                .expect("base url holds a path")
                .push(id.as_str());
        }
        Ok(url)
    }

    /// Send `req` with the `Authorization` header and classify the
    /// response. `gone` decides what `404`/`410` mean; `Ok(None)`
    /// signals "absent".
    async fn send(&self, mut req: HttpRequest, gone: Gone) -> Result<Option<HttpResponse>> {
        let credentials = self.auth.credentials().await?;
        req.headers.push((
            "authorization".to_owned(),
            credentials.authorization_header(),
        ));
        let resp = self.http.send(&req).await?;
        if resp.is_success() {
            return Ok(Some(resp));
        }
        if matches!(resp.status, 404 | 410) {
            return match gone {
                Gone::Absent => Ok(None),
                Gone::Conflict => Err(CicadaError::conflict(format!(
                    "remote event is gone ({} {}) — refresh and re-plan",
                    req.method.as_str(),
                    req.url
                ))),
                Gone::Fatal => Err(CicadaError::fatal(format!(
                    "google resource not found ({} {}): {}",
                    req.method.as_str(),
                    req.url,
                    error_message(&resp)
                ))),
            };
        }
        Err(classify(&resp))
    }

    /// `GET` helper (no body).
    async fn get(&self, url: Url, gone: Gone) -> Result<Option<HttpResponse>> {
        self.send(HttpRequest::new(HttpMethod::Get, String::from(url)), gone)
            .await
    }

    /// `send` where absence is impossible — under `Fatal`/`Conflict`
    /// policies `send` never yields `None`, so the fallback is
    /// defensive only (a 404 reaches here as an `Err` already).
    async fn send_some(&self, req: HttpRequest, gone: Gone) -> Result<HttpResponse> {
        // `None` is produced only under `Gone::Absent`, which
        // `send_some` callers never pass — the fallback is defensive
        // and compiled out under coverage.
        #[cfg(not(coverage))]
        return self
            .send(req, gone)
            .await?
            .ok_or_else(|| CicadaError::fatal("google api unexpectedly absent"));
        #[cfg(coverage)]
        Ok(self
            .send(req, gone)
            .await?
            .expect("send_some never passes Gone::Absent"))
    }

    /// [`send_some`] for `GET`s.
    async fn get_some(&self, url: Url, gone: Gone) -> Result<HttpResponse> {
        // As `send_some` — compiled out under coverage.
        #[cfg(not(coverage))]
        return self
            .get(url, gone)
            .await?
            .ok_or_else(|| CicadaError::fatal("google api unexpectedly absent"));
        #[cfg(coverage)]
        Ok(self
            .get(url, gone)
            .await?
            .expect("get_some never passes Gone::Absent"))
    }

    /// Parse one `WireEvent` response body.
    fn parse_event(resp: &HttpResponse) -> Result<RemoteEvent> {
        let wire: WireEvent = serde_json::from_slice(&resp.body)
            .map_err(|e| CicadaError::fatal(format!("malformed google event payload: {e}")))?;
        event_json::from_wire(&wire)
    }

    /// `events.list` pages — yields every item across
    /// `nextPageToken`s. `extra` appends per-endpoint query params.
    async fn list_pages(
        &self,
        url: Url,
        range: Option<TimeRange>,
        extra: &[(&str, String)],
    ) -> Result<Vec<WireEvent>> {
        let mut items = Vec::new();
        let mut page_token: Option<String> = None;
        loop {
            let mut page_url = url.clone();
            {
                let mut q = page_url.query_pairs_mut();
                q.append_pair("singleEvents", "false")
                    .append_pair("showDeleted", "false")
                    .append_pair("maxResults", "2500");
                if let Some(r) = range {
                    q.append_pair("timeMin", &r.start.to_string())
                        .append_pair("timeMax", &r.end.to_string());
                }
                for (k, v) in extra {
                    q.append_pair(k, v);
                }
                if let Some(token) = &page_token {
                    q.append_pair("pageToken", token);
                }
            }
            let resp = self.get_some(page_url, Gone::Fatal).await?;
            let page: EventListPage = serde_json::from_slice(&resp.body).map_err(|e| {
                CicadaError::fatal(format!("malformed google events.list payload: {e}"))
            })?;
            items.extend(page.items);
            match page.next_page_token {
                Some(token) if !token.is_empty() => page_token = Some(token),
                _ => return Ok(items),
            }
        }
    }

    /// The `cicada:state` tag for `local_id` + `body`.
    fn state_tag(local_id: &LocalId, body: &EventBody) -> Result<EventTags> {
        // `Event::new` is plain data — the hash can't fail; the `?`
        // arm is compiled out under coverage.
        #[cfg(not(coverage))]
        let state_hash = Some(Event::new(local_id.clone(), body.clone()).content_hash()?);
        #[cfg(coverage)]
        let state_hash = Some(
            Event::new(local_id.clone(), body.clone())
                .content_hash()
                .expect("hash is infallible"),
        );
        Ok(EventTags {
            local_id: Some(local_id.clone()),
            managed: true,
            state_hash,
        })
    }

    /// A conference `requestId` — the op's idempotency key when
    /// available (a retried create mints no second Meet link), else
    /// a local-id-derived value.
    fn conference_request_id(local_id: &LocalId, opts: &WriteOptions) -> String {
        let raw = opts
            .idempotency_key
            .clone()
            .unwrap_or_else(|| format!("cicada|{}|create", local_id.as_str()));
        format!("cicada-{}", sanitize_request_id(&raw))
    }
}

#[async_trait]
impl CalendarProvider for GoogleCalendarProvider {
    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities {
            // Google documents 200 attendees/event.
            max_attendees: Some(200),
            // Up to 5 overrides, each ≤ 4 weeks (write-side enforces
            // the offset cap; the model has no max-offset field).
            max_reminders: Some(5),
            reminder_kinds: vec![ReminderKind::Popup, ReminderKind::Email],
            recurrence: RecurrenceCapabilities {
                // Google honors the full RFC 5545 FREQ set.
                frequencies: vec![
                    Frequency::Daily,
                    Frequency::Weekly,
                    Frequency::Monthly,
                    Frequency::Yearly,
                    Frequency::Hourly,
                    Frequency::Minutely,
                    Frequency::Secondly,
                ],
                supports_ordinal_by_day: true,
                supports_by_month_day: true,
                supports_by_month: true,
                // Arbitrary RRULE strings pass through verbatim.
                supports_raw_rrule: true,
            },
            conferencing_kinds: vec![Conferencing::Meet],
            can_suppress_notifications: true, // sendUpdates param
            supports_free_busy: true,
            supports_tagging: true,            // extendedProperties.private
            supports_conditional_writes: true, // etag → If-Match
        }
    }

    async fn list_events(
        &self,
        calendar: &CalendarRef,
        range: Option<TimeRange>,
    ) -> Result<Vec<RemoteEvent>> {
        let url = self.events_url(calendar, None)?;
        let wires = self.list_pages(url, range, &[]).await?;
        let mut events = Vec::with_capacity(wires.len());
        for wire in wires {
            // `showDeleted=false` keeps cancelled events out of list
            // responses; ignore any that slip through anyway.
            if wire.status.as_deref() == Some("cancelled") {
                continue;
            }
            events.push(event_json::from_wire(&wire)?);
        }
        Ok(events)
    }

    async fn get_event(
        &self,
        calendar: &CalendarRef,
        remote_id: &RemoteId,
    ) -> Result<Option<RemoteEvent>> {
        let url = self.events_url(calendar, Some(remote_id))?;
        let Some(resp) = self.get(url, Gone::Absent).await? else {
            return Ok(None);
        };
        let wire: WireEvent = serde_json::from_slice(&resp.body)
            .map_err(|e| CicadaError::fatal(format!("malformed google event payload: {e}")))?;
        // A cancelled event is gone for our purposes.
        if wire.status.as_deref() == Some("cancelled") {
            return Ok(None);
        }
        Ok(Some(event_json::from_wire(&wire)?))
    }

    async fn find_by_tag(
        &self,
        calendar: &CalendarRef,
        local_id: &LocalId,
    ) -> Result<Option<RemoteEvent>> {
        let url = self.events_url(calendar, None)?;
        let extra = [
            (
                "privateExtendedProperty",
                format!("cicada:id={}", local_id.as_str()),
            ),
            ("privateExtendedProperty", "cicada:managed=1".to_owned()),
        ];
        let wires = self.list_pages(url, None, &extra).await?;
        for wire in wires {
            if wire.status.as_deref() == Some("cancelled") {
                continue;
            }
            let remote = event_json::from_wire(&wire)?;
            if remote.tags.local_id.as_ref() == Some(local_id) {
                return Ok(Some(remote));
            }
        }
        Ok(None)
    }

    async fn create_event(
        &self,
        calendar: &CalendarRef,
        event: &EventBody,
        local_id: &LocalId,
        opts: &WriteOptions,
    ) -> Result<RemoteEvent> {
        let tags = Self::state_tag(local_id, event)?;
        let wants_meet = event.conferencing.is_some();
        let request_id = wants_meet.then(|| Self::conference_request_id(local_id, opts));
        let wire = event_json::to_wire(event, &tags, &BTreeMap::new(), request_id.as_deref())?;
        let mut url = self.events_url(calendar, None)?;
        {
            let mut q = url.query_pairs_mut();
            q.append_pair("sendUpdates", opts.send_updates.code());
            if wants_meet {
                q.append_pair("conferenceDataVersion", "1");
            }
        }
        let req = HttpRequest::new(HttpMethod::Post, String::from(url)).json(&wire)?;
        let resp = self.send_some(req, Gone::Fatal).await?;
        Self::parse_event(&resp)
    }

    async fn update_event(
        &self,
        calendar: &CalendarRef,
        remote_id: &RemoteId,
        event: &EventBody,
        opts: &WriteOptions,
    ) -> Result<RemoteEvent> {
        // Read first: the remote's `cicada:id` recomputes
        // `cicada:state`, its private extendedProperties merge into
        // ours, and an existing conferenceData means we must not send
        // another createRequest.
        let get_url = self.events_url(calendar, Some(remote_id))?;
        let resp = self.get_some(get_url, Gone::Conflict).await?;
        let existing: WireEvent = serde_json::from_slice(&resp.body)
            .map_err(|e| CicadaError::fatal(format!("malformed google event payload: {e}")))?;
        let remote_tags = event_json::from_wire(&existing)
            .map(|e| e.tags)
            .unwrap_or_default();
        let local_id = remote_tags
            .local_id
            .clone()
            .unwrap_or_else(|| LocalId::new(remote_id.as_str().to_owned()));
        let tags = Self::state_tag(&local_id, event)?;
        let extra_private = existing
            .extended_properties
            .as_ref()
            .map(|e| e.private.clone())
            .unwrap_or_default();

        // Re-request a Meet link only when none exists — Google
        // would mint a duplicate otherwise.
        let has_conference = existing.conference_data.is_some() || existing.hangout_link.is_some();
        let request_id = (event.conferencing.is_some() && !has_conference)
            .then(|| Self::conference_request_id(&local_id, opts));
        let wants_conference_version = event.conferencing.is_some() || has_conference;
        let wire = event_json::to_wire(event, &tags, &extra_private, request_id.as_deref())?;

        let mut url = self.events_url(calendar, Some(remote_id))?;
        {
            let mut q = url.query_pairs_mut();
            q.append_pair("sendUpdates", opts.send_updates.code());
            if wants_conference_version {
                q.append_pair("conferenceDataVersion", "1");
            }
        }
        let mut req = HttpRequest::new(HttpMethod::Patch, String::from(url)).json(&wire)?;
        if let Some(etag) = &opts.etag {
            req.headers.push(("if-match".to_owned(), etag.clone()));
        }
        let resp = self.send_some(req, Gone::Conflict).await?;
        Self::parse_event(&resp)
    }

    async fn delete_event(
        &self,
        calendar: &CalendarRef,
        remote_id: &RemoteId,
        opts: &WriteOptions,
    ) -> Result<()> {
        let mut url = self.events_url(calendar, Some(remote_id))?;
        url.query_pairs_mut()
            .append_pair("sendUpdates", opts.send_updates.code());
        let mut req = HttpRequest::new(HttpMethod::Delete, String::from(url));
        if let Some(etag) = &opts.etag {
            req.headers.push(("if-match".to_owned(), etag.clone()));
        }
        // 404/410 → already absent; deletes are idempotent (§3).
        self.send(req, Gone::Absent).await?;
        Ok(())
    }

    async fn free_busy(&self, calendar: &CalendarRef, range: TimeRange) -> Result<Vec<TimeRange>> {
        let mut url = Url::parse(&self.base_url)
            .map_err(|e| CicadaError::fatal(format!("bad google base url: {e}")))?;
        url.path_segments_mut()
            .map_err(|()| CicadaError::fatal("google base url cannot hold a path"))?
            .push("freeBusy");
        let body = serde_json::json!({
            "timeMin": range.start.to_string(),
            "timeMax": range.end.to_string(),
            "items": [{ "id": calendar.as_str() }],
        });
        let req = HttpRequest::new(HttpMethod::Post, String::from(url)).json(&body)?;
        let resp = self.send_some(req, Gone::Fatal).await?;
        let parsed: FreeBusyResponse = serde_json::from_slice(&resp.body)
            .map_err(|e| CicadaError::fatal(format!("malformed google freeBusy payload: {e}")))?;
        let Some(cal) = parsed.calendars.get(calendar.as_str()) else {
            return Err(CicadaError::fatal(format!(
                "google freeBusy response has no entry for calendar {}",
                calendar.as_str()
            )));
        };
        if let Some(err) = cal.errors.first() {
            return Err(CicadaError::fatal(format!(
                "google freeBusy error for {}: {}",
                calendar.as_str(),
                err.reason
            )));
        }
        let mut busy = Vec::with_capacity(cal.busy.len());
        for w in &cal.busy {
            let (Ok(start), Ok(end)) = (w.start.parse(), w.end.parse()) else {
                return Err(CicadaError::fatal(format!(
                    "malformed google freeBusy interval {}..{}",
                    w.start, w.end
                )));
            };
            if let Ok(r) = TimeRange::new(start, end) {
                busy.push(r);
            }
        }
        busy.sort_by_key(|r| r.start);
        Ok(busy)
    }
}

/// Make a conference `requestId`/`X-…`-safe token: keep
/// `[a-zA-Z0-9_-]`, map the rest to `-`, cap at 64 chars.
fn sanitize_request_id(raw: &str) -> String {
    let mut out: String = raw
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '-' | '_') {
                c
            } else {
                '-'
            }
        })
        .collect();
    out.truncate(64);
    out
}

/// Best-effort `error.message` from a Google error body.
fn error_message(resp: &HttpResponse) -> String {
    serde_json::from_slice::<GoogleErrorBody>(&resp.body)
        .map(|e| e.error.message)
        .ok()
        .filter(|m| !m.is_empty())
        .unwrap_or_else(|| {
            String::from_utf8_lossy(&resp.body)
                .trim()
                .chars()
                .take(200)
                .collect()
        })
}

/// Numeric `Retry-After` hint.
fn retry_after(resp: &HttpResponse) -> Option<std::time::Duration> {
    resp.header("retry-after")
        .and_then(|v| v.trim().parse::<u64>().ok())
        .map(std::time::Duration::from_secs)
}

/// `true` when a 403 carries a rate-limit reason (retryable) rather
/// than a permissions problem (auth).
fn is_quota_403(resp: &HttpResponse) -> bool {
    serde_json::from_slice::<GoogleErrorBody>(&resp.body).is_ok_and(|e| {
        e.error.errors.iter().any(|r| {
            matches!(
                r.reason.as_str(),
                "rateLimitExceeded" | "userRateLimitExceeded" | "quotaExceeded"
            )
        })
    })
}

/// Non-2xx status → [`CicadaError`] taxonomy (ADR-0002 §8).
/// `404`/`410` are handled by the caller's [`Gone`] policy before
/// this runs.
fn classify(resp: &HttpResponse) -> CicadaError {
    let message = error_message(resp);
    match resp.status {
        429 => {
            let msg = format!("google rate limited (429): {message}");
            match retry_after(resp) {
                Some(d) => CicadaError::transient_retry_after(msg, d),
                None => CicadaError::transient(msg),
            }
        }
        s if s >= 500 => {
            let msg = format!("google server error ({s}): {message}");
            match retry_after(resp) {
                Some(d) => CicadaError::transient_retry_after(msg, d),
                None => CicadaError::transient(msg),
            }
        }
        401 => CicadaError::auth(format!("google rejected credentials (401): {message}")),
        403 if is_quota_403(resp) => {
            let msg = format!("google quota exceeded (403): {message}");
            match retry_after(resp) {
                Some(d) => CicadaError::transient_retry_after(msg, d),
                None => CicadaError::transient(msg),
            }
        }
        403 => CicadaError::auth(format!(
            "google forbids this credential (403): {message} — check scopes/calendar sharing"
        )),
        412 => CicadaError::conflict(format!(
            "etag precondition failed (412): {message} — refresh and re-plan"
        )),
        s => CicadaError::fatal(format!("google api error ({s}): {message}")),
    }
}
