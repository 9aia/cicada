//! Identifier newtypes.
//!
//! - [`LocalId`] — the stable, user-declared id (`id: standup`) that
//!   anchors idempotent update/delete across runs (ADR-0001 §3).
//! - [`RemoteId`] — the provider-assigned event id recorded in state.
//! - [`ContentHash`] — `sha256:<hex>` of an event's canonical content;
//!   stored in recorded state and `cicada:state` remote tags.
//! - [`ZoneId`] — a validated IANA time-zone name
//!   (`America/Sao_Paulo`).

use std::fmt;

use serde::{Deserialize, Serialize};

macro_rules! string_newtype {
    ($(#[$meta:meta])* $name:ident) => {
        $(#[$meta])*
        #[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(pub String);

        impl $name {
            /// Wrap a string without validation.
            pub fn new(s: impl Into<String>) -> Self {
                Self(s.into())
            }

            /// Borrow the inner string.
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.0)
            }
        }

        impl From<String> for $name {
            fn from(s: String) -> Self {
                Self(s)
            }
        }

        impl From<&str> for $name {
            fn from(s: &str) -> Self {
                Self(s.to_owned())
            }
        }

        impl AsRef<str> for $name {
            fn as_ref(&self) -> &str {
                &self.0
            }
        }
    };
}

string_newtype! {
    /// Stable user-declared event id (`id: standup` in the DSL).
    /// This is the anchor that makes plan/apply idempotent: recorded
    /// state maps `LocalId -> RemoteId` so re-runs converge instead
    /// of duplicating.
    LocalId
}

string_newtype! {
    /// Provider-assigned event id (e.g. Google's event id).
    RemoteId
}

string_newtype! {
    /// `"sha256:<hex>"` content hash of an event's canonical form.
    /// Written to recorded state and to the `cicada:state` remote tag
    /// so diffs and state rebuilds can compare content, not strings.
    ContentHash
}

/// A validated IANA time-zone identifier (`America/New_York`).
///
/// Validation goes through jiff's bundled/system tz database, so a
/// `ZoneId` that constructed successfully will always resolve.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct ZoneId(String);

impl ZoneId {
    /// Validate `name` against the tz database.
    ///
    /// # Errors
    /// [`CicadaError::Fatal`] when `name` is not a known IANA zone.
    pub fn new(name: impl Into<String>) -> crate::domain::error::Result<Self> {
        let name = name.into();
        jiff::tz::TimeZone::get(&name).map_err(|e| {
            crate::domain::error::CicadaError::fatal(format!(
                "unknown IANA time zone {name:?}: {e}"
            ))
        })?;
        Ok(Self(name))
    }

    /// Wrap without validating — for trusted/internal paths where the
    /// zone was already resolved. Prefer [`ZoneId::new`].
    pub fn new_unchecked(name: impl Into<String>) -> Self {
        Self(name.into())
    }

    /// Borrow the zone name.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Resolve to a jiff [`jiff::tz::TimeZone`].
    ///
    /// # Errors
    /// Fails only if the tz database lost the zone since construction
    /// (a `ZoneId::new` value should always resolve).
    pub fn timezone(&self) -> crate::domain::error::Result<jiff::tz::TimeZone> {
        jiff::tz::TimeZone::get(&self.0).map_err(|e| {
            crate::domain::error::CicadaError::fatal(format!(
                "IANA zone {name} no longer resolves: {e}",
                name = self.0
            ))
        })
    }
}

impl fmt::Display for ZoneId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl TryFrom<String> for ZoneId {
    type Error = crate::domain::error::CicadaError;
    fn try_from(s: String) -> crate::domain::error::Result<Self> {
        Self::new(s)
    }
}

impl From<ZoneId> for String {
    fn from(z: ZoneId) -> Self {
        z.0
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn zone_id_validates_iana_names() {
        assert!(ZoneId::new("America/Sao_Paulo").is_ok());
        assert!(ZoneId::new("UTC").is_ok());
        assert!(ZoneId::new("Not/AZone").is_err());
    }

    #[test]
    fn zone_id_serde_is_a_string() {
        let z = ZoneId::new("Europe/Lisbon").unwrap();
        let s = serde_json::to_string(&z).unwrap();
        assert_eq!(s, "\"Europe/Lisbon\"");
        let back: ZoneId = serde_json::from_str(&s).unwrap();
        assert_eq!(back, z);
        assert!(serde_json::from_str::<ZoneId>("\"Bogus/Zone\"").is_err());
    }
}
