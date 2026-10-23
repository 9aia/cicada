//! Error taxonomy per ADR-0002 §8.
//!
//! Every port error classifies as `Transient` (429, 5xx, network —
//! retryable, backoff + resume-safe), `Conflict` (drift conflict),
//! `Auth` (surface token flow), or `Fatal` (validation, capability).
//! MCP tools surface the *kind*, not just the message, so agents can
//! decide retry vs ask-human.

use std::time::Duration;

/// Classification of a [`CicadaError`], per ADR-0002 §8.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ErrorKind {
    /// Retryable: HTTP 429/5xx, network failures. Safe to resume.
    Transient,
    /// Live state diverged from recorded state mid-apply (ADR-0002 §4).
    Conflict,
    /// Credential acquisition/refresh failed; surface the token flow.
    Auth,
    /// Validation, capability, or logic errors. Never retryable.
    Fatal,
}

/// The single error type returned by every port and used throughout
/// the domain. Structured so callers can branch on [`ErrorKind`]
/// without parsing messages.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CicadaError {
    /// Retryable failure (429/5xx/network). `retry_after` carries the
    /// provider's Retry-After hint when present.
    #[error("transient error: {message}")]
    Transient {
        /// Human-readable description of what failed.
        message: String,
        /// Provider-suggested delay before retrying, if any.
        retry_after: Option<Duration>,
    },
    /// Drift conflict: live ≠ recorded for an op the plan touches.
    #[error("conflict: {0}")]
    Conflict(String),
    /// Auth failed — token refresh/acquire must be surfaced.
    #[error("auth error: {0}")]
    Auth(String),
    /// Non-retryable failure: validation, capability, invariant.
    #[error("{0}")]
    Fatal(String),
}

impl CicadaError {
    /// Convenience constructor for [`CicadaError::Transient`].
    pub fn transient(message: impl Into<String>) -> Self {
        Self::Transient {
            message: message.into(),
            retry_after: None,
        }
    }

    /// Convenience constructor for a transient error with an explicit
    /// provider retry hint.
    pub fn transient_retry_after(message: impl Into<String>, retry_after: Duration) -> Self {
        Self::Transient {
            message: message.into(),
            retry_after: Some(retry_after),
        }
    }

    /// Convenience constructor for [`CicadaError::Conflict`].
    pub fn conflict(message: impl Into<String>) -> Self {
        Self::Conflict(message.into())
    }

    /// Convenience constructor for [`CicadaError::Auth`].
    pub fn auth(message: impl Into<String>) -> Self {
        Self::Auth(message.into())
    }

    /// Convenience constructor for [`CicadaError::Fatal`].
    pub fn fatal(message: impl Into<String>) -> Self {
        Self::Fatal(message.into())
    }

    /// The [`ErrorKind`] class of this error.
    pub fn kind(&self) -> ErrorKind {
        match self {
            Self::Transient { .. } => ErrorKind::Transient,
            Self::Conflict(_) => ErrorKind::Conflict,
            Self::Auth(_) => ErrorKind::Auth,
            Self::Fatal(_) => ErrorKind::Fatal,
        }
    }

    /// Whether the operation may be retried (possibly after backoff).
    /// Only transient errors are retryable — conflicts need a refresh
    /// and re-plan, auth needs a token flow, fatal needs a fix.
    pub fn is_retryable(&self) -> bool {
        matches!(self, Self::Transient { .. })
    }

    /// Provider retry hint, if this is a transient error that carried
    /// one.
    pub fn retry_after(&self) -> Option<Duration> {
        match self {
            Self::Transient { retry_after, .. } => *retry_after,
            _ => None,
        }
    }
}

/// Crate-wide result alias; every port method returns this.
pub type Result<T> = std::result::Result<T, CicadaError>;

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn kinds_classify() {
        assert_eq!(CicadaError::transient("x").kind(), ErrorKind::Transient);
        assert_eq!(CicadaError::conflict("x").kind(), ErrorKind::Conflict);
        assert_eq!(CicadaError::auth("x").kind(), ErrorKind::Auth);
        assert_eq!(CicadaError::fatal("x").kind(), ErrorKind::Fatal);
    }

    #[test]
    fn only_transient_is_retryable() {
        assert!(CicadaError::transient("429").is_retryable());
        assert!(!CicadaError::conflict("drift").is_retryable());
        assert!(!CicadaError::auth("token").is_retryable());
        assert!(!CicadaError::fatal("bad").is_retryable());
    }

    #[test]
    fn retry_after_round_trips() {
        let e = CicadaError::transient_retry_after("slow down", Duration::from_secs(30));
        assert_eq!(e.retry_after(), Some(Duration::from_secs(30)));
        assert_eq!(CicadaError::fatal("x").retry_after(), None);
    }
}
