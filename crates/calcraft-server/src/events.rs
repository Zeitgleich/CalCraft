//! Events as the web UI edits them: reading the dialog's fields from a
//! calendar object and applying changes to it (`/api/v1/calendars/…/events`).
//!
//! Everything here works on the parsed component tree and only touches the
//! properties the dialog owns (times, texts, class, the simple recurrence
//! rule, display reminders); every other property, parameter and component
//! (attendees, attachments, other alarms, X- properties, unknown
//! components) is kept as it was. The result is serialised and stored
//! through the same path as a DAV PUT, which validates it again.
//!
//! Recurrence editing model:
//! - the series (or a single event) is the master component;
//! - one instance is an override (`RECURRENCE-ID`), created from the master
//!   on the first change; deleting one instance adds an `EXDATE` and drops
//!   its override;
//! - moving the series' start shifts its `EXDATE`s and the overrides'
//!   `RECURRENCE-ID`s by the same local-time delta, so they keep naming the
//!   same instances; changing between all-day and timed (or the time zone)
//!   drops them, as they can no longer name an instance.

use calcraft_core::api::{
    Alarm, EventAttachment, EventFields, EventInput, EventTime, EventTimes, Frequency, Participant,
    Recurrence, TimeZoneInfo,
};
use calcraft_ical::{
    Component, DateTime, Parameter, Property, attach, format_date_or_datetime, master_instance_id,
    override_index, parse_date_or_datetime, parse_duration,
    write::{escape_text, split_unescaped, unescape_text},
    zone_to_local, zone_to_utc,
};

/// Longest summary or location.
pub const MAX_SHORT_TEXT: usize = 1_000;
/// Longest description.
pub const MAX_DESCRIPTION: usize = 32 * 1024;
/// Most reminders per event.
pub const MAX_ALARMS: usize = 10;
/// Earliest reminder: four weeks before the start.
pub const MAX_ALARM_MINUTES: i64 = 4 * 7 * 24 * 60;
/// Largest recurrence interval and count.
pub const MAX_INTERVAL: u32 = 999;
pub const MAX_COUNT: u32 = 5_000;
/// Most observances of a time zone definition sent by the UI.
const MAX_OBSERVANCES: usize = 8;
const MAX_TZID_LEN: usize = 64;
const DAY: i64 = 86_400;
/// `RECURRENCE-ID` components an object may have (as for attachments).
const MAX_OVERRIDES: usize = attach::MAX_INSTANCE_COMPONENTS;
const WEEKDAYS: [&str; 7] = ["MO", "TU", "WE", "TH", "FR", "SA", "SU"];

/// Why a change cannot be applied.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EditError {
    /// The request is invalid; the text says why (shown to the user).
    Invalid(&'static str),
    /// The object is no event, or has no such instance.
    NotFound,
}

type Result<T> = std::result::Result<T, EditError>;

fn invalid(message: &'static str) -> EditError {
    EditError::Invalid(message)
}

fn property(name: &str, parameters: Vec<Parameter>, value: String) -> Property {
    Property {
        name: name.to_owned(),
        parameters,
        value,
    }
}

fn parameter(name: &str, value: &str) -> Parameter {
    Parameter {
        name: name.to_owned(),
        values: vec![value.to_owned()],
    }
}

fn is_event(component: &Component) -> bool {
    component.name == "VEVENT"
}

/// Index of the master event (the first VEVENT without RECURRENCE-ID), or
/// of the first VEVENT for objects with overrides only.
pub fn master_index(root: &Component) -> Option<usize> {
    root.components
        .iter()
        .position(|c| is_event(c) && c.property("RECURRENCE-ID").is_none())
        .or_else(|| root.components.iter().position(is_event))
}

/// Whether the object is a recurring series.
pub fn is_recurring(root: &Component) -> bool {
    root.components.iter().any(|c| {
        is_event(c)
            && (c.property("RRULE").is_some()
                || c.property("RDATE").is_some()
                || c.property("RECURRENCE-ID").is_some())
    })
}

/// Unescaped text of a property, `None` if absent or empty.
pub fn text(component: &Component, name: &str) -> Option<String> {
    component
        .property(name)
        .map(|p| unescape_text(&p.value))
        .filter(|t| !t.trim().is_empty())
}

fn tzid(property: &Property) -> Option<&str> {
    property
        .parameter("TZID")
        .and_then(|p| p.values.first())
        .map(String::as_str)
}

// ---------------------------------------------------------------- reading

fn format_date(seconds: i64) -> String {
    let compact = format_date_or_datetime(seconds, true, false).unwrap_or_default();
    if compact.len() == 8 {
        format!("{}-{}-{}", &compact[..4], &compact[4..6], &compact[6..])
    } else {
        compact
    }
}

fn format_time(seconds: i64) -> String {
    let day = seconds.rem_euclid(DAY);
    format!("{:02}:{:02}", day / 3600, day / 60 % 60)
}

