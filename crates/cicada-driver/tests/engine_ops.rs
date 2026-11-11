//! e2e: the engine ops surface — `ops` manifest shape, `run`
//! passthrough equivalence with `cicada`, and `cli.<verb>` wire
//! queries over `serve` (ndjson JSON-RPC on stdio).
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, Command, Stdio};

const BIN: &str = env!("CARGO_BIN_EXE_tamias-driver-cicada");

/// A scratch cicada workspace: `cicada.yaml` manifest + one
/// calendar — enough for `validate`/`plan`/`range`/`show` to run
/// credential-free (no `provider:` → local-only paths).
struct Fixture {
    _tmp: tempfile::TempDir,
    dir: std::path::PathBuf,
}

fn fixture() -> Fixture {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().to_path_buf();
    std::fs::create_dir_all(dir.join("calendars")).unwrap();
    std::fs::write(
        dir.join("cicada.yaml"),
        "apiVersion: cicada/v1\nkind: Manifest\ncalendars: [calendars/work.yaml]\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("calendars/work.yaml"),
        r#"apiVersion: cicada/v1
kind: Calendar
metadata:
  name: work
  timeZone: UTC
spec:
  events:
    - id: standup
      summary: Daily Standup
      start: "2026-03-20T09:00"
      duration: 30m
"#,
    )
    .unwrap();
    Fixture { _tmp: tmp, dir }
}

fn driver(dir: &Path, args: &[&str]) -> std::process::Output {
    Command::new(BIN)
        .args(args)
        .current_dir(dir)
        .output()
        .expect("driver runs")
}

fn stdout_json(out: &std::process::Output) -> serde_json::Value {
    serde_json::from_slice(&out.stdout).expect("stdout is JSON")
}

#[test]
fn ops_manifest_covers_bounded_verbs() {
    let fx = fixture();
    let out = driver(&fx.dir, &["ops"]);
    assert!(out.status.success());
    let m = stdout_json(&out);
    assert_eq!(m["engine"], "cicada");
    let ops = m["ops"].as_array().unwrap();
    let queries: Vec<&str> = ops.iter().filter_map(|o| o["query"].as_str()).collect();
    for expected in [
        "cli.init",
        "cli.validate",
        "cli.list",
        "cli.plan",
        "cli.apply",
        "cli.refresh",
        "cli.drift",
        "cli.range",
        "cli.show",
        "cli.fmt",
    ] {
        assert!(queries.contains(&expected), "missing {expected}");
    }
    // surfaces — never ops (cicada-mcp's `mcp` verb was deleted in
    // T-019; the digest gate rides cli.plan/cli.apply)
    assert!(!queries.contains(&"cli.mcp"));
    assert!(!queries.contains(&"cli.provider.add"));
    assert_eq!(m["surfaces"], serde_json::json!(["provider.add"]));
    // mutates honesty
    let apply = ops.iter().find(|o| o["query"] == "cli.apply").unwrap();
    assert_eq!(apply["mutates"], true);
    let validate = ops.iter().find(|o| o["query"] == "cli.validate").unwrap();
    assert_eq!(validate["mutates"], false);
}

#[test]
fn run_passthrough_matches_cicada_validate() {
    let fx = fixture();
    let out = driver(&fx.dir, &["run", "validate"]);
    assert!(
        out.status.success(),
        "run validate failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("work"), "expected calendar name in {text}");
}

/// One ndjson request/response round-trip against `serve`.
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

    fn request(&mut self, value: &serde_json::Value) -> serde_json::Value {
        writeln!(self.stdin, "{value}").unwrap();
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
fn serve_answers_ops_and_cli_queries() {
    let fx = fixture();
    let mut serve = Serve::spawn(&fx.dir);

    let ops = serve.request(&serde_json::json!({"op": "query", "query": "ops"}));
    assert_eq!(ops["ok"], true, "ops query: {ops}");
    assert!(ops["result"]["data"]["ops"].is_array());

    // `cli.validate` — a bounded read-only op; params: none needed
    // (manifest discovery from the inherited cwd).
    let v = serve.request(&serde_json::json!({
        "op": "query",
        "query": "cli.validate",
        "params": {}
    }));
    assert_eq!(v["ok"], true, "cli.validate: {v}");
    let data = &v["result"]["data"];
    assert_eq!(data["op"], "validate");
    assert_eq!(data["code"], 0, "validate code: {data}");
    assert!(
        data["stdout"].as_str().unwrap().contains("work"),
        "validate stdout: {data}"
    );

    // `cli.show` with params — positional `id` + `files`.
    let s = serve.request(&serde_json::json!({
        "op": "query",
        "query": "cli.show",
        "params": {"id": "standup"}
    }));
    assert_eq!(s["ok"], true, "cli.show: {s}");
    let data = &s["result"]["data"];
    assert_eq!(data["code"], 0, "show code: {data}");
    assert!(
        data["stdout"].as_str().unwrap().contains("standup"),
        "show stdout: {data}"
    );

    // unknown query fails the op honestly
    let bad = serve.request(&serde_json::json!({
        "op": "query",
        "query": "cli.bogus"
    }));
    assert_eq!(bad["ok"], false);
}
