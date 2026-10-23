//! Local time in a time zone known only as a function "UTC instant →
//! offset" (what the browser offers), with the RFC 5545 rules for local
//! times that do not exist (DST gap: shifted forward by the gap) or exist
//! twice (DST overlap: the first one).

use calcraft_core::api::{Observance, TimeZoneInfo};

use super::date::{DAY, civil, days_from_civil, days_in_month, weekday};

/// A time zone: the UTC offset (seconds east of UTC) in effect at an instant.
pub trait Zone {
    fn offset(&self, utc: i64) -> i64;
}

/// A zone with a constant offset (UTC, tests).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Fixed(pub i64);

impl Zone for Fixed {
    fn offset(&self, _: i64) -> i64 {
        self.0
    }
}

/// Local wall time (seconds, read as if UTC) of a UTC instant.
pub fn to_local(zone: &dyn Zone, utc: i64) -> i64 {
    utc + zone.offset(utc)
}

/// The UTC instant of a local wall time.
pub fn to_utc(zone: &dyn Zone, local: i64) -> i64 {
    let before = zone.offset(local - DAY);
    let after = zone.offset(local + DAY);
    let first = local - before;
    let second = local - after;
    match (zone.offset(first) == before, zone.offset(second) == after) {
        (true, true) => first.min(second),
        (true, false) => first,
        (false, true) => second,
        // In a gap: the offset before it, i.e. shifted forward.
        (false, false) => local - before,
    }
}

/// UTC instant of local midnight starting `days`.
pub fn day_start(zone: &dyn Zone, days: i64) -> i64 {
    to_utc(zone, days * DAY)
}

/// `+HHMM` / `-HHMM`.
pub fn format_offset(seconds: i64) -> String {
    let sign = if seconds < 0 { '-' } else { '+' };
    let minutes = seconds.abs() / 60;
    format!("{sign}{:02}{:02}", minutes / 60, minutes % 60)
}

/// One change of the offset.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Transition {
    /// UTC instant of the change.
    pub at: i64,
    pub from: i64,
    pub to: i64,
}

/// The offset changes in `year` (found day by day, then bisected).
pub fn transitions(zone: &dyn Zone, year: i64) -> Vec<Transition> {
    let start = days_from_civil(year, 1, 1) * DAY;
    let end = days_from_civil(year + 1, 1, 1) * DAY;
    let mut found = Vec::new();
    let mut t = start;
    let mut current = zone.offset(t);
    while t < end {
        let next = (t + DAY).min(end);
        let offset = zone.offset(next);
        if offset != current {
            // The change lies in (t, next]: bisect to the second.
            let (mut lo, mut hi) = (t, next);
            while hi - lo > 1 {
                let mid = lo + (hi - lo) / 2;
                if zone.offset(mid) == current {
                    lo = mid;
                } else {
                    hi = mid;
                }
            }
            found.push(Transition {
                at: hi,
                from: current,
                to: offset,
            });
            current = offset;
        }
        t = next;
    }
    found
}

/// The `BYDAY` of a date as a yearly rule: `2SU`, or `-1SU` for the last
/// such weekday of the month.
fn by_day(days: i64) -> (u32, String) {
    const NAMES: [&str; 7] = ["MO", "TU", "WE", "TH", "FR", "SA", "SU"];
    let (year, month, day) = civil(days);
    let name = NAMES[weekday(days)];
    if day + 7 > days_in_month(year, month) {
        (month, format!("-1{name}"))
    } else {
        (month, format!("{}{name}", (day - 1) / 7 + 1))
    }
}

/// The day of 1970 a `BYDAY` rule names.
fn in_1970(month: u32, by_day: &str) -> i64 {
    const NAMES: [&str; 7] = ["MO", "TU", "WE", "TH", "FR", "SA", "SU"];
    let (n, name) = by_day.split_at(by_day.len() - 2);
    let wanted = NAMES.iter().position(|d| *d == name).unwrap_or(6);
    let n: i64 = n.parse().unwrap_or(1);
    if n < 0 {
        let last = days_from_civil(1970, month, days_in_month(1970, month));
        last - (weekday(last) as i64 - wanted as i64).rem_euclid(7)
    } else {
        let first = days_from_civil(1970, month, 1);
        first + (wanted as i64 - weekday(first) as i64).rem_euclid(7) + (n - 1) * 7
    }
}

fn stamp(local: i64) -> String {
    let (year, month, day) = civil(local.div_euclid(DAY));
    let time = local.rem_euclid(DAY);
    format!(
        "{year:04}{month:02}{day:02}T{:02}{:02}{:02}",
        time / 3600,
        time / 60 % 60,
        time % 60
    )
}

/// The VTIMEZONE definition of `zone` as seen in `year`: one observance
/// per offset change, as a yearly rule from 1970 when the change falls on
/// an n-th (or last) weekday as in all current DST rules; a zone without
/// changes gets one STANDARD observance.
pub fn definition(zone: &dyn Zone, tzid: &str, year: i64) -> TimeZoneInfo {
    let changes = transitions(zone, year);
    let mut observances = Vec::new();
    if changes.is_empty() {
        let offset = format_offset(zone.offset(days_from_civil(year, 7, 1) * DAY));
        observances.push(Observance {
            daylight: false,
            start: "19700101T000000".to_owned(),
            offset_from: offset.clone(),
            offset_to: offset,
            rrule: None,
            name: None,
        });
    }
    let max = changes.iter().map(|c| c.to).max().unwrap_or(0);
    let yearly = changes.len() == 2;
    for change in &changes {
        // The onset in the local time before the change.
        let local = change.at + change.from;
        let day = local.div_euclid(DAY);
        let (start, rrule) = if yearly {
            let (month, rule) = by_day(day);
            let start = in_1970(month, &rule) * DAY + local.rem_euclid(DAY);
            (
                stamp(start),
                Some(format!("FREQ=YEARLY;BYMONTH={month};BYDAY={rule}")),
            )
        } else {
            (stamp(local), None)
        };
        observances.push(Observance {
            daylight: yearly && change.to == max && change.to > change.from,
            start,
            offset_from: format_offset(change.from),
            offset_to: format_offset(change.to),
            rrule,
            name: None,
        });
    }
    TimeZoneInfo {
        tzid: tzid.to_owned(),
        observances,
    }
}
