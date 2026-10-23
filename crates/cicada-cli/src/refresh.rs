//! `cicada refresh` — fold live provider state into recorded state
//! (ADR-0002 §4). Runs under the state file's advisory lock like
//! apply; it's the write that makes drift reconcile.

use std::path::Path;
use std::process::ExitCode;

use cicada_core::application::{RefreshReport, RefreshRequest, RefreshState};
use cicada_core::ports::clock::SystemClock;
use cicada_core::ports::{CalendarRef, StateStore};
use cicada_state::JsonFileStateStore;

use crate::RefreshArgs;
use cicada_shell::backend;

/// Run the verb; exit `FAILURE` on provider/store errors.
pub(crate) fn run(args: &RefreshArgs) -> ExitCode {
    if let Err(e) = backend::provider_name(args.provider.as_deref(), "refresh") {
        eprintln!("{e}");
        return ExitCode::FAILURE;
    }
    let dir = backend::manifest_dir(Path::new("."), &[]);
    let state_path = cicada_state::resolve_state_path(&dir, args.state.as_deref());
    let store = JsonFileStateStore::new(&state_path);
    let provider = match backend::fake_provider(args.fake_file.as_deref(), &dir, "refresh") {
        Ok(p) => p,
        Err(e) => {
            eprintln!("{e}");
            return ExitCode::FAILURE;
        }
    };
    let runtime = match backend::runtime("refresh") {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("{e}");
            return ExitCode::FAILURE;
        }
    };

    let _guard = match store.lock() {
        Ok(g) => g,
        Err(e) => {
            eprintln!("cicada refresh: {e}");
            return ExitCode::FAILURE;
        }
    };
    let state = match runtime.block_on(store.load()) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("cicada refresh: cannot load {}: {e}", state_path.display());
            return ExitCode::FAILURE;
        }
    };
    if state.calendars.is_empty() {
        println!("refresh: no recorded state — nothing applied yet");
        return ExitCode::SUCCESS;
    }
    let clock = SystemClock;

    let mut failed = false;
    for (name, recorded) in &state.calendars {
        let remote = CalendarRef::new(recorded.calendar_id.clone());
        let req = RefreshRequest {
            calendar_name: name,
            provider: &provider,
            remote_calendar: &remote,
            state: &store,
            clock: &clock,
        };
        match runtime.block_on(RefreshState::new().run(&req)) {
            Ok(report) => print_report(&report),
            Err(e) => {
                eprintln!("cicada refresh {name}: {e}");
                failed = true;
            }
        }
    }
    if failed {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}

/// Render one calendar's refresh report.
fn print_report(report: &RefreshReport) {
    if report.reconciled.is_empty() {
        println!(
            "refreshed {}: already in sync ({} records)",
            report.calendar, report.unchanged
        );
    } else {
        println!(
            "refreshed {}: {} updated · {} removed · {} unchanged",
            report.calendar,
            report.updated.len(),
            report.removed.len(),
            report.unchanged
        );
        for id in &report.updated {
            println!("  ~ {id} — record now matches live");
        }
        for id in &report.removed {
            println!("  - {id} — live event gone; record dropped (next plan recreates)");
        }
    }
    for id in &report.adoption_candidates {
        println!(
            "  ? {id} — live event carries cicada tags but isn't recorded (adoption candidate)"
        );
    }
}
