//! `cicada validate [files...]` — parse each calendar document
//! (schema, layer 1) and run [`ValidateCalendar`] (domain invariants,
//! layer 2), printing per-file ✓/✗ lines plus indented diagnostics.
//!
//! With no arguments the `cicada.yaml`/`cicada.yml` manifest in the
//! current directory is discovered and its `calendars:` list is
//! validated; a `kind: Manifest` passed explicitly expands the same
//! way. File reading lives here, not in core — the use case takes
//! parsed domain data.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use cicada_core::application::{Severity, ValidateCalendar};
use cicada_core::ports::logger::{LogLevel, Logger};
use cicada_schema::{Document, SchemaError, parse_calendar, parse_document};

use crate::logging::ConsoleLogger;

/// Manifest filenames probed when `validate` gets no arguments.
const MANIFEST_NAMES: [&str; 2] = ["cicada.yaml", "cicada.yml"];

/// A file to validate, or a discovery failure to report in place.
enum Target {
    /// Validate this calendar file.
    File(PathBuf),
    /// Discovery failed — report `reason` under `path`.
    Failure(PathBuf, String),
}

/// Run the verb; exit `FAILURE` when any file reports an error.
pub(crate) fn run(files: &[PathBuf]) -> ExitCode {
    let logger = ConsoleLogger::new();
    let targets = match collect_targets(files, logger) {
        Ok(t) if t.is_empty() => {
            logger.error("no calendar files to validate");
            return ExitCode::FAILURE;
        }
        Ok(t) => t,
        Err(e) => {
            logger.error(&e);
            return ExitCode::FAILURE;
        }
    };

    let mut ok = 0usize;
    let mut failed = 0usize;
    for target in &targets {
        let (path, result) = match target {
            Target::File(p) => (p.clone(), validate_file(p)),
            Target::Failure(p, reason) => (p.clone(), Err(reason.clone())),
        };
        match result {
            Ok(outcome) => {
                if outcome.errors == 0 {
                    ok += 1;
                    println!("✓ {} — {}", path.display(), outcome.summary);
                } else {
                    failed += 1;
                    println!("✗ {}", path.display());
                }
                for d in &outcome.diagnostics {
                    println!("  {d}");
                }
            }
            Err(reason) => {
                failed += 1;
                println!("✗ {}", path.display());
                println!("  error: {reason}");
            }
        }
    }
    println!("{ok} valid, {failed} failed");
    if failed == 0 {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

/// What one file produced: a summary plus rendered diagnostics.
struct Outcome {
    /// `name — N event(s)[, M warning(s)]`.
    summary: String,
    /// `severity path: message` lines, in report order.
    diagnostics: Vec<String>,
    /// How many diagnostics are errors.
    errors: usize,
}

/// Layer 1 + 2 for one file: read, parse, validate. IO and schema
/// failures come back as `Err(rendered)`.
fn validate_file(path: &Path) -> Result<Outcome, String> {
    let source = fs::read_to_string(path).map_err(|e| format!("cannot read: {e}"))?;
    let calendar = parse_calendar(&source).map_err(|e| render_schema_error(&e))?;
    let report = ValidateCalendar::new().run(&calendar);
    let errors = report.count(Severity::Error);
    let warnings = report.count(Severity::Warning);
    let diagnostics = report
        .diagnostics
        .iter()
        .map(|d| format!("{} {}: {}", d.severity, d.path, d.message))
        .collect();
    let summary = format!(
        "{} — {} event(s){}",
        calendar.name,
        calendar.len(),
        if warnings == 0 {
            String::new()
        } else {
            format!(", {warnings} warning(s)")
        }
    );
    Ok(Outcome {
        summary,
        diagnostics,
        errors,
    })
}

/// Where a `SchemaError` happened, compact: `path: msg` or
/// `line:col: msg`.
fn render_schema_error(e: &SchemaError) -> String {
    if let Some(path) = &e.path {
        format!("{}: {}", path, e.message)
    } else if let (Some(l), Some(c)) = (e.line, e.column) {
        format!("{l}:{c}: {}", e.message)
    } else {
        e.message.clone()
    }
}

/// The full target list: explicit files (manifests expand), or the
/// discovered manifest.
fn collect_targets(files: &[PathBuf], logger: ConsoleLogger) -> Result<Vec<Target>, String> {
    if files.is_empty() {
        let manifest = MANIFEST_NAMES
            .iter()
            .map(PathBuf::from)
            .find(|p| p.is_file())
            .ok_or_else(|| {
                "no files given and no cicada.yaml manifest found in the \
                 current directory"
                    .to_owned()
            })?;
        logger.log(
            LogLevel::Info,
            &format!("discovered manifest {}", manifest.display()),
            &[],
        );
        return Ok(expand(&manifest));
    }
    let mut targets = Vec::new();
    for f in files {
        targets.extend(expand(f));
    }
    Ok(targets)
}

/// Resolve one argument: a `kind: Manifest` expands to its calendar
/// list (relative to its own directory); anything else — a calendar
/// or a file that fails to parse — stays a target so `validate_file`
/// reports the precise schema error in place.
fn expand(path: &Path) -> Vec<Target> {
    let Ok(source) = fs::read_to_string(path) else {
        return vec![Target::File(path.to_path_buf())];
    };
    match parse_document(&source) {
        Ok(Document::Manifest(m)) => {
            let dir = path
                .parent()
                .map_or_else(|| Path::new(".").to_path_buf(), Path::to_path_buf);
            if m.calendars.is_empty() {
                return vec![Target::Failure(
                    path.to_path_buf(),
                    "manifest lists no calendars".into(),
                )];
            }
            m.calendars
                .iter()
                .map(|c| Target::File(dir.join(c)))
                .collect()
        }
        _ => vec![Target::File(path.to_path_buf())],
    }
}
