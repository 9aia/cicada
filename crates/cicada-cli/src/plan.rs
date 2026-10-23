//! `cicada plan` — desired vs recorded vs live → a printed plan plus
//! an optional `cicada/plan-v1` JSON document (ADR-0002 §11).
//!
//! Provider selection: `--provider fake` (the default) plans against
//! the file-backed [`FakeFileProvider`]; `--provider google` is the
//! next build step. Recorded state loads from `.cicada/state.yaml`
//! via [`JsonFileStateStore`] — absent means first-run, all-`Create`
//! plans.

use std::fmt::Write as _;
use std::fs;
use std::process::ExitCode;

use cicada_core::application::{PlanCalendar, PlanRequest};
use cicada_core::domain::operation::{EventDiff, OpKind, Operation};
use cicada_core::domain::time::Time;
use cicada_core::planning::{Plan, Severity};
use cicada_core::ports::clock::SystemClock;
use cicada_state::JsonFileStateStore;

use crate::PlanArgs;
use crate::backend;
use crate::input;

/// Run the verb; exit `FAILURE` on load/plan errors or when any plan
/// carries error-severity findings (apply would refuse it anyway).
pub(crate) fn run(args: &PlanArgs) -> ExitCode {
    let provider_name = match backend::provider_name(args.provider.as_deref(), "plan") {
        Ok(n) => n,
        Err(e) => {
            eprintln!("{e}");
            return ExitCode::FAILURE;
        }
    };
    let files = match input::resolve_calendar_files(&args.files) {
        Ok(f) if f.is_empty() => {
            eprintln!("cicada plan: no calendar files found");
            return ExitCode::FAILURE;
        }
        Ok(f) => f,
        Err(e) => {
            eprintln!("cicada plan: {e}");
            return ExitCode::FAILURE;
        }
    };
    let calendars = match input::load_calendars(&files) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("cicada plan: {e}");
            return ExitCode::FAILURE;
        }
    };
    let dir = backend::manifest_dir(&args.files);
    let state_path = cicada_state::resolve_state_path(&dir, args.state.as_deref());
    let store = JsonFileStateStore::new(&state_path);
    let provider = match backend::fake_provider(args.fake_file.as_deref(), &dir, "plan") {
        Ok(p) => p,
        Err(e) => {
            eprintln!("{e}");
            return ExitCode::FAILURE;
        }
    };
    let runtime = match backend::runtime("plan") {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("{e}");
            return ExitCode::FAILURE;
        }
    };
    let state = match runtime.block_on(cicada_core::ports::StateStore::load(&store)) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("cicada plan: cannot load {}: {e}", state_path.display());
            return ExitCode::FAILURE;
        }
    };
    let clock = SystemClock;

    let mut plans = Vec::new();
    let mut failed = false;
    for cal in &calendars {
        let recorded = state.calendar(&cal.name);
        let remote = backend::remote_calendar(cal, recorded);
        let req = PlanRequest {
            desired: cal,
            recorded,
            provider: &provider,
            remote_calendar: &remote,
            clock: &clock,
            send_updates: cal.send_updates,
        };
        match runtime.block_on(PlanCalendar::new().run(&req)) {
            Ok(plan) => {
                print_plan(&plan, &provider_name, remote.as_str());
                plans.push(plan);
            }
            Err(e) => {
                eprintln!("cicada plan {}: {e}", cal.name);
                failed = true;
            }
        }
    }

    if let Some(out) = &args.out {
        if let Err(e) = write_plans(&plans, out) {
            eprintln!("cicada plan: cannot write {}: {e}", out.display());
            failed = true;
        }
    }
    if failed || plans.iter().any(Plan::has_errors) {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}

/// Write `--out`: a single `plan-v1` document when one calendar was
/// planned, a JSON array of them otherwise.
fn write_plans(plans: &[Plan], out: &std::path::Path) -> Result<(), String> {
    let json = if let [plan] = plans {
        serde_json::to_string_pretty(plan)
    } else {
        serde_json::to_string_pretty(plans)
    }
    .map_err(|e| e.to_string())?;
    fs::write(out, format!("{json}\n")).map_err(|e| e.to_string())
}

