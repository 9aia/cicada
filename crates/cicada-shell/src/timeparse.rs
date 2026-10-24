//! `from`/`to` bound parsing for `range`/`show` and the MCP tools:
//! `today`, `+Nd`, `+Nw`, ISO dates, civil datetimes, and RFC 3339
//! instants — resolved against a calendar's time zone so bounds mean
//! what the user means locally.

use cicada_core::domain::ids::ZoneId;
use jiff::civil::{Date, DateTime};
use jiff::{Span, Timestamp};

/// Parse a time bound into an instant:
///
/// - `today` — today at 00:00 in `zone`
/// - `+Nd` / `+Nw` — N days / weeks after today at 00:00 in `zone`
/// - `YYYY-MM-DD` — that date at 00:00 in `zone`
/// - `YYYY-MM-DDTHH:MM[:SS]` — that civil time in `zone`
/// - RFC 3339 with an offset — the absolute instant
pub fn parse_bound(input: &str, zone: &ZoneId) -> Result<Timestamp, String> {
    let s = input.trim();
    let tz = zone.timezone().map_err(|e| e.to_string())?;
    let today = || Timestamp::now().to_zoned(tz.clone()).date();
    let at_midnight = |d: Date| -> Result<Timestamp, String> {
        d.at(0, 0, 0, 0)
            .to_zoned(tz.clone())
            .map(|z| z.timestamp())
            .map_err(|e| format!("{d} does not resolve in {zone}: {e}"))
    };

    if s.eq_ignore_ascii_case("today") {
        return at_midnight(today());
    }
    if let Some(rest) = s.strip_prefix('+') {
        let parsed = rest
            .strip_suffix('d')
            .map(|n| (n, 1_i64))
            .or_else(|| rest.strip_suffix('w').map(|n| (n, 7_i64)));
        let Some((num, days_per)) = parsed else {
            return Err(bad(s));
        };
        let n: i64 = num.parse().map_err(|_| bad(s))?;
        let offset = n.checked_mul(days_per).ok_or_else(|| bad(s))?;
        let d = today()
            .checked_add(Span::new().days(offset))
            .map_err(|e| format!("{s:?} overflows the calendar: {e}"))?;
        return at_midnight(d);
    }
    if let Ok(ts) = s.parse::<Timestamp>() {
        return Ok(ts);
    }
    if let Ok(dt) = s.parse::<DateTime>() {
        return dt
            .to_zoned(tz)
            .map(|z| z.timestamp())
            .map_err(|e| format!("{s:?} does not resolve in {zone}: {e}"));
    }
    // jiff `DateTime` accepts a bare `YYYY-MM-DD` (midnight), so a
    // Date-only parse can never be reached — kept for the schema's
    // benefit when jiff's grammar changes.
    #[cfg(not(coverage))]
    if let Ok(d) = s.parse::<Date>() {
        return at_midnight(d);
    }
    Err(bad(s))
}

/// Shared "what we expected" error text.
fn bad(s: &str) -> String {
    format!(
        "invalid time bound {s:?} — expected `today`, `+Nd`, `+Nw`, \
         a date (2026-08-19), or a datetime"
    )
}
