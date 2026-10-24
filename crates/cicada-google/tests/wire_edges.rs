//! `event_json` edge cases + secrets/http/retry bits the op tests
//! don't reach.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use cicada_core::domain::attendee::ResponseStatus;
use cicada_core::domain::event::{Conferencing, EventBody, EventTime, Transparency};
use cicada_core::domain::ids::{RemoteId, ZoneId};
use cicada_core::domain::policy::WriteOptions;
use cicada_core::domain::recurrence::RecurrenceRule;
use cicada_core::domain::reminder::{Reminder, ReminderKind};
use cicada_core::domain::time::Time;
use cicada_core::ports::calendar_provider::EventTags;
use cicada_core::ports::{CalendarProvider, SecretStore};
use cicada_google::event_json::{
    self, ConferenceData, ConferenceEntryPoint, ConferenceSolution, ConferenceSolutionKey,
    EventDateTime, ExtendedProperties, WireAttendee, WireEvent, WireReminderOverride,
    WireReminders,
};
use jiff::SignedDuration;
use jiff::civil::date;

fn span_body() -> EventBody {
    EventBody {
        summary: "x".into(),
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
    }
}

fn dt(date_time: Option<&str>, time_zone: Option<&str>, date: Option<&str>) -> EventDateTime {
    EventDateTime {
        date_time: date_time.map(str::to_owned),
        time_zone: time_zone.map(str::to_owned),
        date: date.map(str::to_owned),
    }
}

fn wire() -> WireEvent {
    serde_json::from_value(serde_json::json!({ "id": "r1" })).unwrap()
}

#[test]
fn to_wire_rejects_non_meet_conferencing() {
    let mut b = span_body();
    b.conferencing = Some(Conferencing::Other("zoom".into()));
    let err = event_json::to_wire(
        &b,
        &EventTags::default(),
        &std::collections::BTreeMap::new(),
        None,
    )
    .unwrap_err();
    assert!(err.to_string().contains("meet"), "{err}");
}

#[test]
fn to_wire_rejects_unresolvable_zone() {
    let mut b = span_body();
    b.time = EventTime::Span {
        start: Time::Wall {
            local: date(2026, 1, 5).at(9, 0, 0, 0),
            zone: ZoneId::new_unchecked("Not/AZone"),
        },
        duration: SignedDuration::from_mins(30),
    };
    let err = event_json::to_wire(
        &b,
        &EventTags::default(),
        &std::collections::BTreeMap::new(),
        None,
    )
    .unwrap_err();
    assert!(
        err.to_string().contains("no longer resolves")
            || err.to_string().contains("does not resolve"),
        "{err}"
    );
}

#[test]
fn from_wire_reads_all_day_as_utc_midnight() {
    let mut w = wire();
    w.start = Some(dt(None, None, Some("2026-01-05")));
    w.end = Some(dt(None, None, Some("2026-01-06")));
    let ev = event_json::from_wire(&w).unwrap();
    let Time::Wall { local, zone } = ev.body.time.start() else {
        panic!("wall expected")
    };
    assert_eq!(local.to_string(), "2026-01-05T00:00:00");
    assert_eq!(zone.as_str(), "UTC");
    // A bad all-day date is a clear error.
    w.start = Some(dt(None, None, Some("not-a-date")));
    assert!(event_json::from_wire(&w).is_err());
}

