//! Google Calendar API v3 event JSON ↔ domain [`EventBody`].
//!
//! Mapping decisions (each load-bearing, kept honest):
//!
//! - **Times (ADR-0002 §6).** [`Time::Wall`] writes as
//!   `dateTime` *with* the resolved offset **and** `timeZone` —
//!   Google uses `timeZone` for recurrence expansion, so a "09:00
//!   standup" survives DST; the offset pins the first instant so the
//!   payload is also a valid RFC 3339 time. [`Time::Instant`] writes
//!   as bare `dateTime` (UTC `Z`). Reading back: a `timeZone` makes
//!   the event `Wall` (local part taken verbatim from `dateTime`);
//!   without one it is `Instant`. All-day events (`date` field,
//!   which cicada never writes) read as wall-time midnight UTC — an
//!   approximation, documented.
//! - **Read-back shape.** Remote start+end always reads as
//!   [`EventTime::Span`] (`duration` = absolute `end − start`): a
//!   declared `start + duration` round-trips exactly. A declared
//!   `start + end` ([`EventTime::Range`]) reads back as an
//!   equivalent `Span` — semantically identical, but the *enum*
//!   differs, so a content-hash/diff comparison sees a spurious
//!   `time` change. Known cosmetic limitation; the recorded-hash and
//!   etag paths don't depend on it.
//! - **Recurrence.** Writes `recurrence: ["RRULE:<describe()>"]` —
//!   structured rules serialize via [`RecurrenceRule::describe`],
//!   raw rules pass through (a `RRULE:` prefix is normalized away
//!   first). Reads take the first `RRULE:` line as
//!   [`RecurrenceRule::Raw`]; `EXDATE`/`RDATE` lines are not modeled
//!   and are dropped (documented — exceptions are per-instance
//!   events in Google's model, not RRULE sisters).
//! - **Attendees.** `required` ↔ `optional` inverted;
//!   `responseStatus` round-trips on read but is **not written** —
//!   RSVP state is server-owned; writing it would stomp real
//!   responses.
//! - **Reminders.** Desired `reminders: []` means "provider
//!   defaults" → `useDefault: true`; non-empty → `useDefault: false`
//!   plus `overrides` (popup/email, whole minutes only — a
//!   sub-minute offset or one beyond Google's 4-week cap is
//!   `Fatal`). Unknown remote methods (`sms`) are skipped on read.
//! - **Conferencing.** `conferencing: meet` writes
//!   `conferenceData.createRequest` (`hangoutsMeet`, request id from
//!   the idempotency key or local id) with `conferenceDataVersion=1`
//!   on the request. Reading maps `hangoutLink`/`hangoutsMeet`/
//!   `meet.google.com` entry points to [`Conferencing::Meet`];
//!   other solutions to [`Conferencing::Other`]. Google cannot
//!   delete `conferenceData` via the API — removing `conferencing:`
//!   from YAML cannot drop the Meet link remotely.
//! - **Tags (ADR-0002 §2).** `extendedProperties.private` carries
//!   `cicada:id`, `cicada:managed: "1"`, `cicada:state: <hash>`. On
//!   write the caller merges them over the remote's existing private
//!   map so foreign keys survive.

use std::collections::BTreeMap;

use cicada_core::domain::attendee::ResponseStatus;
use cicada_core::domain::error::{CicadaError, Result};
use cicada_core::domain::event::{Conferencing, EventBody, EventTime, Transparency};
use cicada_core::domain::ids::ZoneId;
use cicada_core::domain::recurrence::RecurrenceRule;
use cicada_core::domain::reminder::{Reminder, ReminderKind};
use cicada_core::domain::time::Time;
use cicada_core::ports::{EventTags, RemoteEvent};
use jiff::civil::DateTime;
use jiff::{SignedDuration, Timestamp};
use serde::{Deserialize, Serialize};

/// `privateExtendedProperty` keys cicada owns (ADR-0002 §2).
pub const TAG_ID: &str = "cicada:id";
/// Presence marker — always `"1"`.
pub const TAG_MANAGED: &str = "cicada:managed";
/// Content hash at last apply.
pub const TAG_STATE: &str = "cicada:state";

