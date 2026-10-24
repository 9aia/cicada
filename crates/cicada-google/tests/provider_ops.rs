//! Fixture-driven `GoogleCalendarProvider` tests — no network.
//!
//! A `FixtureHttp` (cicada-testkit) answers each request; assertions
//! then inspect the recorded request stream — URLs, query params,
//! `If-Match`, `Authorization`, JSON bodies — plus the parsed
//! `RemoteEvent`s and the error taxonomy.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;
use std::time::Duration;

use cicada_core::domain::error::CicadaError;
use cicada_core::domain::event::{Conferencing, EventBody, EventTime, Transparency};
use cicada_core::domain::ids::ZoneId;
use cicada_core::domain::policy::{SendUpdates, WriteOptions};
use cicada_core::domain::time::{Time, TimeRange};
use cicada_core::ports::{
    AuthProvider, CalendarProvider, CalendarRef, HttpMethod, HttpRequest, HttpResponse,
};
use cicada_google::{
    FixedJitter, GoogleAuthConfig, GoogleAuthProvider, GoogleCalendarProvider, RecordingSleeper,
    RetryHttp,
};
use cicada_testkit::{FakeClock, FixtureHttp};
use jiff::SignedDuration;
use jiff::civil::date;

const EVENT_FULL: &str = include_str!("fixtures/event_full.json");
const PAGE1: &str = include_str!("fixtures/events_page1.json");
const PAGE2: &str = include_str!("fixtures/events_page2.json");
const FREEBUSY: &str = include_str!("fixtures/freebusy.json");

/// A provider over the fixture client with a static test token.
fn provider(http: &Arc<FixtureHttp>) -> GoogleCalendarProvider {
    let auth: Arc<dyn AuthProvider> = Arc::new(GoogleAuthProvider::new(
        GoogleAuthConfig::StaticToken {
            access_token: "test-token".into(),
        },
        http.clone(),
        None,
        Arc::new(FakeClock::at("2026-10-23T00:00:00Z".parse().unwrap())),
    ));
    GoogleCalendarProvider::new(http.clone(), auth)
}

/// Provider over `RetryHttp<FixtureHttp>` (retry exercised through
/// the provider's real request path).
fn retrying_provider(
    http: &Arc<FixtureHttp>,
    sleeper: Arc<RecordingSleeper>,
) -> GoogleCalendarProvider {
    let retrying: Arc<dyn cicada_core::ports::Http> = Arc::new(
        RetryHttp::new(http.as_ref().clone())
            .with_sleeper(sleeper)
            .with_jitter(Arc::new(FixedJitter(1.0))),
    );
    let auth: Arc<dyn AuthProvider> = Arc::new(GoogleAuthProvider::new(
        GoogleAuthConfig::StaticToken {
            access_token: "test-token".into(),
        },
        retrying.clone(),
        None,
        Arc::new(FakeClock::at("2026-10-23T00:00:00Z".parse().unwrap())),
    ));
    GoogleCalendarProvider::new(retrying, auth)
}

fn cal() -> CalendarRef {
    CalendarRef::new("primary")
}

fn body(summary: &str) -> EventBody {
    EventBody {
        summary: summary.into(),
        description: Some("declared in YAML".into()),
        location: None,
        time: EventTime::Span {
            start: Time::Wall {
                local: date(2026, 10, 20).at(9, 0, 0, 0),
                zone: ZoneId::new_unchecked("America/Sao_Paulo"),
            },
            duration: SignedDuration::from_mins(30),
        },
        recurrence: None,
        attendees: vec![],
        reminders: vec![],
        conferencing: None,
        transparency: Transparency::Opaque,
    }
}

fn opts() -> WriteOptions {
    WriteOptions {
        send_updates: SendUpdates::None,
        idempotency_key: Some("primary|standup|create|sha256:abc".into()),
        etag: None,
    }
}

fn json_response(status: u16, body: &str) -> HttpResponse {
    HttpResponse {
        status,
        headers: vec![("content-type".into(), "application/json".into())],
        body: body.as_bytes().to_vec(),
    }
}

