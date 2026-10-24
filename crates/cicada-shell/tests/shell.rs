//! cicada-shell sweep — time bounds, input resolution, backend
//! selection/dispatch that e2e can't reach directly.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::default_trait_access)]

use std::fs;
use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;
use cicada_core::domain::calendar::Calendar;
use cicada_core::domain::error::Result as CicadaResult;
use cicada_core::domain::event::{EventBody, EventTime, Transparency};
use cicada_core::domain::ids::{LocalId, RemoteId, ZoneId};
use cicada_core::domain::policy::WriteOptions;
use cicada_core::domain::state::RecordedCalendar;
use cicada_core::domain::time::{Time, TimeRange};
use cicada_core::ports::auth::Token;
use cicada_core::ports::{
    AuthProvider, CalendarProvider, CalendarRef, Credentials, Http, RemoteEvent,
};
use cicada_google::GoogleCalendarProvider;
use cicada_shell::backend::{self, Backend};
use cicada_shell::input;
use cicada_shell::timeparse::parse_bound;
use cicada_testkit::{FakeFileProvider, FixtureHttp};

// ---------- timeparse ----------

#[test]
fn parse_bound_accepts_every_form() {
    let zone = ZoneId::new_unchecked("UTC");
    for s in [
        "today",
        "TODAY",
        "+1d",
        "+2w",
        "2026-08-19",
        "2026-08-19T09:00",
        "2026-08-19T09:00:00Z",
        "2026-08-19T09:00:00-03:00",
    ] {
        assert!(parse_bound(s, &zone).is_ok(), "{s}");
    }
    for s in ["nope", "+3", "+xd", "+9999999999999999999d", "2026-13-99"] {
        assert!(parse_bound(s, &zone).is_err(), "{s}");
    }
    // Unresolvable zone surfaces the ZoneId error.
    let bad = ZoneId::new_unchecked("Not/AZone");
    assert!(parse_bound("today", &bad).is_err());
}

// ---------- input ----------

const CALENDAR: &str = "apiVersion: cicada/v1\nkind: Calendar\nmetadata:\n  name: work\n  timeZone: UTC\nspec:\n  events: []\n";

fn write_manifest(dir: &Path) {
    fs::write(
        dir.join("cicada.yaml"),
        "apiVersion: cicada/v1\nkind: Manifest\ncalendars: [calendars/work.yaml]\n",
    )
    .unwrap();
    fs::create_dir_all(dir.join("calendars")).unwrap();
    fs::write(dir.join("calendars/work.yaml"), CALENDAR).unwrap();
}