/// A DTSTART/DTEND value as the dialog shows it.
fn event_time(parsed: DateTime, tzid: Option<&str>) -> EventTime {
    if parsed.is_date {
        return EventTime {
            date: format_date(parsed.seconds),
            time: None,
            tzid: None,
        };
    }
    EventTime {
        date: format_date(parsed.seconds),
        time: Some(format_time(parsed.seconds)),
        tzid: if parsed.is_utc {
            Some("UTC".to_owned())
        } else {
            tzid.map(str::to_owned)
        },
    }
}

/// Start and end of a component for the dialog.
fn component_times(component: &Component) -> Result<(EventTime, EventTime)> {
    let start_property = component.property("DTSTART").ok_or(EditError::NotFound)?;
    let start = parse_date_or_datetime(&start_property.value).map_err(|_| EditError::NotFound)?;
    let start_tzid = tzid(start_property);
    let end = match component.property("DTEND") {
        Some(end_property) => parse_date_or_datetime(&end_property.value)
            .ok()
            .map(|end| event_time(end, tzid(end_property))),
        None => None,
    };
    let end = end.unwrap_or_else(|| {
        let duration = component
            .property("DURATION")
            .and_then(|p| parse_duration(&p.value).ok())
            .unwrap_or(if start.is_date { DAY } else { 0 });
        event_time(
            DateTime {
                seconds: start.seconds.saturating_add(duration.max(0)),
                ..start
            },
            start_tzid,
        )
    });
    Ok((event_time(start, start_tzid), end))
}

fn alarms(component: &Component) -> Vec<Alarm> {
    component
        .components
        .iter()
        .filter_map(owned_alarm_minutes)
        .map(|minutes_before| Alarm { minutes_before })
        .collect()
}

/// Minutes before the start of a reminder the dialog owns: a VALARM with
/// ACTION DISPLAY or AUDIO and a trigger relative to the start.
fn owned_alarm_minutes(alarm: &Component) -> Option<i64> {
    if alarm.name != "VALARM" {
        return None;
    }
    let action = alarm.property("ACTION")?.value.to_ascii_uppercase();
    if action != "DISPLAY" && action != "AUDIO" {
        return None;
    }
    let trigger = alarm.property("TRIGGER")?;
    let absolute = trigger
        .parameter("VALUE")
        .is_some_and(|p| p.values.iter().any(|v| v.eq_ignore_ascii_case("DATE-TIME")));
    let related_end = trigger
        .parameter("RELATED")
        .is_some_and(|p| p.values.iter().any(|v| v.eq_ignore_ascii_case("END")));
    if absolute || related_end {
        return None;
    }
    let seconds = parse_duration(&trigger.value).ok()?;
    (seconds <= 0).then_some(-seconds / 60)
}

fn is_private(component: &Component, master: Option<&Component>) -> bool {
    let class = component
        .property("CLASS")
        .or_else(|| master.and_then(|m| m.property("CLASS")));
    class.is_some_and(|c| !c.value.trim().eq_ignore_ascii_case("PUBLIC"))
}

fn fields(component: &Component, master: Option<&Component>) -> Result<EventFields> {
    let (start, end) = component_times(component)?;
    Ok(EventFields {
        summary: text(component, "SUMMARY").unwrap_or_default(),
        location: text(component, "LOCATION").unwrap_or_default(),
        description: text(component, "DESCRIPTION").unwrap_or_default(),
        start,
        end,
        alarms: alarms(component),
        private: is_private(component, master),
    })
}

fn participant(property: &Property) -> Participant {
    let value = property.value.trim();
    let address = value
        .get(..7)
        .filter(|scheme| scheme.eq_ignore_ascii_case("mailto:"))
        .map_or(value, |_| &value[7..]);
    let first = |name: &str| {
        property
            .parameter(name)
            .and_then(|p| p.values.first())
            .cloned()
    };
    Participant {
        address: address.to_owned(),
        name: first("CN"),
        status: first("PARTSTAT"),
        role: first("ROLE"),
    }
}

/// Download link of an attachment the UI may show: managed attachments
/// through the API, plain `http(s)` links as they are.
fn attachment(property: &Property) -> Option<EventAttachment> {
    let first = |name: &str| {
        property
            .parameter(name)
            .and_then(|p| p.values.first())
            .cloned()
    };
    if let Some(id) = attach::managed_id(property) {
        return calcraft_store::valid_managed_id(id).then(|| EventAttachment {
            name: first("FILENAME"),
            url: format!("/api/v1/attachments/{id}"),
            size: first("SIZE").and_then(|s| s.parse().ok()),
            managed: true,
        });
    }
    let binary = property
        .parameter("VALUE")
        .is_some_and(|p| p.values.iter().any(|v| v.eq_ignore_ascii_case("BINARY")));
    let url = property.value.trim();
    let lower = url.to_ascii_lowercase();
    let web = (lower.starts_with("https://") || lower.starts_with("http://"))
        && url.len() <= 2048
        && url.bytes().all(|b| b.is_ascii_graphic());
    (!binary && web).then(|| EventAttachment {
        name: first("FILENAME"),
        url: url.to_owned(),
        size: None,
        managed: false,
    })
}

