//! Text of reminder mails: plain text only (no HTML, no images, no
//! tracking), the description only when the user opted in.

use std::fmt::Write as _;

use calcraft_core::mail::{civil_from_days, month_name, single_line, truncate_chars, weekday_name};
use calcraft_ical::{
    Component,
    alarm::{AlarmOccurrence, Instance},
    write::unescape_text,
};

/// Characters of a summary or location kept.
const MAX_FIELD: usize = 200;
/// Characters of a description kept.
const MAX_DESCRIPTION: usize = 2000;
/// Characters of a time zone label kept.
const MAX_ZONE: usize = 64;

/// What a reminder mail says.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Content {
    pub subject: String,
    pub body: String,
}

fn text_property(component: &Component, name: &str, max: usize) -> Option<String> {
    let value = component.property(name)?;
    let text = truncate_chars(&single_line(&unescape_text(&value.value)), max);
    (!text.is_empty()).then_some(text)
}

/// A description: line breaks kept, other control characters removed.
fn description(component: &Component) -> Option<String> {
    let raw = unescape_text(&component.property("DESCRIPTION")?.value);
    let cleaned: String = raw
        .replace("\r\n", "\n")
        .chars()
        .map(|c| if c == '\n' || !c.is_control() { c } else { ' ' })
        .collect();
    let lines: Vec<String> = cleaned.lines().map(single_line).collect();
    let text = truncate_chars(lines.join("\n").trim(), MAX_DESCRIPTION);
    (!text.is_empty()).then_some(text)
}

/// The zone label of the instance's times: the TZID, `UTC`, or nothing
/// for floating times and dates.
fn zone_label(component: &Component) -> Option<String> {
    let start = component
        .property("DTSTART")
        .or_else(|| component.property("DUE"))?;
    if let Some(tzid) = start.parameter("TZID").and_then(|p| p.values.first()) {
        let label = truncate_chars(&single_line(tzid), MAX_ZONE);
        return (!label.is_empty()).then_some(label);
    }
    start.value.ends_with('Z').then(|| "UTC".to_owned())
}

fn date(naive: i64) -> String {
    let days = naive.div_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    format!("{}, {day} {} {year}", weekday_name(days), month_name(month))
}

fn time(naive: i64) -> String {
    let secs = naive.rem_euclid(86_400);
    format!("{:02}:{:02}", secs / 3600, secs / 60 % 60)
}

/// `Mon, 26 Oct 2026, 09:00–10:00 (Europe/Berlin)` and similar.
pub fn when(instance: &Instance, zone: Option<&str>, todo: bool) -> String {
    let suffix = zone.map(|z| format!(" ({z})")).unwrap_or_default();
    if todo {
        let due = instance.local_end;
        return if instance.is_date {
            date(due)
        } else {
            format!("{}, {}{suffix}", date(due), time(due))
        };
    }
    let (start, end) = (instance.local_start, instance.local_end);
    if instance.is_date {
        // DTEND of all-day events is exclusive.
        let last = if end > start { end - 86_400 } else { start };
        return if last.div_euclid(86_400) == start.div_euclid(86_400) {
            format!("{} (all day)", date(start))
        } else {
            format!("{} – {} (all day)", date(start), date(last))
        };
    }
    if end <= start {
        format!("{}, {}{suffix}", date(start), time(start))
    } else if end.div_euclid(86_400) == start.div_euclid(86_400) {
        format!("{}, {}–{}{suffix}", date(start), time(start), time(end))
    } else {
        format!(
            "{}, {} – {}, {}{suffix}",
            date(start),
            time(start),
            date(end),
            time(end)
        )
    }
}

/// The mail for one alarm of `root`.
pub fn reminder(
    root: &Component,
    occurrence: &AlarmOccurrence,
    calendar: &str,
    include_description: bool,
    base_url: Option<&str>,
) -> Content {
    let component = root.components.get(occurrence.component);
    let todo = occurrence.kind == calcraft_ical::ComponentKind::Todo;
    let summary = component
        .and_then(|c| text_property(c, "SUMMARY", MAX_FIELD))
        .unwrap_or_else(|| "(no title)".to_owned());
    let zone = component.and_then(zone_label);
    let mut body = String::new();
    let _ = writeln!(body, "{summary}\n");
    let label = if todo { "Due" } else { "When" };
    let _ = writeln!(
        body,
        "{label}: {}",
        when(&occurrence.instance, zone.as_deref(), todo)
    );
    if let Some(location) = component.and_then(|c| text_property(c, "LOCATION", MAX_FIELD)) {
        let _ = writeln!(body, "Location: {location}");
    }
    let calendar = truncate_chars(&single_line(calendar), MAX_FIELD);
    let _ = writeln!(body, "Calendar: {calendar}");
    if include_description && let Some(text) = component.and_then(description) {
        let _ = write!(body, "\n{text}\n");
    }
    body.push_str("\n-- \nThis reminder was sent by CalCraft for an alarm in your calendar.\n");
    if let Some(base) = base_url {
        let _ = writeln!(body, "Open your calendar: {base}/");
    }
    body.push_str("You can turn reminder e-mails off in your account settings.\n");
    Content {
        subject: format!("Reminder: {summary}"),
        body,
    }
}

/// The test mail of `calcraft smtp test` and `POST /api/v1/me/reminders/test`.
pub fn test_mail(base_url: Option<&str>) -> Content {
    let mut body = String::from(
        "This is a test message from CalCraft.\n\n\
         If you can read it, reminder e-mails for your calendar alarms reach you.\n",
    );
    if let Some(base) = base_url {
        let _ = writeln!(body, "\nOpen your calendar: {base}/");
    }
    Content {
        subject: "CalCraft test message".to_owned(),
        body,
    }
}
