//! `spec.events[]` — the DSL event shape and its lowering to
//! [`Event`].
//!
//! Ergonomic scalars handled here:
//!
//! - `start`/`end` accept `"09:00"` (bare wall clock), a civil
//!   `YYYY-MM-DD[THH:MM[:SS]]` (attaches `metadata.timeZone`), or an
//!   RFC 3339 instant with explicit offset.
//! - `duration`/reminder offsets accept `30m`, `1h30m`, `2d`, `1w`.
//! - `recurrence` accepts `{freq: …, byDay: …}` (structured),
//!   `{rrule: "FREQ=…"}` or a bare `"FREQ=…"` string (raw RFC 5545).
//!
//! A bare `"HH:MM"` start is only meaningful for recurring events:
//! it anchors to the earliest date the rule generates on or after a
//! fixed epoch (1970-01-01), so parsed output is deterministic. Use
//! an explicit date when `count`/`until` matter — they count from
//! the anchor.

use cicada_core::domain::attendee::Attendee;
use cicada_core::domain::event::{Conferencing, Event, EventBody, EventTime, Transparency};
use cicada_core::domain::ids::ZoneId;
use cicada_core::domain::recurrence::{ByDay, Frequency, RecurrenceRule, StructuredRule, Weekday};
use cicada_core::domain::reminder::Reminder;
use cicada_core::domain::time::Time;
use jiff::civil::{Date, DateTime, Time as Clock};
use jiff::{SignedDuration, Span, Timestamp};
use serde::Deserialize;

use crate::duration::parse_duration;
use crate::error::SchemaError;

/// A duration written as `30m` / `1h30m` / `2d`. Deserializes from a
/// YAML string; a bare number errors with a unit hint.
#[derive(Debug, Clone, Copy)]
pub(crate) struct SpecDuration(SignedDuration);

impl<'de> Deserialize<'de> for SpecDuration {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        match serde_yaml::Value::deserialize(d)? {
            serde_yaml::Value::String(s) => parse_duration(&s)
                .map(Self)
                .map_err(serde::de::Error::custom),
            other => Err(serde::de::Error::custom(format!(
                "invalid duration — expected a string like \"30m\" or \"1h30m\", \
                 got {}",
                describe_value(&other)
            ))),
        }
    }
}

/// One `start`/`end` scalar, before the calendar zone is attached.
#[derive(Debug, Clone, Copy)]
enum TimeSpec {
    /// `"09:00"` — wall clock with no date.
    Clock(Clock),
    /// `"2026-08-19"` or `"2026-08-19T09:00"` — civil, attaches the
    /// calendar `timeZone`.
    Local(DateTime),
    /// `"2026-08-19T09:00:00-03:00"` — absolute instant.
    Instant(Timestamp),
}

impl<'de> Deserialize<'de> for TimeSpec {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        match serde_yaml::Value::deserialize(d)? {
            serde_yaml::Value::String(s) => parse_time(&s).map_err(serde::de::Error::custom),
            other => Err(serde::de::Error::custom(format!(
                "invalid time — expected \"HH:MM\", \"YYYY-MM-DD[THH:MM]\", or an \
                 RFC 3339 instant, got {}",
                describe_value(&other)
            ))),
        }
    }
}

/// Parse a `start`/`end` scalar. Instant first so offset-bearing
/// strings can't be mistaken for civil times.
fn parse_time(input: &str) -> Result<TimeSpec, String> {
    let s = input.trim();
    if let Ok(ts) = s.parse::<Timestamp>() {
        return Ok(TimeSpec::Instant(ts));
    }
    if let Ok(dt) = s.parse::<DateTime>() {
        return Ok(TimeSpec::Local(dt));
    }
    // jiff `DateTime` accepts a bare `YYYY-MM-DD` (midnight), so a
    // Date-only parse can never be reached — kept for when jiff's
    // grammar changes; compiled out under coverage.
    #[cfg(not(coverage))]
    if let Ok(d) = s.parse::<Date>() {
        return Ok(TimeSpec::Local(d.at(0, 0, 0, 0)));
    }
    if let Ok(t) = s.parse::<Clock>() {
        return Ok(TimeSpec::Clock(t));
    }
    Err(format!(
        "invalid time {s:?} — expected \"HH:MM\", \"YYYY-MM-DD[THH:MM]\", \
         or an RFC 3339 instant like \"2026-08-19T09:00:00-03:00\""
    ))
}