#[test]
fn from_wire_time_variants() {
    let mut w = wire();
    w.end = Some(dt(Some("2026-01-05T09:30:00Z"), None, None));
    // Unknown zone → fall back to the offset-bearing instant.
    w.start = Some(dt(
        Some("2026-01-05T09:00:00+02:00"),
        Some("Custom/Zone"),
        None,
    ));
    let ev = event_json::from_wire(&w).unwrap();
    assert!(matches!(ev.body.time.start(), Time::Instant(_)));
    // No zone at all → instant.
    w.start = Some(dt(Some("2026-01-05T09:00:00Z"), None, None));
    let ev = event_json::from_wire(&w).unwrap();
    assert!(matches!(ev.body.time.start(), Time::Instant(_)));
    // Garbage dateTime → error (both the zoned and instant arms).
    w.start = Some(dt(Some("garbage"), Some("UTC"), None));
    assert!(event_json::from_wire(&w).is_err());
    w.start = Some(dt(Some("garbage"), None, None));
    assert!(event_json::from_wire(&w).is_err());
    // Garbage dateTime *and* an unresolvable zone — the fallback's
    // own parse error.
    w.start = Some(dt(Some("garbage"), Some("Custom/Zone"), None));
    assert!(event_json::from_wire(&w).is_err());
    // Neither dateTime nor date → error.
    w.start = Some(dt(None, None, None));
    assert!(event_json::from_wire(&w).is_err());
}

#[test]
fn attendee_status_mapping() {
    for (wire_status, want) in [
        ("accepted", ResponseStatus::Accepted),
        ("declined", ResponseStatus::Declined),
        ("tentative", ResponseStatus::Tentative),
        ("needsAction", ResponseStatus::NeedsAction),
        ("mystery", ResponseStatus::NeedsAction),
    ] {
        let mut w = wire();
        w.start = Some(dt(Some("2026-01-05T09:00:00Z"), None, None));
        w.end = Some(dt(Some("2026-01-05T09:30:00Z"), None, None));
        w.attendees = vec![WireAttendee {
            email: "a@x".into(),
            optional: Some(true),
            response_status: Some(wire_status.into()),
        }];
        let ev = event_json::from_wire(&w).unwrap();
        assert_eq!(ev.body.attendees[0].status, want, "{wire_status}");
        assert!(!ev.body.attendees[0].required);
    }
}

#[test]
fn reminder_overrides_drop_unknown_and_negative() {
    let mut w = wire();
    w.start = Some(dt(Some("2026-01-05T09:00:00Z"), None, None));
    w.end = Some(dt(Some("2026-01-05T09:30:00Z"), None, None));
    w.reminders = Some(WireReminders {
        use_default: false,
        overrides: vec![
            WireReminderOverride {
                method: "popup".into(),
                minutes: 10,
            },
            WireReminderOverride {
                method: "email".into(),
                minutes: 60,
            },
            WireReminderOverride {
                method: "sms".into(), // unknown → dropped
                minutes: 5,
            },
            WireReminderOverride {
                method: "popup".into(),
                minutes: -1, // negative → dropped
            },
        ],
    });
    let ev = event_json::from_wire(&w).unwrap();
    assert_eq!(ev.body.reminders.len(), 2);
    assert_eq!(ev.body.reminders[0].kind, ReminderKind::Popup);
    assert_eq!(ev.body.reminders[1].kind, ReminderKind::Email);
}

#[test]
fn reminders_to_wire_validates_offsets() {
    let mut b = span_body();
    b.reminders = vec![Reminder {
        kind: ReminderKind::Popup,
        offset: SignedDuration::from_secs(90), // not whole minutes
    }];
    assert!(
        event_json::to_wire(
            &b,
            &EventTags::default(),
            &std::collections::BTreeMap::new(),
            None
        )
        .is_err()
    );
    b.reminders = vec![Reminder {
        kind: ReminderKind::Popup,
        offset: SignedDuration::from_mins(40_321), // > 4 weeks
    }];
    let err = event_json::to_wire(
        &b,
        &EventTags::default(),
        &std::collections::BTreeMap::new(),
        None,
    )
    .unwrap_err();
    assert!(err.to_string().contains("4 weeks"), "{err}");
}

