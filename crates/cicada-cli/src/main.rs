//! `cicada` — calendars as code. Wires adapters into cicada-core use
//! cases: init, validate, plan, apply, refresh, drift, range, show,
//! fmt, provider add, mcp.

#![forbid(unsafe_code)]

mod apply;
mod drift;
mod logging;
mod plan;
mod range;
mod refresh;
mod show;
mod validate;

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Args, Parser, Subcommand};

/// Declare calendars in YAML; plan, apply, and detect drift against
/// live providers — the way you manage infrastructure.
#[derive(Debug, Parser)]
#[command(
    name = "cicada",
    version,
    about = "calendars as code — declare calendars in YAML, reconcile like infrastructure"
)]
struct Cli {
    /// The verb to run.
    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Debug, Subcommand)]
enum Commands {
    /// Scaffold a cicada.yaml manifest and a starter calendar.
    Init,
    /// Validate calendar YAML: schema shape plus domain invariants.
    ///
    /// With no files, discovers the `cicada.yaml` manifest in the
    /// current directory and validates every calendar it lists. A
    /// `kind: Manifest` file passed explicitly expands the same way.
    Validate(ValidateArgs),
    /// Diff desired vs recorded vs live into an ordered plan.
    Plan(PlanArgs),
    /// Reconcile: execute the plan against the provider.
    Apply(ApplyArgs),
    /// Pull live state into recorded state.
    Refresh(RefreshArgs),
    /// Show recorded-vs-live drift.
    Drift(DriftArgs),
    /// Expand events over a time range (local; --provider for live).
    Range(RangeArgs),
    /// Show one event by stable local id.
    Show(ShowArgs),
    /// Normalize calendar YAML formatting in place.
    Fmt(FmtArgs),
    /// Manage provider registrations (`provider add google`).
    Provider(ProviderArgs),
    /// Serve MCP over stdio — the same core, for agents (ADR-0001 §8a).
    Mcp(cicada_mcp::ServerOptions),
}

/// `cicada validate [files...]`.
#[derive(Debug, Args)]
struct ValidateArgs {
    /// Calendar or manifest YAML files. Defaults to the `cicada.yaml`
    /// manifest in the current directory.
    files: Vec<PathBuf>,
}

/// `cicada plan`.
#[derive(Debug, Args)]
struct PlanArgs {
    /// Calendar or manifest YAML files. Defaults to the `cicada.yaml`
    /// manifest in the current directory.
    files: Vec<PathBuf>,
    /// Provider adapter to plan against (fake | google).
    #[arg(long)]
    provider: Option<String>,
    /// Fake provider backend file (test/demo only; default
    /// `.cicada/fake-provider.json` beside the manifest).
    #[arg(long)]
    fake_file: Option<PathBuf>,
    /// Recorded-state file (default `.cicada/state.yaml` beside the
    /// manifest; `CICADA_STATE` also overrides).
    #[arg(long)]
    state: Option<PathBuf>,
    /// Write the plan JSON here (`cicada/plan-v1`); a JSON array when
    /// more than one calendar is planned.
    #[arg(long)]
    out: Option<PathBuf>,
}

/// `cicada apply`.
#[derive(Debug, Args)]
struct ApplyArgs {
    /// Calendar or manifest YAML files. Defaults to the `cicada.yaml`
    /// manifest in the current directory.
    files: Vec<PathBuf>,
    /// Provider adapter to apply against (fake | google).
    #[arg(long)]
    provider: Option<String>,
    /// Fake provider backend file (test/demo only; default
    /// `.cicada/fake-provider.json` beside the manifest).
    #[arg(long)]
    fake_file: Option<PathBuf>,
    /// Recorded-state file (default `.cicada/state.yaml`; also
    /// `CICADA_STATE`).
    #[arg(long)]
    state: Option<PathBuf>,
    /// Skip the confirmation prompt.
    #[arg(long)]
    auto_approve: bool,
    /// Resume after a crash — skip already-journaled ops.
    #[arg(long)]
    resume: bool,
    /// Overwrite out-of-band edits instead of failing on drift
    /// conflict (ADR-0002 §4).
    #[arg(long)]
    force: bool,
    /// Attendee notifications: none | externalOnly | all.
    #[arg(long)]
    notify: Option<String>,
}

