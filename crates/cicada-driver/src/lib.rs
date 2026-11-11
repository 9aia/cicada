//! cicada-driver — library half of `tamias-driver-cicada`, one
//! binary serving two consumers with disjoint argv (see `main.rs`).
//!
//! **tamias content-type driver** (`type: cicada` packages —
//! tamias always passes `--dir <payload> --name <pkg>
//! [--config <f>]`): [`verbs`] implements `install` (validate
//! only), `deploy` (plan-only), `uninstall`/`status` (read-only
//! reports), `config-schema`, behind the clap front in
//! [`tamias`]. [`config`] is the operator `--config` YAML model.
//!
//! **pavo view/engine driver**: [`engine_ops`] is the ops manifest
//! — every bounded `cicada` CLI verb as a `cli.<verb>` wire query,
//! derived from the engine's own clap tree (`cicada::command`).
//! `{"op":"query","query":"ops"}` answers it;
//! `query:"cli.<verb>"` re-execs `run` in a captured child and
//! returns `{op, argv, code, stdout, stderr}`. Unbounded /
//! host-interactive verbs (`mcp`, `provider.add`) ride `surfaces`.
//! This driver ships no views — `view`/`query` names outside
//! `ops`/`cli.*` answer `ok:false`.
//!
//! Optional surface: feature `mcp` embeds pavo-driver-kit's
//! surface layer — `mcp` serves the ops manifest as MCP tools on
//! stdio. `cli` is not a feature: it's the `bins[]` shim
//! passthrough verb in `main.rs`, always on.

pub mod config;
pub mod engine_ops;
#[cfg(feature = "mcp")]
pub mod surface;
mod tamias;
pub mod verbs;

use pavo_driver_kit as kit;
use pavo_protocol::DriverManifest;
use pavo_protocol::wire::{self, Request, Response, ViewSummary};
use serde_json::{Value, json};

/// The pavo driver manifest — no views (engine-ops driver);
/// `ops`/`run` are argv verbs, `serve`/`manifest` are the wire
/// verbs `kit` already declares.
fn manifest() -> DriverManifest {
    kit::Manifest::new("cicada", "Cicada")
        .version(env!("CARGO_PKG_VERSION"))
        .verb("ops")
        .verb("run")
        .verb("capabilities")
        .build()
}

/// `manifest` verb payload — the manifest as a JSON value.
pub fn manifest_value() -> Value {
    serde_json::to_value(manifest()).unwrap_or(Value::Null)
}

/// `capabilities` verb payload — merged for both consumers of this
/// binary. pavo's discovery checks `capabilities`/`verbs` for
/// `"pavo"`/`"serve"`; tamias's checks `types` for the content
/// kind and warns on undeclared `verbs`. One JSON answers both:
/// the kit's `{capabilities, verbs}` shape plus `"types":
/// ["cicada"]` and the tamias provider verbs folded into `verbs`.
pub fn capabilities_value() -> Value {
    let m = manifest();
    let mut v = kit::capabilities_value(&m);
    v["views"] = json!(0);
    v["types"] = json!(["cicada"]);
    // tamias is the other consumer of this binary — advertise it
    // alongside pavo, with the content-type verbs in `verbs`.
    if let Some(caps) = v["capabilities"].as_array_mut() {
        if !caps.iter().any(|c| c.as_str() == Some("tamias")) {
            caps.push(json!("tamias"));
        }
    }
    let mut verbs: Vec<Value> = v["verbs"].as_array().cloned().unwrap_or_default();
    // The tamias verb list comes from `verbs::capabilities` — one
    // source of truth for the content-type surface.
    if let Some(extra) = verbs::capabilities()["verbs"].as_array() {
        for v in extra {
            if !verbs.contains(v) {
                verbs.push(v.clone());
            }
        }
    }
    for extra in ["ops", "run"] {
        if !verbs.iter().any(|x| x.as_str() == Some(extra)) {
            verbs.push(json!(extra));
        }
    }
    // Feature-gated embedded surface — advertised only when the
    // binary actually answers it. (`cli` is the always-on bins[]
    // shim passthrough, not an advertised surface verb.)
    #[cfg(feature = "mcp")]
    if !verbs.iter().any(|x| x.as_str() == Some("mcp")) {
        verbs.push(json!("mcp"));
    }
    v["verbs"] = Value::Array(verbs);
    v
}

/// Dispatch one parsed request to its response.
pub fn dispatch(req: &Request) -> Response {
    match req {
        Request::Manifest => Response::ok_typed(&manifest()),
        Request::Views => Response::ok_typed(&Vec::<ViewSummary>::new()),
        Request::View { view, .. } => Response::err(format!(
            "tamias-driver-cicada claims no views (view '{view}')"
        )),
        Request::Render { kind, .. } => Response::err(format!(
            "tamias-driver-cicada claims no renderers (kind '{kind}')"
        )),
        Request::Query { query, params } => {
            // `ops` and `cli.<verb>` are engine ops; there are no
            // view queries — unknown names fail the op.
            if query == "ops" {
                return Response::ok_typed(&wire::QueryResult {
                    query: query.clone(),
                    data: engine_ops::manifest(),
                });
            }
            if let Some(op) = engine_ops::op_of(query) {
                return engine_ops::run_verb(&op, params);
            }
            Response::err(format!("unknown query: {query}"))
        }
        // `submit`/`jobs`/`notifications` — this driver claims
        // neither forms nor feeds; cicada mutations go through
        // `cli.<verb>`/`run`, never `submit`.
        Request::Submit { .. } | Request::Jobs | Request::Notifications => {
            Response::err("tamias-driver-cicada implements no submit/jobs/notifications ops")
        }
    }
}

/// The `serve` verb — ndjson JSON-RPC on stdio until EOF or a
/// broken pipe, via pavo-driver-kit's loop.
pub fn serve() -> i32 {
    // The kit reports only unexpected I/O errors; the historical
    // contract is exit 0 on EOF/EPIPE either way.
    let _ = kit::serve_stdio(dispatch);
    0
}

/// The tamias verb front — `install`/`deploy`/`uninstall`/
/// `status`/`config-schema` behind their clap parser. `argv`
/// includes argv[0] (the binary name). Returns the process exit
/// code; clap errors/help exit through clap itself.
pub fn tamias_run<I, T>(argv: I) -> i32
where
    I: IntoIterator<Item = T>,
    T: Into<std::ffi::OsString> + Clone,
{
    tamias::run_from(argv)
}