fn header<'a>(req: &'a HttpRequest, name: &str) -> Option<&'a str> {
    req.headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case(name))
        .map(|(_, v)| v.as_str())
}

// ---------------------------------------------------------------
// Reads
// ---------------------------------------------------------------

#[tokio::test]
async fn list_paginates_and_skips_cancelled() {
    let http = Arc::new(FixtureHttp::new());
    http.expect_fn(
        |r| {
            r.method == HttpMethod::Get
                && r.url.contains("/calendars/primary/events")
                && !r.url.contains("pageToken")
        },
        Ok(json_response(200, PAGE1)),
    );
    http.expect_fn(
        |r| r.method == HttpMethod::Get && r.url.contains("pageToken=page-two-token"),
        Ok(json_response(200, PAGE2)),
    );

    let events = provider(&http).list_events(&cal(), None).await.unwrap();
    let ids: Vec<&str> = events.iter().map(|e| e.remote_id.as_str()).collect();
    // Page 1's tagged event + page 2's all-day event; the cancelled
    // one is skipped.
    assert_eq!(ids, ["g-evt-page1", "g-evt-page2"]);
    assert_eq!(
        events[0].tags.local_id.as_ref().unwrap().as_str(),
        "page-one"
    );

    let reqs = http.requests();
    assert_eq!(reqs.len(), 2);
    assert!(reqs[0].url.contains("singleEvents=false"));
    assert!(reqs[0].url.contains("showDeleted=false"));
    assert!(reqs[0].url.contains("maxResults=2500"));
    assert_eq!(header(&reqs[0], "authorization"), Some("Bearer test-token"));
    assert!(http.is_exhausted());
}

#[tokio::test]
async fn list_range_sends_time_bounds() {
    let http = Arc::new(FixtureHttp::new());
    http.expect_json(
        HttpMethod::Get,
        "/calendars/primary/events",
        &serde_json::json!({"items": []}),
    );
    let range = TimeRange::new(
        "2026-10-20T00:00:00Z".parse().unwrap(),
        "2026-10-28T00:00:00Z".parse().unwrap(),
    )
    .unwrap();
    let events = provider(&http)
        .list_events(&cal(), Some(range))
        .await
        .unwrap();
    assert!(events.is_empty());
    let req = &http.requests()[0];
    assert!(req.url.contains("timeMin=2026-10-20T00%3A00%3A00Z"));
    assert!(req.url.contains("timeMax=2026-10-28T00%3A00%3A00Z"));
}

#[tokio::test]
async fn get_404_and_410_are_absent() {
    for status in [404u16, 410] {
        let http = Arc::new(FixtureHttp::new());
        http.expect(HttpMethod::Get, "/events/gone", json_response(status, ""));
        let found = provider(&http)
            .get_event(&cal(), &"gone".into())
            .await
            .unwrap();
        assert!(found.is_none(), "status {status} should read as absent");
    }
}

