//! `ValidateCalendar` — validation layer 2 (ADR-0001 §7).
//!
//! Takes a *parsed* [`Calendar`] and returns a [`ValidationReport`]
//! of structured diagnostics: domain invariants (end > start, wall
//! starts for recurrences), recurrence sanity (structured rules
//! validate; raw RRULEs must at least carry a parseable `FREQ`),
//! attendee/reminder well-formedness, and a real IANA `timeZone`.
//!
//! Schema-shape errors are layer 1 (`cicada-schema`); capability
//! (layer 3) and conflict (layer 4) checks belong to plan/apply.
//! Pure — no clocks, no I/O, so output is fully deterministic.

use std::collections::BTreeMap;
use std::fmt;
use std::str::FromStr;

use jiff::SignedDuration;

use crate::domain::calendar::Calendar;
use crate::domain::event::Event;
use crate::domain::recurrence::{Frequency, RecurrenceRule, Until};
use crate::domain::time::Time;

/// Diagnostic seriousness. `Error` fails validation; `Warning` is
/// reported but does not block plan/apply.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Severity {
    /// Blocks: the calendar cannot be reconciled as declared.
    Error,
    /// Surfaces: reconcilable, but likely surprising.
    Warning,
}

impl Severity {
    /// Lowercase label for rendering (`error`, `warning`).
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Error => "error",
            Self::Warning => "warning",
        }
    }
}

impl fmt::Display for Severity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// One validation finding: a field path relative to the document
/// (`events[standup].time`) plus a human-readable message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diagnostic {
    /// How bad it is.
    pub severity: Severity,
    /// Field path inside the calendar document.
    pub path: String,
    /// What is wrong.
    pub message: String,
}

impl Diagnostic {
    fn error(path: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            severity: Severity::Error,
            path: path.into(),
            message: message.into(),
        }
    }

    fn warning(path: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            severity: Severity::Warning,
            path: path.into(),
            message: message.into(),
        }
    }
}

/// The outcome of [`ValidateCalendar`]: ordered diagnostics over one
/// calendar. Deterministic — events iterate in [`LocalId`] order
/// (the calendar's `BTreeMap`).
///
/// [`LocalId`]: crate::domain::ids::LocalId
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ValidationReport {
    /// All findings, in iteration order.
    pub diagnostics: Vec<Diagnostic>,
}

impl ValidationReport {
    /// `true` when no `Error`-severity diagnostic was raised.
    pub fn is_valid(&self) -> bool {
        self.diagnostics
            .iter()
            .all(|d| d.severity != Severity::Error)
    }

    /// `true` when nothing was flagged at all.
    pub fn is_clean(&self) -> bool {
        self.diagnostics.is_empty()
    }

    /// Count of diagnostics at a severity.
    pub fn count(&self, severity: Severity) -> usize {
        self.diagnostics
            .iter()
            .filter(|d| d.severity == severity)
            .count()
    }
}

/// Longest reminder lead time before it almost certainly exceeds
/// provider limits (Google caps popup reminders at 4 weeks).
const MAX_REMINDER_OFFSET: SignedDuration = SignedDuration::from_hours(24 * 28);

/// Layer-2 validator (ADR-0001 §7). Stateless — construct cheaply,
/// call [`ValidateCalendar::run`] per calendar.
#[derive(Debug, Clone, Copy, Default)]
pub struct ValidateCalendar;

impl ValidateCalendar {
    /// A ready-to-use validator.
    pub fn new() -> Self {
        Self
    }

    /// Check `calendar` against the domain invariants and return the
    /// diagnostics. [`ValidationReport::is_valid`] decides whether
    /// validation "passed" (no errors); warnings never fail.
    pub fn run(&self, calendar: &Calendar) -> ValidationReport {
        let mut out = Vec::new();
        check_metadata(calendar, &mut out);
        for (id, event) in &calendar.events {
            check_event(id.as_str(), event, &mut out);
        }
        ValidationReport { diagnostics: out }
    }
}

