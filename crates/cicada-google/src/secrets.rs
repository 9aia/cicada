//! `FileSecretStore` — the file-backed [`SecretStore`] for
//! `~/.local/state/cicada/secrets` (ADR-0002 §7).
//!
//! One file per key, name percent-encoded (`google/refresh-token`
//! → `google%2Frefresh-token`), directory `0700`, files `0600`.
//! Kept in `cicada-google` because it's the only consumer today —
//! the layout is generic enough to move to a shared crate when a
//! second provider needs it.
//!
//! The trait is async but the I/O here is a few hundred bytes of
//! config-dir file access — it runs inline rather than on
//! `spawn_blocking` so the store works on any executor.

use std::path::PathBuf;

use async_trait::async_trait;
use cicada_core::domain::error::{CicadaError, Result};
use cicada_core::ports::SecretStore;

/// File-per-key secret store under a `0700` directory.
#[derive(Debug, Clone)]
pub struct FileSecretStore {
    dir: PathBuf,
}

impl FileSecretStore {
    /// A store rooted at `dir` (created lazily on first `set`).
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }

    /// A store at [`crate::auth::default_secrets_dir`].
    pub fn default_location() -> Self {
        Self::new(crate::auth::default_secrets_dir())
    }

    /// The on-disk path for `key` (also the test/debug hook).
    pub fn key_path(&self, key: &str) -> PathBuf {
        self.dir.join(encode_key(key))
    }

    fn fail(op: &str, key: &str, e: impl std::fmt::Display) -> CicadaError {
        CicadaError::fatal(format!("secret store: cannot {op} {key:?}: {e}"))
    }
}

#[async_trait]
impl SecretStore for FileSecretStore {
    async fn get(&self, key: &str) -> Result<Option<String>> {
        match std::fs::read_to_string(self.key_path(key)) {
            Ok(v) => Ok(Some(v.trim_end_matches('\n').to_owned())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(Self::fail("read", key, e)),
        }
    }

    async fn set(&self, key: &str, value: &str) -> Result<()> {
        std::fs::create_dir_all(&self.dir).map_err(|e| Self::fail("create dir for", key, e))?;
        #[cfg(unix)]
        std::fs::set_permissions(
            &self.dir,
            <std::fs::Permissions as std::os::unix::fs::PermissionsExt>::from_mode(0o700),
        )
        .map_err(|e| Self::fail("chmod dir for", key, e))?;
        write_private(&self.key_path(key), value.as_bytes())
            .map_err(|e| Self::fail("write", key, e))
    }

    async fn delete(&self, key: &str) -> Result<()> {
        match std::fs::remove_file(self.key_path(key)) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(Self::fail("delete", key, e)),
        }
    }
}

/// Write `bytes` to `path` mode `0600` (unix; default perms
/// elsewhere — the secrets dir is the guard there).
fn write_private(path: &std::path::Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let mut file = opts.open(path)?;
    file.write_all(bytes)
}

/// `key` → a safe filename: unreserved `[A-Za-z0-9._-]` pass
/// through, everything else (incl. `/`) percent-encodes.
fn encode_key(key: &str) -> String {
    key.bytes()
        .flat_map(|b| {
            if b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-') {
                vec![b]
            } else {
                format!("%{b:02X}").into_bytes()
            }
        })
        .map(|b| b as char)
        .collect()
}
