//! `cicada apply` — plan then execute against the provider, with the
//! state file under its advisory lock for the whole run (ADR-0002
//! §7). The plan is printed first; without `--auto-approve` the verb
//! asks before writing.
//!
//! Per-op outcomes are printed as they land; the exit code is
//! `FAILURE` when any op failed (conflict-refused, provider error) —
//! partial applies are honest (ADR-0001 §4).

use std::io::Write as _;
use std::path::Path;
use std::process::ExitCode;

use cicada_core::application::{
    ApplyPlan, ApplyReport, ApplyRequest, OpResult, PlanCalendar, PlanRequest,
};
use cicada_core::domain::calendar::Calendar;
use cicada_core::domain::policy::SendUpdates;
use cicada_core::ports::CalendarRef;
use cicada_core::ports::clock::SystemClock;
use cicada_state::JsonFileStateStore;

use crate::ApplyArgs;
use crate::plan::print_plan;
use cicada_shell::backend;
use cicada_shell::input;

/// One calendar's plan + binding, staged for application.
struct Staged<'a> {
    /// The desired calendar.
    cal: &'a Calendar,
    /// Remote binding.
    remote: CalendarRef,
    /// The computed plan.
    plan: cicada_core::planning::Plan,
    /// Effective sendUpdates (flag override else spec).
    send_updates: SendUpdates,
}

/// Run the verb.
pub(crate) fn run(args: &ApplyArgs) -> ExitCode {
    let provider_name = match backend::provider_name(args.provider.as_deref(), "apply") {
        Ok(n) => n,
        Err(e) => {
            eprintln!("{e}");
            return ExitCode::FAILURE;
        }
    };
    let notify = match args.notify.as_deref().map(SendUpdates::parse).transpose() {
        Ok(n) => n,
        Err(e) => {
            eprintln!("cicada apply: --notify: {e}");
            return ExitCode::FAILURE;
        }
    };
    let files = match input::resolve_calendar_files(Path::new("."), &args.files) {
        Ok(f) if f.is_empty() => {
            eprintln!("cicada apply: no calendar files found");
            return ExitCode::FAILURE;
        }
        Ok(f) => f,
        Err(e) => {
            eprintln!("cicada apply: {e}");
            return ExitCode::FAILURE;
        }
    };
    let calendars = match input::load_calendars(&files) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("cicada apply: {e}");
            return ExitCode::FAILURE;
        }
    };
    let dir = backend::manifest_dir(Path::new("."), &args.files);
    let state_path = cicada_state::resolve_state_path(&dir, args.state.as_deref());
    let store = JsonFileStateStore::new(&state_path);
    let provider = match backend::provider(&provider_name, args.fake_file.as_deref(), &dir, "apply")
    {
        Ok(p) => p,
        Err(e) => {
            eprintln!("{e}");
            return ExitCode::FAILURE;
        }
    };
    let runtime = match backend::runtime("apply") {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("{e}");
            return ExitCode::FAILURE;
        }
    };

    // The advisory lock spans plan+apply: the plan's drift check is
    // apply's implicit refresh (ADR-0002 §4), so both read the same
    // live/recorded snapshot.
    let _guard = match store.lock() {
        Ok(g) => g,
        Err(e) => {
            eprintln!("cicada apply: {e}");
            return ExitCode::FAILURE;
        }
    };
    let state = match runtime.block_on(cicada_core::ports::StateStore::load(&store)) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("cicada apply: cannot load {}: {e}", state_path.display());
            return ExitCode::FAILURE;
        }
    };
    let clock = SystemClock;

    // Stage one plan per calendar.
    let mut staged = Vec::new();
    let mut failed = false;
    for cal in &calendars {
        let recorded = state.calendar(&cal.name);
        let remote = backend::remote_calendar(cal, recorded);
        let send_updates = notify.unwrap_or(cal.send_updates);
        let req = PlanRequest {
            desired: cal,
            recorded,
            provider: &provider,
            remote_calendar: &remote,
            clock: &clock,
            send_updates,
        };
        match runtime.block_on(PlanCalendar::new().run(&req)) {
            Ok(plan) => {
                print_plan(&plan, &provider_name, remote.as_str());
                if plan.has_errors() {
                    eprintln!(
                        "cicada apply {}: plan contains errors — refusing to apply",
                        cal.name
                    );
                    failed = true;
                } else {
                    staged.push(Staged {
                        cal,
                        remote,
                        plan,
                        send_updates,
                    });
                }
            }
            Err(e) => {
                eprintln!("cicada apply {}: {e}", cal.name);
                failed = true;
            }
        }
    }
    if failed {
        return ExitCode::FAILURE;
    }

    let remote_ops: usize = staged.iter().map(|s| s.plan.remote_op_count()).sum();
    if remote_ops == 0 {
        println!("apply: nothing to do — recorded state already matches desired");
        return ExitCode::SUCCESS;
    }
    if !args.auto_approve && !confirm(remote_ops, staged.len()) {
        eprintln!("cicada apply: aborted");
        return ExitCode::FAILURE;
    }

    // Execute; the store journals per op, so the lock must outlive
    // every apply.
    let mut any_failed = false;
    for s in &staged {
        let req = ApplyRequest {
            plan: &s.plan,
            desired: s.cal,
            provider: &provider,
            state: &store,
            remote_calendar: &s.remote,
            provider_name: &provider_name,
            send_updates: s.send_updates,
            force: args.force,
            resume: args.resume,
            clock: &clock,
        };
        match runtime.block_on(ApplyPlan::new().run(&req)) {
            Ok(report) => {
                print_report(&report);
                any_failed |= report.has_failures();
            }
            Err(e) => {
                eprintln!("cicada apply {}: {e}", s.cal.name);
                any_failed = true;
            }
        }
    }
    if any_failed {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}

/// Interactive gate — a bare `y`/Enter aborts. Skipped by
/// `--auto-approve`.
fn confirm(remote_ops: usize, calendars: usize) -> bool {
    print!(
        "apply {remote_ops} remote op{} across {calendars} calendar{}? [y/N] ",
        if remote_ops == 1 { "" } else { "s" },
        if calendars == 1 { "" } else { "s" },
    );
    let _ = std::io::stdout().flush();
    let mut answer = String::new();
    if std::io::stdin().read_line(&mut answer).is_err() {
        return false;
    }
    matches!(answer.trim(), "y" | "Y" | "yes")
}

/// Per-op outcome lines plus the summary count.
fn print_report(report: &ApplyReport) {
    for outcome in &report.outcomes {
        let kind = outcome.op.code();
        let id = &outcome.local_id;
        match &outcome.result {
            OpResult::Applied { remote_id } => {
                let rid = remote_id
                    .as_ref()
                    .map_or("?".to_owned(), ToString::to_string);
                println!("  ✓ {kind:<7} {id} → remote {rid}");
            }
            OpResult::AlreadyApplied { remote_id } => {
                let rid = remote_id
                    .as_ref()
                    .map_or("?".to_owned(), ToString::to_string);
                println!("  ≈ {kind:<7} {id} → already applied (remote {rid})");
            }
            OpResult::SkippedResume => {
                println!("  - {kind:<7} {id} → skipped (journaled)");
            }
            OpResult::Noop => {
                println!("  · {kind:<7} {id}");
            }
            OpResult::Failed(e) => {
                println!("  ✗ {kind:<7} {id} — {e}");
            }
        }
    }
    println!(
        "apply {}: {} applied · {} skipped · {} failed",
        report.calendar,
        report.applied(),
        report.skipped(),
        report.failed()
    );
}
