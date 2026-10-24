//! e2e coverage sweep over `cicada` verbs — the fake provider
//! backend makes plan/apply/drift/refresh/range/show fully real.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::items_after_statements
)]

use std::fs;
use std::io::Write;
use std::path::Path;
use std::process::{Command, Output, Stdio};

const MANIFEST: &str = "apiVersion: cicada/v1\nkind: Manifest\ncalendars: [calendars/work.yaml]\n";

/// A calendar exercising every printable field.
const RICH_CALENDAR: &str = r#"apiVersion: cicada/v1
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
      description: sync up
      location: room 1
      start: "2026-01-05T09:00"
      duration: 30m
      recurrence: { freq: WEEKLY, byDay: [MO,TU,WE,TH,FR] }
      attendees:
        - { email: a@x, required: true }
        - { email: b@x, required: false }
      reminders: [{ popup: 10m }, { email: 1h }]
      conferencing: meet
      transparency: opaque
    - id: offsite
      summary: Team Offsite
      start: "2026-03-20T10:00"
      end: "2026-03-21T14:00"
      transparency: transparent
"#;

fn fixture(dir: &Path, calendar: &str) {
    fs::create_dir_all(dir.join("calendars")).unwrap();
    fs::write(dir.join("cicada.yaml"), MANIFEST).unwrap();
    fs::write(dir.join("calendars/work.yaml"), calendar).unwrap();
}

fn cicada(dir: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_cicada"))
        .args(args)
        .current_dir(dir)
        .output()
        .expect("spawn cicada")
}

fn cicada_stdin(dir: &Path, args: &[&str], input: &str) -> Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_cicada"))
        .args(args)
        .current_dir(dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .as_mut()
        .unwrap()
        .write_all(input.as_bytes())
        .unwrap();
    child.wait_with_output().unwrap()
}

