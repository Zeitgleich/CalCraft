//! Recurrence rules (RFC 5545 §3.3.10): parsing and bounded expansion.
//!
//! One parser serves validation at PUT and expansion, so the two can never
//! disagree about what a rule means (the Open WebUI pattern, see
//! `docs/security/angriffshistorie.md`). Expansion works on local "naive"
//! times (seconds since 1970 read as if UTC); callers convert them to UTC.
//! Every loop iteration and every generated candidate is charged to a
//! [`Budget`]; running out is reported as [`Inexact`], never hidden.

use crate::{
    IcalError,
    datetime::{
        DateTime, MAX_TIME, civil_from_days, days_from_civil, days_in_month, is_leap,
        parse_date_or_datetime, weekday,
    },
    recurrence::{Budget, Inexact},
};

/// More parts than any meaningful rule has (RFC 5545 defines 14).
const MAX_PARTS: usize = 32;
const KNOWN_PARTS: [&str; 14] = [
    "FREQ",
    "UNTIL",
    "COUNT",
    "INTERVAL",
    "BYSECOND",
    "BYMINUTE",
    "BYHOUR",
    "BYDAY",
    "BYMONTHDAY",
    "BYYEARDAY",
    "BYWEEKNO",
    "BYMONTH",
    "BYSETPOS",
    "WKST",
];
/// Entries per BYxxx list; the longest meaningful list (BYYEARDAY) has 732.
const MAX_LIST: usize = 1_000;
const DAY: i64 = 86_400;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Freq {
    Secondly,
    Minutely,
    Hourly,
    Daily,
    Weekly,
    Monthly,
    Yearly,
}

/// A parsed RRULE. Lists are sorted and free of duplicates.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Rule {
    pub freq: Freq,
    pub interval: i64,
    pub count: Option<u32>,
    pub until: Option<DateTime>,
    /// `false` if the rule uses parts this expander does not evaluate
    /// (RSCALE, SKIP, x-parts, combinations RFC 5545 forbids). Such a rule
    /// is accepted but only used for conservative bounds.
    pub supported: bool,
    by_second: Vec<i64>,
    by_minute: Vec<i64>,
    by_hour: Vec<i64>,
    /// `(ordinal, weekday)`; ordinal 0 means every such weekday.
    by_day: Vec<(i64, i64)>,
    by_month_day: Vec<i64>,
    by_year_day: Vec<i64>,
    by_week_no: Vec<i64>,
    by_month: Vec<i64>,
    by_set_pos: Vec<i64>,
    wkst: i64,
}

fn invalid() -> IcalError {
    IcalError::Invalid("invalid RRULE")
}

fn positive(value: &str) -> Result<u32, IcalError> {
    match value.parse::<u32>() {
        Ok(n) if n > 0 => Ok(n),
        _ => Err(invalid()),
    }
}

fn weekday_code(code: &str) -> Option<i64> {
    Some(match code {
        "MO" => 0,
        "TU" => 1,
        "WE" => 2,
        "TH" => 3,
        "FR" => 4,
        "SA" => 5,
        "SU" => 6,
        _ => return None,
    })
}

/// Splits an optional sign off a list item; `None` if a sign is not allowed.
fn signed(item: &str, allow_sign: bool) -> Option<(i64, &str)> {
    match item.as_bytes().first() {
        Some(b'-') if allow_sign => Some((-1, &item[1..])),
        Some(b'+') if allow_sign => Some((1, &item[1..])),
        Some(b'-' | b'+') => None,
        _ => Some((1, item)),
    }
}

fn small_number(digits: &str) -> Option<i64> {
    if digits.is_empty() || digits.len() > 3 || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    digits.parse().ok()
}

/// A comma-separated number list with values in `min..=max`, negated
/// values allowed if `allow_sign`.
fn numbers(value: &str, min: i64, max: i64, allow_sign: bool) -> Result<Vec<i64>, IcalError> {
    let mut out = Vec::new();
    for item in value.split(',') {
        if out.len() >= MAX_LIST {
            return Err(IcalError::Limit("RRULE list too long"));
        }
        let (sign, digits) = signed(item, allow_sign).ok_or_else(invalid)?;
        let n = small_number(digits).ok_or_else(invalid)?;
        if n < min || n > max {
            return Err(invalid());
        }
        out.push(sign * n);
    }
    out.sort_unstable();
    out.dedup();
    Ok(out)
}

