//! Dispatch-level tests — drive [`cicada_mcp::tools::dispatch`]
//! directly against a tempdir manifest + file-backed fake provider +
//! real `JsonFileStateStore`. No transport: these assert on
//! [`ToolOutcome`] shapes agents consume.
#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::needless_pass_by_value
)]

use std::collections::BTreeSet;
use std::fs;
use std::path::Path;
use std::sync::Mutex;

use cicada_core::ports::StateStore;
use cicada_mcp::ctx::{ServerOptions, Session};
use cicada_mcp::tools::{self, IssuedDigests};
use serde_json::{Value, json};

/// Write a manifest + one calendar into `root` and open a session.
fn fixture(root: &Path) -> Session {
    fs::create_dir_all(root.join("calendars")).unwrap();
    fs::write(
        root.join("cicada.yaml"),
        "apiVersion: cicada/v1\nkind: Manifest\ncalendars: [calendars/work.yaml]\n",
    )
    .unwrap();
    fs::write(
        root.join("calendars/work.yaml"),
        "apiVersion: cicada/v1\n\
         kind: Calendar\n\
         metadata:\n\
         \x20 name: work\n\
         \x20 timeZone: UTC\n\
         spec:\n\
         \x20 events:\n\
         \x20   - id: standup\n\
         \x20     summary: Daily Standup\n\
         \x20     start: \"2026-01-05T09:00:00\"\n\
         \x20     duration: 30m\n\
         \x20     recurrence: { freq: DAILY, count: 5 }\n",
    )
    .unwrap();
    Session::open(&ServerOptions {
        root: Some(root.to_path_buf()),
        provider: None,
        fake_file: None,
        state: None,
    })
    .expect("session opens")
}

fn issued() -> IssuedDigests {
    Mutex::new(BTreeSet::new())
}

fn args(v: Value) -> Option<serde_json::Map<String, Value>> {
    v.as_object().cloned()
}

/// Preview → digest → apply with confirm — the happy path.
async fn preview_and_apply(session: &Session, issued: &IssuedDigests) -> Value {
    let preview = tools::dispatch(session, issued, "cicada.plan.preview", args(json!({}))).await;
    assert!(!preview.is_error, "preview failed: {}", preview.text);
    let digest = preview.structured["planDigest"]
        .as_str()
        .expect("planDigest")
        .to_owned();
    let apply = tools::dispatch(
        session,
        issued,
        "cicada.plan.apply",
        args(json!({ "planDigest": digest, "confirm": true })),
    )
    .await;
    assert!(!apply.is_error, "apply failed: {}", apply.text);
    apply.structured
}

#[tokio::test(flavor = "current_thread")]
async fn tool_errors_when_provider_or_calendars_fail() {
    let dir = tempfile::tempdir().unwrap();
    // provider() inside the tool fails — google selected but no
    // credentials in the environment (open() only resolves the
    // name; the adapter builds per call).
    fs::create_dir_all(dir.path().join("calendars")).unwrap();
    fs::write(
        dir.path().join("cicada.yaml"),
        "apiVersion: cicada/v1\nkind: Manifest\ncalendars: [calendars/work.yaml]\n",
    )
    .unwrap();
    fs::write(
        dir.path().join("calendars/work.yaml"),
        "apiVersion: cicada/v1\nkind: Calendar\nmetadata: {name: work, timeZone: UTC}\nspec: {events: []}\n",
    )
    .unwrap();
    let session = Session::open(&ServerOptions {
        root: Some(dir.path().to_path_buf()),
        provider: Some("google".into()),
        fake_file: None,
        state: None,
    })
    .unwrap();
    let out = tools::dispatch(&session, &issued(), "cicada.plan.preview", args(json!({}))).await;
    assert!(out.is_error, "{}", out.text);

    // calendars() fails when a declared file turns unreadable.
    let session = fixture(dir.path());
    fs::write(dir.path().join("calendars/work.yaml"), "not: [valid\n").unwrap();
    let out = tools::dispatch(&session, &issued(), "cicada.plan.preview", args(json!({}))).await;
    assert!(out.is_error, "{}", out.text);
    let out = tools::dispatch(&session, &issued(), "cicada.calendar.list", None).await;
    assert!(out.is_error, "{}", out.text);
    let out = tools::dispatch(
        &session,
        &issued(),
        "cicada.events.show",
        args(json!({ "id": "standup" })),
    )
    .await;
    assert!(out.is_error, "{}", out.text);
}

