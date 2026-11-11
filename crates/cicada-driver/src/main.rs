//! `tamias-driver-cicada` — one binary, two consumers, disjoint
//! argv (the engine lives in `cicada_driver`, src/lib.rs).
//!
//! **pavo driver** (verbs take no `--dir`/`--name`):
//!
//! - `manifest` — print the [`pavo_protocol::DriverManifest`] JSON
//!   (discovery probe; never touches cicada state).
//! - `capabilities` — merged JSON advertising BOTH surfaces:
//!   pavo's `{"capabilities": ["pavo"], "verbs": […"serve"…]}`
//!   shape plus tamias's `"types": ["cicada"]` and the provider
//!   verbs in `verbs`. One payload satisfies both discoveries.
//! - `serve` — ndjson JSON-RPC on stdio. `query:"ops"` answers the
//!   engine ops manifest; `query:"cli.<verb>"` runs a bounded
//!   `cicada` CLI verb in a captured child.
//!
//! Engine surface (P-021 — the `cicada` CLI as a generated
//! surface):
//!
//! - `ops` — the ops manifest: every bounded `cicada` verb as a
//!   `cli.<verb>` wire query, derived from the engine's own clap
//!   tree (`cicada::command`). Host-interactive verbs
//!   (`provider.add`) are `surfaces`, never ops.
//! - `run <argv>` — engine passthrough: `tamias-driver-cicada run
//!   plan` ≡ `cicada plan`. Also the child side of `cli.*` wire
//!   queries (`engine_ops::run_verb` re-execs this same verb so
//!   engine output never touches the serve stream).
//!
//! `cli <cicada argv>` is the tamias `bins[]` shim verb (T-012):
//! `tamias bin exec` calls it with the standard `--dir`/`--name`
//! driver flags ahead of the user's argv; they're stripped and the
//! rest runs through `cicada::run_from` in-process with inherited
//! stdio — full engine parity a generated ops→CLI surface can't
//! express: `--help`/`--version`, globals before the verb, and the
//! interactive `provider.add` credential flow all behave exactly
//! like the retired `cicada` binary.
//!
//! Surface (feature `mcp`, default on since T-019 deleted
//! cicada-mcp): pavo-driver-kit's surface layer — `mcp` serves the
//! ops manifest as MCP tools on stdio, the ops→tools rendering of
//! the same `cli.*` queries `serve` answers. The engine's
//! preview→digest→apply gate rides those ops (`plan` issues a
//! persisted `planDigest`; `apply --plan-digest` enforces it), so
//! no session state is needed. There is no `cli` feature: `cli` is
//! the shim verb above, not a generated surface — the engine's own
//! argv grammar is the contract.
//!
//! **tamias content-type driver** (`type: cicada` packages — tamias
//! always passes `--dir <payload> --name <pkg> [--config <f>]`, so
//! these verbs are unreachable from pavo's bare probes):
//!
//! - `install` — validate the payload (manifest + calendar docs
//!   through cicada-schema/cicada-core); JSON report, no provider
//!   calls, nothing applied.
//! - `deploy` — refresh the operator config (`driver.json`) and run
//!   a **plan** (desired vs recorded vs live) via the configured
//!   provider; `plan.json` lands under the resolved state dir.
//!   Plan-only — the apply confirmation gate stays with the operator.
//! - `uninstall` — drift/managed-inventory JSON report; deletes no
//!   remote events (managed-boundary safe default).
//! - `status` — recorded-state summary; no provider calls.
//! - `config-schema` — JSON Schema (draft 2020-12) of the optional
//!   `--config` file (see `config.rs`).
//!
//! See `cicada_driver::verbs` for the verb implementations and
//! `cicada_driver::config` for the config format.

#![forbid(unsafe_code)]

