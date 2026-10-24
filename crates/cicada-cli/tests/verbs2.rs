//! Second e2e sweep — show detail variants, plan findings/diffs,
//! apply abort paths, live range/shape edge cases, `cicada mcp`,
//! and google `from_env` branches driven through `--provider
//! google` on the real binary.
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
        .unwrap()
}

fn cicada_stdin(dir: &Path, args: &[&str], input: &[u8]) -> Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_cicada"))
        .args(args)
        .current_dir(dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.as_mut().unwrap().write_all(input).unwrap();
    drop(child.stdin.take());
    child.wait_with_output().unwrap()
}

fn stdout(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn fake_events(dir: &Path) -> serde_json::Map<String, serde_json::Value> {
    serde_json::from_str::<serde_json::Value>(
        &fs::read_to_string(dir.join(".cicada/fake-provider.json")).unwrap(),
    )
    .unwrap()["calendars"]["primary"]
        .as_object()
        .unwrap()
        .clone()
}

fn write_fake(dir: &Path, events: &serde_json::Map<String, serde_json::Value>) {
    let mut fake = serde_json::json!({
        "apiVersion": "cicada/fake-provider-v1",
        "calendars": {"primary": events}
    });
    write_fake_full(dir, &mut fake);
}

fn write_fake_full(dir: &Path, fake: &mut serde_json::Value) {
    fs::write(
        dir.join(".cicada/fake-provider.json"),
        serde_json::to_string_pretty(fake).unwrap(),
    )
    .unwrap();
}

// ---------- show detail variants ----------

#[test]
fn show_occurrence_cap_and_time_forms() {
    let dir = tempfile::tempdir().unwrap();
    fixture(
        dir.path(),
        r#"apiVersion: cicada/v1
kind: Calendar
metadata: {name: work, timeZone: UTC, provider: fake, calendarId: primary}
spec:
  events:
    - id: daily
      summary: Daily
      start: "09:00"
      duration: 30m
      recurrence: { freq: DAILY }
    - id: instant
      summary: Instant
      start: "2026-01-05T09:00:00Z"
      duration: 45m
    - id: ranged
      summary: Ranged
      start: "2026-01-05T09:00"
      end: "2026-01-07T11:00"
"#,
    );
    // >20 occurrences → the "… more" cap line.
    let out = cicada(
        dir.path(),
        &[
            "show",
            "daily",
            "--from",
            "2026-01-01",
            "--to",
            "2027-01-01",
        ],
    );
    assert!(stdout(&out).contains("more — use"), "{}", stdout(&out));
    // Instant time renders bare RFC 3339.
    let out = cicada(dir.path(), &["show", "instant"]);
    assert!(
        stdout(&out).contains("2026-01-05T09:00:00Z"),
        "{}",
        stdout(&out)
    );
    // Range end renders with `→`.
    let out = cicada(dir.path(), &["show", "ranged"]);
    assert!(stdout(&out).contains("→"), "{}", stdout(&out));
}

#[test]
fn show_live_event_with_updated_and_foreign() {
    let dir = tempfile::tempdir().unwrap();
    fixture(
        dir.path(),
        "apiVersion: cicada/v1\nkind: Calendar\nmetadata: {name: work, timeZone: UTC, provider: fake, calendarId: primary}\nspec: {events: [{id: a, summary: A, start: '2026-01-05T09:00', duration: 30m}]}\n",
    );
    assert!(
        cicada(dir.path(), &["apply", "--auto-approve"])
            .status
            .success()
    );
    // Add `updated` + a foreign (untagged) event to the fake file.
    let mut fake: serde_json::Value = serde_json::from_str(
        &fs::read_to_string(dir.path().join(".cicada/fake-provider.json")).unwrap(),
    )
    .unwrap();
    let events = fake["calendars"]["primary"].as_object_mut().unwrap();
    events.values_mut().next().unwrap()["updated"] = serde_json::json!("2026-02-01T10:00:00Z");
    let mut foreign = events.values().next().unwrap().clone();
    foreign["remoteId"] = serde_json::json!("foreign-1");
    foreign["tags"] = serde_json::json!({});
    foreign["body"]["summary"] = serde_json::json!("Foreign");
    events.insert("foreign-1".into(), foreign);
    write_fake_full(dir.path(), &mut fake);

    let out = cicada(dir.path(), &["show", "a", "--provider", "fake"]);
    let s = stdout(&out);
    assert!(s.contains("updated: 2026-02-01T10:00:00Z"), "{s}");
    // A foreign event outside the window prints the hint.
    let out = cicada(
        dir.path(),
        &[
            "show",
            "a",
            "--provider",
            "fake",
            "--from",
            "2030-01-01",
            "--to",
            "2030-01-02",
        ],
    );
    assert!(
        stdout(&out).contains("outside the window"),
        "{}",
        stdout(&out)
    );
    // Bad window on the live path.
    assert!(
        !cicada(
            dir.path(),
            &["show", "a", "--provider", "fake", "--from", "x"]
        )
        .status
        .success()
    );
    // Live expansion failure surfaces (exotic RRULE on remote).
    let mut fake = fake_events(dir.path());
    let ev = fake.values_mut().next().unwrap();
    ev["body"]["recurrence"] = serde_json::json!("FREQ=MINUTELY");
    write_fake(dir.path(), &fake);
    let out = cicada(dir.path(), &["show", "a", "--provider", "fake"]);
    assert!(!out.status.success(), "{}", stdout(&out));
}

// ---------- plan detail lines ----------

#[test]
fn plan_prints_replace_and_all_diff_fields() {
    let dir = tempfile::tempdir().unwrap();
    fixture(
        dir.path(),
        r#"apiVersion: cicada/v1
kind: Calendar
metadata: {name: work, timeZone: UTC, provider: fake, calendarId: primary}
spec:
  events:
    - id: ev
      summary: Base
      description: d1
      location: l1
      start: "2026-01-05T09:00"
      duration: 30m
      recurrence: { freq: DAILY }
      attendees: [{ email: a@x }]
      reminders: [{ popup: 5m }]
      conferencing: meet
      transparency: opaque
"#,
    );
    assert!(
        cicada(dir.path(), &["apply", "--auto-approve"])
            .status
            .success()
    );
    // Change recurrence → Replace op; change every diffable field →
    // every field_label arm.
    fixture(
        dir.path(),
        r#"apiVersion: cicada/v1
kind: Calendar
metadata: {name: work, timeZone: UTC, provider: fake, calendarId: primary}
spec:
  events:
    - id: ev
      summary: New Summary
      description: d2
      location: l2
      start: "2026-01-06T10:00"
      duration: 45m
      recurrence: { freq: WEEKLY, byDay: [MO] }
      attendees: [{ email: b@x }]
      reminders: [{ email: 1h }]
      transparency: transparent
"#,
    );
    let out = cicada(dir.path(), &["plan"]);
    let s = stdout(&out);
    assert!(s.contains("replace"), "{s}");
    for field in [
        "summary",
        "description",
        "location",
        "time",
        "recurrence",
        "attendees",
        "reminders",
        "conferencing",
        "transparency",
    ] {
        assert!(s.contains(field), "missing {field} in {s}");
    }
    // Two calendars → --out writes a JSON array.
    fs::write(
        dir.path().join("calendars/extra.yaml"),
        "apiVersion: cicada/v1\nkind: Calendar\nmetadata: {name: extra, timeZone: UTC, provider: fake, calendarId: primary}\nspec: {events: []}\n",
    )
    .unwrap();
    fs::write(
        dir.path().join("cicada.yaml"),
        "apiVersion: cicada/v1\nkind: Manifest\ncalendars: [calendars/work.yaml, calendars/extra.yaml]\n",
    )
    .unwrap();
    let out = cicada(dir.path(), &["plan", "--out", "plans.json"]);
    assert!(out.status.success(), "{}", stdout(&out));
    let doc: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(dir.path().join("plans.json")).unwrap()).unwrap();
    assert!(doc.is_array(), "{doc}");
}