#[tokio::test(flavor = "current_thread")]
async fn issued_mutex_poisoning_is_tolerated() {
    let dir = tempfile::tempdir().unwrap();
    let session = fixture(dir.path());
    let issued = issued();
    // Issue a digest normally.
    let preview = tools::dispatch(&session, &issued, "cicada.plan.preview", args(json!({}))).await;
    assert!(!preview.is_error, "{}", preview.text);
    let digest = preview.structured["planDigest"]
        .as_str()
        .expect("planDigest")
        .to_owned();
    // Poison the digest mutex — a panic inside the guard.
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _g = issued.lock().unwrap();
        panic!("poison");
    }));
    // Next preview's `if let Ok` arm can't take — tolerated.
    let out = tools::dispatch(&session, &issued, "cicada.plan.preview", args(json!({}))).await;
    assert!(!out.is_error, "{}", out.text);
    // Apply's membership check sees the poisoned mutex as "not
    // issued" — the `is_ok_and` guard-false arm reports unknown.
    let out = tools::dispatch(
        &session,
        &issued,
        "cicada.plan.apply",
        args(json!({ "planDigest": digest, "confirm": true })),
    )
    .await;
    assert!(out.is_error);
    assert!(out.text.contains("unknown planDigest"), "{}", out.text);
}

#[tokio::test(flavor = "current_thread")]
async fn calendar_list_reports_declared() {
    let dir = tempfile::tempdir().unwrap();
    let session = fixture(dir.path());
    let out = tools::dispatch(&session, &issued(), "cicada.calendar.list", None).await;
    assert!(!out.is_error, "{}", out.text);
    let cals = out.structured["calendars"].as_array().unwrap();
    assert_eq!(cals.len(), 1);
    assert_eq!(cals[0]["name"], "work");
    assert_eq!(cals[0]["timeZone"], "UTC");
    assert_eq!(cals[0]["calendarId"], "primary");
    assert_eq!(cals[0]["declaredEvents"], 1);
}

#[tokio::test(flavor = "current_thread")]
async fn events_range_expands_recurrence() {
    let dir = tempfile::tempdir().unwrap();
    let session = fixture(dir.path());
    let out = tools::dispatch(
        &session,
        &issued(),
        "cicada.events.range",
        args(json!({ "from": "2026-01-05", "to": "2026-01-10" })),
    )
    .await;
    assert!(!out.is_error, "{}", out.text);
    assert_eq!(out.structured["totalOccurrences"], 5);
    let occs = out.structured["calendars"][0]["occurrences"]
        .as_array()
        .unwrap();
    assert_eq!(occs[0]["localId"], "standup");
    assert!(occs[0]["recurring"].as_bool().unwrap());
    assert!(occs.iter().all(|o| o["summary"] == "Daily Standup"));
}

#[tokio::test(flavor = "current_thread")]
async fn events_show_returns_detail() {
    let dir = tempfile::tempdir().unwrap();
    let session = fixture(dir.path());
    let out = tools::dispatch(
        &session,
        &issued(),
        "cicada.events.show",
        args(json!({ "id": "standup", "from": "2026-01-05", "to": "2026-01-07" })),
    )
    .await;
    assert!(!out.is_error, "{}", out.text);
    assert_eq!(out.structured["calendar"], "work");
    assert_eq!(out.structured["event"]["summary"], "Daily Standup");
    assert_eq!(out.structured["occurrences"].as_array().unwrap().len(), 2);

    let missing = tools::dispatch(
        &session,
        &issued(),
        "cicada.events.show",
        args(json!({ "id": "nope" })),
    )
    .await;
    assert!(missing.is_error);
    assert_eq!(missing.structured["error"]["kind"], "fatal");
}

