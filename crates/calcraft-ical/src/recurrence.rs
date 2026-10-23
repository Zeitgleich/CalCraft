//! Instances of calendar objects for time-range matching (RFC 4791 §9.9).
//!
//! An object's instances come from DTSTART, RRULE and RDATE, minus EXDATE,
//! with RECURRENCE-ID overrides replacing the instances they name. Each
//! instance is reported as a UTC interval that is guaranteed to contain it:
//!
//! - exact for UTC times and for local times whose TZID refers to a
//!   VTIMEZONE in the object that [`Tz`] can evaluate;
//! - widened by [`SLACK`] (±14 h, the largest real UTC offset) for floating
//!   times, all-day dates and TZIDs without a usable VTIMEZONE.
//!
//! All work is charged to a [`Budget`]. When it runs out, or the object
//! uses something this module cannot evaluate, the result is
//! [`Inexact`] and callers fall back to conservative answers: an object
//! that might match is always returned, never dropped.

use std::{
    collections::HashSet,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

use crate::{
    Component, Property,
    datetime::{DateTime, parse_date_or_datetime, parse_duration},
    object::{ComponentKind, TimeRange},
    rrule::{Expansion, Rule, parse_rule},
    tz::{MAX_OFFSET, Tz},
};

/// Largest UTC offset of any real timezone (UTC+14, UTC-12).
pub(crate) const SLACK: i64 = 14 * 3600;
/// Default work budget for evaluating one object.
pub const OBJECT_BUDGET: u64 = 200_000;
/// Most instances one expansion may visit before it counts as too large.
pub const MAX_INSTANCES: usize = 10_000;
/// Steps charged per instance: up to three time zone conversions, each a
/// bounded search (see [`Tz`]).
const INSTANCE_COST: u64 = 4;
/// Distinct TZIDs evaluated per object.
const MAX_ZONES: usize = 16;
/// RDATE plus EXDATE values per component.
const MAX_DATE_VALUES: usize = 10_000;
const DAY: i64 = 86_400;
/// Instances are generated in local time; this covers any offset between
/// local and UTC order plus widening.
pub(crate) const MARGIN: i64 = MAX_OFFSET + SLACK + DAY;

/// Work budget for recurrence evaluation, in abstract steps (one loop
/// iteration, generated candidate, date value or time zone conversion
/// each). Optionally tied to a cancel flag: once it is set, the budget
/// counts as exhausted.
#[derive(Debug, Clone)]
pub struct Budget {
    steps: u64,
    cancel: Option<Arc<AtomicBool>>,
}

impl Budget {
    pub fn new(steps: u64) -> Self {
        Self {
            steps,
            cancel: None,
        }
    }

    /// A budget that also ends when `cancel` becomes `true` (e.g. because
    /// the request that needs the answer has gone away).
    pub fn with_cancel(steps: u64, cancel: Arc<AtomicBool>) -> Self {
        Self {
            steps,
            cancel: Some(cancel),
        }
    }

    /// Steps left.
    pub fn remaining(&self) -> u64 {
        self.steps
    }

    pub(crate) fn charge(&mut self, steps: u64) -> Result<(), Inexact> {
        let cancelled = self
            .cancel
            .as_ref()
            .is_some_and(|flag| flag.load(Ordering::Relaxed));
        match self.steps.checked_sub(steps) {
            Some(left) if !cancelled => {
                self.steps = left;
                Ok(())
            }
            _ => {
                self.steps = 0;
                Err(Inexact)
            }
        }
    }
}

/// The exact answer could not be computed (budget exhausted or an
/// unsupported construct); use a conservative one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Inexact;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Zone {
    Utc,
    Floating,
    /// Index into [`Zones::cache`].
    Tz(usize),
    /// Too many zones: never exact, never equal to anything.
    Unresolved,
}

/// Identity of a start time, for EXDATE and RECURRENCE-ID matching. Only
/// values that are certainly equal produce equal keys.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct Key {
    /// 0 = UTC, 1 = floating, n + 2 = unresolvable zone n.
    zone: usize,
    time: i64,
    is_date: bool,
}

#[derive(Debug, Clone, Copy)]
struct Value {
    naive: i64,
    is_date: bool,
    zone: Zone,
}

/// Interval in UTC seconds containing an instance; `i64::MIN`/`MAX` for
/// unbounded sides.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Span {
    pub lo: i64,
    pub hi: i64,
}