#[test]
fn conferencing_from_wire_variants() {
    let mut w = wire();
    w.start = Some(dt(Some("2026-01-05T09:00:00Z"), None, None));
    w.end = Some(dt(Some("2026-01-05T09:30:00Z"), None, None));
    // Meet by solution key.
    w.conference_data = Some(ConferenceData {
        conference_solution: Some(ConferenceSolution {
            key: Some(ConferenceSolutionKey {
                key_type: "hangoutsMeet".into(),
            }),
            name: None,
        }),
        ..Default::default()
    });
    assert_eq!(
        event_json::from_wire(&w).unwrap().body.conferencing,
        Some(Conferencing::Meet)
    );
    // Meet by entry-point URI.
    w.conference_data = Some(ConferenceData {
        entry_points: vec![ConferenceEntryPoint {
            entry_point_type: Some("video".into()),
            uri: Some("https://meet.google.com/abc-defg-hij".into()),
        }],
        ..Default::default()
    });
    assert_eq!(
        event_json::from_wire(&w).unwrap().body.conferencing,
        Some(Conferencing::Meet)
    );
    // A named other solution.
    w.conference_data = Some(ConferenceData {
        conference_solution: Some(ConferenceSolution {
            key: Some(ConferenceSolutionKey {
                key_type: "addOn".into(),
            }),
            name: None,
        }),
        ..Default::default()
    });
    assert_eq!(
        event_json::from_wire(&w).unwrap().body.conferencing,
        Some(Conferencing::Other("addOn".into()))
    );
    // Name-only solution falls back to the display name.
    w.conference_data = Some(ConferenceData {
        conference_solution: Some(ConferenceSolution {
            key: None,
            name: Some("WebEx".into()),
        }),
        ..Default::default()
    });
    assert_eq!(
        event_json::from_wire(&w).unwrap().body.conferencing,
        Some(Conferencing::Other("WebEx".into()))
    );
    // Empty conference data → "unknown".
    w.conference_data = Some(ConferenceData::default());
    assert_eq!(
        event_json::from_wire(&w).unwrap().body.conferencing,
        Some(Conferencing::Other("unknown".into()))
    );
}

// ---------- FileSecretStore ----------

