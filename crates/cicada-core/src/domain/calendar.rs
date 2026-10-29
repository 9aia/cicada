//! A declared calendar — the unit plans operate on.
//!
//! Per ADR-0002 §9, a `kind: Calendar` declares a *logical* calendar:
//! `name` is the DSL identity, `provider`/`provider_calendar_id` bind
//! it to a concrete remote calendar. Events are keyed by stable
//! [`LocalId`] in a `BTreeMap` so iteration order — and therefore
//! plan output — is deterministic.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::error::{CicadaError, Result};
use super::event::Event;
use super::ids::{LocalId, ZoneId};
use super::policy::SendUpdates;

/// A logical calendar: name + zone + declared events.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Calendar {
    /// Logical name (`metadata.name`), e.g. `work`.
    pub name: String,
    /// IANA zone (`metadata.timeZone`) that `Time::Wall` starts in
    /// this calendar resolve against.
    pub time_zone: ZoneId,
    /// Provider selector (`metadata.provider`), e.g. `google`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    /// Concrete remote binding (`metadata.calendarId`), e.g.
    /// `primary`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_calendar_id: Option<String>,
    /// Notification policy for writes (`spec.apply.sendUpdates`;
    /// default `ExternalOnly`, ADR-0002 §5). Interfaces may
    /// override — `--notify`, the `plan.apply` tool parameter.
    #[serde(default, skip_serializing_if = "SendUpdates::is_default")]
    pub send_updates: SendUpdates,
    /// Declared events keyed by stable local id.
    #[serde(default)]
    pub events: BTreeMap<LocalId, Event>,
}

impl Calendar {
    /// An empty named calendar in `zone`.
    ///
    /// # Errors
    /// [`CicadaError::Fatal`] for an unknown IANA zone.
    pub fn new(name: impl Into<String>, zone: impl Into<String>) -> Result<Self> {
        Ok(Self {
            name: name.into(),
            time_zone: ZoneId::new(zone)?,
            provider: None,
            provider_calendar_id: None,
            send_updates: SendUpdates::default(),
            events: BTreeMap::new(),
        })
    }

    /// Insert an event keyed by its own id.
    ///
    /// # Errors
    /// [`CicadaError::Fatal`] on a duplicate local id.
    pub fn insert(&mut self, event: Event) -> Result<()> {
        if self.events.contains_key(&event.id) {
            return Err(CicadaError::fatal(format!(
                "duplicate event id {:?} in calendar {:?}",
                event.id.as_str(),
                self.name
            )));
        }
        self.events.insert(event.id.clone(), event);
        Ok(())
    }

    /// Look an event up by local id.
    pub fn get(&self, id: &LocalId) -> Option<&Event> {
        self.events.get(id)
    }

    /// Number of declared events.
    pub fn len(&self) -> usize {
        self.events.len()
    }

    /// `true` when no events are declared.
    pub fn is_empty(&self) -> bool {
        self.events.is_empty()
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use jiff::SignedDuration;
    use jiff::civil::date;

    use super::*;
    use crate::domain::event::{EventBody, EventTime};
    use crate::domain::ids::ZoneId;
    use crate::domain::time::Time;

    fn event(id: &str) -> Event {
        Event::new(
            id,
            EventBody {
                summary: id.into(),
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
                transparency: crate::domain::event::Transparency::Opaque,
            },
        )
    }

    #[test]
    fn insert_rejects_duplicate_ids() {
        let mut cal = Calendar::new("work", "UTC").unwrap();
        cal.insert(event("standup")).unwrap();
        assert!(cal.insert(event("standup")).is_err());
        assert_eq!(cal.len(), 1);
    }

    #[test]
    fn events_iterate_in_id_order() {
        let mut cal = Calendar::new("work", "UTC").unwrap();
        cal.insert(event("zebra")).unwrap();
        cal.insert(event("alpha")).unwrap();
        let ids: Vec<&str> = cal.events.keys().map(LocalId::as_str).collect();
        assert_eq!(ids, ["alpha", "zebra"]);
    }

    #[test]
    fn is_empty_tracks_event_count() {
        let mut cal = Calendar::new("work", "UTC").unwrap();
        assert!(cal.is_empty());
        cal.insert(event("a")).unwrap();
        assert!(!cal.is_empty());
    }

    #[test]
    fn new_rejects_unknown_zone() {
        // `ZoneId::new` `?` arm.
        assert!(Calendar::new("work", "Not/AZone").is_err());
    }
}