#[tokio::test]
async fn get_cancelled_event_is_absent() {
    let http = Arc::new(FixtureHttp::new());
    http.expect(
        HttpMethod::Get,
        "/events/g-evt-cancelled",
        json_response(200, r#"{"id":"g-evt-cancelled","status":"cancelled"}"#),
    );
    let found = provider(&http)
        .get_event(&cal(), &"g-evt-cancelled".into())
        .await
        .unwrap();
    assert!(found.is_none());
}

#[tokio::test]
async fn find_by_tag_sends_private_extended_property() {
    let http = Arc::new(FixtureHttp::new());
    http.expect_fn(
        |r| {
            r.method == HttpMethod::Get
                && r.url
                    .contains("privateExtendedProperty=cicada%3Aid%3Dstandup")
                && r.url
                    .contains("privateExtendedProperty=cicada%3Amanaged%3D1")
        },
        Ok(json_response(
            200,
            &format!("{{\"items\": [{EVENT_FULL}]}}"),
        )),
    );
    let found = provider(&http)
        .find_by_tag(&cal(), &"standup".into())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(found.remote_id.as_str(), "g-evt-standup");
    assert!(http.is_exhausted());
}

#[tokio::test]
async fn find_by_tag_absent_is_none() {
    let http = Arc::new(FixtureHttp::new());
    http.expect_json(
        HttpMethod::Get,
        "/calendars/primary/events",
        &serde_json::json!({"items": []}),
    );
    let found = provider(&http)
        .find_by_tag(&cal(), &"nope".into())
        .await
        .unwrap();
    assert!(found.is_none());
}

// ---------------------------------------------------------------
// Writes
// ---------------------------------------------------------------

#[tokio::test]
async fn create_stamps_tags_and_send_updates() {
    let http = Arc::new(FixtureHttp::new());
    http.expect_fn(
        |r| {
            r.method == HttpMethod::Post
                && r.url.contains("/calendars/primary/events?")
                && r.url.contains("sendUpdates=none")
        },
        Ok(json_response(
            200,
            r#"{"id":"g-new","etag":"\"e1\"","status":"confirmed","summary":"Standup",
               "start":{"dateTime":"2026-10-20T09:00:00-03:00","timeZone":"America/Sao_Paulo"},
               "end":{"dateTime":"2026-10-20T09:30:00-03:00","timeZone":"America/Sao_Paulo"},
               "extendedProperties":{"private":{"cicada:id":"standup","cicada:managed":"1","cicada:state":"sha256:x"}}}"#,
        )),
    );

    let created = provider(&http)
        .create_event(&cal(), &body("Standup"), &"standup".into(), &opts())
        .await
        .unwrap();
    assert_eq!(created.remote_id.as_str(), "g-new");
    assert_eq!(created.etag.as_deref(), Some("\"e1\""));

    let req = &http.requests()[0];
    assert_eq!(header(req, "authorization"), Some("Bearer test-token"));
    // `|` in the idempotency key isn't sent anywhere — Google has no
    // create idempotency; tags carry the contract.
    let sent: serde_json::Value = serde_json::from_slice(req.body.as_ref().unwrap()).unwrap();
    let private = &sent["extendedProperties"]["private"];
    assert_eq!(private["cicada:id"], "standup");
    assert_eq!(private["cicada:managed"], "1");
    assert!(
        private["cicada:state"]
            .as_str()
            .unwrap()
            .starts_with("sha256:")
    );
    assert_eq!(sent["summary"], "Standup");
    assert_eq!(sent["start"]["timeZone"], "America/Sao_Paulo");
    // No conferencing → no createRequest, no conferenceDataVersion.
    assert!(sent.get("conferenceData").is_none());
    assert!(!req.url.contains("conferenceDataVersion"));
    assert!(http.is_exhausted());
}

#[tokio::test]
async fn create_with_meet_attaches_create_request() {
    let http = Arc::new(FixtureHttp::new());
    http.expect_fn(
        |r| {
            r.method == HttpMethod::Post
                && r.url.contains("conferenceDataVersion=1")
                && r.url.contains("sendUpdates=all")
        },
        Ok(json_response(
            200,
            r#"{"id":"g-meet","status":"confirmed","summary":"Sync",
               "start":{"dateTime":"2026-10-20T09:00:00-03:00","timeZone":"America/Sao_Paulo"},
               "end":{"dateTime":"2026-10-20T09:30:00-03:00","timeZone":"America/Sao_Paulo"},
               "hangoutLink":"https://meet.google.com/abc-defg-hij",
               "conferenceData":{"conferenceSolution":{"key":{"type":"hangoutsMeet"}},
                "entryPoints":[{"entryPointType":"video","uri":"https://meet.google.com/abc-defg-hij"}]}}"#,
        )),
    );
    let mut event = body("Sync");
    event.conferencing = Some(Conferencing::Meet);
    let mut opts = opts();
    opts.send_updates = SendUpdates::All;

    let created = provider(&http)
        .create_event(&cal(), &event, &"sync".into(), &opts)
        .await
        .unwrap();
    assert_eq!(created.body.conferencing, Some(Conferencing::Meet));

    let sent: serde_json::Value =
        serde_json::from_slice(http.requests()[0].body.as_ref().unwrap()).unwrap();
    let req = &sent["conferenceData"]["createRequest"];
    assert_eq!(req["conferenceSolutionKey"]["type"], "hangoutsMeet");
    // The op's idempotency key feeds requestId (sanitized).
    assert_eq!(
        req["requestId"].as_str().unwrap(),
        "cicada-primary-standup-create-sha256-abc"
    );
}