impl Span {
    fn overlaps(self, start: Option<i64>, end: Option<i64>) -> bool {
        end.is_none_or(|e| self.lo < e) && start.is_none_or(|s| self.hi > s)
    }
}

/// Exact times of one instance, for alarms: only computed where start and
/// end are exact UTC instants.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Exact {
    /// Start in UTC seconds.
    pub start: i64,
    /// End in UTC seconds (at least `start`); DUE for VTODOs.
    pub end: i64,
    /// Start in the instance's own local time (naive seconds).
    pub local_start: i64,
    /// End in the instance's own local time (naive seconds).
    pub local_end: i64,
    /// The instance is a whole day (DATE value).
    pub is_date: bool,
}

struct Zones<'a> {
    root: &'a Component,
    cache: Vec<(&'a str, Option<Tz>)>,
    /// Read floating times and dates as UTC instead of widening them (free
    /// busy answers, which must not report ±14 h around every all-day
    /// event).
    floating_utc: bool,
    /// Read floating times and dates in this zone instead (alarms, with
    /// the calendar's time zone); takes precedence over `floating_utc`.
    floating_tz: Option<Tz>,
    /// Compute [`Exact`] times for every instance (alarms).
    want_exact: bool,
}

impl<'a> Zones<'a> {
    fn new(root: &'a Component) -> Self {
        Self {
            root,
            cache: Vec::new(),
            floating_utc: false,
            floating_tz: None,
            want_exact: false,
        }
    }

    fn tz(&self, index: usize) -> Option<&Tz> {
        self.cache.get(index).and_then(|(_, tz)| tz.as_ref())
    }

    fn zone(
        &mut self,
        property: &'a Property,
        value: &DateTime,
        budget: &mut Budget,
    ) -> Result<Zone, Inexact> {
        if value.is_utc {
            return Ok(Zone::Utc);
        }
        let tzid = property.parameter("TZID").and_then(|p| p.values.first());
        let Some(tzid) = tzid.filter(|_| !value.is_date) else {
            return Ok(Zone::Floating);
        };
        if let Some(index) = self.cache.iter().position(|(id, _)| id == tzid) {
            return Ok(Zone::Tz(index));
        }
        if self.cache.len() >= MAX_ZONES {
            return Ok(Zone::Unresolved);
        }
        let vtimezone = self.root.components.iter().find(|c| {
            c.name == "VTIMEZONE" && c.property("TZID").is_some_and(|p| p.value == *tzid)
        });
        let tz = match vtimezone {
            Some(vtimezone) => Tz::build(vtimezone, budget)?,
            None => None,
        };
        self.cache.push((tzid, tz));
        Ok(Zone::Tz(self.cache.len() - 1))
    }

    fn value(
        &mut self,
        property: &'a Property,
        raw: &str,
        budget: &mut Budget,
    ) -> Result<Option<Value>, Inexact> {
        let Ok(parsed) = parse_date_or_datetime(raw) else {
            return Ok(None);
        };
        Ok(Some(Value {
            naive: parsed.seconds,
            is_date: parsed.is_date,
            zone: self.zone(property, &parsed, budget)?,
        }))
    }

    /// UTC bounds of a local time.
    fn bounds(&self, naive: i64, zone: Zone) -> (i64, i64) {
        let exact = match zone {
            Zone::Utc => Some(naive),
            Zone::Floating if self.floating_tz.is_some() => {
                self.floating_tz.as_ref().and_then(|tz| tz.to_utc(naive))
            }
            Zone::Floating if self.floating_utc => Some(naive),
            Zone::Tz(index) => self.tz(index).and_then(|tz| tz.to_utc(naive)),
            Zone::Floating | Zone::Unresolved => None,
        };
        match exact {
            Some(utc) => (utc, utc),
            None => (naive.saturating_sub(SLACK), naive.saturating_add(SLACK)),
        }
    }

    fn key(&self, value: Value) -> Option<Key> {
        let (zone, time) = match value.zone {
            Zone::Utc => (0, value.naive),
            Zone::Floating => (1, value.naive),
            Zone::Tz(index) => match self.tz(index).and_then(|tz| tz.to_utc(value.naive)) {
                Some(utc) => (0, utc),
                None => (index + 2, value.naive),
            },
            Zone::Unresolved => return None,
        };
        Some(Key {
            zone,
            time,
            is_date: value.is_date && zone != 0,
        })
    }
}