fn weekdays(value: &str) -> Result<Vec<(i64, i64)>, IcalError> {
    let mut out = Vec::new();
    for item in value.split(',') {
        if out.len() >= MAX_LIST {
            return Err(IcalError::Limit("RRULE list too long"));
        }
        if item.len() < 2 || !item.is_ascii() {
            return Err(invalid());
        }
        let (prefix, code) = item.split_at(item.len() - 2);
        let day = weekday_code(code).ok_or_else(invalid)?;
        let ordinal = if prefix.is_empty() {
            0
        } else {
            let (sign, digits) = signed(prefix, true).ok_or_else(invalid)?;
            match small_number(digits) {
                Some(n @ 1..=53) => sign * n,
                _ => return Err(invalid()),
            }
        };
        out.push((ordinal, day));
    }
    out.sort_unstable();
    out.dedup();
    Ok(out)
}

/// Parses and validates an RRULE value. Errors only for what CalCraft has
/// always rejected (no or unknown FREQ, invalid UNTIL, COUNT with UNTIL,
/// zero or non-numeric INTERVAL/COUNT, parts without `=`); every other
/// irregularity, and rules this expander cannot evaluate, are returned with
/// `supported == false`.
pub(crate) fn parse_rule(value: &str) -> Result<Rule, IcalError> {
    let mut rule = Rule {
        freq: Freq::Daily,
        interval: 1,
        count: None,
        until: None,
        supported: true,
        by_second: Vec::new(),
        by_minute: Vec::new(),
        by_hour: Vec::new(),
        by_day: Vec::new(),
        by_month_day: Vec::new(),
        by_year_day: Vec::new(),
        by_week_no: Vec::new(),
        by_month: Vec::new(),
        by_set_pos: Vec::new(),
        wkst: 0,
    };
    let mut freq = None;
    // Bit per known part, to notice repetitions.
    let mut seen = 0u32;
    let mut has_count = false;
    let mut has_until = false;
    for (index, part) in value.split(';').enumerate() {
        if index >= MAX_PARTS {
            rule.supported = false;
        }
        let (key, value) = part.split_once('=').ok_or_else(invalid)?;
        let key = key.to_ascii_uppercase();
        let upper = value.to_ascii_uppercase();
        // Only what earlier versions rejected is an error, so objects from
        // other servers keep syncing; anything else odd only makes the rule
        // unsupported (indexed and matched conservatively).
        let parsed: Result<(), IcalError> = match key.as_str() {
            "FREQ" => {
                freq = Some(match upper.as_str() {
                    "SECONDLY" => Freq::Secondly,
                    "MINUTELY" => Freq::Minutely,
                    "HOURLY" => Freq::Hourly,
                    "DAILY" => Freq::Daily,
                    "WEEKLY" => Freq::Weekly,
                    "MONTHLY" => Freq::Monthly,
                    "YEARLY" => Freq::Yearly,
                    _ => return Err(invalid()),
                });
                Ok(())
            }
            "INTERVAL" => {
                rule.interval = i64::from(positive(value)?);
                Ok(())
            }
            "COUNT" => {
                has_count = true;
                rule.count = Some(positive(value)?);
                Ok(())
            }
            "UNTIL" => {
                has_until = true;
                rule.until = Some(parse_date_or_datetime(value)?);
                Ok(())
            }
            "BYSECOND" => numbers(value, 0, 60, false).map(|v| rule.by_second = v),
            "BYMINUTE" => numbers(value, 0, 59, false).map(|v| rule.by_minute = v),
            "BYHOUR" => numbers(value, 0, 23, false).map(|v| rule.by_hour = v),
            "BYDAY" => weekdays(&upper).map(|v| rule.by_day = v),
            "BYMONTHDAY" => numbers(value, 1, 31, true).map(|v| rule.by_month_day = v),
            "BYYEARDAY" => numbers(value, 1, 366, true).map(|v| rule.by_year_day = v),
            "BYWEEKNO" => numbers(value, 1, 53, true).map(|v| rule.by_week_no = v),
            "BYMONTH" => numbers(value, 1, 12, false).map(|v| rule.by_month = v),
            "BYSETPOS" => numbers(value, 1, 366, true).map(|v| rule.by_set_pos = v),
            "WKST" => weekday_code(&upper)
                .map(|d| rule.wkst = d)
                .ok_or_else(invalid),
            // RSCALE/SKIP (RFC 7529) and x-parts change the meaning of a rule.
            _ => Err(invalid()),
        };
        if parsed.is_err() {
            rule.supported = false;
        }
        if let Some(bit) = KNOWN_PARTS.iter().position(|k| *k == key) {
            if seen & (1 << bit) != 0 {
                rule.supported = false;
            }
            seen |= 1 << bit;
        }
    }
    rule.freq = freq.ok_or_else(invalid)?;
    if has_count && has_until {
        return Err(invalid());
    }
    // Combinations RFC 5545 forbids ("MUST NOT"): accepted for
    // compatibility, but not expanded.
    let ordinals = rule.by_day.iter().any(|&(n, _)| n != 0);
    let freq = rule.freq;
    if (!rule.by_week_no.is_empty() && freq != Freq::Yearly)
        || (!rule.by_year_day.is_empty()
            && matches!(freq, Freq::Daily | Freq::Weekly | Freq::Monthly))
        || (!rule.by_month_day.is_empty() && freq == Freq::Weekly)
        || (ordinals && !matches!(freq, Freq::Monthly | Freq::Yearly))
        || (ordinals && !rule.by_week_no.is_empty())
    {
        rule.supported = false;
    }
    Ok(rule)
}

