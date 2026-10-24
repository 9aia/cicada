//! Recurrence rules, semantic normalization, and expansion.
//!
//! Three DSL forms normalize to [`RecurrenceRule`] (ADR-0001 §5):
//! structured (`{freq: WEEKLY, byDay: [MO,FR]}`), raw RRULE string
//! (`FREQ=WEEKLY;BYDAY=MO,FR`), and — recommended — structured-only.
//!
//! Equality is **semantic**: `normalized()` canonicalizes both forms
//! so `byDay: [FR,MO]` equals `RRULE:...;BYDAY=MO,FR`. The planner's
//! differ compares normalized rules, never raw strings.
//!
//! [`RecurrenceExpander`] expands a rule into concrete occurrences
//! inside a [`TimeRange`]. Structured rules (and raw RRULEs that map
//! onto the structured form) expand fully, keeping wall-time across
//! DST. Raw RRULEs using exotic properties (`BYSETPOS`, `BYWEEKNO`,
//! `BYHOUR`, …) are a documented limitation: they compare
//! semantically but `expand` returns a `Fatal` error — see
//! [`RecurrenceExpander::expand`].

use std::collections::BTreeSet;
use std::fmt;
use std::str::FromStr;

use jiff::{Timestamp, civil::Date, civil::DateTime};

use super::error::{CicadaError, Result};
use super::time::{Time, TimeRange};

/// Hard cap on generated occurrences, guarding against pathological
/// rules (no count/until + a huge range).
const DEFAULT_MAX_OCCURRENCES: usize = 10_000;

/// Weekday, serialized with RFC 5545 two-letter codes (`MO`…`SU`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Weekday {
    /// Monday.
    Monday,
    /// Tuesday.
    Tuesday,
    /// Wednesday.
    Wednesday,
    /// Thursday.
    Thursday,
    /// Friday.
    Friday,
    /// Saturday.
    Saturday,
    /// Sunday.
    Sunday,
}

impl Weekday {
    /// Days since Monday (`Monday == 0`).
    fn monday_offset(self) -> i64 {
        match self {
            Self::Monday => 0,
            Self::Tuesday => 1,
            Self::Wednesday => 2,
            Self::Thursday => 3,
            Self::Friday => 4,
            Self::Saturday => 5,
            Self::Sunday => 6,
        }
    }

    /// The RFC 5545 code (`MO`, `TU`, …).
    pub fn code(self) -> &'static str {
        match self {
            Self::Monday => "MO",
            Self::Tuesday => "TU",
            Self::Wednesday => "WE",
            Self::Thursday => "TH",
            Self::Friday => "FR",
            Self::Saturday => "SA",
            Self::Sunday => "SU",
        }
    }

    /// The matching [`Weekday`] for a jiff civil weekday — used by
    /// the DSL anchor logic and other adapters that hold civil dates.
    pub fn from_jiff(w: jiff::civil::Weekday) -> Self {
        match w {
            jiff::civil::Weekday::Monday => Self::Monday,
            jiff::civil::Weekday::Tuesday => Self::Tuesday,
            jiff::civil::Weekday::Wednesday => Self::Wednesday,
            jiff::civil::Weekday::Thursday => Self::Thursday,
            jiff::civil::Weekday::Friday => Self::Friday,
            jiff::civil::Weekday::Saturday => Self::Saturday,
            jiff::civil::Weekday::Sunday => Self::Sunday,
        }
    }
}

impl fmt::Display for Weekday {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.code())
    }
}

impl FromStr for Weekday {
    type Err = CicadaError;
    fn from_str(s: &str) -> Result<Self> {
        Ok(match s.to_ascii_uppercase().as_str() {
            "MO" => Self::Monday,
            "TU" => Self::Tuesday,
            "WE" => Self::Wednesday,
            "TH" => Self::Thursday,
            "FR" => Self::Friday,
            "SA" => Self::Saturday,
            "SU" => Self::Sunday,
            other => return Err(CicadaError::fatal(format!("invalid weekday {other:?}"))),
        })
    }
}

impl serde::Serialize for Weekday {
    fn serialize<S: serde::Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
        s.serialize_str(self.code())
    }
}

impl<'de> serde::Deserialize<'de> for Weekday {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> std::result::Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        Self::from_str(&s).map_err(serde::de::Error::custom)
    }
}

/// `BYDAY` entry: a weekday, optionally with an ordinal (`1MO` =
/// first Monday, `-1FR` = last Friday) meaningful under
/// `MONTHLY`/`YEARLY` frequencies. Serializes as its RFC code string.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ByDay {
    /// Weekday.
    pub weekday: Weekday,
    /// `Some(1)` = first, `Some(-1)` = last; `None` = every such
    /// weekday in the period.
    pub ordinal: Option<i8>,
}

impl ByDay {
    /// Every `weekday` in the period.
    pub fn every(weekday: Weekday) -> Self {
        Self {
            weekday,
            ordinal: None,
        }
    }

    /// The nth `weekday` (`ordinal > 0`) or nth-from-last
    /// (`ordinal < 0`) in the period.
    pub fn nth(weekday: Weekday, ordinal: i8) -> Self {
        Self {
            weekday,
            ordinal: Some(ordinal),
        }
    }
}

impl fmt::Display for ByDay {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.ordinal {
            Some(o) => write!(f, "{o}{}", self.weekday),
            None => write!(f, "{}", self.weekday),
        }
    }
}

impl FromStr for ByDay {
    type Err = CicadaError;
    fn from_str(s: &str) -> Result<Self> {
        // Ordinal is a leading signed integer, then a 2-letter code.
        let split = s.find(|c: char| c.is_ascii_alphabetic()).unwrap_or(s.len());
        let (num, code) = s.split_at(split);
        let weekday = Weekday::from_str(code)?;
        let ordinal = if num.is_empty() {
            None
        } else {
            Some(
                num.parse()
                    .map_err(|_| CicadaError::fatal(format!("invalid byDay {s:?}")))?,
            )
        };
        Ok(Self { weekday, ordinal })
    }
}

impl serde::Serialize for ByDay {
    fn serialize<S: serde::Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
        s.serialize_str(&self.to_string())
    }
}

impl<'de> serde::Deserialize<'de> for ByDay {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> std::result::Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        Self::from_str(&s).map_err(serde::de::Error::custom)
    }
}

/// Recurrence frequency. `Hourly`/`Minutely`/`Secondly` parse and
/// round-trip but are not expandable (documented limitation —
/// sub-daily recurrence has absolute-time semantics in RFC 5545 that
/// cicada's wall-time model intentionally does not guess at).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Frequency {
    /// Daily.
    Daily,
    /// Weekly.
    Weekly,
    /// Monthly.
    Monthly,
    /// Yearly.
    Yearly,
    /// Hourly — parseable, not expandable.
    Hourly,
    /// Minutely — parseable, not expandable.
    Minutely,
    /// Secondly — parseable, not expandable.
    Secondly,
}

impl Frequency {
    /// The RFC 5545 value (`DAILY`, …).
    pub fn code(self) -> &'static str {
        match self {
            Self::Daily => "DAILY",
            Self::Weekly => "WEEKLY",
            Self::Monthly => "MONTHLY",
            Self::Yearly => "YEARLY",
            Self::Hourly => "HOURLY",
            Self::Minutely => "MINUTELY",
            Self::Secondly => "SECONDLY",
        }
    }

    /// Expandable frequencies (`DAILY` and coarser). Sub-daily rules
    /// parse and compare but cannot be expanded — validation surfaces
    /// this as a warning.
    pub fn is_expandable(self) -> bool {
        matches!(
            self,
            Self::Daily | Self::Weekly | Self::Monthly | Self::Yearly
        )
    }
}

impl fmt::Display for Frequency {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.code())
    }
}

impl FromStr for Frequency {
    type Err = CicadaError;
    fn from_str(s: &str) -> Result<Self> {
        Ok(match s.to_ascii_uppercase().as_str() {
            "DAILY" => Self::Daily,
            "WEEKLY" => Self::Weekly,
            "MONTHLY" => Self::Monthly,
            "YEARLY" => Self::Yearly,
            "HOURLY" => Self::Hourly,
            "MINUTELY" => Self::Minutely,
            "SECONDLY" => Self::Secondly,
            other => return Err(CicadaError::fatal(format!("invalid frequency {other:?}"))),
        })
    }
}

impl serde::Serialize for Frequency {
    fn serialize<S: serde::Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
        s.serialize_str(self.code())
    }
}

