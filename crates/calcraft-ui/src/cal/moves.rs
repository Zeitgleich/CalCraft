//! Instances in the viewer's local time, and their changes by drag and
//! drop or keyboard, converted back to the API's encoding
//! ([`calcraft_core::api::EventTimes`]).
//!
//! Moves keep the wall-clock time: an event moved from a summer to a
//! winter day stays at 09:00 local time (its UTC instant changes by the
//! DST difference), like in every calendar application.

use calcraft_core::api::{EventInstance, EventTimes};

use super::{
    date::DAY,
    zone::{Zone, to_local, to_utc},
};

/// An instance in local wall time (seconds, read as if UTC).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LocalSpan {
    pub start: i64,
    pub end: i64,
    pub all_day: bool,
}

impl LocalSpan {
    /// The days the instance touches, `[first, last]`. An all-day event
    /// ends at midnight of the day after its last; a timed event ending at
    /// midnight does not touch the next day.
    pub fn days(&self) -> (i64, i64) {
        let first = self.start.div_euclid(DAY);
        let last = if self.end > self.start {
            (self.end - 1).div_euclid(DAY)
        } else {
            first
        };
        (first, last.max(first))
    }
}

/// The instance in the viewer's zone: UTC instants converted, all-day and
/// floating times as they are.
pub fn local_span(instance: &EventInstance, zone: &dyn Zone) -> LocalSpan {
    if instance.all_day || instance.floating {
        return LocalSpan {
            start: instance.start,
            end: instance.end.max(instance.start),
            all_day: instance.all_day,
        };
    }
    LocalSpan {
        start: to_local(zone, instance.start),
        end: to_local(zone, instance.end.max(instance.start)),
        all_day: false,
    }
}

/// The part of a timed instance on `day`, in minutes after local midnight
/// `[from, to)`; `None` if it does not touch the day.
pub fn day_piece(span: &LocalSpan, day: i64) -> Option<(i64, i64)> {
    let (first, last) = span.days();
    if span.all_day || day < first || day > last {
        return None;
    }
    let midnight = day * DAY;
    let from = (span.start - midnight).max(0) / 60;
    let to = ((span.end - midnight).min(DAY) / 60).max(from);
    Some((from, to))
}

/// Rounds minutes to the nearest multiple of `step`.
pub fn snap(minutes: i64, step: i64) -> i64 {
    (minutes as f64 / step as f64).round() as i64 * step
}

/// A change of an instance: moved by days and minutes, and its end moved by
/// `resize` minutes more.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Change {
    pub days: i64,
    pub minutes: i64,
    pub resize: i64,
}

impl Change {
    pub fn is_none(&self) -> bool {
        self.days == 0 && self.minutes == 0 && self.resize == 0
    }
}

/// Shortest event a resize leaves.
pub const MIN_MINUTES: i64 = 15;

/// The local span after a change (resizes keep at least
/// [`MIN_MINUTES`]; all-day events at least a day).
pub fn changed_span(span: &LocalSpan, change: Change) -> LocalSpan {
    let delta = if span.all_day {
        change.days * DAY
    } else {
        change.days * DAY + change.minutes * 60
    };
    let start = span.start + delta;
    let mut end = span.end + delta;
    if change.resize != 0 {
        let minimum = if span.all_day { DAY } else { MIN_MINUTES * 60 };
        let resize = if span.all_day {
            change.resize.div_euclid(24 * 60) * DAY
        } else {
            change.resize * 60
        };
        end = (end + resize).max(start + minimum);
    }
    LocalSpan {
        start,
        end,
        all_day: span.all_day,
    }
}

/// The API request for an instance after `change`. Timed events keep their
/// wall-clock times in the viewer's zone; floating and all-day events are
/// moved as local times.
pub fn changed_times(instance: &EventInstance, zone: &dyn Zone, change: Change) -> EventTimes {
    let span = local_span(instance, zone);
    let moved = changed_span(&span, change);
    let (start, end) = if instance.all_day || instance.floating {
        (moved.start, moved.end)
    } else {
        (to_utc(zone, moved.start), to_utc(zone, moved.end))
    };
    EventTimes {
        recurrence_id: instance.recurrence_id.clone(),
        start,
        end,
    }
}