/// Lazily generates the start times of a rule in increasing order,
/// beginning with DTSTART itself (which RFC 5545 always counts as an
/// instance).
pub(crate) struct Expansion<'r> {
    rule: &'r Rule,
    start: i64,
    is_date: bool,
    months: Vec<i64>,
    month_days: Vec<i64>,
    days: Vec<(i64, i64)>,
    hours: Vec<i64>,
    minutes: Vec<i64>,
    seconds: Vec<i64>,
    /// Period of index 0, in the unit of the frequency.
    base: i64,
    /// Next period to generate.
    k: i64,
    until: Option<i64>,
    remaining: Option<u32>,
    buf: Vec<i64>,
    pos: usize,
    pending_start: bool,
    done: bool,
}

impl<'r> Expansion<'r> {
    /// `start` is DTSTART as a naive local time, `until` the inclusive
    /// upper bound in the same terms. `None` if the rule cannot be
    /// expanded exactly.
    pub(crate) fn new(
        rule: &'r Rule,
        start: i64,
        is_date: bool,
        until: Option<i64>,
    ) -> Option<Self> {
        if !rule.supported {
            return None;
        }
        if is_date
            && (rule.freq < Freq::Daily
                || !rule.by_hour.is_empty()
                || !rule.by_minute.is_empty()
                || !rule.by_second.is_empty())
        {
            return None;
        }
        let day = start.div_euclid(DAY);
        let (year, month, month_day) = civil_from_days(day);
        let time = start.rem_euclid(DAY);
        let (hour, minute, second) = (time / 3600, time % 3600 / 60, time % 60);

        let mut months = rule.by_month.clone();
        let mut month_days = rule.by_month_day.clone();
        let mut days = rule.by_day.clone();
        if rule.by_week_no.is_empty()
            && rule.by_year_day.is_empty()
            && month_days.is_empty()
            && days.is_empty()
        {
            match rule.freq {
                Freq::Yearly => {
                    if months.is_empty() {
                        months = vec![month];
                    }
                    month_days = vec![month_day];
                }
                Freq::Monthly => month_days = vec![month_day],
                Freq::Weekly => days = vec![(0, weekday(day))],
                _ => {}
            }
        }
        let or_default = |list: &Vec<i64>, value: i64, applies: bool| {
            if list.is_empty() && applies {
                vec![value]
            } else {
                list.clone()
            }
        };
        let hours = or_default(&rule.by_hour, hour, rule.freq >= Freq::Daily);
        let minutes = or_default(&rule.by_minute, minute, rule.freq >= Freq::Hourly);
        let seconds = or_default(&rule.by_second, second, rule.freq >= Freq::Minutely);

        let base = match rule.freq {
            Freq::Yearly => year,
            Freq::Monthly => year * 12 + month - 1,
            Freq::Weekly => day - (weekday(day) - rule.wkst).rem_euclid(7),
            Freq::Daily => day,
            Freq::Hourly => start - start.rem_euclid(3600),
            Freq::Minutely => start - start.rem_euclid(60),
            Freq::Secondly => start,
        };
        Some(Self {
            rule,
            start,
            is_date,
            months,
            month_days,
            days,
            hours,
            minutes,
            seconds,
            base,
            k: 0,
            until,
            remaining: rule.count,
            buf: Vec::new(),
            pos: 0,
            pending_start: true,
            done: false,
        })
    }