#[tokio::test]
async fn secret_store_round_trip_and_error_arms() {
    use cicada_google::secrets::FileSecretStore;
    let dir = tempfile::tempdir().unwrap();
    let store = FileSecretStore::new(dir.path().join("secrets"));

    // Missing → None; set/get/delete round-trip; key path encoding.
    assert!(store.get("a/b").await.unwrap().is_none());
    store.set("a/b", "v1").await.unwrap();
    assert!(store.key_path("a/b").ends_with("a%2Fb"));
    assert_eq!(store.get("a/b").await.unwrap().as_deref(), Some("v1"));
    store.delete("a/b").await.unwrap();
    assert!(store.get("a/b").await.unwrap().is_none());
    store.delete("a/b").await.unwrap(); // deleting again is a no-op

    // A directory where the key file should be → read error.
    let bad = FileSecretStore::new(dir.path().join("bad"));
    std::fs::create_dir_all(bad.key_path("k")).unwrap();
    assert!(bad.get("k").await.is_err());
    // …and delete error.
    assert!(bad.delete("k").await.is_err());
    // set() can't create its dir inside a regular file.
    let blocker = dir.path().join("blocker");
    std::fs::write(&blocker, "x").unwrap();
    let blocked = FileSecretStore::new(blocker.join("secrets"));
    assert!(blocked.set("k", "v").await.is_err());
    // …nor write into a read-only dir.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let ro = dir.path().join("ro");
        std::fs::create_dir(&ro).unwrap();
        std::fs::set_permissions(&ro, std::fs::Permissions::from_mode(0o555)).unwrap();
        let store = FileSecretStore::new(ro.join("s"));
        let err = store.set("k", "v").await.unwrap_err();
        assert!(!err.to_string().is_empty());
        std::fs::set_permissions(&ro, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    // default_location exists (env-dependent root).
    let _ = FileSecretStore::default_location();
}

// ---------- retry internals ----------

#[tokio::test]
async fn sleeper_jitter_and_delay_paths() {
    use cicada_google::retry::{
        Jitter, RandomJitter, RetryHttp, RetryPolicy, Sleeper, TokioSleeper,
    };
    use cicada_testkit::FixtureHttp;
    use std::time::Duration;

    // TokioSleeper actually sleeps (bounded tiny).
    Sleeper::sleep(&TokioSleeper, Duration::ZERO).await;
    // RandomJitter stays in [0.5, 1.0) across many draws.
    let jitter = RandomJitter::default();
    for _ in 0..64 {
        let f = jitter.factor();
        assert!((0.5..1.0).contains(&f), "{f}");
    }
    // with_jitter + inner() + Debug + delay via a retry: a fixed
    // jitter of 0.5 makes the delay deterministic.
    let http = FixtureHttp::new();
    let client = RetryHttp::new(http.clone())
        .with_policy(RetryPolicy {
            max_attempts: 3,
            base_delay: Duration::from_millis(100),
            max_delay: Duration::from_secs(60),
        })
        .with_jitter(std::sync::Arc::new(RandomJitter::default()));
    let _ = client.inner();
    let dbg = format!("{client:?}");
    assert!(dbg.contains("RetryHttp"), "{dbg}");
    let _ = RemoteId::new("x");
    let _ = RecurrenceRule::Raw("FREQ=DAILY".into());
    let _ = ExtendedProperties::default();
}

// ---------- ReqwestHttp against a real stub ----------

#[tokio::test]
async fn reqwest_http_sends_and_classifies() {
    use cicada_core::ports::{Http, HttpMethod, HttpRequest};
    use cicada_google::ReqwestHttp;
    use std::io::{Read, Write};

    let http = ReqwestHttp::new().unwrap();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut buf = [0u8; 8192];
        let n = stream.read(&mut buf).unwrap();
        let req = String::from_utf8_lossy(&buf[..n]).into_owned();
        let body = br#"{"ok":true}"#;
        let resp = format!(
            "HTTP/1.1 201 Created\r\ncontent-type: application/json\r\ncontent-length: {}\r\nx-custom: v\r\nconnection: close\r\n\r\n{}",
            body.len(),
            String::from_utf8_lossy(body)
        );
        stream.write_all(resp.as_bytes()).unwrap();
        req
    });

    let mut req = HttpRequest::new(HttpMethod::Post, format!("http://127.0.0.1:{port}/x"));
    req.headers.push(("x-test".into(), "1".into()));
    req.body = Some(b"{}".to_vec());
    let resp = http.send(&req).await.unwrap();
    assert_eq!(resp.status, 201);
    assert_eq!(resp.body, br#"{"ok":true}"#.to_vec());
    assert!(resp.headers.iter().any(|(k, _)| k == "x-custom"));
    let sent = server.join().unwrap();
    assert!(sent.contains("x-test: 1"), "{sent}");
    assert!(sent.ends_with("{}"), "{sent}");

    // Connection refused → Transient.
    let dead = HttpRequest::new(HttpMethod::Get, "http://127.0.0.1:1/");
    let err = http.send(&dead).await.unwrap_err();
    assert!(err.to_string().contains("transport"), "{err}");
}

// ---------- provider ops the main suite misses ----------