/// `recurrence:` — structured object, `{rrule: "…"}`, or a bare
/// RRULE string (domain [`RecurrenceRule`] accepts all three shapes
/// through this shim).
#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum RecurrenceSpec {
    /// `{rrule: "FREQ=WEEKLY;…"}` — raw RFC 5545 escape hatch.
    Raw {
        /// The RRULE value, with or without an `RRULE:` prefix.
        rrule: String,
    },
    /// `recurrence: "FREQ=WEEKLY;…"` — bare-string shorthand.
    Bare(String),
    /// `{freq: WEEKLY, byDay: [MO,FR], interval: …, …}`.
    Structured(StructuredRule),
}

impl RecurrenceSpec {
    fn into_rule(self) -> RecurrenceRule {
        match self {
            Self::Raw { rrule } | Self::Bare(rrule) => RecurrenceRule::Raw(rrule),
            Self::Structured(rule) => RecurrenceRule::Structured(rule.normalized()),
        }
    }
}

/// `reminders[]` — `{popup: "10m"}` or `{email: "1h"}`; exactly one
/// key required.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ReminderSpec {
    /// Popup/notification reminder.
    popup: Option<SpecDuration>,
    /// Email reminder.
    email: Option<SpecDuration>,
}

impl ReminderSpec {
    fn into_reminder(self, path: &str) -> Result<Reminder, SchemaError> {
        match (self.popup, self.email) {
            (Some(d), None) => Ok(Reminder::popup(d.0)),
            (None, Some(d)) => Ok(Reminder::email(d.0)),
            _ => Err(SchemaError::at(
                path,
                "reminder needs exactly one of `popup` or `email`",
            )),
        }
    }
}

/// One `spec.events[]` entry.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct EventSpec {
    /// Stable local id (`id: standup`) — the anchor for idempotent
    /// update/delete across runs.
    id: String,
    /// Title shown on the calendar.
    summary: String,
    /// Long-form description.
    #[serde(default)]
    description: Option<String>,
    /// Where.
    #[serde(default)]
    location: Option<String>,
    /// When it starts (see [`TimeSpec`] for accepted forms).
    start: TimeSpec,
    /// Explicit end — exclusive with `duration`.
    #[serde(default)]
    end: Option<TimeSpec>,
    /// Length — exclusive with `end`.
    #[serde(default)]
    duration: Option<SpecDuration>,
    /// Recurrence rule.
    #[serde(default)]
    recurrence: Option<RecurrenceSpec>,
    /// Invitees — domain [`Attendee`] shape (`email`, `required`,
    /// `status`).
    #[serde(default)]
    attendees: Vec<Attendee>,
    /// `{popup: "10m"}` / `{email: "1h"}` entries.
    #[serde(default)]
    reminders: Vec<ReminderSpec>,
    /// Conferencing intent (`meet`, `teams`, or a provider-named kind).
    #[serde(default)]
    conferencing: Option<Conferencing>,
    /// `opaque` (busy, default) or `transparent` (free).
    #[serde(default)]
    transparency: Transparency,
}

