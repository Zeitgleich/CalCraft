//! Instances of an object for display (the web UI's calendar views).
//!
//! Unlike the time-range matching above, which only needs a UTC interval
//! guaranteed to contain each instance, a calendar view needs the instance
//! itself: its start and end as an exact UTC instant where the object's
//! VTIMEZONE can be evaluated, otherwise as local (floating) time or as a
//! date, and the RECURRENCE-ID that names it. The same [`Series`] and
//! [`Zones`] code generates the instances, with the same [`Budget`] and
//! [`MAX_INSTANCES`] limits.
//!
//! THISANDFUTURE overrides are shown as a single changed instance (the
//! later instances keep the master's times); this only affects display.

use std::collections::HashSet;

use super::{
    Budget, DAY, Inexact, InstantKeys, MARGIN, MAX_INSTANCES, PeriodEnd, SLACK, Series, Value,
    Zone, Zones, is_instance_component, master_instance,
};
use crate::{Component, Property, datetime::format_date_or_datetime, rrule::Expansion, tz::Tz};

/// Start or end of a displayed instance.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstanceTime {
    /// An exact instant (Unix seconds).
    Utc(i64),
    /// Local time without a usable zone (Unix seconds read as if UTC).
    Floating(i64),
    /// A date: midnight of that day (Unix seconds read as if UTC).
    Date(i64),
}

impl InstanceTime {
    /// The seconds, whatever their meaning.
    pub fn seconds(self) -> i64 {
        match self {
            Self::Utc(s) | Self::Floating(s) | Self::Date(s) => s,
        }
    }

    /// UTC bounds of the value: local times may be anywhere within
    /// [`SLACK`] of their naive value.
    fn bounds(self) -> (i64, i64) {
        match self {
            Self::Utc(s) => (s, s),
            Self::Floating(s) | Self::Date(s) => (s.saturating_sub(SLACK), s.saturating_add(SLACK)),
        }
    }
}

/// One instance of an object.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Instance {
    /// Index of the component (master or override) in `root.components`.
    pub component: usize,
    pub start: InstanceTime,
    /// Exclusive end; equal to `start` for zero-length events.
    pub end: InstanceTime,
    /// For instances of a recurring object: the RECURRENCE-ID value that
    /// names the instance, formatted like the master's DTSTART (an
    /// override's own value for overrides). `None` for single objects.
    pub recurrence_id: Option<String>,
}

/// Instances near `[start, end)` (UTC seconds; local times are compared
/// with ±14 h slack, so callers filter again in the viewer's zone).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Expanded {
    pub instances: Vec<Instance>,
    /// `false` if more than `max` instances overlapped the range: only the
    /// first `max` (in generation order) are returned.
    pub complete: bool,
}

/// Generates the instances of `root` overlapping `[start, end)`, at most
/// `max`. `None` if the object cannot be evaluated within `budget` or uses
/// something the recurrence engine does not support.
pub fn instances(
    root: &Component,
    start: i64,
    end: i64,
    budget: &mut Budget,
    max: usize,
) -> Option<Expanded> {
    expand(root, start, end, budget, max).ok()
}

