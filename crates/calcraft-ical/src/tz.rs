//! Time zones from the VTIMEZONE components embedded in a calendar object
//! (RFC 5545 §3.6.5), without a timezone database.
//!
//! The STANDARD and DAYLIGHT observances are expanded (DTSTART, RRULE,
//! RDATE) into a table of UTC transitions up to [`HORIZON_YEAR`]. Local
//! times are converted with the RFC 5545 rules: a local time that does not
//! exist (DST gap) is shifted forward by the length of the gap, an
//! ambiguous one (DST overlap) means its first occurrence. Outside the
//! table, and for definitions this module cannot evaluate, the zone is
//! unknown and callers fall back to conservative widening.

use crate::{
    Component,
    datetime::{days_from_civil, parse_date_or_datetime, parse_utc_offset},
    recurrence::{Budget, Inexact},
    rrule::{Expansion, parse_rule},
};

/// Transitions are computed up to the start of this year.
pub(crate) const HORIZON_YEAR: i64 = 2200;
/// Larger UTC offsets are not plausible; such zones are not evaluated.
pub(crate) const MAX_OFFSET: i64 = 18 * 3600;
const MAX_TRANSITIONS: usize = 5_000;
const MAX_OBSERVANCES: usize = 100;
/// Bounds the work of one conversion: at most this many distinct offsets
/// are tried, and transitions closer than [`MIN_TRANSITION_GAP`] (which no
/// real zone has) make the zone unusable, so a gap search looks at a
/// handful of transitions only.
const MAX_OFFSETS: usize = 16;
const MIN_TRANSITION_GAP: i64 = 6 * 3600;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Transition {
    /// UTC instant of the onset.
    at: i64,
    from: i64,
    to: i64,
}

/// A zone evaluated from a VTIMEZONE.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Tz {
    transitions: Vec<Transition>,
    /// Offsets are known for UTC instants before this.
    covered_until: i64,
    /// Distinct offsets of the zone.
    offsets: Vec<i64>,
}

fn offset(observance: &Component, name: &str) -> Option<i64> {
    let value = parse_utc_offset(&observance.property(name)?.value)?;
    (value.abs() <= MAX_OFFSET).then_some(value)
}

impl Tz {
    /// Evaluates a VTIMEZONE. `Ok(None)` if it cannot be evaluated exactly.
    pub(crate) fn build(
        vtimezone: &Component,
        budget: &mut Budget,
    ) -> Result<Option<Self>, Inexact> {
        let horizon = days_from_civil(HORIZON_YEAR, 1, 1) * 86_400;
        let mut transitions = Vec::new();
        let mut covered_until = i64::MAX;
        let observances = vtimezone
            .components
            .iter()
            .filter(|c| c.name == "STANDARD" || c.name == "DAYLIGHT");
        for (index, observance) in observances.enumerate() {
            if index >= MAX_OBSERVANCES {
                return Ok(None);
            }
            budget.charge(1)?;
            let start = match observance
                .property("DTSTART")
                .map(|p| parse_date_or_datetime(&p.value))
            {
                Some(Ok(start)) if !start.is_date && !start.is_utc => start,
                _ => return Ok(None),
            };
            let (Some(from), Some(to)) = (
                offset(observance, "TZOFFSETFROM"),
                offset(observance, "TZOFFSETTO"),
            ) else {
                return Ok(None);
            };
            // Onsets are local times in the offset in effect before them.
            transitions.push(Transition {
                at: start.seconds - from,
                from,
                to,
            });
            let mut rules = observance.properties_named("RRULE");
            if let Some(property) = rules.next() {
                if rules.next().is_some() {
                    return Ok(None);
                }
                let Ok(rule) = parse_rule(&property.value) else {
                    return Ok(None);
                };
                let until = rule.until.map(|u| {
                    if u.is_utc {
                        u.seconds + from
                    } else {
                        u.seconds
                    }
                });
                let Some(mut expansion) = Expansion::new(&rule, start.seconds, false, until) else {
                    return Ok(None);
                };
                loop {
                    match expansion.next(budget)? {
                        Some(local) if local >= horizon => {
                            covered_until = covered_until.min(horizon - MAX_OFFSET);
                            break;
                        }
                        Some(local) => {
                            if local != start.seconds {
                                transitions.push(Transition {
                                    at: local - from,
                                    from,
                                    to,
                                });
                            }
                            if transitions.len() > MAX_TRANSITIONS {
                                return Ok(None);
                            }
                        }
                        None => break,
                    }
                }
            }
            for property in observance.properties_named("RDATE") {
                for value in property.value.split(',') {
                    budget.charge(1)?;
                    match parse_date_or_datetime(value) {
                        Ok(onset) if !onset.is_date => transitions.push(Transition {
                            at: if onset.is_utc {
                                onset.seconds
                            } else {
                                onset.seconds - from
                            },
                            from,
                            to,
                        }),
                        _ => return Ok(None),
                    }
                    if transitions.len() > MAX_TRANSITIONS {
                        return Ok(None);
                    }
                }
            }
        }
        if transitions.is_empty() {
            return Ok(None);
        }
        transitions.sort_by_key(|t| t.at);
        transitions.dedup_by_key(|t| t.at);
        budget.charge(u64::try_from(transitions.len()).unwrap_or(u64::MAX))?;
        if transitions
            .windows(2)
            .any(|pair| pair[1].at - pair[0].at < MIN_TRANSITION_GAP)
        {
            return Ok(None);
        }
        let mut offsets: Vec<i64> = transitions.iter().flat_map(|t| [t.from, t.to]).collect();
        offsets.sort_unstable();
        offsets.dedup();
        if offsets.len() > MAX_OFFSETS {
            return Ok(None);
        }
        Ok(Some(Self {
            transitions,
            covered_until,
            offsets,
        }))
    }

    /// Offset in effect at a UTC instant; `None` before the first onset
    /// (undefined by RFC 5545) or beyond the computed transitions.
    pub(crate) fn offset_at(&self, utc: i64) -> Option<i64> {
        if utc >= self.covered_until {
            return None;
        }
        let index = self.transitions.partition_point(|t| t.at <= utc);
        index
            .checked_sub(1)
            .and_then(|i| self.transitions.get(i))
            .map(|t| t.to)
    }

    /// Converts a naive local time to UTC.
    pub(crate) fn to_utc(&self, local: i64) -> Option<i64> {
        // Ambiguous times: the first occurrence is the earliest instant.
        let valid = self
            .offsets
            .iter()
            .map(|&o| local - o)
            .filter(|&utc| self.offset_at(utc) == Some(local - utc))
            .min();
        if valid.is_some() {
            return valid;
        }
        // A gap: shift forward by its length, i.e. use the offset before it.
        let first = self
            .transitions
            .partition_point(|t| t.at < local - 2 * MAX_OFFSET);
        self.transitions
            .get(first..)?
            .iter()
            .take_while(|t| t.at <= local + 2 * MAX_OFFSET)
            .find(|t| t.to > t.from && local >= t.at + t.from && local < t.at + t.to)
            .filter(|t| t.at < self.covered_until)
            .map(|t| local - t.from)
    }

    /// Converts a UTC instant to naive local time.
    pub(crate) fn to_local(&self, utc: i64) -> Option<i64> {
        self.offset_at(utc).map(|o| utc + o)
    }

    /// Difference between the largest and smallest offset.
    pub(crate) fn spread(&self) -> i64 {
        match (self.offsets.first(), self.offsets.last()) {
            (Some(min), Some(max)) => max - min,
            _ => 0,
        }
    }
}
