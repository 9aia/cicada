use std::process::{Command, Stdio};

fn run(cmd: &str, args: &[&str]) -> anyhow::Result<()> {
    let status = Command::new(cmd).args(args).status()?;
    anyhow::ensure!(status.success(), "{cmd} {args:?} failed");
    Ok(())
}

/// True when the `cargo-nextest` subcommand is installed. Probed up
/// front so a *failing* nextest run reports its own failure instead of
/// silently re-running the whole suite a second time under `cargo test`.
fn nextest_available() -> bool {
    match Command::new("cargo")
        .args(["nextest", "--version"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
    {
        Ok(status) => status.success(),
        Err(_) => false,
    }
}

fn main() -> anyhow::Result<()> {
    match std::env::args().nth(1).as_deref() {
        Some("check") => {
            // clippy --all-targets type-checks every target already —
            // a separate `cargo check` pass would redo that work.
            run("cargo", &["fmt", "--all", "--check"])?;
            run(
                "cargo",
                &[
                    "clippy",
                    "--workspace",
                    "--all-targets",
                    "--",
                    "-D",
                    "warnings",
                ],
            )?;
        }
        // Prefer nextest when installed, but only fall back to
        // `cargo test` when the subcommand is *missing* — a failing
        // nextest run must not re-run the whole suite a second way.
        Some("test") => {
            let cmd = if nextest_available() {
                // --no-tests=pass: an empty workspace (scaffold phase)
                // is a pass, not an error.
                &["nextest", "run", "--workspace", "--no-tests=pass"][..]
            } else {
                &["test", "--workspace"][..]
            };
            let status = Command::new("cargo").args(cmd).status()?;
            anyhow::ensure!(status.success(), "workspace tests failed");
        }
        Some("install") => {
            // Build every user-facing binary in release and copy it to
            // ~/.local/bin (idempotent, PATH-friendly names).
            let bins = ["cicada", "cicada-mcp"];
            for bin in bins {
                run("cargo", &["build", "--release", "--bin", bin])?;
                let src = format!("target/release/{bin}");
                let dst = format!("{}/.local/bin/{bin}", std::env::var("HOME")?);
                std::fs::copy(&src, &dst)?;
            }
        }
        other => {
            eprintln!("usage: cargo xtask <check|test|install> (got {other:?})");
            std::process::exit(2);
        }
    }
    Ok(())
}
