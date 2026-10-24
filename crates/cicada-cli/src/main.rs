//! `cicada` — calendars as code. Wires adapters into cicada-core use
//! cases: init, validate, plan, apply, refresh, drift, range, show,
//! fmt, provider add, mcp.

#![forbid(unsafe_code)]

mod apply;
mod drift;
mod fmt;
mod init;
mod logging;
mod plan;
mod provider;
mod range;
mod refresh;
mod show;
mod validate;

use std::path::PathBuf;
use std::process::ExitCode;

use cicada_shell::backend;
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
    Init(InitArgs),
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

/// `cicada init`.
#[derive(Debug, Args)]
struct InitArgs {
    /// Directory to scaffold into (default: current directory).
    dir: Option<PathBuf>,
    /// Overwrite existing `cicada.yaml` / `calendars/work.yaml`.
    #[arg(long)]
    force: bool,
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
    /// Expand the live provider's events instead of local YAML
    /// (fake | google).
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
    /// Show the live provider's version of the event (found via its
    /// `cicada:id` tag) instead of the declared YAML.
    #[arg(long)]
    provider: Option<String>,
}

/// `cicada fmt`.
#[derive(Debug, Args)]
struct FmtArgs {
    /// Files to format; defaults to the manifest's calendars.
    files: Vec<PathBuf>,
    /// Check only — exit nonzero when a file isn't canonical; writes
    /// nothing (CI-friendly).
    #[arg(long)]
    check: bool,
    /// Sort events by stable id instead of preserving file order.
    #[arg(long)]
    sort: bool,
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
        /// Register a service-account key JSON instead of running the
        /// OAuth flow (non-interactive).
        #[arg(long, value_name = "KEY_JSON")]
        service_account: Option<PathBuf>,
        /// OAuth installed-app client id (else
        /// `CICADA_GOOGLE_CLIENT_ID`).
        #[arg(long)]
        client_id: Option<String>,
        /// OAuth installed-app client secret (else
        /// `CICADA_GOOGLE_CLIENT_SECRET`).
        #[arg(long)]
        client_secret: Option<String>,
        /// Authorization code — skips the interactive prompt.
        #[arg(long)]
        code: Option<String>,
    },
}

/// Print `msg` to stderr and exit nonzero — for failures a caller
/// can't recover from anyway (a tokio runtime that won't build).
/// `!` lets call sites keep the value they were unwrapping.
/// The call is unreachable in tests — compiled out under coverage.
#[cfg(not(coverage))]
fn die(msg: &str) -> ! {
    eprintln!("{msg}");
    std::process::exit(1)
}

/// The verb's tokio runtime — `die` on a build failure, which can't
/// happen in tests, so under coverage the unwrap is unconditional
/// (keeps `Result::unwrap` — and `die` itself — out of the report).
#[cfg(not(coverage))]
pub(crate) fn runtime_or_die(who: &str) -> tokio::runtime::Runtime {
    backend::runtime(who).unwrap_or_else(|e| crate::die(&e))
}

/// Coverage variant — see above.
#[cfg(coverage)]
pub(crate) fn runtime_or_die(who: &str) -> tokio::runtime::Runtime {
    backend::runtime(who).expect("tokio runtime builds")
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
        Some(Commands::Init(args)) => init::run(&args),
        Some(Commands::Fmt(args)) => fmt::run(&args),
        Some(Commands::Provider(args)) => provider::run(&args.command),
        Some(Commands::Mcp(opts)) => cicada_mcp::run_stdio(&opts),
    }
}