    /// Length of `interval` periods in the base unit (days for weekly and
    /// daily, seconds below that).
    fn step(&self) -> i64 {
        let unit = match self.rule.freq {
            Freq::Weekly => 7,
            Freq::Hourly => 3600,
            Freq::Minutely => 60,
            _ => 1,
        };
        unit * self.rule.interval
    }

    /// First instant of period `k`; `None` beyond year 9999.
    fn period_start(&self, k: i64) -> Option<i64> {
        let offset = k.checked_mul(self.step())?;
        let unit = self.base.checked_add(offset)?;
        let start = match self.rule.freq {
            Freq::Yearly => {
                if unit > 9999 {
                    return None;
                }
                days_from_civil(unit, 1, 1) * DAY
            }
            Freq::Monthly => {
                let year = unit.div_euclid(12);
                if year > 9999 {
                    return None;
                }
                days_from_civil(year, unit.rem_euclid(12) + 1, 1) * DAY
            }
            Freq::Weekly | Freq::Daily => unit.checked_mul(DAY)?,
            _ => unit,
        };
        (start < MAX_TIME).then_some(start)
    }

    /// Index of the period that contains the naive time `t`.
    fn index_at(&self, t: i64) -> i64 {
        let unit = match self.rule.freq {
            Freq::Yearly => civil_from_days(t.div_euclid(DAY)).0,
            Freq::Monthly => {
                let (year, month, _) = civil_from_days(t.div_euclid(DAY));
                year * 12 + month - 1
            }
            Freq::Weekly | Freq::Daily => t.div_euclid(DAY),
            _ => t,
        };
        (unit - self.base).div_euclid(self.step())
    }

    /// Index of the first period that starts at or after `t`.
    fn index_from(&self, t: i64) -> i64 {
        let k = self.index_at(t);
        match self.period_start(k) {
            Some(start) if start < t => k + 1,
            _ => k,
        }
    }

    /// Jumps ahead to the period containing `t`. Only rules without COUNT
    /// can skip, because counting needs every earlier instance.
    pub(crate) fn skip_to(&mut self, t: i64) {
        if self.remaining.is_some() {
            return;
        }
        let k = self.index_at(t);
        if k > self.k {
            self.k = k;
            self.buf.clear();
            self.pos = 0;
        }
    }

    /// The next start time, or `None` when the rule is exhausted.
    pub(crate) fn next(&mut self, budget: &mut Budget) -> Result<Option<i64>, Inexact> {
        if self.pending_start {
            self.pending_start = false;
            return Ok(Some(self.start));
        }
        loop {
            if let Some(&candidate) = self.buf.get(self.pos) {
                self.pos += 1;
                if candidate < self.start {
                    continue;
                }
                if self.until.is_some_and(|until| candidate > until) {
                    self.done = true;
                    self.buf.clear();
                    return Ok(None);
                }
                if let Some(remaining) = &mut self.remaining {
                    if *remaining == 0 {
                        self.done = true;
                        return Ok(None);
                    }
                    *remaining -= 1;
                }
                // DTSTART was already returned first.
                if candidate == self.start {
                    continue;
                }
                return Ok(Some(candidate));
            }
            if self.done || self.remaining == Some(0) {
                self.done = true;
                return Ok(None);
            }
            budget.charge(1)?;
            let Some(period) = self.period_start(self.k) else {
                self.done = true;
                return Ok(None);
            };
            if self.until.is_some_and(|until| period > until) {
                self.done = true;
                return Ok(None);
            }
            self.buf.clear();
            self.pos = 0;
            let next = self.fill(period, budget)?;
            self.k = next.max(self.k + 1);
        }
    }

