//! e2e smoke — `tamias-driver-cicada run` (the engine passthrough
//! the `cicada` bins[] shim rides) prints the banner and exits 0.
#![allow(clippy::expect_used)]

use std::process::Command;

/// `tamias-driver-cicada run` — the engine passthrough the `cicada`
/// bins[] shim uses; argv[0] is the driver's own, `run` hands the
/// rest to `cicada::run_from` exactly like the `cli` shim verb does
/// after stripping `--dir`/`--name`.
fn cicada_cmd() -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_tamias-driver-cicada"));
    c.arg("run");
    c
}

#[test]
fn cli_prints_banner() {
    let out = cicada_cmd().output().expect("spawn cicada");
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("calendars as code"));
}