/// A rule the dialog can edit, from `RRULE` text; `None` for anything
/// else (BYMONTHDAY, BYSETPOS, numbered weekdays, …).
pub fn simple_rule(rule: &str, start: &Property, root: &Component) -> Option<Recurrence> {
    let mut recurrence = Recurrence {
        freq: Frequency::Daily,
        interval: 1,
        count: None,
        until: None,
        weekdays: Vec::new(),
    };
    let mut freq = None;
    for part in rule.split(';') {
        let (key, value) = part.split_once('=')?;
        match key.to_ascii_uppercase().as_str() {
            "FREQ" => {
                freq = Some(match value.to_ascii_uppercase().as_str() {
                    "DAILY" => Frequency::Daily,
                    "WEEKLY" => Frequency::Weekly,
                    "MONTHLY" => Frequency::Monthly,
                    "YEARLY" => Frequency::Yearly,
                    _ => return None,
                });
            }
            "INTERVAL" => recurrence.interval = value.parse().ok().filter(|i| *i >= 1)?,
            "COUNT" => recurrence.count = Some(value.parse().ok()?),
            "UNTIL" => recurrence.until = Some(until_date(value, start, root)?),
            "BYDAY" => {
                for day in value.split(',') {
                    let day = day.to_ascii_uppercase();
                    WEEKDAYS.contains(&day.as_str()).then_some(())?;
                    recurrence.weekdays.push(day);
                }
            }
            "WKST" => {}
            _ => return None,
        }
    }
    recurrence.freq = freq?;
    if !recurrence.weekdays.is_empty() && recurrence.freq != Frequency::Weekly {
        return None;
    }
    Some(recurrence)
}

/// The last day of an `UNTIL` value, in the zone of DTSTART.
fn until_date(value: &str, start: &Property, root: &Component) -> Option<String> {
    let until = parse_date_or_datetime(value).ok()?;
    let local = match (until.is_utc, tzid(start)) {
        (true, Some(zone)) => zone_to_local(root, zone, until.seconds).unwrap_or(until.seconds),
        _ => until.seconds,
    };
    Some(format_date(local))
}

/// What the event dialog shows of an object.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Detail {
    pub uid: String,
    pub fields: EventFields,
    pub is_override: bool,
    pub recurrence: Option<Recurrence>,
    pub recurrence_rule: Option<String>,
    pub organizer: Option<Participant>,
    pub attendees: Vec<Participant>,
    pub attachments: Vec<EventAttachment>,
}

/// Reads the series (`rid = None`) or one instance of an event.
pub fn detail(root: &Component, rid: Option<&str>) -> Result<Detail> {
    let master_at = master_index(root).ok_or(EditError::NotFound)?;
    let master = &root.components[master_at];
    let master_ref = master.property("RECURRENCE-ID").is_none().then_some(master);
    let (component, is_override, fields) = match rid {
        None => (master, false, fields(master, None)?),
        Some(rid) => match override_index(root, rid) {
            Some(index) => {
                let component = &root.components[index];
                (component, true, fields(component, master_ref)?)
            }
            None => {
                let formatted = master_instance_id(root, rid).ok_or(EditError::NotFound)?;
                // The instance as the master would produce it.
                let mut instance = master.clone();
                set_instance_start(&mut instance, &formatted)?;
                (master, false, fields(&instance, None)?)
            }
        },
    };
    let (recurrence, recurrence_rule) = match (master.property("RRULE"), master.property("DTSTART"))
    {
        (Some(rule), Some(start)) => match simple_rule(&rule.value, start, root) {
            Some(simple) => (Some(simple), None),
            None => (None, Some(rule.value.clone())),
        },
        _ => (None, None),
    };
    Ok(Detail {
        uid: master
            .property("UID")
            .map(|p| p.value.clone())
            .unwrap_or_default(),
        fields,
        is_override,
        recurrence,
        recurrence_rule,
        organizer: component
            .property("ORGANIZER")
            .or_else(|| master.property("ORGANIZER"))
            .map(participant),
        attendees: component
            .properties_named("ATTENDEE")
            .map(participant)
            .collect(),
        attachments: component
            .properties_named("ATTACH")
            .filter_map(attachment)
            .collect(),
    })
}

// ---------------------------------------------------------------- writing

/// Checks a TZID the UI sent: printable ASCII as IANA names use it.
fn valid_tzid(tzid: &str) -> bool {
    !tzid.is_empty()
        && tzid.len() <= MAX_TZID_LEN
        && tzid
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'/' | b'_' | b'-' | b'+'))
}

fn parse_day(date: &str) -> Result<i64> {
    let compact: String = date.split('-').collect();
    if date.len() != 10 || compact.len() != 8 {
        return Err(invalid("dates must be YYYY-MM-DD"));
    }
    match parse_date_or_datetime(&compact) {
        Ok(parsed) if parsed.is_date => Ok(parsed.seconds),
        _ => Err(invalid("dates must be YYYY-MM-DD")),
    }
}