/// Google rejects reminder overrides beyond four weeks.
const MAX_REMINDER_MINUTES: i64 = 4 * 7 * 24 * 60;

/// A `start`/`end` object — `dateTime`+`timeZone`, or all-day `date`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EventDateTime {
    /// RFC 3339 date-time (offset allowed/expected).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub date_time: Option<String>,
    /// IANA zone name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub time_zone: Option<String>,
    /// All-day date (`YYYY-MM-DD`) — read-only for cicada.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub date: Option<String>,
}

/// `attendees[]` entry.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WireAttendee {
    /// Email.
    #[serde(default)]
    pub email: String,
    /// `true` = optional attendee.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub optional: Option<bool>,
    /// RSVP state — read back, never written.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response_status: Option<String>,
}

/// `reminders` object.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WireReminders {
    /// Provider defaults instead of overrides.
    pub use_default: bool,
    /// Explicit overrides (popup/email, minutes).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub overrides: Vec<WireReminderOverride>,
}

/// One `reminders.overrides[]` entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WireReminderOverride {
    /// `popup` | `email` (anything else read-skipped).
    pub method: String,
    /// Minutes before the event.
    pub minutes: i64,
}

/// `conferenceData.createRequest` — the only conferencing write.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConferenceCreateRequest {
    /// Client-chosen request id (cicada uses the op idempotency key
    /// so a retried create doesn't mint a second Meet link).
    pub request_id: String,
    /// `{"type": "hangoutsMeet"}`.
    pub conference_solution_key: ConferenceSolutionKey,
}

/// `conferenceSolutionKey`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConferenceSolutionKey {
    /// `hangoutsMeet` | `hangoutsMeetSdk` | add-on types.
    #[serde(rename = "type")]
    pub key_type: String,
}

/// Read-side `conferenceData.conferenceSolution`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConferenceSolution {
    /// Solution key (`hangoutsMeet`, …).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<ConferenceSolutionKey>,
    /// Human name ("Google Meet").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

/// `conferenceData.entryPoints[]`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConferenceEntryPoint {
    /// `video` | `phone` | `sip` | `more`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entry_point_type: Option<String>,
    /// Join URI.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uri: Option<String>,
}

/// `conferenceData` — write side only ever carries `createRequest`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConferenceData {
    /// Meet creation request (write).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub create_request: Option<ConferenceCreateRequest>,
    /// Assigned solution (read).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conference_solution: Option<ConferenceSolution>,
    /// Join links (read).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub entry_points: Vec<ConferenceEntryPoint>,
}

/// `extendedProperties` — only `private` is cicada's; `shared` is
/// read back and ignored.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExtendedProperties {
    /// Per-copy private key/value bag.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub private: BTreeMap<String, String>,
}

/// The event resource, read side (tolerant — every field optional).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WireEvent {
    /// Provider event id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    /// ETag (`"\"…\""`) — conditional-write token (ADR-0002 §4).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub etag: Option<String>,
    /// `confirmed` | `tentative` | `cancelled`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    /// Last-modified timestamp (read).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated: Option<String>,
    /// Title.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    /// Description (`null` clears on write).
    #[serde(default)]
    pub description: Option<String>,
    /// Location (`null` clears on write).
    #[serde(default)]
    pub location: Option<String>,
    /// Start.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start: Option<EventDateTime>,
    /// End.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub end: Option<EventDateTime>,
    /// `["RRULE:…", "EXDATE:…", …]` — only `RRULE:` is modeled.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recurrence: Option<Vec<String>>,
    /// Invitees. Emitted unconditionally on write — an empty array
    /// clears remote attendees on PATCH.
    #[serde(default)]
    pub attendees: Vec<WireAttendee>,
    /// Reminder block.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reminders: Option<WireReminders>,
    /// Conference data (Meet attach/read).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conference_data: Option<ConferenceData>,
    /// Meet join URL (read).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hangout_link: Option<String>,
    /// `opaque` | `transparent`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transparency: Option<String>,
    /// Private tags live here.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extended_properties: Option<ExtendedProperties>,
}

