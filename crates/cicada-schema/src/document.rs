//! Document envelope — `apiVersion`/`kind` dispatch (ADR-0001 §6,
//! ADR-0002 §9).
//!
//! Two documents exist:
//!
//! - `kind: Calendar` — `metadata` + `spec.events[]`, lowers to a
//!   domain [`Calendar`].
//! - `kind: Manifest` — the root `cicada.yaml` listing calendar files.
//!
//! `templates/`, `groups.yml`, and `$ref` composability are deferred
//! (ADR backlog); the envelope leaves room for them.

use std::collections::BTreeMap;
use std::path::PathBuf;

use cicada_core::domain::calendar::Calendar;
use cicada_core::domain::ids::ZoneId;
use serde::Deserialize;
use serde::de::IgnoredAny;

use crate::error::SchemaError;
use crate::event::EventSpec;

/// The only `apiVersion` this schema understands.
pub const API_VERSION: &str = "cicada/v1";

/// A parsed YAML document — the result of
/// [`crate::parse_document`].
#[derive(Debug)]
pub enum Document {
    /// `kind: Calendar` lowered to the domain type.
    Calendar(Calendar),
    /// `kind: Manifest` — calendar file paths, relative to the
    /// manifest's directory.
    Manifest(Manifest),
}

/// The root `cicada.yaml` manifest (ADR-0002 §9).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Manifest {
    /// Calendar files, relative to the manifest's directory.
    pub calendars: Vec<PathBuf>,
}

/// `kind:` discriminator.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
enum Kind {
    /// `kind: Calendar`.
    Calendar,
    /// `kind: Manifest`.
    Manifest,
}

/// `apiVersion` — validated at parse so a document from a future
/// format version fails loudly instead of misinterpreting.
#[derive(Debug, Clone, Copy)]
struct ApiVersion;

impl<'de> Deserialize<'de> for ApiVersion {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let v = String::deserialize(d)?;
        if v == API_VERSION {
            Ok(Self)
        } else {
            Err(serde::de::Error::custom(format!(
                "unsupported apiVersion {v:?} — expected {API_VERSION:?}"
            )))
        }
    }
}

/// First-pass envelope: version + kind, enough to dispatch and to
/// report envelope errors at their field paths. Everything else is
/// ignored here and checked by the concrete document pass.
#[derive(Debug, Deserialize)]
struct Probe {
    /// `apiVersion` — must be `cicada/v1`.
    #[serde(rename = "apiVersion")]
    api_version: ApiVersion,
    /// `kind` — selects the document shape.
    kind: Kind,
}

/// `kind: Calendar` body. `apiVersion`/`kind` were validated by the
/// probe; they exist here so `deny_unknown_fields` still covers the
/// whole mapping.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CalendarDoc {
    /// Format version (checked by [`Probe`]).
    #[serde(rename = "apiVersion")]
    api_version: IgnoredAny,
    /// Document kind (checked by [`Probe`]).
    kind: IgnoredAny,
    /// `metadata` — name, zone, provider binding.
    metadata: MetadataDoc,
    /// `spec` — the declared events.
    spec: SpecDoc,
}

/// `metadata:` block.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct MetadataDoc {
    /// Logical calendar name (`name: work`).
    name: String,
    /// IANA zone that wall-time starts resolve against.
    time_zone: String,
    /// Provider selector (`google`).
    #[serde(default)]
    provider: Option<String>,
    /// Concrete remote binding (`primary`).
    #[serde(default)]
    calendar_id: Option<String>,
}

/// `spec:` block.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SpecDoc {
    /// Declared events.
    #[serde(default)]
    events: Vec<EventSpec>,
}

/// `kind: Manifest` body.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ManifestDoc {
    /// Format version (checked by [`Probe`]).
    #[serde(rename = "apiVersion")]
    api_version: IgnoredAny,
    /// Document kind (checked by [`Probe`]).
    kind: IgnoredAny,
    /// Calendar files, relative to the manifest's directory.
    #[serde(default)]
    calendars: Vec<PathBuf>,
}

impl CalendarDoc {
    /// Lower to a domain [`Calendar`]: attach `metadata.timeZone`,
    /// convert each event, and key by stable id (which is also where
    /// duplicate ids surface).
    fn into_calendar(self) -> Result<Calendar, SchemaError> {
        // apiVersion/kind were validated by the probe pass; the fields
        // exist so deny_unknown_fields still covers the whole mapping.
        let _ = (self.api_version, self.kind);
        let metadata = self.metadata;
        let spec = self.spec;
        let zone = ZoneId::new(metadata.time_zone.clone())
            .map_err(|e| SchemaError::at("metadata.timeZone", e.to_string()))?;
        let mut calendar = Calendar {
            name: metadata.name,
            time_zone: zone.clone(),
            provider: metadata.provider,
            provider_calendar_id: metadata.calendar_id,
            events: BTreeMap::default(),
        };
        for (i, spec) in spec.events.into_iter().enumerate() {
            let path = format!("spec.events[{i}]");
            let event = spec.into_event(&zone, &path)?;
            calendar
                .insert(event)
                .map_err(|e| SchemaError::at(format!("{path}.id"), e.to_string()))?;
        }
        Ok(calendar)
    }
}

/// Deserialize a single-document YAML body, tracking the field path
/// through `serde_path_to_error` so errors read
/// `spec.events[0].start: …` instead of bare serde sprawl.
fn deserialize_doc<T: for<'de> Deserialize<'de>>(source: &str) -> Result<T, SchemaError> {
    let mut docs = serde_yaml::Deserializer::from_str(source);
    let Some(first) = docs.next() else {
        return Err(SchemaError::document("empty YAML document"));
    };
    let doc = serde_path_to_error::deserialize::<_, T>(first).map_err(|e| {
        let path = e.path().to_string();
        let inner = e.into_inner();
        // serde_path_to_error renders the root as "." and unknown
        // segments as "?" — those mean "no useful field path", so keep
        // the scanner location instead.
        if path.is_empty() || path == "." || path == "?" {
            // Root-level failure (syntax, missing top-level field) —
            // keep the scanner location when there is one.
            let mut err = SchemaError::syntax(&inner);
            if err.line.is_none() && err.message.is_empty() {
                err.message = inner.to_string();
            }
            err
        } else {
            SchemaError::at(path, inner.to_string())
        }
    })?;
    if docs.next().is_some() {
        return Err(SchemaError::document(
            "multi-document YAML is not supported — one document per file",
        ));
    }
    Ok(doc)
}

/// Parse `source` into a [`Document`], dispatching on `kind`.
pub(super) fn parse_any(source: &str) -> Result<Document, SchemaError> {
    let probe = deserialize_doc::<Probe>(source)?;
    let _ = probe.api_version; // validated at parse; only `kind` dispatches
    match probe.kind {
        Kind::Calendar => Ok(Document::Calendar(
            deserialize_doc::<CalendarDoc>(source)?.into_calendar()?,
        )),
        Kind::Manifest => {
            let doc = deserialize_doc::<ManifestDoc>(source)?;
            let _ = (doc.api_version, doc.kind);
            Ok(Document::Manifest(Manifest {
                calendars: doc.calendars,
            }))
        }
    }
}
