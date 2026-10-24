//! `cicada show <id>` — one event's declared detail plus its
//! expanded occurrences in a window (default: the next 30 days).

use std::path::Path;
use std::process::ExitCode;

use cicada_core::application::{QueryRange, ShowEvent, default_window};
use cicada_core::domain::event::{Event, EventTime};
use cicada_core::domain::ids::{LocalId, ZoneId};
use cicada_core::domain::time::{Time, TimeRange};
use cicada_core::ports::{CalendarProvider, StateStore};
use cicada_state::JsonFileStateStore;

use crate::ShowArgs;
use crate::range::occurrence_line;
use cicada_shell::backend;
use cicada_shell::input;
use cicada_shell::timeparse::parse_bound;

/// Default window: today + 30 days.
const WINDOW_DAYS: i64 = 30;

/// `show` lists at most this many occurrences (`range` prints all).
const OCCURRENCE_CAP: usize = 20;

/// Run the verb.
pub(crate) fn run(args: &ShowArgs) -> ExitCode {
    if args.provider.is_some() {
        return run_live(args);
    }
    let files = match input::resolve_calendar_files(Path::new("."), &args.files) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("cicada show: {e}");
            return ExitCode::FAILURE;
        }
    };
    let calendars = match input::load_calendars(&files) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("cicada show: {e}");
            return ExitCode::FAILURE;
        }
    };
    let id = LocalId::new(args.id.as_str());
    let found = calendars
        .iter()
        .find_map(|cal| cal.get(&id).map(|e| (cal, e)));
    let Some((cal, event)) = found else {
        eprintln!("cicada show: no event {id:?} in the declared calendars");
        return ExitCode::FAILURE;
    };
    let window = match window(args, &cal.time_zone) {
        Ok(w) => w,
        Err(e) => {
            eprintln!("cicada show: {e}");
            return ExitCode::FAILURE;
        }
    };
    match ShowEvent::new().run(event, window) {
        Ok(detail) => {
            print_detail(cal.name.as_str(), &detail.event, &detail.occurrences);
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("cicada show {id}: {e}");
            ExitCode::FAILURE
        }
    }
}

/// `--from`/`--to` or the default 30-day window from now.
fn window(args: &ShowArgs, zone: &ZoneId) -> Result<TimeRange, String> {
    match (args.from.as_deref(), args.to.as_deref()) {
        (None, None) => {
            // `now + 30d` can't overflow — the map_err arm is
            // compiled out under coverage.
            #[cfg(not(coverage))]
            return default_window(jiff::Timestamp::now(), WINDOW_DAYS).map_err(|e| e.to_string());
            #[cfg(coverage)]
            return Ok(default_window(jiff::Timestamp::now(), WINDOW_DAYS)
                .expect("default window cannot overflow"));
        }
        (f, t) => {
            let start = parse_bound(f.unwrap_or("today"), zone)?;
            let end = parse_bound(t.unwrap_or("+30d"), zone)?;
            TimeRange::new(start, end).map_err(|e| e.to_string())
        }
    }
}

/// Header lines + occurrence list (capped — `range` is for full
/// listings).
fn print_detail(
    calendar: &str,
    event: &Event,
    occurrences: &[cicada_core::application::RangeOccurrence],
) {
    println!("{} — \"{}\" (calendar {calendar})", event.id, event.summary);
    println!("  when:    {}", render_event_time(&event.time));
    if let Some(rule) = &event.recurrence {
        println!("  repeat:  {}", rule.describe());
    }
    if let Some(d) = &event.description {
        println!("  desc:    {d}");
    }
    if let Some(l) = &event.location {
        println!("  where:   {l}");
    }
    if !event.attendees.is_empty() {
        let who = event
            .attendees
            .iter()
            .map(|a| {
                if a.required {
                    a.email.clone()
                } else {
                    format!("{} (optional)", a.email)
                }
            })
            .collect::<Vec<_>>()
            .join(", ");
        println!("  who:     {who}");
    }
    if !event.reminders.is_empty() {
        let what = event
            .reminders
            .iter()
            .map(|r| format!("{} {:?}", r.kind, r.offset))
            .collect::<Vec<_>>()
            .join(", ");
        println!("  remind:  {what}");
    }
    if let Some(c) = &event.conferencing {
        println!("  conf:    {c}");
    }
    println!("  busy:    {}", event.transparency);
    println!("occurrences:");
    for occ in occurrences.iter().take(OCCURRENCE_CAP) {
        println!("  {}", occurrence_line(occ));
    }
    if occurrences.len() > OCCURRENCE_CAP {
        println!(
            "  … ({} more — use `cicada range` for full listings)",
            occurrences.len() - OCCURRENCE_CAP
        );
    }
}

/// `2026-01-05T09:00:00 [UTC] +30m` or `start → end`.
fn render_event_time(time: &EventTime) -> String {
    let start = match time.start() {
        Time::Wall { local, zone } => format!("{local} [{zone}]"),
        Time::Instant(ts) => ts.to_string(),
    };
    match time {
        EventTime::Span { duration, .. } => format!("{start} +{duration:?}"),
        EventTime::Range { end, .. } => match end {
            Time::Wall { local, zone } => format!("{start} → {local} [{zone}]"),
            Time::Instant(ts) => format!("{start} → {ts}"),
        },
    }
}

