//! `cicada list` — the declared calendars with their bindings and
//! recorded-state counts (the read `cicada.calendar.list` MCP tool
//! used to serve; T-019 moved the whole surface engine-side).
//!
//! Read-only: calendars come from YAML, recorded counts from the
//! state file — no provider calls.

use std::path::Path;
use std::process::ExitCode;

use cicada_core::ports::StateStore;
use cicada_state::JsonFileStateStore;

use crate::ListArgs;
use cicada_shell::backend;
use cicada_shell::input;

/// Run the verb.
pub(crate) fn run(args: &ListArgs) -> ExitCode {
    let provider_name = match backend::provider_name(args.provider.as_deref(), "list") {
        Ok(n) => n,
        Err(e) => {
            eprintln!("{e}");
            return ExitCode::FAILURE;
        }
    };
    let files = match input::resolve_calendar_files(Path::new("."), &args.files) {
        Ok(f) if f.is_empty() => {
            eprintln!("cicada list: no calendar files found");
            return ExitCode::FAILURE;
        }
        Ok(f) => f,
        Err(e) => {
            eprintln!("cicada list: {e}");
            return ExitCode::FAILURE;
        }
    };
    let calendars = match input::load_calendars(&files) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("cicada list: {e}");
            return ExitCode::FAILURE;
        }
    };
    let dir = backend::manifest_dir(Path::new("."), &args.files);
    let state_path = cicada_state::resolve_state_path(&dir, args.state.as_deref());
    let store = JsonFileStateStore::new(&state_path);
    let runtime = crate::runtime_or_die("list");
    let state = match runtime.block_on(StateStore::load(&store)) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("cicada list: cannot load {}: {e}", state_path.display());
            return ExitCode::FAILURE;
        }
    };

    println!("{} calendar(s):", calendars.len());
    for cal in &calendars {
        let recorded = state.calendar(&cal.name);
        let remote = backend::remote_calendar(cal, recorded);
        println!(
            "  {} — {} (provider: {} · calendar: {} · {} declared · {} recorded)",
            cal.name,
            cal.time_zone.as_str(),
            cal.provider.as_deref().unwrap_or(provider_name.as_str()),
            remote.as_str(),
            cal.len(),
            recorded.map_or(0, |r| r.events.len()),
        );
    }
    ExitCode::SUCCESS
}