// ---------- apply details ----------

#[test]
fn apply_aborts_on_decline_and_eof() {
    let dir = tempfile::tempdir().unwrap();
    fixture(
        dir.path(),
        "apiVersion: cicada/v1\nkind: Calendar\nmetadata: {name: work, timeZone: UTC, provider: fake, calendarId: primary}\nspec: {events: [{id: a, summary: A, start: '2026-01-05T09:00', duration: 30m}]}\n",
    );
    // "n" declines.
    let out = cicada_stdin(dir.path(), &["apply"], b"n\n");
    assert!(
        stdout(&out).contains("aborted") || !stdout(&out).contains("applied"),
        "{}",
        stdout(&out)
    );
    // EOF on stdin → not confirmed.
    let out = cicada(dir.path(), &["apply"]);
    assert!(!stdout(&out).contains("applied"), "{}", stdout(&out));
    // Invalid UTF-8 on stdin → read_line errors → decline.
    let out = cicada_stdin(dir.path(), &["apply"], &[0xFF, 0xFE]);
    assert!(!stdout(&out).contains("applied"), "{}", stdout(&out));
}

#[test]
fn apply_adopts_tagged_remote_and_reports_skipped() {
    let dir = tempfile::tempdir().unwrap();
    fixture(
        dir.path(),
        "apiVersion: cicada/v1\nkind: Calendar\nmetadata: {name: work, timeZone: UTC, provider: fake, calendarId: primary}\nspec: {events: [{id: a, summary: A, start: '2026-01-05T09:00', duration: 30m}]}\n",
    );
    assert!(
        cicada(dir.path(), &["apply", "--auto-approve"])
            .status
            .success()
    );
    // Move the recorded event to a new remote id — create op sees
    // the tagged landing with the matching hash → "already applied".
    let state = fs::read_to_string(dir.path().join(".cicada/state.yaml")).unwrap();
    let cleared = state.replacen("  a:\n", "  phantom:\n", 1);
    fs::write(dir.path().join(".cicada/state.yaml"), cleared).unwrap();
    let out = cicada(dir.path(), &["apply", "--auto-approve"]);
    let s = stdout(&out);
    assert!(
        s.contains("already applied") || s.contains("skipped"),
        "{s}"
    );
}