#[tokio::test(flavor = "current_thread")]
async fn preview_issues_digest_and_plan_v1() {
    let dir = tempfile::tempdir().unwrap();
    let session = fixture(dir.path());
    let set = issued();
    let out = tools::dispatch(&session, &set, "cicada.plan.preview", None).await;
    assert!(!out.is_error, "{}", out.text);
    let digest = out.structured["planDigest"].as_str().unwrap();
    assert!(digest.starts_with("sha256:"));
    assert_eq!(out.structured["remoteOps"], 1);
    let plans = out.structured["plans"].as_array().unwrap();
    assert_eq!(plans[0]["apiVersion"], "cicada/plan-v1");
    assert_eq!(plans[0]["calendar"], "work");
    assert_eq!(plans[0]["ops"][0]["kind"], "create");
    // The digest is recorded in-session.
    assert!(set.lock().unwrap().contains(digest));
}

#[tokio::test(flavor = "current_thread")]
async fn apply_requires_confirm_true() {
    let dir = tempfile::tempdir().unwrap();
    let session = fixture(dir.path());
    let set = issued();
    let out = tools::dispatch(
        &session,
        &set,
        "cicada.plan.apply",
        args(json!({ "planDigest": "sha256:x", "confirm": false })),
    )
    .await;
    assert!(out.is_error);
    assert!(out.text.contains("confirm: true"), "{}", out.text);
    // Provider file was never written.
    assert!(!dir.path().join(".cicada/fake-provider.json").exists());
}

#[tokio::test(flavor = "current_thread")]
async fn apply_rejects_unissued_digest() {
    let dir = tempfile::tempdir().unwrap();
    let session = fixture(dir.path());
    let out = tools::dispatch(
        &session,
        &issued(),
        "cicada.plan.apply",
        args(json!({ "planDigest": "sha256:notissued", "confirm": true })),
    )
    .await;
    assert!(out.is_error);
    assert!(out.text.contains("unknown planDigest"), "{}", out.text);
}

#[tokio::test(flavor = "current_thread")]
async fn apply_rejects_stale_digest() {
    let dir = tempfile::tempdir().unwrap();
    let session = fixture(dir.path());
    let set = issued();
    let preview = tools::dispatch(&session, &set, "cicada.plan.preview", None).await;
    let digest = preview.structured["planDigest"]
        .as_str()
        .unwrap()
        .to_owned();

    // Desired YAML changed since the preview → digest is stale.
    fs::write(
        dir.path().join("calendars/work.yaml"),
        "apiVersion: cicada/v1\n\
         kind: Calendar\n\
         metadata:\n\
         \x20 name: work\n\
         \x20 timeZone: UTC\n\
         spec:\n\
         \x20 events:\n\
         \x20   - id: standup\n\
         \x20     summary: Standup Renamed\n\
         \x20     start: \"2026-01-05T09:00:00\"\n\
         \x20     duration: 30m\n\
         \x20     recurrence: { freq: DAILY, count: 5 }\n",
    )
    .unwrap();

    let out = tools::dispatch(
        &session,
        &set,
        "cicada.plan.apply",
        args(json!({ "planDigest": digest, "confirm": true })),
    )
    .await;
    assert!(out.is_error);
    assert!(out.text.contains("stale planDigest"), "{}", out.text);
    // Nothing was written remotely.
    assert!(!dir.path().join(".cicada/fake-provider.json").exists());
}

