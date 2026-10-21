//! Validation of CalDAV calendar object resources (RFC 4791 §4.1, §5.3.2.1)
//! and extraction of the data needed for indexing.

use crate::{
    Component, IcalError, Property,
    datetime::{DateTime, parse_date_or_datetime, parse_duration},
    parse,
};

const MAX_UID_LEN: usize = 1024;
/// Largest UTC offset of any timezone (UTC+14). Local times are widened by
/// this much in both directions, which keeps time-range matching correct
/// without a timezone database.
const LOCAL_TIME_SLACK: i64 = 14 * 3600;

/// Component type of a calendar object resource.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ComponentKind {
    Event,
    Todo,
    Journal,
}

impl ComponentKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Event => "VEVENT",
            Self::Todo => "VTODO",
            Self::Journal => "VJOURNAL",
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "VEVENT" => Some(Self::Event),
            "VTODO" => Some(Self::Todo),
            "VJOURNAL" => Some(Self::Journal),
            _ => None,
        }
    }
}

/// Inclusive-exclusive span in Unix seconds; `None` means unbounded.
///
/// The span may be wider than the exact set of occurrences, never narrower:
/// recurring objects without an `UNTIL` are open-ended, and local times are
/// widened by the maximum UTC offset.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TimeRange {
    pub start: Option<i64>,
    pub end: Option<i64>,
}

impl TimeRange {
    pub const UNBOUNDED: Self = Self {
        start: None,
        end: None,
    };

    /// Whether this span overlaps the query interval `[start, end)`.
    pub fn overlaps(&self, start: Option<i64>, end: Option<i64>) -> bool {
        let begins_before_end = match (self.start, end) {
            (Some(s), Some(e)) => s < e,
            _ => true,
        };
        let ends_after_start = match (self.end, start) {
            (Some(e), Some(s)) => e > s,
            _ => true,
        };
        begins_before_end && ends_after_start
    }

    fn union(self, other: Self) -> Self {
        Self {
            start: self.start.zip(other.start).map(|(a, b)| a.min(b)),
            end: self.end.zip(other.end).map(|(a, b)| a.max(b)),
        }
    }
}

/// A validated calendar object resource.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CalendarObject {
    pub uid: String,
    pub kind: ComponentKind,
    pub range: TimeRange,
}

impl CalendarObject {
    /// Parses and validates a calendar object resource as stored via PUT.
    pub fn parse(data: &[u8]) -> Result<Self, IcalError> {
        let calendar = parse(data)?;
        if calendar.name != "VCALENDAR" {
            return Err(IcalError::Invalid("top-level component must be VCALENDAR"));
        }
        if single(&calendar, "VERSION")?.map(|p| p.value.as_str()) != Some("2.0") {
            return Err(IcalError::Invalid("VERSION:2.0 is required"));
        }
        // Scheduling messages are not calendar object resources (RFC 4791 §4.1).
        if calendar.property("METHOD").is_some() {
            return Err(IcalError::Invalid(
                "METHOD is not allowed in stored objects",
            ));
        }

        let mut kind: Option<ComponentKind> = None;
        let mut uid: Option<&str> = None;
        let mut range: Option<TimeRange> = None;
        let mut masters = 0;
        let mut recurrence_ids: Vec<&str> = Vec::new();

        for component in &calendar.components {
            if component.name == "VTIMEZONE" {
                continue;
            }
            let this_kind = ComponentKind::from_name(&component.name)
                .ok_or(IcalError::Invalid("unsupported component type"))?;
            if kind.is_some_and(|k| k != this_kind) {
                return Err(IcalError::Invalid("mixed component types"));
            }
            kind = Some(this_kind);

            let this_uid = single(component, "UID")?
                .map(|p| p.value.as_str())
                .ok_or(IcalError::Invalid("UID is required"))?;
            if this_uid.is_empty() || this_uid.len() > MAX_UID_LEN {
                return Err(IcalError::Invalid("invalid UID"));
            }
            if uid.is_some_and(|u| u != this_uid) {
                return Err(IcalError::Invalid("all components must share one UID"));
            }
            uid = Some(this_uid);

            match single(component, "RECURRENCE-ID")? {
                Some(rid) => {
                    if recurrence_ids.contains(&rid.value.as_str()) {
                        return Err(IcalError::Invalid("duplicate RECURRENCE-ID"));
                    }
                    recurrence_ids.push(&rid.value);
                }
                None => masters += 1,
            }
            if masters > 1 {
                return Err(IcalError::Invalid("more than one master component"));
            }

            let component_range = component_range(component, this_kind)?;
            range = Some(range.map_or(component_range, |r| r.union(component_range)));
        }

        Ok(Self {
            uid: uid
                .ok_or(IcalError::Invalid("no event, todo or journal"))?
                .to_owned(),
            kind: kind.ok_or(IcalError::Invalid("no event, todo or journal"))?,
            range: range.unwrap_or(TimeRange::UNBOUNDED),
        })
    }
}