#[tokio::test]
async fn update_sends_if_match_and_preserves_foreign_tags() {
    let http = Arc::new(FixtureHttp::new());
    // The read-first GET returns the existing event (foreign private
    // key + a cicada:id).
    http.expect_fn(
        |r| r.method == HttpMethod::Get && r.url.contains("/events/g-evt-standup"),
        Ok(json_response(200, EVENT_FULL)),
    );
    http.expect_fn(
        |r| {
            r.method == HttpMethod::Patch
                && r.url.contains("/events/g-evt-standup")
                && r.url.contains("sendUpdates=externalOnly")
        },
        Ok(json_response(200, EVENT_FULL)),
    );

    let mut opts = opts();
    opts.send_updates = SendUpdates::ExternalOnly;
    opts.etag = Some("\"3385852702942000\"".into());
    let mut new_body = body("Daily Standup");
    new_body.transparency = Transparency::Transparent;

    let updated = provider(&http)
        .update_event(&cal(), &"g-evt-standup".into(), &new_body, &opts)
        .await
        .unwrap();
    assert_eq!(updated.remote_id.as_str(), "g-evt-standup");

    let reqs = http.requests();
    assert_eq!(reqs.len(), 2);
    let patch = &reqs[1];
    assert_eq!(header(patch, "if-match"), Some("\"3385852702942000\""));
    let sent: serde_json::Value = serde_json::from_slice(patch.body.as_ref().unwrap()).unwrap();
    let private = &sent["extendedProperties"]["private"];
    // cicada tags refreshed from the remote's own cicada:id …
    assert_eq!(private["cicada:id"], "standup");
    assert_eq!(private["cicada:managed"], "1");
    // … and the foreign key survived the merge.
    assert_eq!(private["other-tool:note"], "left alone");
    // Desired values in the patch.
    assert_eq!(sent["summary"], "Daily Standup");
    assert_eq!(sent["transparency"], "transparent");
    // Remote already has conferenceData; no new createRequest —
    // Google would mint a second link.
    assert!(sent["conferenceData"].get("createRequest").is_none());
    assert!(http.is_exhausted());
}

#[tokio::test]
async fn update_requests_meet_when_remote_lacks_it() {
    let http = Arc::new(FixtureHttp::new());
    let existing = r#"{"id":"g-1","etag":"\"e9\"","status":"confirmed","summary":"S",
        "start":{"dateTime":"2026-10-20T09:00:00-03:00","timeZone":"America/Sao_Paulo"},
        "end":{"dateTime":"2026-10-20T09:30:00-03:00","timeZone":"America/Sao_Paulo"},
        "extendedProperties":{"private":{"cicada:id":"s","cicada:managed":"1","cicada:state":"sha256:old"}}}"#;
    http.expect_fn(
        |r| r.method == HttpMethod::Get,
        Ok(json_response(200, existing)),
    );
    http.expect_fn(
        |r| r.method == HttpMethod::Patch && r.url.contains("conferenceDataVersion=1"),
        Ok(json_response(200, existing)),
    );
    let mut event = body("S");
    event.conferencing = Some(Conferencing::Meet);
    provider(&http)
        .update_event(&cal(), &"g-1".into(), &event, &opts())
        .await
        .unwrap();
    let sent: serde_json::Value =
        serde_json::from_slice(http.requests()[1].body.as_ref().unwrap()).unwrap();
    assert_eq!(
        sent["conferenceData"]["createRequest"]["conferenceSolutionKey"]["type"],
        "hangoutsMeet"
    );
}