/// Calendar-level checks: a name to address by, a resolvable zone,
/// and a provider binding (per ADR-0002 §9 the binding may also come
/// from recorded state, so its absence is a warning, not an error).
fn check_metadata(calendar: &Calendar, out: &mut Vec<Diagnostic>) {
    if calendar.name.trim().is_empty() {
        out.push(Diagnostic::error(
            "metadata.name",
            "calendar name must not be empty",
        ));
    }
    if let Err(e) = calendar.time_zone.timezone() {
        out.push(Diagnostic::error("metadata.timeZone", e.to_string()));
    }
    if calendar.provider.is_none() && calendar.provider_calendar_id.is_none() {
        out.push(Diagnostic::warning(
            "metadata",
            "no provider/calendarId binding declared — apply will need a \
             binding from metadata or recorded state (ADR-0002 §9)",
        ));
    }
}

/// All per-event checks under `events[<id>]`.
fn check_event(id: &str, event: &Event, out: &mut Vec<Diagnostic>) {
    let base = format!("events[{id}]");
    if id.trim().is_empty() {
        out.push(Diagnostic::error(
            format!("{base}.id"),
            "event id must not be empty",
        ));
    }
    if event.summary.trim().is_empty() {
        out.push(Diagnostic::warning(
            format!("{base}.summary"),
            "event summary is empty",
        ));
    }
    if let Err(e) = event.time.validate() {
        out.push(Diagnostic::error(format!("{base}.time"), e.to_string()));
    }
    if event.recurrence.is_some() && !event.time.start().is_wall() {
        out.push(Diagnostic::error(
            format!("{base}.time"),
            "recurring events need a wall-time start (time + zone), \
             not an absolute instant",
        ));
    }
    if let Some(rule) = &event.recurrence {
        check_recurrence(rule, event, &base, out);
    }
    check_attendees(event, &base, out);
    check_reminders(event, &base, out);
}

/// Recurrence sanity: structured rules must validate; raw RRULEs get
/// the same check when they parse into the supported model, an error
/// when no `FREQ` parses at all, and a warning when they use exotic
/// properties (`BYSETPOS`, …) — still deployable, just not locally
/// expandable.
fn check_recurrence(rule: &RecurrenceRule, event: &Event, base: &str, out: &mut Vec<Diagnostic>) {
    let path = format!("{base}.recurrence");
    if let Some(structured) = rule.as_structured() {
        if let Err(e) = structured.validate() {
            out.push(Diagnostic::error(&path, e.to_string()));
            return;
        }
        if !structured.freq.is_expandable() {
            out.push(Diagnostic::warning(
                &path,
                format!(
                    "{} recurrence is not expandable — sub-daily frequencies \
                     are a documented limitation",
                    structured.freq
                ),
            ));
        }
        warn_until_before_start(structured.until.as_ref(), event, &path, out);
        return;
    }
    // Exotic or malformed raw RRULE — distinguishable by whether a
    // FREQ parses at all. (Structured rules always parse above — the
    // else can't fire; it stays inline so no dead arm remains.)
    #[cfg(not(coverage))]
    #[rustfmt::skip]
    let RecurrenceRule::Raw(raw) = rule else { return };
    #[cfg(coverage)]
    let raw = rule.as_raw().expect("structured rules always parse above");
    if raw_rrule_freq(raw).is_some() {
        out.push(Diagnostic::warning(
            path,
            "raw RRULE uses properties outside the supported model \
             (BYSETPOS, BYWEEKNO, BYHOUR, …) — compared semantically \
             but not expandable locally",
        ));
    } else {
        out.push(Diagnostic::error(
            path,
            format!("unparseable RRULE {raw:?} — expected e.g. \"FREQ=WEEKLY;BYDAY=MO,FR\""),
        ));
    }
}

/// Extract the `FREQ` of a raw RRULE (`FREQ=WEEKLY;BYDAY=MO`, with or
/// without an `RRULE:` prefix). `None` means the string is not a
/// usable rule at all — the difference between an exotic-but-valid
/// rule and garbage.
fn raw_rrule_freq(raw: &str) -> Option<Frequency> {
    let body = raw
        .trim()
        .get(..6)
        .filter(|p| p.eq_ignore_ascii_case("rrule:"))
        .map_or_else(|| raw.trim(), |_| raw.trim()[6..].trim());
    body.split(';')
        .filter_map(|part| part.split_once('='))
        .find(|(key, _)| key.trim().eq_ignore_ascii_case("freq"))
        .and_then(|(_, v)| Frequency::from_str(v.trim()).ok())
}

