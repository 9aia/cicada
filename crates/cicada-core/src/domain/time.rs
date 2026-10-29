//! Domain time model per ADR-0002 §6.
//!
//! Recurring events are declared in local wall time (`"09:00"` +
//! `metadata.timeZone`). [`Time::Wall`] stores `(LocalDateTime, TzId)`
//! and materializes instants only at expansion — so DST shifts keep a
//! "09:00 standup" at 09:00 local. Bare-UTC storage of recurring
//! times is banned in core. Non-recurring absolute datetimes may use
//! [`Time::Instant`] with an explicit offset.

use jiff::{SignedDuration, Timestamp, Zoned, civil::DateTime};

use super::error::{CicadaError, Result};
use super::ids::ZoneId;

/// A point in time as declared by the user.
///
/// Two representations, per ADR-0002 §6:
/// - `Wall` — local civil time + IANA zone. Required for recurring
///   events so wall-time survives DST transitions.
/// - `Instant` — an absolute instant (explicit-offset datetime in the
///   DSL, or a remote API timestamp). Not valid as a recurrence start.
#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Time {
    /// Wall-clock time in an IANA zone.
    Wall {
        /// Civil date-time as written in the DSL (`2026-08-19T09:00`).
        local: DateTime,
        /// IANA zone name (`America/Sao_Paulo`).
        zone: ZoneId,
    },
    /// Absolute instant (DSL datetimes with explicit offsets map here).
    Instant(Timestamp),
}

impl Time {
    /// Construct a wall-time value, validating the zone.
    ///
    /// # Errors
    /// [`CicadaError::Fatal`] for an unknown IANA zone name.
    pub fn wall(local: DateTime, zone: &str) -> Result<Self> {
        Ok(Self::Wall {
            local,
            zone: ZoneId::new(zone)?,
        })
    }

    /// Materialize this time as an absolute instant.
    ///
    /// For `Wall`, jiff resolves the civil time in the zone — DST gaps
    /// and folds are disambiguated by jiff's defaults (gap → after,
    /// fold → earlier), matching RFC 5545 practice.
    ///
    /// # Errors
    /// [`CicadaError::Fatal`] if the zone no longer resolves.
    pub fn to_timestamp(&self) -> Result<Timestamp> {
        match self {
            Self::Instant(ts) => Ok(*ts),
            Self::Wall { .. } => Ok(self.to_zoned()?.timestamp()),
        }
    }

    /// Materialize as a [`Zoned`]. `Instant` is rendered in UTC.
    ///
    /// # Errors
    /// [`CicadaError::Fatal`] if the zone no longer resolves.
    pub fn to_zoned(&self) -> Result<Zoned> {
        match self {
            Self::Instant(ts) => Ok(ts.to_zoned(jiff::tz::TimeZone::UTC)),
            Self::Wall { local, zone } => local.to_zoned(zone.timezone()?).map_err(|e| {
                CicadaError::fatal(format!(
                    "local time {local} does not resolve in {zone}: {e}"
                ))
            }),
        }
    }

    /// The civil (wall) representation — the stored local time for
    /// `Wall`, the UTC civil time for `Instant`.
    pub fn local(&self) -> DateTime {
        match self {
            Self::Wall { local, .. } => *local,
            Self::Instant(ts) => ts.to_zoned(jiff::tz::TimeZone::UTC).datetime(),
        }
    }

    /// The zone this wall time is declared in, if any.
    pub fn zone(&self) -> Option<&ZoneId> {
        match self {
            Self::Wall { zone, .. } => Some(zone),
            Self::Instant(_) => None,
        }
    }

    /// `true` when this is a wall-time value (required for recurring
    /// event starts).
    pub fn is_wall(&self) -> bool {
        matches!(self, Self::Wall { .. })
    }

    /// Shift forward by an absolute duration.
    ///
    /// `Wall` stays wall-time: `09:00 + 30m` is `09:30` civil,
    /// regardless of what the clock did overnight. `Instant` shifts in
    /// absolute time.
    ///
    /// # Errors
    /// [`CicadaError::Fatal`] on civil overflow.
    pub fn plus(&self, duration: SignedDuration) -> Result<Self> {
        match self {
            Self::Instant(ts) => Ok(Self::Instant(*ts + duration)),
            Self::Wall { local, zone } => Ok(Self::Wall {
                local: local.checked_add(duration).map_err(|e| {
                    CicadaError::fatal(format!("adding {duration} to {local} overflowed: {e}"))
                })?,
                zone: zone.clone(),
            }),
        }
    }
}

/// A half-open interval `[start, end)` of absolute instants. Used for
/// `range` queries, free/busy, and bounding recurrence expansion.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TimeRange {
    /// Inclusive start instant.
    pub start: Timestamp,
    /// Exclusive end instant. Must be after `start`.
    pub end: Timestamp,
}

impl TimeRange {
    /// Construct, enforcing `start < end`.
    ///
    /// # Errors
    /// [`CicadaError::Fatal`] when `end <= start`.
    pub fn new(start: Timestamp, end: Timestamp) -> Result<Self> {
        if end <= start {
            return Err(CicadaError::fatal(format!(
                "invalid time range: end {end} is not after start {start}"
            )));
        }
        Ok(Self { start, end })
    }

