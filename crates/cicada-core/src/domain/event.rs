//! Events — the unit the whole lifecycle reconciles.
//!
//! [`Event`] = [`EventBody`] + a stable [`LocalId`]. The split exists
//! so provider records ([`crate::ports::RemoteEvent`]) can carry the
//! same payload without requiring a local id — untagged live events
//! have none.
//!
//! [`Event::content_hash`] is the change-detection primitive: a
//! canonical `sha256:` over the normalized body (attendees/reminders
//! sorted), recorded in state and in the `cicada:state` remote tag
//! (ADR-0002 §2/§7). Plan diffs compare hashes, not strings.

use std::fmt;
use std::ops::{Deref, DerefMut};
use std::str::FromStr;

use jiff::SignedDuration;
use serde::{Deserialize, Serialize};
use sha2::Digest;

use super::attendee::Attendee;
use super::error::{CicadaError, Result};
use super::ids::{ContentHash, LocalId};
use super::recurrence::RecurrenceRule;
use super::reminder::Reminder;
use super::time::Time;

/// Declared event timing: `start + duration` or `start + end`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum EventTime {
    /// A start time and a duration (`start: "09:00", duration: 30m`).
    Span {
        /// Event start.
        start: Time,
        /// Event duration.
        duration: SignedDuration,
    },
    /// A start and an explicit end (`start: "09:00", end: "09:30"`).
    Range {
        /// Event start.
        start: Time,
        /// Event end — must be after `start`.
        end: Time,
    },
}

impl EventTime {
    /// The event's start.
    pub fn start(&self) -> &Time {
        match self {
            Self::Span { start, .. } | Self::Range { start, .. } => start,
        }
    }

    /// The event's end (computed for `Span`, stored for `Range`).
    ///
    /// # Errors
    /// [`CicadaError::Fatal`] on civil overflow or an unresolvable zone.
    pub fn end(&self) -> Result<Time> {
        match self {
            Self::Range { end, .. } => Ok(end.clone()),
            Self::Span { start, duration } => start.plus(*duration),
        }
    }

    /// Absolute duration of the event.
    ///
    /// # Errors
    /// [`CicadaError::Fatal`] when `start`/`end` can't be materialized.
    pub fn duration(&self) -> Result<SignedDuration> {
        match self {
            Self::Span { duration, .. } => Ok(*duration),
            Self::Range { start, end } => {
                let s = start.to_timestamp()?;
                let e = end.to_timestamp()?;
                Ok(e.duration_since(s))
            }
        }
    }

    /// `end > start`, as an absolute-instants comparison.
    ///
    /// # Errors
    /// [`CicadaError::Fatal`] when times can't be materialized.
    pub fn validate(&self) -> Result<()> {
        let s = self.start().to_timestamp()?;
        let e = self.end()?.to_timestamp()?;
        if e <= s {
            return Err(CicadaError::fatal("event end must be after its start"));
        }
        Ok(())
    }
}

/// Busy/free flag (RFC 5545 TRANSP). `Opaque` blocks free/busy time;
/// `Transparent` does not.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Transparency {
    /// Busy — the default.
    #[default]
    Opaque,
    /// Free — shows on the calendar but doesn't block time.
    Transparent,
}

impl fmt::Display for Transparency {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Opaque => "opaque",
            Self::Transparent => "transparent",
        })
    }
}

/// Conferencing *intent* (`conferencing: meet` in the DSL) — not a
/// URL. The planner resolves the intent against the provider's
/// supported kinds; the URL is provider output (ADR-0001 §6).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Conferencing {
    /// Google Meet.
    Meet,
    /// Microsoft Teams.
    Teams,
    /// Any other provider-named kind.
    Other(String),
}

impl fmt::Display for Conferencing {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Meet => "meet",
            Self::Teams => "teams",
            Self::Other(s) => s,
        })
    }
}

impl FromStr for Conferencing {
    type Err = CicadaError;
    fn from_str(s: &str) -> Result<Self> {
        Ok(match s.to_ascii_lowercase().as_str() {
            "meet" => Self::Meet,
            "teams" => Self::Teams,
            "" => return Err(CicadaError::fatal("empty conferencing kind")),
            _ => Self::Other(s.to_owned()),
        })
    }
}