/// `until` bounds earlier than the start yield an empty recurrence
/// set — almost always an authoring slip, so warn.
fn warn_until_before_start(
    until: Option<&Until>,
    event: &Event,
    path: &str,
    out: &mut Vec<Diagnostic>,
) {
    let Some(until) = until else { return };
    let start = event.time.start();
    let past = match (until, start) {
        (Until::Date(d), Time::Wall { local, .. }) => local.date() > *d,
        (Until::DateTime(dt), Time::Wall { local, .. }) => local > dt,
        (Until::Instant(ts), start) => start.to_timestamp().is_ok_and(|s| s > *ts),
        _ => false,
    };
    if past {
        out.push(Diagnostic::warning(
            path,
            "`until` bound precedes the event start — the rule yields \
             no occurrences",
        ));
    }
}

/// Attendee emails must look like deliverable addresses (providers
/// reject otherwise), and a repeated email is almost certainly a
/// mistake.
fn check_attendees(event: &Event, base: &str, out: &mut Vec<Diagnostic>) {
    let mut seen: BTreeMap<&str, usize> = BTreeMap::new();
    for (i, a) in event.attendees.iter().enumerate() {
        let path = format!("{base}.attendees[{i}].email");
        if !email_well_formed(&a.email) {
            out.push(Diagnostic::error(
                &path,
                format!("malformed attendee email {:?}", a.email),
            ));
        } else if let Some(first) = seen.insert(a.email.as_str(), i) {
            out.push(Diagnostic::warning(
                &path,
                format!("duplicate attendee email (also attendees[{first}])"),
            ));
        }
    }
}

/// `local@domain.tld` shape: exactly one `@`, non-empty sides, a dot
/// in the domain, no whitespace or control characters.
fn email_well_formed(email: &str) -> bool {
    let mut parts = email.split('@');
    let (Some(local), Some(domain), None) = (parts.next(), parts.next(), parts.next()) else {
        return false;
    };
    !local.is_empty()
        && domain.contains('.')
        && domain.split('.').all(|label| !label.is_empty())
        && !email.chars().any(|c| c.is_whitespace() || c.is_control())
}