    /// Generates the candidates of the period starting at `period` into
    /// `buf`. Returns the index of the next period to look at.
    fn fill(&mut self, period: i64, budget: &mut Budget) -> Result<i64, Inexact> {
        let next = self.k + 1;
        let mut days: Vec<i64> = Vec::new();
        let first_day = period.div_euclid(DAY);
        match self.rule.freq {
            Freq::Yearly => {
                let (year, _, _) = civil_from_days(first_day);
                let months: Vec<i64> = if self.months.is_empty() {
                    (1..=12).collect()
                } else {
                    self.months.clone()
                };
                for month in months {
                    self.month_candidates(year, month, &mut days, budget)?;
                }
            }
            Freq::Monthly => {
                let (year, month, _) = civil_from_days(first_day);
                if self.months.is_empty() || self.months.contains(&month) {
                    self.month_candidates(year, month, &mut days, budget)?;
                }
            }
            Freq::Weekly => {
                for day in first_day..first_day + 7 {
                    budget.charge(1)?;
                    if self.day_matches(day) {
                        days.push(day);
                    }
                }
            }
            Freq::Daily => {
                if self.day_matches(first_day) {
                    days.push(first_day);
                }
            }
            Freq::Hourly | Freq::Minutely | Freq::Secondly => {
                return self.fill_sub_daily(period, budget);
            }
        }
        for day in days {
            if self.is_date {
                budget.charge(1)?;
                self.buf.push(day * DAY);
                continue;
            }
            for &hour in &self.hours {
                for &minute in &self.minutes {
                    for &second in &self.seconds {
                        budget.charge(1)?;
                        self.buf
                            .push(day * DAY + hour * 3600 + minute * 60 + second);
                    }
                }
            }
        }
        self.finish_period();
        Ok(next)
    }

    /// Hourly, minutely and secondly periods: one instant whose day and
    /// coarser fields are limits. Non-matching days, hours or minutes are
    /// skipped as a whole.
    fn fill_sub_daily(&mut self, period: i64, budget: &mut Budget) -> Result<i64, Inexact> {
        let next = self.k + 1;
        let day = period.div_euclid(DAY);
        if !self.day_matches(day) {
            return Ok(self.index_from((day + 1) * DAY).max(next));
        }
        let time = period.rem_euclid(DAY);
        let (hour, minute, second) = (time / 3600, time % 3600 / 60, time % 60);
        let hour_start = period - time % 3600;
        if !self.rule.by_hour.is_empty() && !self.rule.by_hour.contains(&hour) {
            return Ok(self.index_from(hour_start + 3600).max(next));
        }
        let freq = self.rule.freq;
        let minutes: Vec<i64> = if freq == Freq::Hourly {
            self.minutes.clone()
        } else if self.rule.by_minute.is_empty() || self.rule.by_minute.contains(&minute) {
            vec![minute]
        } else {
            return Ok(self.index_from(hour_start + (minute + 1) * 60).max(next));
        };
        let seconds: Vec<i64> = if freq == Freq::Secondly {
            if !self.rule.by_second.is_empty() && !self.rule.by_second.contains(&second) {
                return Ok(next);
            }
            vec![second]
        } else {
            self.seconds.clone()
        };
        for &minute in &minutes {
            for &second in &seconds {
                budget.charge(1)?;
                self.buf.push(hour_start + minute * 60 + second);
            }
        }
        self.finish_period();
        Ok(next)
    }

    /// Sorts the candidates of a period and applies BYSETPOS.
    fn finish_period(&mut self) {
        self.buf.sort_unstable();
        self.buf.dedup();
        if self.rule.by_set_pos.is_empty() {
            return;
        }
        let len = i64::try_from(self.buf.len()).unwrap_or(i64::MAX);
        let mut selected: Vec<i64> = self
            .rule
            .by_set_pos
            .iter()
            .filter_map(|&pos| {
                let index = if pos > 0 { pos - 1 } else { len + pos };
                usize::try_from(index)
                    .ok()
                    .and_then(|i| self.buf.get(i).copied())
            })
            .collect();
        selected.sort_unstable();
        selected.dedup();
        self.buf = selected;
    }