#[tokio::test(flavor = "current_thread")]
async fn gated_apply_lands_and_digest_is_spent() {
    let dir = tempfile::tempdir().unwrap();
    let session = fixture(dir.path());
    let set = issued();
    let applied = preview_and_apply(&session, &set).await;

    let cal = &applied["calendars"][0];
    assert_eq!(cal["calendar"], "work");
    assert_eq!(cal["applied"], 1);
    let outcome = &cal["outcomes"][0];
    assert_eq!(outcome["op"], "create");
    assert_eq!(outcome["status"], "applied");
    assert!(outcome["remoteId"].is_string());
    // State file recorded the event.
    let state = session.store().load().await.unwrap();
    assert_eq!(state.calendar("work").unwrap().events.len(), 1);

    // A second apply with the same digest is refused — digests are
    // one-shot; the agent must re-preview.
    let again = tools::dispatch(
        &session,
        &set,
        "cicada.plan.apply",
        args(json!({ "planDigest": applied["digest"], "confirm": true })),
    )
    .await;
    assert!(again.is_error);
    assert!(again.text.contains("unknown planDigest"), "{}", again.text);

    // A fresh preview now plans a no-op — recorded == desired == live.
    let preview = tools::dispatch(&session, &set, "cicada.plan.preview", None).await;
    assert_eq!(preview.structured["remoteOps"], 0);
}

#[tokio::test(flavor = "current_thread")]
async fn drift_reports_out_of_band_edit() {
    let dir = tempfile::tempdir().unwrap();
    let session = fixture(dir.path());
    let set = issued();
    preview_and_apply(&session, &set).await;

    // Out-of-band edit: change the summary inside the fake provider
    // snapshot (simulates the provider UI). The etag must bump too —
    // where both sides expose one, etags are the authoritative drift
    // signal, and real providers bump on every write.
    let file = dir.path().join(".cicada/fake-provider.json");
    let mut snap: Value = serde_json::from_str(&fs::read_to_string(&file).unwrap()).unwrap();
    let event = snap["calendars"]["primary"]
        .as_object_mut()
        .unwrap()
        .values_mut()
        .next()
        .unwrap();
    event["body"]["summary"] = json!("Edited Remotely");
    event["etag"] = json!("etag-edited");
    fs::write(&file, serde_json::to_string_pretty(&snap).unwrap()).unwrap();

    let out = tools::dispatch(&session, &set, "cicada.state.drift", None).await;
    assert!(!out.is_error, "{}", out.text);
    assert_eq!(out.structured["totalDrifted"], 1);
    let entry = &out.structured["calendars"][0]["drift"][0];
    assert_eq!(entry["kind"], "modified");
    assert_eq!(entry["localId"], "standup");

    // Refresh reconciles recorded state to live truth.
    let refresh = tools::dispatch(&session, &set, "cicada.state.refresh", None).await;
    assert!(!refresh.is_error, "{}", refresh.text);
    assert_eq!(
        refresh.structured["calendars"][0]["updated"],
        json!(["standup"])
    );

    // Drift is gone after refresh.
    let out = tools::dispatch(&session, &set, "cicada.state.drift", None).await;
    assert_eq!(out.structured["totalDrifted"], 0);
}

#[tokio::test(flavor = "current_thread")]
async fn drift_without_state_reports_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let session = fixture(dir.path());
    let out = tools::dispatch(&session, &issued(), "cicada.state.drift", None).await;
    assert!(!out.is_error);
    assert_eq!(out.structured["totalDrifted"], 0);
}

#[tokio::test(flavor = "current_thread")]
async fn no_adhoc_create_and_unknown_tool_errors() {
    // ADR-0002 §10: the write path is YAML + plan/apply only.
    let names = tools::tool_names();
    assert_eq!(names.len(), 7);
    assert!(!names.iter().any(|n| n.contains("create")));

    let dir = tempfile::tempdir().unwrap();
    let session = fixture(dir.path());
    let out = tools::dispatch(&session, &issued(), "cicada.events.create", None).await;
    assert!(out.is_error);
    assert_eq!(out.structured["error"]["kind"], "fatal");
}

#[tokio::test(flavor = "current_thread")]
async fn bad_arguments_are_fatal_tool_errors() {
    let dir = tempfile::tempdir().unwrap();
    let session = fixture(dir.path());
    let out = tools::dispatch(
        &session,
        &issued(),
        "cicada.events.show",
        args(json!({ "unexpected": 1 })),
    )
    .await;
    assert!(out.is_error);
    assert!(out.text.contains("invalid arguments"), "{}", out.text);
}

