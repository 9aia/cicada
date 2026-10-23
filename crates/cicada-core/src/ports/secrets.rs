//! `SecretStore` — persist tokens/refresh tokens (ADR-0002 §7:
//! file adapter at `~/.local/state/cicada/secrets`, mode 0600;
//! OS keychain later).

use async_trait::async_trait;

use crate::domain::error::Result;

/// Keyed opaque secret storage. Keys are adapter-defined names like
/// `google/refresh-token`; values are raw secret strings.
#[async_trait]
pub trait SecretStore: Send + Sync {
    /// Fetch a secret; `Ok(None)` when absent.
    async fn get(&self, key: &str) -> Result<Option<String>>;

    /// Store (or overwrite) a secret.
    async fn set(&self, key: &str, value: &str) -> Result<()>;

    /// Remove a secret; absent keys are not an error.
    async fn delete(&self, key: &str) -> Result<()>;
}
