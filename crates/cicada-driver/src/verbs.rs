//! The tamias driver verbs — `install`, `deploy`, `uninstall`,
//! `status`, `config-schema`, `capabilities` — behind the dispatch
//! contract `tamias-driver-<type> <verb> --dir <d> --name <n>
//! [--config <c>]`.
//!
//! Every verb wires the *same* cicada-core use cases the `cicada` CLI
//! runs ([`PlanCalendar`], [`DetectDrift`], [`ValidateCalendar`]) over
//! the same `cicada-shell` plumbing ([`backend::provider`],
//! [`backend::runtime`], [`input`]) — the driver adds no business
//! logic of its own.
//!
//! Safety posture (ADR-0002):
//!
//! - `install` **validates only** — schema + domain invariants; it
//!   never writes remote state.
//! - `deploy` is **plan-only** — it refreshes the operator artifacts
//!   (`driver.json`, `plan.json`) under the resolved state dir and
//!   diffs desired vs recorded vs live, but never applies. The
//!   confirmation gate stays with the operator (`cicada apply`).
//! - `uninstall` **reports** the managed inventory + drift as JSON
//!   and deletes nothing — cicada's managed boundary means remote
//!   events are the operator's to remove.
//! - `status` reads recorded state only — no provider calls, so it
//!   works without credentials.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use cicada_core::application::{
    DetectDrift, DriftRequest, PlanCalendar, PlanRequest, Severity, ValidateCalendar,
};
use cicada_core::domain::calendar::Calendar;
use cicada_core::ports::clock::SystemClock;
use cicada_core::ports::{CalendarRef, StateStore};
use cicada_schema::parse_calendar;
use cicada_shell::{backend, input};
use cicada_state::JsonFileStateStore;
use serde_json::{Value, json};

use crate::config::{self, DriverConfig};

/// The name errors are prefixed with — matches the binary.
const WHO: &str = "tamias-driver-cicada";

/// One dispatched verb invocation — the flags tamias always passes.
#[derive(Debug, Clone)]
pub struct Invocation {
    /// The installed payload dir (`tamias.yaml` + `cicada.yaml` +
    /// `calendars/`).
    pub dir: PathBuf,
    /// The installed package name (from the store record).
    pub name: String,
    /// Optional `--config` file.
    pub config: Option<PathBuf>,
}

/// `capabilities` — the discovery payload tamias reads for `types`
/// routing and verb warnings. `capabilities` names the consumer
/// surface (`tamias`); `types` is the content kind.
pub fn capabilities() -> Value {
    json!({
        "capabilities": ["tamias"],
        "verbs": [
            "capabilities",
            "install",
            "deploy",
            "uninstall",
            "status",
            "config-schema",
        ],
        "types": ["cicada"],
    })
}

/// `config-schema` — the reserved verb printing the JSON Schema of
/// the `--config` file (see [`config::config_schema`]).
pub fn config_schema() -> Value {
    config::config_schema()
}

/// `install` — validate the payload: expand the manifest, parse every
/// calendar (schema layer), and run [`ValidateCalendar`] (domain
/// layer). Reports per-file results as JSON; never touches a
/// provider.
pub fn install(inv: &Invocation) -> ExitCode {
    match install_inner(inv) {
        Ok(report) => {
            let ok = report["ok"] == true;
            emit(&report);
            if ok {
                ExitCode::SUCCESS
            } else {
                ExitCode::FAILURE
            }
        }
        Err(e) => fail(&e),
    }
}

fn install_inner(inv: &Invocation) -> Result<Value, String> {
    let files = input::resolve_calendar_files(&inv.dir, &[]).map_err(|e| format!("{WHO}: {e}"))?;
    if files.is_empty() {
        return Ok(json!({
            "ok": false,
            "package": inv.name,
            "error": "manifest lists no calendars",
        }));
    }
    let mut calendars = Vec::new();
    let mut errors = 0usize;
    let mut warnings = 0usize;
    for file in &files {
        let entry = validate_file(file);
        errors += entry["errors"].as_u64().unwrap_or(0) as usize
            + usize::from(entry.get("error").is_some());
        warnings += entry["warnings"].as_u64().unwrap_or(0) as usize;
        calendars.push(entry);
    }
    Ok(json!({
        "ok": errors == 0,
        "package": inv.name,
        "payload": inv.dir,
        "calendars": calendars,
        "errors": errors,
        "warnings": warnings,
        "applied": false,
    }))
}