    /// Matching days of one month (yearly and monthly rules).
    fn month_candidates(
        &self,
        year: i64,
        month: i64,
        out: &mut Vec<i64>,
        budget: &mut Budget,
    ) -> Result<(), Inexact> {
        let first = days_from_civil(year, month, 1);
        let length = days_in_month(year, month);
        let only_month_days = self.days.is_empty()
            && self.rule.by_year_day.is_empty()
            && self.rule.by_week_no.is_empty()
            && !self.month_days.is_empty();
        if only_month_days {
            // Direct: no need to look at every day.
            for &month_day in &self.month_days {
                budget.charge(1)?;
                let day = if month_day > 0 {
                    month_day
                } else {
                    length + month_day + 1
                };
                // Invalid dates (Feb 30, the 31st of short months) are skipped.
                if (1..=length).contains(&day) {
                    out.push(first + day - 1);
                }
            }
            out.sort_unstable();
            return Ok(());
        }
        for day in first..first + length {
            budget.charge(1)?;
            if self.day_matches(day) {
                out.push(day);
            }
        }
        Ok(())
    }

    /// Whether a day passes all date-level BYxxx parts.
    fn day_matches(&self, day: i64) -> bool {
        let (year, month, month_day) = civil_from_days(day);
        if !self.months.is_empty() && !self.months.contains(&month) {
            return false;
        }
        if !self.month_days.is_empty() {
            let length = days_in_month(year, month);
            if !self.month_days.contains(&month_day)
                && !self.month_days.contains(&(month_day - length - 1))
            {
                return false;
            }
        }
        if !self.rule.by_year_day.is_empty() {
            let year_day = day - days_from_civil(year, 1, 1) + 1;
            let length = if is_leap(year) { 366 } else { 365 };
            if !self.rule.by_year_day.contains(&year_day)
                && !self.rule.by_year_day.contains(&(year_day - length - 1))
            {
                return false;
            }
        }
        if !self.rule.by_week_no.is_empty() {
            let (number, weeks) = week_number(day, year, self.rule.wkst);
            if !self.rule.by_week_no.contains(&number)
                && !self.rule.by_week_no.contains(&(number - weeks - 1))
            {
                return false;
            }
        }
        if !self.days.is_empty() {
            let wd = weekday(day);
            // Ordinals count within the month for monthly rules and for
            // yearly rules with BYMONTH, otherwise within the year.
            let (first, last) = if self.rule.freq == Freq::Monthly
                || (self.rule.freq == Freq::Yearly && !self.rule.by_month.is_empty())
            {
                let first = days_from_civil(year, month, 1);
                (first, first + days_in_month(year, month) - 1)
            } else {
                (days_from_civil(year, 1, 1), days_from_civil(year, 12, 31))
            };
            let matches = self.days.iter().any(|&(ordinal, d)| {
                d == wd
                    && match ordinal {
                        0 => true,
                        n if n > 0 => (day - first) / 7 + 1 == n,
                        n => (last - day) / 7 + 1 == -n,
                    }
            });
            if !matches {
                return false;
            }
        }
        true
    }
}

/// First day of week 1 of `year`: the first week (starting on `wkst`) with
/// at least four days in that year (RFC 5545 BYWEEKNO).
fn week_one(year: i64, wkst: i64) -> i64 {
    let jan1 = days_from_civil(year, 1, 1);
    let week_start = jan1 - (weekday(jan1) - wkst).rem_euclid(7);
    if jan1 - week_start <= 3 {
        week_start
    } else {
        week_start + 7
    }
}

/// Week number of `day` (which lies in calendar `year`) and the number of
/// weeks of its week-numbering year.
fn week_number(day: i64, year: i64, wkst: i64) -> (i64, i64) {
    let week_year = if day < week_one(year, wkst) {
        year - 1
    } else if day >= week_one(year + 1, wkst) {
        year + 1
    } else {
        year
    };
    let first = week_one(week_year, wkst);
    let weeks = (week_one(week_year + 1, wkst) - first) / 7;
    ((day - first) / 7 + 1, weeks)
}

#[cfg(test)]
#[path = "rrule_tests.rs"]
mod rfc_examples;

#[cfg(test)]
mod tests;
