//! Shared lifecycle plumbing for `plan`/`apply`/`refresh`/`drift`:
//! provider selection, state-path resolution, the file-backed fake
//! provider, and the current-thread runtime every verb builds.
//!
//! Provider story: `fake` (default) is the only wired adapter —
//! backed by [`FakeFileProvider`] so the whole lifecycle persists
//! across processes (test/demo only). `google` lands in a later
//! build step.

use std::fs;
use std::path::{Path, PathBuf};

use cicada_core::domain::calendar::Calendar;
use cicada_core::domain::state::RecordedCalendar;
use cicada_core::ports::CalendarRef;
use cicada_schema::{Document, parse_document};
use cicada_testkit::FakeFileProvider;

/// The provider names the CLI accepts.
const PROVIDERS: [&str; 2] = ["fake", "google"];

/// Validate `--provider`: `fake` (default) is wired; `google` is a
/// stub error; anything else is unknown.
pub(crate) fn provider_name(name: Option<&str>, verb: &str) -> Result<String, String> {
    match name.unwrap_or("fake") {
        "fake" => Ok("fake".to_owned()),
        "google" => Err(format!(
            "cicada {verb}: the google provider is not implemented yet — use --provider fake"
        )),
        other => Err(format!(
            "cicada {verb}: unknown provider {other:?} (supported: {})",
            PROVIDERS.join(", ")
        )),
    }
}

/// The directory `.cicada/` lives in — beside the manifest. When
/// explicit files were given, a leading `kind: Manifest` argument
/// anchors it to that manifest's directory; otherwise the current
/// directory (where manifests are discovered).
pub(crate) fn manifest_dir(files: &[PathBuf]) -> PathBuf {
    if let Some(first) = files.first() {
        if let Ok(source) = fs::read_to_string(first) {
            if let Ok(Document::Manifest(_)) = parse_document(&source) {
                return first
                    .parent()
                    .map_or_else(|| PathBuf::from("."), Path::to_path_buf);
            }
        }
    }
    PathBuf::from(".")
}

/// Open the file-backed fake provider: `--fake-file` wins, else
/// `.cicada/fake-provider.json` beside the manifest (the same
/// `.cicada/` that hosts `state.yaml`).
pub(crate) fn fake_provider(
    fake_file: Option<&Path>,
    manifest_dir: &Path,
    verb: &str,
) -> Result<FakeFileProvider, String> {
    let path = fake_file.map_or_else(
        || manifest_dir.join(".cicada").join("fake-provider.json"),
        Path::to_path_buf,
    );
    FakeFileProvider::open(&path)
        .map_err(|e| format!("cicada {verb}: cannot open {}: {e}", path.display()))
}

/// A current-thread runtime — all port calls are `block_on`-driven.
pub(crate) fn runtime(verb: &str) -> Result<tokio::runtime::Runtime, String> {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| format!("cicada {verb}: cannot start runtime: {e}"))
}

/// Remote calendar binding for a logical calendar (ADR-0002 §9):
/// `metadata.calendarId` wins, then the recorded binding, then
/// `primary`.
pub(crate) fn remote_calendar(cal: &Calendar, recorded: Option<&RecordedCalendar>) -> CalendarRef {
    CalendarRef::new(
        cal.provider_calendar_id
            .clone()
            .or_else(|| {
                recorded.and_then(|r| {
                    if r.calendar_id.is_empty() {
                        None
                    } else {
                        Some(r.calendar_id.clone())
                    }
                })
            })
            .unwrap_or_else(|| "primary".to_owned()),
    )
}