#[test]
fn resolve_discovers_manifest_and_expands_it() {
    let dir = tempfile::tempdir().unwrap();
    // No manifest anywhere → a clear error.
    assert!(input::resolve_calendar_files(dir.path(), &[]).is_err());
    write_manifest(dir.path());
    let files = input::resolve_calendar_files(dir.path(), &[]).unwrap();
    assert_eq!(files.len(), 1);
    // An explicit manifest path expands the same way.
    let files =
        input::resolve_calendar_files(dir.path(), &[dir.path().join("cicada.yaml")]).unwrap();
    assert_eq!(files.len(), 1);
    // Unreadable args stay themselves so the loader reports in place.
    let files = input::resolve_calendar_files(dir.path(), &[dir.path().join("nope.yaml")]).unwrap();
    assert_eq!(files.len(), 1);
    // A non-manifest document stays itself too.
    let cal = dir.path().join("calendars/work.yaml");
    let files = input::resolve_calendar_files(dir.path(), std::slice::from_ref(&cal)).unwrap();
    assert_eq!(files, vec![cal]);
    // cicada.yml is also discovered.
    let dir2 = tempfile::tempdir().unwrap();
    fs::write(
        dir2.path().join("cicada.yml"),
        "apiVersion: cicada/v1\nkind: Manifest\ncalendars: [c.yaml]\n",
    )
    .unwrap();
    fs::write(dir2.path().join("c.yaml"), CALENDAR).unwrap();
    assert_eq!(
        input::resolve_calendar_files(dir2.path(), &[])
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn load_calendars_reports_read_and_parse_failures() {
    let dir = tempfile::tempdir().unwrap();
    let missing = dir.path().join("missing.yaml");
    let err = input::load_calendars(&[missing]).unwrap_err();
    assert!(err.contains("cannot read"), "{err}");
    let bad = dir.path().join("bad.yaml");
    fs::write(&bad, "kind: Bogus\n").unwrap();
    let err = input::load_calendars(&[bad]).unwrap_err();
    assert!(!err.is_empty());
}

#[test]
fn render_schema_error_picks_a_shape() {
    use cicada_schema::SchemaError;
    let at = SchemaError::at("spec.events[0].start", "bad time");
    assert_eq!(
        input::render_schema_error(&at),
        "spec.events[0].start: bad time"
    );
    let doc = SchemaError::document("boom");
    assert_eq!(input::render_schema_error(&doc), "boom");
    // A syntax error carries line:column.
    let err = cicada_schema::parse_calendar(":\n").unwrap_err();
    let rendered = input::render_schema_error(&err);
    assert!(!rendered.is_empty());
}

// ---------- backend ----------

#[test]
fn provider_name_validates_and_defaults() {
    assert_eq!(backend::provider_name(None, "x").unwrap(), "fake");
    assert_eq!(backend::provider_name(Some("fake"), "x").unwrap(), "fake");
    assert_eq!(
        backend::provider_name(Some("google"), "x").unwrap(),
        "google"
    );
    let err = backend::provider_name(Some("bogus"), "cicada plan").unwrap_err();
    assert!(err.contains("bogus"), "{err}");
    assert!(err.contains("cicada plan"), "{err}");
}

#[test]
fn manifest_dir_anchors_to_a_manifest_argument() {
    let dir = tempfile::tempdir().unwrap();
    write_manifest(dir.path());
    // Explicit manifest → its own directory.
    let d = backend::manifest_dir(Path::new("."), &[dir.path().join("cicada.yaml")]);
    assert_eq!(d, dir.path());
    // A calendar arg → the root.
    let d = backend::manifest_dir(Path::new("."), &[dir.path().join("calendars/work.yaml")]);
    assert_eq!(d, Path::new("."));
    // Unreadable arg → root.
    let d = backend::manifest_dir(Path::new("."), &[dir.path().join("nope.yaml")]);
    assert_eq!(d, Path::new("."));
    // No args → root.
    assert_eq!(
        backend::manifest_dir(Path::new("/tmp/x"), &[]),
        Path::new("/tmp/x")
    );
}

#[test]
fn fake_provider_surfaces_open_errors() {
    let dir = tempfile::tempdir().unwrap();
    let bad = dir.path().join("fake.json");
    fs::write(&bad, "{ nope").unwrap();
    let err = backend::fake_provider(Some(&bad), dir.path(), "plan").unwrap_err();
    assert!(err.contains("plan"), "{err}");
    // No file → default path under manifest dir, opens empty.
    let p = backend::fake_provider(None, dir.path(), "plan").unwrap();
    assert!(p.path().ends_with("fake-provider.json"));
}

#[test]
fn remote_calendar_binds_metadata_then_recorded_then_primary() {
    let mut cal = Calendar::new("work", "UTC").unwrap();
    cal.provider_calendar_id = Some("team@group".into());
    let recorded = RecordedCalendar {
        provider: "fake".into(),
        calendar_id: "recorded-id".into(),
        events: Default::default(),
    };
    assert_eq!(
        backend::remote_calendar(&cal, Some(&recorded)).as_str(),
        "team@group"
    );
    let cal = Calendar::new("work", "UTC").unwrap();
    assert_eq!(
        backend::remote_calendar(&cal, Some(&recorded)).as_str(),
        "recorded-id"
    );
    let empty = RecordedCalendar {
        provider: "fake".into(),
        calendar_id: String::new(),
        events: Default::default(),
    };
    assert_eq!(
        backend::remote_calendar(&cal, Some(&empty)).as_str(),
        "primary"
    );
    assert_eq!(backend::remote_calendar(&cal, None).as_str(), "primary");
}

/// A no-network auth stub — static credentials, no refresh.
struct StaticAuth;

#[async_trait]
impl AuthProvider for StaticAuth {
    async fn credentials(&self) -> CicadaResult<Credentials> {
        Ok(Credentials {
            scheme: "Bearer".into(),
            access_token: Token::new("x"),
            expires_at: None,
        })
    }
}

/// Drive every `Backend` dispatch arm — Fake through the real file
/// provider, Google through a fixture-less client (calls fail, the
/// match arm is what counts).
#[tokio::test(flavor = "current_thread")]
async fn backend_dispatch_covers_both_variants() {
    let dir = tempfile::tempdir().unwrap();
    let fake = Backend::Fake(Box::new(
        FakeFileProvider::open(dir.path().join("fake.json")).unwrap(),
    ));
    let http: Arc<dyn Http> = Arc::new(FixtureHttp::new());
    let google = Backend::Google(GoogleCalendarProvider::new(http, Arc::new(StaticAuth)));

    let cal = CalendarRef::new("primary");
    let id = LocalId::new("a");
    let rid = RemoteId::new("r-1");
    let body = EventBody {
        summary: "x".into(),
        description: None,
        location: None,
        time: EventTime::Span {
            start: Time::Instant("2026-01-05T09:00:00Z".parse().unwrap()),
            duration: jiff::SignedDuration::from_mins(30),
        },
        recurrence: None,
        attendees: vec![],
        reminders: vec![],
        conferencing: None,
        transparency: Transparency::Opaque,
    };
    let opts = WriteOptions::default();
    let range = TimeRange::new(
        "2026-01-01T00:00:00Z".parse().unwrap(),
        "2026-02-01T00:00:00Z".parse().unwrap(),
    )
    .unwrap();
    let _ = std::mem::size_of::<RemoteEvent>();

    for backend in [&fake, &google] {
        let caps = backend.capabilities();
        assert!(caps.supports_tagging);
        let _ = backend.list_events(&cal, None).await;
        let _ = backend.get_event(&cal, &rid).await;
        let _ = backend.find_by_tag(&cal, &id).await;
        let _ = backend.create_event(&cal, &body, &id, &opts).await;
        let _ = backend.update_event(&cal, &rid, &body, &opts).await;
        let _ = backend.delete_event(&cal, &rid, &opts).await;
        let _ = backend.free_busy(&cal, range).await;
    }
    // Debug impls exist on both variants.
    let _ = format!("{fake:?} {google:?}");
}