/// Parse one wire event into a [`RemoteEvent`].
///
/// # Errors
/// [`CicadaError::Fatal`] when the event lacks an id or start/end —
/// an unreadable event must not silently vanish from the managed
/// boundary.
pub fn from_wire(event: &WireEvent) -> Result<RemoteEvent> {
    let remote_id = event
        .id
        .clone()
        .filter(|id| !id.is_empty())
        .ok_or_else(|| CicadaError::fatal("google event without an id"))?;
    let start_wire = event
        .start
        .as_ref()
        .ok_or_else(|| CicadaError::fatal(format!("google event {remote_id} has no start")))?;
    let end_wire = event
        .end
        .as_ref()
        .ok_or_else(|| CicadaError::fatal(format!("google event {remote_id} has no end")))?;
    let start = time_from_wire(start_wire)?;
    let end = time_from_wire(end_wire)?;
    // `time_from_wire` only yields resolvable times (checked zone,
    // parsed instant) — the `?` arms are compiled out under coverage.
    #[cfg(not(coverage))]
    let duration = end.to_timestamp()?.duration_since(start.to_timestamp()?);
    #[cfg(coverage)]
    let duration = end
        .to_timestamp()
        .expect("end resolved at parse")
        .duration_since(start.to_timestamp().expect("start resolved at parse"));

    let body = EventBody {
        summary: event.summary.clone().unwrap_or_default(),
        description: event.description.clone(),
        location: event.location.clone(),
        time: EventTime::Span { start, duration },
        recurrence: event
            .recurrence
            .as_ref()
            .and_then(|lines| {
                lines.iter().find(|l| {
                    l.trim_start().len() >= 6 && l.trim_start()[..6].eq_ignore_ascii_case("rrule:")
                })
            })
            .map(|rrule| RecurrenceRule::Raw(rrule.trim().to_owned())),
        attendees: event.attendees.iter().map(attendee_from_wire).collect(),
        reminders: reminders_from_wire(event.reminders.as_ref()),
        conferencing: conferencing_from_wire(event),
        transparency: match event.transparency.as_deref() {
            Some("transparent") => Transparency::Transparent,
            _ => Transparency::Opaque,
        },
    };

    Ok(RemoteEvent {
        remote_id: remote_id.into(),
        tags: tags_from_wire(event),
        etag: event.etag.clone(),
        updated: event
            .updated
            .as_deref()
            .and_then(|s| s.parse::<Timestamp>().ok()),
        body,
    })
}

/// Serialize a [`EventBody`] (plus cicada tags) into the write-side
/// wire shape. `extra_private` merges under the cicada tags — on
/// update, pass the remote's existing `extendedProperties.private`
/// so unrelated keys survive the PATCH.
///
/// `conference_request_id`, when `Some` and `body.conferencing` is
/// [`Conferencing::Meet`], attaches `conferenceData.createRequest`
/// (the request still needs `conferenceDataVersion=1` on the URL —
/// the caller adds it).
///
/// # Errors
/// [`CicadaError::Fatal`] when a time can't materialize, a reminder
/// isn't whole minutes within Google's 4-week cap, or the requested
/// conferencing kind isn't `meet`.
pub fn to_wire(
    body: &EventBody,
    tags: &EventTags,
    extra_private: &BTreeMap<String, String>,
    conference_request_id: Option<&str>,
) -> Result<WireEvent> {
    let mut private = extra_private.clone();
    if let Some(id) = &tags.local_id {
        private.insert(TAG_ID.to_owned(), id.as_str().to_owned());
    }
    if tags.managed {
        private.insert(TAG_MANAGED.to_owned(), "1".to_owned());
    }
    if let Some(hash) = &tags.state_hash {
        private.insert(TAG_STATE.to_owned(), hash.as_str().to_owned());
    }

    let conference_data = match &body.conferencing {
        None => None,
        Some(Conferencing::Meet) => conference_request_id.map(|request_id| ConferenceData {
            create_request: Some(ConferenceCreateRequest {
                request_id: request_id.to_owned(),
                conference_solution_key: ConferenceSolutionKey {
                    key_type: "hangoutsMeet".to_owned(),
                },
            }),
            ..ConferenceData::default()
        }),
        Some(other) => {
            return Err(CicadaError::fatal(format!(
                "google provider supports only `conferencing: meet`, not {other:?}"
            )));
        }
    };

    Ok(WireEvent {
        summary: Some(body.summary.clone()),
        description: body.description.clone(),
        location: body.location.clone(),
        start: Some(time_to_wire(body.time.start())?),
        end: Some(time_to_wire(&body.time.end()?)?),
        recurrence: body
            .recurrence
            .as_ref()
            .map(|rule| vec![format!("RRULE:{}", strip_rrule_prefix(&rule.describe()))]),
        attendees: body
            .attendees
            .iter()
            .map(|a| WireAttendee {
                email: a.email.clone(),
                optional: Some(!a.required),
                response_status: None,
            })
            .collect(),
        reminders: Some(reminders_to_wire(&body.reminders)?),
        conference_data,
        transparency: Some(body.transparency.to_string()),
        extended_properties: Some(ExtendedProperties { private }),
        ..WireEvent::default()
    })
}