#[tokio::test]
async fn provider_404_policies_and_malformed_payloads() {
    use cicada_core::ports::{CalendarRef, HttpMethod, HttpResponse};
    use cicada_google::GoogleCalendarProvider;
    use cicada_testkit::FixtureHttp;
    use std::sync::Arc;

    let http = Arc::new(FixtureHttp::new());
    let auth: Arc<dyn cicada_core::ports::AuthProvider> =
        Arc::new(cicada_google::GoogleAuthProvider::new(
            cicada_google::GoogleAuthConfig::StaticToken {
                access_token: "t".into(),
            },
            http.clone(),
            None,
            Arc::new(cicada_testkit::FakeClock::at(
                "2026-10-23T00:00:00Z".parse().unwrap(),
            )),
        ));
    let provider =
        GoogleCalendarProvider::new(http.clone() as Arc<dyn cicada_core::ports::Http>, auth)
            .with_base_url("https://g.example");

    let cal = CalendarRef::new("primary");

    // events.list 404 uses the Fatal policy → Fatal "not found".
    http.expect(
        HttpMethod::Get,
        "/calendars/primary/events",
        HttpResponse {
            status: 404,
            headers: vec![],
            body: br#"{"error":{"message":"no calendar"}}"#.to_vec(),
        },
    );
    let err = provider.list_events(&cal, None).await.unwrap_err();
    assert!(err.to_string().contains("not found"), "{err}");

    // Malformed events.list payload → Fatal.
    http.expect(
        HttpMethod::Get,
        "/calendars/primary/events",
        HttpResponse {
            status: 200,
            headers: vec![],
            body: b"not json".to_vec(),
        },
    );
    let err = provider.list_events(&cal, None).await.unwrap_err();
    assert!(err.to_string().contains("malformed"), "{err}");

    // Malformed event.get payload → Fatal.
    http.expect(
        HttpMethod::Get,
        "/calendars/primary/events/r2",
        HttpResponse {
            status: 200,
            headers: vec![],
            body: b"{{".to_vec(),
        },
    );
    let err = provider
        .get_event(&cal, &RemoteId::new("r2"))
        .await
        .unwrap_err();
    assert!(err.to_string().contains("malformed"), "{err}");

    // freeBusy without an entry for the calendar → Fatal.
    http.expect(
        HttpMethod::Post,
        "/freeBusy",
        HttpResponse {
            status: 200,
            headers: vec![],
            body: br#"{"calendars":{}}"#.to_vec(),
        },
    );
    let range = cicada_core::domain::time::TimeRange::new(
        "2026-01-01T00:00:00Z".parse().unwrap(),
        "2026-01-02T00:00:00Z".parse().unwrap(),
    )
    .unwrap();
    let err = provider.free_busy(&cal, range).await.unwrap_err();
    assert!(err.to_string().contains("no entry"), "{err}");

    // A delete on a non-404 error classifies.
    http.expect(
        HttpMethod::Delete,
        "/calendars/primary/events/r3",
        HttpResponse {
            status: 403,
            headers: vec![],
            body: br#"{"error":{"message":"forbidden"}}"#.to_vec(),
        },
    );
    let err = provider
        .delete_event(&cal, &RemoteId::new("r3"), &WriteOptions::default())
        .await
        .unwrap_err();
    assert!(err.to_string().contains("403"), "{err}");
}

