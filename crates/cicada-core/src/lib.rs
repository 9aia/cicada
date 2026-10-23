//! `cicada-core` — pure domain, ports, and planning scaffolding.
//!
//! Layered per ADR-0001: `domain` holds entities and value objects,
//! `application` the use cases, `ports` the seven traits implemented
//! by adapters, and `planning` the pure diff/plan machinery
//! (scaffolded in build step 1).
//!
//! Purity contract: no `std::fs`, no `tokio`, no network or HTTP
//! client types anywhere in this crate. All I/O lives behind the
//! port traits so the same core serves the CLI and the MCP server.

#![forbid(unsafe_code)]

pub mod application;
pub mod domain;
pub mod planning;
pub mod ports;

pub use domain::error::{CicadaError, ErrorKind, Result};