/// Explicit end of an RDATE period.
#[derive(Debug, Clone, Copy)]
enum PeriodEnd {
    At(Value),
    After(i64),
}

/// THISANDFUTURE override: later instances move by a delta and take the
/// override's length. Bounds only, as the exact semantics need property
/// inheritance this server does not model.
#[derive(Debug, Clone, Copy)]
struct Shift {
    rid_lo: i64,
    delta_lo: i64,
    delta_hi: i64,
    len_hi: i64,
}

/// One component (master or override) with everything needed to generate
/// its instances.
struct Series {
    todo: bool,
    start: Value,
    /// Duration in local time (DTEND - DTSTART in the same zone, DURATION).
    nominal: Option<i64>,
    /// Duration in UTC seconds.
    exact: Option<i64>,
    rule: Option<Rule>,
    rdates: Vec<(Value, Option<PeriodEnd>)>,
    exdates: HashSet<Key>,
}

fn is_period(property: &Property) -> bool {
    property
        .parameter("VALUE")
        .is_some_and(|p| p.values.iter().any(|v| v.eq_ignore_ascii_case("PERIOD")))
}

impl Series {
    /// `Ok(None)` for components without DTSTART (and without DUE).
    fn build<'a>(
        zones: &mut Zones<'a>,
        component: &'a Component,
        budget: &mut Budget,
    ) -> Result<Option<Self>, Inexact> {
        let kind = ComponentKind::from_name(&component.name).ok_or(Inexact)?;
        let end_name = match kind {
            ComponentKind::Event => Some("DTEND"),
            ComponentKind::Todo => Some("DUE"),
            ComponentKind::Journal => None,
        };
        let end_property = end_name.and_then(|n| component.property(n));
        let (start_property, end_property) = match (component.property("DTSTART"), end_property) {
            (Some(start), end) => (start, end),
            // A VTODO with only DUE: a single point in time.
            (None, Some(due)) if kind == ComponentKind::Todo => (due, None),
            (None, _) => return Ok(None),
        };
        let start = zones
            .value(start_property, &start_property.value, budget)?
            .ok_or(Inexact)?;
        let duration = match component.property("DURATION") {
            Some(p) => Some(parse_duration(&p.value).map_err(|_| Inexact)?),
            None => None,
        };
        let (nominal, exact) = match (end_property, duration) {
            (Some(end_property), _) => {
                let end = zones
                    .value(end_property, &end_property.value, budget)?
                    .ok_or(Inexact)?;
                let nominal = (end.zone == start.zone && end.is_date == start.is_date)
                    .then(|| end.naive - start.naive);
                let (s_lo, s_hi) = zones.bounds(start.naive, start.zone);
                let (e_lo, e_hi) = zones.bounds(end.naive, end.zone);
                let exact = if s_lo == s_hi && e_lo == e_hi {
                    Some(e_lo - s_lo)
                } else if nominal.is_none() {
                    Some(e_hi - s_lo)
                } else {
                    None
                };
                (nominal.map(|d| d.max(0)), exact.map(|d| d.max(0)))
            }
            (None, Some(d)) => (Some(d.max(0)), Some(d.max(0))),
            (None, None) if start.is_date && kind != ComponentKind::Todo => (Some(DAY), None),
            (None, None) => (None, Some(0)),
        };
        let rule = match component.property("RRULE") {
            Some(p) if component.property("DTSTART").is_some() => {
                Some(parse_rule(&p.value).map_err(|_| Inexact)?)
            }
            Some(_) => return Err(Inexact),
            None => None,
        };

        let mut values = 0usize;
        let mut rdates = Vec::new();
        for property in component.properties_named("RDATE") {
            let period = is_period(property);
            for raw in property.value.split(',') {
                values += 1;
                if values > MAX_DATE_VALUES {
                    return Err(Inexact);
                }
                budget.charge(1)?;
                if period {
                    let (from, to) = raw.split_once('/').ok_or(Inexact)?;
                    let value = zones.value(property, from, budget)?.ok_or(Inexact)?;
                    let end = if to.starts_with(['P', '+', '-']) {
                        PeriodEnd::After(parse_duration(to).map_err(|_| Inexact)?.max(0))
                    } else {
                        PeriodEnd::At(zones.value(property, to, budget)?.ok_or(Inexact)?)
                    };
                    rdates.push((value, Some(end)));
                } else {
                    rdates.push((zones.value(property, raw, budget)?.ok_or(Inexact)?, None));
                }
            }
        }
        let mut exdates = HashSet::new();
        for property in component.properties_named("EXDATE") {
            for raw in property.value.split(',') {
                values += 1;
                if values > MAX_DATE_VALUES {
                    return Err(Inexact);
                }
                budget.charge(1)?;
                // Unreadable values exclude nothing (conservative).
                if let Some(key) = zones
                    .value(property, raw, budget)?
                    .and_then(|v| zones.key(v))
                {
                    exdates.insert(key);
                }
            }
        }
        Ok(Some(Self {
            todo: kind == ComponentKind::Todo,
            start,
            nominal,
            exact,
            rule,
            rdates,
            exdates,
        }))
    }

    /// Exact times of the instance starting at `start`; `None` unless start
    /// and end are exact instants.
    fn exact(&self, zones: &Zones<'_>, start: Value, end: Option<PeriodEnd>) -> Option<Exact> {
        let (lo, hi) = zones.bounds(start.naive, start.zone);
        if lo != hi {
            return None;
        }
        let exact_at = |naive: i64, zone: Zone| {
            let (a, b) = zones.bounds(naive, zone);
            (a == b).then_some(a)
        };
        let (end_utc, local_end) = match end {
            Some(PeriodEnd::At(end)) => (exact_at(end.naive, end.zone)?, end.naive),
            Some(PeriodEnd::After(d)) => (
                exact_at(start.naive.saturating_add(d), start.zone)?,
                start.naive.saturating_add(d),
            ),
            None => match (self.nominal, self.exact) {
                (Some(d), _) => (
                    exact_at(start.naive.saturating_add(d), start.zone)?,
                    start.naive.saturating_add(d),
                ),
                (None, Some(d)) => (lo.saturating_add(d), start.naive.saturating_add(d)),
                (None, None) => (lo, start.naive),
            },
        };
        Some(Exact {
            start: lo,
            end: end_utc.max(lo),
            local_start: start.naive,
            local_end: local_end.max(start.naive),
            is_date: start.is_date,
        })
    }

    fn max_duration(&self) -> i64 {
        self.nominal.unwrap_or(0).max(self.exact.unwrap_or(0))
    }

    fn instance(&self, zones: &Zones<'_>, start: Value, end: Option<PeriodEnd>) -> Span {
        let (s_lo, s_hi) = zones.bounds(start.naive, start.zone);
        let mut hi = s_hi;
        match end {
            Some(PeriodEnd::At(end)) => hi = hi.max(zones.bounds(end.naive, end.zone).1),
            Some(PeriodEnd::After(d)) => {
                hi = hi
                    .max(zones.bounds(start.naive.saturating_add(d), start.zone).1)
                    .max(s_hi.saturating_add(d));
            }
            None => {
                if let Some(d) = self.nominal {
                    hi = hi.max(zones.bounds(start.naive.saturating_add(d), start.zone).1);
                }
                if let Some(d) = self.exact {
                    hi = hi.max(s_hi.saturating_add(d));
                }
            }
        }
        // Zero-length instances match at their start (RFC 4791 §9.9).
        let hi = hi.max(s_hi.saturating_add(1));
        if self.todo {
            // VTODO conditions compare inclusively at both ends.
            Span {
                lo: s_lo.saturating_sub(1),
                hi: hi.saturating_add(1),
            }
        } else {
            Span { lo: s_lo, hi }
        }
    }

    /// Inclusive UNTIL bound in local time, and in UTC if UNTIL is UTC.
    fn until(&self, zones: &Zones<'_>, rule: &Rule) -> (Option<i64>, Option<i64>) {
        let Some(until) = rule.until else {
            return (None, None);
        };
        let start = self.start;
        if until.is_date {
            let local = if start.is_date {
                until.seconds
            } else {
                until.seconds + DAY - 1 + SLACK
            };
            return (Some(local), None);
        }
        if until.is_utc {
            let local = match start.zone {
                Zone::Utc => Some(until.seconds),
                Zone::Tz(index) => zones
                    .tz(index)
                    .and_then(|tz| tz.to_local(until.seconds).map(|l| l + tz.spread())),
                Zone::Floating | Zone::Unresolved => None,
            };
            return (
                Some(local.unwrap_or(until.seconds + MAX_OFFSET)),
                Some(until.seconds),
            );
        }
        match start.zone {
            Zone::Utc => (Some(until.seconds + SLACK), None),
            _ => (Some(until.seconds), None),
        }
    }

    /// Visits the instances; returns `true` as soon as `visit` does.
    fn walk(
        &self,
        zones: &Zones<'_>,
        budget: &mut Budget,
        mode: Mode,
        overridden: &HashSet<Key>,
        shifts: &[Shift],
        visit: &mut dyn FnMut(Span, Option<Exact>) -> bool,
    ) -> Result<bool, Inexact> {
        // Each instance costs its time zone conversions plus one step per
        // THISANDFUTURE shift.
        let cost = INSTANCE_COST + u64::try_from(shifts.len()).unwrap_or(u64::MAX);
        let emit = |span: Span,
                    exact: Option<Exact>,
                    key: Option<Key>,
                    budget: &mut Budget,
                    visit: &mut dyn FnMut(Span, Option<Exact>) -> bool|
         -> Result<bool, Inexact> {
            budget.charge(cost)?;
            if key.is_some_and(|k| self.exdates.contains(&k) || overridden.contains(&k)) {
                return Ok(false);
            }
            if visit(span, exact) {
                return Ok(true);
            }
            // Shifted instances only have bounds, never exact times.
            Ok(shifts.iter().any(|shift| {
                span.hi >= shift.rid_lo
                    && visit(
                        Span {
                            lo: span.lo.saturating_add(shift.delta_lo),
                            hi: span
                                .hi
                                .saturating_add(shift.delta_hi)
                                .saturating_add(shift.len_hi),
                        },
                        None,
                    )
            }))
        };
        let exact = |value: Value, end: Option<PeriodEnd>| {
            if zones.want_exact {
                self.exact(zones, value, end)
            } else {
                None
            }
        };
        // How far THISANDFUTURE shifts can move an instance: the window for
        // skipping ahead and stopping must be widened by as much.
        let reach = shifts
            .iter()
            .map(|s| {
                s.delta_lo
                    .unsigned_abs()
                    .max(s.delta_hi.saturating_add(s.len_hi).unsigned_abs())
            })
            .max()
            .map_or(0, |r| i64::try_from(r).unwrap_or(i64::MAX));
        match &self.rule {
            None => {
                if emit(
                    self.instance(zones, self.start, None),
                    exact(self.start, None),
                    zones.key(self.start),
                    budget,
                    visit,
                )? {
                    return Ok(true);
                }
            }
            Some(rule)
                if mode == Mode::Span
                    && rule.supported
                    && rule.count.is_none()
                    && rule.until.is_none() =>
            {
                // Open-ended: the span starts at DTSTART and never ends.
                // A shift applies to every instance that reaches the RID,
                // which may start anywhere from DTSTART on (long instances
                // span the RID), so it can move the start as far as
                // DTSTART + delta.
                let first = self.instance(zones, self.start, None);
                let lo = shifts
                    .iter()
                    .map(|s| first.lo.saturating_add(s.delta_lo.min(0)))
                    .fold(first.lo, i64::min);
                if visit(Span { lo, hi: i64::MAX }, None) {
                    return Ok(true);
                }
            }
            Some(rule) => {
                let (until_local, until_utc) = self.until(zones, rule);
                let mut expansion =
                    Expansion::new(rule, self.start.naive, self.start.is_date, until_local)
                        .ok_or(Inexact)?;
                let (window_start, window_end) = match mode {
                    Mode::Span => (None, None),
                    Mode::Overlap(start, end) => (start, end),
                };
                if let Some(start) = window_start {
                    expansion.skip_to(
                        start
                            .saturating_sub(self.max_duration())
                            .saturating_sub(MARGIN)
                            .saturating_sub(reach),
                    );
                }
                let mut visited = 0usize;
                while let Some(naive) = expansion.next(budget)? {
                    visited += 1;
                    if visited > MAX_INSTANCES {
                        return Err(Inexact);
                    }
                    if window_end
                        .is_some_and(|end| naive > end.saturating_add(MARGIN).saturating_add(reach))
                    {
                        break;
                    }
                    let value = Value {
                        naive,
                        ..self.start
                    };
                    budget.charge(INSTANCE_COST)?;
                    let span = self.instance(zones, value, None);
                    // UNTIL is inclusive: instances certainly after it are dropped.
                    if until_utc.is_some_and(|until| span.lo > until.saturating_add(1)) {
                        continue;
                    }
                    if emit(span, exact(value, None), zones.key(value), budget, visit)? {
                        return Ok(true);
                    }
                }
            }
        }
        for &(value, end) in &self.rdates {
            if emit(
                self.instance(zones, value, end),
                exact(value, end),
                zones.key(value),
                budget,
                visit,
            )? {
                return Ok(true);
            }
        }
        Ok(false)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    /// All instances (the hull of open-ended rules without expanding them).
    Span,
    /// Instances near `[start, end)`.
    Overlap(Option<i64>, Option<i64>),
}