impl EventSpec {
    /// Lower to a domain [`Event`], attaching `zone` to civil times.
    /// `path` is the entry's field path (`spec.events[2]`).
    pub(crate) fn into_event(self, zone: &ZoneId, path: &str) -> Result<Event, SchemaError> {
        if self.end.is_some() && self.duration.is_some() {
            return Err(SchemaError::at(
                path,
                "specify `end` or `duration`, not both",
            ));
        }
        let recurrence = self.recurrence.map(RecurrenceSpec::into_rule);
        let start_path = format!("{path}.start");
        let start = match self.start {
            TimeSpec::Clock(t) => clock_start(t, recurrence.as_ref(), &start_path, zone)?,
            TimeSpec::Local(local) => Time::Wall {
                local,
                zone: zone.clone(),
            },
            TimeSpec::Instant(ts) => Time::Instant(ts),
        };
        let time = if let Some(d) = self.duration {
            EventTime::Span {
                start,
                duration: d.0,
            }
        } else if let Some(end) = self.end {
            EventTime::Range {
                end: resolve_end(end, &start, zone, &format!("{path}.end"))?,
                start,
            }
        } else {
            return Err(SchemaError::at(path, "event needs `end` or `duration`"));
        };
        let reminders = self
            .reminders
            .into_iter()
            .enumerate()
            .map(|(i, r)| r.into_reminder(&format!("{path}.reminders[{i}]")))
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Event::new(
            self.id,
            EventBody {
                summary: self.summary,
                description: self.description,
                location: self.location,
                time,
                recurrence,
                attendees: self.attendees,
                reminders,
                conferencing: self.conferencing,
                transparency: self.transparency,
            },
        ))
    }
}

/// Resolve a bare-clock start: only legal when `recurrence` is
/// present and maps onto the supported model. Anchors at the
/// earliest date the rule generates on/after [`ANCHOR_EPOCH`].
fn clock_start(
    t: Clock,
    recurrence: Option<&RecurrenceRule>,
    path: &str,
    zone: &ZoneId,
) -> Result<Time, SchemaError> {
    let Some(rule) = recurrence else {
        return Err(SchemaError::at(
            path,
            "bare \"HH:MM\" start needs `recurrence` — a one-off event \
             needs a date (\"2026-08-19T09:00\")",
        ));
    };
    let Some(rule) = rule.as_structured() else {
        return Err(SchemaError::at(
            path,
            "bare \"HH:MM\" start needs a recurrence rule in the supported \
             model — use an explicit dated `start` for exotic RRULEs",
        ));
    };
    let local = anchor_datetime(&rule, t).ok_or_else(|| {
        SchemaError::at(
            path,
            "recurrence rule generates no date to anchor \"HH:MM\" to — \
             the filters match nothing",
        )
    })?;
    Ok(Time::Wall {
        local,
        zone: zone.clone(),
    })
}

/// Resolve `end` against the already-resolved `start`. A bare-clock
/// end takes start's date, rolling one day forward when its
/// time-of-day is earlier (`23:00`→`01:00` is an overnight event).
/// Equal times stay on the same day so `end == start` still fails
/// layer-2 validation instead of silently becoming 24h.
fn resolve_end(
    end: TimeSpec,
    start: &Time,
    zone: &ZoneId,
    path: &str,
) -> Result<Time, SchemaError> {
    match (end, start) {
        (TimeSpec::Clock(t), Time::Wall { local, .. }) => {
            let date = if t < local.time() {
                local
                    .date()
                    .checked_add(Span::new().days(1))
                    .map_err(|e| SchemaError::at(path, format!("end date overflowed: {e}")))?
            } else {
                local.date()
            };
            Ok(Time::Wall {
                local: date.at(t.hour(), t.minute(), t.second(), t.subsec_nanosecond()),
                zone: zone.clone(),
            })
        }
        (TimeSpec::Clock(_), Time::Instant(_)) => Err(SchemaError::at(
            path,
            "bare \"HH:MM\" end needs a local `start` — use `duration` or \
             an absolute `end` with an instant start",
        )),
        (TimeSpec::Local(local), _) => Ok(Time::Wall {
            local,
            zone: zone.clone(),
        }),
        (TimeSpec::Instant(ts), _) => Ok(Time::Instant(ts)),
    }
}

