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
        Some("pack") => pack()?,
        Some("install") => {
            // Build the remaining user-facing binaries in release and
            // copy them to ~/.local/bin (idempotent, PATH-friendly
            // names). There is no `cicada` cargo bin — the command
            // ships as the tamias pack (`cargo xtask pack` + `tamias
            // install ./libraries/cicada`); `tamias-driver-cicada`
            // serves both the `type: cicada` content verbs and the
            // `cli` bins[] shim passthrough (plus the `mcp`
            // ops→tools surface — cicada-mcp was deleted in T-019).
            let bins = ["tamias-driver-cicada"];
            for bin in bins {
                run("cargo", &["build", "--release", "--bin", bin])?;
                let src = format!("{}/release/{bin}", target_dir()?.display());
                let dst = format!("{}/.local/bin/{bin}", std::env::var("HOME")?);
                std::fs::copy(&src, &dst)?;
            }
            println!("installed tamias-driver-cicada to ~/.local/bin");
            println!("install the cicada command (generated bins[] shim):");
            println!("  cargo xtask pack && tamias install --allow-drivers ./libraries/cicada");
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
            eprintln!("usage: cargo xtask <check|test|pack|install|coverage> (got {other:?})");
            std::process::exit(2);
        }
    }
    Ok(())
}

/// Captured stdout of a command — for probes like `rustc -vV`.
fn run_out(args: &[&str]) -> Option<String> {
    let (prog, rest) = args.split_first()?;
    let out = Command::new(prog).args(rest).output().ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

/// The effective target dir — `cargo metadata` reports it, which
/// keeps `pack`/`install` correct under a shared CARGO_TARGET_DIR
/// (`target/release` relative to the workspace is stale).
fn target_dir() -> anyhow::Result<std::path::PathBuf> {
    let out = Command::new("cargo")
        .args(["metadata", "--no-deps", "--format-version", "1"])
        .output()?;
    let meta: serde_json::Value = serde_json::from_slice(&out.stdout)?;
    meta.get("target_directory")
        .and_then(|v| v.as_str())
        .map(std::path::PathBuf::from)
        .ok_or_else(|| anyhow::anyhow!("cargo metadata: no target_directory"))
}

/// The host target triple — the `drivers/bin/<triple>/` selector
/// tamias uses for compiled driver payloads.
fn host_triple() -> String {
    run_out(&["rustc", "-vV"])
        .and_then(|v| {
            v.lines()
                .find_map(|l| l.strip_prefix("host: ").map(str::to_string))
        })
        .unwrap_or_else(|| "x86_64-unknown-linux-gnu".to_string())
}

/// `cargo xtask pack` — stage the tamias pack payload (T-012):
/// release-build `tamias-driver-cicada` and drop it under
/// `libraries/cicada/drivers/bin/<host-triple>/` — the
/// bundled-driver dir `tamias install` registers into
/// `<tamias-home>/drivers/`. Gitignored on purpose: the binary is a
/// build artifact; the committed pack is just `tamias.yaml` + this
/// staging verb.
fn pack() -> anyhow::Result<()> {
    run("cargo", &["build", "--release", "-p", "cicada-driver"])?;
    let triple = host_triple();
    let dest_dir = std::path::PathBuf::from("libraries/cicada/drivers/bin").join(&triple);
    std::fs::create_dir_all(&dest_dir)?;
    let src = target_dir()?.join("release/tamias-driver-cicada");
    let dest = dest_dir.join("tamias-driver-cicada");
    std::fs::copy(&src, &dest)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&dest, std::fs::Permissions::from_mode(0o755));
    }
    println!("pack staged: {}", dest.display());
    println!();
    println!("install the cicada command (generated bins[] shim):");
    println!("  tamias install --allow-drivers ./libraries/cicada");
    println!("  # or a pinned tarball: tamias bundle pack libraries/cicada");
    Ok(())
}