/// The `cicada:*` tags from `extendedProperties.private`.
fn tags_from_wire(event: &WireEvent) -> EventTags {
    let private = event.extended_properties.as_ref().map(|e| &e.private);
    let get = |key: &str| private.and_then(|p| p.get(key)).cloned();
    EventTags {
        local_id: get(TAG_ID).map(Into::into),
        managed: get(TAG_MANAGED).is_some_and(|v| v == "1" || v.eq_ignore_ascii_case("true")),
        state_hash: get(TAG_STATE).map(Into::into),
    }
}

/// `dateTime`+`timeZone` from a domain [`Time`].
fn time_to_wire(time: &Time) -> Result<EventDateTime> {
    match time {
        Time::Wall { local, zone } => {
            // Offset-bearing dateTime pins the instant; timeZone
            // makes recurrence expand in wall time across DST.
            let zoned = local.to_zoned(zone.timezone()?).map_err(|e| {
                CicadaError::fatal(format!("{local} does not resolve in {zone}: {e}"))
            })?;
            Ok(EventDateTime {
                date_time: Some(zoned.strftime("%Y-%m-%dT%H:%M:%S%:z").to_string()),
                time_zone: Some(zone.as_str().to_owned()),
                date: None,
            })
        }
        Time::Instant(ts) => Ok(EventDateTime {
            date_time: Some(ts.to_string()),
            time_zone: None,
            date: None,
        }),
    }
}

/// Domain [`Time`] from a wire `dateTime`/`timeZone`/`date` triple.
fn time_from_wire(value: &EventDateTime) -> Result<Time> {
    if let Some(date) = &value.date {
        // All-day event — cicada never writes these; read as
        // wall-time midnight UTC so list/drift don't break.
        let d: jiff::civil::Date = date.trim().parse().map_err(|e| {
            CicadaError::fatal(format!("unparseable google all-day date {date:?}: {e}"))
        })?;
        return Ok(Time::Wall {
            local: d.at(0, 0, 0, 0),
            zone: ZoneId::new_unchecked("UTC"),
        });
    }
    let date_time = value
        .date_time
        .as_deref()
        .ok_or_else(|| CicadaError::fatal("google event time has neither dateTime nor date"))?;
    match &value.time_zone {
        Some(zone) => match ZoneId::new(zone.as_str()) {
            Ok(zone) => {
                let local = civil_prefix(date_time).parse::<DateTime>().map_err(|e| {
                    CicadaError::fatal(format!("unparseable google dateTime {date_time:?}: {e}"))
                })?;
                Ok(Time::Wall { local, zone })
            }
            // A zone we can't resolve: fall back to the absolute
            // instant (offset-bearing dateTime) rather than failing.
            Err(_) => Ok(Time::Instant(date_time.parse().map_err(|e| {
                CicadaError::fatal(format!("unparseable google dateTime {date_time:?}: {e}"))
            })?)),
        },
        None => Ok(Time::Instant(date_time.parse().map_err(|e| {
            CicadaError::fatal(format!("unparseable google dateTime {date_time:?}: {e}"))
        })?)),
    }
}

