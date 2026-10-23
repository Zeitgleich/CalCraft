//! Calendar views of the web UI: the calendars a user sees, the instances
//! of their events in a time range, and events to show, create and change
//! (`/api/v1/calendars`, `/api/v1/events`).
//!
//! Times of instances use one compact encoding: `start`/`end` are Unix
//! seconds, exact UTC instants unless `all_day` (midnight of the date, read
//! as if UTC) or `floating` (local wall time, read as if UTC) is set.

use serde::{Deserialize, Serialize};

/// Where a calendar comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CalendarSource {
    Own,
    /// Shared by another user (and accepted).
    Shared,
    /// A subscription to a feed (read-only).
    Subscription,
}

/// A calendar of the user's home (`GET /api/v1/calendars`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CalendarView {
    /// Slug in the user's own home (for shares: the sharee's slug).
    pub slug: String,
    pub display_name: Option<String>,
    pub color: Option<String>,
    pub source: CalendarSource,
    /// Events can be created, changed and deleted.
    pub writable: bool,
    /// Username of the owner of a shared calendar.
    #[serde(default)]
    pub owner: Option<String>,
    /// A read-only share: private events are shown as busy blocks.
    #[serde(default)]
    pub private_hidden: bool,
}

/// One instance of an event (`GET /api/v1/events`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EventInstance {
    /// Slug of the calendar in the user's home.
    pub calendar: String,
    /// Object name (`….ics`).
    pub name: String,
    pub etag: String,
    /// Names the instance of a recurring event; `None` for single events.
    #[serde(default)]
    pub recurrence_id: Option<String>,
    #[serde(default)]
    pub summary: Option<String>,
    #[serde(default)]
    pub location: Option<String>,
    pub all_day: bool,
    #[serde(default)]
    pub floating: bool,
    pub start: i64,
    /// Exclusive end.
    pub end: i64,
    /// A busy block of a private event in a read-only share.
    #[serde(default)]
    pub busy: bool,
    #[serde(default)]
    pub cancelled: bool,
    /// Part of a recurring series.
    #[serde(default)]
    pub recurring: bool,
    /// The instance can be changed (calendar writable, no busy block).
    #[serde(default)]
    pub writable: bool,
}

/// `GET /api/v1/events?start=…&end=…[&calendar=…]`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct EventList {
    pub instances: Vec<EventInstance>,
    /// Limits were reached: some instances are missing.
    #[serde(default)]
    pub truncated: bool,
}

/// A start or end in the event dialog.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EventTime {
    /// `YYYY-MM-DD`.
    pub date: String,
    /// `HH:MM`; `None` for all-day events.
    #[serde(default)]
    pub time: Option<String>,
    /// IANA zone such as `Europe/Berlin`, `UTC`, or `None` for floating time.
    #[serde(default)]
    pub tzid: Option<String>,
}

/// Frequency of a simple recurrence rule.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Frequency {
    Daily,
    Weekly,
    Monthly,
    Yearly,
}

/// A recurrence rule the dialog can edit (`RRULE` with FREQ, INTERVAL,
/// COUNT or UNTIL and, for weekly rules, BYDAY).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Recurrence {
    pub freq: Frequency,
    #[serde(default = "one")]
    pub interval: u32,
    #[serde(default)]
    pub count: Option<u32>,
    /// Last day (inclusive), `YYYY-MM-DD`.
    #[serde(default)]
    pub until: Option<String>,
    /// `MO` … `SU`, for weekly rules.
    #[serde(default)]
    pub weekdays: Vec<String>,
}

fn one() -> u32 {
    1
}

/// A reminder (`VALARM` with `ACTION:DISPLAY`) before the start.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Alarm {
    pub minutes_before: i64,
}

/// Organizer or attendee of a meeting (display only).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Participant {
    /// Calendar user address without `mailto:`.
    pub address: String,
    #[serde(default)]
    pub name: Option<String>,
    /// `PARTSTAT` (`ACCEPTED`, `NEEDS-ACTION`, …).
    #[serde(default)]
    pub status: Option<String>,
    /// `ROLE` (`REQ-PARTICIPANT`, …).
    #[serde(default)]
    pub role: Option<String>,
}

