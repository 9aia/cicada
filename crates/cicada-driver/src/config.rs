//! The `--config` file `tamias-driver-cicada` consumes — a small YAML
//! (JSON parses too; YAML is a superset) document an operator passes
//! via `tamias deploy <pkg> --config <file>` or
//! `tamias driver exec <pkg> <verb> --config <file>`.
//!
//! ```yaml
//! provider: fake        # fake | google — default google (see below)
//! stateDir: ../cicada-state   # dir holding state.yaml + driver
//!                              # artifacts; default .cicada/ inside
//!                              # the installed payload
//! fakeFile: ../fake.json      # fake provider backend file
//! ```
//!
//! Relative `stateDir`/`fakeFile` paths resolve against the config
//! file's own directory, so a config can travel with the operator's
//! notes instead of pinning machine-absolute paths.
//!
//! **Provider default.** `provider` defaults to `google` — the real
//! use case; `fake` is the credential-free test/demo backend
//! (`cicada_shell::backend::FakeFileProvider`) and must be selected
//! explicitly. An unset provider on a `google` run with no
//! credentials fails loudly at provider construction — never a
//! silent fallback.
//!
//! **State placement.** The default state dir is `.cicada/` *inside*
//! the installed payload — which tamias treats as content-addressed
//! and evictable, so recorded state can vanish under store GC. Point
//! `stateDir` outside the payload (e.g. under `$TAMIAS_HOME` or an
//! operator dir) for durable deployments; `fakeFile` should move
//! with it for the same reason.

use std::fs;
use std::path::{Path, PathBuf};

use serde::Deserialize;

/// Provider used when `--config` is absent or `provider` is unset —
/// `google`, the real adapter. `fake` stays opt-in for tests/demos.
pub const DEFAULT_PROVIDER: &str = "google";

/// The parsed `--config` document. Every field is optional; an
/// absent file means all defaults.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DriverConfig {
    /// Provider adapter: `fake` or `google` (validated by
    /// `cicada_shell::backend::provider_name`).
    #[serde(default)]
    pub provider: Option<String>,
    /// Directory holding `state.yaml` plus the driver's own
    /// artifacts (`driver.json`, `plan.json`). Default:
    /// `.cicada/` beside the manifest inside the payload.
    #[serde(default)]
    pub state_dir: Option<PathBuf>,
    /// Fake-provider backend file (test/demo only). Default:
    /// `.cicada/fake-provider.json` beside the manifest.
    #[serde(default)]
    pub fake_file: Option<PathBuf>,
}

impl DriverConfig {
    /// The effective provider name — `provider` or
    /// [`DEFAULT_PROVIDER`].
    pub fn provider_name(&self) -> &str {
        self.provider.as_deref().unwrap_or(DEFAULT_PROVIDER)
    }
}

/// Load `--config`: `None` → all defaults; a path → YAML (JSON is
/// valid YAML) deserialized into [`DriverConfig`], with relative
/// paths rebased onto the config file's directory.
///
/// # Errors
/// Unreadable file or a document that doesn't match the schema
/// (unknown keys are rejected — see [`config_schema`]).
pub fn load(path: Option<&Path>) -> Result<DriverConfig, String> {
    let Some(path) = path else {
        return Ok(DriverConfig::default());
    };
    let source = fs::read_to_string(path)
        .map_err(|e| format!("cannot read config {}: {e}", path.display()))?;
    let mut config: DriverConfig = serde_yaml::from_str(&source)
        .map_err(|e| format!("{}: invalid driver config: {e}", path.display()))?;
    // A readable config path always has a parent — the "." fallback
    // exists only for a bare filename resolved to `/`; compiled out
    // under coverage.
    #[cfg(not(coverage))]
    let base = path
        .parent()
        .map_or_else(|| Path::new(".").to_path_buf(), Path::to_path_buf);
    #[cfg(coverage)]
    let base = path
        .parent()
        .expect("readable config has a parent")
        .to_path_buf();
    if let Some(dir) = &config.state_dir {
        config.state_dir = Some(rebase(dir, &base));
    }
    if let Some(file) = &config.fake_file {
        config.fake_file = Some(rebase(file, &base));
    }
    Ok(config)
}

/// Absolute `path` wins; a relative one joins `base` (the config
/// file's directory).
fn rebase(path: &Path, base: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        base.join(path)
    }
}

/// The JSON Schema (draft 2020-12) for the `--config` document —
/// tamias's reserved `config-schema` verb payload. Instance-agnostic:
/// describes the format, not any one deployment.
pub fn config_schema() -> serde_json::Value {
    serde_json::json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "$id": "https://9aia.com/cicada/tamias-driver-config.schema.json",
        "title": "tamias-driver-cicada --config",
        "description": "Operator config for the cicada tamias content driver. YAML (JSON parses too). All fields optional.",
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "provider": {
                "type": "string",
                "enum": ["fake", "google"],
                "default": DEFAULT_PROVIDER,
                "description": "Calendar provider adapter. 'google' is the real Calendar API v3 backend (credentials via CICADA_GOOGLE_* env / FileSecretStore); 'fake' is the file-backed test/demo backend and must be selected explicitly."
            },
            "stateDir": {
                "type": "string",
                "description": "Directory holding state.yaml plus driver artifacts (driver.json, plan.json). Default: .cicada/ inside the installed payload — evictable under tamias store GC, so durable deployments should point this outside the payload. Relative paths resolve against the config file's directory."
            },
            "fakeFile": {
                "type": "string",
                "description": "Fake-provider backend JSON file (provider: fake only). Default: .cicada/fake-provider.json beside the manifest. Relative paths resolve against the config file's directory."
            }
        }
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn defaults_when_no_config() {
        let cfg = load(None).unwrap();
        assert_eq!(cfg.provider_name(), "google");
        assert!(cfg.state_dir.is_none());
        assert!(cfg.fake_file.is_none());
    }

    #[test]
    fn parses_yaml_and_rebases_relative_paths() {
        let dir = tempfile::tempdir().unwrap();
        let cfg_path = dir.path().join("driver.yaml");
        std::fs::write(
            &cfg_path,
            "provider: fake\nstateDir: state\nfakeFile: /abs/fake.json\n",
        )
        .unwrap();
        let cfg = load(Some(&cfg_path)).unwrap();
        assert_eq!(cfg.provider.as_deref(), Some("fake"));
        assert_eq!(cfg.state_dir, Some(dir.path().join("state")));
        assert_eq!(cfg.fake_file, Some(PathBuf::from("/abs/fake.json")));
    }

    #[test]
    fn rejects_unknown_fields() {
        let dir = tempfile::tempdir().unwrap();
        let cfg_path = dir.path().join("driver.yaml");
        std::fs::write(&cfg_path, "provider: fake\napplyWindow: 2h\n").unwrap();
        assert!(load(Some(&cfg_path)).is_err());
    }

    #[test]
    fn schema_is_draft_2020_12_with_documented_fields() {
        let schema = config_schema();
        assert_eq!(
            schema["$schema"],
            "https://json-schema.org/draft/2020-12/schema"
        );
        assert_eq!(schema["additionalProperties"], false);
        for field in ["provider", "stateDir", "fakeFile"] {
            assert!(schema["properties"][field].is_object(), "{field}");
        }
        assert_eq!(
            schema["properties"]["provider"]["enum"],
            serde_json::json!(["fake", "google"])
        );
    }
}