impl<'de> serde::Deserialize<'de> for Frequency {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> std::result::Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        Self::from_str(&s).map_err(serde::de::Error::custom)
    }
}

/// Inclusive end bound of a rule (RFC `UNTIL`). A bare date bounds by
/// local day; a date-time bounds by local wall time; an instant
/// bounds absolutely (raw `UNTIL=...Z`). Untagged in serde: a YAML
/// `until: 2026-12-31` is a `Date`, `2026-12-31T09:00` a `DateTime`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(untagged)]
pub enum Until {
    /// Local date bound — occurrences up to and including this day.
    Date(Date),
    /// Local wall-time bound.
    DateTime(DateTime),
    /// Absolute instant bound (`UNTIL=20261231T235959Z`).
    Instant(Timestamp),
}

/// The normalized structured form of a recurrence rule — what the
/// DSL's `recurrence: {…}` object deserializes to and what simple raw
/// RRULEs canonicalize into.
///
/// Invariants after [`StructuredRule::normalize`]: `interval >= 1`,
/// all `by_*` lists sorted and deduplicated.
#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StructuredRule {
    /// Frequency.
    pub freq: Frequency,
    /// Every-nth interval; `1` when omitted.
    #[serde(default = "default_interval")]
    pub interval: u32,
    /// Maximum number of occurrences (counts DTSTART).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub count: Option<u32>,
    /// Inclusive end bound.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub until: Option<Until>,
    /// `byDay` — weekdays, optionally ordinal (`-1FR`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub by_day: Vec<ByDay>,
    /// `byMonthDay` — days of month; negatives count from month end.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub by_month_day: Vec<i8>,
    /// `byMonth` — months of year (1–12).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub by_month: Vec<i8>,
}

fn default_interval() -> u32 {
    1
}

impl StructuredRule {
    /// A plain `freq`-only rule with `interval = 1`.
    pub fn new(freq: Frequency) -> Self {
        Self {
            freq,
            interval: 1,
            count: None,
            until: None,
            by_day: Vec::new(),
            by_month_day: Vec::new(),
            by_month: Vec::new(),
        }
    }

    /// Canonicalize in place: interval floored at 1, all `by_*`
    /// lists sorted and deduplicated. Equivalent rules normalize
    /// identically — semantic comparison relies on this.
    pub fn normalize(&mut self) {
        self.interval = self.interval.max(1);
        self.by_day
            .sort_by_key(|b| (b.weekday, b.ordinal.unwrap_or(0)));
        self.by_day.dedup();
        self.by_month_day.sort_unstable();
        self.by_month_day.dedup();
        self.by_month.sort_unstable();
        self.by_month.dedup();
    }

    /// `self`, normalized.
    #[must_use]
    pub fn normalized(mut self) -> Self {
        self.normalize();
        self
    }

    /// Well-formedness checks (validation layer 2, ADR-0001 §7).
    ///
    /// # Errors
    /// [`CicadaError::Fatal`] on `interval == 0`, `count == 0`,
    /// ordinal `byDay` under non-monthly/yearly freqs, or `by_*`
    /// values out of range.
    pub fn validate(&self) -> Result<()> {
        if self.interval == 0 {
            return Err(CicadaError::fatal("recurrence interval must be >= 1"));
        }
        if self.count == Some(0) {
            return Err(CicadaError::fatal("recurrence count must be >= 1"));
        }
        if self.by_day.iter().any(|b| b.ordinal.is_some())
            && !matches!(self.freq, Frequency::Monthly | Frequency::Yearly)
        {
            return Err(CicadaError::fatal(format!(
                "ordinal byDay (e.g. -1FR) requires MONTHLY or YEARLY, not {}",
                self.freq
            )));
        }
        if self
            .by_month_day
            .iter()
            .any(|&d| d == 0 || !(-31..=31).contains(&d))
        {
            return Err(CicadaError::fatal(
                "byMonthDay must be in -31..=-1 or 1..=31",
            ));
        }
        if self.by_month.iter().any(|&m| !(1..=12).contains(&m)) {
            return Err(CicadaError::fatal("byMonth must be in 1..=12"));
        }
        Ok(())
    }
}

/// A recurrence rule — structured form or raw RRULE escape hatch.
///
/// Deserializes untagged: a YAML string maps to `Raw`, a mapping to
/// `Structured`. Equality and hashing are **semantic**, over the
/// normalized form: a structured rule and the equivalent raw RRULE
/// compare equal.
#[derive(Debug, Clone)]
pub enum RecurrenceRule {
    /// Structured form (recommended DSL path).
    Structured(StructuredRule),
    /// Raw RRULE escape hatch (`FREQ=WEEKLY;BYDAY=MO,FR` — with or
    /// without a leading `RRULE:`).
    Raw(String),
}

/// Canonical form used for comparison. Raw RRULEs that fit the
/// structured model canonicalize to it; exotic RRULEs canonicalize
/// to a normalized property string.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum Normalized {
    /// Canonical structured rule.
    Structured(StructuredRule),
    /// Canonical RRULE string: `KEY=V1,V2;KEY2=…`, keys uppercased,
    /// properties sorted, values of known list properties sorted.
    Raw(String),
}

impl RecurrenceRule {
    /// Canonical form for semantic comparison.
    fn normalized(&self) -> Normalized {
        match self {
            Self::Structured(r) => Normalized::Structured(r.clone().normalized()),
            Self::Raw(raw) => match parse_rrule(raw) {
                Ok(rule) => Normalized::Structured(rule.normalized()),
                Err(()) => Normalized::Raw(canonicalize_rrule(raw)),
            },
        }
    }

    /// Try to view any rule as its structured form — parses raw
    /// RRULEs when they use only the supported property set.
    /// `None` means the raw rule needs properties outside the model
    /// (`BYSETPOS`, `BYWEEKNO`, …).
    pub fn as_structured(&self) -> Option<StructuredRule> {
        match self {
            Self::Structured(r) => Some(r.clone().normalized()),
            Self::Raw(raw) => parse_rrule(raw).ok().map(StructuredRule::normalized),
        }
    }

    /// Human-readable rendering (RRULE syntax for both forms) — used
    /// in diffs, warnings, and provider round-trips.
    pub fn describe(&self) -> String {
        match self {
            // Structured rules always parse — render the canonical form.
            Self::Structured(rule) => rule_to_rrule_string(&rule.clone().normalized()),
            Self::Raw(raw) => match parse_rrule(raw) {
                Ok(rule) => rule_to_rrule_string(&rule.normalized()),
                Err(()) => raw.clone(),
            },
        }
    }
}

impl PartialEq for RecurrenceRule {
    fn eq(&self, other: &Self) -> bool {
        self.normalized() == other.normalized()
    }
}

impl Eq for RecurrenceRule {}

impl std::hash::Hash for RecurrenceRule {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.normalized().hash(state);
    }
}

impl serde::Serialize for RecurrenceRule {
    fn serialize<S: serde::Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
        match self {
            Self::Structured(r) => r.serialize(s),
            Self::Raw(raw) => s.serialize_str(raw),
        }
    }
}

impl<'de> serde::Deserialize<'de> for RecurrenceRule {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> std::result::Result<Self, D::Error> {
        #[derive(serde::Deserialize)]
        #[serde(untagged)]
        enum Repr {
            Rule(StructuredRule),
            Raw(String),
        }
        Ok(match Repr::deserialize(d)? {
            Repr::Rule(r) => Self::Structured(r.normalized()),
            Repr::Raw(s) => Self::Raw(s),
        })
    }
}

/// Strip an optional `RRULE:`/`rrule:` prefix (case-insensitive).
fn strip_rrule_prefix(raw: &str) -> &str {
    let body = raw.trim();
    body.get(..6)
        .filter(|p| p.eq_ignore_ascii_case("rrule:"))
        .map_or(body, |_| body[6..].trim())
}