fn is_instance_component(component: &Component) -> bool {
    ComponentKind::from_name(&component.name).is_some()
}

/// Visits every instance of the object with the component it comes from;
/// `true` as soon as `visit` does.
fn walk(
    root: &Component,
    budget: &mut Budget,
    mode: Mode,
    floating_utc: bool,
    visit: &mut dyn FnMut(&Component, Span, Option<Exact>) -> bool,
) -> Result<bool, Inexact> {
    let mut zones = Zones::new(root);
    zones.floating_utc = floating_utc;
    walk_in(&mut zones, budget, mode, visit)
}

fn walk_in<'a>(
    zones: &mut Zones<'a>,
    budget: &mut Budget,
    mode: Mode,
    visit: &mut dyn FnMut(&'a Component, Span, Option<Exact>) -> bool,
) -> Result<bool, Inexact> {
    let root = zones.root;
    let components: Vec<&Component> = root
        .components
        .iter()
        .filter(|c| is_instance_component(c))
        .collect();

    // Overrides: which master instances they replace.
    let mut overridden = HashSet::new();
    let mut shifts = Vec::new();
    let mut series = Vec::new();
    for &component in &components {
        budget.charge(1)?;
        let built = Series::build(zones, component, budget)?;
        let Some(rid_property) = component.property("RECURRENCE-ID") else {
            series.push((component, built, false));
            continue;
        };
        let rid = zones
            .value(rid_property, &rid_property.value, budget)?
            .ok_or(Inexact)?;
        if let Some(key) = zones.key(rid) {
            overridden.insert(key);
        }
        let this_and_future = rid_property.parameter("RANGE").is_some_and(|p| {
            p.values
                .iter()
                .any(|v| v.eq_ignore_ascii_case("THISANDFUTURE"))
        });
        if this_and_future {
            let own = built.as_ref().ok_or(Inexact)?;
            let first = own.instance(zones, own.start, None);
            let (rid_lo, rid_hi) = zones.bounds(rid.naive, rid.zone);
            shifts.push(Shift {
                rid_lo,
                delta_lo: first.lo.saturating_sub(rid_hi),
                delta_hi: first
                    .lo
                    .saturating_sub(rid_lo)
                    .saturating_add(2 * SLACK + 2),
                len_hi: first.hi.saturating_sub(first.lo),
            });
        }
        series.push((component, built, true));
    }

    let none = HashSet::new();
    for (component, built, is_override) in &series {
        match built {
            Some(s) => {
                let (overridden, shifts) = if *is_override {
                    (&none, &[][..])
                } else {
                    (&overridden, &shifts[..])
                };
                if s.walk(
                    zones,
                    budget,
                    mode,
                    overridden,
                    shifts,
                    &mut |span, exact| visit(component, span, exact),
                )? {
                    return Ok(true);
                }
            }
            None => {
                // No dates: matches every time range.
                if component.property("RRULE").is_some() || component.property("RDATE").is_some() {
                    return Err(Inexact);
                }
                if visit(
                    component,
                    Span {
                        lo: i64::MIN,
                        hi: i64::MAX,
                    },
                    None,
                ) {
                    return Ok(true);
                }
            }
        }
    }
    Ok(false)
}