fn parse_clock(time: &str) -> Result<i64> {
    let (hours, minutes) = time.split_once(':').ok_or(invalid("times must be HH:MM"))?;
    let number = |s: &str| {
        (s.len() == 2 && s.bytes().all(|b| b.is_ascii_digit()))
            .then(|| s.parse::<i64>().ok())
            .flatten()
    };
    match (number(hours), number(minutes)) {
        (Some(h), Some(m)) if h < 24 && m < 60 => Ok(h * 3600 + m * 60),
        _ => Err(invalid("times must be HH:MM")),
    }
}

/// What a dialog time becomes.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Written {
    /// Naive seconds (local, UTC or midnight of the date).
    naive: i64,
    kind: Kind,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Kind {
    Date,
    Floating,
    Utc,
    Zone(String),
}

fn written(time: &EventTime) -> Result<Written> {
    let day = parse_day(&time.date)?;
    let Some(clock) = &time.time else {
        return Ok(Written {
            naive: day,
            kind: Kind::Date,
        });
    };
    let naive = day + parse_clock(clock)?;
    let kind = match time.tzid.as_deref() {
        None => Kind::Floating,
        Some("UTC") => Kind::Utc,
        Some(zone) if valid_tzid(zone) => Kind::Zone(zone.to_owned()),
        Some(_) => return Err(invalid("invalid time zone")),
    };
    Ok(Written { naive, kind })
}

fn time_property(name: &str, value: &Written) -> Result<Property> {
    let (parameters, is_date, is_utc) = match &value.kind {
        Kind::Date => (vec![parameter("VALUE", "DATE")], true, false),
        Kind::Floating => (Vec::new(), false, false),
        Kind::Utc => (Vec::new(), false, true),
        Kind::Zone(zone) => (vec![parameter("TZID", zone)], false, false),
    };
    let text = format_date_or_datetime(value.naive, is_date, is_utc)
        .ok_or(invalid("date out of range"))?;
    Ok(property(name, parameters, text))
}

/// Comparable instant of a written time (local times as if UTC).
fn instant(root: &Component, value: &Written) -> i64 {
    match &value.kind {
        Kind::Zone(zone) => zone_to_utc(root, zone, value.naive).unwrap_or(value.naive),
        _ => value.naive,
    }
}

fn valid_offset(offset: &str) -> bool {
    let bytes = offset.as_bytes();
    offset.len() == 5
        && matches!(bytes[0], b'+' | b'-')
        && bytes[1..].iter().all(u8::is_ascii_digit)
        && offset[1..3].parse::<u32>().is_ok_and(|h| h <= 18)
        && offset[3..5].parse::<u32>().is_ok_and(|m| m < 60)
}

/// Builds a VTIMEZONE from the browser's definition of a zone.
fn vtimezone(info: &TimeZoneInfo) -> Result<Component> {
    let bad = invalid("invalid time zone definition");
    if !valid_tzid(&info.tzid)
        || info.observances.is_empty()
        || info.observances.len() > MAX_OBSERVANCES
    {
        return Err(bad);
    }
    let mut components = Vec::new();
    for observance in &info.observances {
        let start = parse_date_or_datetime(&observance.start).map_err(|_| bad)?;
        if start.is_date
            || start.is_utc
            || !valid_offset(&observance.offset_from)
            || !valid_offset(&observance.offset_to)
        {
            return Err(bad);
        }
        let mut properties = vec![
            property("DTSTART", Vec::new(), observance.start.clone()),
            property("TZOFFSETFROM", Vec::new(), observance.offset_from.clone()),
            property("TZOFFSETTO", Vec::new(), observance.offset_to.clone()),
        ];
        if let Some(rule) = &observance.rrule {
            let simple = rule.len() <= 200
                && rule.starts_with("FREQ=YEARLY")
                && rule
                    .bytes()
                    .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b"=;,+-".contains(&b));
            if !simple {
                return Err(bad);
            }
            properties.push(property("RRULE", Vec::new(), rule.clone()));
        }
        if let Some(name) = &observance.name {
            if name.len() > MAX_TZID_LEN {
                return Err(bad);
            }
            properties.push(property("TZNAME", Vec::new(), escape_text(name)));
        }
        components.push(Component {
            name: if observance.daylight {
                "DAYLIGHT"
            } else {
                "STANDARD"
            }
            .to_owned(),
            properties,
            components: Vec::new(),
        });
    }
    Ok(Component {
        name: "VTIMEZONE".to_owned(),
        properties: vec![property("TZID", Vec::new(), info.tzid.clone())],
        components,
    })
}