/// One calendar file through both validation layers. A parse failure
/// is data, not a crash — install reports every broken file at once.
fn validate_file(path: &Path) -> Value {
    let file = path.display().to_string();
    let source = match fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) => {
            return json!({"file": file, "ok": false, "error": format!("cannot read: {e}")});
        }
    };
    let calendar = match parse_calendar(&source) {
        Ok(c) => c,
        Err(e) => {
            return json!({
                "file": file,
                "ok": false,
                "error": input::render_schema_error(&e),
            });
        }
    };
    calendar_report(path, &calendar)
}

/// Layer 2 for one parsed calendar.
fn calendar_report(path: &Path, calendar: &Calendar) -> Value {
    let report = ValidateCalendar::new().run(calendar);
    let errors = report.count(Severity::Error);
    let warnings = report.count(Severity::Warning);
    let diagnostics: Vec<String> = report
        .diagnostics
        .iter()
        .map(|d| format!("{} {}: {}", d.severity, d.path, d.message))
        .collect();
    json!({
        "file": path.display().to_string(),
        "calendar": calendar.name,
        "ok": errors == 0,
        "events": calendar.len(),
        "errors": errors,
        "warnings": warnings,
        "diagnostics": diagnostics,
    })
}

/// `deploy` — refresh the operator artifacts and run a **plan**
/// (desired vs recorded vs live). Plan-only per ADR-0002's
/// confirmation gate: nothing is applied, no remote event is
/// written.
///
/// Artifacts, under the resolved state dir (`stateDir` or the
/// payload's `.cicada/`):
///
/// - `driver.json` — the effective operator config (provider, state
///   file, payload), `cicada/driver-config-v1`.
/// - `plan.json` — the `cicada/plan-v1` document(s), same shape as
///   `cicada plan --out`.
pub fn deploy(inv: &Invocation) -> ExitCode {
    match deploy_inner(inv) {
        Ok((report, ok)) => {
            emit(&report);
            if ok {
                ExitCode::SUCCESS
            } else {
                ExitCode::FAILURE
            }
        }
        Err(e) => fail(&e),
    }
}

fn deploy_inner(inv: &Invocation) -> Result<(Value, bool), String> {
    let cfg = config::load(inv.config.as_deref())?;
    let calendars = load_calendars(&inv.dir)?;
    if calendars.is_empty() {
        return Ok((
            json!({"ok": false, "package": inv.name, "error": "manifest lists no calendars"}),
            false,
        ));
    }
    let provider_name = provider_name(&cfg)?;
    let state_path = state_path(&inv.dir, &cfg);
    // Artifacts live beside state.yaml — `.cicada/` by default, the
    // configured `stateDir` when set. `parent` of a file path always
    // exists except for a bare `/state.yaml`, compiled out under
    // coverage.
    #[cfg(not(coverage))]
    let artifact_dir = state_path
        .parent()
        .map_or_else(|| inv.dir.clone(), Path::to_path_buf);
    #[cfg(coverage)]
    let artifact_dir = state_path
        .parent()
        .expect("state path has a parent")
        .to_path_buf();

    let provider = backend::provider(&provider_name, cfg.fake_file.as_deref(), &inv.dir, WHO)?;
    let runtime = backend::runtime(WHO)?;
    let store = JsonFileStateStore::new(&state_path);
    let state = runtime
        .block_on(StateStore::load(&store))
        .map_err(|e| format!("{WHO}: cannot load {}: {e}", state_path.display()))?;
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
            Ok(plan) => plans.push(plan),
            Err(e) => {
                eprintln!("{WHO}: plan {}: {e}", cal.name);
                failed = true;
            }
        }
    }

    let driver_config = json!({
        "apiVersion": "cicada/driver-config-v1",
        "package": inv.name,
        "payload": inv.dir,
        "provider": provider_name,
        "stateFile": state_path,
        "fakeFile": cfg.fake_file,
        "configSource": inv.config,
        "note": "effective operator config — refresh on every deploy; stateDir should live outside the evictable payload",
    });
    let config_path = artifact_dir.join("driver.json");
    write_json(&config_path, &driver_config)?;
    let plan_path = artifact_dir.join("plan.json");
    write_plans(&plans, &plan_path)?;

    let plan_summaries: Vec<Value> = plans
        .iter()
        .map(|p| {
            json!({
                "calendar": p.calendar,
                "ops": p.ops.len(),
                "remoteOps": p.remote_op_count(),
                "drift": p.drift.len(),
                "warnings": p.warnings.len(),
                "hasErrors": p.has_errors(),
            })
        })
        .collect();
    let has_errors = plans.iter().any(cicada_core::planning::Plan::has_errors);
    let ok = !failed && !has_errors;
    Ok((
        json!({
            "ok": ok,
            "package": inv.name,
            "provider": provider_name,
            "applied": false,
            "stateFile": state_path,
            "configFile": config_path,
            "planFile": plan_path,
            "plans": plan_summaries,
        }),
        ok,
    ))
}