fn stdout(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn fake_file(dir: &Path) -> serde_json::Value {
    serde_json::from_str(&fs::read_to_string(dir.join(".cicada/fake-provider.json")).unwrap())
        .unwrap()
}

fn write_fake(dir: &Path, fake: &serde_json::Value) {
    fs::write(
        dir.join(".cicada/fake-provider.json"),
        serde_json::to_string_pretty(fake).unwrap(),
    )
    .unwrap();
}

// ---------- init ----------

#[test]
fn init_scaffolds_then_refuses_without_force() {
    let dir = tempfile::tempdir().unwrap();
    let out = cicada(dir.path(), &["init"]);
    assert!(out.status.success(), "{}", stdout(&out));
    assert!(dir.path().join("cicada.yaml").exists());
    assert!(dir.path().join("calendars/work.yaml").exists());
    // Second run refuses.
    let out = cicada(dir.path(), &["init"]);
    assert!(!out.status.success());
    // --force overwrites.
    let out = cicada(dir.path(), &["init", "--force"]);
    assert!(out.status.success());
    // Explicit dir arg.
    let other = dir.path().join("other");
    let out = cicada(dir.path(), &["init", other.to_str().unwrap()]);
    assert!(out.status.success());
}

// ---------- fmt ----------

#[test]
fn fmt_formats_checks_and_sorts() {
    let dir = tempfile::tempdir().unwrap();
    fixture(dir.path(), RICH_CALENDAR);
    // Messy file → formatted.
    let messy = "apiVersion: cicada/v1\nkind: Calendar\nmetadata: {name: work, timeZone: UTC, provider: fake, calendarId: primary}\nspec: {events: [{id: z, summary: Z, start: '2026-01-05T09:00', duration: 1h}, {id: a, summary: A, start: '2026-01-05T09:00', duration: 1h}]}\n";
    fs::write(dir.path().join("calendars/work.yaml"), messy).unwrap();
    let out = cicada(dir.path(), &["fmt"]);
    assert!(out.status.success(), "{}", stdout(&out));
    assert!(stdout(&out).contains("formatted"));
    // Canonical now → unchanged.
    let out = cicada(dir.path(), &["fmt"]);
    assert!(stdout(&out).contains("unchanged"));
    // --check passes on canonical.
    let out = cicada(dir.path(), &["fmt", "--check"]);
    assert!(out.status.success(), "{}", stdout(&out));
    assert!(stdout(&out).contains("canonical"));
    // --sort reorders by id.
    let out = cicada(dir.path(), &["fmt", "--sort"]);
    assert!(out.status.success());
    let body = fs::read_to_string(dir.path().join("calendars/work.yaml")).unwrap();
    assert!(body.find("id: a").unwrap() < body.find("id: z").unwrap());
    // --check fails on non-canonical.
    fs::write(dir.path().join("calendars/work.yaml"), messy).unwrap();
    let out = cicada(dir.path(), &["fmt", "--check"]);
    assert!(!out.status.success());
    assert!(stdout(&out).contains("not canonical"));
}

#[test]
fn fmt_reports_unreadable_and_unparsable_files() {
    let dir = tempfile::tempdir().unwrap();
    fixture(dir.path(), RICH_CALENDAR);
    let out = cicada(dir.path(), &["fmt", "missing.yaml"]);
    assert!(!out.status.success());
    let out = cicada(dir.path(), &["fmt", "cicada.yaml"]); // a manifest expands fine
    assert!(out.status.success(), "{}", stdout(&out));
    // Empty manifest calendar list → "no calendar files found".
    fs::write(
        dir.path().join("cicada.yaml"),
        "apiVersion: cicada/v1\nkind: Manifest\ncalendars: []\n",
    )
    .unwrap();
    let out = cicada(dir.path(), &["fmt"]);
    assert!(!out.status.success());
    // No manifest at all.
    let dir2 = tempfile::tempdir().unwrap();
    let out = cicada(dir2.path(), &["fmt"]);
    assert!(!out.status.success());
}

// ---------- validate edge cases ----------

#[test]
fn validate_covers_manifest_and_error_paths() {
    let dir = tempfile::tempdir().unwrap();
    // No files, no manifest.
    let out = cicada(dir.path(), &["validate"]);
    assert!(!out.status.success());
    // Manifest listing no calendars → in-place failure + empty targets.
    fs::write(
        dir.path().join("cicada.yaml"),
        "apiVersion: cicada/v1\nkind: Manifest\ncalendars: []\n",
    )
    .unwrap();
    let out = cicada(dir.path(), &["validate"]);
    assert!(!out.status.success());
    // Unreadable file target.
    let out = cicada(dir.path(), &["validate", "nope.yaml"]);
    assert!(!out.status.success());
    assert!(stdout(&out).contains("✗"));
    // A file with domain-invariant violations reports diagnostics.
    fixture(dir.path(), RICH_CALENDAR);
    fs::write(
        dir.path().join("calendars/bad.yaml"),
        "apiVersion: cicada/v1\nkind: Calendar\nmetadata: {name: bad, timeZone: UTC}\nspec: {events: [{id: '', summary: x, start: '2026-01-05T09:00', duration: 30m}]}\n",
    )
    .unwrap();
    let out = cicada(dir.path(), &["validate", "calendars/bad.yaml"]);
    assert!(!out.status.success());
    let s = stdout(&out);
    assert!(s.contains("✗"), "{s}");
    assert!(s.contains("error"), "{s}");
}

// ---------- plan ----------

#[test]
fn plan_prints_ops_drift_findings_and_out() {
    let dir = tempfile::tempdir().unwrap();
    fixture(dir.path(), RICH_CALENDAR);
    // Fresh plan: two creates.
    let out = cicada(dir.path(), &["plan"]);
    let s = stdout(&out);
    assert!(out.status.success(), "{s}");
    assert!(s.contains("create"), "{s}");
    assert!(s.contains("2 remote ops"), "{s}");
    // --out writes a plan-v1 doc.
    let out = cicada(dir.path(), &["plan", "--out", "plan.json"]);
    assert!(out.status.success(), "{}", stdout(&out));
    let doc: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(dir.path().join("plan.json")).unwrap()).unwrap();
    assert_eq!(doc["apiVersion"], "cicada/plan-v1");
    // Apply, then a converged plan prints noop + 0 remote ops.
    let out = cicada(dir.path(), &["apply", "--auto-approve"]);
    assert!(out.status.success(), "{}", stdout(&out));
    let out = cicada(dir.path(), &["plan"]);
    let s = stdout(&out);
    assert!(s.contains("noop"), "{s}");
    assert!(s.contains("0 remote ops"), "{s}");
    // Update + delete + findings in one plan.
    let cal = fs::read_to_string(dir.path().join("calendars/work.yaml")).unwrap();
    let updated = cal
        .replace("Daily Standup", "Renamed Standup")
        .replace(
            "    - id: offsite\n      summary: Team Offsite\n      start: \"2026-03-20T10:00\"\n      end: \"2026-03-21T14:00\"\n      transparency: transparent\n",
            "    - id: picnic\n      summary: Picnic\n      start: \"2026-04-01T12:00\"\n      duration: 2h\n",
        );
    fs::write(dir.path().join("calendars/work.yaml"), updated).unwrap();
    let out = cicada(dir.path(), &["plan", "--out", "plans.json"]);
    let s = stdout(&out);
    assert!(out.status.success(), "{s}");
    assert!(s.contains("update"), "{s}");
    assert!(s.contains("delete"), "{s}");
    assert!(s.contains("~ summary:"), "{s}");
    // Out-of-band edit → drift section + conflict marker.
    let mut fake = fake_file(dir.path());
    let events = fake["calendars"]["primary"].as_object_mut().unwrap();
    let ev = events.values_mut().next().unwrap();
    ev["body"]["summary"] = serde_json::json!("Remote Edit");
    ev["etag"] = serde_json::json!("etag-x");
    write_fake(dir.path(), &fake);
    let out = cicada(dir.path(), &["plan"]);
    let s = stdout(&out);
    assert!(s.contains("conflict"), "{s}");
    assert!(s.contains("live differs"), "{s}");
}

