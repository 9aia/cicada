//! Ports — the seven traits adapters implement (ADR-0001 §2).
//!
//! Every method returns [`crate::domain::error::Result`]; the error
//! *kind* is part of the contract (ADR-0002 §8) — adapters must
//! classify 429/5xx/network as `Transient`, drift conflicts as
//! `Conflict`, credential failures as `Auth`, and everything else as
//! `Fatal`, because MCP surfaces the class so agents can decide
//! retry vs ask-human.
//!
//! All signatures use portable types only — no `reqwest`, `tokio`,
//! or filesystem types may appear here.

pub mod auth;
pub mod calendar_provider;
pub mod clock;
pub mod http;
pub mod logger;
pub mod secrets;
pub mod state_store;

pub use auth::{AuthProvider, Credentials, Token};
pub use calendar_provider::{CalendarProvider, CalendarRef, EventTags, RemoteEvent};
pub use clock::Clock;
pub use http::{Http, HttpMethod, HttpRequest, HttpResponse};
pub use logger::{LogLevel, Logger};
pub use secrets::SecretStore;
pub use state_store::StateStore;