/// `uninstall` — the managed-inventory + drift report. Read-only:
/// recorded state is compared against the live provider and the
/// result is JSON; **no remote event is deleted** — removing
/// provider-side events stays an explicit `cicada apply`/provider
/// decision (ADR-0002 managed boundary).
pub fn uninstall(inv: &Invocation) -> ExitCode {
    match uninstall_inner(inv) {
        Ok(report) => {
            emit(&report);
            ExitCode::SUCCESS
        }
        Err(e) => fail(&e),
    }
}

fn uninstall_inner(inv: &Invocation) -> Result<Value, String> {
    let cfg = config::load(inv.config.as_deref())?;
    let state_path = state_path(&inv.dir, &cfg);
    let runtime = backend::runtime(WHO)?;
    let store = JsonFileStateStore::new(&state_path);
    let state = runtime
        .block_on(StateStore::load(&store))
        .map_err(|e| format!("{WHO}: cannot load {}: {e}", state_path.display()))?;
    if state.calendars.is_empty() {
        return Ok(json!({
            "ok": true,
            "package": inv.name,
            "provider": cfg.provider_name(),
            "stateFile": state_path,
            "calendars": [],
            "driftTotal": 0,
            "deletedRemoteEvents": false,
            "note": "no recorded state — nothing was ever applied by cicada",
        }));
    }
    let provider_name = provider_name(&cfg)?;
    let provider = backend::provider(&provider_name, cfg.fake_file.as_deref(), &inv.dir, WHO)?;

    let mut calendars = Vec::new();
    let mut drift_total = 0usize;
    for (name, recorded) in &state.calendars {
        let remote = CalendarRef::new(recorded.calendar_id.clone());
        let req = DriftRequest {
            calendar_name: name,
            recorded,
            provider: &provider,
            remote_calendar: &remote,
        };
        let entries = runtime
            .block_on(DetectDrift::new().run(&req))
            .map_err(|e| format!("{WHO}: drift {name}: {e}"))?;
        drift_total += entries.len();
        calendars.push(json!({
            "name": name,
            "remoteCalendar": recorded.calendar_id,
            "recordedEvents": recorded.events.len(),
            "drift": entries,
        }));
    }
    Ok(json!({
        "ok": true,
        "package": inv.name,
        "provider": provider_name,
        "stateFile": state_path,
        "calendars": calendars,
        "driftTotal": drift_total,
        "deletedRemoteEvents": false,
        "note": "managed inventory only — remote events are left in place; remove them via the provider or `cicada apply`",
    }))
}

