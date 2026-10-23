//! `cicada-google` — the Google Calendar provider adapter
//! (ADR-0001 §10 step 6).
//!
//! Implements two ports from `cicada-core`:
//!
//! - [`GoogleCalendarProvider`] — [`CalendarProvider`] over the
//!   Calendar API v3: `events.list/get/insert/patch/delete` +
//!   `freeBusy.query`, `extendedProperties.private` tagging
//!   (ADR-0002 §2), etag `If-Match` conditional writes (§4),
//!   `sendUpdates` (§5), Meet `conferenceData` for
//!   `conferencing: meet`, and the §8 error taxonomy.
//! - [`GoogleAuthProvider`] — [`AuthProvider`]: service-account JWT
//!   bearer, OAuth installed-app refresh tokens, or a static token;
//!   resolved from the environment by [`GoogleAuthConfig::from_env`].
//!
//! Supporting pieces:
//!
//! - [`ReqwestHttp`] — the production [`Http`] impl (rustls). All
//!   HTTP in this workspace goes through the `Http` port, so tests
//!   drive the provider through `cicada_testkit::FixtureHttp` with
//!   zero network.
//! - [`RetryHttp`] — exponential backoff + jitter around any `Http`
//!   (retries live in the adapter layer, ADR-0001 §4).
//! - [`FileSecretStore`] — refresh-token persistence at
//!   `~/.local/state/cicada/secrets` (ADR-0002 §7).
//!
//! Not yet covered (deliberate): calendar provisioning
//! (`kind: Calendar` binds an existing `calendarId`, ADR-0002 §9),
//! event instances/`instances.list`, `EXDATE`/`RDATE`, `watch`/push
//! channels, attachments, and `events.move`. The Meet link a
//! `conferenceData` mints is read back as the `meet` intent —
//! `RemoteEvent` has no URL field to carry the link itself.

#![forbid(unsafe_code)]

pub mod auth;
pub mod event_json;
pub mod http;
pub mod provider;
pub mod retry;
pub mod secrets;

pub use auth::{
    AUTHORIZATION_URL, CALENDAR_SCOPE, GoogleAuthConfig, GoogleAuthProvider, REFRESH_TOKEN_KEY,
    SERVICE_ACCOUNT_KEY, ServiceAccountKey, TOKEN_URL, TokenExchange, authorization_url,
    default_secrets_dir,
};
pub use http::ReqwestHttp;
pub use provider::GoogleCalendarProvider;
pub use retry::{
    FixedJitter, Jitter, RandomJitter, RecordingSleeper, RetryHttp, RetryPolicy, Sleeper,
    TokioSleeper,
};
pub use secrets::FileSecretStore;