/// Hull of all instances: exact first start and last end where they can
/// be computed within the budget; open-ended rules stay open.
pub(crate) fn span(root: &Component, budget: &mut Budget) -> Result<TimeRange, Inexact> {
    let mut lo = i64::MAX;
    let mut hi = i64::MIN;
    let mut any = false;
    walk(root, budget, Mode::Span, false, &mut |_, span, _| {
        any = true;
        lo = lo.min(span.lo);
        hi = hi.max(span.hi);
        false
    })?;
    if !any {
        return Ok(TimeRange::UNBOUNDED);
    }
    Ok(TimeRange {
        start: (lo != i64::MIN).then_some(lo),
        end: (hi != i64::MAX).then_some(hi),
    })
}

/// Whether any instance overlaps `[start, end)`.
pub(crate) fn overlaps(
    root: &Component,
    start: Option<i64>,
    end: Option<i64>,
    budget: &mut Budget,
) -> Result<bool, Inexact> {
    walk(
        root,
        budget,
        Mode::Overlap(start, end),
        false,
        &mut |_, span, _| span.overlaps(start, end),
    )
}

/// Instances of VEVENTs overlapping `[start, end)` for free/busy answers,
/// clipped to the range, with the component (master or override) each
/// comes from. Floating times and dates are read as UTC; times with a TZID
/// that cannot be resolved stay widened (conservatively busy).
pub(crate) fn busy_instances(
    root: &Component,
    start: i64,
    end: i64,
    budget: &mut Budget,
    visit: &mut dyn FnMut(&Component, i64, i64),
) -> Result<(), Inexact> {
    walk(
        root,
        budget,
        Mode::Overlap(Some(start), Some(end)),
        true,
        &mut |component, span, _| {
            if component.name == "VEVENT"
                && span.lo != i64::MIN
                && span.overlaps(Some(start), Some(end))
            {
                visit(component, span.lo.max(start), span.hi.min(end));
            }
            false
        },
    )?;
    Ok(())
}

