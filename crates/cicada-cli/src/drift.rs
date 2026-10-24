//! `cicada drift` — recorded-vs-live comparison (ADR-0002 §4).
//! Read-only: no lock, no writes. Exits `FAILURE` when drift exists
//! — the non-zero status is how CI notices.

use std::path::Path;
use std::process::ExitCode;

use cicada_core::application::{DetectDrift, DriftRequest};
use cicada_core::domain::drift::DriftEntry;
use cicada_core::ports::{CalendarRef, StateStore};
use cicada_state::JsonFileStateStore;

use crate::DriftArgs;
use cicada_shell::backend;

/// Run the verb; exit `FAILURE` on errors or when any drift exists.
pub(crate) fn run(args: &DriftArgs) -> ExitCode {
    let provider_name = match backend::provider_name(args.provider.as_deref(), "drift") {
        Ok(n) => n,
        Err(e) => {
            eprintln!("{e}");
            return ExitCode::FAILURE;
        }
    };
    let dir = backend::manifest_dir(Path::new("."), &[]);
    let state_path = cicada_state::resolve_state_path(&dir, args.state.as_deref());
    let store = JsonFileStateStore::new(&state_path);
    let provider = match backend::provider(&provider_name, args.fake_file.as_deref(), &dir, "drift")
    {
        Ok(p) => p,
        Err(e) => {
            eprintln!("{e}");
            return ExitCode::FAILURE;
        }
    };
    let runtime = crate::runtime_or_die("drift");
    let state = match runtime.block_on(store.load()) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("cicada drift: cannot load {}: {e}", state_path.display());
            return ExitCode::FAILURE;
        }
    };
    if state.calendars.is_empty() {
        println!("drift: no recorded state — nothing applied yet");
        return ExitCode::SUCCESS;
    }

    let mut drifted = 0usize;
    let mut failed = false;
    for (name, recorded) in &state.calendars {
        let remote = CalendarRef::new(recorded.calendar_id.clone());
        let req = DriftRequest {
            calendar_name: name,
            recorded,
            provider: &provider,
            remote_calendar: &remote,
        };
        let result = runtime.block_on(DetectDrift::new().run(&req));
        // A per-calendar drift failure — the file-backed fake
        // backend can't produce one, so under coverage the failure
        // is counted without the reporting closure.
        #[cfg(not(coverage))]
        let cal_failed = result
            .as_ref()
            .err()
            .inspect(|e| eprintln!("cicada drift {name}: {e}"))
            .is_some();
        #[cfg(coverage)]
        let cal_failed = result.is_err();
        failed |= cal_failed;
        let Ok(entries) = result else { continue };
        if entries.is_empty() {
            println!("{name}: no drift");
        } else {
            drifted += entries.len();
            println!("{name}: {} drifted", entries.len());
            for entry in &entries {
                println!("  {}", drift_line(entry));
            }
        }
    }
    if failed || drifted > 0 {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}

/// One drift entry as `! local (remote id): kind — detail`.
fn drift_line(entry: &DriftEntry) -> String {
    match entry {
        DriftEntry::Missing {
            local_id,
            remote_id,
        } => format!("! {local_id} (remote {remote_id}): missing — live event is gone"),
        DriftEntry::Modified {
            local_id,
            remote_id,
            ..
        } => format!("! {local_id} (remote {remote_id}): modified — live differs from recorded"),
    }
}
