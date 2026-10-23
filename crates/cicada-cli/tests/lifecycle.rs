//! e2e lifecycle: `apply` writes state + fake-provider files,
//! `drift`/`refresh`/`plan` read them back across processes — the
//! file-backed fake makes the whole loop real (test/demo only).
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::fs;
use std::io::Write;
use std::path::Path;
use std::process::{Command, Output, Stdio};

const MANIFEST: &str = r"apiVersion: cicada/v1
kind: Manifest
calendars: [calendars/work.yaml]
";

const CALENDAR: &str = r#"apiVersion: cicada/v1
kind: Calendar
metadata:
  name: work
  timeZone: UTC
  provider: fake
  calendarId: primary
spec:
  apply:
    sendUpdates: none
  events:
    - id: standup
      summary: Daily Standup
      start: "09:00"
      duration: 30m
      recurrence: { freq: WEEKLY, byDay: [MO,TU,WE,TH,FR] }
    - id: offsite
      summary: Team Offsite
      start: "2026-03-20T10:00"
      duration: 4h
"#;

fn fixture() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    fs::create_dir_all(dir.path().join("calendars")).unwrap();
    fs::write(dir.path().join("cicada.yaml"), MANIFEST).unwrap();
    fs::write(dir.path().join("calendars/work.yaml"), CALENDAR).unwrap();
    dir
}

fn cicada(dir: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_cicada"))
        .args(args)
        .current_dir(dir)
        .output()
        .expect("spawn cicada")
}

fn stdout(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn state_file(dir: &Path) -> String {
    fs::read_to_string(dir.join(".cicada/state.yaml")).unwrap()
}

fn fake_file(dir: &Path) -> serde_json::Value {
    serde_json::from_str(&fs::read_to_string(dir.join(".cicada/fake-provider.json")).unwrap())
        .unwrap()
}

#[test]
fn full_lifecycle_apply_drift_refresh_plan() {
    let dir = fixture();

    // apply: the plan prints, ops land, state + fake backend persist.
    let out = cicada(dir.path(), &["apply", "--auto-approve"]);
    let s = stdout(&out);
    assert!(
        out.status.success(),
        "{s}\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(s.contains("create"), "{s}");
    assert!(s.contains("2 applied"), "{s}");
    let state = state_file(dir.path());
    assert!(state.contains("apiVersion: cicada/state-v1"), "{state}");
    assert!(state.contains("remoteId: fake-1"), "{state}");
    assert!(state.contains("journal"), "{state}");

    // drift: nothing out of band — clean.
    let out = cicada(dir.path(), &["drift"]);
    assert!(out.status.success(), "{}", stdout(&out));
    assert!(stdout(&out).contains("no drift"));

    // plan: converged — zero remote ops.
    let out = cicada(dir.path(), &["plan"]);
    let s = stdout(&out);
    assert!(out.status.success(), "{s}");
    assert!(s.contains("noop"), "{s}");
    assert!(s.contains("0 remote ops"), "{s}");

    // Out-of-band edit on the fake backend file — the provider-UI
    // simulation. Rename the event AND bump its etag.
    let mut fake = fake_file(dir.path());
    let events = fake["calendars"]["primary"].as_object_mut().unwrap();
    let event = events.values_mut().next().unwrap();
    event["body"]["summary"] = serde_json::json!("Renamed in the UI");
    event["etag"] = serde_json::json!("etag-999");
    fs::write(
        dir.path().join(".cicada/fake-provider.json"),
        serde_json::to_string_pretty(&fake).unwrap(),
    )
    .unwrap();

    // drift now reports the edit and exits non-zero for CI.
    let out = cicada(dir.path(), &["drift"]);
    let s = stdout(&out);
    assert!(!out.status.success(), "{s}");
    assert!(s.contains("1 drifted"), "{s}");
    assert!(s.contains("modified"), "{s}");

    // refresh folds live back into recorded state.
    let out = cicada(dir.path(), &["refresh"]);
    let s = stdout(&out);
    assert!(out.status.success(), "{s}");
    assert!(s.contains("1 updated"), "{s}");

    // drift clean again…
    let out = cicada(dir.path(), &["drift"]);
    assert!(out.status.success(), "{}", stdout(&out));

    // …and plan now proposes the repair: desired ≠ recorded(=live).
    let out = cicada(dir.path(), &["plan"]);
    let s = stdout(&out);
    assert!(out.status.success(), "{s}");
    assert!(s.contains("update"), "{s}");
}

#[test]
fn apply_without_auto_approve_requires_confirmation() {
    let dir = fixture();
    // Answering "n" aborts and writes nothing.
    let mut child = Command::new(env!("CARGO_BIN_EXE_cicada"))
        .args(["apply"])
        .current_dir(dir.path())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.as_mut().unwrap().write_all(b"n\n").unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(!out.status.success());
    assert!(!dir.path().join(".cicada/state.yaml").exists());
}

#[test]
fn refresh_and_drift_without_state_are_noops_not_errors() {
    let dir = fixture();
    let out = cicada(dir.path(), &["drift"]);
    assert!(out.status.success(), "{}", stdout(&out));
    assert!(stdout(&out).contains("no recorded state"));
    let out = cicada(dir.path(), &["refresh"]);
    assert!(out.status.success(), "{}", stdout(&out));
}