/// Makes sure `root` defines the zone `zone`: an existing VTIMEZONE is
/// kept, otherwise the UI's definition is added. Fails if neither exists
/// or the definition cannot be evaluated.
fn ensure_zone(root: &mut Component, zone: &str, infos: &[TimeZoneInfo], probe: i64) -> Result<()> {
    let defined = root
        .components
        .iter()
        .any(|c| c.name == "VTIMEZONE" && c.property("TZID").is_some_and(|p| p.value == zone));
    if !defined {
        let info = infos
            .iter()
            .find(|i| i.tzid == zone)
            .ok_or(invalid("unknown time zone"))?;
        let at = root
            .components
            .iter()
            .position(|c| c.name != "VTIMEZONE")
            .unwrap_or(root.components.len());
        root.components.insert(at, vtimezone(info)?);
    }
    if zone_to_utc(root, zone, probe).is_none() {
        return Err(invalid("the time zone cannot be evaluated"));
    }
    Ok(())
}

fn remove(component: &mut Component, names: &[&str]) {
    component
        .properties
        .retain(|p| !names.contains(&p.name.as_str()));
}

fn set(component: &mut Component, new: Property) {
    let at = component.properties.iter().position(|p| p.name == new.name);
    component.properties.retain(|p| p.name != new.name);
    let at = at.unwrap_or(component.properties.len());
    component.properties.insert(at, new);
}

fn set_text(component: &mut Component, name: &str, value: &str, max: usize) -> Result<()> {
    if value.len() > max {
        return Err(invalid("text too long"));
    }
    let value = value.trim();
    if value.is_empty() {
        remove(component, &[name]);
    } else {
        set(component, property(name, Vec::new(), escape_text(value)));
    }
    Ok(())
}

fn utc_stamp(now: i64) -> String {
    format_date_or_datetime(now, false, true).unwrap_or_default()
}

/// DTSTAMP and LAST-MODIFIED to now; SEQUENCE up for significant changes
/// (RFC 5546 §2.1.4).
fn touch(component: &mut Component, now: i64, significant: bool) {
    set(component, property("DTSTAMP", Vec::new(), utc_stamp(now)));
    set(
        component,
        property("LAST-MODIFIED", Vec::new(), utc_stamp(now)),
    );
    if significant {
        let sequence = component
            .property("SEQUENCE")
            .and_then(|p| p.value.trim().parse::<u32>().ok())
            .unwrap_or(0);
        set(
            component,
            property(
                "SEQUENCE",
                Vec::new(),
                sequence.saturating_add(1).to_string(),
            ),
        );
    }
}

fn set_alarms(component: &mut Component, alarms: &[Alarm], summary: &str) -> Result<()> {
    if alarms.len() > MAX_ALARMS {
        return Err(invalid("too many reminders"));
    }
    if alarms
        .iter()
        .any(|a| !(0..=MAX_ALARM_MINUTES).contains(&a.minutes_before))
    {
        return Err(invalid(
            "reminders must be at most four weeks before the start",
        ));
    }
    let wanted: Vec<i64> = alarms.iter().map(|a| a.minutes_before).collect();
    let current: Vec<i64> = component
        .components
        .iter()
        .filter_map(owned_alarm_minutes)
        .collect();
    if current == wanted {
        // Unchanged: keep the alarms as they are (UIDs, descriptions, …).
        return Ok(());
    }
    component
        .components
        .retain(|c| owned_alarm_minutes(c).is_none());
    let description = if summary.trim().is_empty() {
        "Reminder".to_owned()
    } else {
        escape_text(summary.trim())
    };
    for minutes in wanted {
        let trigger = if minutes == 0 {
            "PT0S".to_owned()
        } else {
            format!("-PT{minutes}M")
        };
        component.components.push(Component {
            name: "VALARM".to_owned(),
            properties: vec![
                property("ACTION", Vec::new(), "DISPLAY".to_owned()),
                property("DESCRIPTION", Vec::new(), description.clone()),
                property("TRIGGER", Vec::new(), trigger),
            ],
            components: Vec::new(),
        });
    }
    Ok(())
}

fn set_class(component: &mut Component, private: bool, inherited: bool) {
    if private {
        set(
            component,
            property("CLASS", Vec::new(), "PRIVATE".to_owned()),
        );
    } else if component.property("CLASS").is_some() || inherited {
        set(
            component,
            property("CLASS", Vec::new(), "PUBLIC".to_owned()),
        );
    }
}