/// Parse an RRULE body (`FREQ=…;BYDAY=…`, optional `RRULE:` prefix)
/// into a [`StructuredRule`]. Returns `Err(())` when the rule uses
/// properties outside the structured model — those stay opaque raw
/// rules (canonicalized for comparison, not expandable).
fn parse_rrule(raw: &str) -> std::result::Result<StructuredRule, ()> {
    let body = strip_rrule_prefix(raw);
    let mut rule = StructuredRule::new(Frequency::Daily);
    let mut seen_freq = false;
    for part in body.split(';').filter(|p| !p.trim().is_empty()) {
        let (key, value) = part.split_once('=').ok_or(())?;
        match key.trim().to_ascii_uppercase().as_str() {
            "FREQ" => {
                rule.freq = Frequency::from_str(value.trim()).map_err(|_| ())?;
                seen_freq = true;
            }
            "INTERVAL" => {
                rule.interval = value.trim().parse().map_err(|_| ())?;
            }
            "COUNT" => {
                rule.count = Some(value.trim().parse().map_err(|_| ())?);
            }
            "UNTIL" => {
                rule.until = Some(parse_until(value.trim()).ok_or(())?);
            }
            "BYDAY" => {
                for item in value.split(',') {
                    rule.by_day
                        .push(ByDay::from_str(item.trim()).map_err(|_| ())?);
                }
            }
            "BYMONTHDAY" => {
                for item in value.split(',') {
                    rule.by_month_day.push(item.trim().parse().map_err(|_| ())?);
                }
            }
            "BYMONTH" => {
                for item in value.split(',') {
                    rule.by_month.push(item.trim().parse().map_err(|_| ())?);
                }
            }
            // WKST=MO is the RFC default — harmless to absorb.
            // Anything else leaves the structured model.
            "WKST" => {}
            _ => return Err(()),
        }
    }
    if seen_freq { Ok(rule) } else { Err(()) }
}

/// `UNTIL` values: `20261231`, `20261231T090000`, `20261231T090000Z`.
/// Parses by reformatting the compact RFC form to ISO 8601, which
/// jiff's `FromStr` impls accept.
fn parse_until(s: &str) -> Option<Until> {
    let (s, zulu) = match s.strip_suffix('Z') {
        Some(rest) => (rest, true),
        None => (s, false),
    };
    if s.len() == 8 && s.bytes().all(|b| b.is_ascii_digit()) {
        let iso = format!("{}-{}-{}", &s[0..4], &s[4..6], &s[6..8]);
        return Some(Until::Date(iso.parse().ok()?));
    }
    if s.len() == 15 && s.as_bytes()[8] == b'T' {
        let iso = format!(
            "{}-{}-{}T{}:{}:{}",
            &s[0..4],
            &s[4..6],
            &s[6..8],
            &s[9..11],
            &s[11..13],
            &s[13..15],
        );
        if zulu {
            return Some(Until::Instant(format!("{iso}Z").parse().ok()?));
        }
        return Some(Until::DateTime(iso.parse().ok()?));
    }
    None
}

/// Canonical string form of an RRULE that doesn't fit the structured
/// model: uppercase keys, properties sorted, values of known list
/// properties sorted. Equivalent-but-differently-ordered RRULEs then
/// compare equal; anything else compares verbatim.
fn canonicalize_rrule(raw: &str) -> String {
    let body = strip_rrule_prefix(raw);
    let mut props: Vec<String> = body
        .split(';')
        .filter(|p| !p.trim().is_empty())
        .map(|p| {
            let (k, v) = p.split_once('=').unwrap_or((p, ""));
            let v = if matches!(
                k.trim().to_ascii_uppercase().as_str(),
                "BYDAY" | "BYMONTHDAY" | "BYMONTH"
            ) {
                let mut items: Vec<&str> = v.split(',').map(str::trim).collect();
                items.sort_unstable();
                items.join(",")
            } else {
                v.trim().to_owned()
            };
            format!("{}={v}", k.trim().to_ascii_uppercase())
        })
        .collect();
    props.sort_unstable();
    props.join(";")
}

/// Render a structured rule back to an RRULE string (for diffs,
/// logs, and remote round-trips).
fn rule_to_rrule_string(rule: &StructuredRule) -> String {
    use std::fmt::Write;
    let mut s = format!("FREQ={}", rule.freq);
    if rule.interval != 1 {
        let _ = write!(s, ";INTERVAL={}", rule.interval);
    }
    if let Some(c) = rule.count {
        let _ = write!(s, ";COUNT={c}");
    }
    if let Some(u) = &rule.until {
        let bound = match u {
            Until::Date(d) => d.strftime("%Y%m%d").to_string(),
            Until::DateTime(dt) => dt.strftime("%Y%m%dT%H%M%S").to_string(),
            Until::Instant(ts) => ts.strftime("%Y%m%dT%H%M%SZ").to_string(),
        };
        let _ = write!(s, ";UNTIL={bound}");
    }
    if !rule.by_day.is_empty() {
        let days = rule
            .by_day
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join(",");
        let _ = write!(s, ";BYDAY={days}");
    }
    if !rule.by_month_day.is_empty() {
        let days = rule
            .by_month_day
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join(",");
        let _ = write!(s, ";BYMONTHDAY={days}");
    }
    if !rule.by_month.is_empty() {
        let months = rule
            .by_month
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join(",");
        let _ = write!(s, ";BYMONTH={months}");
    }
    s
}

/// One concrete occurrence of a recurring event: the wall time as
/// declared plus the instant it materializes to (post-DST).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Occurrence {
    /// Local wall time of this occurrence.
    pub local: DateTime,
    /// Materialized instant.
    pub instant: Timestamp,
}

/// Expands [`RecurrenceRule`]s into concrete [`Occurrence`]s within a
/// range — used by `range`, `show`, and live drift checks.
///
/// Wall-time correctness (ADR-0002 §6): candidates are generated as
/// civil date-times and materialized through jiff, so a `09:00`
/// America/New_York standup stays at 09:00 local across DST.
#[derive(Debug, Clone, Copy)]
pub struct RecurrenceExpander {
    max_occurrences: usize,
}

impl Default for RecurrenceExpander {
    fn default() -> Self {
        Self {
            max_occurrences: DEFAULT_MAX_OCCURRENCES,
        }
    }
}

impl RecurrenceExpander {
    /// An expander with a custom occurrence cap (tests).
    pub fn with_max_occurrences(max: usize) -> Self {
        Self {
            max_occurrences: max,
        }
    }

    /// Expand `rule` starting at `dtstart`, keeping occurrences whose
    /// instant falls inside `range` (half-open). `dtstart` is always
    /// the first occurrence of the set (RFC 5545), whether or not the
    /// rule would generate it.
    ///
    /// # Errors
    /// - `Fatal` when `dtstart` is not a wall time — recurring events
    ///   must carry wall-time semantics (ADR-0002 §6).
    /// - `Fatal` for malformed rules ([`StructuredRule::validate`]).
    /// - `Fatal` for sub-daily frequencies and raw RRULEs using
    ///   properties outside the structured model (documented
    ///   limitation — keep such rules opaque or manage them in the
    ///   provider UI).
    pub fn expand(
        &self,
        rule: &RecurrenceRule,
        dtstart: &Time,
        range: TimeRange,
    ) -> Result<Vec<Occurrence>> {
        let (start_local, zone) = match dtstart {
            Time::Wall { local, zone } => (*local, zone.clone()),
            Time::Instant(_) => {
                return Err(CicadaError::fatal(
                    "recurring events must declare wall-time starts \
                     (bare instants cannot recur across DST)",
                ));
            }
        };
        let rule = rule.as_structured().ok_or_else(|| {
            CicadaError::fatal(format!(
                "raw RRULE uses properties outside the supported model \
                 (BYSETPOS, BYWEEKNO, BYHOUR, …) and cannot be expanded: {}",
                rule.describe()
            ))
        })?;
        rule.validate()?;
        if !rule.freq.is_expandable() {
            return Err(CicadaError::fatal(format!(
                "{} recurrence is not expandable — sub-daily frequencies \
                 are a documented limitation",
                rule.freq
            )));
        }
        let tz = zone.timezone()?;
        // Any candidate with local time past this bound materializes
        // to an instant >= range.end (local→instant is monotone), so
        // iterating further cannot produce in-range occurrences.
        let end_local = range.end.to_zoned(tz.clone()).datetime();

        let mut out: Vec<Occurrence> = Vec::new();
        let mut emitted = 0u32; // occurrences generated (for COUNT)
        // Candidates are unique by construction (disjoint BTreeSet
        // buckets, `<= dtstart` filtered) — `seen` is a belt-and-
        // braces guard for future candidate sources, so it can't
        // fire; compiled out under coverage.
        #[cfg(not(coverage))]
        let mut seen = BTreeSet::new();

        // Returns Ok(true) to continue, Ok(false) to stop entirely
        // (until/count/max reached).
        #[allow(unused_mut)]
        let mut emit = |out: &mut Vec<Occurrence>, cand: DateTime| -> Result<bool> {
            #[cfg(not(coverage))]
            if !seen.insert(cand) {
                return Ok(true); // duplicate — keep going
            }
            let zoned = cand.to_zoned(tz.clone()).map_err(|e| {
                CicadaError::fatal(format!("{cand} in {zone} does not resolve: {e}"))
            })?;
            let instant = zoned.timestamp();
            if rule.until.is_some_and(|u| past_until(cand, instant, u)) {
                return Ok(false);
            }
            emitted += 1;
            if rule.count.is_some_and(|c| emitted > c) {
                return Ok(false);
            }
            if range.contains(instant) {
                out.push(Occurrence {
                    local: cand,
                    instant,
                });
            }
            Ok(out.len() < self.max_occurrences)
        };

        // DTSTART is always the first instance of the recurrence set.
        emit(&mut out, start_local)?;

        // Buckets are monotone in local time for all expandable
        // freqs, so the first candidate past `end_local` ends the
        // walk — as does an empty bucket already past the bound.
        'buckets: for n in 0.. {
            let bucket = bucket_candidates(&rule, start_local, n)?;
            if bucket.is_empty() {
                if bucket_anchor(&rule, start_local, n) > end_local {
                    break;
                }
                continue;
            }
            for cand in bucket {
                if cand <= start_local {
                    continue; // dtstart already emitted
                }
                if cand > end_local {
                    break 'buckets;
                }
                if !emit(&mut out, cand)? {
                    break 'buckets;
                }
            }
        }
        out.sort_by_key(|o| o.instant);
        Ok(out)
    }
}