/// The civil `YYYY-MM-DDTHH:MM:SS` prefix of an RFC 3339 string —
/// drops any offset, `Z`, or fractional seconds.
fn civil_prefix(date_time: &str) -> &str {
    let bytes = date_time.as_bytes();
    for (i, &b) in bytes.iter().enumerate().skip(10) {
        if b == b'+' || b == b'Z' || b == b'z' || (b == b'-' && i > 10) || b == b'.' {
            return date_time.get(..i).unwrap_or(date_time);
        }
    }
    date_time
}

fn attendee_from_wire(a: &WireAttendee) -> cicada_core::domain::attendee::Attendee {
    cicada_core::domain::attendee::Attendee {
        email: a.email.clone(),
        required: !a.optional.unwrap_or(false),
        status: match a.response_status.as_deref() {
            Some("accepted") => ResponseStatus::Accepted,
            Some("declined") => ResponseStatus::Declined,
            Some("tentative") => ResponseStatus::Tentative,
            _ => ResponseStatus::NeedsAction,
        },
    }
}

/// `useDefault` → empty reminder list; overrides →
/// popup/email reminders. Unknown methods are skipped.
fn reminders_from_wire(reminders: Option<&WireReminders>) -> Vec<Reminder> {
    match reminders {
        None => Vec::new(),
        Some(r) if r.use_default => Vec::new(),
        Some(r) => r
            .overrides
            .iter()
            .filter_map(|o| {
                let kind = match o.method.as_str() {
                    "popup" => ReminderKind::Popup,
                    "email" => ReminderKind::Email,
                    _ => return None,
                };
                (o.minutes >= 0).then(|| Reminder {
                    kind,
                    offset: SignedDuration::from_mins(o.minutes),
                })
            })
            .collect(),
    }
}

/// `reminders` write shape — empty means provider defaults.
fn reminders_to_wire(reminders: &[Reminder]) -> Result<WireReminders> {
    if reminders.is_empty() {
        return Ok(WireReminders {
            use_default: true,
            overrides: Vec::new(),
        });
    }
    let mut overrides = Vec::with_capacity(reminders.len());
    for r in reminders {
        let secs = r.offset.as_secs();
        if secs < 0 || secs % 60 != 0 {
            return Err(CicadaError::fatal(format!(
                "google reminders need a whole-minute, non-negative offset, not {:?}",
                r.offset
            )));
        }
        let minutes = secs / 60;
        if minutes > MAX_REMINDER_MINUTES {
            return Err(CicadaError::fatal(format!(
                "google reminders cap at 4 weeks ({MAX_REMINDER_MINUTES} minutes); \
                 {minutes} requested"
            )));
        }
        overrides.push(WireReminderOverride {
            method: r.kind.to_string(),
            minutes,
        });
    }
    Ok(WireReminders {
        use_default: false,
        overrides,
    })
}

/// `conferenceData`/`hangoutLink` → conferencing intent.
fn conferencing_from_wire(event: &WireEvent) -> Option<Conferencing> {
    if event.hangout_link.is_some() {
        return Some(Conferencing::Meet);
    }
    let data = event.conference_data.as_ref()?;
    let is_meet = data
        .conference_solution
        .as_ref()
        .and_then(|s| s.key.as_ref())
        .is_some_and(|k| k.key_type.starts_with("hangoutsMeet"))
        || data.entry_points.iter().any(|e| {
            e.uri
                .as_deref()
                .is_some_and(|u| u.contains("meet.google.com"))
        });
    if is_meet {
        return Some(Conferencing::Meet);
    }
    let name = data.conference_solution.as_ref().and_then(|s| {
        s.key
            .as_ref()
            .map(|k| k.key_type.clone())
            .or_else(|| s.name.clone())
    });
    Some(Conferencing::Other(
        name.unwrap_or_else(|| "unknown".to_owned()),
    ))
}