/// Returns the property if it occurs at most once.
fn single<'a>(component: &'a Component, name: &'a str) -> Result<Option<&'a Property>, IcalError> {
    let mut found = component.properties_named(name);
    let first = found.next();
    if found.next().is_some() {
        return Err(IcalError::Invalid("property must not occur more than once"));
    }
    Ok(first)
}

fn datetime(component: &Component, name: &str) -> Result<Option<DateTime>, IcalError> {
    single(component, name)?
        .map(|p| parse_date_or_datetime(&p.value))
        .transpose()
}

fn duration(component: &Component) -> Result<Option<i64>, IcalError> {
    single(component, "DURATION")?
        .map(|p| parse_duration(&p.value))
        .transpose()
}

/// Validates the RRULE syntax that matters for safe expansion and returns
/// its UNTIL, if any. The rule is not expanded here.
fn rrule_until(rule: &str) -> Result<Option<DateTime>, IcalError> {
    let invalid = IcalError::Invalid("invalid RRULE");
    let mut freq = false;
    let mut until = None;
    let mut count = false;
    for part in rule.split(';') {
        let (key, value) = part.split_once('=').ok_or(invalid.clone())?;
        match key.to_ascii_uppercase().as_str() {
            "FREQ" => {
                if !matches!(
                    value.to_ascii_uppercase().as_str(),
                    "SECONDLY" | "MINUTELY" | "HOURLY" | "DAILY" | "WEEKLY" | "MONTHLY" | "YEARLY"
                ) {
                    return Err(invalid);
                }
                freq = true;
            }
            "INTERVAL" | "COUNT" => {
                let n: u32 = value.parse().map_err(|_| invalid.clone())?;
                if n == 0 {
                    return Err(invalid);
                }
                count |= key.eq_ignore_ascii_case("COUNT");
            }
            "UNTIL" => until = Some(parse_date_or_datetime(value)?),
            _ => {}
        }
    }
    if !freq || (count && until.is_some()) {
        return Err(invalid);
    }
    Ok(until)
}