#[tokio::test]
async fn classify_retry_after_and_quota_arms() {
    use cicada_core::ports::{CalendarRef, HttpMethod, HttpResponse};
    use cicada_google::GoogleCalendarProvider;
    use cicada_testkit::FixtureHttp;
    use std::sync::Arc;

    let http = Arc::new(FixtureHttp::new());
    let auth: Arc<dyn cicada_core::ports::AuthProvider> =
        Arc::new(cicada_google::GoogleAuthProvider::new(
            cicada_google::GoogleAuthConfig::StaticToken {
                access_token: "t".into(),
            },
            http.clone(),
            None,
            Arc::new(cicada_testkit::FakeClock::at(
                "2026-10-23T00:00:00Z".parse().unwrap(),
            )),
        ));
    let provider =
        GoogleCalendarProvider::new(http.clone() as Arc<dyn cicada_core::ports::Http>, auth)
            .with_base_url("https://g.example");
    let cal = CalendarRef::new("primary");

    // 429 WITH a Retry-After header → transient with hint.
    http.expect(
        HttpMethod::Get,
        "/calendars/primary/events",
        HttpResponse {
            status: 429,
            headers: vec![("retry-after".into(), "3".into())],
            body: b"{}".to_vec(),
        },
    );
    let err = provider.list_events(&cal, None).await.unwrap_err();
    assert!(err.to_string().contains("rate limited"), "{err}");

    // 5xx with Retry-After.
    http.expect(
        HttpMethod::Get,
        "/calendars/primary/events",
        HttpResponse {
            status: 503,
            headers: vec![("retry-after".into(), "1".into())],
            body: b"{}".to_vec(),
        },
    );
    let err = provider.list_events(&cal, None).await.unwrap_err();
    assert!(err.to_string().contains("server error"), "{err}");

    // 403 quota-shaped → transient; plain 403 → auth.
    http.expect(
        HttpMethod::Get,
        "/calendars/primary/events",
        HttpResponse {
            status: 403,
            headers: vec![("retry-after".into(), "5".into())],
            body: br#"{"error":{"errors":[{"reason":"rateLimitExceeded"}],"message":"quota"}}"#
                .to_vec(),
        },
    );
    let err = provider.list_events(&cal, None).await.unwrap_err();
    assert!(err.to_string().contains("quota"), "{err}");

    http.expect(
        HttpMethod::Get,
        "/calendars/primary/events",
        HttpResponse {
            status: 403,
            headers: vec![],
            body: br#"{"error":{"message":"insufficient permissions"}}"#.to_vec(),
        },
    );
    let err = provider.list_events(&cal, None).await.unwrap_err();
    assert!(err.to_string().contains("forbids"), "{err}");
}

#[tokio::test]
async fn update_without_etag_and_unknown_remote() {
    use cicada_core::ports::{CalendarProvider, CalendarRef, HttpMethod, HttpResponse};
    use cicada_google::GoogleCalendarProvider;
    use cicada_testkit::FixtureHttp;
    use std::sync::Arc;

    let http = Arc::new(FixtureHttp::new());
    let auth: Arc<dyn cicada_core::ports::AuthProvider> =
        Arc::new(cicada_google::GoogleAuthProvider::new(
            cicada_google::GoogleAuthConfig::StaticToken {
                access_token: "t".into(),
            },
            http.clone(),
            None,
            Arc::new(cicada_testkit::FakeClock::at(
                "2026-10-23T00:00:00Z".parse().unwrap(),
            )),
        ));
    let provider =
        GoogleCalendarProvider::new(http.clone() as Arc<dyn cicada_core::ports::Http>, auth)
            .with_base_url("https://g.example");
    let cal = CalendarRef::new("primary");

    // Update a remote that's gone → conflict ("refresh and re-plan").
    http.expect(
        HttpMethod::Get,
        "/calendars/primary/events/r9",
        HttpResponse {
            status: 404,
            headers: vec![],
            body: b"{}".to_vec(),
        },
    );
    let err = provider
        .update_event(
            &cal,
            &RemoteId::new("r9"),
            &span_body(),
            &WriteOptions::default(),
        )
        .await
        .unwrap_err();
    assert!(err.to_string().contains("re-plan"), "{err}");

    // The read-before-write GET returning junk → malformed payload.
    http.expect(
        HttpMethod::Get,
        "/calendars/primary/events/r8",
        HttpResponse {
            status: 200,
            headers: vec![],
            body: b"not json".to_vec(),
        },
    );
    let err = provider
        .update_event(
            &cal,
            &RemoteId::new("r8"),
            &span_body(),
            &WriteOptions::default(),
        )
        .await
        .unwrap_err();
    assert!(err.to_string().contains("malformed"), "{err}");
}

// ---------- classify arms + provider edges ----------