/// The RRULE for a dialog rule, with UNTIL in the form RFC 5545 requires
/// for `start` (a date, UTC, or floating local time).
fn rule_text(recurrence: &Recurrence, start: &Written, root: &Component) -> Result<String> {
    let freq = match recurrence.freq {
        Frequency::Daily => "DAILY",
        Frequency::Weekly => "WEEKLY",
        Frequency::Monthly => "MONTHLY",
        Frequency::Yearly => "YEARLY",
    };
    if !(1..=MAX_INTERVAL).contains(&recurrence.interval) {
        return Err(invalid("invalid recurrence interval"));
    }
    let mut rule = format!("FREQ={freq}");
    if recurrence.interval > 1 {
        rule.push_str(&format!(";INTERVAL={}", recurrence.interval));
    }
    match (recurrence.count, &recurrence.until) {
        (Some(_), Some(_)) => return Err(invalid("use either a count or an end date")),
        (Some(count), None) => {
            if !(1..=MAX_COUNT).contains(&count) {
                return Err(invalid("invalid recurrence count"));
            }
            rule.push_str(&format!(";COUNT={count}"));
        }
        (None, Some(until)) => {
            let day = parse_day(until)?;
            if day < start.naive.div_euclid(DAY) * DAY {
                return Err(invalid("the recurrence ends before it starts"));
            }
            let last = day + DAY - 1;
            let value = match &start.kind {
                Kind::Date => format_date_or_datetime(day, true, false),
                Kind::Floating => format_date_or_datetime(last, false, false),
                Kind::Utc => format_date_or_datetime(last, false, true),
                Kind::Zone(zone) => {
                    let utc = zone_to_utc(root, zone, last).ok_or(invalid("unknown time zone"))?;
                    format_date_or_datetime(utc, false, true)
                }
            }
            .ok_or(invalid("date out of range"))?;
            rule.push_str(&format!(";UNTIL={value}"));
        }
        (None, None) => {}
    }
    if !recurrence.weekdays.is_empty() {
        if recurrence.freq != Frequency::Weekly {
            return Err(invalid("weekdays are only supported for weekly rules"));
        }
        let mut days: Vec<&str> = WEEKDAYS
            .iter()
            .copied()
            .filter(|d| recurrence.weekdays.iter().any(|w| w == d))
            .collect();
        if days.len() != recurrence.weekdays.len() {
            return Err(invalid("invalid weekday"));
        }
        days.dedup();
        rule.push_str(";BYDAY=");
        rule.push_str(&days.join(","));
    }
    Ok(rule)
}

/// Shifts a date or date-time value by `delta` seconds of local time,
/// keeping its form.
fn shift_value(value: &str, delta: i64) -> Option<String> {
    let parsed = parse_date_or_datetime(value).ok()?;
    format_date_or_datetime(
        parsed.seconds.checked_add(delta)?,
        parsed.is_date,
        parsed.is_utc,
    )
}

/// The form of a DTSTART property: date, UTC, floating or zone.
fn kind_of(property: &Property) -> Option<(i64, Kind)> {
    let parsed = parse_date_or_datetime(&property.value).ok()?;
    let kind = if parsed.is_date {
        Kind::Date
    } else if parsed.is_utc {
        Kind::Utc
    } else {
        match tzid(property) {
            Some(zone) => Kind::Zone(zone.to_owned()),
            None => Kind::Floating,
        }
    };
    Some((parsed.seconds, kind))
}

/// After the series' start changed from `old` to `new`: shift (same form)
/// or drop (other form) its EXDATEs and the overrides' RECURRENCE-IDs.
fn follow_start(root: &mut Component, master_at: usize, old: &(i64, Kind), new: &Written) {
    if old.1 == new.kind {
        let delta = new.naive - old.0;
        if delta == 0 {
            return;
        }
        let shift_all = |p: &mut Property| {
            let values: Option<Vec<String>> = split_unescaped(&p.value, ',')
                .into_iter()
                .map(|v| shift_value(v, delta))
                .collect();
            match values {
                Some(values) => {
                    p.value = values.join(",");
                    true
                }
                None => false,
            }
        };
        let master = &mut root.components[master_at];
        master
            .properties
            .retain_mut(|p| p.name != "EXDATE" || shift_all(p));
        for component in &mut root.components {
            if let Some(rid) = component
                .properties
                .iter_mut()
                .find(|p| p.name == "RECURRENCE-ID")
                && let Some(shifted) = shift_value(&rid.value, delta)
            {
                rid.value = shifted;
            }
        }
    } else {
        remove(&mut root.components[master_at], &["EXDATE"]);
        drop_overrides(root);
    }
}

fn drop_overrides(root: &mut Component) {
    root.components
        .retain(|c| !(is_event(c) && c.property("RECURRENCE-ID").is_some()));
}

/// Gives an instance copy of the master the start `rid` (formatted like
/// DTSTART) and the master's duration.
fn set_instance_start(instance: &mut Component, rid: &str) -> Result<()> {
    let start = instance.property("DTSTART").ok_or(EditError::NotFound)?;
    let old = parse_date_or_datetime(&start.value).map_err(|_| EditError::NotFound)?;
    let new = parse_date_or_datetime(rid).map_err(|_| EditError::NotFound)?;
    let delta = new.seconds - old.seconds;
    let parameters = start.parameters.clone();
    set(instance, property("DTSTART", parameters, rid.to_owned()));
    if let Some(end) = instance.properties.iter_mut().find(|p| p.name == "DTEND")
        && let Some(shifted) = shift_value(&end.value, delta)
    {
        end.value = shifted;
    }
    remove(instance, &["RRULE", "RDATE", "EXDATE"]);
    Ok(())
}