/// Fixed epoch for bare-clock anchors — deterministic, so a parsed
/// calendar never depends on when it was read.
const ANCHOR_EPOCH: Date = Date::constant(1970, 1, 1);

/// Anchor scan bound: four years covers any solvable yearly rule
/// (`byMonth` + `byMonthDay` can skip at most ~13 months between
/// matches); exhaustion means the filters match nothing at all.
const ANCHOR_SCAN_DAYS: i64 = 4 * 366;

/// Earliest `date ≥ ANCHOR_EPOCH` the rule generates, at clock `t`.
/// Mirrors the expander's day-level filters so the anchor is a real
/// recurrence-set member, keeping `count` and weekly `interval`
/// parity honest.
///
/// `pub(crate)` — the `fmt` emitter reuses this to decide whether a
/// wall-time start can round-trip through the ergonomic `"HH:MM"`
/// form (emitting it is only safe when re-parsing yields the same
/// anchor).
pub(crate) fn anchor_datetime(rule: &StructuredRule, t: Clock) -> Option<DateTime> {
    let mut d = ANCHOR_EPOCH;
    for _ in 0..ANCHOR_SCAN_DAYS {
        if rule_generates_on(rule, d) {
            return Some(d.at(t.hour(), t.minute(), t.second(), t.subsec_nanosecond()));
        }
        // The scan starts at 1970 and walks ~4 years — it can't
        // reach the civil ceiling; the `?` arm is compiled out
        // under coverage.
        #[cfg(not(coverage))]
        {
            d = d.checked_add(Span::new().days(1)).ok()?;
        }
        #[cfg(coverage)]
        {
            d = d
                .checked_add(Span::new().days(1))
                .expect("anchor scan stays in range");
        }
    }
    None
}

/// Would `rule` generate an occurrence on date `d` (ignoring
/// `count`/`until`/`interval`)? Mirrors the bucket filters in
/// [`cicada_core::domain::RecurrenceExpander`]: `byMonth` applies to
/// every frequency, `byMonthDay` and non-ordinal `byDay` filter
/// daily/weekly, and ordinal `byDay` is resolved within the month for
/// monthly/yearly.
fn rule_generates_on(rule: &StructuredRule, d: Date) -> bool {
    if !rule.by_month.is_empty() && !rule.by_month.contains(&d.month()) {
        return false;
    }
    match rule.freq {
        Frequency::Daily | Frequency::Weekly => {
            if !rule.by_month_day.is_empty() && !month_day_match(rule, d) {
                return false;
            }
            rule.by_day.is_empty()
                || rule
                    .by_day
                    .iter()
                    .any(|b| b.ordinal.is_none() && b.weekday == weekday_of(d))
        }
        Frequency::Monthly | Frequency::Yearly => {
            if rule.by_day.iter().any(|b| b.ordinal.is_some()) {
                return rule.by_day.iter().any(|b| ordinal_match(*b, d))
                    && (rule.by_month_day.is_empty() || month_day_match(rule, d));
            }
            if !rule.by_month_day.is_empty() && !month_day_match(rule, d) {
                return false;
            }
            if !rule.by_day.is_empty() && !rule.by_day.iter().any(|b| b.weekday == weekday_of(d)) {
                return false;
            }
            // No day filters: the rule uses the anchor's own
            // day-of-month, so any date is self-consistent.
            true
        }
        // Not expandable anyway — any date anchors the wall time.
        Frequency::Hourly | Frequency::Minutely | Frequency::Secondly => true,
    }
}

/// `d.day()` membership in `byMonthDay` (negatives count from the
/// month end), matching the expander's `month_day_matches`.
fn month_day_match(rule: &StructuredRule, d: Date) -> bool {
    let dim = d.last_of_month().day();
    rule.by_month_day.iter().any(|&md| {
        let resolved = if md < 0 { dim + md + 1 } else { md };
        resolved == d.day()
    })
}