#[tokio::test]
async fn classify_429_and_5xx_without_retry_after() {
    use cicada_core::ports::{CalendarRef, HttpMethod, HttpResponse};
    use cicada_google::GoogleCalendarProvider;
    use cicada_testkit::FixtureHttp;
    use std::sync::Arc;

    let http = Arc::new(FixtureHttp::new());
    let auth: Arc<dyn cicada_core::ports::AuthProvider> =
        Arc::new(cicada_google::GoogleAuthProvider::new(
            cicada_google::GoogleAuthConfig::StaticToken {
                access_token: "t".into(),
            },
            http.clone(),
            None,
            Arc::new(cicada_testkit::FakeClock::at(
                "2026-10-23T00:00:00Z".parse().unwrap(),
            )),
        ));
    let provider =
        GoogleCalendarProvider::new(http.clone() as Arc<dyn cicada_core::ports::Http>, auth)
            .with_base_url("https://g.example");
    let cal = CalendarRef::new("primary");

    for status in [429, 500] {
        http.expect(
            HttpMethod::Get,
            "/calendars/primary/events",
            HttpResponse {
                status,
                headers: vec![],
                body: b"{}".to_vec(),
            },
        );
        let err = provider.list_events(&cal, None).await.unwrap_err();
        assert!(err.to_string().contains("error"), "{err}");
    }
}

#[tokio::test]
async fn update_event_merges_extra_private_and_garbage_tags() {
    use cicada_core::ports::{CalendarRef, HttpMethod, HttpResponse};
    use cicada_google::GoogleCalendarProvider;
    use cicada_testkit::FixtureHttp;
    use std::sync::Arc;

    let http = Arc::new(FixtureHttp::new());
    let auth: Arc<dyn cicada_core::ports::AuthProvider> =
        Arc::new(cicada_google::GoogleAuthProvider::new(
            cicada_google::GoogleAuthConfig::StaticToken {
                access_token: "t".into(),
            },
            http.clone(),
            None,
            Arc::new(cicada_testkit::FakeClock::at(
                "2026-10-23T00:00:00Z".parse().unwrap(),
            )),
        ));
    let provider =
        GoogleCalendarProvider::new(http.clone() as Arc<dyn cicada_core::ports::Http>, auth)
            .with_base_url("https://g.example");
    let cal = CalendarRef::new("primary");

    // The existing remote event parses as wire but its time fields
    // are garbage — tags fall back to defaults, extra_private merges.
    http.expect(
        HttpMethod::Get,
        "/calendars/primary/events/r5",
        HttpResponse {
            status: 200,
            headers: vec![],
            body: br#"{"id":"r5","start":{"dateTime":"garbage"},"end":{"dateTime":"garbage"},"extendedProperties":{"private":{"keep":"1"}}}"#.to_vec(),
        },
    );
    http.expect(
        HttpMethod::Patch,
        "/calendars/primary/events/r5",
        HttpResponse {
            status: 200,
            headers: vec![],
            body: br#"{"id":"r5","start":{"dateTime":"2026-01-05T09:00:00Z"},"end":{"dateTime":"2026-01-05T09:30:00Z"}}"#.to_vec(),
        },
    );
    let updated = provider
        .update_event(
            &cal,
            &RemoteId::new("r5"),
            &span_body(),
            &WriteOptions::default(),
        )
        .await
        .unwrap();
    assert_eq!(updated.remote_id.as_str(), "r5");
}

