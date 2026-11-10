//! `tamias-driver-cicada` — the tamias content **driver** for
//! `type: cicada` packages: a cicada project payload (`cicada.yaml`
//! manifest + `calendars/*.yaml`) installed through tamias.
//!
//! tamias discovers this binary as `tamias-driver-cicada` on `PATH` /
//! `<tamias-home>/drivers/` and dispatches
//! `<driver> <verb> --dir <d> --name <n> [--config <c>]`:
//!
//! - `capabilities` — JSON `{capabilities, verbs, types: ["cicada"]}`
//!   for tamias's kind routing + verb warnings.
//! - `install` — validate the payload (manifest + calendar docs
//!   through cicada-schema/cicada-core); JSON report, no provider
//!   calls, nothing applied.
//! - `deploy` — refresh the operator config (`driver.json`) and run a
//!   **plan** (desired vs recorded vs live) via the configured
//!   provider; `plan.json` lands under the resolved state dir.
//!   Plan-only — the apply confirmation gate stays with the operator.
//! - `uninstall` — drift/managed-inventory JSON report; deletes no
//!   remote events (managed-boundary safe default).
//! - `status` — recorded-state summary; no provider calls.
//! - `config-schema` — JSON Schema (draft 2020-12) of the optional
//!   `--config` file (see `config.rs`).
//!
//! The `--config` file is YAML (`provider`, `stateDir`, `fakeFile`);
//! `provider` defaults to `google` — `fake` is the credential-free
//! test/demo backend. See `verbs.rs` for the verb implementations and
//! `config.rs` for the config format.

#![forbid(unsafe_code)]

mod config;
mod verbs;

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Args, Parser, Subcommand};
use verbs::Invocation;

/// The tamias content driver for `type: cicada` packages — declares,
/// plans, and reports on cicada calendar payloads.
#[derive(Debug, Parser)]
#[command(
    name = "tamias-driver-cicada",
    version,
    about = "tamias content driver for cicada calendar packages"
)]
struct Cli {
    /// The verb to run.
    #[command(subcommand)]
    command: Commands,
}

/// The flags tamias appends to every dispatched verb.
#[derive(Debug, Args)]
struct VerbArgs {
    /// The installed payload dir (`tamias.yaml` + `cicada.yaml` +
    /// `calendars/`).
    #[arg(long)]
    dir: PathBuf,
    /// The installed package name.
    #[arg(long)]
    name: String,
    /// Optional operator config file (YAML; see `config-schema`).
    #[arg(long)]
    config: Option<PathBuf>,
}

impl From<VerbArgs> for Invocation {
    fn from(args: VerbArgs) -> Self {
        Self {
            dir: args.dir,
            name: args.name,
            config: args.config,
        }
    }
}

#[derive(Debug, Subcommand)]
enum Commands {
    /// Validate the payload — manifest + calendar docs through
    /// cicada-schema/cicada-core. JSON report; nothing applied.
    Install(VerbArgs),
    /// Refresh the operator config and run a plan (desired vs
    /// recorded vs live). Plan-only — never applies.
    Deploy(VerbArgs),
    /// Managed-inventory + drift report as JSON. Deletes no remote
    /// events.
    Uninstall(VerbArgs),
    /// Recorded-state summary — no provider calls.
    Status(VerbArgs),
    /// Print the JSON Schema (draft 2020-12) of the `--config` file —
    /// tamias's reserved `config-schema` verb.
    ConfigSchema(VerbArgs),
    /// Report the content types + verbs this driver handles (JSON).
    Capabilities,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match cli.command {
        Commands::Capabilities => {
            verbs::emit(&verbs::capabilities());
            ExitCode::SUCCESS
        }
        Commands::ConfigSchema(_) => {
            verbs::emit(&verbs::config_schema());
            ExitCode::SUCCESS
        }
        Commands::Install(args) => verbs::install(&args.into()),
        Commands::Deploy(args) => verbs::deploy(&args.into()),
        Commands::Uninstall(args) => verbs::uninstall(&args.into()),
        Commands::Status(args) => verbs::status(&args.into()),
    }
}
