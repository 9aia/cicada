//! e2e: the engine-side preview→apply `planDigest` gate (T-019) —
//! `plan` issues a digest persisted beside the state file, `apply
//! --plan-digest` enforces issued+fresh+unspent, through BOTH the
//! `run` passthrough (the `cicada` shim path) and `cli.*` wire ops
//! over `serve` (the generated-ops path cicada-mcp used to own).
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, Command, Output, Stdio};

const BIN: &str = env!("CARGO_BIN_EXE_tamias-driver-cicada");

const CAL: &str = "apiVersion: cicada/v1\nkind: Calendar\nmetadata: {name: work, timeZone: UTC, provider: fake, calendarId: primary}\nspec: {events: [{id: standup, summary: Standup, start: '2026-01-05T09:00', duration: 30m}]}\n";

fn fixture() -> (tempfile::TempDir, std::path::PathBuf) {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().to_path_buf();
    std::fs::create_dir_all(dir.join("calendars")).unwrap();
    std::fs::write(
        dir.join("cicada.yaml"),
        "apiVersion: cicada/v1\nkind: Manifest\ncalendars: [calendars/work.yaml]\n",
    )
    .unwrap();
    std::fs::write(dir.join("calendars/work.yaml"), CAL).unwrap();
    (tmp, dir)
}

/// `tamias-driver-cicada run` — the engine passthrough the `cicada`
/// bins[] shim uses.
fn cicada(dir: &Path, args: &[&str]) -> Output {
    Command::new(BIN)
        .arg("run")
        .args(args)
        .current_dir(dir)
        .output()
        .expect("run passthrough")
}