#[tokio::test(flavor = "current_thread")]
async fn store_and_provider_failures_surface_as_tool_errors() {
    let dir = tempfile::tempdir().unwrap();
    let session = fixture(dir.path());
    let set = issued();
    // First apply lands recorded state — `calendar.list` then reads
    // the recorded-events arm.
    let _ = preview_and_apply(&session, &set).await;
    let out = tools::dispatch(&session, &set, "cicada.calendar.list", None).await;
    assert_eq!(out.structured["calendars"][0]["recordedEvents"], 1);

    // A malformed state file fails every store().load() caller.
    fs::write(dir.path().join(".cicada/state.yaml"), "bogus: [").unwrap();
    for tool in [
        "cicada.calendar.list",
        "cicada.state.drift",
        "cicada.plan.preview",
    ] {
        let out = tools::dispatch(&session, &set, tool, None).await;
        assert!(out.is_error, "{tool} should fail on a malformed state file");
    }
    let out = tools::dispatch(&session, &set, "cicada.state.refresh", None).await;
    assert!(out.is_error);
    // plan.apply routes through plan_apply_locked → plan_all's load
    // failure.
    let out = tools::dispatch(
        &session,
        &set,
        "cicada.plan.apply",
        args(json!({ "planDigest": "x", "confirm": true })),
    )
    .await;
    assert!(out.is_error);

    // A lock whose parent path is a file fails lock() inside
    // refresh/apply.
    fs::write(dir.path().join("blocker"), "x").unwrap();
    let session2 = Session::open(&ServerOptions {
        root: Some(dir.path().to_path_buf()),
        provider: None,
        fake_file: None,
        state: Some(dir.path().join("blocker/state.yaml")),
    })
    .expect("session opens");
    let out = tools::dispatch(&session2, &set, "cicada.state.refresh", None).await;
    assert!(out.is_error);
    // With the digest issued, apply gets past the gates and the
    // lock() failure surfaces from plan_apply_locked's `?` arm.
    set.lock().unwrap().insert("sha256:x".to_owned());
    let out = tools::dispatch(
        &session2,
        &set,
        "cicada.plan.apply",
        args(json!({ "planDigest": "sha256:x", "confirm": true })),
    )
    .await;
    assert!(out.is_error);

    // An unresolvable provider name fails provider() inside each
    // state tool — the map_err(fatal) arms.
    let session3 = Session::open(&ServerOptions {
        root: Some(dir.path().to_path_buf()),
        provider: Some("google".to_owned()),
        fake_file: None,
        state: None,
    })
    .expect("session opens");
    for tool in ["cicada.state.drift", "cicada.state.refresh"] {
        let out = tools::dispatch(&session3, &set, tool, None).await;
        assert!(out.is_error, "{tool}");
    }
    let out = tools::dispatch(
        &session3,
        &set,
        "cicada.plan.apply",
        args(json!({ "planDigest": "x", "confirm": true })),
    )
    .await;
    assert!(out.is_error);
}

#[tokio::test(flavor = "current_thread")]
async fn apply_errors_when_the_plan_has_error_diagnostics() {
    let dir = tempfile::tempdir().unwrap();
    let session = fixture(dir.path());
    let set = issued();
    // An unexpandable recurrence yields a plan-time error → apply
    // refuses (ApplyPlan::run error arm).
    fs::write(
        dir.path().join("calendars/work.yaml"),
        "apiVersion: cicada/v1\n\
         kind: Calendar\n\
         metadata:\n\
         \x20 name: work\n\
         \x20 timeZone: UTC\n\
         spec:\n\
         \x20 events:\n\
         \x20   - id: standup\n\
         \x20     summary: S\n\
         \x20     start: \"2026-01-05T09:00:00\"\n\
         \x20     duration: 30m\n\
         \x20     recurrence: { freq: HOURLY }\n",
    )
    .unwrap();
    let preview = tools::dispatch(&session, &set, "cicada.plan.preview", args(json!({}))).await;
    let digest = preview.structured["planDigest"]
        .as_str()
        .unwrap()
        .to_owned();
    let out = tools::dispatch(
        &session,
        &set,
        "cicada.plan.apply",
        args(json!({ "planDigest": digest, "confirm": true })),
    )
    .await;
    assert!(out.is_error);
}