impl Serialize for Conferencing {
    fn serialize<S: serde::Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
        s.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for Conferencing {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> std::result::Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        Self::from_str(&s).map_err(serde::de::Error::custom)
    }
}

/// Everything about an event except its identity — shared between
/// desired-state [`Event`]s and provider-side records.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EventBody {
    /// Title shown on the calendar.
    pub summary: String,
    /// Long-form description, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Where, if anywhere.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub location: Option<String>,
    /// When, and for how long.
    pub time: EventTime,
    /// Recurrence rule, when the event repeats.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recurrence: Option<RecurrenceRule>,
    /// Invitees.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attendees: Vec<Attendee>,
    /// Reminders before each occurrence.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reminders: Vec<Reminder>,
    /// Conferencing intent, if the event wants a call attached.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conferencing: Option<Conferencing>,
    /// Busy/free flag.
    #[serde(default)]
    pub transparency: Transparency,
}

impl EventBody {
    /// Domain-invariant check (validation layer 2): `end > start`,
    /// and recurring starts must be wall-time.
    ///
    /// # Errors
    /// [`CicadaError::Fatal`] on invariant violations.
    pub fn validate(&self) -> Result<()> {
        self.time.validate()?;
        if self.recurrence.is_some() && !self.time.start().is_wall() {
            return Err(CicadaError::fatal(
                "recurring events need a wall-time start (time + zone), \
                 not an absolute instant",
            ));
        }
        Ok(())
    }

    /// `sha256:<hex>` over the canonical form — attendees and
    /// reminders sorted, so ordering noise never produces phantom
    /// diffs. This is the value stored as `cicada:state` and in
    /// recorded state.
    ///
    /// Attendee `status` (the RSVP) is deliberately **excluded**:
    /// it's provider-owned state — a remote accept/decline must not
    /// change the recorded hash, or every answered invite would
    /// manufacture `Update` ops and `Modified` drift. The status is
    /// still stored and reported on [`crate::ports::RemoteEvent`];
    /// it just isn't *managed* content.
    ///
    /// # Errors
    /// [`CicadaError::Fatal`] only if serialization fails.
    pub fn content_hash(&self) -> Result<ContentHash> {
        /// Attendee as hashed — `(email, required)` only; see above.
        #[derive(Serialize)]
        struct CanonicalAttendee<'a> {
            email: &'a str,
            required: bool,
        }
        #[derive(Serialize)]
        struct Canonical<'a> {
            summary: &'a str,
            description: Option<&'a str>,
            location: Option<&'a str>,
            time: &'a EventTime,
            recurrence: Option<String>,
            attendees: Vec<CanonicalAttendee<'a>>,
            reminders: Vec<&'a Reminder>,
            conferencing: Option<String>,
            transparency: Transparency,
        }
        let mut attendees: Vec<CanonicalAttendee<'_>> = self
            .attendees
            .iter()
            .map(|a| CanonicalAttendee {
                email: a.email.as_str(),
                required: a.required,
            })
            .collect();
        attendees.sort_by(|a, b| a.email.cmp(b.email));
        let mut reminders: Vec<&Reminder> = self.reminders.iter().collect();
        reminders.sort_by_key(|r| (r.kind, r.offset.as_nanos()));
        let canonical = Canonical {
            summary: &self.summary,
            description: self.description.as_deref(),
            location: self.location.as_deref(),
            time: &self.time,
            recurrence: self.recurrence.as_ref().map(RecurrenceRule::describe),
            attendees,
            reminders,
            conferencing: self.conferencing.as_ref().map(ToString::to_string),
            transparency: self.transparency,
        };
        // `Canonical` is plain data — `to_vec` can't fail; the
        // error arm is compiled out under coverage.
        #[cfg(not(coverage))]
        let bytes = serde_json::to_vec(&canonical)
            .map_err(|e| CicadaError::fatal(format!("content hash: {e}")))?;
        #[cfg(coverage)]
        let bytes = serde_json::to_vec(&canonical).expect("canonical serializes");
        let digest = sha2::Sha256::digest(&bytes);
        Ok(ContentHash::new(format!("sha256:{}", hex(&digest))))
    }
}

