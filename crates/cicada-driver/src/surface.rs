//! Embedded surface dispatch — the driver serving its own engine's
//! ops through `pavo-driver-kit`'s surface layer, no second binary
//! needed. `cli`/`mcp` argv verbs (feature-gated) plug this
//! [`Surface`] impl into `surface::cli::run` /
//! `surface::mcp::serve_stdio`.
//!
//! `op` reuses the same `cli.*` path wire queries take
//! (`engine_ops::run_verb` re-execs `run` in a child — output stays
//! off the serve stream). `surface` dispatches in-process through
//! `cicada::run_from`, which owns exit codes exactly like the
//! `cicada` bin — that's where the unbounded (`mcp`) and
//! host-interactive (`provider.add`) verbs live, since they need
//! inherited stdio/tty a captured op child can't provide. Surface
//! names may be dotted (`provider.add`) — they split back into
//! nested subcommands here.

use pavo_driver_kit::surface::{OpOutcome, OpSpec, Surface};
use serde_json::{Map, Value};

use crate::engine_ops;

/// cicada's [`Surface`] — engine dispatch, already linked.
pub struct Engine;

impl Surface for Engine {
    /// Same path as the `cli.<op>` wire query: re-exec `run` in a
    /// child, captured output → `OpOutcome`.
    fn op(&mut self, op: &OpSpec, params: &Map<String, Value>) -> OpOutcome {
        OpOutcome::from_response(&engine_ops::run_op(&op.name, params))
    }

    /// `cicada <name> <args>` in-process — for the surfaces
    /// (`mcp`, `provider.add`): inherited stdio. Bounded ops never
    /// reach here.
    fn surface(&mut self, name: &str, args: &[String]) -> i32 {
        let argv = std::iter::once("cicada".to_string())
            .chain(name.split('.').map(String::from))
            .chain(args.iter().cloned());
        cicada::run_from(argv)
    }
}