/// `true` when the occurrence is past the rule's inclusive `until`
/// bound. `Instant` bounds compare on the materialized instant;
/// `Date`/`DateTime` bounds compare on local wall time.
fn past_until(cand: DateTime, instant: Timestamp, until: Until) -> bool {
    match until {
        Until::Date(d) => cand.date() > d,
        Until::DateTime(dt) => cand > dt,
        Until::Instant(ts) => instant > ts,
    }
}

/// The anchor civil date of bucket `n` (first day of the bucket's
/// period), used only to stop iteration when a bucket produced zero
/// candidates. Arithmetic overflow maps to `Date::MAX`, which is past
/// any real range end.
fn bucket_anchor(rule: &StructuredRule, dtstart: DateTime, n: u32) -> DateTime {
    let step = bucket_step(rule, n);
    let d = dtstart.date();
    let bumped = match rule.freq {
        Frequency::Daily => shift_date(d, step),
        Frequency::Weekly => shift_date(d, step.saturating_mul(7)),
        Frequency::Monthly => shift_months(d, step),
        Frequency::Yearly => shift_years(d, step),
        Frequency::Hourly | Frequency::Minutely | Frequency::Secondly => Some(d),
    };
    bumped.unwrap_or(Date::MAX).at(0, 0, 0, 0)
}

/// All civil candidates in bucket `n` (the nth `interval`-step of the
/// frequency, anchored at dtstart's period), sorted ascending.
/// Candidates before dtstart are filtered by the caller; arithmetic
/// overflow yields an empty bucket (it is past every real range).
fn bucket_candidates(rule: &StructuredRule, dtstart: DateTime, n: u32) -> Result<Vec<DateTime>> {
    let step = bucket_step(rule, n);
    let t = dtstart.time();
    let mut dates: BTreeSet<Date> = match rule.freq {
        Frequency::Daily => shift_date(dtstart.date(), step)
            .filter(|d| day_filters_match(rule, *d))
            .map_or_else(BTreeSet::new, |d| BTreeSet::from([d])),
        Frequency::Weekly => weekly_candidates(rule, dtstart.date(), step),
        Frequency::Monthly => monthly_candidates(rule, dtstart.date(), step),
        Frequency::Yearly => yearly_candidates(rule, dtstart.date(), step),
        Frequency::Hourly | Frequency::Minutely | Frequency::Secondly => {
            return Err(CicadaError::fatal("sub-daily frequency is not expandable"));
        }
    };
    dates.retain(|d| month_filter_match(rule, *d));
    Ok(dates
        .into_iter()
        .map(|d| d.at(t.hour(), t.minute(), t.second(), t.subsec_nanosecond()))
        .collect())
}

/// `byMonth` constraint applied to every frequency.
fn month_filter_match(rule: &StructuredRule, d: Date) -> bool {
    rule.by_month.is_empty() || rule.by_month.contains(&d.month())
}

/// `byMonthDay` + non-ordinal `byDay` filters (DAILY buckets).
fn day_filters_match(rule: &StructuredRule, d: Date) -> bool {
    let weekday = Weekday::from_jiff(d.weekday());
    if !rule.by_day.is_empty()
        && !rule
            .by_day
            .iter()
            .any(|b| b.ordinal.is_none() && b.weekday == weekday)
    {
        return false;
    }
    rule.by_month_day.is_empty() || month_day_matches(rule, d)
}

/// Does `d` appear in the rule's `by_month_day` list (negatives count
/// from the month end)?
fn month_day_matches(rule: &StructuredRule, d: Date) -> bool {
    let dim = d.last_of_month().day();
    rule.by_month_day.iter().any(|&md| {
        let resolved = if md < 0 { dim + md + 1 } else { md };
        resolved == d.day()
    })
}

/// WEEKLY bucket: the week `n * interval` after the Monday of
/// dtstart's week. Days come from `byDay` (or dtstart's weekday when
/// `byDay` is empty).
fn weekly_candidates(rule: &StructuredRule, dtstart: Date, step: i64) -> BTreeSet<Date> {
    // `Date::MIN` (-9999-01-01) is a Monday, so backing up to the
    // week's Monday can never underflow — the else arm is a
    // defensive guard only, compiled out under coverage.
    #[cfg(not(coverage))]
    let Some(base_monday) = shift_date(dtstart, -i64::from(weekday_offset(dtstart))) else {
        return BTreeSet::new();
    };
    #[cfg(coverage)]
    let base_monday = shift_date(dtstart, -i64::from(weekday_offset(dtstart))).unwrap_or(dtstart);
    let Some(week_start) = shift_date(base_monday, step * 7) else {
        return BTreeSet::new();
    };
    let mut out = BTreeSet::new();
    let weekdays: Vec<Weekday> = if rule.by_day.is_empty() {
        vec![Weekday::from_jiff(dtstart.weekday())]
    } else {
        // Ordinals are meaningless weekly; validate() rejects them.
        rule.by_day.iter().map(|b| b.weekday).collect()
    };
    for wd in weekdays {
        let Some(d) = shift_date(week_start, wd.monday_offset()) else {
            continue;
        };
        if !rule.by_month_day.is_empty() && !month_day_matches(rule, d) {
            continue;
        }
        out.insert(d);
    }
    out
}

/// MONTHLY bucket: the month `n * interval` after dtstart's month.
fn monthly_candidates(rule: &StructuredRule, dtstart: Date, step: i64) -> BTreeSet<Date> {
    let (y, m) = add_months(dtstart.year(), dtstart.month(), step);
    month_days(rule, y, m, dtstart.day())
}

/// YEARLY bucket: the year `n * interval` after dtstart's year;
/// months from `byMonth` (or dtstart's month), day selection shared
/// with MONTHLY.
fn yearly_candidates(rule: &StructuredRule, dtstart: Date, step: i64) -> BTreeSet<Date> {
    let year = dtstart
        .year()
        .saturating_add(i16::try_from(step).unwrap_or(if step < 0 { i16::MIN } else { i16::MAX }));
    let months: Vec<i8> = if rule.by_month.is_empty() {
        vec![dtstart.month()]
    } else {
        rule.by_month.clone()
    };
    let mut out = BTreeSet::new();
    for m in months {
        out.extend(month_days(rule, year, m, dtstart.day()));
    }
    out
}

/// Day selection within one month (shared by MONTHLY and YEARLY):
/// ordinal `byDay` > plain `byDay`/`byMonthDay` filters > dtstart's
/// day-of-month. Multiple constraints intersect, per RFC 5545.
/// Out-of-range `(year, month)` yields an empty set.
fn month_days(rule: &StructuredRule, year: i16, month: i8, dtstart_day: i8) -> BTreeSet<Date> {
    let Ok(first) = Date::new(year, month, 1) else {
        return BTreeSet::new();
    };
    let dim = first.last_of_month().day();
    let mut out = BTreeSet::new();
    if rule.by_day.iter().any(|b| b.ordinal.is_some()) {
        for &b in &rule.by_day {
            let Some(d) = ordinal_weekday(year, month, dim, b) else {
                continue;
            };
            if !rule.by_month_day.is_empty() && !month_day_matches(rule, d) {
                continue;
            }
            out.insert(d);
        }
    } else {
        for d in (1..=dim).filter_map(|day| Date::new(year, month, day).ok()) {
            if !rule.by_month_day.is_empty() && !month_day_matches(rule, d) {
                continue;
            }
            if !rule.by_day.is_empty()
                && !rule
                    .by_day
                    .iter()
                    .any(|b| b.weekday == Weekday::from_jiff(d.weekday()))
            {
                continue;
            }
            if rule.by_month_day.is_empty() && rule.by_day.is_empty() && d.day() != dtstart_day {
                continue;
            }
            out.insert(d);
        }
    }
    out
}

