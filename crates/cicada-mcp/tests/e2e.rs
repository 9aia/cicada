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
    stdin: ChildStdin,
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
            stdin,
            lines,
            stderr,
        }
    }

    /// Write one JSON-RPC message.
    fn send(&mut self, msg: &Value) {
        let line = serde_json::to_string(msg).unwrap();
        writeln!(self.stdin, "{line}").unwrap();
        self.stdin.flush().unwrap();
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