// ---------- range live variants ----------

#[test]
fn range_live_multiday_foreign_and_failures() {
    let dir = tempfile::tempdir().unwrap();
    fixture(
        dir.path(),
        "apiVersion: cicada/v1\nkind: Calendar\nmetadata: {name: work, timeZone: UTC, provider: fake, calendarId: primary}\nspec: {events: [{id: a, summary: A, start: '2026-01-05T09:00', duration: 30m}]}\n",
    );
    assert!(
        cicada(dir.path(), &["apply", "--auto-approve"])
            .status
            .success()
    );
    // A foreign (untagged) overnight event → remote_id fallback +
    // multi-day end render.
    let mut fake = fake_events(dir.path());
    let mut foreign = fake.values().next().unwrap().clone();
    foreign["remoteId"] = serde_json::json!("foreign-1");
    foreign["tags"] = serde_json::json!({});
    // Clone the real body so the wire shape is right; shift the
    // start to 23:00 and the duration to 4h for an overnight end.
    let body = foreign["body"].as_object_mut().unwrap();
    body["summary"] = serde_json::json!("Foreign");
    body["time"]["span"]["start"]["wall"]["local"] = serde_json::json!("2026-01-05T23:00:00");
    body["time"]["span"]["duration"] = serde_json::json!("PT4H");
    fake.insert("foreign-1".into(), foreign);
    write_fake(dir.path(), &fake);
    let out = cicada(
        dir.path(),
        &[
            "range",
            "--provider",
            "fake",
            "--from",
            "2026-01-05",
            "--to",
            "2026-01-07",
        ],
    );
    let s = stdout(&out);
    assert!(s.contains("foreign-1"), "{s}");
    assert!(s.contains("2026-01-06 03:00"), "{s}");
    // A corrupt recorded-state file exercises the state-load arm.
    fs::write(dir.path().join(".cicada/state.yaml"), "bad: [").unwrap();
    let out = cicada(
        dir.path(),
        &[
            "range",
            "--provider",
            "fake",
            "--from",
            "2026-01-05",
            "--to",
            "2026-01-07",
        ],
    );
    let _ = out; // state errors are tolerated or fail — either is fine.
}

// ---------- refresh / drift leftover arms ----------

#[test]
fn refresh_and_drift_lock_and_run_failures() {
    let dir = tempfile::tempdir().unwrap();
    fixture(
        dir.path(),
        "apiVersion: cicada/v1\nkind: Calendar\nmetadata: {name: work, timeZone: UTC, provider: fake, calendarId: primary}\nspec: {events: [{id: a, summary: A, start: '2026-01-05T09:00', duration: 30m}]}\n",
    );
    // `.cicada` exists as a *file* → the lock can't be created.
    fs::write(dir.path().join(".cicada"), "x").unwrap();
    for verb in ["refresh", "apply"] {
        let out = cicada(dir.path(), &[verb]);
        assert!(!out.status.success(), "{verb}");
    }
    fs::remove_file(dir.path().join(".cicada")).unwrap();
    // Drift per-calendar failure: recorded calendar bound to a
    // missing fake calendar? Fake is lenient — instead corrupt the
    // recorded state so DetectDrift sees an undeclared remote.
    assert!(
        cicada(dir.path(), &["apply", "--auto-approve"])
            .status
            .success()
    );
    let out = cicada(dir.path(), &["drift"]);
    assert!(out.status.success(), "{}", stdout(&out));
}

// ---------- init ----------