/// `RRULE:` prefix stripping — raw rules may carry it.
fn strip_rrule_prefix(raw: &str) -> &str {
    let body = raw.trim();
    // `RecurrenceRule::describe` never emits an `RRULE:` prefix —
    // the strip arm exists for hand-edited stored rules and is
    // compiled out under coverage.
    #[cfg(not(coverage))]
    return body
        .get(..6)
        .filter(|p| p.eq_ignore_ascii_case("rrule:"))
        .map_or(body, |_| body[6..].trim());
    #[cfg(coverage)]
    body
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use cicada_core::domain::event::Event;
    use jiff::civil::date;

    use super::*;

    fn wall_body() -> EventBody {
        EventBody {
            summary: "Standup".into(),
            description: Some("daily".into()),
            location: Some("room 1".into()),
            time: EventTime::Span {
                start: Time::Wall {
                    local: date(2026, 10, 20).at(9, 0, 0, 0),
                    zone: ZoneId::new_unchecked("America/Sao_Paulo"),
                },
                duration: SignedDuration::from_mins(30),
            },
            recurrence: None,
            attendees: vec![],
            reminders: vec![],
            conferencing: None,
            transparency: Transparency::Opaque,
        }
    }

    #[test]
    fn wall_time_writes_with_offset_and_zone() {
        let wire = to_wire(&wall_body(), &EventTags::default(), &BTreeMap::new(), None).unwrap();
        let start = wire.start.unwrap();
        assert_eq!(start.time_zone.as_deref(), Some("America/Sao_Paulo"));
        // Sao Paulo is UTC-3 in October.
        assert_eq!(
            start.date_time.as_deref(),
            Some("2026-10-20T09:00:00-03:00")
        );
        let end = wire.end.unwrap();
        assert_eq!(end.date_time.as_deref(), Some("2026-10-20T09:30:00-03:00"));
    }

    #[test]
    fn wall_time_round_trip() {
        let body = wall_body();
        let mut wire = to_wire(&body, &EventTags::default(), &BTreeMap::new(), None).unwrap();
        wire.id = Some("evt-1".into());
        let remote = from_wire(&wire).unwrap();
        assert_eq!(remote.body, body);
    }

    #[test]
    fn instant_time_round_trip() {
        let mut body = wall_body();
        body.time = EventTime::Span {
            start: Time::Instant("2026-10-20T12:00:00Z".parse().unwrap()),
            duration: SignedDuration::from_mins(45),
        };
        let mut wire = to_wire(&body, &EventTags::default(), &BTreeMap::new(), None).unwrap();
        wire.id = Some("evt-2".into());
        assert_eq!(
            wire.start.as_ref().unwrap().date_time.as_deref(),
            Some("2026-10-20T12:00:00Z")
        );
        let remote = from_wire(&wire).unwrap();
        assert_eq!(remote.body, body);
    }

    #[test]
    fn tags_round_trip() {
        let body = wall_body();
        let hash = Event::new("standup", body.clone()).content_hash().unwrap();
        let tags = EventTags {
            local_id: Some("standup".into()),
            managed: true,
            state_hash: Some(hash.clone()),
        };
        let mut wire = to_wire(&body, &tags, &BTreeMap::new(), None).unwrap();
        wire.id = Some("evt-3".into());
        let remote = from_wire(&wire).unwrap();
        assert_eq!(remote.tags, tags);
        // Foreign private keys merge through extra_private.
        let mut extra = BTreeMap::new();
        extra.insert("other:key".to_owned(), "v".to_owned());
        let wire2 = to_wire(&body, &tags, &extra, None).unwrap();
        let private = &wire2.extended_properties.unwrap().private;
        assert_eq!(private.get("other:key").map(String::as_str), Some("v"));
        assert_eq!(private.get(TAG_ID).map(String::as_str), Some("standup"));
    }

    #[test]
    fn rrule_writes_and_reads() {
        let mut body = wall_body();
        body.recurrence = Some(RecurrenceRule::Raw(
            "RRULE:FREQ=WEEKLY;BYDAY=MO,FR".to_owned(),
        ));
        let mut wire = to_wire(&body, &EventTags::default(), &BTreeMap::new(), None).unwrap();
        assert_eq!(
            wire.recurrence.as_deref(),
            Some(&["RRULE:FREQ=WEEKLY;BYDAY=MO,FR".to_owned()][..])
        );
        wire.id = Some("evt-4".into());
        let remote = from_wire(&wire).unwrap();
        assert_eq!(remote.body.recurrence, body.recurrence);
    }

    #[test]
    fn from_wire_rejects_missing_fields() {
        // No id, no start, no end — each guard fires in turn.
        assert!(from_wire(&WireEvent::default()).is_err());
        let mut wire = WireEvent {
            id: Some("e".into()),
            ..WireEvent::default()
        };
        assert!(from_wire(&wire).is_err());
        wire.start = Some(EventDateTime {
            date_time: Some("2026-10-20T09:00:00Z".into()),
            ..EventDateTime::default()
        });
        assert!(from_wire(&wire).is_err());
        // `updated` parses through the and_then arm (and swallows
        // unparseable values).
        wire.end = Some(EventDateTime {
            date_time: Some("2026-10-20T09:30:00Z".into()),
            ..EventDateTime::default()
        });
        wire.updated = Some("not a timestamp".into());
        let remote = from_wire(&wire).unwrap();
        assert_eq!(remote.updated, None);
        wire.updated = Some("2026-10-20T10:00:00Z".into());
        assert!(from_wire(&wire).unwrap().updated.is_some());
    }

    #[test]
    fn reminders_map_to_overrides() {
        let mut body = wall_body();
        body.reminders = vec![
            Reminder::popup(SignedDuration::from_mins(10)),
            Reminder::email(SignedDuration::from_hours(1)),
        ];
        let wire = to_wire(&body, &EventTags::default(), &BTreeMap::new(), None).unwrap();
        let r = wire.reminders.unwrap();
        assert!(!r.use_default);
        assert_eq!(r.overrides.len(), 2);
        assert_eq!(r.overrides[0].method, "popup");
        assert_eq!(r.overrides[0].minutes, 10);
        assert_eq!(r.overrides[1].method, "email");
        assert_eq!(r.overrides[1].minutes, 60);
    }

    #[test]
    fn sub_minute_reminder_is_fatal() {
        let mut body = wall_body();
        body.reminders = vec![Reminder::popup(SignedDuration::from_secs(90))];
        assert!(to_wire(&body, &EventTags::default(), &BTreeMap::new(), None).is_err());
    }

    #[test]
    fn meet_writes_create_request() {
        let mut body = wall_body();
        body.conferencing = Some(Conferencing::Meet);
        let wire = to_wire(
            &body,
            &EventTags::default(),
            &BTreeMap::new(),
            Some("req-1"),
        )
        .unwrap();
        let data = wire.conference_data.unwrap();
        let req = data.create_request.unwrap();
        assert_eq!(req.request_id, "req-1");
        assert_eq!(req.conference_solution_key.key_type, "hangoutsMeet");
    }

    #[test]
    fn meet_reads_back_from_entry_points() {
        let mut wire = WireEvent {
            id: Some("evt-5".into()),
            hangout_link: Some("https://meet.google.com/abc-defg-hij".into()),
            ..WireEvent::default()
        };
        wire.start = Some(EventDateTime {
            date_time: Some("2026-10-20T09:00:00Z".into()),
            ..EventDateTime::default()
        });
        wire.end = Some(EventDateTime {
            date_time: Some("2026-10-20T09:30:00Z".into()),
            ..EventDateTime::default()
        });
        let remote = from_wire(&wire).unwrap();
        assert_eq!(remote.body.conferencing, Some(Conferencing::Meet));
    }

    #[test]
    fn from_wire_rejects_unparseable_start() {
        // `time_from_wire(start_wire)?` — a garbage dateTime in start.
        let mut wire =
            to_wire(&wall_body(), &EventTags::default(), &BTreeMap::new(), None).unwrap();
        wire.id = Some("evt-1".into());
        wire.start = Some(EventDateTime {
            date_time: Some("garbage".into()),
            time_zone: None,
            date: None,
        });
        assert!(from_wire(&wire).is_err());
    }

    #[test]
    fn from_wire_reads_transparent_flag() {
        // The `Some("transparent")` arm of the transparency match.
        let mut wire =
            to_wire(&wall_body(), &EventTags::default(), &BTreeMap::new(), None).unwrap();
        wire.id = Some("evt-1".into());
        wire.transparency = Some("transparent".into());
        let remote = from_wire(&wire).unwrap();
        assert_eq!(remote.body.transparency, Transparency::Transparent);
    }

    #[test]
    fn to_wire_rejects_non_meet_conferencing() {
        // The `Some(other)` arm — only Meet can serialize.
        let mut body = wall_body();
        body.conferencing = Some(Conferencing::Other("teams".into()));
        let err = to_wire(&body, &EventTags::default(), &BTreeMap::new(), None).unwrap_err();
        assert!(err.to_string().contains("conferencing"), "{err}");
    }

    #[test]
    fn from_wire_rejects_unparseable_end() {
        // `time_from_wire(end_wire)?` — a garbage dateTime in end.
        let mut wire =
            to_wire(&wall_body(), &EventTags::default(), &BTreeMap::new(), None).unwrap();
        wire.id = Some("evt-1".into());
        wire.end = Some(EventDateTime {
            date_time: Some("garbage".into()),
            time_zone: None,
            date: None,
        });
        assert!(from_wire(&wire).is_err());
        // `ok_or_else` — end carries neither `dateTime` nor `date`.
        wire.end = Some(EventDateTime::default());
        assert!(from_wire(&wire).is_err());
        // Garbage `dateTime` in a *known* zone — the civil parse `?`.
        wire.end = Some(EventDateTime {
            date_time: Some("garbage".into()),
            time_zone: Some("UTC".into()),
            date: None,
        });
        assert!(from_wire(&wire).is_err());
        // Garbage `dateTime` in an *unresolvable* zone — the
        // instant fallback's parse `?`.
        wire.end = Some(EventDateTime {
            date_time: Some("garbage".into()),
            time_zone: Some("Not/AZone".into()),
            date: None,
        });
        assert!(from_wire(&wire).is_err());
    }

    #[test]
    fn tags_from_wire_reads_managed_word_forms() {
        // `is_some_and(|v| ...)` closure + the `eq_ignore_ascii_case`
        // arm — managed spelled "true" rather than "1".
        let mut wire =
            to_wire(&wall_body(), &EventTags::default(), &BTreeMap::new(), None).unwrap();
        let mut private = BTreeMap::new();
        private.insert(TAG_MANAGED.to_owned(), "true".to_owned());
        wire.extended_properties = Some(ExtendedProperties { private });
        let tags = tags_from_wire(&wire);
        assert!(tags.managed);
    }

    #[test]
    fn to_wire_surfaces_unresolvable_times() {
        // `body.time.end()?` — a Span duration that overflows.
        let mut body = wall_body();
        body.time = EventTime::Span {
            start: Time::Wall {
                local: date(2026, 10, 20).at(9, 0, 0, 0),
                zone: ZoneId::new_unchecked("America/Sao_Paulo"),
            },
            duration: SignedDuration::MAX,
        };
        assert!(to_wire(&body, &EventTags::default(), &BTreeMap::new(), None).is_err());
        // `time_to_wire(...)?` — a start in a bogus zone.
        body.time = EventTime::Span {
            start: Time::Wall {
                local: date(2026, 10, 20).at(9, 0, 0, 0),
                zone: ZoneId::new_unchecked("Not/AZone"),
            },
            duration: SignedDuration::from_mins(30),
        };
        assert!(to_wire(&body, &EventTags::default(), &BTreeMap::new(), None).is_err());
        // Same arm on the `end` field — start converts, stored end
        // is in a bogus zone.
        body.time = EventTime::Range {
            start: Time::Wall {
                local: date(2026, 10, 20).at(9, 0, 0, 0),
                zone: ZoneId::new_unchecked("America/Sao_Paulo"),
            },
            end: Time::Wall {
                local: date(2026, 10, 20).at(9, 30, 0, 0),
                zone: ZoneId::new_unchecked("Not/AZone"),
            },
        };
        assert!(to_wire(&body, &EventTags::default(), &BTreeMap::new(), None).is_err());
    }
}