fn expand(
    root: &Component,
    start: i64,
    end: i64,
    budget: &mut Budget,
    max: usize,
) -> Result<Expanded, Inexact> {
    let mut zones = Zones::new(root);
    let mut overridden = HashSet::new();
    let mut series = Vec::new();
    let mut recurring = false;
    for (index, component) in root.components.iter().enumerate() {
        if !is_instance_component(component) {
            continue;
        }
        budget.charge(1)?;
        let built = Series::build(&mut zones, component, budget)?;
        let rid = match component.property("RECURRENCE-ID") {
            Some(property) => {
                recurring = true;
                let value = zones
                    .value(property, &property.value, budget)?
                    .ok_or(Inexact)?;
                if let Some(key) = zones.key(value) {
                    overridden.insert(key);
                }
                Some(property.value.clone())
            }
            None => None,
        };
        if let Some(built) = built {
            recurring |= built.rule.is_some() || !built.rdates.is_empty();
            series.push((index, built, rid));
        }
    }

    let mut out = Expanded {
        instances: Vec::new(),
        complete: true,
    };
    let none = HashSet::new();
    for (index, built, rid) in &series {
        let is_override = rid.is_some();
        let skip = if is_override { &none } else { &overridden };
        let mut push = |value: Value, period: Option<PeriodEnd>, budget: &mut Budget| {
            budget.charge(super::INSTANCE_COST)?;
            if zones
                .key(value)
                .is_some_and(|k| built.exdates.contains(&k) || skip.contains(&k))
            {
                return Ok(true);
            }
            let (from, to) = times(built, &zones, value, period);
            let (lo, _) = from.bounds();
            let (_, hi) = to.bounds();
            // Zero-length instances match at their start (RFC 4791 §9.9).
            if lo >= end || hi.max(lo.saturating_add(1)) <= start {
                return Ok(true);
            }
            if out.instances.len() >= max {
                out.complete = false;
                return Ok(false);
            }
            let recurrence_id = match rid {
                Some(rid) => Some(rid.clone()),
                None if recurring => Some(
                    format_date_or_datetime(value.naive, value.is_date, value.zone == Zone::Utc)
                        .ok_or(Inexact)?,
                ),
                None => None,
            };
            out.instances.push(Instance {
                component: *index,
                start: from,
                end: to,
                recurrence_id,
            });
            Ok(true)
        };
        if !each_start(built, &zones, budget, start, end, &mut push)? {
            break;
        }
    }
    Ok(out)
}

/// Callback of [`each_start`]: an instance start (with an RDATE period's
/// end); `false` stops.
type StartVisitor<'v> =
    dyn FnMut(Value, Option<PeriodEnd>, &mut Budget) -> Result<bool, Inexact> + 'v;

/// Visits the starts of a series' instances near `[start, end)` (DTSTART
/// or the rule's instances, then RDATEs). `visit` returns `false` to stop;
/// so does this function.
fn each_start(
    series: &Series,
    zones: &Zones<'_>,
    budget: &mut Budget,
    start: i64,
    end: i64,
    visit: &mut StartVisitor<'_>,
) -> Result<bool, Inexact> {
    match &series.rule {
        None => {
            if !visit(series.start, None, budget)? {
                return Ok(false);
            }
        }
        Some(rule) => {
            let (until_local, until_utc) = series.until(zones, rule);
            let mut expansion =
                Expansion::new(rule, series.start.naive, series.start.is_date, until_local)
                    .ok_or(Inexact)?;
            expansion.skip_to(
                start
                    .saturating_sub(series.max_duration())
                    .saturating_sub(MARGIN),
            );
            let mut visited = 0usize;
            while let Some(naive) = expansion.next(budget)? {
                visited += 1;
                if visited > MAX_INSTANCES {
                    return Err(Inexact);
                }
                if naive > end.saturating_add(MARGIN) {
                    break;
                }
                let value = Value {
                    naive,
                    ..series.start
                };
                // UNTIL is inclusive: instances certainly after it are dropped.
                if until_utc.is_some_and(|until| zones.bounds(naive, value.zone).0 > until) {
                    continue;
                }
                if !visit(value, None, budget)? {
                    return Ok(false);
                }
            }
        }
    }
    for &(value, period) in &series.rdates {
        if !visit(value, period, budget)? {
            return Ok(false);
        }
    }
    Ok(true)
}

/// A local time as an instance time: exact if its zone can be evaluated.
fn local_time(zones: &Zones<'_>, naive: i64, value: Value) -> InstanceTime {
    if value.is_date {
        return InstanceTime::Date(naive);
    }
    match value.zone {
        Zone::Utc => InstanceTime::Utc(naive),
        Zone::Tz(index) => match zones.tz(index).and_then(|tz| tz.to_utc(naive)) {
            Some(utc) => InstanceTime::Utc(utc),
            None => InstanceTime::Floating(naive),
        },
        Zone::Floating | Zone::Unresolved => InstanceTime::Floating(naive),
    }
}

