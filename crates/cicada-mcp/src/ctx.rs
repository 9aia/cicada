//! `Session` — the server lifetime's wiring: working root, resolved
//! calendar files, provider selection, and the state store path.
//!
//! One session = one MCP server process = one working root. The
//! session deliberately re-reads inputs on every tool call —
//! calendars from YAML, recorded state from disk, and the fake
//! provider from its file — because a long-running server shares
//! those files with concurrent `cicada` CLI invocations (ADR-0002
//! §7); caching them would hide drift the tools exist to expose.
//! Writes stay serialized through the state file's advisory lock.

use std::env;
use std::path::{Path, PathBuf};

use cicada_core::domain::calendar::Calendar;
use cicada_core::domain::state::RecordedCalendar;
use cicada_core::ports::CalendarRef;
use cicada_shell::backend::{self, Backend};
use cicada_shell::input;
use cicada_state::JsonFileStateStore;
use clap::Args;

/// Environment variable holding the working root when `--root` is
/// absent.
pub const ROOT_ENV: &str = "CICADA_ROOT";

/// Server options shared by the `cicada-mcp` binary and
/// `cicada mcp` (clap `Args` so it flattens into both parsers).
#[derive(Debug, Clone, Args)]
pub struct ServerOptions {
    /// Working root — the directory `cicada.yaml` is discovered in.
    /// Defaults to `CICADA_ROOT`, then the current directory.
    #[arg(long)]
    pub root: Option<PathBuf>,
    /// Provider adapter to use (fake | google; google needs
    /// credentials via env — see `GoogleAuthConfig::from_env`).
    #[arg(long)]
    pub provider: Option<String>,
    /// Fake provider backend file (test/demo only; default
    /// `.cicada/fake-provider.json` beside the manifest).
    #[arg(long)]
    pub fake_file: Option<PathBuf>,
    /// Recorded-state file (default `.cicada/state.yaml` beside the
    /// manifest; `CICADA_STATE` also overrides).
    #[arg(long)]
    pub state: Option<PathBuf>,
}

/// The session's resolved environment.
#[derive(Debug)]
pub struct Session {
    /// The working root the manifest was discovered in.
    root: PathBuf,
    /// Calendar files resolved from the root manifest.
    files: Vec<PathBuf>,
    /// Selected provider adapter name (`fake` | `google`).
    provider_name: String,
    /// Explicit `--fake-file`, if any.
    fake_file: Option<PathBuf>,
    /// The recorded-state store (`load`/`save`/`lock` per call).
    store: JsonFileStateStore,
}

impl Session {
    /// Resolve the session from options: root → manifest → provider
    /// + state store. Everything after this is per-call reload.
    ///
    /// # Errors
    /// `String` — transport-level surfaces (tool error results) wrap
    /// these; an unknown `--provider` fails here.
    pub fn open(opts: &ServerOptions) -> Result<Self, String> {
        let root = opts
            .root
            .clone()
            .or_else(|| env::var_os(ROOT_ENV).map(PathBuf::from))
            .unwrap_or_else(|| env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
        let provider_name = backend::provider_name(opts.provider.as_deref(), "cicada-mcp")?;
        let files = input::resolve_calendar_files(&root, &[])?;
        if files.is_empty() {
            return Err(format!(
                "cicada-mcp: the manifest in {} lists no calendars",
                root.display()
            ));
        }
        let dir = backend::manifest_dir(&root, &files);
        let state_path = cicada_state::resolve_state_path(&dir, opts.state.as_deref());
        Ok(Self {
            root,
            files,
            provider_name,
            fake_file: opts.fake_file.clone(),
            store: JsonFileStateStore::new(&state_path),
        })
    }

    /// The working root.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The selected provider name (`fake`).
    pub fn provider_name(&self) -> &str {
        &self.provider_name
    }

    /// Re-read + re-parse all calendar files (fresh per call — see
    /// module docs).
    pub fn calendars(&self) -> Result<Vec<Calendar>, String> {
        input::load_calendars(&self.files)
    }

    /// Open the selected provider backend — `fake` re-reads its
    /// on-disk snapshot so out-of-band edits by other processes are
    /// visible; `google` builds a fresh adapter over live HTTP.
    pub fn provider(&self, who: &str) -> Result<Backend, String> {
        backend::provider(
            &self.provider_name,
            self.fake_file.as_deref(),
            &self.root,
            who,
        )
    }

    /// The state store handle (loads/saves on demand).
    pub fn store(&self) -> &JsonFileStateStore {
        &self.store
    }

    /// Remote binding for a logical calendar (ADR-0002 §9).
    pub fn remote_calendar(
        &self,
        cal: &Calendar,
        recorded: Option<&RecordedCalendar>,
    ) -> CalendarRef {
        backend::remote_calendar(cal, recorded)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn session_root_reports_the_working_root() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("calendars")).unwrap();
        std::fs::write(
            dir.path().join("cicada.yaml"),
            "apiVersion: cicada/v1\nkind: Manifest\ncalendars: [calendars/work.yaml]\n",
        )
        .unwrap();
        std::fs::write(
            dir.path().join("calendars/work.yaml"),
            "apiVersion: cicada/v1\nkind: Calendar\nmetadata: {name: work, timeZone: UTC}\nspec: {events: []}\n",
        )
        .unwrap();
        let session = Session::open(&ServerOptions {
            root: Some(dir.path().to_path_buf()),
            provider: None,
            fake_file: None,
            state: None,
        })
        .unwrap();
        assert_eq!(session.root(), dir.path());
    }
}
