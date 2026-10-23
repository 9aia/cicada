//! `cicada-testkit` — reusable fakes and conformance helpers for
//! tests across the workspace.
//!
//! - [`FakeCalendarProvider`] — in-memory `CalendarProvider`
//!   honoring the `cicada:*` tagging contract (ADR-0002 §2), with
//!   configurable capabilities, idempotency-key dedupe, ETag
//!   conditional writes, and out-of-band-edit seams.
//! - [`MemoryStateStore`] — in-memory `StateStore` (state + journal).
//! - [`FakeClock`] — settable `Clock`.
//!
//! Golden fixtures for `plan-v1`/`state-v1` documents land here as
//! the wire formats stabilize (ADR-0002 §7/§11).

#![forbid(unsafe_code)]

pub mod clock;
pub mod file_provider;
pub mod provider;
pub mod state;

pub use clock::FakeClock;
pub use file_provider::FakeFileProvider;
pub use provider::{FakeCalendarProvider, FakeSnapshot};
pub use state::MemoryStateStore;
