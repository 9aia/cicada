//! `cicada range` — expand desired events into concrete occurrences
//! over a window (`--from`/`--to`, defaulting to the next week).
//! The default view is local desired state; `--provider` lists the
//! remote calendar and expands what is actually scheduled
//! ([`QueryRange::expand_remote`]).

use std::path::Path;
use std::process::ExitCode;

use cicada_core::application::{QueryRange, RangeOccurrence};
use cicada_core::domain::ids::ZoneId;
use cicada_core::domain::time::TimeRange;
use cicada_core::ports::{CalendarProvider, StateStore};
use cicada_state::JsonFileStateStore;

use crate::RangeArgs;
use cicada_shell::backend;
use cicada_shell::input;
use cicada_shell::timeparse::parse_bound;

/// Default window when neither bound is given.
const DEFAULT_FROM: &str = "today";
const DEFAULT_TO: &str = "+7d";

/// Run the verb.
pub(crate) fn run(args: &RangeArgs) -> ExitCode {
    if args.provider.is_some() {
        return run_live(args);
    }
    let (calendars, zone) = match load_and_zone(&args.files) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("cicada range: {e}");
            return ExitCode::FAILURE;
        }
    };
    let window = match window(args.from.as_deref(), args.to.as_deref(), &zone) {
        Ok(w) => w,
        Err(e) => {
            eprintln!("cicada range: {e}");
            return ExitCode::FAILURE;
        }
    };

    let mut failed = false;
    for cal in &calendars {
        println!("{}:", cal.name);
        match QueryRange::new().expand(cal.events.values(), window) {
            Ok(occs) if occs.is_empty() => println!("  (no occurrences in range)"),
            Ok(occs) => {
                for occ in &occs {
                    println!("  {}", occurrence_line(occ));
                }
            }
            Err(e) => {
                eprintln!("cicada range {}: {e}", cal.name);
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

/// Load all declared calendars; the window's zone is the first
/// calendar's `timeZone` (multi-calendar manifests share it in
/// practice).
fn load_and_zone(
    files: &[std::path::PathBuf],
) -> Result<(Vec<cicada_core::domain::calendar::Calendar>, ZoneId), String> {
    let files = input::resolve_calendar_files(Path::new("."), files)?;
    let calendars = input::load_calendars(&files)?;
    let zone = calendars
        .first()
        .map_or_else(|| ZoneId::new_unchecked("UTC"), |c| c.time_zone.clone());
    Ok((calendars, zone))
}

/// `--from`/`--to` with defaults → a validated [`TimeRange`].
fn window(from: Option<&str>, to: Option<&str>, zone: &ZoneId) -> Result<TimeRange, String> {
    let start = parse_bound(from.unwrap_or(DEFAULT_FROM), zone)?;
    let end = parse_bound(to.unwrap_or(DEFAULT_TO), zone)?;
    TimeRange::new(start, end).map_err(|e| e.to_string())
}

/// `range --provider <name>` — the live view: list each calendar's
/// remote binding in the window and expand remote rules
/// (unmanaged events included — this is what is actually
/// scheduled).
fn run_live(args: &RangeArgs) -> ExitCode {
    let provider_name = match backend::provider_name(args.provider.as_deref(), "range") {
        Ok(n) => n,
        Err(e) => {
            eprintln!("{e}");
            return ExitCode::FAILURE;
        }
    };
    let (calendars, zone) = match load_and_zone(&args.files) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("cicada range: {e}");
            return ExitCode::FAILURE;
        }
    };
    let window = match window(args.from.as_deref(), args.to.as_deref(), &zone) {
        Ok(w) => w,
        Err(e) => {
            eprintln!("cicada range: {e}");
            return ExitCode::FAILURE;
        }
    };
    let dir = backend::manifest_dir(Path::new("."), &args.files);
    let provider = match backend::provider(&provider_name, None, &dir, "range") {
        Ok(p) => p,
        Err(e) => {
            eprintln!("{e}");
            return ExitCode::FAILURE;
        }
    };
    let runtime = match backend::runtime("range") {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("{e}");
            return ExitCode::FAILURE;
        }
    };
    // Recorded state only feeds the remote-calendar binding; absence
    // means `metadata.calendarId`/`primary`.
    let store = JsonFileStateStore::new(cicada_state::resolve_state_path(&dir, None));
    let state = runtime
        .block_on(store.load())
        .unwrap_or_else(|_| cicada_core::domain::state::RecordedState::new());

    let mut failed = false;
    for cal in &calendars {
        let remote = backend::remote_calendar(cal, state.calendar(&cal.name));
        let occurrences = runtime
            .block_on(provider.list_events(&remote, Some(window)))
            .and_then(|live| QueryRange::new().expand_remote(&live, window));
        println!("{} (live @ {remote}):", cal.name);
        match occurrences {
            Ok(occs) if occs.is_empty() => println!("  (no occurrences in range)"),
            Ok(occs) => {
                for occ in &occs {
                    println!("  {}", occurrence_line(occ));
                }
            }
            Err(e) => {
                eprintln!("cicada range {}: {e}", cal.name);
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

/// `2026-01-05 09:00 [America/New_York] → 09:30 · standup — Daily Standup`.
pub(crate) fn occurrence_line(occ: &RangeOccurrence) -> String {
    let zone = occ.zone.as_ref().map_or("UTC", |z| z.as_str());
    let end = if occ.local_end.date() == occ.local_start.date() {
        occ.local_end.strftime("%H:%M").to_string()
    } else {
        occ.local_end.strftime("%Y-%m-%d %H:%M").to_string()
    };
    let id = occ
        .local_id
        .as_ref()
        .map(cicada_core::domain::ids::LocalId::as_str)
        .or_else(|| {
            occ.remote_id
                .as_ref()
                .map(cicada_core::domain::ids::RemoteId::as_str)
        })
        .unwrap_or("?");
    format!(
        "{} [{}] → {} · {} — {}",
        occ.local_start.strftime("%Y-%m-%d %H:%M"),
        zone,
        end,
        id,
        occ.summary
    )
}