    /// `true` when `ts` falls inside `[start, end)`.
    pub fn contains(&self, ts: Timestamp) -> bool {
        self.start <= ts && ts < self.end
    }

    /// `true` when the two half-open ranges overlap.
    pub fn overlaps(&self, other: Self) -> bool {
        self.start < other.end && other.start < self.end
    }

    /// Length of the range as an absolute duration.
    pub fn duration(&self) -> SignedDuration {
        self.end.duration_since(self.start)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use jiff::civil::date;

    use super::*;

    fn wall() -> Time {
        Time::wall(date(2026, 10, 20).at(9, 0, 0, 0), "America/Sao_Paulo").unwrap()
    }

    #[test]
    fn wall_time_materializes_an_instant() {
        // Sao Paulo is UTC-3 with no DST in October.
        let ts = wall().to_timestamp().unwrap();
        assert_eq!(ts.to_string(), "2026-10-20T12:00:00Z");
    }

    #[test]
    fn wall_time_keeps_civil_time() {
        assert_eq!(wall().local(), date(2026, 10, 20).at(9, 0, 0, 0));
        assert_eq!(wall().zone().unwrap().as_str(), "America/Sao_Paulo");
        assert!(wall().is_wall());
    }

    #[test]
    fn instant_times_are_absolute() {
        let ts = Timestamp::from_second(0).unwrap();
        let t = Time::Instant(ts);
        assert!(!t.is_wall());
        assert_eq!(t.to_timestamp().unwrap(), ts);
        assert_eq!(t.zone(), None);
    }

    #[test]
    fn plus_keeps_wall_semantics() {
        let end = wall().plus(SignedDuration::from_mins(30)).unwrap();
        assert!(matches!(end, Time::Wall { local, .. } if local.time().hour() == 9));
    }

    #[test]
    fn time_range_validates_and_contains() {
        let start = Timestamp::from_second(1_000).unwrap();
        let end = Timestamp::from_second(2_000).unwrap();
        let r = TimeRange::new(start, end).unwrap();
        assert!(r.contains(Timestamp::from_second(1_500).unwrap()));
        assert!(!r.contains(end)); // half-open
        assert!(TimeRange::new(end, start).is_err());
        assert!(TimeRange::new(start, start).is_err());
    }

    #[test]
    fn instant_zones_into_utc() {
        let ts = Timestamp::from_second(0).unwrap();
        let zoned = Time::Instant(ts).to_zoned().unwrap();
        assert_eq!(zoned.timestamp(), ts);
        assert_eq!(*zoned.time_zone(), jiff::tz::TimeZone::UTC);
    }

    #[test]
    fn wall_zoned_fails_when_zone_is_gone() {
        let t = Time::Wall {
            local: date(2026, 1, 5).at(9, 0, 0, 0),
            zone: crate::domain::ids::ZoneId::new_unchecked("Not/AZone"),
        };
        let err = t.to_zoned().unwrap_err();
        assert!(err.to_string().contains("no longer resolves"));
    }

    #[test]
    fn wall_plus_overflow_is_an_error() {
        assert!(wall().plus(SignedDuration::MAX).is_err());
    }

    #[test]
    fn time_range_overlap() {
        let a = TimeRange::new(
            Timestamp::from_second(0).unwrap(),
            Timestamp::from_second(100).unwrap(),
        )
        .unwrap();
        let b = TimeRange::new(
            Timestamp::from_second(50).unwrap(),
            Timestamp::from_second(150).unwrap(),
        )
        .unwrap();
        let c = TimeRange::new(
            Timestamp::from_second(100).unwrap(),
            Timestamp::from_second(200).unwrap(),
        )
        .unwrap();
        assert!(a.overlaps(b));
        assert!(!a.overlaps(c));
    }

    #[test]
    fn wall_time_serde_shape() {
        let t = wall();
        let json = serde_json::to_value(&t).unwrap();
        assert_eq!(
            json,
            serde_json::json!({
                "wall": { "local": "2026-10-20T09:00:00", "zone": "America/Sao_Paulo" }
            })
        );
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod edge_tests {
    use jiff::civil::date;

    use super::*;
    use crate::domain::ids::ZoneId;

    #[test]
    fn wall_time_past_timestamp_max_fails_to_materialize() {
        // 9999-12-31T23:00 in a UTC-minus zone is a valid civil time
        // but its instant exceeds Timestamp::MAX — to_zoned must
        // surface a clean error rather than panic. (DST gaps do NOT
        // error: jiff disambiguates them per the to_zoned docs.)
        let t = Time::Wall {
            local: date(9999, 12, 31).at(23, 0, 0, 0),
            zone: ZoneId::new("Etc/GMT+12").unwrap(),
        };
        assert!(t.to_zoned().is_err());
        assert!(t.to_timestamp().is_err());
    }

    #[test]
    fn wall_rejects_unknown_zone() {
        // `ZoneId::new` `?` arm — unlike `new_unchecked` below.
        assert!(Time::wall(date(2026, 1, 5).at(9, 0, 0, 0), "Not/AZone").is_err());
    }
}