/// The component to change for `rid`: the master (`None`), an existing
/// override, or a new override created from the master.
fn target(root: &mut Component, rid: Option<&str>) -> Result<usize> {
    let master_at = master_index(root).ok_or(EditError::NotFound)?;
    let Some(rid) = rid else {
        return Ok(master_at);
    };
    if let Some(index) = override_index(root, rid) {
        return Ok(index);
    }
    let formatted = master_instance_id(root, rid).ok_or(EditError::NotFound)?;
    let overrides = root
        .components
        .iter()
        .filter(|c| is_event(c) && c.property("RECURRENCE-ID").is_some())
        .count();
    if overrides >= MAX_OVERRIDES {
        return Err(invalid("too many changed instances"));
    }
    let master = &root.components[master_at];
    let start = master.property("DTSTART").ok_or(EditError::NotFound)?;
    let mut instance = master.clone();
    // Alarms and attachments stay; the rule belongs to the master.
    instance.properties.retain(|p| p.name != "SEQUENCE");
    set_instance_start(&mut instance, &formatted)?;
    let rid_property = property("RECURRENCE-ID", start.parameters.clone(), formatted);
    let at = instance
        .properties
        .iter()
        .position(|p| p.name == "DTSTART")
        .unwrap_or(instance.properties.len());
    instance.properties.insert(at, rid_property);
    if let Some(sequence) = master.property("SEQUENCE") {
        instance.properties.push(sequence.clone());
    }
    root.components.push(instance);
    Ok(root.components.len() - 1)
}

/// Writes the dialog's start and end into `component` (index `at`).
fn set_times(
    root: &mut Component,
    at: usize,
    start: &EventTime,
    end: &EventTime,
) -> Result<(Written, bool)> {
    let start = written(start)?;
    let end = written(end)?;
    if (start.kind == Kind::Date) != (end.kind == Kind::Date) {
        return Err(invalid(
            "start and end must both be all-day or both have a time",
        ));
    }
    for value in [&start, &end] {
        if let Kind::Zone(zone) = &value.kind
            && zone_to_utc(root, zone, value.naive).is_none()
        {
            return Err(invalid("unknown time zone"));
        }
    }
    let (from, to) = (instant(root, &start), instant(root, &end));
    if to < from || (start.kind == Kind::Date && to == from) {
        return Err(invalid("the event ends before it starts"));
    }
    let component = &mut root.components[at];
    let before = (
        component.property("DTSTART").cloned(),
        component.property("DTEND").cloned(),
        component.property("DURATION").cloned(),
    );
    set(component, time_property("DTSTART", &start)?);
    remove(component, &["DTEND", "DURATION"]);
    component.properties.push(time_property("DTEND", &end)?);
    let after = (
        component.property("DTSTART").cloned(),
        component.property("DTEND").cloned(),
        None,
    );
    Ok((start, before != after))
}

/// A new event object (`VCALENDAR` with the event and the zones it uses).
pub fn new_object(input: &EventInput, uid: &str, now: i64) -> Result<Component> {
    if input.recurrence_id.is_some() {
        return Err(invalid("a new event has no instances yet"));
    }
    let mut root = Component {
        name: "VCALENDAR".to_owned(),
        properties: vec![
            property("VERSION", Vec::new(), "2.0".to_owned()),
            property(
                "PRODID",
                Vec::new(),
                format!(
                    "-//{}//{} {}//EN",
                    calcraft_core::NAME,
                    calcraft_core::NAME,
                    calcraft_core::VERSION
                ),
            ),
        ],
        components: vec![Component {
            name: "VEVENT".to_owned(),
            properties: vec![
                property("UID", Vec::new(), uid.to_owned()),
                property("DTSTAMP", Vec::new(), utc_stamp(now)),
                property("CREATED", Vec::new(), utc_stamp(now)),
            ],
            components: Vec::new(),
        }],
    };
    apply(&mut root, input, now)?;
    // A new event starts at sequence 0.
    if let Some(at) = master_index(&root) {
        remove(&mut root.components[at], &["SEQUENCE"]);
    }
    Ok(root)
}