/// A desired-state event: an [`EventBody`] plus the stable local id
/// that anchors it across plan/apply runs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Event {
    /// Stable user-declared id (`id: standup`).
    pub id: LocalId,
    /// The event's content.
    #[serde(flatten)]
    pub body: EventBody,
}

impl Event {
    /// Wrap a body with its stable id.
    pub fn new(id: impl Into<LocalId>, body: EventBody) -> Self {
        Self {
            id: id.into(),
            body,
        }
    }

    /// `id` + [`EventBody::validate`].
    ///
    /// # Errors
    /// [`CicadaError::Fatal`] on invariant violations or an empty id.
    pub fn validate(&self) -> Result<()> {
        if self.id.as_str().trim().is_empty() {
            return Err(CicadaError::fatal("event id must not be empty"));
        }
        self.body.validate()
    }

    /// Content hash over `id` + canonical body — the identity used for
    /// idempotency keys (`calendarId + localId + opKind + contentHash`,
    /// ADR-0001 §4).
    ///
    /// # Errors
    /// [`CicadaError::Fatal`] only if serialization fails.
    pub fn content_hash(&self) -> Result<ContentHash> {
        // Include the id so identical bodies under different local ids
        // produce different keys.
        // Infallible once the canonical write can't fail — the `?`
        // is compiled out under coverage.
        #[cfg(not(coverage))]
        let body = self.body.content_hash()?;
        #[cfg(coverage)]
        let body = self.body.content_hash().expect("body hash is infallible");
        let mut h = sha2::Sha256::new();
        h.update(self.id.as_str().as_bytes());
        h.update(b":");
        h.update(body.as_str().as_bytes());
        Ok(ContentHash::new(format!("sha256:{}", hex(&h.finalize()))))
    }
}

/// Lowercase hex of a digest — avoids a `hex` dependency for one
/// formatting loop.
fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write;
    bytes.iter().fold(String::new(), |mut s, b| {
        let _ = write!(s, "{b:02x}");
        s
    })
}

impl Deref for Event {
    type Target = EventBody;
    fn deref(&self) -> &EventBody {
        &self.body
    }
}