/// How floating times and dates are read for alarms.
#[derive(Debug, Clone, Copy)]
pub(crate) enum Floating<'a> {
    /// In this VTIMEZONE (the calendar's time zone).
    Zone(&'a Component),
    /// As UTC.
    Utc,
}

fn alarm_zones<'a>(
    root: &'a Component,
    floating: Floating<'_>,
    budget: &mut Budget,
) -> Result<Zones<'a>, Inexact> {
    let mut zones = Zones::new(root);
    zones.want_exact = true;
    zones.floating_utc = true;
    if let Floating::Zone(vtimezone) = floating {
        zones.floating_tz = Tz::build(vtimezone, budget)?;
    }
    Ok(zones)
}

/// Instances near `[start, end)` with their exact times (`None` where they
/// cannot be computed exactly), for alarms. The window works like a
/// time-range query: every instance overlapping it is visited, and
/// possibly some more around it.
pub(crate) fn alarm_instances(
    root: &Component,
    start: i64,
    end: i64,
    floating: Floating<'_>,
    budget: &mut Budget,
    visit: &mut dyn FnMut(&Component, Option<Exact>),
) -> Result<(), Inexact> {
    let mut zones = alarm_zones(root, floating, budget)?;
    walk_in(
        &mut zones,
        budget,
        Mode::Overlap(Some(start), Some(end)),
        &mut |component, span, exact| {
            if span.overlaps(Some(start), Some(end)) {
                visit(component, exact);
            }
            false
        },
    )?;
    Ok(())
}

