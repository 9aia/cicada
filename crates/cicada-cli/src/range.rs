//! `cicada range` — expand desired events into concrete occurrences
//! over a window (`--from`/`--to`, defaulting to the next week).
//! Local view over the YAML only; `--provider` is reserved for the
//! live view and noted when passed.

use std::process::ExitCode;

use cicada_core::application::{QueryRange, RangeOccurrence};
use cicada_core::domain::ids::ZoneId;
use cicada_core::domain::time::TimeRange;

use crate::RangeArgs;
use crate::input;
use crate::timeparse::parse_bound;

/// Default window when neither bound is given.
const DEFAULT_FROM: &str = "today";
const DEFAULT_TO: &str = "+7d";

/// Run the verb.
pub(crate) fn run(args: &RangeArgs) -> ExitCode {
    if args.provider.is_some() {
        eprintln!("note: --provider live views aren't wired yet — showing local desired state");
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
    let files = input::resolve_calendar_files(files)?;
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
