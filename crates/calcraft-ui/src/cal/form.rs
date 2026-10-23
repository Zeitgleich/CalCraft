//! State of the event dialog and its conversion from and to the API.
//!
//! All-day events end at midnight after their last day in iCalendar (and
//! the API); the dialog shows the last day itself.

use calcraft_core::api::{
    Alarm, EventDetail, EventFields, EventInput, EventTime, Frequency, Recurrence, TimeZoneInfo,
};

use super::date::{DAY, format_date, format_time, parse_date, parse_time};
use crate::text;

/// Repeat choice of the dialog.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Repeat {
    None,
    Daily,
    Weekly,
    Monthly,
    Yearly,
    /// A rule the dialog cannot edit; kept as it is.
    Custom,
}

impl Repeat {
    pub const CHOICES: [Repeat; 5] = [
        Repeat::None,
        Repeat::Daily,
        Repeat::Weekly,
        Repeat::Monthly,
        Repeat::Yearly,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Daily => "daily",
            Self::Weekly => "weekly",
            Self::Monthly => "monthly",
            Self::Yearly => "yearly",
            Self::Custom => "custom",
        }
    }

    pub fn parse(value: &str) -> Self {
        match value {
            "daily" => Self::Daily,
            "weekly" => Self::Weekly,
            "monthly" => Self::Monthly,
            "yearly" => Self::Yearly,
            "custom" => Self::Custom,
            _ => Self::None,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::None => text::EV_REPEAT_NONE,
            Self::Daily => text::EV_REPEAT_DAILY,
            Self::Weekly => text::EV_REPEAT_WEEKLY,
            Self::Monthly => text::EV_REPEAT_MONTHLY,
            Self::Yearly => text::EV_REPEAT_YEARLY,
            Self::Custom => text::EV_REPEAT_CUSTOM,
        }
    }

    fn frequency(self) -> Option<Frequency> {
        match self {
            Self::Daily => Some(Frequency::Daily),
            Self::Weekly => Some(Frequency::Weekly),
            Self::Monthly => Some(Frequency::Monthly),
            Self::Yearly => Some(Frequency::Yearly),
            Self::None | Self::Custom => None,
        }
    }
}

/// How a repetition ends.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ends {
    Never,
    Count,
    Until,
}

impl Ends {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Never => "never",
            Self::Count => "count",
            Self::Until => "until",
        }
    }

    pub fn parse(value: &str) -> Self {
        match value {
            "count" => Self::Count,
            "until" => Self::Until,
            _ => Self::Never,
        }
    }
}

pub const WEEKDAY_CODES: [&str; 7] = ["MO", "TU", "WE", "TH", "FR", "SA", "SU"];

/// The dialog's fields, as text the inputs hold.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EventForm {
    pub calendar: String,
    pub summary: String,
    pub location: String,
    pub description: String,
    pub all_day: bool,
    pub start_date: String,
    pub start_time: String,
    /// For all-day events the last day (inclusive).
    pub end_date: String,
    pub end_time: String,
    /// `""` floating, `UTC`, or an IANA name.
    pub tzid: String,
    pub repeat: Repeat,
    pub interval: String,
    pub ends: Ends,
    pub count: String,
    pub until: String,
    pub weekdays: [bool; 7],
    /// Minutes before the start, as typed.
    pub alarms: Vec<String>,
    pub private: bool,
}

impl EventForm {
    /// A new event at local `start` (seconds), one hour long, or an
    /// all-day event on its day.
    pub fn new_at(calendar: &str, start: i64, all_day: bool, tzid: &str) -> Self {
        let day = start.div_euclid(DAY);
        let minutes = start.rem_euclid(DAY) / 60;
        let end = start + 3600;
        Self {
            calendar: calendar.to_owned(),
            summary: String::new(),
            location: String::new(),
            description: String::new(),
            all_day,
            start_date: format_date(day),
            start_time: format_time(minutes),
            end_date: format_date(if all_day { day } else { end.div_euclid(DAY) }),
            end_time: format_time(end.rem_euclid(DAY) / 60),
            tzid: tzid.to_owned(),
            repeat: Repeat::None,
            interval: "1".to_owned(),
            ends: Ends::Never,
            count: "10".to_owned(),
            until: String::new(),
            weekdays: weekday_flags(day),
            alarms: Vec::new(),
            private: false,
        }
    }