/// `status` — recorded-state summary. No provider calls: works
/// without credentials and never blocks on a live read.
pub fn status(inv: &Invocation) -> ExitCode {
    match status_inner(inv) {
        Ok(report) => {
            emit(&report);
            ExitCode::SUCCESS
        }
        Err(e) => fail(&e),
    }
}

fn status_inner(inv: &Invocation) -> Result<Value, String> {
    let cfg = config::load(inv.config.as_deref())?;
    let state_path = state_path(&inv.dir, &cfg);
    let runtime = backend::runtime(WHO)?;
    let store = JsonFileStateStore::new(&state_path);
    let state = runtime
        .block_on(StateStore::load(&store))
        .map_err(|e| format!("{WHO}: cannot load {}: {e}", state_path.display()))?;
    let calendars: Vec<Value> = state
        .calendars
        .iter()
        .map(|(name, recorded)| {
            json!({
                "name": name,
                "provider": recorded.provider,
                "calendarId": recorded.calendar_id,
                "events": recorded.events.len(),
            })
        })
        .collect();
    Ok(json!({
        "ok": true,
        "package": inv.name,
        "provider": cfg.provider_name(),
        "stateFile": state_path,
        "applied": !state.calendars.is_empty(),
        "calendars": calendars,
    }))
}

/// Resolve + parse every calendar the payload's manifest declares.
fn load_calendars(dir: &Path) -> Result<Vec<Calendar>, String> {
    let files = input::resolve_calendar_files(dir, &[]).map_err(|e| format!("{WHO}: {e}"))?;
    input::load_calendars(&files).map_err(|e| format!("{WHO}: {e}"))
}

/// The effective provider name — config `provider` or
/// [`config::DEFAULT_PROVIDER`], validated against the known set.
fn provider_name(cfg: &DriverConfig) -> Result<String, String> {
    backend::provider_name(Some(cfg.provider_name()), WHO)
}

/// The recorded-state file: `stateDir/state.yaml` when configured,
/// else cicada's default resolution (`.cicada/state.yaml` beside the
/// manifest, `CICADA_STATE` honored).
fn state_path(dir: &Path, cfg: &DriverConfig) -> PathBuf {
    cfg.state_dir.as_ref().map_or_else(
        || cicada_state::resolve_state_path(dir, None),
        |d| d.join("state.yaml"),
    )
}

/// Write `plans` as `cicada/plan-v1` JSON — a single document for one
/// calendar, an array for several (same contract as
/// `cicada plan --out`).
fn write_plans(plans: &[cicada_core::planning::Plan], path: &Path) -> Result<(), String> {
    // A `Plan` is plain serializable data — serialization can't
    // fail; the error arm exists for the `Result` contract.
    let value = if let [plan] = plans {
        serde_json::to_value(plan)
    } else {
        serde_json::to_value(plans)
    }
    .map_err(|e| format!("{WHO}: cannot serialize plan: {e}"))?;
    write_json(path, &value)
}

/// Pretty JSON + trailing newline, creating parent dirs.
fn write_json(path: &Path, value: &Value) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|e| format!("{WHO}: cannot create {}: {e}", parent.display()))?;
    }
    // `Value` serialization is infallible in practice; the error arm
    // stays for the `Result` contract.
    let text = serde_json::to_string_pretty(value)
        .map_err(|e| format!("{WHO}: cannot serialize {}: {e}", path.display()))?;
    fs::write(path, format!("{text}\n"))
        .map_err(|e| format!("{WHO}: cannot write {}: {e}", path.display()))
}

/// Print a JSON report to stdout.
pub fn emit(value: &Value) {
    // `to_string_pretty` on a `Value` can't fail; the compact
    // `Display` fallback keeps the contract total.
    match serde_json::to_string_pretty(value) {
        Ok(text) => println!("{text}"),
        Err(_) => println!("{value}"),
    }
}

/// stderr + `FAILURE` for failures a caller can't recover from.
fn fail(message: &str) -> ExitCode {
    eprintln!("{WHO}: {message}");
    ExitCode::FAILURE
}