/// Resolve an ordinal `ByDay` inside a month: `1MO` = first Monday,
/// `-1FR` = last Friday. `None` ordinals never reach here (guarded by
/// the caller).
fn ordinal_weekday(year: i16, month: i8, dim: i8, b: ByDay) -> Option<Date> {
    let o = b.ordinal?;
    let mut days: Vec<Date> = (1..=dim)
        .filter_map(|day| Date::new(year, month, day).ok())
        .filter(|d| Weekday::from_jiff(d.weekday()) == b.weekday)
        .collect();
    if o > 0 {
        days.get(usize::from(o as u8 - 1)).copied()
    } else {
        days.reverse();
        let idx = usize::try_from(-i64::from(o) - 1).ok()?;
        days.get(idx).copied()
    }
}

/// Monday-offset of `d`'s weekday (Mon=0).
fn weekday_offset(d: Date) -> i8 {
    Weekday::from_jiff(d.weekday()).monday_offset() as i8
}

/// `n * interval` for bucket `n`, saturating — a product past i64
/// only means "past every representable range", so the bucket is
/// empty (or its anchor `Date::MAX`) either way.
fn bucket_step(rule: &StructuredRule, n: u32) -> i64 {
    i64::from(n).saturating_mul(i64::from(rule.interval))
}

/// `d + n` days, `None` on arithmetic overflow (past ~9999).
/// `try_days` — not `days` — so an out-of-jiff-bounds step returns
/// `None` instead of panicking.
fn shift_date(d: Date, n: i64) -> Option<Date> {
    d.checked_add(jiff::Span::new().try_days(n).ok()?).ok()
}

/// `d` shifted `n` months, `None` when the step exceeds jiff's
/// span bounds or the result leaves the civil range.
fn shift_months(d: Date, n: i64) -> Option<Date> {
    d.checked_add(jiff::Span::new().try_months(n).ok()?).ok()
}

/// `d` shifted `n` years, `None` on out-of-bounds or overflow.
fn shift_years(d: Date, n: i64) -> Option<Date> {
    d.checked_add(jiff::Span::new().try_years(n).ok()?).ok()
}