#[test]
fn session_open_rejects_unknown_provider() {
    // `provider_name(...)?` — an unrecognized provider name aborts
    // session open before any file resolution.
    let dir = tempfile::tempdir().unwrap();
    let res = Session::open(&ServerOptions {
        root: Some(dir.path().to_path_buf()),
        provider: Some("bogus".into()),
        fake_file: None,
        state: None,
    });
    let Err(err) = res else {
        panic!("bogus provider must fail")
    };
    assert!(err.contains("unknown provider"), "{err}");
}

#[tokio::test(flavor = "current_thread")]
async fn events_range_singularizes_one_occurrence() {
    // `occurrences.len() == 1` — the singular `""` arm of the
    // pluralizer (the default fixture event repeats five times).
    let dir = tempfile::tempdir().unwrap();
    let session = fixture(dir.path());
    let out = tools::dispatch(
        &session,
        &issued(),
        "cicada.events.range",
        args(json!({ "from": "2026-01-05", "to": "2026-01-06" })),
    )
    .await;
    assert!(!out.is_error, "{}", out.text);
    assert!(out.text.contains("1 occurrence in"), "{}", out.text);
}

#[tokio::test(flavor = "current_thread")]
async fn plan_preview_pluralizes_multiple_calendars() {
    // `plans.len() != 1` — the plural `"s"` arm of the summary.
    let dir = tempfile::tempdir().unwrap();
    fs::create_dir_all(dir.path().join("calendars")).unwrap();
    fs::write(
        dir.path().join("cicada.yaml"),
        "apiVersion: cicada/v1\nkind: Manifest\ncalendars: [calendars/a.yaml, calendars/b.yaml]\n",
    )
    .unwrap();
    for name in ["a", "b"] {
        fs::write(
            dir.path().join(format!("calendars/{name}.yaml")),
            "apiVersion: cicada/v1\n\
             kind: Calendar\n\
             metadata:\n\
             \x20 name: w\n\
             \x20 timeZone: UTC\n\
             spec:\n\
             \x20 events:\n\
             \x20   - id: standup\n\
             \x20     summary: S\n\
             \x20     start: \"2026-01-05T09:00:00\"\n\
             \x20     duration: 30m\n",
        )
        .unwrap();
    }
    let session = Session::open(&ServerOptions {
        root: Some(dir.path().to_path_buf()),
        provider: None,
        fake_file: None,
        state: None,
    })
    .expect("session opens");
    let out = tools::dispatch(&session, &issued(), "cicada.plan.preview", args(json!({}))).await;
    assert!(!out.is_error, "{}", out.text);
    assert!(out.text.contains("across 2 calendars"), "{}", out.text);
}

#[tokio::test(flavor = "current_thread")]
async fn plan_apply_fails_when_provider_cannot_open() {
    // `.map_err(ToolOutcome::fatal)?` on `session.provider` in
    // plan_apply_locked — the fake backend file turns unreadable
    // between the preview that issued the digest and the apply.
    let dir = tempfile::tempdir().unwrap();
    let session = fixture(dir.path());
    let set = issued();
    let preview = tools::dispatch(&session, &set, "cicada.plan.preview", args(json!({}))).await;
    assert!(!preview.is_error, "{}", preview.text);
    let digest = preview.structured["planDigest"]
        .as_str()
        .expect("planDigest")
        .to_owned();
    let fake = dir.path().join(".cicada/fake-provider.json");
    fs::create_dir_all(fake.parent().unwrap()).unwrap();
    fs::write(&fake, "not json").unwrap();
    let out = tools::dispatch(
        &session,
        &set,
        "cicada.plan.apply",
        args(json!({ "planDigest": digest, "confirm": true })),
    )
    .await;
    assert!(out.is_error);
    assert!(out.text.contains("cannot open"), "{}", out.text);
}