/// Start and end of the instance starting at `value`.
fn times(
    series: &Series,
    zones: &Zones<'_>,
    value: Value,
    period: Option<PeriodEnd>,
) -> (InstanceTime, InstanceTime) {
    let from = local_time(zones, value.naive, value);
    let same_kind = |end: Value| end.zone == value.zone && end.is_date == value.is_date;
    let shifted = |seconds: i64| match from {
        InstanceTime::Utc(s) => InstanceTime::Utc(s.saturating_add(seconds)),
        InstanceTime::Floating(s) => InstanceTime::Floating(s.saturating_add(seconds)),
        InstanceTime::Date(s) => InstanceTime::Date(s.saturating_add(seconds)),
    };
    let to = match period {
        Some(PeriodEnd::At(end)) if same_kind(end) => local_time(zones, end.naive, end),
        Some(PeriodEnd::At(end)) => {
            let (lo, _) = from.bounds();
            let (_, hi) = zones.bounds(end.naive, end.zone);
            shifted(hi.saturating_sub(lo).max(0))
        }
        Some(PeriodEnd::After(d)) => shifted(d),
        None => match (series.nominal, series.exact) {
            (Some(nominal), _) => local_time(zones, value.naive.saturating_add(nominal), value),
            (None, Some(exact)) => shifted(exact),
            (None, None) if value.is_date => shifted(DAY),
            (None, None) => from,
        },
    };
    (from, to)
}

/// The master component of a recurring object: the first event, to-do or
/// journal without RECURRENCE-ID.
fn master(root: &Component) -> Option<&Component> {
    root.components
        .iter()
        .find(|c| is_instance_component(c) && c.property("RECURRENCE-ID").is_none())
}

/// Checks a RECURRENCE-ID value given for the master of `root`: if the
/// master has an instance there that is not excluded, returns the value
/// formatted like its DTSTART (for a new override or an EXDATE). The value
/// is read like the master's DTSTART (same TZID); a UTC value (`…Z`) is
/// compared as an instant.
pub fn master_instance_id(root: &Component, rid: &str) -> Option<String> {
    let start = master(root)?.property("DTSTART")?;
    let mut keys = InstantKeys::new(root);
    let key = keys.key(start, rid)?;
    master_instance(root, &key, &mut Budget::new(super::OBJECT_BUDGET))
        .ok()
        .flatten()
}

/// Index (in `root.components`) of the override for the instance named by
/// `rid` (read like [`master_instance_id`]), if there is one.
pub fn override_index(root: &Component, rid: &str) -> Option<usize> {
    let start = master(root)
        .and_then(|m| m.property("DTSTART"))
        .or_else(|| {
            root.components
                .iter()
                .find(|c| is_instance_component(c))
                .and_then(|c| c.property("RECURRENCE-ID"))
        })?;
    let mut keys = InstantKeys::new(root);
    let wanted = keys.key(start, rid)?;
    root.components.iter().position(|c| {
        is_instance_component(c)
            && c.property("RECURRENCE-ID")
                .and_then(|p| keys.key(p, &p.value))
                .is_some_and(|key| key == wanted)
    })
}

/// Converts a local time in the zone `tzid` (defined by a VTIMEZONE of
/// `root`) to UTC. `None` if the zone is missing or cannot be evaluated.
pub fn zone_to_utc(root: &Component, tzid: &str, naive: i64) -> Option<i64> {
    zone(root, tzid)?.to_utc(naive)
}

/// Converts a UTC instant to local time in the zone `tzid` of `root`.
pub fn zone_to_local(root: &Component, tzid: &str, utc: i64) -> Option<i64> {
    zone(root, tzid)?.to_local(utc)
}

fn zone(root: &Component, tzid: &str) -> Option<Tz> {
    let vtimezone = root
        .components
        .iter()
        .find(|c| c.name == "VTIMEZONE" && c.property("TZID").is_some_and(|p| p.value == tzid))?;
    Tz::build(vtimezone, &mut Budget::new(super::OBJECT_BUDGET))
        .ok()
        .flatten()
}

/// Whether two RECURRENCE-ID/EXDATE values of `root` (each with the
/// property it belongs to) name the same instant.
pub fn same_instant(root: &Component, a: (&Property, &str), b: (&Property, &str)) -> bool {
    let mut keys = InstantKeys::new(root);
    match (keys.key(a.0, a.1), keys.key(b.0, b.1)) {
        (Some(a), Some(b)) => a == b,
        _ => false,
    }
}

#[cfg(test)]
#[path = "recurrence_expand_tests.rs"]
mod tests;
