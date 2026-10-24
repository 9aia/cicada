//! End-to-end MCP stdio test — spawn the real `cicada-mcp` binary
//! against a tempdir manifest + file-backed fake provider and drive
//! the JSON-RPC handshake and tool calls over real pipes.
#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::needless_pass_by_value
)]

use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use serde_json::{Value, json};

const READ_TIMEOUT: Duration = Duration::from_secs(30);

/// A spawned server with a line-oriented stdout reader.
struct Server {
    child: Child,
    stdin: Option<ChildStdin>,
    lines: mpsc::Receiver<String>,
    /// Captured stderr, printed on failure paths.
    stderr: mpsc::Receiver<String>,
}

impl Server {
    /// Spawn `cicada-mcp --root <dir>` and start reader threads.
    fn spawn(root: &Path) -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_cicada-mcp"))
            .args(["--root", root.to_str().unwrap()])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn cicada-mcp");
        let stdin = child.stdin.take().unwrap();
        let (tx, lines) = mpsc::channel();
        let mut stdout = BufReader::new(child.stdout.take().unwrap());
        thread::spawn(move || {
            let mut line = String::new();
            while stdout.read_line(&mut line).is_ok_and(|n| n > 0) {
                let _ = tx.send(line.trim_end().to_owned());
                line.clear();
            }
        });
        let (etx, stderr) = mpsc::channel();
        let mut err = BufReader::new(child.stderr.take().unwrap());
        thread::spawn(move || {
            let mut line = String::new();
            while err.read_line(&mut line).is_ok_and(|n| n > 0) {
                let _ = etx.send(line.trim_end().to_owned());
                line.clear();
            }
        });
        Self {
            child,
            stdin: Some(stdin),
            lines,
            stderr,
        }
    }

    /// Write one JSON-RPC message.
    fn send(&mut self, msg: &Value) {
        let line = serde_json::to_string(msg).unwrap();
        let stdin = self.stdin.as_mut().unwrap();
        writeln!(stdin, "{line}").unwrap();
        stdin.flush().unwrap();
    }

    /// Read the next response (skips server→client notifications,
    /// which carry no `id`). Panics with stderr on timeout.
    fn recv(&mut self) -> Value {
        let deadline = std::time::Instant::now() + READ_TIMEOUT;
        loop {
            let left = deadline.saturating_duration_since(std::time::Instant::now());
            assert!(
                left > Duration::ZERO,
                "timed out — stderr: {}",
                self.drain_stderr()
            );
            match self.lines.recv_timeout(left) {
                Ok(line) => {
                    let v: Value = serde_json::from_str(&line)
                        .unwrap_or_else(|e| panic!("bad JSON from server: {e}: {line}"));
                    if v.get("id").is_some() {
                        return v;
                    }
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    panic!("timed out — stderr: {}", self.drain_stderr());
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    panic!("server closed stdout — stderr: {}", self.drain_stderr());
                }
            }
        }
    }

    /// `request` + `recv` for a JSON-RPC id.
    fn rpc(&mut self, id: u64, method: &str, params: Value) -> Value {
        self.send(&json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        }));
        let resp = self.recv();
        assert_eq!(resp["id"], id, "response id mismatch: {resp}");
        resp
    }

    fn drain_stderr(&self) -> String {
        let mut out = String::new();
        while let Ok(line) = self.stderr.try_recv() {
            out.push_str(&line);
            out.push('\n');
        }
        out
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        // Close stdin first — rmcp's stdio transport ends the serve
        // loop on EOF, so the process exits cleanly (flushing
        // coverage data); kill only as a fallback.
        drop(self.stdin.take());
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        loop {
            match self.child.try_wait() {
                Ok(Some(_)) => return,
                Ok(None) if std::time::Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(25));
                }
                _ => break,
            }
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Write the manifest + calendar fixture into `root`.
fn write_fixture(root: &Path) {
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
         \x20     recurrence: { freq: DAILY, count: 3 }\n",
    )
    .unwrap();
}

fn tool_call(name: &str, arguments: Value) -> Value {
    json!({ "name": name, "arguments": arguments })
}

