//! The tamias verb front — clap parser + dispatch for
//! `install`/`deploy`/`uninstall`/`status`/`config-schema`, the
//! verbs tamias appends as
//! `<driver> <verb> --dir <d> --name <n> [--config <c>]`.
//! `capabilities` moved to the merged payload in `lib.rs` (it
//! answers both pavo and tamias discovery); the pavo verbs
//! (`manifest`/`serve`/`ops`/`run`/`cli`/`mcp`) never reach this
//! parser — `main.rs` intercepts them first.

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Args, Parser, Subcommand};

use crate::verbs::{self, Invocation};

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
}

/// `ExitCode` → the process-code integer `main` works in.
fn exit_i32(code: ExitCode) -> i32 {
    i32::from(code != ExitCode::SUCCESS)
}

/// Parse `argv` (argv[0] included) and dispatch the tamias verb.
/// Clap errors/help print and exit through clap itself.
pub fn run_from<I, T>(argv: I) -> i32
where
    I: IntoIterator<Item = T>,
    T: Into<std::ffi::OsString> + Clone,
{
    let cli = Cli::parse_from(argv);
    exit_i32(match cli.command {
        Commands::Install(args) => verbs::install(&args.into()),
        Commands::Deploy(args) => verbs::deploy(&args.into()),
        Commands::Uninstall(args) => verbs::uninstall(&args.into()),
        Commands::Status(args) => verbs::status(&args.into()),
        Commands::ConfigSchema(_) => {
            verbs::emit(&verbs::config_schema());
            ExitCode::SUCCESS
        }
    })
}
