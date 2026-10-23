//! Alarms (`VALARM`, RFC 5545 §3.6.6) of stored calendar objects: when
//! they are due, for e-mail reminders.
//!
//! Triggers are relative to the start or end of each instance
//! (`TRIGGER:-PT15M`, `RELATED=END`) or absolute (`VALUE=DATE-TIME`, fires
//! once per component), with `REPEAT`/`DURATION`. Instances come from the
//! recurrence engine (RRULE, RDATE, EXDATE, overrides) within the same
//! work [`Budget`]; only instances with exact UTC times get alarms.
//! Floating times and dates are read in the calendar's time zone if one
//! is given, otherwise as UTC.
//!
//! Alarms that were acknowledged at or after their trigger (RFC 9074
//! `ACKNOWLEDGED`, Thunderbird's `X-MOZ-LASTACK`) are skipped, as are
//! cancelled events and completed tasks. Everything is bounded: alarms per
//! component, repetitions, occurrences per call.

use crate::{
    Component, ComponentKind,
    datetime::{parse_date_or_datetime, parse_duration},
    recurrence::{self, Budget, Exact, Floating},
};

/// Alarms evaluated per component; further ones are ignored.
pub const MAX_ALARMS_PER_COMPONENT: usize = 32;
/// Largest `REPEAT` honoured; larger values are capped.
pub const MAX_REPEAT: u32 = 20;
/// Most occurrences one call returns (the earliest ones).
pub const MAX_OCCURRENCES: usize = 500;
/// Shortest `DURATION` between repetitions that is honoured (one minute).
const MIN_REPEAT_INTERVAL: i64 = 60;
const DAY: i64 = 86_400;

/// What an alarm does in the client.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AlarmAction {
    Email,
    Display,
    Audio,
}

/// Which alarms count.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Selection {
    /// Only `ACTION:EMAIL`.
    EmailOnly,
    /// `EMAIL`, `DISPLAY` and `AUDIO`.
    All,
}

impl Selection {
    fn includes(self, action: AlarmAction) -> bool {
        action == AlarmAction::Email || self == Selection::All
    }
}

/// Exact times of the instance an alarm belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Instance {
    /// Start in UTC seconds (DTSTART, or DUE for a task without DTSTART).
    pub start: i64,
    /// End in UTC seconds (DTEND, DUE, or the start).
    pub end: i64,
    /// Start in the instance's own local time, naive seconds (format with
    /// the TZID of the component's DTSTART, if any).
    pub local_start: i64,
    /// End in local time, naive seconds.
    pub local_end: i64,
    /// An all-day instance.
    pub is_date: bool,
}

impl From<Exact> for Instance {
    fn from(exact: Exact) -> Self {
        Self {
            start: exact.start,
            end: exact.end,
            local_start: exact.local_start,
            local_end: exact.local_end,
            is_date: exact.is_date,
        }
    }
}

/// One due alarm.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AlarmOccurrence {
    /// When it fires, UTC seconds.
    pub trigger: i64,
    pub action: AlarmAction,
    /// Index of the component (master or override) in the root's
    /// `components`; its SUMMARY, LOCATION, … describe the instance.
    pub component: usize,
    pub kind: ComponentKind,
    pub instance: Instance,
}

/// The alarms could not be evaluated (budget exhausted or a construct the
/// recurrence engine cannot evaluate exactly).
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("alarms cannot be evaluated")]
pub struct Unevaluable;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Trigger {
    /// Offset from the start (or the end with `RELATED=END`).
    Relative { offset: i64, end: bool },
    /// UTC instant.
    Absolute(i64),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Alarm {
    action: AlarmAction,
    trigger: Trigger,
    repeat: u32,
    interval: i64,
    acknowledged: Option<i64>,
}

impl Alarm {
    /// Offsets of the repetitions relative to the first trigger.
    fn repetitions(&self) -> impl Iterator<Item = i64> + '_ {
        (0..=i64::from(self.repeat)).map(|n| n.saturating_mul(self.interval))
    }

    fn last_repetition(&self) -> i64 {
        i64::from(self.repeat).saturating_mul(self.interval)
    }

    fn acknowledged(&self, trigger: i64) -> bool {
        self.acknowledged.is_some_and(|ack| ack >= trigger)
    }
}

fn utc_value(component: &Component, name: &str) -> Option<i64> {
    let value = parse_date_or_datetime(&component.property(name)?.value).ok()?;
    (value.is_utc && !value.is_date).then_some(value.seconds)
}