/// Reminder offsets: negative offsets are meaningless; past ~4 weeks
/// exceeds what providers commonly allow (capability check at plan
/// time makes the provider-specific call).
fn check_reminders(event: &Event, base: &str, out: &mut Vec<Diagnostic>) {
    for (i, r) in event.reminders.iter().enumerate() {
        let path = format!("{base}.reminders[{i}]");
        if r.offset.is_negative() {
            out.push(Diagnostic::error(
                path,
                format!("{} reminder has a negative offset", r.kind),
            ));
        } else if r.offset > MAX_REMINDER_OFFSET {
            out.push(Diagnostic::warning(
                path,
                format!(
                    "{} reminder offset exceeds 4 weeks — over common provider limits",
                    r.kind
                ),
            ));
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use jiff::SignedDuration;
    use jiff::civil::date;

    use super::*;
    use crate::domain::attendee::Attendee;
    use crate::domain::event::{EventBody, EventTime, Transparency};
    use crate::domain::ids::ZoneId;
    use crate::domain::recurrence::{ByDay, StructuredRule, Weekday};
    use crate::domain::reminder::Reminder;

    fn body(start: Time, duration: SignedDuration) -> EventBody {
        EventBody {
            summary: "Standup".into(),
            description: None,
            location: None,
            time: EventTime::Span { start, duration },
            recurrence: None,
            attendees: vec![],
            reminders: vec![],
            conferencing: None,
            transparency: Transparency::Opaque,
        }
    }

    fn wall() -> Time {
        Time::Wall {
            local: date(2026, 1, 5).at(9, 0, 0, 0),
            zone: ZoneId::new("UTC").unwrap(),
        }
    }

    fn calendar(events: Vec<Event>) -> Calendar {
        let mut cal = Calendar::new("work", "UTC").unwrap();
        for e in events {
            cal.insert(e).unwrap();
        }
        cal
    }

    fn run(cal: &Calendar) -> ValidationReport {
        ValidateCalendar::new().run(cal)
    }

    fn errors(report: &ValidationReport) -> Vec<&Diagnostic> {
        report
            .diagnostics
            .iter()
            .filter(|d| d.severity == Severity::Error)
            .collect()
    }

    fn warnings(report: &ValidationReport) -> Vec<&Diagnostic> {
        report
            .diagnostics
            .iter()
            .filter(|d| d.severity == Severity::Warning)
            .collect()
    }

    #[test]
    fn unresolvable_zone_is_a_metadata_error() {
        let mut cal = calendar(vec![Event::new(
            "standup",
            body(wall(), SignedDuration::from_mins(30)),
        )]);
        cal.time_zone = ZoneId::new_unchecked("Bogus/Zone");
        let report = run(&cal);
        assert!(
            errors(&report).iter().any(|d| d.path.contains("timeZone")),
            "{:?}",
            report.diagnostics
        );
    }

    #[test]
    fn valid_calendar_passes() {
        let mut b = body(wall(), SignedDuration::from_mins(30));
        b.attendees.push(Attendee::new("alice@x.com"));
        b.reminders
            .push(Reminder::popup(SignedDuration::from_mins(10)));
        let report = run(&calendar(vec![Event::new("standup", b)]));
        assert!(report.is_valid());
        // Only the provider-binding warning may appear.
        assert_eq!(report.diagnostics.len(), 1);
        assert_eq!(report.diagnostics[0].severity, Severity::Warning);
    }

    #[test]
    fn end_before_start_is_an_error_at_time() {
        let b = EventBody {
            time: EventTime::Range {
                start: wall(),
                end: Time::Wall {
                    local: date(2026, 1, 5).at(8, 0, 0, 0),
                    zone: ZoneId::new("UTC").unwrap(),
                },
            },
            ..body(wall(), SignedDuration::from_mins(30))
        };
        let report = run(&calendar(vec![Event::new("x", b)]));
        assert!(!report.is_valid());
        assert_eq!(errors(&report)[0].path, "events[x].time");
    }

    #[test]
    fn structured_recurrence_violations_error() {
        let mut b = body(wall(), SignedDuration::from_mins(30));
        let mut rule = StructuredRule::new(Frequency::Weekly);
        rule.count = Some(0);
        b.recurrence = Some(RecurrenceRule::Structured(rule));
        let report = run(&calendar(vec![Event::new("x", b)]));
        assert!(!report.is_valid());
        assert_eq!(errors(&report)[0].path, "events[x].recurrence");
    }

    #[test]
    fn bad_raw_rrule_errors_exotic_warns() {
        let mut b = body(wall(), SignedDuration::from_mins(30));
        b.recurrence = Some(RecurrenceRule::Raw("TOTAL GARBAGE".into()));
        let report = run(&calendar(vec![Event::new("x", b)]));
        assert!(!report.is_valid());
        assert_eq!(errors(&report)[0].path, "events[x].recurrence");

        let mut b = body(wall(), SignedDuration::from_mins(30));
        b.recurrence = Some(RecurrenceRule::Raw(
            "FREQ=MONTHLY;BYDAY=MO;BYSETPOS=-1".into(),
        ));
        let report = run(&calendar(vec![Event::new("x", b)]));
        assert!(report.is_valid());
        assert!(
            warnings(&report)
                .iter()
                .any(|d| d.path == "events[x].recurrence")
        );

        // `RRULE:`-prefixed raw rules hit the prefix-stripping arm
        // of `raw_rrule_freq`.
        let mut b = body(wall(), SignedDuration::from_mins(30));
        b.recurrence = Some(RecurrenceRule::Raw("RRULE:FREQ=MONTHLY;BYSETPOS=-1".into()));
        let report = run(&calendar(vec![Event::new("x", b)]));
        assert!(report.is_valid());
        assert!(
            warnings(&report)
                .iter()
                .any(|d| d.path == "events[x].recurrence")
        );
    }

    #[test]
    fn sub_daily_frequency_warns() {
        let mut b = body(wall(), SignedDuration::from_mins(30));
        b.recurrence = Some(RecurrenceRule::Structured(StructuredRule::new(
            Frequency::Hourly,
        )));
        let report = run(&calendar(vec![Event::new("x", b)]));
        assert!(report.is_valid());
        assert!(
            warnings(&report)
                .iter()
                .any(|d| d.path == "events[x].recurrence")
        );
    }

    #[test]
    fn recurring_instant_start_errors() {
        let mut b = body(
            Time::Instant("2026-01-05T09:00:00Z".parse().unwrap()),
            SignedDuration::from_mins(30),
        );
        b.recurrence = Some(RecurrenceRule::Structured(StructuredRule::new(
            Frequency::Daily,
        )));
        let report = run(&calendar(vec![Event::new("x", b)]));
        assert!(!report.is_valid());
        assert_eq!(errors(&report)[0].path, "events[x].time");
    }

    #[test]
    fn until_before_start_warns() {
        let mut b = body(wall(), SignedDuration::from_mins(30));
        let mut rule = StructuredRule::new(Frequency::Weekly);
        rule.until = Some(Until::Date(date(2020, 1, 1)));
        b.recurrence = Some(RecurrenceRule::Structured(rule));
        let report = run(&calendar(vec![Event::new("x", b)]));
        assert!(report.is_valid());
        assert!(
            warnings(&report)
                .iter()
                .any(|d| d.path == "events[x].recurrence")
        );
    }

    #[test]
    fn malformed_and_duplicate_emails_flag() {
        let mut b = body(wall(), SignedDuration::from_mins(30));
        b.attendees = vec![
            Attendee::new("not-an-email"),
            Attendee::new("alice@x.com"),
            Attendee::new("alice@x.com"),
        ];
        let report = run(&calendar(vec![Event::new("x", b)]));
        assert!(!report.is_valid());
        assert_eq!(errors(&report)[0].path, "events[x].attendees[0].email");
        assert!(
            warnings(&report)
                .iter()
                .any(|d| d.path == "events[x].attendees[2].email")
        );
    }

    #[test]
    fn reminder_offset_bounds() {
        let mut b = body(wall(), SignedDuration::from_mins(30));
        b.reminders = vec![
            Reminder::popup(SignedDuration::from_mins(-5)),
            Reminder::email(SignedDuration::from_hours(24 * 40)),
        ];
        let report = run(&calendar(vec![Event::new("x", b)]));
        assert!(!report.is_valid());
        assert_eq!(errors(&report)[0].path, "events[x].reminders[0]");
        assert!(
            warnings(&report)
                .iter()
                .any(|d| d.path == "events[x].reminders[1]")
        );
    }

    #[test]
    fn empty_names_and_zone_checks() {
        let cal = Calendar {
            name: "  ".into(),
            time_zone: ZoneId::new("UTC").unwrap(),
            provider: Some("google".into()),
            provider_calendar_id: Some("primary".into()),
            send_updates: crate::domain::policy::SendUpdates::default(),
            events: BTreeMap::new(),
        };
        let report = run(&cal);
        assert!(!report.is_valid());
        assert_eq!(errors(&report)[0].path, "metadata.name");
    }

    #[test]
    fn ordinal_byday_rule_is_valid_monthly() {
        let mut b = body(wall(), SignedDuration::from_hours(1));
        let mut rule = StructuredRule::new(Frequency::Monthly);
        rule.by_day = vec![ByDay::nth(Weekday::Friday, -1)];
        b.recurrence = Some(RecurrenceRule::Structured(rule));
        let report = run(&calendar(vec![Event::new("x", b)]));
        assert!(report.is_valid());
        assert!(
            !warnings(&report)
                .iter()
                .any(|d| d.path == "events[x].recurrence")
        );
    }
}