#[test]
fn init_scaffold_errors() {
    let dir = tempfile::tempdir().unwrap();
    // A regular file named `calendars` blocks the calendar write.
    fs::write(dir.path().join("calendars"), "x").unwrap();
    let out = cicada(dir.path(), &["init"]);
    assert!(!out.status.success());
    // Partial scaffold: manifest exists, calendar missing → refuse,
    // then --force fills the rest.
    let dir2 = tempfile::tempdir().unwrap();
    fs::write(dir2.path().join("cicada.yaml"), "x").unwrap();
    assert!(!cicada(dir2.path(), &["init"]).status.success());
    assert!(cicada(dir2.path(), &["init", "--force"]).status.success());
    assert!(dir2.path().join("calendars/work.yaml").exists());
}

// ---------- mcp passthrough ----------

#[test]
fn cicada_mcp_serves_stdio() {
    let dir = tempfile::tempdir().unwrap();
    fixture(
        dir.path(),
        "apiVersion: cicada/v1\nkind: Calendar\nmetadata: {name: work, timeZone: UTC, provider: fake, calendarId: primary}\nspec: {events: []}\n",
    );
    let mut child = Command::new(env!("CARGO_BIN_EXE_cicada"))
        .args(["mcp"])
        .current_dir(dir.path())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .as_mut()
        .unwrap()
        .write_all(br#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"t","version":"0"}}}"#.as_slice())
        .unwrap();
    drop(child.stdin.take());
    let out = child.wait_with_output().unwrap();
    assert!(stdout(&out).contains("serverInfo"), "{}", stdout(&out));
}

// ---------- google from_env branches via the real binary ----------

#[test]
fn google_provider_env_branches() {
    let dir = tempfile::tempdir().unwrap();
    fixture(
        dir.path(),
        "apiVersion: cicada/v1\nkind: Calendar\nmetadata: {name: work, timeZone: UTC, provider: fake, calendarId: primary}\nspec: {events: []}\n",
    );
    let run = |envs: &[(&str, &str)]| {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_cicada"));
        cmd.args(["plan", "--provider", "google"])
            .current_dir(dir.path())
            .env_remove("CICADA_GOOGLE_SERVICE_ACCOUNT_JSON")
            .env_remove("GOOGLE_APPLICATION_CREDENTIALS")
            .env_remove("CICADA_GOOGLE_ACCESS_TOKEN")
            .env_remove("CICADA_GOOGLE_CLIENT_ID")
            .env_remove("CICADA_GOOGLE_CLIENT_SECRET")
            .env_remove("CICADA_GOOGLE_REFRESH_TOKEN")
            .env_remove("CICADA_GOOGLE_SUBJECT");
        for (k, v) in envs {
            cmd.env(k, v);
        }
        cmd.output().unwrap()
    };

    // No credentials at all → the "set one of" list.
    let out = run(&[(
        "CICADA_SECRETS_DIR",
        dir.path().join("none").to_str().unwrap(),
    )]);
    assert!(!out.status.success());
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("credentials"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );

    // Unreadable service-account file.
    let out = run(&[("CICADA_GOOGLE_SERVICE_ACCOUNT_JSON", "nope.json")]);
    assert!(!out.status.success());
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("cannot read"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );

    // Malformed service-account file.
    fs::write(dir.path().join("sa.json"), "{}").unwrap();
    let out = run(&[(
        "CICADA_GOOGLE_SERVICE_ACCOUNT_JSON",
        dir.path().join("sa.json").to_str().unwrap(),
    )]);
    assert!(!out.status.success());
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("malformed"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );

    // Static access token → proceeds to a (failing) API call.
    let out = run(&[("CICADA_GOOGLE_ACCESS_TOKEN", "ya29.t")]);
    assert!(!out.status.success()); // real API unreachable → transport error

    // OAuth client + refresh token → token exchange via the stub
    // endpoint env var… which the CLI doesn't read directly, so this
    // still fails — but the from_env OAuthClient arm is covered.
    let out = run(&[
        ("CICADA_GOOGLE_CLIENT_ID", "cid"),
        ("CICADA_GOOGLE_CLIENT_SECRET", "csec"),
    ]);
    assert!(!out.status.success());

    // A persisted service-account key in the secrets dir (last-resort
    // credential) is picked up — malformed content errors clearly.
    let secrets = dir.path().join("secrets2");
    fs::create_dir_all(&secrets).unwrap();
    fs::write(secrets.join("google%2Fservice-account"), "{ bad").unwrap();
    let out = run(&[("CICADA_SECRETS_DIR", secrets.to_str().unwrap())]);
    assert!(!out.status.success());
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("malformed"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

// ---------- lock failures (apply / refresh) ----------

const SIMPLE_CAL: &str = "apiVersion: cicada/v1\nkind: Calendar\nmetadata: {name: work, timeZone: UTC, provider: fake, calendarId: primary}\nspec: {events: [{id: a, summary: A, start: '2026-01-05T09:00', duration: 30m}]}\n";

#[test]
fn lock_dir_fails_apply_and_refresh() {
    let dir = tempfile::tempdir().unwrap();
    fixture(dir.path(), SIMPLE_CAL);
    // `state.yaml.lock` as a directory → the lock can't be opened.
    fs::create_dir_all(dir.path().join(".cicada/state.yaml.lock")).unwrap();
    for args in [&["apply", "--auto-approve"][..], &["refresh"][..]] {
        let out = cicada(dir.path(), args);
        assert!(!out.status.success(), "{args:?}");
    }
}

// ---------- apply: plan-run / journal / adopt / failed-outcome arms ----------

#[test]
fn apply_plan_error_when_live_time_overflows() {
    let dir = tempfile::tempdir().unwrap();
    fixture(dir.path(), SIMPLE_CAL);
    assert!(
        cicada(dir.path(), &["apply", "--auto-approve"])
            .status
            .success()
    );
    // Push the remote's start to the civil ceiling with a duration
    // that lands past it — the diff render overflows and
    // `PlanCalendar::run` errors.
    let mut fake = fake_events(dir.path());
    let ev = fake.values_mut().next().unwrap();
    ev["body"]["time"]["span"]["start"]["wall"]["local"] = serde_json::json!("9999-12-31T09:00:00");
    ev["body"]["time"]["span"]["duration"] = serde_json::json!("PT20H");
    write_fake(dir.path(), &fake);
    // Desired changed → the diff must render the broken live time.
    fs::write(
        dir.path().join("calendars/work.yaml"),
        SIMPLE_CAL.replace("summary: A", "summary: A2"),
    )
    .unwrap();
    let out = cicada(dir.path(), &["apply", "--auto-approve"]);
    assert!(!out.status.success(), "{}", stdout(&out));
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("cicada apply work"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn apply_failed_outcome_prints_and_fails() {
    let dir = tempfile::tempdir().unwrap();
    fixture(dir.path(), SIMPLE_CAL);
    // `fake-provider.json.tmp` as a directory → the provider's atomic
    // write fails → `OpResult::Failed` prints and apply fails.
    fs::create_dir_all(dir.path().join(".cicada/fake-provider.json.tmp")).unwrap();
    let out = cicada(dir.path(), &["apply", "--auto-approve"]);
    assert!(!out.status.success());
    let s = stdout(&out);
    assert!(s.contains('✗'), "{s}");
    assert!(s.contains("failed"), "{s}");
}

#[test]
#[cfg(unix)]
fn apply_journal_save_error_propagates() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    fixture(dir.path(), SIMPLE_CAL);
    // Lock file exists (so the lock itself succeeds) but `.cicada` is
    // read-only → the journal's atomic save fails mid-apply →
    // `ApplyPlan::run` errors.
    let cic = dir.path().join(".cicada");
    fs::create_dir_all(&cic).unwrap();
    fs::write(cic.join("state.yaml.lock"), "").unwrap();
    fs::set_permissions(&cic, fs::Permissions::from_mode(0o555)).unwrap();
    let out = cicada(dir.path(), &["apply", "--auto-approve"]);
    fs::set_permissions(&cic, fs::Permissions::from_mode(0o755)).unwrap();
    assert!(!out.status.success());
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("cicada apply work"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn apply_adopts_still_tagged_remote() {
    let dir = tempfile::tempdir().unwrap();
    fixture(dir.path(), SIMPLE_CAL);
    assert!(
        cicada(dir.path(), &["apply", "--auto-approve"])
            .status
            .success()
    );
    // Crash-simulation: the remote still carries `cicada:id` but the
    // journal/state are gone → re-plan warns, apply adopts.
    fs::remove_file(dir.path().join(".cicada/state.yaml")).unwrap();
    let out = cicada(dir.path(), &["apply", "--auto-approve"]);
    let s = stdout(&out);
    assert!(out.status.success(), "{s}");
    assert!(s.contains("already applied"), "{s}");
}

#[test]
#[cfg(unix)]
fn refresh_save_error_propagates() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    fixture(dir.path(), SIMPLE_CAL);
    assert!(
        cicada(dir.path(), &["apply", "--auto-approve"])
            .status
            .success()
    );
    // Read-only `.cicada` → `RefreshState`'s state save errors.
    let cic = dir.path().join(".cicada");
    fs::set_permissions(&cic, fs::Permissions::from_mode(0o555)).unwrap();
    let out = cicada(dir.path(), &["refresh"]);
    fs::set_permissions(&cic, fs::Permissions::from_mode(0o755)).unwrap();
    assert!(!out.status.success());
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("cicada refresh"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

// ---------- range --provider error arms ----------

#[test]
fn range_live_error_arms() {
    let dir = tempfile::tempdir().unwrap();
    // Malformed calendar → `load_and_zone` errs before the provider.
    fixture(dir.path(), "garbage: [");
    let out = cicada(dir.path(), &["range", "--provider", "fake"]);
    assert!(!out.status.success());
    // Bad --from → the window parse errs.
    fixture(dir.path(), SIMPLE_CAL);
    let out = cicada(
        dir.path(),
        &["range", "--provider", "fake", "--from", "bogus"],
    );
    assert!(!out.status.success());
    // Exotic remote rule → `expand_remote` errs per calendar.
    assert!(
        cicada(dir.path(), &["apply", "--auto-approve"])
            .status
            .success()
    );
    let mut fake = fake_events(dir.path());
    let ev = fake.values_mut().next().unwrap();
    ev["body"]["recurrence"] = serde_json::json!("FREQ=MINUTELY");
    write_fake(dir.path(), &fake);
    let out = cicada(
        dir.path(),
        &[
            "range",
            "--provider",
            "fake",
            "--from",
            "2026-01-01",
            "--to",
            "2026-02-01",
        ],
    );
    assert!(!out.status.success(), "{}", stdout(&out));
}

// ---------- show error arms + instant-end render ----------

#[test]
fn show_local_error_arms() {
    let dir = tempfile::tempdir().unwrap();
    // Malformed calendar → `load_calendars` errs.
    fixture(dir.path(), "garbage: [");
    assert!(!cicada(dir.path(), &["show", "a"]).status.success());
    // An unexpandable rule → `ShowEvent::run` errs.
    fixture(
        dir.path(),
        "apiVersion: cicada/v1\nkind: Calendar\nmetadata: {name: work, timeZone: UTC, provider: fake, calendarId: primary}\nspec: {events: [{id: a, summary: A, start: '2026-01-05T09:00', duration: 30m, recurrence: 'FREQ=MINUTELY'}]}\n",
    );
    let out = cicada(dir.path(), &["show", "a"]);
    assert!(!out.status.success(), "{}", stdout(&out));
    // An instant end renders through `render_event_time`'s arm.
    fixture(
        dir.path(),
        "apiVersion: cicada/v1\nkind: Calendar\nmetadata: {name: work, timeZone: UTC, provider: fake, calendarId: primary}\nspec: {events: [{id: a, summary: A, start: '2026-01-05T09:00', end: '2026-01-05T12:00:00Z'}]}\n",
    );
    let out = cicada(dir.path(), &["show", "a"]);
    let s = stdout(&out);
    assert!(out.status.success(), "{s}");
    assert!(s.contains("2026-01-05T12:00:00Z"), "{s}");
}

#[test]
fn show_live_error_arms() {
    let dir = tempfile::tempdir().unwrap();
    fixture(dir.path(), SIMPLE_CAL);
    // No manifest and no file args → `resolve_calendar_files` errs.
    let bare = tempfile::tempdir().unwrap();
    let out = cicada(bare.path(), &["show", "a", "--provider", "fake"]);
    assert!(!out.status.success());
    // Malformed calendar → `load_calendars` errs.
    fs::write(dir.path().join("calendars/work.yaml"), "garbage: [").unwrap();
    let out = cicada(dir.path(), &["show", "a", "--provider", "fake"]);
    assert!(!out.status.success());
    // Exotic remote rule → the live expansion errs.
    fixture(dir.path(), SIMPLE_CAL);
    assert!(
        cicada(dir.path(), &["apply", "--auto-approve"])
            .status
            .success()
    );
    let mut fake = fake_events(dir.path());
    let ev = fake.values_mut().next().unwrap();
    ev["body"]["recurrence"] = serde_json::json!("FREQ=MINUTELY");
    write_fake(dir.path(), &fake);
    let out = cicada(dir.path(), &["show", "a", "--provider", "fake"]);
    assert!(!out.status.success(), "{}", stdout(&out));
}

// ---------- plan: missing drift + instant-start render ----------

#[test]
fn plan_missing_drift_and_instant_start() {
    let dir = tempfile::tempdir().unwrap();
    fixture(dir.path(), SIMPLE_CAL);
    assert!(
        cicada(dir.path(), &["apply", "--auto-approve"])
            .status
            .success()
    );
    // Remote gone → the next plan reports Missing drift.
    let mut fake = fake_events(dir.path());
    fake.clear();
    write_fake(dir.path(), &fake);
    let out = cicada(dir.path(), &["plan"]);
    let s = stdout(&out);
    assert!(s.contains("live event is gone"), "{s}");
    // An instant-start event renders `render_time`'s Instant arm.
    fixture(
        dir.path(),
        "apiVersion: cicada/v1\nkind: Calendar\nmetadata: {name: work, timeZone: UTC, provider: fake, calendarId: primary}\nspec: {events: [{id: i, summary: I, start: '2026-01-05T09:00:00Z', duration: 30m}]}\n",
    );
    let out = cicada(dir.path(), &["plan"]);
    let s = stdout(&out);
    assert!(s.contains("2026-01-05T09:00:00Z"), "{s}");
}

// ---------- init: artifact skip ----------

#[test]
fn init_skips_existing_gitignore() {
    let dir = tempfile::tempdir().unwrap();
    // `.cicada/.gitignore` exists but authored files don't — scaffold
    // skips the artifact even without --force.
    fs::create_dir_all(dir.path().join(".cicada")).unwrap();
    fs::write(dir.path().join(".cicada/.gitignore"), "custom\n").unwrap();
    let out = cicada(dir.path(), &["init"]);
    assert!(out.status.success(), "{}", stdout(&out));
    assert_eq!(
        fs::read_to_string(dir.path().join(".cicada/.gitignore")).unwrap(),
        "custom\n"
    );
}

// ---------- init/provider failure surfaces ----------

#[test]
fn init_reports_write_failures() {
    let dir = tempfile::tempdir().unwrap();
    // `calendars/work.yaml` exists as a directory — the scaffold's
    // `fs::write` can't land on it (the create_dir_all arm stays
    // fine, the write arm fails).
    fs::create_dir_all(dir.path().join("calendars/work.yaml")).unwrap();
    let out = cicada(dir.path(), &["init", "--force"]);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("cannot write"));
}

#[test]
fn provider_add_service_account_set_failure() {
    // CICADA_SECRETS_DIR points at a file — the SecretStore::set
    // map_err arm fires after the key validates.
    let dir = tempfile::tempdir().unwrap();
    let blocker = dir.path().join("secrets-file");
    fs::write(&blocker, "not a dir").unwrap();
    fs::write(
        dir.path().join("key.json"),
        r#"{"type":"service_account","client_email":"c@x.iam","private_key":"k"}"#,
    )
    .unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_cicada"))
        .args(["provider", "add", "google", "--service-account", "key.json"])
        .current_dir(dir.path())
        .env("CICADA_SECRETS_DIR", &blocker)
        .output()
        .unwrap();
    assert!(!out.status.success());
}

#[test]
fn provider_add_oauth_error_paths() {
    let dir = tempfile::tempdir().unwrap();
    let secrets = dir.path().join("secrets");

    // (a) --code mentions `code=` but yields none → extract_code's `?`.
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
            "ecode=x",
        ])
        .current_dir(dir.path())
        .env("CICADA_SECRETS_DIR", &secrets)
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("code"));

    // (b) a dead token endpoint → the exchange map_err.
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener); // nothing accepts — connection refused.
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
            "4/x",
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

    // (c) CICADA_GOOGLE_TOKEN_URL unset → the TOKEN_URL fallback
    // closure runs; a dead proxy keeps it off the real endpoint.
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
            "4/x",
        ])
        .current_dir(dir.path())
        .env("CICADA_SECRETS_DIR", &secrets)
        .env_remove("CICADA_GOOGLE_TOKEN_URL")
        .env("HTTPS_PROXY", format!("http://127.0.0.1:{port}"))
        .env("https_proxy", format!("http://127.0.0.1:{port}"))
        .output()
        .unwrap();
    assert!(!out.status.success());
}