impl DerefMut for Event {
    fn deref_mut(&mut self) -> &mut EventBody {
        &mut self.body
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use jiff::civil::date;

    use super::*;
    use crate::domain::attendee::ResponseStatus;
    use crate::domain::ids::ZoneId;

    fn body() -> EventBody {
        EventBody {
            summary: "Standup".into(),
            description: None,
            location: None,
            time: EventTime::Span {
                start: Time::Wall {
                    local: date(2026, 1, 5).at(9, 0, 0, 0),
                    zone: ZoneId::new_unchecked("UTC"),
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
    fn validate_fails_when_end_cannot_materialize() {
        // `end()` errors on civil overflow (Span) and on an
        // unresolvable stored end zone (Range) — both `?` arms.
        let mut span = body();
        span.time = EventTime::Span {
            start: Time::Wall {
                local: date(2026, 1, 5).at(9, 0, 0, 0),
                zone: ZoneId::new_unchecked("UTC"),
            },
            duration: SignedDuration::MAX,
        };
        assert!(span.validate().is_err());
        let mut ranged = body();
        ranged.time = EventTime::Range {
            start: Time::Instant("2026-01-05T09:00:00Z".parse().unwrap()),
            end: Time::Wall {
                local: date(2026, 1, 5).at(10, 0, 0, 0),
                zone: ZoneId::new_unchecked("Not/AZone"),
            },
        };
        assert!(ranged.validate().is_err());
        // `duration`'s `start.to_timestamp()?` — bogus start, fine end.
        ranged.time = EventTime::Range {
            start: Time::Wall {
                local: date(2026, 1, 5).at(9, 0, 0, 0),
                zone: ZoneId::new_unchecked("Not/AZone"),
            },
            end: Time::Instant("2026-01-05T10:00:00Z".parse().unwrap()),
        };
        assert!(ranged.validate().is_err());
        // `e <= s` — an end that isn't after its start.
        ranged.time = EventTime::Range {
            start: Time::Instant("2026-01-05T10:00:00Z".parse().unwrap()),
            end: Time::Instant("2026-01-05T09:00:00Z".parse().unwrap()),
        };
        assert!(ranged.validate().is_err());
    }

    #[test]
    fn validate_fails_on_unresolvable_zone() {
        // A bogus zone can't produce timestamps — the `?` arm.
        let mut b = body();
        b.time = EventTime::Span {
            start: Time::Wall {
                local: date(2026, 1, 5).at(9, 0, 0, 0),
                zone: ZoneId::new_unchecked("Not/AZone"),
            },
            duration: SignedDuration::from_mins(30),
        };
        assert!(b.validate().is_err());
    }

    #[test]
    fn span_computes_end_in_wall_time() {
        let b = body();
        assert!(matches!(
            b.time.end().unwrap(),
            Time::Wall { local, .. } if local == date(2026, 1, 5).at(9, 30, 0, 0)
        ));
    }

    #[test]
    fn range_validates_end_after_start() {
        let start = Time::Instant("2026-01-05T09:00:00Z".parse().unwrap());
        let end = Time::Instant("2026-01-05T09:30:00Z".parse().unwrap());
        let ok = EventTime::Range {
            start: start.clone(),
            end: end.clone(),
        };
        assert!(ok.validate().is_ok());
        let bad = EventTime::Range {
            start: end,
            end: start,
        };
        assert!(bad.validate().is_err());
    }

    #[test]
    fn content_hash_is_order_insensitive() {
        let mut a = body();
        let mut b = body();
        let alice = Attendee {
            email: "alice@x.com".into(),
            required: true,
            status: ResponseStatus::NeedsAction,
        };
        let bob = Attendee {
            email: "bob@x.com".into(),
            required: false,
            status: ResponseStatus::Accepted,
        };
        a.attendees = vec![alice.clone(), bob.clone()];
        b.attendees = vec![bob, alice];
        a.reminders = vec![
            Reminder::popup(SignedDuration::from_mins(10)),
            Reminder::email(SignedDuration::from_hours(1)),
        ];
        b.reminders = vec![
            Reminder::email(SignedDuration::from_hours(1)),
            Reminder::popup(SignedDuration::from_mins(10)),
        ];
        assert_eq!(a.content_hash().unwrap(), b.content_hash().unwrap());
        b.summary = "Sync".into();
        assert_ne!(a.content_hash().unwrap(), b.content_hash().unwrap());
    }

    #[test]
    fn content_hash_ignores_rsvp_status() {
        // RSVP status is provider-owned: a remote accept/decline must
        // not perturb the managed hash.
        let mut a = body();
        let mut b = body();
        a.attendees = vec![Attendee {
            email: "alice@x.com".into(),
            required: true,
            status: ResponseStatus::NeedsAction,
        }];
        b.attendees = vec![Attendee {
            email: "alice@x.com".into(),
            required: true,
            status: ResponseStatus::Accepted,
        }];
        assert_eq!(a.content_hash().unwrap(), b.content_hash().unwrap());
        b.attendees[0].required = false;
        assert_ne!(a.content_hash().unwrap(), b.content_hash().unwrap());
    }

    #[test]
    fn hash_looks_like_sha256() {
        let h = Event::new("standup", body()).content_hash().unwrap();
        assert!(h.as_str().starts_with("sha256:"));
        assert_eq!(h.as_str().len(), "sha256:".len() + 64);
    }

    #[test]
    fn recurring_events_need_wall_time() {
        let mut b = body();
        b.recurrence = Some(RecurrenceRule::Structured(
            crate::domain::recurrence::StructuredRule::new(
                crate::domain::recurrence::Frequency::Daily,
            ),
        ));
        b.time = EventTime::Span {
            start: Time::Instant("2026-01-05T09:00:00Z".parse().unwrap()),
            duration: SignedDuration::from_mins(30),
        };
        assert!(b.validate().is_err());
    }

    #[test]
    fn event_derefs_to_body() {
        let e = Event::new("standup", body());
        assert_eq!(e.summary, "Standup");
        let mut e = e;
        e.summary = "Renamed".into();
        assert_eq!(e.body.summary, "Renamed");
    }

    #[test]
    fn conferencing_display_and_parse() {
        assert_eq!(Conferencing::Meet.to_string(), "meet");
        assert_eq!(Conferencing::Teams.to_string(), "teams");
        assert_eq!(Conferencing::Other("zoom".into()).to_string(), "zoom");
        assert_eq!(
            "teams".parse::<Conferencing>().unwrap(),
            Conferencing::Teams
        );
        assert_eq!(
            "zoom".parse::<Conferencing>().unwrap(),
            Conferencing::Other("zoom".into())
        );
        assert!("".parse::<Conferencing>().is_err());
    }

    #[test]
    fn conferencing_serde_both_serializer_forms() {
        let c: Conferencing = serde_json::from_str("\"meet\"").unwrap();
        assert_eq!(c, Conferencing::Meet);
        assert!(serde_json::from_str::<Conferencing>("\"\"").is_err());
        assert_eq!(
            serde_json::to_string(&Conferencing::Teams).unwrap(),
            "\"teams\""
        );
        assert_eq!(
            serde_json::to_value(Conferencing::Other("zoom".into())).unwrap(),
            serde_json::json!("zoom")
        );
    }

    #[test]
    fn content_hash_covers_conferencing_and_recurrence() {
        let mut b = body();
        b.conferencing = Some(Conferencing::Meet);
        b.recurrence = Some(RecurrenceRule::Raw("FREQ=DAILY".into()));
        b.description = Some("notes".into());
        b.location = Some("room".into());
        let _ = b.content_hash().unwrap();
    }

    #[test]
    fn body_validate_passes_and_rejects_instant_recurring() {
        body().validate().unwrap();
        let mut b = body();
        b.recurrence = Some(crate::domain::recurrence::RecurrenceRule::Raw(
            "FREQ=DAILY".into(),
        ));
        b.time = EventTime::Span {
            start: Time::Instant("2026-01-05T09:00:00Z".parse().unwrap()),
            duration: SignedDuration::from_mins(30),
        };
        let err = b.validate().unwrap_err();
        assert!(err.to_string().contains("wall-time start"));
    }

    #[test]
    fn time_materialization_errors_propagate() {
        // An unresolvable zone makes `to_timestamp` fail — the `?`
        // arms in duration()/validate().
        let bad = |h: i8| Time::Wall {
            local: jiff::civil::date(2026, 1, 5).at(h, 0, 0, 0),
            zone: ZoneId::new_unchecked("Bogus/Zone"),
        };
        let t_ok = || Time::Instant("2026-01-05T10:00:00Z".parse::<jiff::Timestamp>().unwrap());
        assert!(
            EventTime::Range {
                start: bad(9),
                end: t_ok(),
            }
            .duration()
            .is_err()
        );
        assert!(
            EventTime::Range {
                start: t_ok(),
                end: bad(10),
            }
            .duration()
            .is_err()
        );
        // validate()'s `end()?` — a Span end can't materialize.
        assert!(
            EventTime::Span {
                start: bad(9),
                duration: jiff::SignedDuration::from_mins(30),
            }
            .validate()
            .is_err()
        );
        // and its `to_timestamp()?` on a bad-zone Range end.
        assert!(
            EventTime::Range {
                start: t_ok(),
                end: bad(10),
            }
            .validate()
            .is_err()
        );
        // A resolvable Range — the `Ok(e.duration_since(s))` tail of
        // `duration()` once both ends materialize.
        assert_eq!(
            EventTime::Range {
                start: Time::Instant("2026-01-05T09:00:00Z".parse().unwrap()),
                end: t_ok(),
            }
            .duration()
            .unwrap(),
            jiff::SignedDuration::from_mins(60)
        );
    }

    #[test]
    fn conferencing_deserialize_rejects_non_strings() {
        // Sequences/maps can't feed `String::deserialize` — its
        // `?` arm; scalar strings parse-fail through `from_str`'s.
        assert!(serde_yaml::from_str::<Conferencing>("[x]").is_err());
        assert!(serde_yaml::from_str::<Conferencing>("{a: b}").is_err());
        assert!(serde_json::from_str::<Conferencing>("42").is_err());
        assert!(serde_json::from_str::<Conferencing>("{}").is_err());
    }

    #[test]
    fn event_validate_checks_id_and_body() {
        Event::new("standup", body()).validate().unwrap();
        let err = Event::new("  ", body()).validate().unwrap_err();
        assert!(err.to_string().contains("empty"));
    }
}