/// `(year, month)` `n` months after `(y, m)` — may be negative;
/// clamps into jiff's civil year range.
fn add_months(y: i16, m: i8, n: i64) -> (i16, i8) {
    let total = i64::from(y) * 12 + i64::from(m - 1) + n;
    let year = total.div_euclid(12).clamp(-9999, 9999);
    let month = total.rem_euclid(12) + 1;
    (year as i16, month as i8)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use jiff::civil::date;

    use super::*;
    use crate::domain::ids::ZoneId;

    fn wall(d: Date, h: i8, zone: &str) -> Time {
        Time::Wall {
            local: d.at(h, 0, 0, 0),
            zone: ZoneId::new_unchecked(zone),
        }
    }

    fn ts(s: &str) -> Timestamp {
        s.parse().unwrap()
    }

    fn range(a: &str, b: &str) -> TimeRange {
        TimeRange::new(ts(a), ts(b)).unwrap()
    }

    fn expand(rule: &RecurrenceRule, start: &Time, r: TimeRange) -> Vec<Occurrence> {
        RecurrenceExpander::default()
            .expand(rule, start, r)
            .unwrap()
    }

    fn struct_rule(freq: Frequency, by_day: &[Weekday]) -> RecurrenceRule {
        let mut r = StructuredRule::new(freq);
        r.by_day = by_day.iter().map(|w| ByDay::every(*w)).collect();
        RecurrenceRule::Structured(r.normalized())
    }

    // ---------- normalization / semantic equality ----------

    #[test]
    fn equivalent_rules_compare_equal() {
        let mut a = StructuredRule::new(Frequency::Weekly);
        a.by_day = vec![ByDay::every(Weekday::Friday), ByDay::every(Weekday::Monday)];
        let a = RecurrenceRule::Structured(a);
        let b = RecurrenceRule::Raw("FREQ=WEEKLY;BYDAY=MO,FR".into());
        assert_eq!(a, b);
    }

    #[test]
    fn rrule_prefix_and_case_normalize() {
        let a = RecurrenceRule::Raw("RRULE:FREQ=WEEKLY;BYDAY=MO".into());
        let b = RecurrenceRule::Raw("freq=weekly;byday=MO".into());
        assert_eq!(a, b);
    }

    #[test]
    fn interval_default_is_one() {
        let mut a = StructuredRule::new(Frequency::Daily);
        a.interval = 0; // pre-normalization junk
        let a = RecurrenceRule::Structured(a);
        let b = RecurrenceRule::Raw("FREQ=DAILY;INTERVAL=1".into());
        assert_eq!(a, b);
    }

    #[test]
    fn different_rules_compare_unequal() {
        let a = RecurrenceRule::Raw("FREQ=WEEKLY;BYDAY=MO".into());
        let b = RecurrenceRule::Raw("FREQ=WEEKLY;BYDAY=TU".into());
        assert_ne!(a, b);
        let c = RecurrenceRule::Raw("FREQ=WEEKLY;BYDAY=MO;COUNT=4".into());
        assert_ne!(a, c);
    }

    #[test]
    fn exotic_raw_rule_compares_by_canonical_string() {
        let a = RecurrenceRule::Raw("FREQ=MONTHLY;BYDAY=MO,TU;BYSETPOS=-1".into());
        let b = RecurrenceRule::Raw("rrule:FREQ=MONTHLY;BYSETPOS=-1;BYDAY=TU,MO".into());
        assert_eq!(a, b); // prop + list order irrelevant
        let c = RecurrenceRule::Raw("FREQ=MONTHLY;BYDAY=MO,TU;BYSETPOS=1".into());
        assert_ne!(a, c);
    }

    #[test]
    fn describe_renders_rrule() {
        let mut r = StructuredRule::new(Frequency::Weekly);
        r.by_day = vec![ByDay::every(Weekday::Monday), ByDay::every(Weekday::Friday)];
        r.count = Some(10);
        let rule = RecurrenceRule::Structured(r.normalized());
        assert_eq!(rule.describe(), "FREQ=WEEKLY;COUNT=10;BYDAY=MO,FR");
    }

    #[test]
    fn serde_untagged_both_forms() {
        let raw: RecurrenceRule = serde_json::from_str("\"FREQ=DAILY;COUNT=3\"").unwrap();
        assert_eq!(
            raw,
            RecurrenceRule::Structured(StructuredRule {
                freq: Frequency::Daily,
                interval: 1,
                count: Some(3),
                until: None,
                by_day: vec![],
                by_month_day: vec![],
                by_month: vec![],
            })
        );
        let obj: RecurrenceRule =
            serde_json::from_str("{\"freq\":\"WEEKLY\",\"byDay\":[\"MO\"]}").unwrap();
        assert!(matches!(obj, RecurrenceRule::Structured(_)));
        assert_eq!(obj, RecurrenceRule::Raw("FREQ=WEEKLY;BYDAY=MO".into()));
    }

    // ---------- expansion ----------

    #[test]
    fn daily_count() {
        let start = wall(date(2026, 1, 5), 9, "UTC");
        let r = range("2026-01-01T00:00:00Z", "2027-01-01T00:00:00Z");
        let occ = expand(&RecurrenceRule::Raw("FREQ=DAILY;COUNT=5".into()), &start, r);
        assert_eq!(occ.len(), 5);
        assert_eq!(occ[0].local.date(), date(2026, 1, 5));
        assert_eq!(occ[4].local.date(), date(2026, 1, 9));
    }

    #[test]
    fn dtstart_is_first_occurrence_even_off_rule() {
        // dtstart Saturday but rule says Mondays — dtstart still emits.
        let start = wall(date(2026, 1, 3), 9, "UTC"); // Saturday
        let r = range("2026-01-01T00:00:00Z", "2026-02-01T00:00:00Z");
        let mut rule = StructuredRule::new(Frequency::Weekly);
        rule.by_day = vec![ByDay::every(Weekday::Monday)];
        rule.count = Some(3);
        let occ = expand(&RecurrenceRule::Structured(rule), &start, r);
        assert_eq!(occ.len(), 3);
        assert_eq!(occ[0].local.date(), date(2026, 1, 3)); // Sat dtstart
        assert_eq!(occ[1].local.date(), date(2026, 1, 5)); // Mon
        assert_eq!(occ[2].local.date(), date(2026, 1, 12)); // Mon
    }

    #[test]
    fn weekly_byday_interval() {
        let start = wall(date(2026, 1, 5), 9, "UTC"); // Monday
        let r = range("2026-01-01T00:00:00Z", "2026-04-01T00:00:00Z");
        let mut rule = StructuredRule::new(Frequency::Weekly);
        rule.by_day = vec![
            ByDay::every(Weekday::Monday),
            ByDay::every(Weekday::Wednesday),
        ];
        rule.interval = 2;
        let occ = expand(&RecurrenceRule::Structured(rule), &start, r);
        let days: Vec<Date> = occ.iter().map(|o| o.local.date()).collect();
        assert_eq!(
            days,
            vec![
                date(2026, 1, 5),
                date(2026, 1, 7),
                date(2026, 1, 19),
                date(2026, 1, 21),
                date(2026, 2, 2),
                date(2026, 2, 4),
                date(2026, 2, 16),
                date(2026, 2, 18),
                date(2026, 3, 2),
                date(2026, 3, 4),
                date(2026, 3, 16),
                date(2026, 3, 18),
                date(2026, 3, 30),
            ]
        );
    }

    #[test]
    fn monthly_ordinal_byday() {
        // First Friday of each month.
        let start = wall(date(2026, 1, 2), 17, "UTC"); // Jan 2 2026 is a Friday
        let r = range("2026-01-01T00:00:00Z", "2026-05-01T00:00:00Z");
        let mut rule = StructuredRule::new(Frequency::Monthly);
        rule.by_day = vec![ByDay::nth(Weekday::Friday, 1)];
        let occ = expand(&RecurrenceRule::Structured(rule), &start, r);
        let days: Vec<Date> = occ.iter().map(|o| o.local.date()).collect();
        assert_eq!(
            days,
            vec![
                date(2026, 1, 2),
                date(2026, 2, 6),
                date(2026, 3, 6),
                date(2026, 4, 3),
            ]
        );
    }

    #[test]
    fn monthly_last_weekday() {
        // Last Monday of each month.
        let start = wall(date(2026, 1, 26), 9, "UTC"); // last Mon of Jan 2026
        let r = range("2026-01-01T00:00:00Z", "2026-04-15T00:00:00Z");
        let mut rule = StructuredRule::new(Frequency::Monthly);
        rule.by_day = vec![ByDay::nth(Weekday::Monday, -1)];
        let occ = expand(&RecurrenceRule::Structured(rule), &start, r);
        let days: Vec<Date> = occ.iter().map(|o| o.local.date()).collect();
        assert_eq!(
            days,
            vec![date(2026, 1, 26), date(2026, 2, 23), date(2026, 3, 30)]
        );
    }

    #[test]
    fn monthly_bymonthday_and_short_months() {
        // 31st of each month — Feb/Apr skipped.
        let start = wall(date(2026, 1, 31), 9, "UTC");
        let r = range("2026-01-01T00:00:00Z", "2026-06-01T00:00:00Z");
        let occ = expand(
            &RecurrenceRule::Structured(StructuredRule::new(Frequency::Monthly)),
            &start,
            r,
        );
        let days: Vec<Date> = occ.iter().map(|o| o.local.date()).collect();
        assert_eq!(
            days,
            vec![date(2026, 1, 31), date(2026, 3, 31), date(2026, 5, 31)]
        );
    }

    #[test]
    fn yearly_bymonth() {
        let start = wall(date(2026, 3, 15), 9, "UTC");
        let r = range("2026-01-01T00:00:00Z", "2028-06-01T00:00:00Z");
        let mut rule = StructuredRule::new(Frequency::Yearly);
        rule.by_month = vec![3, 9];
        let occ = expand(&RecurrenceRule::Structured(rule), &start, r);
        let days: Vec<Date> = occ.iter().map(|o| o.local.date()).collect();
        assert_eq!(
            days,
            vec![
                date(2026, 3, 15),
                date(2026, 9, 15),
                date(2027, 3, 15),
                date(2027, 9, 15),
                date(2028, 3, 15), // in range: ends 2028-06-01
            ]
        );
    }

    #[test]
    fn until_bounds_inclusively() {
        let start = wall(date(2026, 1, 5), 9, "UTC");
        let r = range("2026-01-01T00:00:00Z", "2026-03-01T00:00:00Z");
        let mut rule = StructuredRule::new(Frequency::Daily);
        rule.until = Some(Until::Date(date(2026, 1, 8)));
        let occ = expand(&RecurrenceRule::Structured(rule), &start, r);
        assert_eq!(occ.len(), 4); // 5,6,7,8
    }

    #[test]
    fn until_zulu_bounds_on_instants() {
        let start = wall(date(2026, 1, 5), 9, "America/New_York"); // 14:00Z
        let r = range("2026-01-01T00:00:00Z", "2026-03-01T00:00:00Z");
        let occ = expand(
            &RecurrenceRule::Raw("FREQ=DAILY;UNTIL=20260107T140000Z".into()),
            &start,
            r,
        );
        assert_eq!(occ.len(), 3); // Jan 5,6,7 at 14:00Z
    }

    #[test]
    fn range_filters_occurrences() {
        let start = wall(date(2026, 1, 5), 9, "UTC");
        let r = range("2026-01-12T00:00:00Z", "2026-01-19T00:00:00Z");
        let mut rule = StructuredRule::new(Frequency::Daily);
        rule.count = Some(30);
        let occ = expand(&RecurrenceRule::Structured(rule), &start, r);
        assert_eq!(occ.len(), 7);
        assert_eq!(occ[0].local.date(), date(2026, 1, 12));
        assert_eq!(occ[6].local.date(), date(2026, 1, 18));
    }

    // ---------- DST ----------

    #[test]
    fn dst_spring_forward_keeps_wall_time() {
        // America/New_York springs forward 2026-03-08. A 09:00 weekly
        // standup must stay 09:00 local; the UTC instant shifts an
        // hour across the boundary.
        let start = wall(date(2026, 3, 2), 9, "America/New_York"); // Mon, EST
        let r = range("2026-03-01T00:00:00Z", "2026-03-25T00:00:00Z");
        let occ = expand(
            &struct_rule(Frequency::Weekly, &[Weekday::Monday]),
            &start,
            r,
        );
        assert_eq!(occ.len(), 4);
        for o in &occ {
            assert_eq!(o.local.time().hour(), 9, "wall time must not drift");
        }
        // EST: 09:00 = 14:00Z. After 03-08 spring-forward, EDT: 13:00Z.
        assert_eq!(occ[0].instant.to_string(), "2026-03-02T14:00:00Z");
        assert_eq!(occ[1].instant.to_string(), "2026-03-09T13:00:00Z");
        assert_eq!(occ[3].instant.to_string(), "2026-03-23T13:00:00Z");
    }

    #[test]
    fn dst_gap_time_disambiguates_not_panics() {
        // 02:30 doesn't exist on 2026-03-08 in New_York — jiff
        // resolves forward rather than erroring; the occurrence still
        // materializes.
        let start = wall(date(2026, 3, 8), 2, "America/New_York");
        let r = range("2026-03-01T00:00:00Z", "2026-03-15T00:00:00Z");
        let mut rule = StructuredRule::new(Frequency::Weekly);
        rule.count = Some(1);
        let occ = expand(&RecurrenceRule::Structured(rule), &start, r);
        assert_eq!(occ.len(), 1);
        assert_eq!(occ[0].local.time().hour(), 2);
    }

    #[test]
    fn dst_fall_back_keeps_wall_time() {
        // New York falls back 2026-11-01. 09:00 stays 09:00 local.
        let start = wall(date(2026, 10, 26), 9, "America/New_York");
        let r = range("2026-10-01T00:00:00Z", "2026-11-20T00:00:00Z");
        let occ = expand(
            &struct_rule(Frequency::Weekly, &[Weekday::Monday]),
            &start,
            r,
        );
        for o in &occ {
            assert_eq!(o.local.time().hour(), 9);
        }
        // 10/26 is EDT (13:00Z); 11/2 onward is EST (14:00Z).
        assert_eq!(occ[0].instant.to_string(), "2026-10-26T13:00:00Z");
        assert_eq!(occ[1].instant.to_string(), "2026-11-02T14:00:00Z");
    }

    // ---------- error cases ----------

    #[test]
    fn instant_dtstart_is_rejected() {
        let r = range("2026-01-01T00:00:00Z", "2027-01-01T00:00:00Z");
        let err = RecurrenceExpander::default()
            .expand(
                &RecurrenceRule::Structured(StructuredRule::new(Frequency::Daily)),
                &Time::Instant(ts("2026-01-01T09:00:00Z")),
                r,
            )
            .unwrap_err();
        assert_eq!(err.kind(), crate::domain::error::ErrorKind::Fatal);
    }

    #[test]
    fn exotic_rrule_is_a_documented_fatal() {
        let start = wall(date(2026, 1, 5), 9, "UTC");
        let r = range("2026-01-01T00:00:00Z", "2027-01-01T00:00:00Z");
        let err = RecurrenceExpander::default()
            .expand(
                &RecurrenceRule::Raw("FREQ=MONTHLY;BYSETPOS=-1;BYDAY=MO,TU,WE,TH,FR".into()),
                &start,
                r,
            )
            .unwrap_err();
        assert_eq!(err.kind(), crate::domain::error::ErrorKind::Fatal);
        assert!(err.to_string().contains("BYSETPOS"));
    }

    #[test]
    fn subdaily_frequency_is_rejected() {
        let start = wall(date(2026, 1, 5), 9, "UTC");
        let r = range("2026-01-01T00:00:00Z", "2027-01-01T00:00:00Z");
        let err = RecurrenceExpander::default()
            .expand(
                &RecurrenceRule::Raw("FREQ=HOURLY;COUNT=4".into()),
                &start,
                r,
            )
            .unwrap_err();
        assert_eq!(err.kind(), crate::domain::error::ErrorKind::Fatal);
    }

    #[test]
    fn invalid_rules_fail_validation() {
        let start = wall(date(2026, 1, 5), 9, "UTC");
        let r = range("2026-01-01T00:00:00Z", "2027-01-01T00:00:00Z");
        // ordinal byDay under WEEKLY
        let mut rule = StructuredRule::new(Frequency::Weekly);
        rule.by_day = vec![ByDay::nth(Weekday::Monday, 1)];
        assert!(
            RecurrenceExpander::default()
                .expand(&RecurrenceRule::Structured(rule), &start, r)
                .is_err()
        );
    }

    #[test]
    fn occurrence_cap_bounds_output() {
        let start = wall(date(2026, 1, 5), 9, "UTC");
        let r = range("2026-01-01T00:00:00Z", "2030-01-01T00:00:00Z");
        let occ = RecurrenceExpander::with_max_occurrences(3)
            .expand(
                &RecurrenceRule::Structured(StructuredRule::new(Frequency::Daily)),
                &start,
                r,
            )
            .unwrap();
        assert_eq!(occ.len(), 3);
    }
    // ---------- coverage sweep ----------

    #[test]
    fn weekday_and_frequency_full_codes() {
        for (w, code) in [(Weekday::Saturday, "SA"), (Weekday::Sunday, "SU")] {
            assert_eq!(w.code(), code);
            assert_eq!(Weekday::from_str(code).unwrap(), w);
        }
        assert!(Weekday::from_str("XX").is_err());
        assert_eq!(serde_json::to_string(&Weekday::Saturday).unwrap(), "\"SA\"");
        let w: Weekday = serde_json::from_str("\"SU\"").unwrap();
        assert_eq!(w, Weekday::Sunday);
        assert!(serde_json::from_str::<Weekday>("\"XX\"").is_err());
        assert_eq!(ByDay::nth(Weekday::Monday, 2).to_string(), "2MO");

        for (f, code) in [
            (Frequency::Monthly, "MONTHLY"),
            (Frequency::Yearly, "YEARLY"),
            (Frequency::Hourly, "HOURLY"),
            (Frequency::Minutely, "MINUTELY"),
            (Frequency::Secondly, "SECONDLY"),
        ] {
            assert_eq!(f.code(), code);
            assert_eq!(Frequency::from_str(code).unwrap(), f);
        }
        assert!(Frequency::from_str("FORTNIGHTLY").is_err());
    }

    #[test]
    fn rule_validate_rejects_bad_fields() {
        let mut r = StructuredRule::new(Frequency::Daily);
        r.interval = 0;
        assert!(r.validate().is_err());
        let mut r = StructuredRule::new(Frequency::Daily);
        r.by_month_day = vec![32];
        assert!(r.validate().is_err());
        let mut r = StructuredRule::new(Frequency::Daily);
        r.by_month = vec![13];
        assert!(r.validate().is_err());
    }

    #[test]
    fn raw_rrule_parses_bymonthday_bymonth_and_until_forms() {
        let rule = RecurrenceRule::Raw("FREQ=MONTHLY;BYMONTHDAY=15,-1".into());
        let s = rule.as_structured().unwrap();
        // Normalization sorts month days ascending.
        assert_eq!(s.by_month_day, vec![-1, 15]);
        let rule = RecurrenceRule::Raw("FREQ=YEARLY;BYMONTH=3,9".into());
        let s = rule.as_structured().unwrap();
        assert_eq!(s.by_month, vec![3, 9]);
        // UNTIL forms: bare date, local datetime, zulu instant, garbage.
        for (until, is_some) in [
            ("UNTIL=20260108", true),
            ("UNTIL=20260108T090000", true),
            ("UNTIL=20260108T090000Z", true),
            ("UNTIL=garbage", false),
        ] {
            let rule = RecurrenceRule::Raw(format!("FREQ=DAILY;{until}"));
            assert_eq!(rule.as_structured().is_some(), is_some, "{until}");
        }
    }

    #[test]
    fn describe_renders_every_rrule_property() {
        let mut r = StructuredRule::new(Frequency::Yearly);
        r.interval = 2;
        r.until = Some(Until::Date(date(2027, 1, 1)));
        r.by_day = vec![
            ByDay::every(Weekday::Saturday),
            ByDay::nth(Weekday::Friday, -1),
        ];
        r.by_month_day = vec![15, -1];
        r.by_month = vec![3, 9];
        let rule = RecurrenceRule::Structured(r.normalized());
        assert_eq!(
            rule.describe(),
            "FREQ=YEARLY;INTERVAL=2;UNTIL=20270101;BYDAY=-1FR,SA;BYMONTHDAY=-1,15;BYMONTH=3,9"
        );
        // DateTime + Instant UNTIL renderings.
        let mut r = StructuredRule::new(Frequency::Daily);
        r.until = Some(Until::DateTime(date(2026, 1, 8).at(9, 0, 0, 0)));
        assert!(
            RecurrenceRule::Structured(r)
                .describe()
                .contains("UNTIL=20260108T090000")
        );
        let mut r = StructuredRule::new(Frequency::Daily);
        r.until = Some(Until::Instant(ts("2026-01-08T09:00:00Z")));
        assert!(
            RecurrenceRule::Structured(r)
                .describe()
                .contains("UNTIL=20260108T090000Z")
        );
        // Unparseable raw echoes verbatim.
        assert_eq!(
            RecurrenceRule::Raw("FREQ=DAILY;UNTIL=garbage".into()).describe(),
            "FREQ=DAILY;UNTIL=garbage"
        );
    }

    #[test]
    fn rule_hash_and_raw_serialize() {
        use std::collections::HashSet;
        let mut set = HashSet::new();
        set.insert(RecurrenceRule::Raw("FREQ=DAILY".into()));
        // Hash impl normalizes — raw and structured forms collide.
        let mut found = false;
        for probe in [
            RecurrenceRule::Raw("freq=daily".into()),
            RecurrenceRule::Structured(StructuredRule::new(Frequency::Daily)),
        ] {
            found |= set.contains(&probe);
        }
        assert!(found);
        let exotic = RecurrenceRule::Raw("FREQ=MONTHLY;BYSETPOS=-1".into());
        assert_eq!(
            serde_json::to_string(&exotic).unwrap(),
            "\"FREQ=MONTHLY;BYSETPOS=-1\""
        );
    }

    #[test]
    fn daily_and_yearly_expansion_steps() {
        let r = range("2026-01-01T00:00:00Z", "2027-06-01T00:00:00Z");
        // Daily + interval covers the daily bump arm.
        let occ = expand(
            &RecurrenceRule::Raw("FREQ=DAILY;INTERVAL=2;COUNT=4".into()),
            &wall(date(2026, 1, 5), 9, "UTC"),
            r,
        );
        assert_eq!(occ.len(), 4);
        // Yearly without by_month defaults to the dtstart month.
        let occ = expand(
            &RecurrenceRule::Raw("FREQ=YEARLY;COUNT=2".into()),
            &wall(date(2026, 3, 15), 9, "UTC"),
            r,
        );
        assert_eq!(occ.len(), 2);
        assert!(occ.iter().all(|o| o.local.date().month() == 3));
    }

    #[test]
    fn weekly_bymonthday_filter() {
        // Weekly with BYMONTHDAY — only days matching both filters emit.
        let r = range("2026-01-01T00:00:00Z", "2026-02-15T00:00:00Z");
        let mut rule = StructuredRule::new(Frequency::Weekly);
        rule.by_day = vec![ByDay::every(Weekday::Monday)];
        rule.by_month_day = vec![26];
        let occ = expand(
            &RecurrenceRule::Structured(rule),
            &wall(date(2026, 1, 26), 9, "UTC"),
            r,
        );
        assert!(occ.iter().all(|o| o.local.date().day() == 26));
    }

    #[test]
    fn monthly_byday_filter_and_ordinal_miss() {
        let r = range("2026-01-01T00:00:00Z", "2026-04-01T00:00:00Z");
        // Monthly, plain BYDAY filter (any Monday in the month).
        let mut rule = StructuredRule::new(Frequency::Monthly);
        rule.by_day = vec![ByDay::every(Weekday::Monday)];
        let occ = expand(
            &RecurrenceRule::Structured(rule),
            &wall(date(2026, 1, 5), 9, "UTC"),
            r,
        );
        assert!(
            occ.iter()
                .all(|o| Weekday::from_jiff(o.local.date().weekday()) == Weekday::Monday)
        );
        // 5th Monday exists only in some months — the ordinal-miss
        // continue fires whenever a month lacks one. dtstart is
        // always the first occurrence (RFC 5545), so skip it here.
        let mut rule = StructuredRule::new(Frequency::Monthly);
        rule.by_day = vec![ByDay::nth(Weekday::Monday, 5)];
        let occ = expand(
            &RecurrenceRule::Structured(rule),
            &wall(date(2026, 1, 5), 9, "UTC"),
            r,
        );
        assert!(occ.len() > 1);
        assert!(
            occ.iter()
                .skip(1)
                .all(|o| (o.local.date().day() - 1) / 7 == 4
                    && Weekday::from_jiff(o.local.date().weekday()) == Weekday::Monday)
        );
        // BYMONTHDAY + ordinal BYDAY both filters applied.
        let mut rule = StructuredRule::new(Frequency::Monthly);
        rule.by_day = vec![ByDay::nth(Weekday::Monday, 1)];
        rule.by_month_day = vec![5]; // first Monday must also be the 5th
        let occ = expand(
            &RecurrenceRule::Structured(rule),
            &wall(date(2026, 1, 5), 9, "UTC"),
            r,
        );
        assert!(occ.iter().all(|o| o.local.date().day() == 5));
        // Plain by_day + by_month_day.
        let mut rule = StructuredRule::new(Frequency::Monthly);
        rule.by_day = vec![ByDay::every(Weekday::Monday)];
        rule.by_month_day = vec![5, 12];
        let occ = expand(
            &RecurrenceRule::Structured(rule),
            &wall(date(2026, 1, 5), 9, "UTC"),
            r,
        );
        assert!(occ.iter().all(|o| [5, 12].contains(&o.local.date().day())
            && Weekday::from_jiff(o.local.date().weekday()) == Weekday::Monday));
    }

    #[test]
    fn subdaily_expand_errors() {
        let start = wall(date(2026, 1, 5), 9, "UTC");
        let r = range("2026-01-01T00:00:00Z", "2027-01-01T00:00:00Z");
        for f in [Frequency::Minutely, Frequency::Secondly] {
            assert!(
                RecurrenceExpander::default()
                    .expand(
                        &RecurrenceRule::Structured(StructuredRule::new(f)),
                        &start,
                        r
                    )
                    .is_err()
            );
        }
    }

    #[test]
    fn duplicate_candidates_dedupe() {
        // dtstart itself matching the rule emits once, not twice.
        let start = wall(date(2026, 1, 5), 9, "UTC"); // Monday
        let r = range("2026-01-01T00:00:00Z", "2026-02-01T00:00:00Z");
        let occ = expand(
            &struct_rule(Frequency::Weekly, &[Weekday::Monday]),
            &start,
            r,
        );
        let days: Vec<Date> = occ.iter().map(|o| o.local.date()).collect();
        assert_eq!(days.iter().filter(|d| **d == date(2026, 1, 5)).count(), 1);
    }

    #[test]
    fn unresolvable_zone_expand_errors() {
        // A zone that no longer resolves makes expand fail.
        let start = wall(date(2026, 1, 5), 9, "Not/AZone");
        let r = range("2026-01-01T00:00:00Z", "2026-02-01T00:00:00Z");
        assert!(
            RecurrenceExpander::default()
                .expand(
                    &RecurrenceRule::Structured(StructuredRule::new(Frequency::Daily)),
                    &start,
                    r,
                )
                .is_err()
        );
    }

    #[test]
    fn expansion_to_the_year_9999_fails_cleanly() {
        // A wall time just below the civil-DateTime ceiling in a
        // UTC-minus zone materializes past Timestamp::MAX — expand
        // must surface a clean Fatal error, not panic.
        let start = wall(date(9999, 12, 31), 10, "Etc/GMT+12");
        let r = range("2026-01-01T00:00:00Z", "9999-12-30T00:00:00Z");
        let err = RecurrenceExpander::default()
            .expand(
                &RecurrenceRule::Structured(StructuredRule::new(Frequency::Weekly)),
                &start,
                r,
            )
            .unwrap_err();
        assert_eq!(err.kind(), crate::domain::error::ErrorKind::Fatal);
    }

    // ---------- internal candidate generators (edge dates) ----------

    #[test]
    fn bucket_helpers_reject_sub_daily_and_overflow_cleanly() {
        let hourly = StructuredRule::new(Frequency::Hourly);
        let dtstart = date(2026, 1, 5).at(9, 0, 0, 0);
        // Sub-daily frequencies aren't bucketed — the anchor is the
        // dtstart *date* at midnight, candidates error.
        assert_eq!(
            bucket_anchor(&hourly, dtstart, 3),
            dtstart.date().at(0, 0, 0, 0)
        );
        assert!(bucket_candidates(&hourly, dtstart, 0).is_err());

        let weekly = StructuredRule::new(Frequency::Weekly);
        // Date::MIN is a Monday, so a dtstart in the first civil
        // week still finds its bucket's Monday.
        let at_min = date(-9999, 1, 1).at(9, 0, 0, 0);
        assert!(!bucket_candidates(&weekly, at_min, 0).unwrap().is_empty());
        // A week start absurdly far out: the Monday shift overflows
        // → empty bucket.
        let start = date(2026, 1, 5).at(9, 0, 0, 0);
        assert!(
            bucket_candidates(&weekly, start, u32::MAX)
                .unwrap()
                .is_empty()
        );
        // A candidate weekday past Date::MAX is skipped — the set
        // comes back empty. (9999-12-31 is a Friday *and* Date::MAX,
        // so the overflowing candidate must be a later weekday.)
        let mut last_week = StructuredRule::new(Frequency::Weekly);
        last_week.by_day = vec![ByDay::every(Weekday::Sunday)];
        assert!(weekly_candidates(&last_week, date(9999, 12, 31), 0).is_empty());
        // month_days on an out-of-range year → empty.
        assert!(month_days(&weekly, 10000, 1, 15).is_empty());

        // A yearly rule whose filter never matches yields empty
        // buckets — `bucket_anchor`'s Yearly arm (+ `shift_years`)
        // is what stops the walk. dtstart still emits (RFC 5545).
        let occ = expand(
            &RecurrenceRule::Raw("FREQ=YEARLY;BYMONTH=2;BYMONTHDAY=30".into()),
            &wall(date(2026, 2, 15), 9, "UTC"),
            range("2026-01-01T00:00:00Z", "2030-01-01T00:00:00Z"),
        );
        assert_eq!(occ.len(), 1);
        assert_eq!(occ[0].local.date(), date(2026, 2, 15));
    }
}
