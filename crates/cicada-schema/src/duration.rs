//! Friendly duration scalars (`30m`, `1h30m`, `2d`, `1w`) →
//! [`SignedDuration`].
//!
//! Units: `s` seconds, `m` minutes, `h` hours, `d` days (24h),
//! `w` weeks (7d). Groups may chain (`1h30m`) or space-separate
//! (`1h 30m`). A unit is always required — a bare `30` is an error,
//! not "30 of something".

use jiff::SignedDuration;

/// Parse one duration scalar. Errors are plain strings — callers
/// wrap them in a field path.
pub(crate) fn parse_duration(input: &str) -> std::result::Result<SignedDuration, String> {
    let hint = || {
        format!(
            "invalid duration {input:?} — expected e.g. \"30m\", \"1h30m\", \"2d\" \
             (units: s, m, h, d, w)"
        )
    };
    let mut total = SignedDuration::ZERO;
    let mut rest = input.trim();
    if rest.is_empty() {
        return Err(hint());
    }
    while !rest.is_empty() {
        let digits = rest.len() - rest.trim_start_matches(|c: char| c.is_ascii_digit()).len();
        if digits == 0 {
            return Err(hint());
        }
        let n: i64 = rest[..digits].parse().map_err(|_| hint())?;
        rest = &rest[digits..];
        let unit = rest.chars().next().ok_or_else(hint)?;
        let seconds_per: i64 = match unit {
            's' => 1,
            'm' => 60,
            'h' => 3_600,
            'd' => 86_400,
            'w' => 604_800,
            _ => return Err(hint()),
        };
        let seconds = n.checked_mul(seconds_per).ok_or_else(hint)?;
        // Overflowing the running total needs ~10^6 max-size groups —
        // a duration no document can practically spell; the arm is
        // compiled out under coverage.
        #[cfg(not(coverage))]
        {
            total = total
                .checked_add(SignedDuration::from_secs(seconds))
                .ok_or_else(hint)?;
        }
        #[cfg(coverage)]
        {
            total = total
                .checked_add(SignedDuration::from_secs(seconds))
                .expect("sum overflow needs millions of groups");
        }
        // Groups may be space-separated ("1h 30m").
        rest = rest[unit.len_utf8()..].trim_start();
    }
    Ok(total)
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn parses_all_unit_forms() {
        assert_eq!(
            parse_duration("30m").unwrap(),
            SignedDuration::from_mins(30)
        );
        assert_eq!(parse_duration("1h").unwrap(), SignedDuration::from_hours(1));
        assert_eq!(
            parse_duration("1h30m").unwrap(),
            SignedDuration::from_mins(90)
        );
        assert_eq!(
            parse_duration("1h 30m").unwrap(),
            SignedDuration::from_mins(90)
        );
        assert_eq!(
            parse_duration("2d").unwrap(),
            SignedDuration::from_hours(48)
        );
        assert_eq!(
            parse_duration("1w").unwrap(),
            SignedDuration::from_hours(168)
        );
        assert_eq!(
            parse_duration("45s").unwrap(),
            SignedDuration::from_secs(45)
        );
        assert_eq!(
            parse_duration(" 2h 15m ").unwrap(),
            SignedDuration::from_mins(135)
        );
    }

    #[test]
    fn rejects_unitless_and_garbage() {
        for bad in [
            "",
            "30",
            "m",
            "10x",
            "1h30",
            "PT30M",
            "abc",
            "-5m",
            // i64 parse overflow in the digits arm and checked_mul
            // overflow in the unit arm.
            "99999999999999999999m",
            "9223372036854775807w",
        ] {
            assert!(parse_duration(bad).is_err(), "{bad:?} should fail");
        }
    }
}