fn parse_alarm(valarm: &Component) -> Option<Alarm> {
    let action = match valarm
        .property("ACTION")?
        .value
        .to_ascii_uppercase()
        .as_str()
    {
        "EMAIL" => AlarmAction::Email,
        "DISPLAY" => AlarmAction::Display,
        "AUDIO" => AlarmAction::Audio,
        _ => return None,
    };
    let trigger = valarm.property("TRIGGER")?;
    let absolute = trigger
        .parameter("VALUE")
        .is_some_and(|p| p.values.iter().any(|v| v.eq_ignore_ascii_case("DATE-TIME")));
    let trigger = if absolute {
        // RFC 5545: an absolute trigger MUST be in UTC.
        let value = parse_date_or_datetime(&trigger.value).ok()?;
        if !value.is_utc || value.is_date {
            return None;
        }
        Trigger::Absolute(value.seconds)
    } else {
        let end = trigger
            .parameter("RELATED")
            .is_some_and(|p| p.values.iter().any(|v| v.eq_ignore_ascii_case("END")));
        Trigger::Relative {
            offset: parse_duration(&trigger.value).ok()?,
            end,
        }
    };
    let repeat = valarm
        .property("REPEAT")
        .and_then(|p| p.value.trim().parse::<u32>().ok())
        .unwrap_or(0)
        .min(MAX_REPEAT);
    let interval = valarm
        .property("DURATION")
        .and_then(|p| parse_duration(&p.value).ok())
        .unwrap_or(0);
    // REPEAT needs a positive DURATION (both or neither, RFC 5545).
    let (repeat, interval) = if repeat > 0 && interval >= MIN_REPEAT_INTERVAL {
        (repeat, interval)
    } else {
        (0, 0)
    };
    let acknowledged = match (
        utc_value(valarm, "ACKNOWLEDGED"),
        utc_value(valarm, "X-MOZ-LASTACK"),
    ) {
        (Some(a), Some(b)) => Some(a.max(b)),
        (a, b) => a.or(b),
    };
    Some(Alarm {
        action,
        trigger,
        repeat,
        interval,
        acknowledged,
    })
}

/// Whether the component's alarms are moot: a cancelled event or task, or
/// a completed task.
fn inactive(component: &Component) -> bool {
    let status = component
        .property("STATUS")
        .map(|p| p.value.to_ascii_uppercase());
    matches!(status.as_deref(), Some("CANCELLED" | "COMPLETED"))
        || (component.name == "VTODO" && component.property("COMPLETED").is_some())
}

fn alarms_of(component: &Component, selection: Selection) -> Vec<Alarm> {
    if inactive(component) {
        return Vec::new();
    }
    component
        .components
        .iter()
        .filter(|c| c.name == "VALARM")
        .take(MAX_ALARMS_PER_COMPONENT)
        .filter_map(parse_alarm)
        .filter(|alarm| selection.includes(alarm.action))
        .collect()
}

/// The alarms of every instance component of `root` (index, kind, alarms).
fn components(root: &Component, selection: Selection) -> Vec<(usize, ComponentKind, Vec<Alarm>)> {
    root.components
        .iter()
        .enumerate()
        .filter_map(|(index, component)| {
            let kind = ComponentKind::from_name(&component.name)?;
            if kind == ComponentKind::Journal {
                return None;
            }
            let alarms = alarms_of(component, selection);
            (!alarms.is_empty()).then_some((index, kind, alarms))
        })
        .collect()
}

/// The VTIMEZONE of a `CALDAV:calendar-timezone` value (a VCALENDAR with
/// one VTIMEZONE, RFC 4791 §5.2.2), for reading floating times.
pub fn calendar_timezone(text: &str) -> Option<Component> {
    let root = crate::parse(text.trim().as_bytes()).ok()?;
    root.components.into_iter().find(|c| c.name == "VTIMEZONE")
}