/// `cicada refresh`.
#[derive(Debug, Args)]
struct RefreshArgs {
    /// Provider adapter to refresh from (fake | google).
    #[arg(long)]
    provider: Option<String>,
    /// Fake provider backend file (test/demo only).
    #[arg(long)]
    fake_file: Option<PathBuf>,
    /// Recorded-state file (default `.cicada/state.yaml`; also
    /// `CICADA_STATE`).
    #[arg(long)]
    state: Option<PathBuf>,
}

/// `cicada drift`.
#[derive(Debug, Args)]
struct DriftArgs {
    /// Provider adapter to check against (fake | google).
    #[arg(long)]
    provider: Option<String>,
    /// Fake provider backend file (test/demo only).
    #[arg(long)]
    fake_file: Option<PathBuf>,
    /// Recorded-state file (default `.cicada/state.yaml`; also
    /// `CICADA_STATE`).
    #[arg(long)]
    state: Option<PathBuf>,
}

/// `cicada range`.
#[derive(Debug, Args)]
struct RangeArgs {
    /// Calendar or manifest YAML files. Defaults to the `cicada.yaml`
    /// manifest in the current directory.
    files: Vec<PathBuf>,
    /// Range start (`today`, `+Nd`, a date, or a datetime).
    #[arg(long)]
    from: Option<String>,
    /// Range end (`+7d`, a date, or a datetime).
    #[arg(long)]
    to: Option<String>,
    /// Expand the live provider's rules instead of local YAML
    /// (reserved — not wired yet).
    #[arg(long)]
    provider: Option<String>,
}

/// `cicada show <id>`.
#[derive(Debug, Args)]
struct ShowArgs {
    /// Stable local id of the event.
    id: String,
    /// Calendar or manifest YAML files. Defaults to the `cicada.yaml`
    /// manifest in the current directory.
    files: Vec<PathBuf>,
    /// Occurrence window start (`today`, `+Nd`, a date, or a
    /// datetime). With `--to`, defaults to `today`.
    #[arg(long)]
    from: Option<String>,
    /// Occurrence window end (defaults to the window start + 30d).
    #[arg(long)]
    to: Option<String>,
}

/// `cicada fmt`.
#[derive(Debug, Args)]
struct FmtArgs {
    /// Files to format; defaults to the manifest's calendars.
    files: Vec<PathBuf>,
}

/// `cicada provider`.
#[derive(Debug, Args)]
struct ProviderArgs {
    /// What to do with providers.
    #[command(subcommand)]
    command: ProviderCommands,
}

#[derive(Debug, Subcommand)]
enum ProviderCommands {
    /// Register a provider (OAuth flow / service account).
    Add {
        /// Provider name (`google`).
        name: String,
    },
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match cli.command {
        None => {
            println!(
                "cicada — calendars as code. Declare calendars in YAML; plan, \
                 apply, and detect drift.\nRun `cicada --help` for usage."
            );
            ExitCode::SUCCESS
        }
        Some(Commands::Validate(args)) => validate::run(&args.files),
        Some(Commands::Plan(args)) => plan::run(&args),
        Some(Commands::Range(args)) => range::run(&args),
        Some(Commands::Show(args)) => show::run(&args),
        Some(Commands::Apply(args)) => apply::run(&args),
        Some(Commands::Refresh(args)) => refresh::run(&args),
        Some(Commands::Drift(args)) => drift::run(&args),
        Some(Commands::Init) => not_implemented("init"),
        Some(Commands::Fmt(_)) => not_implemented("fmt"),
        Some(Commands::Provider(_)) => not_implemented("provider"),
        Some(Commands::Mcp(opts)) => cicada_mcp::run_stdio(&opts),
    }
}

/// Stub for verbs whose use cases land in later build steps.
fn not_implemented(verb: &str) -> ExitCode {
    eprintln!("cicada {verb}: not yet implemented");
    ExitCode::FAILURE
}
