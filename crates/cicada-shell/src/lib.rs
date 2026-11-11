//! `cicada-shell` — the plumbing every user-facing interface shares.
//!
//! `cicada-core` is pure; adapters live in their own crates. Between
//! them sit the interfaces (the `cicada` engine, `tamias-driver-
//! cicada`) — and between *those* lives this crate: manifest
//! discovery and calendar loading
//! ([`input`]), provider/state wiring ([`backend`]), and time-bound
//! parsing ([`timeparse`]). Everything here is thin glue — the use
//! cases stay in core, and no business logic accumulates in the
//! shells.
//!
//! All functions take an explicit working `root` (the directory the
//! manifest is discovered in) so every interface shares one code
//! path.

#![forbid(unsafe_code)]

pub mod backend;
pub mod input;
pub mod timeparse;
