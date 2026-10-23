//! Application layer — the use cases interfaces drive (ADR-0001 §1).
//!
//! Use cases take domain data (never paths or handles), depend only
//! on port traits for I/O, and return structured results interfaces
//! render. `cicada validate` → [`ValidateCalendar`]; `plan`, `apply`,
//! `refresh`, `drift`, `range`, `show`, `fmt` land in later build
//! steps against the same shape.
//!
//! Nothing in this module performs I/O: the CLI/MCP shells read files
//! and call providers; use cases see only parsed, portable values.

pub mod validate;

pub use validate::{Diagnostic, Severity, ValidateCalendar, ValidationReport};