#[tokio::test]
async fn update_404_is_conflict_and_412_is_conflict() {
    for (status, note) in [(404u16, "get→conflict"), (412, "patch→conflict")] {
        let http = Arc::new(FixtureHttp::new());
        if note.starts_with("patch") {
            http.expect_fn(
                |r| r.method == HttpMethod::Get,
                Ok(json_response(200, EVENT_FULL)),
            );
        }
        http.expect_fn(
            |r| r.method == HttpMethod::Patch || r.method == HttpMethod::Get,
            Ok(json_response(status, "{}")),
        );
        let err = provider(&http)
            .update_event(&cal(), &"g-evt-standup".into(), &body("x"), &opts())
            .await
            .unwrap_err();
        assert!(
            matches!(err, CicadaError::Conflict(_)),
            "{note}: got {err:?}"
        );
    }
}

#[tokio::test]
async fn delete_sends_if_match_and_410_is_ok() {
    let http = Arc::new(FixtureHttp::new());
    http.expect_fn(
        |r| {
            r.method == HttpMethod::Delete
                && r.url.contains("/events/g-evt-standup")
                && r.url.contains("sendUpdates=none")
        },
        Ok(HttpResponse {
            status: 204,
            headers: vec![],
            body: vec![],
        }),
    );
    http.expect_fn(
        |r| r.method == HttpMethod::Delete,
        Ok(json_response(410, "")),
    );
    let p = provider(&http);
    let mut o = opts();
    o.etag = Some("\"e1\"".into());
    p.delete_event(&cal(), &"g-evt-standup".into(), &o)
        .await
        .unwrap();
    // Idempotent: already-gone is success (ADR-0002 §3).
    p.delete_event(&cal(), &"g-evt-standup".into(), &opts())
        .await
        .unwrap();
    assert_eq!(header(&http.requests()[0], "if-match"), Some("\"e1\""));
}

// ---------------------------------------------------------------
// Error taxonomy (ADR-0002 §8)
// ---------------------------------------------------------------

