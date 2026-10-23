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

/// True when the `cargo-llvm-cov` subcommand is installed.
fn llvm_cov_available() -> bool {
    Command::new("cargo")
        .args(["llvm-cov", "--version"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
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
        // `cargo xtask coverage [llvm-cov args…]` — HTML report under
        // target/llvm-cov/html plus a printed summary. Extra args pass
        // through to cargo-llvm-cov (e.g. `--fail-under-lines 100`,
        // `--lcov --output-path lcov.info` for CI upload); passing any
        // report flag replaces the default --html.
        Some("coverage") => {
            anyhow::ensure!(
                llvm_cov_available(),
                "cargo-llvm-cov not installed — `cargo install cargo-llvm-cov` \
                 (needs the llvm-tools rustup component)"
            );
            // Same runner as `xtask test` so coverage sees the same
            // test selection/isolation (nextest partitions, its
            // .config/nextest.toml filters).
            let extra: Vec<String> = std::env::args().skip(2).collect();
            let wants_report = extra.iter().any(|a| {
                matches!(
                    a.as_str(),
                    "--text"
                        | "--summary-only"
                        | "--json"
                        | "--lcov"
                        | "--cobertura"
                        | "--codecov"
                        | "--open"
                )
            });
            let mut args: Vec<String> = vec!["llvm-cov".into()];
            if nextest_available() {
                args.push("nextest".into());
                // An empty suite (scaffold phase) is a pass, matching
                // `xtask test` — nextest exits 4 on "no tests to run".
                args.push("--no-tests=pass".into());
            }
            args.push("--workspace".into());
            args.push("--exclude".into());
            args.push("xtask".into());
            if !wants_report {
                args.push("--html".into());
            }
            args.extend(extra);
            let status = Command::new("cargo").args(&args).status()?;
            anyhow::ensure!(status.success(), "cargo {} failed", args.join(" "));
        }
        other => {
            eprintln!("usage: cargo xtask <check|test|install|coverage> (got {other:?})");
            std::process::exit(2);
        }
    }
    Ok(())
}