/// Is `d` the `b.ordinal`-th `b.weekday` of its month? (`1MO` = first
/// Monday, `-1FR` = last Friday.)
fn ordinal_match(b: ByDay, d: Date) -> bool {
    let Some(o) = b.ordinal else { return false };
    if b.weekday != weekday_of(d) {
        return false;
    }
    if o > 0 {
        // Same-weekday days share `day mod 7`: d is the
        // `((day-1)/7)+1`-th occurrence.
        i32::from((d.day() - 1) / 7 + 1) == i32::from(o)
    } else {
        let last = d.day() + 7 * ((d.last_of_month().day() - d.day()) / 7);
        i32::from((last - d.day()) / 7 + 1) == i32::from(-o)
    }
}

/// jiff civil weekday → domain [`Weekday`].
fn weekday_of(d: Date) -> Weekday {
    Weekday::from_jiff(d.weekday())
}

/// Short description of a YAML value for error messages.
fn describe_value(v: &serde_yaml::Value) -> String {
    match v {
        serde_yaml::Value::Null => "null".into(),
        serde_yaml::Value::Bool(b) => format!("bool {b}"),
        serde_yaml::Value::Number(n) => format!("number {n}"),
        serde_yaml::Value::String(s) => format!("string {s:?}"),
        serde_yaml::Value::Sequence(_) => "a sequence".into(),
        serde_yaml::Value::Mapping(_) => "a mapping".into(),
        serde_yaml::Value::Tagged(t) => format!("tagged value {}", t.tag),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn rule(yaml: &str) -> StructuredRule {
        let spec: RecurrenceSpec = serde_yaml::from_str(yaml).unwrap();
        spec.into_rule().as_structured().unwrap()
    }

    #[test]
    fn anchor_weekly_first_matching_weekday() {
        // 1970-01-01 was a Thursday — inside MO..FR.
        let r = rule("{freq: WEEKLY, byDay: [MO,TU,WE,TH,FR]}");
        let a = anchor_datetime(&r, "09:00".parse().unwrap()).unwrap();
        assert_eq!(a, Date::constant(1970, 1, 1).at(9, 0, 0, 0));

        // First Saturday on/after the epoch is 1970-01-03.
        let r = rule("{freq: WEEKLY, byDay: [SA]}");
        let a = anchor_datetime(&r, "09:00".parse().unwrap()).unwrap();
        assert_eq!(a.date(), Date::constant(1970, 1, 3));

        // No byDay → the anchor's own weekday recurs weekly.
        let r = rule("{freq: WEEKLY}");
        let a = anchor_datetime(&r, "09:00".parse().unwrap()).unwrap();
        assert_eq!(a.date(), Date::constant(1970, 1, 1));
    }

    #[test]
    fn anchor_daily_respects_byday_filter() {
        let r = rule("{freq: DAILY, byDay: [MO]}");
        let a = anchor_datetime(&r, "09:00".parse().unwrap()).unwrap();
        // First Monday ≥ 1970-01-01 is 1970-01-05.
        assert_eq!(a.date(), Date::constant(1970, 1, 5));
    }

    #[test]
    fn anchor_monthly_and_yearly() {
        let r = rule("{freq: MONTHLY, byMonthDay: [15]}");
        let a = anchor_datetime(&r, "09:00".parse().unwrap()).unwrap();
        assert_eq!(a.date(), Date::constant(1970, 1, 15));

        // Last Friday of January 1970 is the 30th.
        let r = rule("{freq: MONTHLY, byDay: [-1FR]}");
        let a = anchor_datetime(&r, "09:00".parse().unwrap()).unwrap();
        assert_eq!(a.date(), Date::constant(1970, 1, 30));

        let r = rule("{freq: YEARLY, byMonth: [6], byMonthDay: [15]}");
        let a = anchor_datetime(&r, "09:00".parse().unwrap()).unwrap();
        assert_eq!(a.date(), Date::constant(1970, 6, 15));
    }

    #[test]
    fn anchor_impossible_rule_exhausts() {
        // February never has a 30th.
        let r = rule("{freq: YEARLY, byMonth: [2], byMonthDay: [30]}");
        assert!(anchor_datetime(&r, "09:00".parse().unwrap()).is_none());
    }

    #[test]
    fn time_spec_parses_each_form() {
        assert!(matches!(parse_time("09:00").unwrap(), TimeSpec::Clock(_)));
        assert!(matches!(
            parse_time("2026-08-19").unwrap(),
            TimeSpec::Local(_)
        ));
        assert!(matches!(
            parse_time("2026-08-19T09:00").unwrap(),
            TimeSpec::Local(_)
        ));
        assert!(matches!(
            parse_time("2026-08-19T09:00:00-03:00").unwrap(),
            TimeSpec::Instant(_)
        ));
        assert!(matches!(
            parse_time("2026-08-19T09:00:00Z").unwrap(),
            TimeSpec::Instant(_)
        ));
        assert!(parse_time("nope").is_err());
    }

    #[test]
    fn overnight_clock_end_rolls_a_day() {
        let zone = ZoneId::new_unchecked("UTC");
        let start = Time::Wall {
            local: Date::constant(2026, 8, 19).at(23, 0, 0, 0),
            zone: zone.clone(),
        };
        let end = resolve_end(
            TimeSpec::Clock("01:00".parse().unwrap()),
            &start,
            &zone,
            "e.end",
        )
        .unwrap();
        #[rustfmt::skip]
        let Time::Wall { local, .. } = end else { unreachable!() };
        assert_eq!(local, Date::constant(2026, 8, 20).at(1, 0, 0, 0));
    }

    #[test]
    fn overnight_end_at_the_civil_ceiling_errors() {
        // Rolling a bare-clock end past 9999-12-31 overflows the
        // date arithmetic — the map_err arm.
        let zone = ZoneId::new_unchecked("UTC");
        let start = Time::Wall {
            local: Date::MAX.at(23, 0, 0, 0),
            zone: zone.clone(),
        };
        let err = resolve_end(
            TimeSpec::Clock("00:30".parse().unwrap()),
            &start,
            &zone,
            "e.end",
        )
        .unwrap_err();
        assert!(err.to_string().contains("overflowed"), "{err}");
    }

    #[test]
    fn missing_start_routes_through_timespec_missing_field() {
        // `start` is a required TimeSpec — a doc without it drives
        // TimeSpec::deserialize on serde's missing-field
        // deserializer.
        let err =
            serde_yaml::from_str::<EventSpec>("id: e\nsummary: s\nduration: 30m\n").unwrap_err();
        assert!(err.to_string().contains("start"), "{err}");
    }

    #[test]
    fn anchor_self_consistency_edge_rules() {
        // byMonthDay that excludes the anchor's own day-of-month →
        // the consistency check must see it.
        let r = rule("{freq: DAILY, byMonthDay: [15]}");
        assert!(!rule_generates_on(&r, Date::constant(2026, 8, 19)));
        assert!(rule_generates_on(&r, Date::constant(2026, 8, 15)));
        // MONTHLY with a plain (non-ordinal) byDay that excludes the
        // anchor's weekday → inconsistent.
        let r = rule("{freq: MONTHLY, byDay: [MO]}");
        // 2026-08-19 is a Wednesday.
        assert!(!rule_generates_on(&r, Date::constant(2026, 8, 19)));
        assert!(rule_generates_on(&r, Date::constant(2026, 8, 17)));
        // A positive ordinal byDay that matches the anchor.
        let r = rule("{freq: MONTHLY, byDay: [2TU]}");
        // 2026-08-11 is the second Tuesday.
        assert!(rule_generates_on(&r, Date::constant(2026, 8, 11)));
        // Sub-daily rules anchor anywhere.
        let r = StructuredRule::new(Frequency::Hourly);
        assert!(rule_generates_on(&r, Date::constant(2026, 8, 19)));
    }

    #[test]
    fn timespec_describe_value_renders_every_shape() {
        // Non-string start values produce `describe_value` output.
        for (src, want) in [
            ("5", "number 5"),
            ("true", "bool true"),
            ("null", "null"),
            ("[a]", "a sequence"),
            ("{k: v}", "a mapping"),
            ("!tag x", "tagged"),
        ] {
            let err = serde_yaml::from_str::<TimeSpec>(src).unwrap_err();
            assert!(err.to_string().contains(want), "{src}: {err}");
        }
        // Strings never reach `describe_value` through the
        // deserializers (they match the String arm first) — the arm
        // exists for completeness; call it directly.
        assert_eq!(
            describe_value(&serde_yaml::Value::String("x".into())),
            "string \"x\""
        );
    }

    /// A deserializer that fails `deserialize_any` — drives the `?`
    /// on `serde_yaml::Value::deserialize(d)` for deserializers a
    /// YAML doc can never produce.
    struct FailAny;

    impl<'de> serde::Deserializer<'de> for FailAny {
        type Error = serde::de::value::Error;

        fn deserialize_any<V: serde::de::Visitor<'de>>(
            self,
            _visitor: V,
        ) -> Result<V::Value, Self::Error> {
            Err(serde::de::Error::custom("boom"))
        }

        serde::forward_to_deserialize_any! {
            bool i8 i16 i32 i64 i128 u8 u16 u32 u64 u128 f32 f64 char str
            string bytes byte_buf option unit unit_struct newtype_struct
            seq tuple tuple_struct map struct enum identifier ignored_any
        }
    }

    #[test]
    fn spec_deserializers_propagate_deserializer_errors() {
        assert!(SpecDuration::deserialize(FailAny).is_err());
        assert!(TimeSpec::deserialize(FailAny).is_err());
    }

    #[test]
    fn spec_deserializers_cover_more_deserializer_shapes() {
        // serde's `value` deserializers instantiate `deserialize`
        // with `D` types a YAML doc never produces — every
        // instantiation must see both arms.
        use serde::de::value::{Error as DeError, StrDeserializer, U64Deserializer};
        assert!(TimeSpec::deserialize(StrDeserializer::<DeError>::new("09:00")).is_ok());
        assert!(TimeSpec::deserialize(StrDeserializer::<DeError>::new("bogus")).is_err());
        assert!(SpecDuration::deserialize(StrDeserializer::<DeError>::new("30m")).is_ok());
        assert!(SpecDuration::deserialize(U64Deserializer::<DeError>::new(30)).is_err());
        assert!(TimeSpec::deserialize(U64Deserializer::<DeError>::new(9)).is_err());
        assert!(serde_json::from_str::<SpecDuration>("\"30m\"").is_ok());
        assert!(serde_json::from_str::<SpecDuration>("{}").is_err());
        assert!(serde_json::from_str::<TimeSpec>("\"09:00\"").is_ok());
        assert!(serde_json::from_str::<TimeSpec>("42").is_err());
        assert!(serde_json::from_value::<TimeSpec>(serde_json::Value::Null).is_err());
    }

    #[test]
    fn rule_generates_on_negative_and_ordinal_filters() {
        // Negative byMonthDay counts from the month end.
        let r = rule("{freq: MONTHLY, byMonthDay: [-1]}");
        assert!(rule_generates_on(&r, Date::constant(2026, 1, 31)));
        assert!(!rule_generates_on(&r, Date::constant(2026, 1, 30)));
        // ordinal_match on a non-ordinal ByDay → the `else` guard.
        assert!(!ordinal_match(
            ByDay::every(Weekday::Monday),
            Date::constant(2026, 1, 5)
        ));
        // MONTHLY ordinal byDay + byMonthDay — both filters run.
        let r = rule("{freq: MONTHLY, byDay: [-1FR], byMonthDay: [30]}");
        assert!(rule_generates_on(&r, Date::constant(2026, 1, 30)));
        assert!(!rule_generates_on(&r, Date::constant(2026, 1, 23)));
    }
}
