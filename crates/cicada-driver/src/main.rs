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
//!   tree (`cicada::command`). Unbounded/host-interactive verbs
//!   (`mcp`, `provider.add`) are `surfaces`, never ops.
//! - `run <argv>` — engine passthrough: `tamias-driver-cicada run
//!   plan` ≡ `cicada plan`. Also the child side of `cli.*` wire
//!   queries (`engine_ops::run_verb` re-execs this same verb so
//!   engine output never touches the serve stream).
//!
//! Optional surfaces: features `cli`/`mcp` embed pavo-driver-kit's
//! surface layer — `cli <op> [args]` is a generated CLI over the
//! ops manifest (`mcp`/`provider.add` ride as streaming
//! subcommands), `mcp` serves the ops as MCP tools on stdio.
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
#[cfg(any(feature = "cli", feature = "mcp"))]
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
        // Embedded surface verbs (feature-gated, optional): the
        // pavo-driver-kit surface layer renders this driver's own
        // ops manifest — `cli` = generated clap CLI (op →
        // subcommand, `mcp`/`provider.add` stream), `mcp` = ops →
        // MCP tools over stdio.
        #[cfg(feature = "cli")]
        Some("cli") => {
            let m = kit::surface::OpsManifest::from_value(&drv::engine_ops::manifest())
                .unwrap_or_default();
            let argv: Vec<String> = std::iter::once("cicada".to_string())
                .chain(args.iter().skip(1).cloned())
                .collect();
            kit::surface::cli::run("cicada", &m, &argv, &mut drv::surface::Engine)
        }
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