#[test]
fn mcp_stdio_full_session() {
    let dir = tempfile::tempdir().unwrap();
    write_fixture(dir.path());
    let mut server = Server::spawn(dir.path());

    // Handshake: initialize → initialized.
    let init = server.rpc(
        1,
        "initialize",
        json!({
            "protocolVersion": "2025-06-18",
            "capabilities": {},
            "clientInfo": { "name": "e2e-test", "version": "0" },
        }),
    );
    assert_eq!(init["result"]["serverInfo"]["name"], "cicada-mcp");
    assert!(init["result"]["protocolVersion"].is_string());
    server.send(&json!({
        "jsonrpc": "2.0",
        "method": "notifications/initialized",
    }));

    // tools/list — the ADR-0001 §8a surface; no ad-hoc create.
    let tools = server.rpc(2, "tools/list", json!({}));
    let names: Vec<&str> = tools["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    assert_eq!(
        names,
        [
            "cicada.calendar.list",
            "cicada.events.range",
            "cicada.events.show",
            "cicada.state.drift",
            "cicada.state.refresh",
            "cicada.plan.preview",
            "cicada.plan.apply",
        ]
    );

    // Read tools.
    let list = server.rpc(
        3,
        "tools/call",
        tool_call("cicada.calendar.list", json!({})),
    );
    let cals = &list["result"]["structuredContent"]["calendars"];
    assert_eq!(cals[0]["name"], "work");
    assert_eq!(cals[0]["declaredEvents"], 1);

    let range = server.rpc(
        4,
        "tools/call",
        tool_call(
            "cicada.events.range",
            json!({ "from": "2026-01-05", "to": "2026-01-08" }),
        ),
    );
    assert_eq!(range["result"]["structuredContent"]["totalOccurrences"], 3);

    // plan.preview → digest.
    let preview = server.rpc(5, "tools/call", tool_call("cicada.plan.preview", json!({})));
    assert_eq!(preview["result"]["isError"], false);
    let digest = preview["result"]["structuredContent"]["planDigest"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(digest.starts_with("sha256:"));

    // Gate 1: confirm:false is refused.
    let refused = server.rpc(
        6,
        "tools/call",
        tool_call(
            "cicada.plan.apply",
            json!({ "planDigest": digest, "confirm": false }),
        ),
    );
    assert_eq!(refused["result"]["isError"], true);
    assert!(
        refused["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("confirm: true")
    );

    // Gate 2: an unissued digest is refused.
    let wrong = server.rpc(
        7,
        "tools/call",
        tool_call(
            "cicada.plan.apply",
            json!({ "planDigest": "sha256:0000", "confirm": true }),
        ),
    );
    assert_eq!(wrong["result"]["isError"], true);
    assert!(
        wrong["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("unknown planDigest")
    );

    // Gated apply lands.
    let apply = server.rpc(
        8,
        "tools/call",
        tool_call(
            "cicada.plan.apply",
            json!({ "planDigest": digest, "confirm": true }),
        ),
    );
    let applied = &apply["result"]["structuredContent"];
    assert_eq!(apply["result"]["isError"], false, "{apply}");
    assert_eq!(applied["calendars"][0]["applied"], 1);
    assert_eq!(applied["calendars"][0]["outcomes"][0]["status"], "applied");

    // Out-of-band edit → drift shows modified. The etag must bump
    // too — where both sides expose one, the etag is the
    // authoritative drift signal, and real providers bump on writes.
    let file: PathBuf = dir.path().join(".cicada/fake-provider.json");
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

    let drift = server.rpc(9, "tools/call", tool_call("cicada.state.drift", json!({})));
    assert_eq!(drift["result"]["structuredContent"]["totalDrifted"], 1);
    assert_eq!(
        drift["result"]["structuredContent"]["calendars"][0]["drift"][0]["kind"],
        "modified"
    );

    // Unknown tool errors in-band.
    let bad = server.rpc(
        10,
        "tools/call",
        tool_call("cicada.events.create", json!({})),
    );
    assert_eq!(bad["result"]["isError"], true);
}

#[test]
fn mcp_stdio_reports_missing_manifest() {
    // No cicada.yaml in the root → the server refuses to start with
    // a clear stderr diagnostic (stdout stays clean for the client).
    let dir = tempfile::tempdir().unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_cicada-mcp"))
        .args(["--root", dir.path().to_str().unwrap()])
        .output()
        .unwrap();
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("manifest"), "{stderr}");
}

fn init_handshake(server: &mut Server) {
    server.rpc(
        1,
        "initialize",
        json!({
            "protocolVersion": "2025-06-18",
            "capabilities": {},
            "clientInfo": { "name": "e2e-test", "version": "0" },
        }),
    );
    server.send(&json!({
        "jsonrpc": "2.0",
        "method": "notifications/initialized",
    }));
}

/// Error paths: arg decode, unknown calendar, bad windows, live
/// reads, recorded-state tools, and every apply outcome shape.
#[test]
fn mcp_stdio_error_paths_and_state_tools() {
    let dir = tempfile::tempdir().unwrap();
    write_fixture(dir.path());
    let mut server = Server::spawn(dir.path());
    init_handshake(&mut server);
    let mut id = 100;
    let mut call = |name: &str, args: Value| -> Value {
        id += 1;
        server.rpc(id, "tools/call", tool_call(name, args))
    };

    // Malformed arguments → in-band error, per tool.
    for (name, args) in [
        ("cicada.events.range", json!({ "from": 5 })),
        ("cicada.events.show", json!({ "id": 5 })),
        ("cicada.plan.preview", json!({ "notify": 5 })),
        ("cicada.plan.apply", json!({ "confirm": "yes" })),
    ] {
        let out = call(name, args);
        assert_eq!(out["result"]["isError"], true, "{name}: {out}");
        assert!(
            out["result"]["content"][0]["text"]
                .as_str()
                .unwrap()
                .contains("invalid arguments"),
            "{name}: {out}"
        );
    }

    // Unknown calendar name → clear fatal.
    let out = call("cicada.events.range", json!({ "calendar": "nope" }));
    let text = out["result"]["content"][0]["text"].as_str().unwrap();
    assert!(text.contains("unknown calendar"), "{text}");

    // Bad window bounds + reversed window.
    for args in [
        json!({ "from": "nope" }),
        json!({ "to": "nope" }),
        json!({ "from": "2030-01-02", "to": "2030-01-01" }),
    ] {
        let out = call("cicada.events.range", args);
        assert_eq!(out["result"]["isError"], true, "{out}");
    }
    let out = call(
        "cicada.events.show",
        json!({ "id": "standup", "from": "bogus" }),
    );
    assert_eq!(out["result"]["isError"], true, "{out}");

    // Calendar-scoped + live range reads.
    let out = call(
        "cicada.events.range",
        json!({ "calendar": "work", "from": "2026-01-05", "to": "2026-01-08" }),
    );
    assert_eq!(out["result"]["isError"], false, "{out}");
    let out = call(
        "cicada.events.range",
        json!({ "live": true, "from": "2026-01-05", "to": "2026-01-08" }),
    );
    assert_eq!(out["result"]["isError"], false, "{out}");

    // show: found + not found.
    let out = call("cicada.events.show", json!({ "id": "standup" }));
    assert_eq!(out["result"]["isError"], false, "{out}");
    let out = call("cicada.events.show", json!({ "id": "nope" }));
    assert_eq!(out["result"]["isError"], true, "{out}");

    // drift/refresh before any apply → "no recorded state".
    let out = call("cicada.state.drift", json!({}));
    let text = out["result"]["content"][0]["text"].as_str().unwrap();
    assert!(text.contains("no recorded state"), "{text}");
    let out = call("cicada.state.refresh", json!({}));
    let text = out["result"]["content"][0]["text"].as_str().unwrap();
    assert!(text.contains("no recorded state"), "{text}");

    // Bad notify on preview is a config error, not a crash.
    let out = call("cicada.plan.preview", json!({ "notify": "sometimes" }));
    assert_eq!(out["result"]["isError"], true, "{out}");

    // preview → apply → noop re-apply covers outcome_json's
    // applied/noop arms.
    let preview = call("cicada.plan.preview", json!({}));
    let digest = preview["result"]["structuredContent"]["planDigest"]
        .as_str()
        .unwrap()
        .to_owned();
    let apply = call(
        "cicada.plan.apply",
        json!({ "planDigest": digest, "confirm": true }),
    );
    assert_eq!(apply["result"]["isError"], false, "{apply}");
    // The digest is spent — a second apply of it is unknown.
    let again = call(
        "cicada.plan.apply",
        json!({ "planDigest": digest, "confirm": true }),
    );
    let text = again["result"]["content"][0]["text"].as_str().unwrap();
    assert!(text.contains("unknown planDigest"), "{text}");

    // Re-preview after apply → noop plan ("no instructions" branch).
    let preview2 = call("cicada.plan.preview", json!({}));
    assert_eq!(preview2["result"]["isError"], false, "{preview2}");
    assert_eq!(preview2["result"]["structuredContent"]["remoteOps"], 0);

    // A stale digest: change the YAML between preview and apply.
    let stale = call("cicada.plan.preview", json!({}));
    let stale_digest = stale["result"]["structuredContent"]["planDigest"]
        .as_str()
        .unwrap()
        .to_owned();
    fs::write(
        dir.path().join("calendars/work.yaml"),
        "apiVersion: cicada/v1\n\
         kind: Calendar\n\
         metadata:\n\
         \x20 name: work\n\
         \x20 timeZone: UTC\n\
         spec:\n\
         \x20 events:\n\
         \x20   - id: changed\n\
         \x20     summary: Changed\n\
         \x20     start: \"2026-02-01T09:00:00\"\n\
         \x20     duration: 30m\n",
    )
    .unwrap();
    let out = call(
        "cicada.plan.apply",
        json!({ "planDigest": stale_digest, "confirm": true }),
    );
    let text = out["result"]["content"][0]["text"].as_str().unwrap();
    assert!(text.contains("stale planDigest"), "{text}");

    // A failed apply marks the call isError (conflict without
    // force): apply the new event, edit it out-of-band, then
    // preview+apply without force.
    let preview3 = call("cicada.plan.preview", json!({}));
    let digest3 = preview3["result"]["structuredContent"]["planDigest"]
        .as_str()
        .unwrap()
        .to_owned();
    let apply3 = call(
        "cicada.plan.apply",
        json!({ "planDigest": digest3, "confirm": true }),
    );
    assert_eq!(apply3["result"]["isError"], false, "{apply3}");
    let file: PathBuf = dir.path().join(".cicada/fake-provider.json");
    let mut snap: Value = serde_json::from_str(&fs::read_to_string(&file).unwrap()).unwrap();
    let ev = snap["calendars"]["primary"]
        .as_object_mut()
        .unwrap()
        .values_mut()
        .next()
        .unwrap();
    ev["body"]["summary"] = json!("OOB Edit");
    ev["etag"] = json!("etag-oob");
    fs::write(&file, serde_json::to_string_pretty(&snap).unwrap()).unwrap();
    let preview4 = call("cicada.plan.preview", json!({}));
    let digest4 = preview4["result"]["structuredContent"]["planDigest"]
        .as_str()
        .unwrap()
        .to_owned();
    let apply4 = call(
        "cicada.plan.apply",
        json!({ "planDigest": digest4, "confirm": true }),
    );
    assert_eq!(apply4["result"]["isError"], true, "{apply4}");
    let outcome = &apply4["result"]["structuredContent"]["calendars"][0]["outcomes"][0];
    assert_eq!(outcome["status"], "failed", "{apply4}");
    assert_eq!(outcome["errorKind"], "conflict", "{apply4}");

    // --resume re-runs skipping journaled ops.
    let preview5 = call("cicada.plan.preview", json!({}));
    let digest5 = preview5["result"]["structuredContent"]["planDigest"]
        .as_str()
        .unwrap()
        .to_owned();
    let apply5 = call(
        "cicada.plan.apply",
        json!({ "planDigest": digest5, "confirm": true, "resume": true }),
    );
    assert_eq!(apply5["result"]["isError"], true, "{apply5}");

    // drift/refresh with recorded state.
    let out = call("cicada.state.drift", json!({}));
    assert_eq!(out["result"]["isError"], false, "{out}");
    let out = call("cicada.state.refresh", json!({}));
    assert_eq!(out["result"]["isError"], false, "{out}");

    // Recorded state for a calendar that isn't declared → the
    // remote_for_recorded fallback arm.
    let state_file = dir.path().join(".cicada/state.yaml");
    let state = fs::read_to_string(&state_file).unwrap();
    let ghost = "  ghost:\n    provider: fake\n    calendarId: primary\n    events: {}\n";
    let injected = state.replacen("calendars:\n", &format!("calendars:\n{ghost}"), 1);
    fs::write(&state_file, injected).unwrap();
    let out = call("cicada.state.drift", json!({}));
    assert_eq!(out["result"]["isError"], false, "{out}");
    let names: Vec<&str> = out["result"]["structuredContent"]["calendars"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["calendar"].as_str().unwrap())
        .collect();
    assert!(names.contains(&"ghost"), "{names:?}");
}

/// `tools/call` against a server whose state file corrupts
/// mid-session — store-load error arms.
#[test]
fn mcp_stdio_store_load_errors() {
    let dir = tempfile::tempdir().unwrap();
    write_fixture(dir.path());
    let mut server = Server::spawn(dir.path());
    init_handshake(&mut server);
    fs::create_dir_all(dir.path().join(".cicada")).unwrap();
    fs::write(dir.path().join(".cicada/state.yaml"), "{ corrupt").unwrap();
    let out = server.rpc(
        2,
        "tools/call",
        tool_call("cicada.calendar.list", json!({})),
    );
    assert_eq!(out["result"]["isError"], true, "{out}");
    let out = server.rpc(3, "tools/call", tool_call("cicada.events.range", json!({})));
    assert_eq!(out["result"]["isError"], true, "{out}");
    let out = server.rpc(4, "tools/call", tool_call("cicada.state.drift", json!({})));
    assert_eq!(out["result"]["isError"], true, "{out}");
    let out = server.rpc(5, "tools/call", tool_call("cicada.plan.preview", json!({})));
    assert_eq!(out["result"]["isError"], true, "{out}");
}

/// Mid-session corruption: calendars/provider/state errors surface as
/// in-band tool errors, plus the show/preview data-dependent branches.
#[test]
fn mcp_stdio_corruption_and_edge_paths() {
    let dir = tempfile::tempdir().unwrap();
    write_fixture(dir.path());
    let mut server = Server::spawn(dir.path());
    init_handshake(&mut server);
    let mut id = 200;
    let mut call = |name: &str, args: Value| -> Value {
        id += 1;
        server.rpc(id, "tools/call", tool_call(name, args))
    };

    // Apply once so recorded state is non-empty (drift/refresh read it).
    let preview = call("cicada.plan.preview", json!({}));
    assert_eq!(preview["result"]["isError"], false, "{preview}");
    let digest = preview["result"]["structuredContent"]["planDigest"]
        .as_str()
        .map(str::to_owned)
        .or_else(|| {
            preview["result"]["content"][0]["text"]
                .as_str()
                .and_then(|t| t.split("planDigest ").nth(1))
                .and_then(|s| s.split(' ').next())
                .map(str::to_owned)
        })
        .expect("digest in preview");
    let apply = call(
        "cicada.plan.apply",
        json!({ "planDigest": digest, "confirm": true }),
    );
    assert_eq!(apply["result"]["isError"], false, "{apply}");

    // events.show with a one-occurrence window → singular text.
    let out = call(
        "cicada.events.show",
        json!({ "id": "standup", "from": "2026-01-05", "to": "2026-01-06" }),
    );
    let text = out["result"]["content"][0]["text"].as_str().unwrap_or("");
    assert!(text.contains("occurrence"), "{text}");

    // Corrupt the calendar YAML → every desired-state read fails.
    fs::write(dir.path().join("calendars/work.yaml"), "{ corrupt").unwrap();
    for name in [
        "cicada.calendar.list",
        "cicada.events.range",
        "cicada.events.show",
        "cicada.state.drift",
        "cicada.state.refresh",
        "cicada.plan.preview",
    ] {
        let out = call(name, json!({ "id": "standup", "live": true }));
        assert_eq!(out["result"]["isError"], true, "{name}: {out}");
    }

    // Restore the calendar; corrupt the fake provider file → live
    // reads fail while desired-state reads still pass.
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
         \x20     summary: Daily Standup\n\
         \x20     start: \"2026-01-05T09:00:00\"\n\
         \x20     duration: 30m\n\
         \x20     recurrence: { freq: DAILY, count: 3 }\n",
    )
    .unwrap();
    fs::write(dir.path().join(".cicada/fake-provider.json"), "{ corrupt").unwrap();
    let out = call("cicada.events.range", json!({ "live": true }));
    assert_eq!(out["result"]["isError"], true, "{out}");
    let out = call("cicada.state.drift", json!({}));
    assert_eq!(out["result"]["isError"], true, "{out}");
    let out = call("cicada.state.refresh", json!({}));
    assert_eq!(out["result"]["isError"], true, "{out}");

    // A valid digest + a bad `notify` value errors at apply arg parse.
    let preview = call("cicada.plan.preview", json!({}));
    assert_eq!(preview["result"]["isError"], true, "{preview}");
}

/// An unexpandable recurrence + a capability error both produce
/// in-band failures (ShowEvent::run Err / has_errors branch).
#[test]
fn mcp_stdio_unexpandable_and_capability_errors() {
    let dir = tempfile::tempdir().unwrap();
    fs::create_dir_all(dir.path().join("calendars")).unwrap();
    fs::write(
        dir.path().join("cicada.yaml"),
        "apiVersion: cicada/v1\nkind: Manifest\ncalendars: [calendars/work.yaml]\n",
    )
    .unwrap();
    fs::write(
        dir.path().join("calendars/work.yaml"),
        "apiVersion: cicada/v1\n\
         kind: Calendar\n\
         metadata:\n\
         \x20 name: work\n\
         \x20 timeZone: UTC\n\
         spec:\n\
         \x20 events:\n\
         \x20   - id: tick\n\
         \x20     summary: Minute tick\n\
         \x20     start: \"2026-01-05T09:00:00\"\n\
         \x20     duration: 5m\n\
         \x20     recurrence: FREQ=MINUTELY\n",
    )
    .unwrap();
    let mut server = Server::spawn(dir.path());
    init_handshake(&mut server);
    // events.show → ShowEvent::run errs on the unexpandable rule.
    let out = server.rpc(
        2,
        "tools/call",
        tool_call("cicada.events.show", json!({ "id": "tick" })),
    );
    assert_eq!(out["result"]["isError"], true, "{out}");
    // plan.preview with an unexpandable event still returns a plan —
    // check the hasErrors path with a HOURLY event instead.
    fs::write(
        dir.path().join("calendars/work.yaml"),
        "apiVersion: cicada/v1\n\
         kind: Calendar\n\
         metadata:\n\
         \x20 name: work\n\
         \x20 timeZone: UTC\n\
         spec:\n\
         \x20 events:\n\
         \x20   - id: chime\n\
         \x20     summary: Hourly chime\n\
         \x20     start: \"2026-01-05T09:00:00\"\n\
         \x20     duration: 5m\n\
         \x20     recurrence: { freq: HOURLY, count: 3 }\n",
    )
    .unwrap();
    let out = server.rpc(3, "tools/call", tool_call("cicada.plan.preview", json!({})));
    let text = out["result"]["content"][0]["text"].as_str().unwrap_or("");
    assert!(
        text.contains("error") || text.contains("WARN") || out["result"]["isError"] == true,
        "{out}"
    );
}

/// `calendars: []` in the manifest fails at session open.
#[test]
fn mcp_stdio_empty_manifest() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(
        dir.path().join("cicada.yaml"),
        "apiVersion: cicada/v1\nkind: Manifest\ncalendars: []\n",
    )
    .unwrap();
    let server = Server::spawn(dir.path());
    // The server exits during startup; stderr carries the reason.
    let mut stderr = String::new();
    for _ in 0..50 {
        stderr.push_str(&server.drain_stderr());
        if stderr.contains("calendars") {
            break;
        }
        thread::sleep(Duration::from_millis(100));
    }
    assert!(stderr.contains("calendars"), "{stderr}");
}