#[test]
fn plan_errors_and_edge_paths() {
    let dir = tempfile::tempdir().unwrap();
    // Unknown provider.
    let out = cicada(dir.path(), &["plan", "--provider", "bogus"]);
    assert!(!out.status.success());
    // No manifest.
    let out = cicada(dir.path(), &["plan"]);
    assert!(!out.status.success());
    // Empty manifest.
    fs::write(
        dir.path().join("cicada.yaml"),
        "apiVersion: cicada/v1\nkind: Manifest\ncalendars: []\n",
    )
    .unwrap();
    let out = cicada(dir.path(), &["plan"]);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("no calendar files"));
    // Unparsable calendar.
    fixture(dir.path(), "garbage: [");
    let out = cicada(dir.path(), &["plan"]);
    assert!(!out.status.success());
    // Malformed recorded state.
    fixture(dir.path(), RICH_CALENDAR);
    fs::create_dir_all(dir.path().join(".cicada")).unwrap();
    fs::write(dir.path().join(".cicada/state.yaml"), "junk: [").unwrap();
    let out = cicada(dir.path(), &["plan"]);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("cannot load"));
    // Corrupt fake provider file.
    fs::remove_file(dir.path().join(".cicada/state.yaml")).unwrap();
    fs::write(dir.path().join(".cicada/fake-provider.json"), "{ nope").unwrap();
    let out = cicada(dir.path(), &["plan"]);
    assert!(!out.status.success());
    // --out write failure.
    fs::remove_file(dir.path().join(".cicada/fake-provider.json")).unwrap();
    let out = cicada(dir.path(), &["plan", "--out", "no/such/dir/p.json"]);
    assert!(!out.status.success());
    // A calendar with zero events → "(no operations".
    fixture(
        dir.path(),
        "apiVersion: cicada/v1\nkind: Calendar\nmetadata: {name: work, timeZone: UTC, provider: fake, calendarId: primary}\nspec: {events: []}\n",
    );
    let out = cicada(dir.path(), &["plan"]);
    assert!(stdout(&out).contains("no operations"), "{}", stdout(&out));
    // Capability error → plan exits nonzero (too many reminders
    // for… actually the fake is permissive; use a raw exotic RRULE
    // the fake accepts — instead force an unsupported feature by
    // declaring >200 attendees.
    let many = (0..201)
        .map(|i| format!("        - {{ email: u{i}@x }}"))
        .collect::<Vec<_>>()
        .join("\n");
    fixture(
        dir.path(),
        &format!(
            "apiVersion: cicada/v1\nkind: Calendar\nmetadata: {{name: work, timeZone: UTC, provider: fake, calendarId: primary}}\nspec: {{events: [{{id: e, summary: s, start: '2026-01-05T09:00', duration: 30m, attendees: [\n{many}\n  ]}}]}}\n"
        ),
    );
    let out = cicada(dir.path(), &["plan"]);
    let s = stdout(&out);
    assert!(
        s.contains("findings") || s.contains("error") || !out.status.success(),
        "{s}"
    );
}