#[tokio::test]
async fn status_401_is_auth() {
    let http = Arc::new(FixtureHttp::new());
    http.expect(
        HttpMethod::Get,
        "/calendars/primary/events",
        json_response(401, r#"{"error":{"message":"Invalid Credentials"}}"#),
    );
    let err = provider(&http).list_events(&cal(), None).await.unwrap_err();
    assert!(matches!(err, CicadaError::Auth(_)), "got {err:?}");
}

#[tokio::test]
async fn status_403_quota_is_transient_but_forbidden_is_auth() {
    let quota = Arc::new(FixtureHttp::new());
    quota.expect(
        HttpMethod::Get,
        "/calendars/primary/events",
        json_response(
            403,
            r#"{"error":{"message":"Quota exceeded","errors":[{"reason":"rateLimitExceeded"}]}}"#,
        ),
    );
    let err = provider(&quota)
        .list_events(&cal(), None)
        .await
        .unwrap_err();
    assert!(matches!(err, CicadaError::Transient { .. }), "got {err:?}");

    let forbidden = Arc::new(FixtureHttp::new());
    forbidden.expect(
        HttpMethod::Get,
        "/calendars/primary/events",
        json_response(
            403,
            r#"{"error":{"message":"Forbidden","errors":[{"reason":"forbidden"}]}}"#,
        ),
    );
    let err = provider(&forbidden)
        .list_events(&cal(), None)
        .await
        .unwrap_err();
    assert!(matches!(err, CicadaError::Auth(_)), "got {err:?}");
}

#[tokio::test]
async fn status_429_retries_once_then_succeeds() {
    let http = Arc::new(FixtureHttp::new());
    http.expect(
        HttpMethod::Get,
        "/calendars/primary/events",
        HttpResponse {
            status: 429,
            headers: vec![("retry-after".into(), "3".into())],
            body: vec![],
        },
    );
    http.expect_json(
        HttpMethod::Get,
        "/calendars/primary/events",
        &serde_json::json!({"items": [{"id": "g-1", "status": "confirmed",
            "summary": "x",
            "start": {"dateTime": "2026-10-20T12:00:00Z"},
            "end": {"dateTime": "2026-10-20T12:30:00Z"}}]}),
    );
    let sleeper = Arc::new(RecordingSleeper::default());
    let events = retrying_provider(&http, sleeper.clone())
        .list_events(&cal(), None)
        .await
        .unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(http.requests().len(), 2);
    // Retry-After: 3 dominates the ~250ms backoff.
    assert_eq!(
        sleeper.slept.lock().unwrap().as_slice(),
        &[Duration::from_secs(3)]
    );
}

#[tokio::test]
async fn exhausted_retries_surface_transient_with_retry_after() {
    let http = Arc::new(FixtureHttp::new());
    for _ in 0..4 {
        http.expect(
            HttpMethod::Get,
            "/calendars/primary/events",
            HttpResponse {
                status: 503,
                headers: vec![("retry-after".into(), "2".into())],
                body: vec![],
            },
        );
    }
    let sleeper = Arc::new(RecordingSleeper::default());
    let err = retrying_provider(&http, sleeper)
        .list_events(&cal(), None)
        .await
        .unwrap_err();
    match err {
        CicadaError::Transient { retry_after, .. } => {
            assert_eq!(retry_after, Some(Duration::from_secs(2)));
        }
        other => panic!("expected transient, got {other:?}"),
    }
    assert_eq!(http.requests().len(), 4); // default max_attempts
}

#[tokio::test]
async fn status_400_is_fatal_and_not_retried() {
    let http = Arc::new(FixtureHttp::new());
    http.expect(
        HttpMethod::Get,
        "/calendars/primary/events",
        json_response(400, r#"{"error":{"message":"Invalid value for timeMin"}}"#),
    );
    let sleeper = Arc::new(RecordingSleeper::default());
    let err = retrying_provider(&http, sleeper.clone())
        .list_events(&cal(), None)
        .await
        .unwrap_err();
    assert!(matches!(err, CicadaError::Fatal(_)), "got {err:?}");
    assert_eq!(http.requests().len(), 1);
    assert!(sleeper.slept.lock().unwrap().is_empty());
}

// ---------------------------------------------------------------
// freeBusy
// ---------------------------------------------------------------

#[tokio::test]
async fn free_busy_request_shape_and_parse() {
    let http = Arc::new(FixtureHttp::new());
    http.expect_fn(
        |r| r.method == HttpMethod::Post && r.url.contains("/freeBusy"),
        Ok(json_response(200, FREEBUSY)),
    );
    let range = TimeRange::new(
        "2026-10-20T00:00:00Z".parse().unwrap(),
        "2026-10-28T00:00:00Z".parse().unwrap(),
    )
    .unwrap();
    let busy = provider(&http).free_busy(&cal(), range).await.unwrap();
    assert_eq!(busy.len(), 2);
    assert_eq!(busy[0].start.to_string(), "2026-10-20T12:00:00Z");
    assert_eq!(busy[0].end.to_string(), "2026-10-20T12:30:00Z");
    assert_eq!(busy[1].start.to_string(), "2026-10-21T17:00:00Z");

    let sent: serde_json::Value =
        serde_json::from_slice(http.requests()[0].body.as_ref().unwrap()).unwrap();
    assert_eq!(sent["timeMin"], "2026-10-20T00:00:00Z");
    assert_eq!(sent["timeMax"], "2026-10-28T00:00:00Z");
    assert_eq!(sent["items"], serde_json::json!([{"id": "primary"}]));
}

// ---------------------------------------------------------------
// Full op sequence (list → create → findByTag → update → delete)
// ---------------------------------------------------------------

#[tokio::test]
async fn end_to_end_op_sequence() {
    let http = Arc::new(FixtureHttp::new());
    let created_json = r#"{"id":"g-1","etag":"\"e1\"","status":"confirmed","summary":"Standup",
        "start":{"dateTime":"2026-10-20T09:00:00-03:00","timeZone":"America/Sao_Paulo"},
        "end":{"dateTime":"2026-10-20T09:30:00-03:00","timeZone":"America/Sao_Paulo"},
        "extendedProperties":{"private":{"cicada:id":"standup","cicada:managed":"1","cicada:state":"sha256:x"}}}"#;
    // 1. list (empty)
    http.expect_json(
        HttpMethod::Get,
        "/calendars/primary/events",
        &serde_json::json!({"items": []}),
    );
    // 2. create
    http.expect_fn(
        |r| r.method == HttpMethod::Post && r.url.contains("/events"),
        Ok(json_response(200, created_json)),
    );
    // 3. findByTag → hit
    http.expect_fn(
        |r| {
            r.method == HttpMethod::Get
                && r.url
                    .contains("privateExtendedProperty=cicada%3Aid%3Dstandup")
        },
        Ok(json_response(
            200,
            &format!("{{\"items\": [{created_json}]}}"),
        )),
    );
    // 4. update: GET then PATCH
    http.expect_fn(
        |r| r.method == HttpMethod::Get && r.url.contains("/events/g-1"),
        Ok(json_response(200, created_json)),
    );
    http.expect_fn(
        |r| r.method == HttpMethod::Patch && r.url.contains("/events/g-1"),
        Ok(json_response(200, created_json)),
    );
    // 5. delete
    http.expect_fn(
        |r| r.method == HttpMethod::Delete && r.url.contains("/events/g-1"),
        Ok(HttpResponse {
            status: 204,
            headers: vec![],
            body: vec![],
        }),
    );

    let p = provider(&http);
    let local = "standup".into();
    let mut o = opts();
    o.etag = Some("\"e1\"".into());

    assert!(p.list_events(&cal(), None).await.unwrap().is_empty());
    let created = p
        .create_event(&cal(), &body("Standup"), &local, &o)
        .await
        .unwrap();
    assert_eq!(created.remote_id.as_str(), "g-1");
    let found = p.find_by_tag(&cal(), &local).await.unwrap().unwrap();
    assert_eq!(found.remote_id, created.remote_id);
    let updated = p
        .update_event(&cal(), &created.remote_id, &body("Standup v2"), &o)
        .await
        .unwrap();
    assert_eq!(updated.remote_id.as_str(), "g-1");
    p.delete_event(&cal(), &created.remote_id, &o)
        .await
        .unwrap();

    let methods: Vec<_> = http.requests().iter().map(|r| r.method.as_str()).collect();
    assert_eq!(methods, ["GET", "POST", "GET", "GET", "PATCH", "DELETE"]);
    assert!(http.is_exhausted());
}

#[test]
fn with_base_url_variants_and_bad_bases() {
    let http = Arc::new(FixtureHttp::new());
    // `with_base_url` generic instantiations.
    let _ = provider(&http).with_base_url("http://x".to_owned());
    let _ = provider(&http).with_base_url("http://y");
}

#[tokio::test]
async fn bad_base_urls_error_on_any_op() {
    let http = Arc::new(FixtureHttp::new());
    // Unparseable base → Url::parse arm.
    let p = provider(&http).with_base_url("not a url");
    assert!(p.get_event(&cal(), &"r".into()).await.is_err());
    // Cannot-be-a-base scheme → path_segments arm.
    let p = provider(&http).with_base_url("mailto:a@b.c");
    assert!(p.get_event(&cal(), &"r".into()).await.is_err());
}

#[tokio::test]
async fn malformed_event_payload_errors() {
    let http = Arc::new(FixtureHttp::new());
    http.expect_fn(|_| true, Ok(json_response(200, "not json")));
    let p = provider(&http);
    let local = "standup".into();
    assert!(
        p.create_event(&cal(), &body("x"), &local, &opts())
            .await
            .is_err()
    );
}

#[tokio::test]
async fn conference_request_id_falls_back_without_idempotency_key() {
    let http = Arc::new(FixtureHttp::new());
    http.expect_fn(|_| true, Ok(json_response(200, EVENT_FULL)));
    let p = provider(&http);
    let mut b = body("meet");
    b.conferencing = Some(Conferencing::Meet);
    let o = WriteOptions {
        send_updates: SendUpdates::None,
        idempotency_key: None,
        etag: None,
    };
    let local = "standup".into();
    let _ = p.create_event(&cal(), &b, &local, &o).await;
    assert!(http.requests()[0].url.contains("conferenceDataVersion=1"));
}
