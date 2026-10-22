//! DATE, DATE-TIME and DURATION values (RFC 5545 §3.3.4, §3.3.5, §3.3.6).
//!
//! Times are converted to Unix seconds without a timezone database. Values
//! in local time (floating or with `TZID`) are flagged as such; callers that
//! need a UTC instant widen their ranges instead of guessing an offset.

use crate::IcalError;

/// A parsed DATE or DATE-TIME.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DateTime {
    /// Seconds since 1970-01-01T00:00:00, read as if the value were UTC.
    pub seconds: i64,
    /// `true` for a DATE value without time.
    pub is_date: bool,
    /// `true` if the value ends in `Z` and is therefore an exact UTC instant.
    pub is_utc: bool,
}

fn digits(s: &str) -> Result<i64, IcalError> {
    if s.is_empty() || !s.bytes().all(|b| b.is_ascii_digit()) {
        return Err(IcalError::Malformed("invalid date/time"));
    }
    s.parse()
        .map_err(|_| IcalError::Malformed("invalid date/time"))
}

pub(crate) fn is_leap(year: i64) -> bool {
    (year % 4 == 0 && year % 100 != 0) || year % 400 == 0
}

pub(crate) fn days_in_month(year: i64, month: i64) -> i64 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        _ if is_leap(year) => 29,
        _ => 28,
    }
}

/// Days since 1970-01-01 (proleptic Gregorian, H. Hinnant's algorithm).
pub(crate) fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (month + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// Calendar date `(year, month, day)` of a day number (inverse of
/// [`days_from_civil`]).
pub(crate) fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    (year, month, day)
}

/// Weekday of a day number, 0 = Monday … 6 = Sunday.
pub(crate) fn weekday(days: i64) -> i64 {
    // 1970-01-01 was a Thursday.
    (days + 3).rem_euclid(7)
}

/// First second of year 10000: every DATE or DATE-TIME (four-digit years)
/// lies before it.
pub(crate) const MAX_TIME: i64 = 253_402_300_800;

/// Parses a UTC offset `+HHMM` or `-HHMMSS` (RFC 5545 §3.3.14) into seconds.
pub(crate) fn parse_utc_offset(value: &str) -> Option<i64> {
    let (sign, rest) = match value.as_bytes().first()? {
        b'+' => (1, &value[1..]),
        b'-' => (-1, &value[1..]),
        _ => return None,
    };
    if !(rest.len() == 4 || rest.len() == 6) || !rest.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let hours: i64 = rest[0..2].parse().ok()?;
    let minutes: i64 = rest[2..4].parse().ok()?;
    let seconds: i64 = if rest.len() == 6 {
        rest[4..6].parse().ok()?
    } else {
        0
    };
    if hours > 23 || minutes > 59 || seconds > 59 {
        return None;
    }
    Some(sign * (hours * 3600 + minutes * 60 + seconds))
}

fn parse_date(s: &str) -> Result<i64, IcalError> {
    if s.len() != 8 {
        return Err(IcalError::Malformed("invalid date"));
    }
    let year = digits(&s[0..4])?;
    let month = digits(&s[4..6])?;
    let day = digits(&s[6..8])?;
    if !(1..=12).contains(&month) || day < 1 || day > days_in_month(year, month) {
        return Err(IcalError::Malformed("invalid date"));
    }
    Ok(days_from_civil(year, month, day) * 86_400)
}

/// Parses `YYYYMMDD` or `YYYYMMDDTHHMMSS[Z]`.
pub fn parse_date_or_datetime(value: &str) -> Result<DateTime, IcalError> {
    if !value.is_ascii() {
        return Err(IcalError::Malformed("invalid date/time"));
    }
    match value.len() {
        8 => Ok(DateTime {
            seconds: parse_date(value)?,
            is_date: true,
            is_utc: false,
        }),
        15 | 16 => {
            let (body, is_utc) = match value.strip_suffix('Z') {
                Some(body) => (body, true),
                None => (value, false),
            };
            if body.len() != 15 || &body[8..9] != "T" {
                return Err(IcalError::Malformed("invalid date-time"));
            }
            let hour = digits(&body[9..11])?;
            let minute = digits(&body[11..13])?;
            let second = digits(&body[13..15])?;
            // A leap second (60) is allowed by RFC 5545.
            if hour > 23 || minute > 59 || second > 60 {
                return Err(IcalError::Malformed("invalid time"));
            }
            Ok(DateTime {
                seconds: parse_date(&body[..8])? + hour * 3600 + minute * 60 + second,
                is_date: false,
                is_utc,
            })
        }
        _ => Err(IcalError::Malformed("invalid date/time")),
    }
}

/// Parses a UTC DATE-TIME as used in CalDAV `time-range` filters.
pub fn parse_utc_datetime(value: &str) -> Result<i64, IcalError> {
    let parsed = parse_date_or_datetime(value)?;
    if parsed.is_date || !parsed.is_utc {
        return Err(IcalError::Malformed("expected a UTC date-time"));
    }
    Ok(parsed.seconds)
}

/// Longest accepted duration: 100 years. Bounds all arithmetic.
const MAX_DURATION: i64 = 100 * 366 * 86_400;

/// Parses a DURATION such as `PT1H30M`, `P2D` or `-P1W` into seconds.
pub fn parse_duration(value: &str) -> Result<i64, IcalError> {
    let err = IcalError::Malformed("invalid duration");
    let (sign, rest) = match value.as_bytes().first() {
        Some(b'-') => (-1, &value[1..]),
        Some(b'+') => (1, &value[1..]),
        _ => (1, value),
    };
    let rest = rest.strip_prefix('P').ok_or(err.clone())?;
    if rest.is_empty() {
        return Err(err);
    }
    let mut total: i64 = 0;
    let mut in_time = false;
    let mut number = String::new();
    let mut seen_unit = false;
    let mut seen_time_unit = false;
    for c in rest.chars() {
        match c {
            '0'..='9' => {
                if number.len() >= 9 {
                    return Err(err);
                }
                number.push(c);
            }
            'T' if !in_time && number.is_empty() => in_time = true,
            'W' | 'D' if !in_time => {
                let n: i64 = number.parse().map_err(|_| err.clone())?;
                total += n * if c == 'W' { 7 * 86_400 } else { 86_400 };
                number.clear();
                seen_unit = true;
            }
            'H' | 'M' | 'S' if in_time => {
                let n: i64 = number.parse().map_err(|_| err.clone())?;
                total += n * match c {
                    'H' => 3600,
                    'M' => 60,
                    _ => 1,
                };
                number.clear();
                seen_unit = true;
                seen_time_unit = true;
            }
            _ => return Err(err),
        }
        if total > MAX_DURATION {
            return Err(IcalError::Limit("duration too long"));
        }
    }
    if !number.is_empty() || !seen_unit || (in_time && !seen_time_unit) {
        return Err(err);
    }
    Ok(sign * total)
}

#[cfg(test)]
mod tests;