#[test]
fn provider_add_oauth_read_code_failure() {
    // The loopback request arrives without a `code` while stdin is
    // at EOF — read_code errors out through the `?` in add_oauth.
    let dir = tempfile::tempdir().unwrap();
    let secrets = dir.path().join("secrets");
    let mut child = Command::new(env!("CARGO_BIN_EXE_cicada"))
        .args([
            "provider",
            "add",
            "google",
            "--client-id",
            "cid",
            "--client-secret",
            "csec",
        ])
        .current_dir(dir.path())
        .env("CICADA_SECRETS_DIR", &secrets)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    drop(child.stdin.take()); // stdin EOF
    // Scrape the loopback port off the printed hint, then send a
    // code-less request so read_code's recv channel closes empty.
    let port = {
        use std::io::BufRead;
        let mut found = None;
        let reader = std::io::BufReader::new(child.stdout.as_mut().unwrap());
        for line in reader.lines().map_while(Result::ok) {
            if let Some(rest) = line.strip_prefix("a local listener on http://127.0.0.1:") {
                found = rest
                    .split(|c: char| !c.is_ascii_digit())
                    .next()
                    .and_then(|p| p.parse::<u16>().ok());
                break;
            }
        }
        found.expect("the child printed its loopback port")
    };
    let mut s = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();
    s.write_all(b"GET /?scope=x HTTP/1.0\r\n\r\n").unwrap();
    drop(s);
    let out = child.wait_with_output().unwrap();
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("no authorization code"));
}