use cicada_driver as drv;
#[cfg(feature = "mcp")]
use pavo_driver_kit as kit;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let code = match args.first().map(String::as_str) {
        // pavo driver verbs — bare argv, no `--dir`/`--name`.
        Some("manifest") => {
            println!(
                "{}",
                serde_json::to_string_pretty(&drv::manifest_value()).unwrap_or_default()
            );
            0
        }
        Some("capabilities") => {
            println!(
                "{}",
                serde_json::to_string_pretty(&drv::capabilities_value()).unwrap_or_default()
            );
            0
        }
        Some("serve") => drv::serve(),
        // Engine ops surface — `ops` prints the manifest derived
        // from `cicada::command()`; `run` is the engine passthrough
        // and the child side of `cli.*` wire queries.
        Some("ops") => {
            println!("{}", drv::engine_ops::manifest());
            0
        }
        Some("run") => cicada::run_from(
            std::iter::once("cicada".to_string()).chain(args.iter().skip(1).cloned()),
        ),
        // The tamias `bins[]` shim verb (T-012): `tamias bin exec`
        // runs `<driver> cli --dir <payload> --name <pkg> <cicada
        // argv>` for a declared `shim: cli` bin in
        // `libraries/cicada/tamias.yaml`. cicada's engine IS the
        // CLI, so this is a passthrough — strip the two standard
        // driver flags and hand the rest to `cicada::run_from`
        // with inherited stdio. That keeps full engine parity a
        // generated ops→CLI surface can't express: `--help`/
        // `--version` and the interactive `provider.add`
        // credential flow all behave exactly like the retired
        // `cicada` binary.
        Some("cli") => {
            let argv = engine_argv(&args[1..]);
            cicada::run_from(argv)
        }
        // Embedded surface (feature `mcp`, default on): the
        // pavo-driver-kit surface layer renders this driver's own
        // ops manifest as MCP tools over stdio — the ops→tools
        // rendering of the same `cli.*` queries `serve` answers.
        // This replaced cicada-mcp in T-019: the preview→digest→
        // apply gate now lives engine-side (`plan` issues a
        // persisted `planDigest`; `apply --plan-digest` enforces
        // it), so the ops carry the whole contract.
        #[cfg(feature = "mcp")]
        Some("mcp") => {
            let m = kit::surface::OpsManifest::from_value(&drv::engine_ops::manifest())
                .unwrap_or_default();
            match kit::surface::mcp::serve_stdio(&m, &mut drv::surface::Engine) {
                Ok(()) => 0,
                Err(e) => {
                    eprintln!("tamias-driver-cicada mcp: {e}");
                    1
                }
            }
        }
        // Tamias verbs — always invoked with `--dir`/`--name` by the
        // tamias dispatch contract; pavo never passes those flags,
        // and the clap front owns them (plus clap's own errors for
        // anything else).
        _ => drv::tamias_run(
            std::iter::once("tamias-driver-cicada".to_string()).chain(args.iter().cloned()),
        ),
    };
    std::process::exit(code);
}

/// `cli` verb argv (after the `cli` token) → engine argv (argv[0]
/// included). `tamias bin exec`/`driver exec` prepend the standard
/// driver flags — `--dir <payload>` `--name <pkg>` — which the
/// engine doesn't know; consume the leading run of those two
/// (`--flag v` and `--flag=v` forms) and pass everything else
/// through untouched. `--config` is deliberately NOT stripped:
/// cicada verbs resolve their own flags, so a user-passed one must
/// reach the engine's parse.
fn engine_argv(args: &[String]) -> Vec<String> {
    let mut out = vec!["cicada".to_string()];
    let mut i = 0;
    while i < args.len() {
        let a = args[i].as_str();
        if a == "--dir" || a == "--name" {
            // flag + separate value — skip both (a trailing flag
            // with no value just ends the scan).
            i = (i + 2).min(args.len());
        } else if a.starts_with("--dir=") || a.starts_with("--name=") {
            i += 1;
        } else {
            break;
        }
    }
    out.extend(args[i..].iter().cloned());
    out
}