fn stdout(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

/// The `planDigest sha256:…` token `plan` prints.
fn plan_digest(out: &Output) -> String {
    stdout(out)
        .lines()
        .find_map(|l| l.strip_prefix("planDigest "))
        .unwrap_or_else(|| panic!("no planDigest in output: {}", stdout(out)))
        .to_owned()
}

#[test]
fn plan_issues_digest_and_apply_consumes_it() {
    let (_tmp, dir) = fixture();

    // Preview — issues + persists the digest.
    let plan = cicada(&dir, &["plan"]);
    assert!(plan.status.success(), "{}", stderr(&plan));
    let digest = plan_digest(&plan);
    assert!(digest.starts_with("sha256:"), "{digest}");
    assert!(
        std::fs::read_to_string(dir.join(".cicada/plan-digest"))
            .unwrap()
            .contains(&digest),
        "digest not persisted beside state.yaml"
    );

    // Apply with the digest — no prompt (stdin is null under
    // `output()`), the digest is the confirmation.
    let apply = cicada(&dir, &["apply", "--plan-digest", &digest]);
    assert!(apply.status.success(), "{}", stderr(&apply));
    assert!(stdout(&apply).contains("applied"), "{}", stdout(&apply));

    // Spent — the same digest can't apply twice.
    let again = cicada(&dir, &["apply", "--plan-digest", &digest]);
    assert!(!again.status.success());
    assert!(
        stderr(&again).contains("unknown planDigest"),
        "{}",
        stderr(&again)
    );
}

#[test]
fn apply_rejects_unknown_and_stale_digests() {
    let (_tmp, dir) = fixture();

    // Never previewed → unknown.
    let out = cicada(&dir, &["apply", "--plan-digest", "sha256:deadbeef"]);
    assert!(!out.status.success());
    assert!(
        stderr(&out).contains("unknown planDigest"),
        "{}",
        stderr(&out)
    );

    // Preview, then change desired state → the issued digest is
    // stale (the re-plan under the lock derives a different one).
    let plan = cicada(&dir, &["plan"]);
    assert!(plan.status.success());
    let digest = plan_digest(&plan);
    std::fs::write(
        dir.join("calendars/work.yaml"),
        CAL.replace("summary: Standup", "summary: Standup v2"),
    )
    .unwrap();
    let out = cicada(&dir, &["apply", "--plan-digest", &digest]);
    assert!(!out.status.success());
    assert!(
        stderr(&out).contains("stale planDigest"),
        "{}",
        stderr(&out)
    );

    // Re-preview issues the fresh digest; it applies.
    let plan = cicada(&dir, &["plan"]);
    let digest = plan_digest(&plan);
    let out = cicada(&dir, &["apply", "--plan-digest", &digest]);
    assert!(out.status.success(), "{}", stderr(&out));
}

#[test]
fn apply_without_digest_still_prompts() {
    let (_tmp, dir) = fixture();
    // No digest, no --auto-approve, null stdin → the interactive
    // gate fails closed exactly as before.
    let out = cicada(&dir, &["apply"]);
    assert!(!out.status.success());
    assert!(stderr(&out).contains("aborted"), "{}", stderr(&out));
}

#[test]
fn list_shows_declared_and_recorded_counts() {
    let (_tmp, dir) = fixture();
    let out = cicada(&dir, &["list"]);
    assert!(out.status.success(), "{}", stderr(&out));
    let s = stdout(&out);
    assert!(s.contains("work"), "{s}");
    assert!(s.contains("1 declared"), "{s}");
    assert!(s.contains("0 recorded"), "{s}");

    // After apply, recorded catches up.
    let plan = cicada(&dir, &["plan"]);
    let digest = plan_digest(&plan);
    assert!(
        cicada(&dir, &["apply", "--plan-digest", &digest])
            .status
            .success()
    );
    let out = cicada(&dir, &["list"]);
    assert!(stdout(&out).contains("1 recorded"), "{}", stdout(&out));
}

// ---------- the generated-ops path: cli.* queries over serve ----------

struct Serve {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<std::process::ChildStdout>,
}

impl Serve {
    fn spawn(dir: &Path) -> Self {
        let mut child = Command::new(BIN)
            .arg("serve")
            .current_dir(dir)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("serve spawns");
        let stdin = child.stdin.take().unwrap();
        let stdout = BufReader::new(child.stdout.take().unwrap());
        Self {
            child,
            stdin,
            stdout,
        }
    }

    fn query(&mut self, query: &str, params: &serde_json::Value) -> serde_json::Value {
        let req = serde_json::json!({
            "op": "query",
            "query": query,
            "params": params,
        });
        writeln!(self.stdin, "{req}").unwrap();
        self.stdin.flush().unwrap();
        let mut line = String::new();
        self.stdout.read_line(&mut line).unwrap();
        serde_json::from_str(&line).expect("response is JSON")
    }
}

impl Drop for Serve {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[test]
fn digest_gate_rides_cli_ops_over_serve() {
    let (_tmp, dir) = fixture();
    let mut serve = Serve::spawn(&dir);

    // `cli.plan` — the op child prints the plan + planDigest on its
    // captured stdout; the digest persists beside the state file.
    let plan = serve.query("cli.plan", &serde_json::json!({}));
    assert_eq!(plan["ok"], true, "cli.plan: {plan}");
    let data = &plan["result"]["data"];
    assert_eq!(data["code"], 0, "plan code: {data}");
    let text = data["stdout"].as_str().unwrap();
    let digest = text
        .lines()
        .find_map(|l| l.strip_prefix("planDigest "))
        .unwrap_or_else(|| panic!("no planDigest in cli.plan stdout: {text}"))
        .to_owned();

    // `cli.apply` with a wrong digest → refused (nonzero engine code,
    // honest data on the wire).
    let bad = serve.query(
        "cli.apply",
        &serde_json::json!({"plan-digest": "sha256:wrong"}),
    );
    assert_eq!(bad["ok"], true, "cli.apply wire: {bad}");
    let bad_data = &bad["result"]["data"];
    assert_ne!(bad_data["code"], 0, "wrong digest must fail: {bad_data}");
    assert!(
        bad_data["stderr"]
            .as_str()
            .unwrap()
            .contains("unknown planDigest"),
        "{}",
        bad_data["stderr"]
    );

    // `cli.apply` with the issued digest → applies; the op child's
    // null stdin never matters — the digest is the confirmation.
    let good = serve.query("cli.apply", &serde_json::json!({"plan-digest": digest}));
    assert_eq!(good["ok"], true, "cli.apply wire: {good}");
    let good_data = &good["result"]["data"];
    assert_eq!(good_data["code"], 0, "apply code: {good_data}");
    assert!(
        good_data["stdout"].as_str().unwrap().contains("applied"),
        "{}",
        good_data["stdout"]
    );

    // And `cli.list` answers the calendar.list read.
    let list = serve.query("cli.list", &serde_json::json!({}));
    let list_data = &list["result"]["data"];
    assert_eq!(list_data["code"], 0, "list code: {list_data}");
    assert!(
        list_data["stdout"].as_str().unwrap().contains("1 recorded"),
        "{}",
        list_data["stdout"]
    );
}
