//! `FakeFileProvider` + `FakeCalendarProvider` coverage sweep:
//! file-format errors, persist failure, the delegation arms, and
//! `overlaps`' conservative-true fallbacks.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::fs;

use cicada_core::domain::capabilities::ProviderCapabilities;
use cicada_core::domain::event::{EventBody, EventTime, Transparency};
use cicada_core::domain::ids::{LocalId, RemoteId, ZoneId};
use cicada_core::domain::policy::WriteOptions;
use cicada_core::domain::recurrence::RecurrenceRule;
use cicada_core::domain::time::{Time, TimeRange};
use cicada_core::ports::{CalendarProvider, CalendarRef, EventTags, RemoteEvent};
use cicada_testkit::{FakeCalendarProvider, FakeFileProvider, FakeSnapshot};
use jiff::SignedDuration;
use jiff::civil::date;

fn body(summary: &str) -> EventBody {
    EventBody {
        summary: summary.into(),
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

fn opts() -> WriteOptions {
    WriteOptions::default()
}

fn range() -> TimeRange {
    TimeRange::new(
        "2026-01-01T00:00:00Z".parse().unwrap(),
        "2026-02-01T00:00:00Z".parse().unwrap(),
    )
    .unwrap()
}

#[tokio::test]
async fn open_rejects_malformed_and_wrong_version_files() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("fake.json");

    fs::write(&path, "{ not json").unwrap();
    let err = FakeFileProvider::open(&path).unwrap_err();
    assert!(err.to_string().contains("malformed"), "{err}");

    fs::write(
        &path,
        r#"{"apiVersion":"cicada/fake-provider-v99","calendars":{}}"#,
    )
    .unwrap();
    let err = FakeFileProvider::open(&path).unwrap_err();
    assert!(err.to_string().contains("apiVersion"), "{err}");

    // A directory can't be read at all.
    let dir_path = dir.path().join("a-dir");
    fs::create_dir(&dir_path).unwrap();
    let err = FakeFileProvider::open(&dir_path).unwrap_err();
    assert!(err.to_string().contains("cannot read"), "{err}");

    // An empty file starts empty rather than failing.
    fs::write(&path, "  \n").unwrap();
    let p = FakeFileProvider::open(&path).unwrap();
    assert_eq!(p.path(), path);
    assert!(
        p.list_events(&CalendarRef::new("primary"), None)
            .await
            .unwrap()
            .is_empty()
    );
    assert!(p.capabilities().supports_tagging);
}

#[tokio::test]
async fn open_with_capabilities_and_accessors() {
    let dir = tempfile::tempdir().unwrap();
    let mut caps = ProviderCapabilities::permissive();
    caps.supports_free_busy = false;
    let p = FakeFileProvider::open_with_capabilities(dir.path().join("f.json"), caps).unwrap();
    assert!(!p.capabilities().supports_free_busy);
    // The test seam hands back the in-memory fake.
    p.fake()
        .seed_foreign(&CalendarRef::new("primary"), body("foreign"));
    assert_eq!(
        p.list_events(&CalendarRef::new("primary"), None)
            .await
            .unwrap()
            .len(),
        1
    );
    // free_busy surfaces the capability error through the delegate.
    let err = p
        .free_busy(&CalendarRef::new("primary"), range())
        .await
        .unwrap_err();
    assert!(err.to_string().contains("freeBusy"), "{err}");
}

#[tokio::test]
async fn mutations_persist_and_read_delegates_work() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("fake.json");
    let p = FakeFileProvider::open(&path).unwrap();
    let cal = CalendarRef::new("primary");
    let id = LocalId::new("a");

    let created = p
        .create_event(&cal, &body("A"), &id, &opts())
        .await
        .unwrap();
    assert!(path.exists());
    assert_eq!(
        p.get_event(&cal, &created.remote_id)
            .await
            .unwrap()
            .unwrap()
            .remote_id,
        created.remote_id
    );
    assert_eq!(
        p.find_by_tag(&cal, &id).await.unwrap().unwrap().remote_id,
        created.remote_id
    );

    let updated = p
        .update_event(&cal, &created.remote_id, &body("A2"), &opts())
        .await
        .unwrap();
    assert_eq!(updated.body.summary, "A2");

    p.delete_event(&cal, &created.remote_id, &opts())
        .await
        .unwrap();
    assert!(
        p.get_event(&cal, &created.remote_id)
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn persist_failure_when_target_path_is_a_directory() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("fake.json");
    let p = FakeFileProvider::open(&path).unwrap();
    // Swap the file for a directory — the rename can't land on it.
    fs::create_dir(&path).unwrap();
    let err = p
        .create_event(
            &CalendarRef::new("primary"),
            &body("A"),
            &LocalId::new("a"),
            &opts(),
        )
        .await
        .unwrap_err();
    assert!(err.to_string().contains("cannot"), "{err}");
}

#[test]
fn snapshot_and_provider_defaults() {
    let snap = FakeSnapshot::default();
    assert_eq!(snap.api_version, FakeSnapshot::API_VERSION);
    let p = FakeCalendarProvider::default();
    assert!(p.capabilities().supports_tagging);
}

#[tokio::test]
async fn seed_and_edit_seams_cover_their_arms() {
    let p = FakeCalendarProvider::new();
    let cal = CalendarRef::new("primary");
    // `seed` inserts a verbatim RemoteEvent.
    let mut ev = RemoteEvent {
        remote_id: RemoteId::new("remote-1"),
        tags: EventTags::default(),
        etag: None,
        updated: None,
        body: body("verbatim"),
    };
    p.seed(&cal, ev.clone());
    assert_eq!(p.events(&cal).len(), 1);
    // Editing a missing remote id reports false.
    assert!(!p.simulate_remote_edit(&cal, &RemoteId::new("nope"), |_| {}));
    // …but the real one edits fine (state_hash-free event keeps going).
    ev.body.summary = "edited".into();
    p.seed(&cal, ev);
    assert!(
        p.simulate_remote_edit(&cal, &RemoteId::new("remote-1"), |b| {
            b.location = Some("room".into());
        })
    );
}

#[tokio::test]
async fn overlaps_is_conservatively_true_on_unexpandable() {
    let p = FakeCalendarProvider::new();
    let cal = CalendarRef::new("primary");

    // A recurring event whose rule can't expand still counts as
    // overlapping (conservative inclusion).
    let mut exotic = body("exotic");
    exotic.recurrence = Some(RecurrenceRule::Raw("FREQ=DAILY".into()));
    exotic.time = EventTime::Span {
        start: Time::Wall {
            local: date(2026, 1, 5).at(9, 0, 0, 0),
            zone: ZoneId::new_unchecked("Not/AZone"),
        },
        duration: SignedDuration::from_mins(30),
    };
    p.seed_foreign(&cal, exotic);

    // A non-recurring event with an unresolvable zone — start can't
    // materialize, still reported as overlapping.
    let mut bad_zone = body("bad zone");
    bad_zone.time = EventTime::Span {
        start: Time::Wall {
            local: date(2026, 1, 5).at(9, 0, 0, 0),
            zone: ZoneId::new_unchecked("Not/AZone"),
        },
        duration: SignedDuration::from_mins(30),
    };
    p.seed_foreign(&cal, bad_zone);

    // An event outside the window is filtered out.
    let mut away = body("away");
    away.time = EventTime::Span {
        start: Time::Wall {
            local: date(2027, 1, 5).at(9, 0, 0, 0),
            zone: ZoneId::new_unchecked("UTC"),
        },
        duration: SignedDuration::from_mins(30),
    };
    p.seed_foreign(&cal, away);

    let listed = p.list_events(&cal, Some(range())).await.unwrap();
    assert_eq!(listed.len(), 2, "{listed:?}");
    let summaries: Vec<_> = listed.iter().map(|e| e.body.summary.as_str()).collect();
    assert!(summaries.contains(&"exotic"));
    assert!(summaries.contains(&"bad zone"));
}

#[test]
fn expectation_debug_renders_outcome() {
    use cicada_core::ports::{HttpMethod, HttpResponse};
    use cicada_testkit::FixtureHttp;
    let http = FixtureHttp::new();
    http.expect(
        HttpMethod::Get,
        "/x",
        HttpResponse {
            status: 200,
            headers: vec![],
            body: vec![],
        },
    );
    let dbg = format!("{http:?}");
    assert!(dbg.contains("FixtureHttp"), "{dbg}");
}