/// Applies the dialog to the series or one instance (`input.recurrence_id`).
pub fn apply(root: &mut Component, input: &EventInput, now: i64) -> Result<()> {
    let fields = &input.fields;
    // Zones first: adding a VTIMEZONE moves the other components.
    for time in [&fields.start, &fields.end] {
        let value = written(time)?;
        if let Kind::Zone(zone) = &value.kind {
            ensure_zone(root, zone, &input.timezones, value.naive)?;
        }
    }
    let master_at = master_index(root).ok_or(EditError::NotFound)?;
    let old_start = root.components[master_at]
        .property("DTSTART")
        .and_then(kind_of);
    let master_private = is_private(&root.components[master_at], None);
    let at = target(root, input.recurrence_id.as_deref())?;
    let (start, times_changed) = set_times(root, at, &fields.start, &fields.end)?;
    let component = &mut root.components[at];
    set_text(component, "SUMMARY", &fields.summary, MAX_SHORT_TEXT)?;
    set_text(component, "LOCATION", &fields.location, MAX_SHORT_TEXT)?;
    set_text(
        component,
        "DESCRIPTION",
        &fields.description,
        MAX_DESCRIPTION,
    )?;
    let inherited = at != master_at && master_private;
    set_class(component, fields.private, inherited);
    set_alarms(component, &fields.alarms, &fields.summary)?;
    let mut significant = times_changed;
    if at == master_at && !input.keep_recurrence {
        let rule = input
            .recurrence
            .as_ref()
            .map(|r| rule_text(r, &start, root))
            .transpose()?;
        let master = &mut root.components[master_at];
        let current = master.property("RRULE").map(|p| p.value.clone());
        if current != rule {
            significant = true;
        }
        match rule {
            Some(rule) => set(master, property("RRULE", Vec::new(), rule)),
            None => {
                if master.property("RDATE").is_some() || master.property("EXDATE").is_some() {
                    significant = true;
                }
                remove(master, &["RRULE", "RDATE", "EXDATE"]);
                drop_overrides(root);
            }
        }
    }
    if at == master_at
        && let Some(old) = old_start
    {
        follow_start(root, master_at, &old, &start);
    }
    touch(&mut root.components[at], now, significant);
    Ok(())
}

/// A start or end from drag and drop, in the form of the component's
/// DTSTART: numbers are UTC for times in a zone that can be evaluated (or
/// UTC times), local seconds otherwise.
fn moved(root: &Component, start: &Property, seconds: i64) -> Result<Written> {
    let (_, kind) = kind_of(start).ok_or(EditError::NotFound)?;
    let naive = match &kind {
        Kind::Date => {
            if seconds.rem_euclid(DAY) != 0 {
                return Err(invalid("all-day events move by whole days"));
            }
            seconds
        }
        Kind::Zone(zone) => match zone_to_local(root, zone, seconds) {
            Some(local) => local,
            None if zone_to_utc(root, zone, seconds).is_none() => seconds,
            None => return Err(invalid("the time zone cannot be evaluated")),
        },
        Kind::Floating | Kind::Utc => seconds,
    };
    Ok(Written { naive, kind })
}

/// Moves or resizes the series or one instance (drag and drop).
pub fn apply_times(root: &mut Component, times: &EventTimes, now: i64) -> Result<()> {
    let master_at = master_index(root).ok_or(EditError::NotFound)?;
    let old_start = root.components[master_at]
        .property("DTSTART")
        .and_then(kind_of);
    let at = target(root, times.recurrence_id.as_deref())?;
    let start_property = root.components[at]
        .property("DTSTART")
        .cloned()
        .ok_or(EditError::NotFound)?;
    let start = moved(root, &start_property, times.start)?;
    let end = moved(root, &start_property, times.end)?;
    if end.naive < start.naive || (start.kind == Kind::Date && end.naive == start.naive) {
        return Err(invalid("the event ends before it starts"));
    }
    let component = &mut root.components[at];
    set(component, time_property("DTSTART", &start)?);
    remove(component, &["DTEND", "DURATION"]);
    component.properties.push(time_property("DTEND", &end)?);
    if at == master_at
        && let Some(old) = old_start
    {
        follow_start(root, master_at, &old, &start);
    }
    touch(&mut root.components[at], now, true);
    Ok(())
}

/// Deletes one instance of a series: an EXDATE on the master, and its
/// override (if any) removed.
pub fn exclude(root: &mut Component, rid: &str, now: i64) -> Result<()> {
    let master_at = master_index(root).ok_or(EditError::NotFound)?;
    let index = override_index(root, rid);
    let formatted = master_instance_id(root, rid);
    if index.is_none() && formatted.is_none() {
        return Err(EditError::NotFound);
    }
    if root.components[master_at]
        .property("RECURRENCE-ID")
        .is_some()
    {
        // Only overrides, no master: remove the one instance.
        let index = index.ok_or(EditError::NotFound)?;
        root.components.remove(index);
        if !root.components.iter().any(is_event) {
            return Err(invalid("this is the last instance: delete the event"));
        }
        return Ok(());
    }
    let master = &root.components[master_at];
    let start = master.property("DTSTART").ok_or(EditError::NotFound)?;
    let value = match (&formatted, index) {
        (Some(formatted), _) => formatted.clone(),
        // An override of an instance the rule no longer has (or an
        // RDATE): exclude its RECURRENCE-ID as written.
        (None, Some(index)) => root.components[index]
            .property("RECURRENCE-ID")
            .map(|p| p.value.clone())
            .ok_or(EditError::NotFound)?,
        (None, None) => return Err(EditError::NotFound),
    };
    let exdate = property("EXDATE", start.parameters.clone(), value);
    if let Some(index) = index {
        root.components.remove(index);
    }
    let master_at = master_index(root).ok_or(EditError::NotFound)?;
    let master = &mut root.components[master_at];
    master.properties.push(exdate);
    touch(master, now, true);
    Ok(())
}

#[cfg(test)]
mod tests;
