//! e2e: `tamias-driver-cicada` verbs against a fixture payload —
//! capabilities/config-schema shapes, install validation (good and
//! bad payloads), deploy's plan-only artifacts, and uninstall's
//! read-only managed-inventory report. The fake provider is the
//! credential-free backend (`provider: fake` + `fakeFile`).
#![allow(clippy::assert_is_empty, clippy::unwrap_used, clippy::expect_used)]

use std::path::PathBuf;
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_tamias-driver-cicada");

/// A scratch payload: `tamias.yaml` + `cicada.yaml` manifest + one
/// calendar, plus a driver `--config` selecting the fake provider
/// with state/fake files outside the payload dir.
struct Fixture {
    /// Tempdir root — everything lives under it.
    _tmp: tempfile::TempDir,
    /// The installed payload dir (`--dir`).
    payload: PathBuf,
    /// The driver config file (`--config`).
    config: PathBuf,
    /// Resolved `stateDir` from the config.
    state_dir: PathBuf,
    /// Resolved `fakeFile` from the config.
    fake_file: PathBuf,
}

fn fixture() -> Fixture {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let payload = root.join("store/pkg/cicada-demo-0.1.0");
    let calendars = payload.join("calendars");
    std::fs::create_dir_all(&calendars).unwrap();

    std::fs::write(
        payload.join("tamias.yaml"),
        "name: cicada-demo\nversion: 0.1.0\ntype: cicada\ndescription: demo\n",
    )
    .unwrap();
    std::fs::write(
        payload.join("cicada.yaml"),
        "apiVersion: cicada/v1\nkind: Manifest\ncalendars: [calendars/work.yaml]\n",
    )
    .unwrap();
    std::fs::write(
        calendars.join("work.yaml"),
        r#"apiVersion: cicada/v1
kind: Calendar
metadata:
  name: work
  timeZone: UTC
  provider: fake
  calendarId: primary
spec:
  events:
    - id: standup
      summary: Daily Standup
      start: "09:00"
      duration: 30m
      recurrence: { freq: WEEKLY, byDay: [MO,TU,WE,TH,FR] }
    - id: retro
      summary: Sprint Retrospective
      start: "2026-11-06T16:00"
      duration: 1h
"#,
    )
    .unwrap();

    let state_dir = root.join("operator/cicada-state");
    let fake_file = root.join("operator/fake-provider.json");
    let config = root.join("driver.yaml");
    std::fs::write(
        &config,
        format!(
            "provider: fake\nstateDir: {}\nfakeFile: {}\n",
            state_dir.display(),
            fake_file.display()
        ),
    )
    .unwrap();

    Fixture {
        _tmp: tmp,
        payload,
        config,
        state_dir,
        fake_file,
    }
}

fn run(args: &[&str]) -> Output {
    Command::new(BIN).args(args).output().expect("spawn driver")
}

fn stdout_json(out: &Output) -> serde_json::Value {
    serde_json::from_slice(&out.stdout).expect("stdout is JSON")
}

fn verb(fx: &Fixture, verb: &str, extra: &[&str]) -> Output {
    let mut args = vec![
        verb,
        "--dir",
        fx.payload.to_str().unwrap(),
        "--name",
        "cicada-demo",
        "--config",
        fx.config.to_str().unwrap(),
    ];
    args.extend_from_slice(extra);
    run(&args)
}

#[test]
fn capabilities_reports_types_and_verbs() {
    let out = run(&["capabilities"]);
    assert!(out.status.success());
    let json = stdout_json(&out);
    assert_eq!(json["types"], serde_json::json!(["cicada"]));
    assert!(json["capabilities"].is_array());
    let verbs: Vec<&str> = json["verbs"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|v| v.as_str())
        .collect();
    for expected in [
        "install",
        "deploy",
        "uninstall",
        "status",
        "config-schema",
        "capabilities",
    ] {
        assert!(verbs.contains(&expected), "missing verb {expected}");
    }
}

#[test]
fn config_schema_is_draft_2020_12() {
    let out = run(&["config-schema", "--dir", "/tmp", "--name", "x"]);
    assert!(out.status.success());
    let json = stdout_json(&out);
    assert_eq!(
        json["$schema"],
        "https://json-schema.org/draft/2020-12/schema"
    );
    assert_eq!(json["type"], "object");
    assert!(
        json["properties"]["provider"]["enum"]
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v == "fake")
    );
}