/// Exact times of the first instance of `component` (its DTSTART), for
/// alarms with an absolute trigger.
pub(crate) fn first_instance(
    root: &Component,
    component: &Component,
    floating: Floating<'_>,
    budget: &mut Budget,
) -> Result<Option<Exact>, Inexact> {
    let mut zones = alarm_zones(root, floating, budget)?;
    let Some(series) = Series::build(&mut zones, component, budget)? else {
        return Ok(None);
    };
    Ok(series.exact(&zones, series.start, None))
}

/// Identity of a RECURRENCE-ID or EXDATE value, comparable between two
/// objects (e.g. an organizer's and an attendee's copy of one event).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) enum InstantKey {
    /// An exact instant.
    Utc(i64),
    /// A local time: floating, a date, or a TZID that cannot be resolved.
    Local {
        naive: i64,
        is_date: bool,
        tzid: Option<String>,
    },
}

/// Computes [`InstantKey`]s of values in one object, caching its time zones.
pub(crate) struct InstantKeys<'a> {
    zones: Zones<'a>,
    budget: Budget,
}

impl<'a> InstantKeys<'a> {
    pub(crate) fn new(root: &'a Component) -> Self {
        Self {
            zones: Zones::new(root),
            budget: Budget::new(OBJECT_BUDGET),
        }
    }