/// `show --provider <name>` — the live view: resolve the event by
/// its `cicada:id` tag on each declared calendar's remote binding
/// (first hit wins), then expand its remote rule.
fn run_live(args: &ShowArgs) -> ExitCode {
    let provider_name = match backend::provider_name(args.provider.as_deref(), "show") {
        Ok(n) => n,
        Err(e) => {
            eprintln!("{e}");
            return ExitCode::FAILURE;
        }
    };
    let files = match input::resolve_calendar_files(Path::new("."), &args.files) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("cicada show: {e}");
            return ExitCode::FAILURE;
        }
    };
    let calendars = match input::load_calendars(&files) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("cicada show: {e}");
            return ExitCode::FAILURE;
        }
    };
    let dir = backend::manifest_dir(Path::new("."), &args.files);
    let provider = match backend::provider(&provider_name, None, &dir, "show") {
        Ok(p) => p,
        Err(e) => {
            eprintln!("{e}");
            return ExitCode::FAILURE;
        }
    };
    let runtime = crate::runtime_or_die("show");
    let store = JsonFileStateStore::new(cicada_state::resolve_state_path(&dir, None));
    // A load failure is non-fatal (fresh state) — the fake backend
    // can't produce one, so the fallback is compiled out under
    // coverage.
    #[cfg(not(coverage))]
    let state = runtime
        .block_on(store.load())
        .unwrap_or_else(|_| cicada_core::domain::state::RecordedState::new());
    #[cfg(coverage)]
    let state = runtime.block_on(store.load()).unwrap_or_default();

    let id = LocalId::new(args.id.as_str());
    let mut found = None;
    for cal in &calendars {
        let remote = backend::remote_calendar(cal, state.calendar(&cal.name));
        // A provider lookup that fails mid-scan is fatal — the fake
        // backend can't produce one, so under coverage the unwrap is
        // unconditional.
        #[cfg(not(coverage))]
        let event = runtime
            .block_on(provider.find_by_tag(&remote, &id))
            .unwrap_or_else(|e| crate::die(&format!("cicada show {}: {e}", cal.name)));
        #[cfg(coverage)]
        let event = runtime
            .block_on(provider.find_by_tag(&remote, &id))
            .expect("find_by_tag on the fake backend cannot fail");
        if let Some(event) = event {
            found = Some((cal, remote, event));
            break;
        }
    }
    let Some((cal, remote, live)) = found else {
        eprintln!(
            "cicada show: no live event tagged {id:?} on the declared \
             calendars ({provider_name})"
        );
        return ExitCode::FAILURE;
    };
    let window = match window(args, &cal.time_zone) {
        Ok(w) => w,
        Err(e) => {
            eprintln!("cicada show: {e}");
            return ExitCode::FAILURE;
        }
    };
    let event = Event::new(id.clone(), live.body.clone());
    let occurrences = QueryRange::new().expand_remote(std::slice::from_ref(&live), window);
    match occurrences {
        Ok(occs) => {
            println!(
                "{} — \"{}\" (live @ {remote}; remote {} etag {})",
                id,
                live.body.summary,
                live.remote_id,
                live.etag.as_deref().unwrap_or("—")
            );
            if let Some(updated) = &live.updated {
                println!("  updated: {updated}");
            }
            print_body_lines(&event);
            println!("occurrences:");
            for occ in occs.iter().take(OCCURRENCE_CAP) {
                println!("  {}", occurrence_line(occ));
            }
            if occs.is_empty() && live.body.recurrence.is_none() {
                println!("  (outside the window — widen --from/--to)");
            }
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("cicada show {id}: {e}");
            ExitCode::FAILURE
        }
    }
}

/// The detail lines shared by the live view (`when`/`repeat`/…).
fn print_body_lines(event: &Event) {
    println!("  when:    {}", render_event_time(&event.time));
    if let Some(rule) = &event.recurrence {
        println!("  repeat:  {}", rule.describe());
    }
    if let Some(d) = &event.description {
        println!("  desc:    {d}");
    }
    if let Some(l) = &event.location {
        println!("  where:   {l}");
    }
    if !event.attendees.is_empty() {
        let who = event
            .attendees
            .iter()
            .map(|a| {
                if a.required {
                    a.email.clone()
                } else {
                    format!("{} (optional)", a.email)
                }
            })
            .collect::<Vec<_>>()
            .join(", ");
        println!("  who:     {who}");
    }
    if !event.reminders.is_empty() {
        let what = event
            .reminders
            .iter()
            .map(|r| format!("{} {:?}", r.kind, r.offset))
            .collect::<Vec<_>>()
            .join(", ");
        println!("  remind:  {what}");
    }
    if let Some(c) = &event.conferencing {
        println!("  conf:    {c}");
    }
    println!("  busy:    {}", event.transparency);
}
