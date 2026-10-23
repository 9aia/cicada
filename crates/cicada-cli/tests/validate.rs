//! e2e: `cicada validate` — explicit files, manifest discovery, exit
//! codes, and per-file ✓/✗ output.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::needless_raw_string_hashes
)]

use std::fs;
use std::path::Path;
use std::process::Command;

const GOOD: &str = r#"
apiVersion: cicada/v1
kind: Calendar
metadata: { name: work, timeZone: America/Sao_Paulo, provider: google, calendarId: primary }
spec:
  events:
    - id: standup
      summary: Daily Standup
      start: "09:00"
      duration: 30m
      recurrence: { freq: WEEKLY, byDay: [MO,TU,WE,TH,FR] }
"#;

const BAD: &str = r#"
apiVersion: cicada/v1
kind: Calendar
metadata: { name: work, timeZone: UTC }
spec:
  events:
    - id: backwards
      summary: Broken
      start: "2026-08-19T10:00"
      end: "2026-08-19T09:00"
"#;

const MANIFEST: &str = r#"
apiVersion: cicada/v1
kind: Manifest
calendars: [calendars/work.yaml, calendars/team.yaml]
"#;

fn cicada(dir: &Path, args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_cicada"))
        .args(args)
        .current_dir(dir)
        .output()
        .expect("spawn cicada")
}

#[test]
fn explicit_good_and_bad_files() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("good.yaml"), GOOD).unwrap();
    fs::write(dir.path().join("bad.yaml"), BAD).unwrap();

    let out = cicada(dir.path(), &["validate", "good.yaml"]);
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("✓ good.yaml"), "{stdout}");

    let out = cicada(dir.path(), &["validate", "bad.yaml"]);
    assert!(!out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("✗ bad.yaml"), "{stdout}");
    assert!(stdout.contains("events[backwards].time"), "{stdout}");

    // Mixed set: still nonzero, both reported.
    let out = cicada(dir.path(), &["validate", "good.yaml", "bad.yaml"]);
    assert!(!out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("1 valid, 1 failed"), "{stdout}");
}

#[test]
fn manifest_discovery_in_cwd() {
    let dir = tempfile::tempdir().unwrap();
    fs::create_dir(dir.path().join("calendars")).unwrap();
    fs::write(dir.path().join("cicada.yaml"), MANIFEST).unwrap();
    fs::write(dir.path().join("calendars/work.yaml"), GOOD).unwrap();
    fs::write(dir.path().join("calendars/team.yaml"), BAD).unwrap();

    let out = cicada(dir.path(), &["validate"]);
    assert!(!out.status.success()); // team.yaml has errors
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("✓"), "{stdout}");
    assert!(stdout.contains("calendars/work.yaml"), "{stdout}");
    assert!(stdout.contains("✗"), "{stdout}");
    assert!(stdout.contains("calendars/team.yaml"), "{stdout}");
}

#[test]
fn explicit_manifest_expands() {
    let dir = tempfile::tempdir().unwrap();
    fs::create_dir(dir.path().join("calendars")).unwrap();
    fs::write(dir.path().join("cicada.yaml"), MANIFEST).unwrap();
    fs::write(dir.path().join("calendars/work.yaml"), GOOD).unwrap();
    fs::write(dir.path().join("calendars/team.yaml"), GOOD).unwrap();

    let out = cicada(dir.path(), &["validate", "cicada.yaml"]);
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("2 valid, 0 failed"), "{stdout}");
}

#[test]
fn no_manifest_no_files_is_an_error() {
    let dir = tempfile::tempdir().unwrap();
    let out = cicada(dir.path(), &["validate"]);
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("no cicada.yaml"), "{stderr}");
}

#[test]
fn missing_file_reports_in_place() {
    let dir = tempfile::tempdir().unwrap();
    let out = cicada(dir.path(), &["validate", "gone.yaml"]);
    assert!(!out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("✗ gone.yaml"), "{stdout}");
    assert!(stdout.contains("cannot read"), "{stdout}");
}

#[test]
fn init_scaffolds_a_working_project() {
    let dir = tempfile::tempdir().unwrap();
    let out = cicada(dir.path(), &["init"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    for path in ["cicada.yaml", "calendars/work.yaml", ".cicada/.gitignore"] {
        assert!(dir.path().join(path).is_file(), "{path} missing");
    }
    // The scaffold validates, plans, and is already fmt-canonical.
    for args in [vec!["validate"], vec!["plan"], vec!["fmt", "--check"]] {
        let out = cicada(dir.path(), &args);
        assert!(
            out.status.success(),
            "{args:?}: {}\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
    }
    // Re-init refuses to clobber without --force.
    let out = cicada(dir.path(), &["init"]);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("--force"));
    let out = cicada(dir.path(), &["init", "--force"]);
    assert!(out.status.success());
}

#[test]
fn provider_add_unknown_provider_errors() {
    let dir = tempfile::tempdir().unwrap();
    let out = cicada(dir.path(), &["provider", "add", "caldav"]);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("unknown provider"));
}

#[test]
fn provider_add_service_account_rejects_bad_json() {
    let dir = tempfile::tempdir().unwrap();
    let bad = dir.path().join("bad.json");
    std::fs::write(&bad, r#"{"not": "a key"}"#).unwrap();
    let out = cicada(
        dir.path(),
        &[
            "provider",
            "add",
            "google",
            "--service-account",
            bad.to_str().unwrap(),
        ],
    );
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("client_email"), "{stderr}");
}

#[test]
fn fmt_check_flags_noncanonical_and_fmt_fixes() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("calendars")).unwrap();
    std::fs::write(
        dir.path().join("cicada.yaml"),
        "apiVersion: cicada/v1\nkind: Manifest\ncalendars: [calendars/work.yaml]\n",
    )
    .unwrap();
    // Flow-style fields + wrong order → not canonical.
    std::fs::write(
        dir.path().join("calendars/work.yaml"),
        "apiVersion: cicada/v1\nkind: Calendar\nmetadata:\n  name: work\n  \
         timeZone: UTC\nspec:\n  events:\n    - id: a\n      summary: A\n      \
         start: \"2026-01-05T09:00\"\n      duration: 30m\n",
    )
    .unwrap();
    let out = cicada(dir.path(), &["fmt", "--check"]);
    assert!(!out.status.success(), "expected non-canonical flag");
    let out = cicada(dir.path(), &["fmt"]);
    assert!(out.status.success());
    let out = cicada(dir.path(), &["fmt", "--check"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stdout)
    );
}
