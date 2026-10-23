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