/// An attachment of an event.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EventAttachment {
    #[serde(default)]
    pub name: Option<String>,
    /// Download URL: `/api/v1/attachments/<id>` for managed attachments,
    /// otherwise the `http(s)` link of the event.
    pub url: String,
    #[serde(default)]
    pub size: Option<i64>,
    pub managed: bool,
}

/// The editable fields of an event (series or one instance).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EventFields {
    #[serde(default)]
    pub summary: String,
    #[serde(default)]
    pub location: String,
    #[serde(default)]
    pub description: String,
    pub start: EventTime,
    pub end: EventTime,
    #[serde(default)]
    pub alarms: Vec<Alarm>,
    /// `CLASS:PRIVATE`: read-only sharees only see a busy block.
    #[serde(default)]
    pub private: bool,
}

/// `GET /api/v1/calendars/{slug}/events/{name}[?recurrence_id=…]`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EventDetail {
    pub calendar: String,
    pub name: String,
    pub etag: String,
    pub uid: String,
    pub writable: bool,
    /// A busy block of a private event in a read-only share.
    #[serde(default)]
    pub busy: bool,
    /// Of the series, or of the instance named by `recurrence_id`.
    pub fields: EventFields,
    /// Set when one instance was requested.
    #[serde(default)]
    pub recurrence_id: Option<String>,
    /// The requested instance is changed from the series (an override).
    #[serde(default)]
    pub is_override: bool,
    /// The series' rule if the dialog can edit it.
    #[serde(default)]
    pub recurrence: Option<Recurrence>,
    /// The series' `RRULE` text if it cannot be edited here (kept as is).
    #[serde(default)]
    pub recurrence_rule: Option<String>,
    #[serde(default)]
    pub organizer: Option<Participant>,
    #[serde(default)]
    pub attendees: Vec<Participant>,
    #[serde(default)]
    pub attachments: Vec<EventAttachment>,
}

/// One STANDARD or DAYLIGHT observance of a time zone, as the browser
/// computed it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Observance {
    pub daylight: bool,
    /// First onset in local time before the change, `YYYYMMDDTHHMMSS`.
    pub start: String,
    /// `+HHMM` / `-HHMM`.
    pub offset_from: String,
    pub offset_to: String,
    /// Yearly rule such as `FREQ=YEARLY;BYMONTH=3;BYDAY=-1SU`.
    #[serde(default)]
    pub rrule: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
}

/// Definition of a time zone the event uses (becomes a `VTIMEZONE` if the
/// object has none for it yet).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TimeZoneInfo {
    pub tzid: String,
    pub observances: Vec<Observance>,
}

/// `POST /api/v1/calendars/{slug}/events` and
/// `PUT /api/v1/calendars/{slug}/events/{name}` (with `If-Match`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EventInput {
    /// Change only this instance of a series (creates or updates an
    /// override); `None` changes the series or the single event.
    #[serde(default)]
    pub recurrence_id: Option<String>,
    pub fields: EventFields,
    /// The series' new rule; `None` makes the event non-recurring unless
    /// `keep_recurrence` is set. Ignored for one instance.
    #[serde(default)]
    pub recurrence: Option<Recurrence>,
    /// Keep the series' rule as it is (e.g. a rule the dialog cannot edit).
    #[serde(default)]
    pub keep_recurrence: bool,
    #[serde(default)]
    pub timezones: Vec<TimeZoneInfo>,
}

/// `PATCH /api/v1/calendars/{slug}/events/{name}` (with `If-Match`): new
/// start and end of the series or one instance, in the encoding of
/// [`EventInstance`] (drag and drop, resize).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EventTimes {
    #[serde(default)]
    pub recurrence_id: Option<String>,
    pub start: i64,
    pub end: i64,
}

/// `POST /api/v1/calendars/{slug}/events/{name}/move` (with `If-Match`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MoveEvent {
    /// Slug of the target calendar (of the same owner).
    pub calendar: String,
}

/// A stored event.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EventSaved {
    pub calendar: String,
    pub name: String,
    /// `None` if the object changed again in the meantime.
    #[serde(default)]
    pub etag: Option<String>,
}