    /// The form for an event as the server sent it.
    pub fn from_detail(detail: &EventDetail, browser_zone: &str) -> Self {
        let fields = &detail.fields;
        let all_day = fields.start.time.is_none();
        let start_day = parse_date(&fields.start.date).unwrap_or(0);
        let end_day = parse_date(&fields.end.date).unwrap_or(start_day);
        let mut form = Self::new_at(&detail.calendar, start_day * DAY, all_day, browser_zone);
        form.summary = fields.summary.clone();
        form.location = fields.location.clone();
        form.description = fields.description.clone();
        form.start_date = fields.start.date.clone();
        form.start_time = fields
            .start
            .time
            .clone()
            .unwrap_or_else(|| "09:00".to_owned());
        form.end_date = if all_day {
            format_date((end_day - 1).max(start_day))
        } else {
            fields.end.date.clone()
        };
        form.end_time = fields
            .end
            .time
            .clone()
            .unwrap_or_else(|| "10:00".to_owned());
        if !all_day {
            form.tzid = fields.start.tzid.clone().unwrap_or_default();
        }
        form.private = fields.private;
        form.alarms = fields
            .alarms
            .iter()
            .map(|a| a.minutes_before.to_string())
            .collect();
        match (&detail.recurrence, &detail.recurrence_rule) {
            (Some(rule), _) => {
                form.repeat = match rule.freq {
                    Frequency::Daily => Repeat::Daily,
                    Frequency::Weekly => Repeat::Weekly,
                    Frequency::Monthly => Repeat::Monthly,
                    Frequency::Yearly => Repeat::Yearly,
                };
                form.interval = rule.interval.to_string();
                if let Some(count) = rule.count {
                    form.ends = Ends::Count;
                    form.count = count.to_string();
                } else if let Some(until) = &rule.until {
                    form.ends = Ends::Until;
                    form.until = until.clone();
                }
                if !rule.weekdays.is_empty() {
                    form.weekdays =
                        WEEKDAY_CODES.map(|code| rule.weekdays.iter().any(|d| d == code));
                }
            }
            (None, Some(_)) => form.repeat = Repeat::Custom,
            (None, None) => {}
        }
        form
    }