#[test]
fn install_validates_a_good_payload() {
    let fx = fixture();
    let out = verb(&fx, "install", &[]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let json = stdout_json(&out);
    assert_eq!(json["ok"], true);
    assert_eq!(json["package"], "cicada-demo");
    assert_eq!(json["applied"], false);
    let calendars = json["calendars"].as_array().unwrap();
    assert_eq!(calendars.len(), 1);
    assert_eq!(calendars[0]["calendar"], "work");
    assert_eq!(calendars[0]["events"], 2);
    assert_eq!(calendars[0]["errors"], 0);
}

#[test]
fn install_rejects_a_bad_payload() {
    let fx = fixture();
    // Corrupt the calendar: drop `kind` and break a field type.
    std::fs::write(
        fx.payload.join("calendars/work.yaml"),
        "apiVersion: cicada/v1\nmetadata:\n  name: work\nspec:\n  events: notalist\n",
    )
    .unwrap();
    let out = verb(&fx, "install", &[]);
    assert!(!out.status.success());
    let json = stdout_json(&out);
    assert_eq!(json["ok"], false);
    assert!(json["errors"].as_u64().unwrap() > 0);
    assert!(
        json["calendars"][0]["error"].is_string()
            || json["calendars"][0]["errors"].as_u64().unwrap() > 0
    );
}

#[test]
fn deploy_writes_plan_artifacts_without_mutating_provider() {
    let fx = fixture();
    let out = verb(&fx, "deploy", &[]);
    assert!(
        out.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let json = stdout_json(&out);
    assert_eq!(json["ok"], true);
    assert_eq!(json["applied"], false);
    assert_eq!(json["provider"], "fake");

    // Operator config + plan artifact land under the resolved
    // stateDir — outside the evictable payload.
    let driver_json = fx.state_dir.join("driver.json");
    let plan_json = fx.state_dir.join("plan.json");
    assert!(driver_json.is_file(), "driver.json missing");
    assert!(plan_json.is_file(), "plan.json missing");
    let plan: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&plan_json).unwrap()).unwrap();
    assert_eq!(plan["apiVersion"], "cicada/plan-v1");
    assert_eq!(plan["calendar"], "work");
    assert!(!plan["ops"].as_array().unwrap().is_empty());
    let driver: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&driver_json).unwrap()).unwrap();
    assert_eq!(driver["apiVersion"], "cicada/driver-config-v1");
    assert_eq!(driver["provider"], "fake");

    // Plan-only: the fake provider backend file is never created —
    // reads don't persist, writes would.
    assert!(!fx.fake_file.exists(), "deploy mutated provider state");
    // And no `.cicada/` leaks into the payload when stateDir is set.
    assert!(!fx.payload.join(".cicada").exists());
}

#[test]
fn uninstall_reports_without_deleting() {
    let fx = fixture();
    // Seed recorded state is absent — nothing applied yet; the report
    // must still be honest and touch nothing.
    let out = verb(&fx, "uninstall", &[]);
    assert!(out.status.success());
    let json = stdout_json(&out);
    assert_eq!(json["ok"], true);
    assert_eq!(json["deletedRemoteEvents"], false);
    assert!(!fx.fake_file.exists(), "uninstall mutated provider state");
}

#[test]
fn status_reads_recorded_state_without_provider() {
    let fx = fixture();
    // No config at all — status must not need credentials or a
    // provider backend.
    let out = run(&[
        "status",
        "--dir",
        fx.payload.to_str().unwrap(),
        "--name",
        "cicada-demo",
    ]);
    assert!(out.status.success());
    let json = stdout_json(&out);
    assert_eq!(json["ok"], true);
    assert_eq!(json["applied"], false);
    assert_eq!(json["provider"], "google"); // documented default
    assert!(json["calendars"].as_array().unwrap().is_empty());
}

#[test]
fn deploy_rejects_a_broken_payload() {
    let fx = fixture();
    std::fs::write(fx.payload.join("cicada.yaml"), "kind: Nope\n").unwrap();
    let out = verb(&fx, "deploy", &[]);
    assert!(!out.status.success());
    // No artifacts may be written for a payload that doesn't parse.
    assert!(!fx.state_dir.join("plan.json").exists());
}