fn component_range(component: &Component, kind: ComponentKind) -> Result<TimeRange, IcalError> {
    let start = datetime(component, "DTSTART")?;
    let end_prop = match kind {
        ComponentKind::Event => datetime(component, "DTEND")?,
        ComponentKind::Todo => datetime(component, "DUE")?,
        ComponentKind::Journal => None,
    };
    let duration = duration(component)?;
    if end_prop.is_some() && duration.is_some() {
        return Err(IcalError::Invalid("both an end and DURATION"));
    }

    let (start, end, local) = match (start, end_prop) {
        (Some(start), end) => {
            let end_seconds = match (end, duration) {
                (Some(end), _) => end.seconds,
                (None, Some(d)) => start.seconds + d,
                (None, None) if start.is_date => start.seconds + 86_400,
                (None, None) => start.seconds,
            };
            if end_seconds < start.seconds {
                return Err(IcalError::Invalid("end before start"));
            }
            let local = !start.is_utc || end.is_some_and(|e| !e.is_utc);
            (Some(start.seconds), Some(end_seconds), local)
        }
        // A VTODO with only DUE spans up to its due date.
        (None, Some(due)) => (Some(due.seconds), Some(due.seconds), !due.is_utc),
        // No dates: matches every time range.
        (None, None) => return Ok(TimeRange::UNBOUNDED),
    };

    // Recurrence: open-ended unless bounded by UNTIL; RDATE makes it open-ended.
    let mut end = end;
    let rrules: Vec<&Property> = component.properties_named("RRULE").collect();
    if rrules.len() > 1 {
        return Err(IcalError::Invalid("more than one RRULE"));
    }
    if let Some(rrule) = rrules.first() {
        end = match rrule_until(&rrule.value)? {
            Some(until) => end.zip(start).map(|(e, s)| until.seconds + (e - s)),
            None => None,
        };
    }
    if component.property("RDATE").is_some() {
        end = None;
    }

    let slack = if local { LOCAL_TIME_SLACK } else { 0 };
    Ok(TimeRange {
        start: start.map(|s| s - slack),
        // Zero-length instances still match at their start time.
        end: end.map(|e| e.max(start.unwrap_or(e) + 1) + slack),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn object(body: &str) -> Result<CalendarObject, IcalError> {
        let data =
            format!("BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//T//EN\r\n{body}END:VCALENDAR\r\n");
        CalendarObject::parse(data.as_bytes())
    }

    #[test]
    fn simple_utc_event() {
        let obj = object("BEGIN:VEVENT\r\nUID:a\r\nDTSTART:20261021T100000Z\r\nDTEND:20261021T110000Z\r\nEND:VEVENT\r\n").unwrap();
        assert_eq!(obj.uid, "a");
        assert_eq!(obj.kind, ComponentKind::Event);
        assert_eq!(obj.range.start, Some(1_792_576_800));
        assert_eq!(obj.range.end, Some(1_792_580_400));
        assert!(obj.range.overlaps(Some(1_792_576_800), Some(1_792_576_801)));
        assert!(!obj.range.overlaps(Some(1_792_580_400), None));
        assert!(!obj.range.overlaps(None, Some(1_792_576_800)));
    }

    #[test]
    fn local_time_is_widened() {
        let obj = object("BEGIN:VEVENT\r\nUID:a\r\nDTSTART;TZID=Europe/Berlin:20261021T100000\r\nDURATION:PT1H\r\nEND:VEVENT\r\n").unwrap();
        assert_eq!(obj.range.start, Some(1_792_576_800 - LOCAL_TIME_SLACK));
        assert_eq!(obj.range.end, Some(1_792_580_400 + LOCAL_TIME_SLACK));
    }

    #[test]
    fn all_day_event_lasts_one_day() {
        let obj = object("BEGIN:VEVENT\r\nUID:a\r\nDTSTART;VALUE=DATE:20261021\r\nEND:VEVENT\r\n")
            .unwrap();
        assert_eq!(
            obj.range.end.unwrap() - obj.range.start.unwrap(),
            86_400 + 2 * LOCAL_TIME_SLACK
        );
    }

    #[test]
    fn recurrence_ranges() {
        let open = object("BEGIN:VEVENT\r\nUID:a\r\nDTSTART:20261021T100000Z\r\nRRULE:FREQ=WEEKLY\r\nEND:VEVENT\r\n").unwrap();
        assert_eq!(open.range.end, None);
        let counted = object("BEGIN:VEVENT\r\nUID:a\r\nDTSTART:20261021T100000Z\r\nRRULE:FREQ=DAILY;COUNT=3\r\nEND:VEVENT\r\n").unwrap();
        assert_eq!(counted.range.end, None);
        let until = object("BEGIN:VEVENT\r\nUID:a\r\nDTSTART:20261021T100000Z\r\nDTEND:20261021T110000Z\r\nRRULE:FREQ=DAILY;UNTIL=20261031T100000Z\r\nEND:VEVENT\r\n").unwrap();
        assert_eq!(until.range.end, Some(1_793_440_800 + 3600));
    }

    #[test]
    fn overrides_share_uid() {
        let obj = object("BEGIN:VEVENT\r\nUID:a\r\nDTSTART:20261021T100000Z\r\nRRULE:FREQ=DAILY\r\nEND:VEVENT\r\nBEGIN:VEVENT\r\nUID:a\r\nRECURRENCE-ID:20261022T100000Z\r\nDTSTART:20261022T120000Z\r\nEND:VEVENT\r\n").unwrap();
        assert_eq!(obj.uid, "a");
    }

    #[test]
    fn todo_without_dates_is_unbounded() {
        let obj = object("BEGIN:VTODO\r\nUID:t\r\nSUMMARY:x\r\nEND:VTODO\r\n").unwrap();
        assert_eq!(obj.kind, ComponentKind::Todo);
        assert_eq!(obj.range, TimeRange::UNBOUNDED);
    }

    #[test]
    fn rejects_invalid_objects() {
        let cases = [
            "BEGIN:VEVENT\r\nDTSTART:20261021T100000Z\r\nEND:VEVENT\r\n",
            "BEGIN:VEVENT\r\nUID:a\r\nUID:b\r\nEND:VEVENT\r\n",
            "BEGIN:VEVENT\r\nUID:a\r\nEND:VEVENT\r\nBEGIN:VEVENT\r\nUID:b\r\nEND:VEVENT\r\n",
            "BEGIN:VEVENT\r\nUID:a\r\nEND:VEVENT\r\nBEGIN:VTODO\r\nUID:a\r\nEND:VTODO\r\n",
            "BEGIN:VEVENT\r\nUID:a\r\nEND:VEVENT\r\nBEGIN:VEVENT\r\nUID:a\r\nEND:VEVENT\r\n",
            "BEGIN:VFREEBUSY\r\nUID:a\r\nEND:VFREEBUSY\r\n",
            "BEGIN:VEVENT\r\nUID:a\r\nDTSTART:20261021T100000Z\r\nDTEND:20261020T100000Z\r\nEND:VEVENT\r\n",
            "BEGIN:VEVENT\r\nUID:a\r\nDTSTART:20261021T100000Z\r\nDTEND:20261021T110000Z\r\nDURATION:PT1H\r\nEND:VEVENT\r\n",
            "BEGIN:VEVENT\r\nUID:a\r\nDTSTART:20261021T100000Z\r\nRRULE:FREQ=DAILY;INTERVAL=0\r\nEND:VEVENT\r\n",
            "BEGIN:VEVENT\r\nUID:a\r\nDTSTART:20261021T100000Z\r\nRRULE:COUNT=3\r\nEND:VEVENT\r\n",
            "BEGIN:VEVENT\r\nUID:a\r\nDTSTART:20261021T100000Z\r\nRRULE:FREQ=DAILY;COUNT=2;UNTIL=20261101T000000Z\r\nEND:VEVENT\r\n",
            "BEGIN:VEVENT\r\nUID:a\r\nDTSTART:2026-10-21\r\nEND:VEVENT\r\n",
            "",
        ];
        for body in cases {
            assert!(object(body).is_err(), "{body:?}");
        }
        assert!(CalendarObject::parse(b"BEGIN:VCALENDAR\r\nVERSION:2.0\r\nMETHOD:REQUEST\r\nBEGIN:VEVENT\r\nUID:a\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n").is_err());
        assert!(
            CalendarObject::parse(
                b"BEGIN:VCALENDAR\r\nBEGIN:VEVENT\r\nUID:a\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n"
            )
            .is_err()
        );
        assert!(CalendarObject::parse(b"BEGIN:VCALENDAR\r\nVERSION:2.0\r\nVERSION:3.0\r\nBEGIN:VEVENT\r\nUID:a\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n").is_err());
        assert!(CalendarObject::parse(b"BEGIN:VCARD\r\nVERSION:4.0\r\nEND:VCARD\r\n").is_err());
    }
}