/// Occurrences of alarms with `after < trigger <= until`, earliest first
/// (at most [`MAX_OCCURRENCES`]). Several alarms of one instance that fire
/// at the same moment count once.
pub fn alarm_occurrences(
    root: &Component,
    after: i64,
    until: i64,
    floating: Option<&Component>,
    selection: Selection,
    budget: &mut Budget,
) -> Result<Vec<AlarmOccurrence>, Unevaluable> {
    let floating = floating.map_or(Floating::Utc, Floating::Zone);
    let components = components(root, selection);
    let mut out: Vec<AlarmOccurrence> = Vec::new();
    if components.is_empty() || until <= after {
        return Ok(out);
    }
    let in_window = |t: i64| t > after && t <= until;

    // Absolute triggers fire once per component, independent of instances.
    for (index, kind, alarms) in &components {
        let component = &root.components[*index];
        let mut instance = None;
        for alarm in alarms {
            let Trigger::Absolute(at) = alarm.trigger else {
                continue;
            };
            for repetition in alarm.repetitions() {
                let trigger = at.saturating_add(repetition);
                if !in_window(trigger) || alarm.acknowledged(trigger) {
                    continue;
                }
                if instance.is_none() {
                    instance = Some(
                        recurrence::first_instance(root, component, floating, budget)
                            .map_err(|_| Unevaluable)?,
                    );
                }
                if let Some(Some(exact)) = instance {
                    out.push(AlarmOccurrence {
                        trigger,
                        action: alarm.action,
                        component: *index,
                        kind: *kind,
                        instance: exact.into(),
                    });
                }
            }
        }
    }

    // Relative triggers: instances whose start (or end) lies within the
    // window shifted by the alarm offsets.
    let relative = components
        .iter()
        .flat_map(|(_, _, alarms)| alarms)
        .filter_map(|alarm| match alarm.trigger {
            Trigger::Relative { offset, .. } => {
                Some((offset, offset.saturating_add(alarm.last_repetition())))
            }
            Trigger::Absolute(_) => None,
        });
    let (min_offset, max_offset) = relative.fold((i64::MAX, i64::MIN), |(lo, hi), (a, b)| {
        (lo.min(a), hi.max(b))
    });
    if min_offset <= max_offset {
        let start = after.saturating_sub(max_offset);
        let end = until.saturating_sub(min_offset).saturating_add(1);
        recurrence::alarm_instances(
            root,
            start,
            end,
            floating,
            budget,
            &mut |component, exact| {
                let Some(exact) = exact else {
                    return;
                };
                let Some((index, kind, alarms)) = components
                    .iter()
                    .find(|(index, ..)| std::ptr::eq(&root.components[*index], component))
                else {
                    return;
                };
                for alarm in alarms {
                    let Trigger::Relative { offset, end } = alarm.trigger else {
                        continue;
                    };
                    let base = if end { exact.end } else { exact.start };
                    for repetition in alarm.repetitions() {
                        let trigger = base.saturating_add(offset).saturating_add(repetition);
                        if in_window(trigger) && !alarm.acknowledged(trigger) {
                            // A bound for pathological objects; the earliest
                            // occurrences are kept below.
                            if out.len() >= MAX_OCCURRENCES * 4 {
                                return;
                            }
                            out.push(AlarmOccurrence {
                                trigger,
                                action: alarm.action,
                                component: *index,
                                kind: *kind,
                                instance: exact.into(),
                            });
                        }
                    }
                }
            },
        )
        .map_err(|_| Unevaluable)?;
    }

    out.sort_by_key(|o| {
        (
            o.trigger,
            o.instance.start,
            o.component,
            action_rank(o.action),
        )
    });
    out.dedup_by(|b, a| {
        a.trigger == b.trigger && a.instance.start == b.instance.start && a.component == b.component
    });
    out.truncate(MAX_OCCURRENCES);
    Ok(out)
}

/// E-mail first, so that it survives deduplication.
fn action_rank(action: AlarmAction) -> u8 {
    match action {
        AlarmAction::Email => 0,
        AlarmAction::Display => 1,
        AlarmAction::Audio => 2,
    }
}

/// The first trigger after `after` within `horizon` seconds, searched in
/// growing windows so that near alarms are cheap to find.
pub fn next_alarm(
    root: &Component,
    after: i64,
    horizon: i64,
    floating: Option<&Component>,
    selection: Selection,
    budget: &mut Budget,
) -> Result<Option<i64>, Unevaluable> {
    let limit = after.saturating_add(horizon.max(0));
    let mut from = after;
    let mut width = DAY;
    while from < limit {
        let to = from.saturating_add(width).min(limit);
        let found = alarm_occurrences(root, from, to, floating, selection, budget)?;
        if let Some(first) = found.first() {
            return Ok(Some(first.trigger));
        }
        from = to;
        width = width.saturating_mul(4);
    }
    Ok(None)
}

#[cfg(test)]
#[path = "alarm_tests.rs"]
mod tests;