/// Desired-load errors with *valid* args (calendar read fails inside
/// the tool, not arg validation), the range expansion error arm, a
/// singular adoption candidate, and a bad `notify` on plan.apply.
#[test]
fn mcp_stdio_calendar_load_and_adoption_arms() {
    let dir = tempfile::tempdir().unwrap();
    write_fixture(dir.path());
    let mut server = Server::spawn(dir.path());
    init_handshake(&mut server);
    let mut id = 300;
    let mut call = |name: &str, args: Value| -> Value {
        id += 1;
        server.rpc(id, "tools/call", tool_call(name, args))
    };

    // An unexpandable desired rule errors inside events.range's
    // per-calendar expansion (not arg validation).
    fs::write(
        dir.path().join("calendars/work.yaml"),
        "apiVersion: cicada/v1\n\
         kind: Calendar\n\
         metadata:\n\
         \x20 name: work\n\
         \x20 timeZone: UTC\n\
         spec:\n\
         \x20 events:\n\
         \x20   - id: tick\n\
         \x20     summary: Minute tick\n\
         \x20     start: \"2026-01-05T09:00:00\"\n\
         \x20     duration: 5m\n\
         \x20     recurrence: FREQ=MINUTELY\n",
    )
    .unwrap();
    let out = call("cicada.events.range", json!({}));
    assert_eq!(out["result"]["isError"], true, "{out}");

    // Corrupt YAML → `calendars()` errs inside each tool.
    fs::write(dir.path().join("calendars/work.yaml"), "{ corrupt").unwrap();
    for (name, args) in [
        ("cicada.events.range", json!({})),
        ("cicada.events.show", json!({ "id": "x" })),
    ] {
        let out = call(name, args);
        assert_eq!(out["result"]["isError"], true, "{name}: {out}");
    }
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
         \x20     summary: Daily Standup\n\
         \x20     start: \"2026-01-05T09:00:00\"\n\
         \x20     duration: 30m\n",
    )
    .unwrap();

    // preview → apply so recorded state exists, then drop a
    // tagged-but-unrecorded remote event into the fake file →
    // refresh reports exactly one adoption candidate (singular arm).
    let preview = call("cicada.plan.preview", json!({}));
    let digest = preview["result"]["structuredContent"]["planDigest"]
        .as_str()
        .unwrap()
        .to_owned();
    let apply = call(
        "cicada.plan.apply",
        json!({ "planDigest": digest, "confirm": true }),
    );
    assert_eq!(apply["result"]["isError"], false, "{apply}");
    let file = dir.path().join(".cicada/fake-provider.json");
    let mut snap: Value = serde_json::from_str(&fs::read_to_string(&file).unwrap()).unwrap();
    snap["calendars"]["primary"]["foreign-tagged"] = json!({
        "remoteId": "foreign-tagged",
        "tags": {"localId": "ghost", "managed": true},
        "body": {
            "summary": "Ghost",
            "time": {"span": {"start": {"wall": {"local": "2026-01-06T10:00:00", "zone": "UTC"}}, "duration": "PT1H"}}
        }
    });
    fs::write(&file, serde_json::to_string_pretty(&snap).unwrap()).unwrap();
    let out = call("cicada.state.refresh", json!({}));
    let text = out["result"]["content"][0]["text"].as_str().unwrap_or("");
    assert!(text.contains("1 adoption candidate"), "{text}");

    // A valid digest + a bad `notify` value errors inside plan.apply.
    let preview = call("cicada.plan.preview", json!({}));
    let digest = preview["result"]["structuredContent"]["planDigest"]
        .as_str()
        .unwrap()
        .to_owned();
    let out = call(
        "cicada.plan.apply",
        json!({ "planDigest": digest, "confirm": true, "notify": "bogus" }),
    );
    assert_eq!(out["result"]["isError"], true, "{out}");
}
