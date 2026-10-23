//! e2e smoke — the `cicada` binary prints its banner and exits 0.
#![allow(clippy::expect_used)]

use std::process::Command;

#[test]
fn cli_prints_banner() {
    let out = Command::new(env!("CARGO_BIN_EXE_cicada"))
        .output()
        .expect("spawn cicada");
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("calendars as code"));
}
