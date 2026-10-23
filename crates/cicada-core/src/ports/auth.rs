//! `AuthProvider` — acquire credentials for a provider.

use std::fmt;

use async_trait::async_trait;
use jiff::Timestamp;
use serde::{Deserialize, Serialize};

use crate::domain::error::Result;

/// A bearer token. `Debug` and `Display` are redacted — secrets must
/// not leak into logs or plan output.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Token(String);

impl Token {
    /// Wrap a raw token string.
    pub fn new(raw: impl Into<String>) -> Self {
        Self(raw.into())
    }

    /// The raw token — for building `Authorization` headers only.
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Token {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Token([redacted])")
    }
}

impl fmt::Display for Token {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("[redacted]")
    }
}

/// Credentials for one provider session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Credentials {
    /// Scheme for the `Authorization` header (`Bearer`).
    #[serde(default = "default_scheme")]
    pub scheme: String,
    /// The access token (redacted in `Debug`).
    pub access_token: Token,
    /// Expiry, when known — callers should re-acquire before this.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<Timestamp>,
}

fn default_scheme() -> String {
    "Bearer".to_owned()
}

impl Credentials {
    /// `Authorization: Bearer <token>`-style header value.
    pub fn authorization_header(&self) -> String {
        format!("{} {}", self.scheme, self.access_token.expose())
    }

    /// Whether the credentials are past expiry at `now`
    /// (`false` when no expiry is recorded).
    pub fn is_expired(&self, now: Timestamp) -> bool {
        self.expires_at.is_some_and(|e| now >= e)
    }
}

/// Acquires credentials for a provider (OAuth refresh, service
/// account, static token — adapter's choice).
#[async_trait]
pub trait AuthProvider: Send + Sync {
    /// Return usable credentials, refreshing/re-acquiring as needed.
    /// Failures are `Auth` errors — never `Transient`, so callers
    /// surface the token flow instead of retrying blindly.
    async fn credentials(&self) -> Result<Credentials>;
}