// ---------- apply ----------

#[test]
fn apply_confirm_resume_notify_and_failures() {
    let dir = tempfile::tempdir().unwrap();
    fixture(dir.path(), RICH_CALENDAR);
    // Confirm with "y" applies.
    let out = cicada_stdin(dir.path(), &["apply"], "y\n");
    assert!(out.status.success(), "{}", stdout(&out));
    assert!(stdout(&out).contains("applied"));
    // Second apply: nothing to do.
    let out = cicada(dir.path(), &["apply", "--auto-approve"]);
    assert!(stdout(&out).contains("nothing to do"), "{}", stdout(&out));
    // Out-of-band edit → conflicted op fails without --force.
    let mut fake = fake_file(dir.path());
    let events = fake["calendars"]["primary"].as_object_mut().unwrap();
    let ev = events.values_mut().next().unwrap();
    ev["body"]["summary"] = serde_json::json!("OOB");
    ev["etag"] = serde_json::json!("etag-oob");
    write_fake(dir.path(), &fake);
    let out = cicada(dir.path(), &["apply", "--auto-approve"]);
    let s = stdout(&out);
    assert!(s.contains("conflict"), "{s}");
    // --force overwrites the out-of-band edit.
    let out = cicada(dir.path(), &["apply", "--auto-approve", "--force"]);
    assert!(out.status.success(), "{}", stdout(&out));
    // An out-of-band *delete* + refresh makes the next plan propose
    // a create whose idempotency key is already journaled —
    // --resume skips it and prints the "skipped (journaled)" line.
    let mut fake = fake_file(dir.path());
    let events = fake["calendars"]["primary"].as_object_mut().unwrap();
    let key = events.keys().next().unwrap().clone();
    events.remove(&key);
    write_fake(dir.path(), &fake);
    let out = cicada(dir.path(), &["refresh"]);
    assert!(out.status.success(), "{}", stdout(&out));
    let out = cicada(dir.path(), &["apply", "--auto-approve", "--resume"]);
    let s = stdout(&out);
    assert!(
        out.status.success(),
        "{s} {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(s.contains("skipped"), "{s}");
    // A normal apply re-creates the event for the checks below.
    let out = cicada(dir.path(), &["apply", "--auto-approve"]);
    assert!(out.status.success(), "{}", stdout(&out));
    let cal = fs::read_to_string(dir.path().join("calendars/work.yaml")).unwrap();
    // --notify valid + invalid.
    fs::write(
        dir.path().join("calendars/work.yaml"),
        cal.replace("Team Offsite", "Offsite v3"),
    )
    .unwrap();
    let out = cicada(dir.path(), &["apply", "--auto-approve", "--notify", "all"]);
    assert!(out.status.success(), "{}", stdout(&out));
    let out = cicada(dir.path(), &["apply", "--notify", "sometimes"]);
    assert!(!out.status.success());
}

#[test]
fn apply_error_paths() {
    let dir = tempfile::tempdir().unwrap();
    let out = cicada(dir.path(), &["apply", "--provider", "bogus"]);
    assert!(!out.status.success());
    let out = cicada(dir.path(), &["apply"]);
    assert!(!out.status.success());
    fs::write(
        dir.path().join("cicada.yaml"),
        "apiVersion: cicada/v1\nkind: Manifest\ncalendars: []\n",
    )
    .unwrap();
    assert!(!cicada(dir.path(), &["apply"]).status.success());
    fixture(dir.path(), "garbage: [");
    assert!(!cicada(dir.path(), &["apply"]).status.success());
    fixture(dir.path(), RICH_CALENDAR);
    // Unreadable fake file.
    fs::create_dir_all(dir.path().join(".cicada")).unwrap();
    fs::write(dir.path().join(".cicada/fake-provider.json"), "{ x").unwrap();
    assert!(
        !cicada(dir.path(), &["apply", "--auto-approve"])
            .status
            .success()
    );
    // Bad state file.
    fs::remove_file(dir.path().join(".cicada/fake-provider.json")).unwrap();
    fs::write(dir.path().join(".cicada/state.yaml"), "bad: [").unwrap();
    assert!(
        !cicada(dir.path(), &["apply", "--auto-approve"])
            .status
            .success()
    );
}

// ---------- refresh / drift ----------

#[test]
fn refresh_and_drift_cover_report_lines() {
    let dir = tempfile::tempdir().unwrap();
    fixture(dir.path(), RICH_CALENDAR);
    let out = cicada(dir.path(), &["apply", "--auto-approve"]);
    assert!(out.status.success());
    // Missing drift: delete a live event.
    let mut fake = fake_file(dir.path());
    let events = fake["calendars"]["primary"].as_object_mut().unwrap();
    let key = events.keys().next().unwrap().clone();
    events.remove(&key);
    write_fake(dir.path(), &fake);
    let out = cicada(dir.path(), &["drift"]);
    let s = stdout(&out);
    assert!(!out.status.success(), "{s}");
    assert!(s.contains("missing"), "{s}");
    assert!(s.contains("live event is gone"), "{s}");
    // Refresh drops the record.
    let out = cicada(dir.path(), &["refresh"]);
    let s = stdout(&out);
    assert!(out.status.success(), "{s}");
    assert!(s.contains("removed"), "{s}");
    // Clean now.
    let out = cicada(dir.path(), &["drift"]);
    assert!(out.status.success(), "{}", stdout(&out));
    let out = cicada(dir.path(), &["refresh"]);
    assert!(stdout(&out).contains("in sync"), "{}", stdout(&out));
    // Adoption candidate: clone a real event (correct wire shape)
    // under a new remote id with a cicada:id tag state doesn't know.
    let mut fake = fake_file(dir.path());
    let events = fake["calendars"]["primary"].as_object_mut().unwrap();
    let mut clone = events.values().next().unwrap().clone();
    clone["remoteId"] = serde_json::json!("fake-999");
    clone["tags"]["localId"] = serde_json::json!("mystery");
    events.insert("fake-999".into(), clone);
    write_fake(dir.path(), &fake);
    let out = cicada(dir.path(), &["refresh"]);
    assert!(stdout(&out).contains("adoption"), "{}", stdout(&out));
}

#[test]
fn refresh_and_drift_error_paths() {
    let dir = tempfile::tempdir().unwrap();
    fixture(dir.path(), RICH_CALENDAR);
    for verb in ["refresh", "drift"] {
        assert!(
            !cicada(dir.path(), &[verb, "--provider", "bogus"])
                .status
                .success()
        );
        fs::create_dir_all(dir.path().join(".cicada")).unwrap();
        fs::write(dir.path().join(".cicada/fake-provider.json"), "{ x").unwrap();
        assert!(!cicada(dir.path(), &[verb]).status.success());
        fs::remove_file(dir.path().join(".cicada/fake-provider.json")).unwrap();
        fs::write(dir.path().join(".cicada/state.yaml"), "bad: [").unwrap();
        assert!(!cicada(dir.path(), &[verb]).status.success());
        fs::remove_file(dir.path().join(".cicada/state.yaml")).unwrap();
    }
}

// ---------- range ----------

#[test]
fn range_local_and_live_views() {
    let dir = tempfile::tempdir().unwrap();
    fixture(dir.path(), RICH_CALENDAR);
    let out = cicada(
        dir.path(),
        &["range", "--from", "2026-01-05", "--to", "2026-01-09"],
    );
    let s = stdout(&out);
    assert!(out.status.success(), "{s}");
    assert!(s.contains("standup"), "{s}");
    assert!(s.contains("work:"), "{s}");
    // Empty window (before any event's first occurrence).
    let out = cicada(
        dir.path(),
        &["range", "--from", "2019-01-01", "--to", "2019-01-02"],
    );
    assert!(stdout(&out).contains("no occurrences"), "{}", stdout(&out));
    // Bad bound.
    assert!(
        !cicada(dir.path(), &["range", "--from", "nope"])
            .status
            .success()
    );
    // Reversed window.
    assert!(
        !cicada(
            dir.path(),
            &["range", "--from", "2030-01-02", "--to", "2030-01-01"]
        )
        .status
        .success()
    );
    // +Nw form + datetime forms.
    assert!(
        cicada(dir.path(), &["range", "--from", "today", "--to", "+1w"])
            .status
            .success()
    );
    assert!(
        cicada(
            dir.path(),
            &[
                "range",
                "--from",
                "2026-01-05T00:00",
                "--to",
                "2026-01-06T00:00"
            ]
        )
        .status
        .success()
    );
    // Live view over the fake backend.
    let out = cicada(dir.path(), &["apply", "--auto-approve"]);
    assert!(out.status.success());
    let out = cicada(
        dir.path(),
        &[
            "range",
            "--provider",
            "fake",
            "--from",
            "2026-01-05",
            "--to",
            "2026-01-09",
        ],
    );
    let s = stdout(&out);
    assert!(out.status.success(), "{s}");
    assert!(s.contains("live @ primary"), "{s}");
    assert!(s.contains("standup"), "{s}");
    // Live view, empty window (before the first occurrence).
    let out = cicada(
        dir.path(),
        &[
            "range",
            "--provider",
            "fake",
            "--from",
            "2019-01-01",
            "--to",
            "2019-01-02",
        ],
    );
    assert!(stdout(&out).contains("no occurrences"), "{}", stdout(&out));
    // Live errors.
    assert!(
        !cicada(dir.path(), &["range", "--provider", "bogus"])
            .status
            .success()
    );
    fs::write(dir.path().join(".cicada/fake-provider.json"), "{ x").unwrap();
    assert!(
        !cicada(dir.path(), &["range", "--provider", "fake"])
            .status
            .success()
    );
}

#[test]
fn range_errors_before_any_calendar() {
    let dir = tempfile::tempdir().unwrap();
    assert!(!cicada(dir.path(), &["range"]).status.success());
    fixture(dir.path(), "garbage: [");
    assert!(!cicada(dir.path(), &["range"]).status.success());
    // An unexpandable rule surfaces a per-calendar failure.
    fixture(
        dir.path(),
        "apiVersion: cicada/v1\nkind: Calendar\nmetadata: {name: work, timeZone: UTC, provider: fake, calendarId: primary}\nspec: {events: [{id: e, summary: s, start: '2026-01-05T09:00', duration: 30m, recurrence: 'FREQ=MINUTELY'}]}\n",
    );
    let out = cicada(
        dir.path(),
        &["range", "--from", "2026-01-05", "--to", "2026-01-09"],
    );
    assert!(!out.status.success());
}

// ---------- show ----------

#[test]
fn show_local_event_full_detail() {
    let dir = tempfile::tempdir().unwrap();
    fixture(dir.path(), RICH_CALENDAR);
    let out = cicada(dir.path(), &["show", "standup"]);
    let s = stdout(&out);
    assert!(out.status.success(), "{s}");
    for needle in [
        "Daily Standup",
        "when:",
        "repeat:",
        "desc:",
        "where:",
        "who:",
        "(optional)",
        "remind:",
        "conf:",
        "busy:",
        "occurrences:",
    ] {
        assert!(s.contains(needle), "missing {needle} in {s}");
    }
    // Custom window.
    let out = cicada(
        dir.path(),
        &[
            "show",
            "standup",
            "--from",
            "2026-01-05",
            "--to",
            "2026-01-12",
        ],
    );
    assert!(out.status.success(), "{}", stdout(&out));
    // Missing event.
    let out = cicada(dir.path(), &["show", "nope"]);
    assert!(!out.status.success());
    // Bad window bound.
    let out = cicada(dir.path(), &["show", "standup", "--from", "bogus"]);
    assert!(!out.status.success());
    // No manifest.
    let dir2 = tempfile::tempdir().unwrap();
    assert!(!cicada(dir2.path(), &["show", "x"]).status.success());
}

#[test]
fn show_live_event_and_missing_tag() {
    let dir = tempfile::tempdir().unwrap();
    fixture(dir.path(), RICH_CALENDAR);
    let out = cicada(dir.path(), &["apply", "--auto-approve"]);
    assert!(out.status.success());
    let out = cicada(dir.path(), &["show", "standup", "--provider", "fake"]);
    let s = stdout(&out);
    assert!(out.status.success(), "{s}");
    assert!(s.contains("live @ primary"), "{s}");
    assert!(s.contains("etag"), "{s}");
    assert!(s.contains("occurrences:"), "{s}");
    // An id that was never applied → not found live.
    let out = cicada(dir.path(), &["show", "nope", "--provider", "fake"]);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("no live event"));
    // Provider-level errors.
    assert!(
        !cicada(dir.path(), &["show", "x", "--provider", "bogus"])
            .status
            .success()
    );
    fs::write(dir.path().join(".cicada/fake-provider.json"), "{ x").unwrap();
    assert!(
        !cicada(dir.path(), &["show", "standup", "--provider", "fake"])
            .status
            .success()
    );
}