    /// Local start and end (seconds, read as if UTC) in the event's zone.
    pub fn local_times(&self) -> Result<(i64, i64), &'static str> {
        let start_day = parse_date(&self.start_date).ok_or(text::ERR_EV_DATE)?;
        let end_day = parse_date(&self.end_date).ok_or(text::ERR_EV_DATE)?;
        if self.all_day {
            if end_day < start_day {
                return Err(text::ERR_EV_END);
            }
            return Ok((start_day * DAY, (end_day + 1) * DAY));
        }
        let start = parse_time(&self.start_time).ok_or(text::ERR_EV_DATE)?;
        let end = parse_time(&self.end_time).ok_or(text::ERR_EV_DATE)?;
        let (start, end) = (start_day * DAY + start * 60, end_day * DAY + end * 60);
        if end < start {
            return Err(text::ERR_EV_END);
        }
        Ok((start, end))
    }

    fn tzid(&self) -> Option<String> {
        let tzid = self.tzid.trim();
        (!self.all_day && !tzid.is_empty()).then(|| tzid.to_owned())
    }

    /// The zone (other than UTC) whose definition the request needs.
    pub fn zone(&self) -> Option<String> {
        self.tzid().filter(|z| z != "UTC")
    }

    fn time(&self, days: i64, minutes: Option<i64>) -> EventTime {
        EventTime {
            date: format_date(days),
            time: minutes.map(format_time),
            tzid: self.tzid(),
        }
    }

    fn recurrence(&self, start_day: i64) -> Result<Option<Recurrence>, &'static str> {
        let Some(freq) = self.repeat.frequency() else {
            return Ok(None);
        };
        let interval: u32 = self
            .interval
            .trim()
            .parse()
            .ok()
            .filter(|i| (1..=999).contains(i))
            .ok_or(text::ERR_EV_INTERVAL)?;
        let (count, until) = match self.ends {
            Ends::Never => (None, None),
            Ends::Count => (
                Some(
                    self.count
                        .trim()
                        .parse::<u32>()
                        .ok()
                        .filter(|c| (1..=5000).contains(c))
                        .ok_or(text::ERR_EV_COUNT)?,
                ),
                None,
            ),
            Ends::Until => {
                let until = parse_date(&self.until)
                    .filter(|d| *d >= start_day)
                    .ok_or(text::ERR_EV_UNTIL)?;
                (None, Some(format_date(until)))
            }
        };
        let weekdays = if freq == Frequency::Weekly {
            let days: Vec<String> = WEEKDAY_CODES
                .iter()
                .zip(self.weekdays)
                .filter(|(_, on)| *on)
                .map(|(code, _)| (*code).to_owned())
                .collect();
            if days.is_empty() {
                return Err(text::ERR_EV_WEEKDAYS);
            }
            days
        } else {
            Vec::new()
        };
        Ok(Some(Recurrence {
            freq,
            interval,
            count,
            until,
            weekdays,
        }))
    }

    /// The API request: for the series (`rid = None`) or one instance.
    /// `zones` are the browser's definitions of the zones used.
    pub fn to_input(
        &self,
        rid: Option<String>,
        zones: Vec<TimeZoneInfo>,
    ) -> Result<EventInput, &'static str> {
        const MAX_SHORT: usize = 1_000;
        const MAX_LONG: usize = 32 * 1024;
        if self.summary.len() > MAX_SHORT
            || self.location.len() > MAX_SHORT
            || self.description.len() > MAX_LONG
        {
            return Err(text::ERR_EV_TOO_LONG);
        }
        let (start, end) = self.local_times()?;
        let (start_day, end_day) = (start.div_euclid(DAY), end.div_euclid(DAY));
        let (start_time, end_time) = if self.all_day {
            (None, None)
        } else {
            (
                Some(start.rem_euclid(DAY) / 60),
                Some(end.rem_euclid(DAY) / 60),
            )
        };
        let mut alarms = Vec::new();
        for alarm in &self.alarms {
            let minutes: i64 = alarm
                .trim()
                .parse()
                .ok()
                .filter(|m| (0..=40_320).contains(m))
                .ok_or(text::ERR_EV_REMINDER)?;
            alarms.push(Alarm {
                minutes_before: minutes,
            });
        }
        let series = rid.is_none();
        let recurrence = if series && self.repeat != Repeat::Custom {
            self.recurrence(start_day)?
        } else {
            None
        };
        Ok(EventInput {
            recurrence_id: rid,
            fields: EventFields {
                summary: self.summary.trim().to_owned(),
                location: self.location.trim().to_owned(),
                description: self.description.clone(),
                start: self.time(start_day, start_time),
                end: self.time(end_day, end_time),
                alarms,
                private: self.private,
            },
            recurrence,
            keep_recurrence: series && self.repeat == Repeat::Custom,
            timezones: zones,
        })
    }

    /// Switches between all-day and timed, keeping the days.
    pub fn set_all_day(&mut self, all_day: bool) {
        self.all_day = all_day;
    }
}

/// Only the weekday of `day` set (default of weekly rules).
pub fn weekday_flags(day: i64) -> [bool; 7] {
    let mut flags = [false; 7];
    flags[super::date::weekday(day)] = true;
    flags
}

/// A rule in words: “every 2 weeks on Mon, Wed, until 2026-12-31”.
pub fn describe(rule: &Recurrence) -> String {
    let unit = match rule.freq {
        Frequency::Daily => "day",
        Frequency::Weekly => "week",
        Frequency::Monthly => "month",
        Frequency::Yearly => "year",
    };
    let mut out = text::every(unit, rule.interval);
    if !rule.weekdays.is_empty() {
        let days: Vec<&str> = WEEKDAY_CODES
            .iter()
            .enumerate()
            .filter(|(_, code)| rule.weekdays.iter().any(|d| d == *code))
            .map(|(i, _)| text::WEEKDAYS_SHORT[i])
            .collect();
        out.push_str(" on ");
        out.push_str(&days.join(", "));
    }
    if let Some(count) = rule.count {
        out.push_str(&format!(", {count} times"));
    }
    if let Some(until) = &rule.until {
        out.push_str(&format!(", until {until}"));
    }
    out
}