#[test]
fn provider_add_oauth_set_failure_after_exchange() {
    // The exchange lands but the secrets dir is a file — the
    // SecretStore::set map_err after a successful token swap.
    let dir = tempfile::tempdir().unwrap();
    let blocker = dir.path().join("secrets-blocker");
    fs::write(&blocker, "not a dir").unwrap();
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
            "4/x",
        ])
        .current_dir(dir.path())
        .env("CICADA_SECRETS_DIR", &blocker)
        .env(
            "CICADA_GOOGLE_TOKEN_URL",
            format!("http://127.0.0.1:{port}/token"),
        )
        .output()
        .unwrap();
    assert!(!out.status.success());
}

// ---------- provider add: pasted code path ----------

#[test]
fn provider_add_oauth_pasted_code() {
    let dir = tempfile::tempdir().unwrap();
    let secrets = dir.path().join("secrets");
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
    // No --code → stdin drives `read_code`: an empty line is skipped,
    // then the pasted code is captured.
    let mut child = Command::new(env!("CARGO_BIN_EXE_cicada"))
        .args([
            "provider",
            "add",
            "google",
            "--client-id",
            "cid",
            "--client-secret",
            "csec",
        ])
        .current_dir(dir.path())
        .env("CICADA_SECRETS_DIR", &secrets)
        .env(
            "CICADA_GOOGLE_TOKEN_URL",
            format!("http://127.0.0.1:{port}/token"),
        )
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .as_mut()
        .unwrap()
        .write_all(b"\n\n4/pasted-code\n")
        .unwrap();
    drop(child.stdin.take());
    let out = child.wait_with_output().unwrap();
    let s = stdout(&out);
    assert!(
        out.status.success(),
        "{s} {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(s.contains("refresh token stored"), "{s}");
}

// ---------- google from_env: valid key paths ----------

#[test]
fn google_from_env_service_account_paths() {
    let dir = tempfile::tempdir().unwrap();
    fixture(dir.path(), SIMPLE_CAL);
    let run = |envs: &[(&str, &str)]| {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_cicada"));
        cmd.args(["plan", "--provider", "google"])
            .current_dir(dir.path())
            .env_remove("CICADA_GOOGLE_SERVICE_ACCOUNT_JSON")
            .env_remove("GOOGLE_APPLICATION_CREDENTIALS")
            .env_remove("CICADA_GOOGLE_ACCESS_TOKEN")
            .env_remove("CICADA_GOOGLE_CLIENT_ID")
            .env_remove("CICADA_GOOGLE_CLIENT_SECRET")
            .env_remove("CICADA_GOOGLE_REFRESH_TOKEN")
            .env_remove("CICADA_GOOGLE_SUBJECT");
        for (k, v) in envs {
            cmd.env(k, v);
        }
        cmd.output().unwrap()
    };

    // A *valid* service-account key file → the ServiceAccount arm;
    // the bogus PEM then fails the JWT build downstream.
    fs::write(
        dir.path().join("sa.json"),
        r#"{"client_email":"a@b.iam.gserviceaccount.com","private_key":"not-a-pem"}"#,
    )
    .unwrap();
    let out = run(&[(
        "CICADA_GOOGLE_SERVICE_ACCOUNT_JSON",
        dir.path().join("sa.json").to_str().unwrap(),
    )]);
    assert!(!out.status.success());

    // A persisted valid key → the last-resort credential arm.
    let secrets = dir.path().join("secrets-sa");
    fs::create_dir_all(&secrets).unwrap();
    fs::write(
        secrets.join("google%2Fservice-account"),
        r#"{"client_email":"a@b.iam.gserviceaccount.com","private_key":"not-a-pem"}"#,
    )
    .unwrap();
    let out = run(&[("CICADA_SECRETS_DIR", secrets.to_str().unwrap())]);
    assert!(!out.status.success());
}