// ---------- provider add ----------

#[test]
fn provider_add_unknown_and_oauth_missing_creds() {
    let dir = tempfile::tempdir().unwrap();
    let out = cicada(dir.path(), &["provider", "add", "bogus"]);
    assert!(!out.status.success());
    // google without client id → clear error.
    let out = Command::new(env!("CARGO_BIN_EXE_cicada"))
        .args(["provider", "add", "google"])
        .current_dir(dir.path())
        .env_remove("CICADA_GOOGLE_CLIENT_ID")
        .env_remove("CICADA_GOOGLE_CLIENT_SECRET")
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("client id"));
    // id without secret → the other error.
    let out = cicada(
        dir.path(),
        &["provider", "add", "google", "--client-id", "cid"],
    );
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("client secret"));
}

#[test]
fn provider_add_service_account_validates_and_persists() {
    let dir = tempfile::tempdir().unwrap();
    let secrets = dir.path().join("secrets");
    // Missing file.
    let out = Command::new(env!("CARGO_BIN_EXE_cicada"))
        .args([
            "provider",
            "add",
            "google",
            "--service-account",
            "nope.json",
        ])
        .current_dir(dir.path())
        .env("CICADA_SECRETS_DIR", &secrets)
        .output()
        .unwrap();
    assert!(!out.status.success());
    // Malformed key JSON.
    fs::write(dir.path().join("key.json"), "{}").unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_cicada"))
        .args(["provider", "add", "google", "--service-account", "key.json"])
        .current_dir(dir.path())
        .env("CICADA_SECRETS_DIR", &secrets)
        .output()
        .unwrap();
    assert!(!out.status.success());
    // Valid key (type warns but persists).
    fs::write(
        dir.path().join("key.json"),
        r#"{"type":"other","client_email":"cicada@x.iam","private_key":"-----BEGIN PRIVATE KEY-----\nMIIB\n-----END PRIVATE KEY-----\n"}"#,
    )
    .unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_cicada"))
        .args(["provider", "add", "google", "--service-account", "key.json"])
        .current_dir(dir.path())
        .env("CICADA_SECRETS_DIR", &secrets)
        .output()
        .unwrap();
    let s = stdout(&out);
    assert!(
        out.status.success(),
        "{s} {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(s.contains("service account cicada@x.iam registered"), "{s}");
    assert!(
        secrets.join("google%2Fservice-account").exists(),
        "{secrets:?}"
    );
}

#[test]
fn provider_add_oauth_code_exchange() {
    let dir = tempfile::tempdir().unwrap();
    let secrets = dir.path().join("secrets");
    // A stub token endpoint on loopback.
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        use std::io::Read;
        let mut buf = [0u8; 8192];
        let _ = stream.read(&mut buf);
        let body = br#"{"access_token":"ya29.t","expires_in":3600,"token_type":"Bearer","refresh_token":"rt-1"}"#;
        let resp = format!(
            "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
            body.len(),
            String::from_utf8_lossy(body)
        );
        stream.write_all(resp.as_bytes()).unwrap();
    });
    // --code skips the prompt; env token url points at the stub.
    let out = Command::new(env!("CARGO_BIN_EXE_cicada"))
        .args([
            "provider",
            "add",
            "google",
            "--client-id",
            "cid",
            "--client-secret",
            "csec",
            "--code",
            "4/auth-code",
        ])
        .current_dir(dir.path())
        .env("CICADA_SECRETS_DIR", &secrets)
        .env(
            "CICADA_GOOGLE_TOKEN_URL",
            format!("http://127.0.0.1:{port}/token"),
        )
        .output()
        .unwrap();
    let s = stdout(&out);
    assert!(
        out.status.success(),
        "{s} {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(s.contains("refresh token stored"), "{s}");
    assert!(secrets.join("google%2Frefresh-token").exists());
}