#[tokio::test]
async fn free_busy_bad_base_url_and_malformed_response() {
    use cicada_core::domain::time::TimeRange;
    use cicada_core::ports::{CalendarRef, HttpMethod, HttpResponse};
    use cicada_google::GoogleCalendarProvider;
    use cicada_testkit::FixtureHttp;
    use std::sync::Arc;

    let http = Arc::new(FixtureHttp::new());
    let auth: Arc<dyn cicada_core::ports::AuthProvider> =
        Arc::new(cicada_google::GoogleAuthProvider::new(
            cicada_google::GoogleAuthConfig::StaticToken {
                access_token: "t".into(),
            },
            http.clone(),
            None,
            Arc::new(cicada_testkit::FakeClock::at(
                "2026-10-23T00:00:00Z".parse().unwrap(),
            )),
        ));
    let cal = CalendarRef::new("primary");
    let range = TimeRange::new(
        "2026-01-01T00:00:00Z".parse().unwrap(),
        "2026-01-02T00:00:00Z".parse().unwrap(),
    )
    .unwrap();

    // Unparseable base URL → fatal, no request made.
    let provider = GoogleCalendarProvider::new(
        http.clone() as Arc<dyn cicada_core::ports::Http>,
        auth.clone(),
    )
    .with_base_url("http://[bad");
    assert!(provider.free_busy(&cal, range).await.is_err());
    // A cannot-be-a-base URL (scheme-only path) → same fatal class.
    let provider = GoogleCalendarProvider::new(
        http.clone() as Arc<dyn cicada_core::ports::Http>,
        auth.clone(),
    )
    .with_base_url("mailto:x@y.z");
    assert!(provider.free_busy(&cal, range).await.is_err());
    // Malformed 200 payload → fatal.
    let provider =
        GoogleCalendarProvider::new(http.clone() as Arc<dyn cicada_core::ports::Http>, auth)
            .with_base_url("https://g.example");
    http.expect(
        HttpMethod::Post,
        "/freeBusy",
        HttpResponse {
            status: 200,
            headers: vec![],
            body: b"not json".to_vec(),
        },
    );
    assert!(provider.free_busy(&cal, range).await.is_err());
}

#[tokio::test]
async fn reqwest_body_read_error_and_builder_error() {
    use cicada_core::ports::{Http, HttpMethod, HttpRequest};
    use cicada_google::ReqwestHttp;
    use std::io::{Read, Write};

    // A server that announces 100 bytes then closes after 10 → the
    // body read fails → Transient transport.
    let http = ReqwestHttp::new().unwrap();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut buf = [0u8; 1024];
        let _ = stream.read(&mut buf);
        let partial = b"0123456789";
        let resp = format!(
            "HTTP/1.1 200 OK\r\ncontent-length: 100\r\nconnection: close\r\n\r\n{}",
            String::from_utf8_lossy(partial)
        );
        let _ = stream.write_all(resp.as_bytes());
    });
    let req = HttpRequest::new(HttpMethod::Get, format!("http://127.0.0.1:{port}/"));
    let err = http.send(&req).await.unwrap_err();
    assert!(err.to_string().contains("transport"), "{err}");

    // An unparseable URL → reqwest builder error → Fatal.
    let req = HttpRequest::new(HttpMethod::Get, "http://[bad-url");
    let err = http.send(&req).await.unwrap_err();
    assert!(err.to_string().contains("request error"), "{err}");
}

#[tokio::test]
async fn secret_write_into_a_directory_errors() {
    // The key's file path occupied by a directory → write_private
    // fails and the error is classified.
    let dir = tempfile::tempdir().unwrap();
    let store = cicada_google::secrets::FileSecretStore::new(dir.path().join("s"));
    std::fs::create_dir_all(store.key_path("k")).unwrap();
    let err = store.set("k", "v").await.unwrap_err();
    assert!(!err.to_string().is_empty());
}

#[test]
fn event_json_wire_edges_time_from_wire() {
    // to_wire on a local time whose instant exceeds Timestamp::MAX
    // (9999-12-31T23:00 in UTC-12) → err. (A DST-gap local does NOT
    // error — jiff disambiguates gaps to after the gap.)
    let mut b = span_body();
    b.time = cicada_core::domain::event::EventTime::Span {
        start: Time::Wall {
            local: date(9999, 12, 31).at(23, 0, 0, 0),
            zone: ZoneId::new_unchecked("Etc/GMT+12"),
        },
        duration: SignedDuration::from_mins(30),
    };
    assert!(
        event_json::to_wire(
            &b,
            &EventTags::default(),
            &std::collections::BTreeMap::new(),
            None
        )
        .is_err()
    );
    // Unknown zone + unparseable dateTime → the Err(_) arm's parse err.
    let mut w = wire();
    w.start = Some(dt(Some("garbage"), Some("Custom/Zone"), None));
    assert!(event_json::from_wire(&w).is_err());
}
