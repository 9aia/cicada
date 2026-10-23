//! Shared input plumbing for interfaces that read calendar YAML:
//! manifest discovery, `kind: Manifest` expansion, and
//! read+parse-in-one-shot loading. `cicada validate` keeps its own
//! per-file reporting path (it must survive individual failures);
//! `plan`/`range`/`show`/the MCP tools fail fast — a partial desired
//! state plans the wrong reconciliation.

use std::fs;
use std::path::{Path, PathBuf};

use cicada_core::domain::calendar::Calendar;
use cicada_schema::{Document, SchemaError, parse_calendar, parse_document};

/// Manifest filenames probed when a verb gets no file arguments.
pub const MANIFEST_NAMES: [&str; 2] = ["cicada.yaml", "cicada.yml"];

/// Resolve the calendar files an interface should operate on:
/// explicit arguments (a `kind: Manifest` expands to its calendar
/// list, relative to its own directory), or the discovered
/// `cicada.yaml`/`cicada.yml` in `root`.
pub fn resolve_calendar_files(root: &Path, files: &[PathBuf]) -> Result<Vec<PathBuf>, String> {
    if files.is_empty() {
        let manifest = MANIFEST_NAMES
            .iter()
            .map(|n| root.join(n))
            .find(|p| p.is_file())
            .ok_or_else(|| {
                format!(
                    "no files given and no cicada.yaml manifest found in {}",
                    root.display()
                )
            })?;
        return Ok(expand(&manifest));
    }
    let mut out = Vec::new();
    for f in files {
        out.extend(expand(f));
    }
    Ok(out)
}

/// One argument → the calendar files it names. A file that can't be
/// read/parsed stays itself so the loader reports the precise error.
fn expand(path: &Path) -> Vec<PathBuf> {
    let Ok(source) = fs::read_to_string(path) else {
        return vec![path.to_path_buf()];
    };
    match parse_document(&source) {
        Ok(Document::Manifest(m)) => {
            let dir = path
                .parent()
                .map_or_else(|| Path::new(".").to_path_buf(), Path::to_path_buf);
            m.calendars.iter().map(|c| dir.join(c)).collect()
        }
        _ => vec![path.to_path_buf()],
    }
}

/// Read + parse every calendar file, failing fast on the first
/// error. Returns calendars in file order.
pub fn load_calendars(files: &[PathBuf]) -> Result<Vec<Calendar>, String> {
    files.iter().map(|p| load_calendar(p)).collect()
}

/// Read + parse one calendar document.
fn load_calendar(path: &Path) -> Result<Calendar, String> {
    let source =
        fs::read_to_string(path).map_err(|e| format!("{}: cannot read: {e}", path.display()))?;
    parse_calendar(&source).map_err(|e| format!("{}: {}", path.display(), render_schema_error(&e)))
}

/// Compact `SchemaError` rendering — `path: msg` or `line:col: msg`.
pub fn render_schema_error(e: &SchemaError) -> String {
    if let Some(path) = &e.path {
        format!("{}: {}", path, e.message)
    } else if let (Some(l), Some(c)) = (e.line, e.column) {
        format!("{l}:{c}: {}", e.message)
    } else {
        e.message.clone()
    }
}
