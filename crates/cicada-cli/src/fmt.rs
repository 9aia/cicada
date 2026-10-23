//! `cicada fmt` — canonical re-serialization of calendar YAML
//! (`parse → emit` via [`cicada_schema::emit_calendar`]): stable
//! field order, normalized scalars, defaults elided. Comments are
//! not preserved — the DSL is data, not a syntax tree.
//!
//! Event order follows the file's declaration order; `--sort`
//! reorders by stable id instead. `--check` diffs without writing
//! and exits nonzero when any file isn't canonical (CI-friendly).

use std::fs;
use std::path::Path;
use std::process::ExitCode;

use cicada_core::domain::ids::LocalId;
use cicada_schema::{emit_calendar, parse_calendar};

use crate::FmtArgs;
use cicada_shell::input::{self, render_schema_error};

/// Run the verb; `FAILURE` on read/parse errors or — under
/// `--check` — on any non-canonical file.
pub(crate) fn run(args: &FmtArgs) -> ExitCode {
    let files = match input::resolve_calendar_files(Path::new("."), &args.files) {
        Ok(f) if f.is_empty() => {
            eprintln!("cicada fmt: no calendar files found");
            return ExitCode::FAILURE;
        }
        Ok(f) => f,
        Err(e) => {
            eprintln!("cicada fmt: {e}");
            return ExitCode::FAILURE;
        }
    };

    let mut failed = false;
    let mut reformatted = 0usize;
    for path in &files {
        match format_one(path, args) {
            Ok(FormatOutcome::Unchanged) => println!("unchanged {}", path.display()),
            Ok(FormatOutcome::Written) => {
                reformatted += 1;
                println!("formatted {}", path.display());
            }
            Ok(FormatOutcome::WouldReformat) => {
                failed = true;
                reformatted += 1;
                println!(
                    "not canonical {} — run `cicada fmt {}`",
                    path.display(),
                    path.display()
                );
            }
            Err(e) => {
                failed = true;
                eprintln!("cicada fmt {}: {e}", path.display());
            }
        }
    }
    if args.check && reformatted == 0 {
        println!("all {} file(s) canonical", files.len());
    }
    if failed {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}

/// What one file did under `fmt`.
enum FormatOutcome {
    /// Already canonical.
    Unchanged,
    /// Rewritten in place.
    Written,
    /// `--check` only — would have been rewritten.
    WouldReformat,
}

/// Parse → emit → compare/write one file.
fn format_one(path: &Path, args: &FmtArgs) -> Result<FormatOutcome, String> {
    let source = fs::read_to_string(path).map_err(|e| format!("cannot read: {e}"))?;
    let calendar = parse_calendar(&source).map_err(|e| render_schema_error(&e))?;
    // File order is lost in the domain's BTreeMap — recover it from
    // the raw document so fmt doesn't reorder events unless --sort.
    let order = if args.sort {
        None
    } else {
        Some(event_order(&source))
    };
    let emitted =
        emit_calendar(&calendar, order.as_deref()).map_err(|e| render_schema_error(&e))?;
    if emitted == source {
        return Ok(FormatOutcome::Unchanged);
    }
    if args.check {
        return Ok(FormatOutcome::WouldReformat);
    }
    fs::write(path, emitted).map_err(|e| format!("cannot write: {e}"))?;
    Ok(FormatOutcome::Written)
}

/// `spec.events[].id` in file order, best-effort — `None` entries
/// (or an unparsable document, already rejected by `parse_calendar`)
/// fall back to sorted emission.
fn event_order(source: &str) -> Vec<LocalId> {
    serde_yaml::from_str::<serde_yaml::Value>(source)
        .ok()
        .and_then(|v| v.get("spec")?.get("events")?.as_sequence().cloned())
        .map_or_else(Vec::new, |events| {
            events
                .iter()
                .filter_map(|e| e.get("id")?.as_str().map(LocalId::new))
                .collect()
        })
}