    /// The key of `raw` (one value of `property`), `None` if it cannot be
    /// read.
    pub(crate) fn key(&mut self, property: &'a Property, raw: &str) -> Option<InstantKey> {
        let value = self.zones.value(property, raw, &mut self.budget).ok()??;
        instant_key(&self.zones, property, value)
    }
}

fn instant_key(zones: &Zones<'_>, property: &Property, value: Value) -> Option<InstantKey> {
    match zones.key(value)? {
        Key { zone: 0, time, .. } => Some(InstantKey::Utc(time)),
        key => Some(InstantKey::Local {
            naive: value.naive,
            is_date: key.is_date,
            tzid: property
                .parameter("TZID")
                .and_then(|p| p.values.first())
                .filter(|_| !value.is_date)
                .cloned(),
        }),
    }
}

/// Whether the master component (without RECURRENCE-ID) of `root` has an
/// instance at `target` that is not excluded. Returns the instance's start
/// formatted like the master's DTSTART (for a new override's
/// RECURRENCE-ID), `None` if there is no such instance.
pub(crate) fn master_instance(
    root: &Component,
    target: &InstantKey,
    budget: &mut Budget,
) -> Result<Option<String>, Inexact> {
    let mut zones = Zones::new(root);
    let Some(master) = root
        .components
        .iter()
        .find(|c| is_instance_component(c) && c.property("RECURRENCE-ID").is_none())
    else {
        return Ok(None);
    };
    let Some(start_property) = master.property("DTSTART") else {
        return Ok(None);
    };
    let Some(series) = Series::build(&mut zones, master, budget)? else {
        return Ok(None);
    };
    let start = series.start;
    let found = |zones: &Zones<'_>, value: Value| -> bool {
        let same_zone = value.zone == start.zone && value.is_date == start.is_date;
        same_zone
            && instant_key(zones, start_property, value).as_ref() == Some(target)
            && !zones
                .key(value)
                .is_some_and(|k| series.exdates.contains(&k))
    };
    let format = |value: Value| {
        crate::datetime::format_date_or_datetime(
            value.naive,
            value.is_date,
            value.zone == Zone::Utc,
        )
    };
    budget.charge(INSTANCE_COST)?;
    if found(&zones, start) {
        return Ok(format(start));
    }
    for &(value, _) in &series.rdates {
        budget.charge(INSTANCE_COST)?;
        if found(&zones, value) {
            return Ok(format(value));
        }
    }
    let Some(rule) = &series.rule else {
        return Ok(None);
    };
    let time = match target {
        InstantKey::Utc(time) => *time,
        InstantKey::Local { naive, .. } => *naive,
    };
    let (until_local, _) = series.until(&zones, rule);
    let mut expansion =
        Expansion::new(rule, start.naive, start.is_date, until_local).ok_or(Inexact)?;
    expansion.skip_to(time.saturating_sub(MARGIN));
    let mut visited = 0usize;
    while let Some(naive) = expansion.next(budget)? {
        visited += 1;
        if visited > MAX_INSTANCES || naive > time.saturating_add(MARGIN) {
            break;
        }
        budget.charge(INSTANCE_COST)?;
        let value = Value { naive, ..start };
        if found(&zones, value) {
            return Ok(format(value));
        }
    }
    Ok(None)
}

/// Whether a stored calendar object has an instance overlapping the time
/// range `[start, end)` (UTC seconds; `None` = unbounded).
///
/// Errs on the side of matching: unparsable data, unsupported rules and an
/// exhausted `budget` all return `true`, so a query never misses an object
/// that might match.
pub fn matches_time_range(
    data: &[u8],
    start: Option<i64>,
    end: Option<i64>,
    budget: &mut Budget,
) -> bool {
    exact_time_range_match(data, start, end, budget).unwrap_or(true)
}

/// Like [`matches_time_range`], but `None` when the answer could not be
/// computed exactly.
pub fn exact_time_range_match(
    data: &[u8],
    start: Option<i64>,
    end: Option<i64>,
    budget: &mut Budget,
) -> Option<bool> {
    let root = crate::parse(data).ok()?;
    overlaps(&root, start, end, budget).ok()
}

#[cfg(test)]
#[path = "recurrence_tests.rs"]
mod tests;
