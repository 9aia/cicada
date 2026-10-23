//! e2e smoke — the `cicada-mcp` binary prints its banner and exits 0.
#![allow(clippy::expect_used)]

use std::process::Command;

#[test]
fn mcp_prints_banner() {
    let out = Command::new(env!("CARGO_BIN_EXE_cicada-mcp"))
        .output()
        .expect("spawn cicada-mcp");
    assert!(out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("cicada-mcp"));
}
