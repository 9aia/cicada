//! e2e: `cicada plan`, `cicada range`, `cicada show` against the
//! committed fixture manifest — plan output shape, the
//! `cicada/plan-v1` JSON contract, occurrence expansion, and
//! `--provider google` reporting not-implemented.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::Path;
use std::process::Command;

/// The committed demo manifest (also used by hand: `cargo run -p
/// cicada-cli -- plan` from that directory).
fn fixture() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

fn cicada(dir: &Path, args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_cicada"))
        .args(args)
        .current_dir(dir)
        .output()
        .expect("spawn cicada")
}

#[test]
fn plan_against_fake_provider_is_all_create() {
    let dir = fixture();
    let out_json = dir.join("target-plan-test.json");
    let out = cicada(&dir, &["plan", "--out", "target-plan-test.json"]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "{stdout}\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(stdout.contains("calendar work"), "{stdout}");
    assert!(stdout.contains("create"), "{stdout}");
    assert!(stdout.contains("standup"), "{stdout}");
    assert!(stdout.contains("remote op"), "{stdout}");

    // The plan document is the versioned plan-v1 contract.
    let json: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&out_json).unwrap()).unwrap();
    std::fs::remove_file(&out_json).unwrap();
    assert_eq!(json["apiVersion"], "cicada/plan-v1");
    assert_eq!(json["calendar"], "work");
    let ops = json["ops"].as_array().unwrap();
    assert_eq!(ops.len(), 3);
    assert!(ops.iter().all(|op| op["kind"] == "create"));
    assert_eq!(ops[0]["desired"]["id"], "offsite");
}

#[test]
fn plan_provider_google_needs_credentials() {
    // The google adapter is wired — without credentials it fails
    // with a clear configuration error, not a stub.
    let dir = fixture();
    let out = cicada(&dir, &["plan", "--provider", "google"]);
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("no google credentials"), "{stderr}");
}

#[test]
fn plan_unknown_provider_errors() {
    let dir = fixture();
    let out = cicada(&dir, &["plan", "--provider", "caldav"]);
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("unknown provider"), "{stderr}");
}

#[test]
fn plan_explicit_file_and_manifest_equivalent() {
    let dir = fixture();
    let explicit = cicada(&dir, &["plan", "calendars/work.yaml"]);
    assert!(explicit.status.success());
    let discovered = cicada(&dir, &["plan"]);
    assert!(discovered.status.success());
    // Everything but the header line must match — `generatedAt`
    // legitimately differs between invocations.
    let tail = |out: &std::process::Output| {
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .skip(1)
            .map(str::to_owned)
            .collect::<Vec<_>>()
    };
    assert_eq!(tail(&explicit), tail(&discovered));
}

#[test]
fn range_expands_occurrences() {
    let dir = fixture();
    // A wide fixed window: the weekly standup must appear many times.
    let out = cicada(
        &dir,
        &["range", "--from", "2026-01-05", "--to", "2026-01-19"],
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "{stdout}\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    // 2 weeks × 5 weekdays = 10 standup occurrences, 2 weekly 1:1s.
    assert_eq!(stdout.matches("standup").count(), 10, "{stdout}");
    assert_eq!(stdout.matches("one-on-one").count(), 2, "{stdout}");
    assert!(stdout.contains("09:00"), "{stdout}");
    // The one-off is outside the window.
    assert!(!stdout.contains("offsite"), "{stdout}");
}

#[test]
fn show_prints_detail_and_occurrences() {
    let dir = fixture();
    let out = cicada(
        &dir,
        &[
            "show",
            "standup",
            "--from",
            "2026-01-05",
            "--to",
            "2026-01-12",
        ],
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "{stdout}");
    assert!(stdout.contains("standup — \"Daily Standup\""), "{stdout}");
    assert!(stdout.contains("repeat:"), "{stdout}");
    assert!(stdout.contains("occurrences:"), "{stdout}");
    assert_eq!(
        stdout.matches("standup — Daily Standup").count(),
        5,
        "{stdout}"
    );
}

#[test]
fn show_unknown_id_fails() {
    let dir = fixture();
    let out = cicada(&dir, &["show", "nope"]);
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("no event"), "{stderr}");
}
