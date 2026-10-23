//! `cicada-schema` — the YAML DSL: Kubernetes-style envelope
//! (`apiVersion: cicada/v1`, `kind:`) parsed into `cicada-core`
//! domain types.
//!
//! Two documents (ADR-0001 §6, ADR-0002 §9):
//!
//! ```yaml
//! apiVersion: cicada/v1
//! kind: Calendar
//! metadata:
//!   name: work
//!   timeZone: America/Sao_Paulo
//! spec:
//!   events:
//!     - id: standup
//!       summary: Daily Standup
//!       start: "09:00"          # bare wall clock → recurring anchor
//!       duration: 30m
//!       recurrence: { freq: WEEKLY, byDay: [MO,TU,WE,TH,FR] }
//! ```
//!
//! ```yaml
//! apiVersion: cicada/v1
//! kind: Manifest
//! calendars: [calendars/work.yaml]
//! ```
//!
//! This crate is validation **layer 1** (schema shape): YAML syntax,
//! required fields, scalar forms, and the lowering to domain types
//! (which includes duplicate ids and unknown zones, since the domain
//! types refuse them). Domain-invariant diagnostics are layer 2 —
//! [`cicada_core::application::ValidateCalendar`].
//!
//! All failures are [`SchemaError`]s carrying a field path
//! (`spec.events[0].start`) and — once the caller attaches it via
//! [`SchemaError::with_file`] — the source file. Templates,
//! `groups.yml`, and `$ref` composability are deferred (ADR backlog).

#![forbid(unsafe_code)]

mod document;
mod duration;
mod error;
mod event;

pub use document::{API_VERSION, Document, Manifest};
pub use error::SchemaError;

/// Parse `source` into whichever document its `kind` declares.
///
/// # Errors
/// [`SchemaError`] on malformed YAML, an unsupported `apiVersion`,
/// an unknown `kind`, a shape violation, or a value the domain types
/// refuse (bad zone, duplicate id).
pub fn parse_document(source: &str) -> Result<Document, SchemaError> {
    document::parse_any(source)
}

/// Parse `source` as `kind: Calendar`, lowering to a domain
/// [`cicada_core::domain::Calendar`].
///
/// # Errors
/// [`SchemaError`] as in [`parse_document`]; a `kind: Manifest`
/// document reports `expected kind: Calendar, found kind: Manifest`.
pub fn parse_calendar(source: &str) -> Result<cicada_core::domain::Calendar, SchemaError> {
    match parse_document(source)? {
        Document::Calendar(c) => Ok(c),
        Document::Manifest(_) => Err(SchemaError::at(
            "kind",
            "expected kind: Calendar, found kind: Manifest",
        )),
    }
}

/// Parse `source` as `kind: Manifest`.
///
/// # Errors
/// [`SchemaError`] as in [`parse_document`]; a `kind: Calendar`
/// document reports `expected kind: Manifest, found kind: Calendar`.
pub fn parse_manifest(source: &str) -> Result<Manifest, SchemaError> {
    match parse_document(source)? {
        Document::Manifest(m) => Ok(m),
        Document::Calendar(_) => Err(SchemaError::at(
            "kind",
            "expected kind: Manifest, found kind: Calendar",
        )),
    }
}
