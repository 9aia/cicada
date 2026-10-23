//! `cicada-mcp` — MCP server over stdio (ADR-0001 §8a). Thin
//! adapter: agents get the same `cicada-core` use cases the CLI
//! drives — `calendar.list`, `events.range`, `events.show`,
//! `state.drift`, `state.refresh`, `plan.preview`, and the
//! confirmation-gated `plan.apply`. All mutations flow through
//! declared YAML + plan/apply; there is no `events.create`
//! (ADR-0002 §10). See `README.md` for the tool contract.
//!
//! Layout: [`ctx`] wires the working root/provider/state into a
//! [`ctx::Session`]; [`tools`] is the transport-neutral dispatch
//! layer (arg decode → use case → rendered result); [`server`] is
//! the rmcp adapter — the only module where rmcp types appear.

#![forbid(unsafe_code)]

pub mod ctx;
pub mod server;
pub mod tools;

pub use ctx::{ServerOptions, Session};
pub use server::{CicadaServer, run_stdio, serve_stdio};