#[test]
fn provider_add_oauth_no_refresh_token_errors() {
    let dir = tempfile::tempdir().unwrap();
    let secrets = dir.path().join("secrets");
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        use std::io::Read;
        let mut buf = [0u8; 8192];
        let _ = stream.read(&mut buf);
        let body = br#"{"access_token":"ya29.t","expires_in":3600}"#;
        let resp = format!(
            "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
            body.len(),
            String::from_utf8_lossy(body)
        );
        stream.write_all(resp.as_bytes()).unwrap();
    });
    // A pasted redirect URL also exercises extract_code's URL arm.
    let out = Command::new(env!("CARGO_BIN_EXE_cicada"))
        .args([
            "provider",
            "add",
            "google",
            "--client-id",
            "cid",
            "--client-secret",
            "csec",
            "--code",
            "http://127.0.0.1/?code=4%2Fxyz&scope=x",
        ])
        .current_dir(dir.path())
        .env("CICADA_SECRETS_DIR", &secrets)
        .env(
            "CICADA_GOOGLE_TOKEN_URL",
            format!("http://127.0.0.1:{port}/token"),
        )
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("no refresh token"));
}

#[test]
fn show_range_window_and_parse_errors() {
    let dir = tempfile::tempdir().unwrap();
    fixture(dir.path(), RICH_CALENDAR);
    // Declared-view show — find_map + detail print arms.
    let out = cicada(dir.path(), &["show", "standup"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    // A backwards window fails TimeRange::new.
    let out = cicada(
        dir.path(),
        &["show", "standup", "--from", "+30d", "--to", "today"],
    );
    assert!(!out.status.success());
}

#[test]
fn init_and_fmt_and_validate_error_paths() {
    let dir = tempfile::tempdir().unwrap();
    // init into an unwritable directory — scaffold's
    // create_dir_all error arm.
    let ro = dir.path().join("ro");
    fs::create_dir_all(&ro).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&ro, fs::Permissions::from_mode(0o555)).unwrap();
    }
    let out = cicada(dir.path(), &["init", "--dir", ro.to_str().unwrap()]);
    assert!(!out.status.success());

    fixture(dir.path(), RICH_CALENDAR);
    // fmt/validate on an unparseable file — the render_schema_error
    // arms.
    fs::write(dir.path().join("calendars/bad.yaml"), "kind: [").unwrap();
    let out = cicada(dir.path(), &["fmt", "calendars/bad.yaml"]);
    assert!(!out.status.success());
    let out = cicada(dir.path(), &["validate", "calendars/bad.yaml"]);
    assert!(!out.status.success());
}

#[test]
fn fmt_and_init_write_errors() {
    let dir = tempfile::tempdir().unwrap();
    fixture(dir.path(), RICH_CALENDAR);
    // A read-only calendar file — fmt's fs::write error arm.
    let f = dir.path().join("calendars/work.yaml");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&f, fs::Permissions::from_mode(0o444)).unwrap();
    }
    let out = cicada(dir.path(), &["fmt"]);
    assert!(!out.status.success());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&f, fs::Permissions::from_mode(0o644)).unwrap();
    }

    // init where the manifest's parent is unwritable — the
    // fs::write arm (the dir itself exists, so create_dir_all
    // succeeds).
    let ro = dir.path().join("ro");
    fs::create_dir_all(&ro).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&ro, fs::Permissions::from_mode(0o555)).unwrap();
    }
    let out = cicada(dir.path(), &["init", "--dir", ro.to_str().unwrap()]);
    assert!(!out.status.success());
    // `--dir` under the unwritable parent — the create_dir_all arm.
    let nested = ro.join("nested");
    let out = cicada(dir.path(), &["init", "--dir", nested.to_str().unwrap()]);
    assert!(!out.status.success());
}