/// Pretty-print one plan: per-op lines with diffs, the drift
/// summary, findings, and a count line. `pub(crate)` — `apply`
/// shows the same plan it executes.
pub(crate) fn print_plan(plan: &Plan, provider: &str, remote: &str) {
    println!(
        "calendar {} (provider: {provider} · calendar: {remote}) — generated {}",
        plan.calendar, plan.generated_at
    );
    if plan.ops.is_empty() {
        println!("  (no operations — nothing declared)");
    }
    for op in &plan.ops {
        for line in op_lines(op) {
            println!("  {line}");
        }
    }
    if !plan.drift.is_empty() {
        println!("drift ({}):", plan.drift.len());
        for d in &plan.drift {
            let detail = match d {
                cicada_core::domain::drift::DriftEntry::Missing { .. } => {
                    "live event is gone".to_owned()
                }
                cicada_core::domain::drift::DriftEntry::Modified {
                    recorded_hash,
                    live_hash,
                    ..
                } => {
                    let live = live_hash.as_ref().map_or("?".to_owned(), short_hash);
                    format!(
                        "live differs from recorded ({} → {live})",
                        short_hash(recorded_hash)
                    )
                }
            };
            println!(
                "  ! {} (remote {}): {} — {detail}",
                d.local_id(),
                d.remote_id(),
                d.kind().code()
            );
        }
    }
    if !plan.warnings.is_empty() {
        println!("findings ({}):", plan.warnings.len());
        for w in &plan.warnings {
            let scope = w
                .local_id
                .as_deref()
                .map_or_else(|| "calendar".to_owned(), str::to_owned);
            println!(
                "  - [{}] {}: {}",
                severity_label(w.severity),
                scope,
                w.message
            );
        }
    }
    let mut counts = String::new();
    for kind in [
        OpKind::Create,
        OpKind::Update,
        OpKind::Delete,
        OpKind::Replace,
        OpKind::NoOp,
    ] {
        let n = plan.ops.iter().filter(|op| op.kind() == kind).count();
        if n > 0 {
            let _ = write!(counts, "{n} {} · ", kind.code());
        }
    }
    let counts = counts.trim_end_matches(" · ");
    println!(
        "plan: {counts}({} remote op{})",
        plan.remote_op_count(),
        if plan.remote_op_count() == 1 { "" } else { "s" }
    );
}

/// One op rendered as one or more lines (diff lines follow the op).
fn op_lines(op: &Operation) -> Vec<String> {
    let conflict = op
        .conflict()
        .map_or_else(String::new, |k| format!(" [conflict: {}]", k.code()));
    let mut out = Vec::new();
    match op {
        Operation::Create { desired, .. } => {
            out.push(format!(
                "{:<7} {:<14} \"{}\" — {}{}",
                "create",
                op.local_id(),
                desired.summary,
                render_time(desired.time.start()),
                conflict
            ));
        }
        Operation::Update {
            remote_id,
            diff,
            desired,
            ..
        }
        | Operation::Replace {
            remote_id,
            diff,
            desired,
            ..
        } => {
            out.push(format!(
                "{:<7} {:<14} remote {remote_id} → \"{}\"{}",
                op.kind().code(),
                op.local_id(),
                desired.summary,
                conflict
            ));
            out.extend(diff_lines(diff));
        }
        Operation::Delete { remote_id, .. } => {
            out.push(format!(
                "{:<7} {:<14} remote {remote_id}{conflict}",
                "delete",
                op.local_id(),
            ));
        }
        Operation::NoOp { .. } => {
            out.push(format!("{:<7} {:<14}{conflict}", "noop", op.local_id()));
        }
    }
    out
}

/// Indented `~ field: from → to` lines for Update/Replace diffs.
fn diff_lines(diff: &EventDiff) -> Vec<String> {
    diff.changes
        .iter()
        .map(|c| {
            let from = c.from.as_deref().unwrap_or("∅");
            let to = c.to.as_deref().unwrap_or("∅");
            format!("        ~ {}: {from} → {to}", field_label(c.field))
        })
        .collect()
}

/// Snake-to-space field names for the diff lines.
fn field_label(field: cicada_core::domain::operation::EventField) -> &'static str {
    use cicada_core::domain::operation::EventField as F;
    match field {
        F::Summary => "summary",
        F::Description => "description",
        F::Location => "location",
        F::Time => "time",
        F::Recurrence => "recurrence",
        F::Attendees => "attendees",
        F::Reminders => "reminders",
        F::Conferencing => "conferencing",
        F::Transparency => "transparency",
    }
}

/// `2026-01-05T09:00:00 [UTC]` or the bare instant.
fn render_time(t: &Time) -> String {
    match t {
        Time::Wall { local, zone } => format!("{local} [{zone}]"),
        Time::Instant(ts) => ts.to_string(),
    }
}

/// `sha256:deadbeef…` → `sha256:deadbee…` for drift lines.
fn short_hash(hash: &cicada_core::domain::ids::ContentHash) -> String {
    let s = hash.as_str();
    let keep = s.char_indices().nth(15).map_or(s.len(), |(i, _)| i);
    format!("{}…", &s[..keep])
}

fn severity_label(severity: Severity) -> &'static str {
    match severity {
        Severity::Warning => "warning",
        Severity::Error => "error",
    }
}
