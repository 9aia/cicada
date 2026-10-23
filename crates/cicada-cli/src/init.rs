//! `cicada init` — scaffold a working project in the current (or a
//! given) directory: the `cicada.yaml` manifest, a `calendars/`
//! starter, and the `.cicada/` state dir. Refuses to overwrite
//! existing files unless `--force`.

use std::fs;
use std::path::Path;
use std::process::ExitCode;

use crate::InitArgs;

/// The root manifest (ADR-0002 §9).
const MANIFEST: &str = "\
apiVersion: cicada/v1
kind: Manifest
calendars: [calendars/work.yaml]
";

/// A starter calendar — README-shaped, bound to the `fake` demo
/// provider so `cicada plan`/`apply` work out of the box. Written in
/// canonical form so `cicada fmt --check` passes on a fresh project.
const CALENDAR: &str = "\
apiVersion: cicada/v1
kind: Calendar
metadata:
  name: work
  timeZone: UTC
  provider: fake
  calendarId: primary
spec:
  events:
  - id: standup
    summary: Daily Standup
    start: 09:00
    duration: 30m
    recurrence:
      freq: WEEKLY
      byDay:
      - MO
      - TU
      - WE
      - TH
      - FR
    attendees:
    - email: alice@example.com
    reminders:
    - popup: 10m
    conferencing: meet
  - id: offsite
    summary: Team Offsite
    start: 2026-03-20T10:00
    duration: 4h
";

/// `.cicada/.gitignore` — the demo backend and lock files are
/// artifacts; `state.yaml` itself is committable (ADR-0002 §7).
const CICADA_GITIGNORE: &str = "\
# cicada runtime artifacts — state.yaml itself is committable.
fake-provider.json
*.lock
";

/// Run the verb.
pub(crate) fn run(args: &InitArgs) -> ExitCode {
    let dir = args.dir.as_deref().unwrap_or_else(|| Path::new("."));
    let manifest = dir.join("cicada.yaml");
    let calendar = dir.join("calendars").join("work.yaml");
    let state_dir = dir.join(".cicada");

    // Refuse to clobber authored files without --force.
    if !args.force {
        let existing: Vec<String> = [&manifest, &calendar]
            .into_iter()
            .filter(|p| p.exists())
            .map(|p| p.display().to_string())
            .collect();
        if !existing.is_empty() {
            eprintln!(
                "cicada init: {} already exist{} — pass --force to overwrite",
                existing.join(", "),
                if existing.len() == 1 { "s" } else { "" },
            );
            return ExitCode::FAILURE;
        }
    }

    if let Err(e) = scaffold(&manifest, &calendar, &state_dir, args.force) {
        eprintln!("cicada init: {e}");
        return ExitCode::FAILURE;
    }

    println!("initialized cicada project in {}", dir.display());
    println!("  cicada.yaml          — the manifest");
    println!("  calendars/work.yaml  — a starter calendar (fake provider demo)");
    println!("  .cicada/             — state dir (state.yaml lands here on apply)");
    println!();
    println!("next steps:");
    println!("  cicada validate              # schema + domain invariants");
    println!("  cicada plan                  # diff desired vs live (fake provider)");
    println!("  cicada apply                 # reconcile against the fake backend");
    println!("  cicada fmt                   # canonical YAML formatting");
    println!();
    println!("going real:");
    println!("  cicada provider add google   # OAuth flow / --service-account <key.json>");
    println!("  # then set metadata.provider: google and pass --provider google");
    println!();
    println!("git: .cicada/state.yaml is committable (ADR-0002 §7); the fake backend");
    println!("     and lock files are covered by .cicada/.gitignore. OAuth refresh");
    println!("     tokens live under ~/.local/state/cicada/secrets — never commit them.");
    ExitCode::SUCCESS
}

/// Write the scaffold. Existing files are only touched under
/// `force`; `.cicada/` content is additive.
fn scaffold(manifest: &Path, calendar: &Path, state_dir: &Path, force: bool) -> Result<(), String> {
    for (path, body) in [
        (manifest, MANIFEST),
        (calendar, CALENDAR),
        (&state_dir.join(".gitignore"), CICADA_GITIGNORE),
    ] {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .map_err(|e| format!("cannot create {}: {e}", parent.display()))?;
        }
        if path.exists() && !force {
            continue; // only authored files gate init — artifacts skip
        }
        fs::write(path, body).map_err(|e| format!("cannot write {}: {e}", path.display()))?;
    }
    Ok(())
}
