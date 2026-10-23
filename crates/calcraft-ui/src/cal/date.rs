//! Civil dates as days since 1970-01-01 (proleptic Gregorian), weeks
//! starting on Monday.

use crate::text;

/// Seconds per day.
pub const DAY: i64 = 86_400;

/// Days since 1970-01-01 of a date (H. Hinnant's algorithm).
pub fn days_from_civil(year: i64, month: u32, day: u32) -> i64 {
    let (month, day) = (i64::from(month), i64::from(day));
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let yoe = year.rem_euclid(400);
    let mp = (month + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// The date of a day number: `(year, month 1-12, day 1-31)`.
pub fn civil(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    // Both are small and positive by construction.
    (year, month.unsigned_abs() as u32, day.unsigned_abs() as u32)
}

pub fn is_leap(year: i64) -> bool {
    (year % 4 == 0 && year % 100 != 0) || year % 400 == 0
}

pub fn days_in_month(year: i64, month: u32) -> u32 {
    match month {
        2 if is_leap(year) => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    }
}

/// Weekday: 0 = Monday … 6 = Sunday.
pub fn weekday(days: i64) -> usize {
    // 1970-01-01 was a Thursday.
    (days + 3).rem_euclid(7) as usize
}

/// The Monday on or before `days`.
pub fn week_start(days: i64) -> i64 {
    days - weekday(days) as i64
}

pub fn month_start(days: i64) -> i64 {
    let (year, month, _) = civil(days);
    days_from_civil(year, month, 1)
}

/// `days` moved by `months`, the day of month clamped (31 Jan + 1 = 28/29 Feb).
pub fn add_months(days: i64, months: i64) -> i64 {
    let (year, month, day) = civil(days);
    let index = year * 12 + i64::from(month) - 1 + months;
    let (year, month) = (index.div_euclid(12), (index.rem_euclid(12) + 1) as u32);
    days_from_civil(year, month, day.min(days_in_month(year, month)))
}

/// First day of the six-week grid showing the month of `days`.
pub fn month_grid_start(days: i64) -> i64 {
    week_start(month_start(days))
}

/// ISO 8601 week number.
pub fn iso_week(days: i64) -> u32 {
    // The Thursday of this week decides the year.
    let thursday = week_start(days) + 3;
    let (year, ..) = civil(thursday);
    let first = days_from_civil(year, 1, 1);
    ((thursday - first) / 7 + 1) as u32
}

/// Parses `YYYY-MM-DD`.
pub fn parse_date(value: &str) -> Option<i64> {
    let value = value.trim();
    let mut parts = value.split('-');
    let (y, m, d) = (parts.next()?, parts.next()?, parts.next()?);
    if parts.next().is_some() || y.len() != 4 || m.len() != 2 || d.len() != 2 {
        return None;
    }
    let digits = |s: &str| s.bytes().all(|b| b.is_ascii_digit());
    if !(digits(y) && digits(m) && digits(d)) {
        return None;
    }
    let (year, month, day): (i64, u32, u32) = (y.parse().ok()?, m.parse().ok()?, d.parse().ok()?);
    if !(1..=12).contains(&month) || day == 0 || day > days_in_month(year, month) {
        return None;
    }
    Some(days_from_civil(year, month, day))
}

/// `YYYY-MM-DD`.
pub fn format_date(days: i64) -> String {
    let (year, month, day) = civil(days);
    format!("{year:04}-{month:02}-{day:02}")
}

/// Parses `HH:MM` (also `H:MM`) into minutes after midnight.
pub fn parse_time(value: &str) -> Option<i64> {
    let (hours, minutes) = value.trim().split_once(':')?;
    let ok = |s: &str, len: std::ops::RangeInclusive<usize>| {
        len.contains(&s.len()) && s.bytes().all(|b| b.is_ascii_digit())
    };
    if !ok(hours, 1..=2) || !ok(minutes, 2..=2) {
        return None;
    }
    let (h, m): (i64, i64) = (hours.parse().ok()?, minutes.parse().ok()?);
    (h < 24 && m < 60).then_some(h * 60 + m)
}

/// `HH:MM` of minutes after midnight.
pub fn format_time(minutes: i64) -> String {
    let minutes = minutes.rem_euclid(24 * 60);
    format!("{:02}:{:02}", minutes / 60, minutes % 60)
}

/// `HH:MM` of local seconds.
pub fn clock(local: i64) -> String {
    format_time(local.rem_euclid(DAY) / 60)
}

/// “Mon 5 Oct”.
pub fn short_day(days: i64) -> String {
    let (_, month, day) = civil(days);
    format!(
        "{} {day} {}",
        text::WEEKDAYS_SHORT[weekday(days)],
        text::MONTHS_SHORT[month as usize - 1]
    )
}

/// “Monday, 5 October 2026”.
pub fn long_day(days: i64) -> String {
    let (year, month, day) = civil(days);
    format!(
        "{}, {day} {} {year}",
        text::WEEKDAYS_LONG[weekday(days)],
        text::MONTHS[month as usize - 1]
    )
}

/// “October 2026”.
pub fn month_title(days: i64) -> String {
    let (year, month, _) = civil(days);
    format!("{} {year}", text::MONTHS[month as usize - 1])
}

/// “5 – 11 Oct 2026”, “28 Sep – 4 Oct 2026”, “29 Dec 2025 – 4 Jan 2026”.
pub fn range_title(first: i64, last: i64) -> String {
    let (y1, m1, d1) = civil(first);
    let (y2, m2, d2) = civil(last);
    let month = |m: u32| text::MONTHS_SHORT[m as usize - 1];
    if y1 != y2 {
        format!("{d1} {} {y1} – {d2} {} {y2}", month(m1), month(m2))
    } else if m1 != m2 {
        format!("{d1} {} – {d2} {} {y2}", month(m1), month(m2))
    } else {
        format!("{d1} – {d2} {} {y2}", month(m2))
    }
}
