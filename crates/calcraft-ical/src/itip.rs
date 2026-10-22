//! iTIP (RFC 5546) building blocks for CalDAV server-side scheduling
//! (RFC 6638): who organizes and attends an object, what each attendee
//! sees, which changes matter to them, and how replies are merged into the
//! organizer's copy.
//!
//! Everything works on parsed component trees and never does I/O. Calendar
//! user addresses are compared through the caller's matcher functions
//! (the server knows which addresses belong to which user); values are
//! never interpreted here beyond that.
//!
//! Merging a reply is the security-relevant part: it may change only the
//! `PARTSTAT` of the replying attendee's own `ATTENDEE` properties in the
//! organizer's copy, and may add an override only for an instance the
//! organizer's series really has (checked with the recurrence engine,
//! within a budget).

use crate::{
    Component, Parameter, Property,
    datetime::{format_date_or_datetime, parse_date_or_datetime},
    recurrence::{self, Budget, InstantKey, InstantKeys, OBJECT_BUDGET},
    write::write_property,
};

/// Request status codes used in `SCHEDULE-STATUS` (RFC 6638 §3.2.9) and
/// `CALDAV:request-status`.
pub mod status {
    /// The message was delivered.
    pub const DELIVERED: &str = "1.2";
    /// Free/busy lookup succeeded.
    pub const SUCCESS: &str = "2.0;Success";
    /// Unknown calendar user.
    pub const INVALID_USER: &str = "3.7";
    /// Not allowed (e.g. a reply to an event the attendee is not invited to).
    pub const NO_AUTHORITY: &str = "3.8";
    /// Delivery failed (quota, limits, rate limit, storage error).
    pub const NOT_DELIVERED: &str = "5.1";
    /// The recipient cannot be scheduled by this server (stage A: any
    /// address outside this server).
    pub const NO_SCHEDULING: &str = "5.3";
}

/// `SCHEDULE-AGENT` of an `ORGANIZER` or `ATTENDEE` (RFC 6638 §7.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Agent {
    /// The server schedules (the default).
    Server,
    /// The client schedules itself.
    Client,
    /// Nobody schedules.
    None,
}

/// The `SCHEDULE-AGENT` parameter. Unknown values are treated like `NONE`:
/// the server does not send messages it was not clearly asked to send.
pub fn agent(property: &Property) -> Agent {
    match property
        .parameter("SCHEDULE-AGENT")
        .and_then(|p| p.values.first())
    {
        None => Agent::Server,
        Some(v) if v.eq_ignore_ascii_case("SERVER") => Agent::Server,
        Some(v) if v.eq_ignore_ascii_case("CLIENT") => Agent::Client,
        Some(_) => Agent::None,
    }
}

/// Why an object cannot be scheduled (RFC 6638 §3.2.4 preconditions).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ItipError {
    /// The components disagree about the organizer
    /// (`CALDAV:same-organizer-in-all-components`).
    DifferentOrganizers,
}

/// Whether a component is an instance component (VEVENT, VTODO, VJOURNAL).
fn is_instance(component: &Component) -> bool {
    matches!(component.name.as_str(), "VEVENT" | "VTODO" | "VJOURNAL")
}

fn instances(calendar: &Component) -> impl Iterator<Item = &Component> {
    calendar.components.iter().filter(|c| is_instance(c))
}

/// The `ORGANIZER` of a calendar object: the same in every instance
/// component, or in none.
pub fn organizer(calendar: &Component) -> Result<Option<&Property>, ItipError> {
    let mut first: Option<Option<&Property>> = None;
    for component in instances(calendar) {
        let this = component.property("ORGANIZER");
        match first {
            None => first = Some(this),
            Some(previous) => {
                if previous.map(|p| p.value.as_str()) != this.map(|p| p.value.as_str()) {
                    return Err(ItipError::DifferentOrganizers);
                }
            }
        }
    }
    Ok(first.flatten())
}

/// The UID of the object's instance components.
pub fn uid(calendar: &Component) -> Option<&str> {
    instances(calendar)
        .find_map(|c| c.property("UID"))
        .map(|p| p.value.as_str())
}

/// Every `ATTENDEE` property of every instance component.
pub fn attendees(calendar: &Component) -> impl Iterator<Item = &Property> {
    instances(calendar).flat_map(|c| c.properties_named("ATTENDEE"))
}

fn param<'a>(property: &'a Property, name: &str) -> Option<&'a str> {
    property
        .parameter(name)
        .and_then(|p| p.values.first())
        .map(String::as_str)
}

/// Sets (in place, keeping the order) or removes a parameter.
fn set_param(property: &mut Property, name: &str, value: Option<&str>) {
    let Some(value) = value else {
        property.parameters.retain(|p| p.name != name);
        return;
    };
    let mut found = false;
    property.parameters.retain_mut(|p| {
        if p.name != name {
            return true;
        }
        if found {
            return false;
        }
        found = true;
        p.values = vec![value.to_owned()];
        true
    });
    if !found {
        property.parameters.push(Parameter {
            name: name.to_owned(),
            values: vec![value.to_owned()],
        });
    }
}

/// `PARTSTAT` of the attendee in a component (`NEEDS-ACTION` if the
/// parameter is missing), `None` if the attendee is not listed.
pub fn partstat(component: &Component, is_attendee: &dyn Fn(&str) -> bool) -> Option<String> {
    component
        .properties_named("ATTENDEE")
        .find(|p| is_attendee(&p.value))
        .map(|p| {
            param(p, "PARTSTAT")
                .unwrap_or("NEEDS-ACTION")
                .to_ascii_uppercase()
        })
}

/// A `PARTSTAT` value worth copying: a short token (RFC 5545 iana-token or
/// x-name).
fn valid_partstat(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 32
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-')
}

/// Sets `name` on every `kind` property (ATTENDEE or ORGANIZER) whose value
/// matches, in every instance component; `None` removes it.
pub fn set_parameter(
    calendar: &mut Component,
    kind: &str,
    matches: &dyn Fn(&str) -> bool,
    name: &str,
    value: Option<&str>,
) {
    for component in calendar.components.iter_mut().filter(|c| is_instance(c)) {
        for property in &mut component.properties {
            if property.name == kind && matches(&property.value) {
                set_param(property, name, value);
            }
        }
    }
}

/// Whether a matching `kind` property carries `SCHEDULE-FORCE-SEND=value`.
pub fn force_send(
    calendar: &Component,
    kind: &str,
    matches: &dyn Fn(&str) -> bool,
    value: &str,
) -> bool {
    instances(calendar)
        .flat_map(|c| c.properties_named(kind))
        .any(|p| {
            matches(&p.value)
                && param(p, "SCHEDULE-FORCE-SEND").is_some_and(|v| v.eq_ignore_ascii_case(value))
        })
}

/// Removes the parameters only clients send to request something of the
/// server (`SCHEDULE-FORCE-SEND`, RFC 6638 §7.3) from ORGANIZER and
/// ATTENDEE properties; they are never stored.
pub fn strip_requests(calendar: &mut Component) {
    for component in calendar.components.iter_mut().filter(|c| is_instance(c)) {
        for property in &mut component.properties {
            if property.name == "ATTENDEE" || property.name == "ORGANIZER" {
                property
                    .parameters
                    .retain(|p| p.name != "SCHEDULE-FORCE-SEND");
            }
        }
    }
}

/// Copies the `SCHEDULE-STATUS` of every ORGANIZER/ATTENDEE property that
/// matches `keep` from `old` (same property value, any component) and
/// removes it from all others: clients cannot set it (RFC 6638 §3.2.9),
/// except for attendees they schedule themselves.
pub fn restore_statuses(
    calendar: &mut Component,
    old: Option<&Component>,
    keep: &dyn Fn(&Property) -> bool,
) {
    let previous = |kind: &str, value: &str| -> Option<String> {
        old.and_then(|old| {
            instances(old)
                .flat_map(|c| c.properties_named(kind))
                .find(|p| p.value == value)
                .and_then(|p| param(p, "SCHEDULE-STATUS"))
                .map(str::to_owned)
        })
    };
    for component in calendar.components.iter_mut().filter(|c| is_instance(c)) {
        for property in &mut component.properties {
            if property.name != "ATTENDEE" && property.name != "ORGANIZER" {
                continue;
            }
            if keep(property) {
                continue;
            }
            let value = previous(&property.name, &property.value);
            set_param(property, "SCHEDULE-STATUS", value.as_deref());
        }
    }
}

/// Removes `VALARM`s and server-managed parameters from a component.
fn cleaned(component: &Component) -> Component {
    let mut component = component.clone();
    component.components.retain(|c| c.name != "VALARM");
    for property in &mut component.properties {
        if property.name == "ATTENDEE" || property.name == "ORGANIZER" {
            property
                .parameters
                .retain(|p| p.name != "SCHEDULE-STATUS" && p.name != "SCHEDULE-FORCE-SEND");
        }
    }
    component
}

/// What one attendee sees of the organizer's object: the components that
/// list them, with instances they are not invited to excluded from the
/// master (EXDATE), without alarms and server-managed parameters. `None`
/// if the attendee is not listed at all.
pub fn project(calendar: &Component, is_attendee: &dyn Fn(&str) -> bool) -> Option<Component> {
    let listed = |c: &Component| {
        c.properties_named("ATTENDEE")
            .any(|p| is_attendee(&p.value))
    };
    let master_listed = instances(calendar)
        .find(|c| c.property("RECURRENCE-ID").is_none())
        .is_some_and(listed);
    // Overrides that do not list the attendee are excluded instances.
    let excluded: Vec<Property> = instances(calendar)
        .filter(|c| master_listed && !listed(c))
        .filter_map(|c| c.property("RECURRENCE-ID"))
        .map(|rid| Property {
            name: "EXDATE".to_owned(),
            parameters: rid
                .parameters
                .iter()
                .filter(|p| p.name == "TZID" || p.name == "VALUE")
                .cloned()
                .collect(),
            value: rid.value.clone(),
        })
        .collect();
    let mut out = Component {
        name: calendar.name.clone(),
        properties: calendar
            .properties
            .iter()
            .filter(|p| p.name != "METHOD")
            .cloned()
            .collect(),
        components: Vec::new(),
    };
    let mut any = false;
    for component in &calendar.components {
        if component.name == "VTIMEZONE" {
            out.components.push(component.clone());
            continue;
        }
        if !is_instance(component) || !listed(component) {
            continue;
        }
        let mut copy = cleaned(component);
        if copy.property("RECURRENCE-ID").is_none() {
            copy.properties.extend(excluded.iter().cloned());
        }
        out.components.push(copy);
        any = true;
    }
    any.then_some(out)
}

/// Properties left out when deciding whether a change matters to an
/// attendee: timestamps and sequence numbers, vendor extensions, and the
/// participation status of attendees (replies are no reason to send a new
/// invitation).
fn canonical(calendar: &Component) -> Vec<(String, Vec<String>)> {
    let mut components: Vec<(String, Vec<String>)> = instances(calendar)
        .map(|component| {
            let rid = component
                .property("RECURRENCE-ID")
                .map(|p| p.value.clone())
                .unwrap_or_default();
            let mut lines: Vec<String> = component
                .properties
                .iter()
                .filter(|p| {
                    !matches!(
                        p.name.as_str(),
                        "DTSTAMP" | "LAST-MODIFIED" | "CREATED" | "SEQUENCE"
                    ) && !p.name.starts_with("X-")
                })
                .map(|p| {
                    let mut p = p.clone();
                    if p.name == "ATTENDEE" || p.name == "ORGANIZER" {
                        p.parameters.retain(|param| {
                            !matches!(
                                param.name.as_str(),
                                "PARTSTAT" | "RSVP" | "SCHEDULE-STATUS" | "SCHEDULE-FORCE-SEND"
                            ) && !param.name.starts_with("X-")
                        });
                    }
                    let mut line = String::new();
                    // Values were valid when parsed; a failure only means
                    // "different".
                    if write_property(&p, &mut line).is_err() {
                        line = format!("{p:?}");
                    }
                    line
                })
                .collect();
            lines.sort();
            (rid, lines)
        })
        .collect();
    components.sort();
    components
}

/// Whether two projections ([`project`]) are the same for the attendee.
pub fn same_for_attendee(a: &Component, b: &Component) -> bool {
    canonical(a) == canonical(b)
}

/// An iTIP message: the components of `body` in a VCALENDAR with
/// `METHOD`, every instance component stamped with `dtstamp` (UTC).
pub fn message(method: &str, body: &Component, dtstamp: &str) -> Component {
    let mut components = body.components.clone();
    for component in components.iter_mut().filter(|c| is_instance(c)) {
        component.properties.retain(|p| p.name != "DTSTAMP");
        component.properties.push(simple("DTSTAMP", dtstamp));
    }
    Component {
        name: "VCALENDAR".to_owned(),
        properties: vec![
            simple("VERSION", "2.0"),
            simple("PRODID", "-//CalCraft//Scheduling//EN"),
            simple("METHOD", method),
        ],
        components,
    }
}

fn simple(name: &str, value: &str) -> Property {
    Property {
        name: name.to_owned(),
        parameters: Vec::new(),
        value: value.to_owned(),
    }
}

/// The object with every instance component marked `STATUS:CANCELLED`.
pub fn cancelled(calendar: &Component) -> Component {
    let mut calendar = calendar.clone();
    for component in calendar.components.iter_mut().filter(|c| is_instance(c)) {
        component.properties.retain(|p| p.name != "STATUS");
        component.properties.push(simple("STATUS", "CANCELLED"));
    }
    calendar
}

/// Whether every instance component is cancelled.
pub fn is_cancelled(calendar: &Component) -> bool {
    instances(calendar).all(|c| {
        c.property("STATUS")
            .is_some_and(|p| p.value.eq_ignore_ascii_case("CANCELLED"))
    })
}

/// The attendee's new copy: the organizer's projection, keeping the
/// attendee's own alarms of the components that still exist.
pub fn attendee_copy(projection: &Component, old: Option<&Component>) -> Component {
    let mut copy = projection.clone();
    let Some(old) = old else {
        return copy;
    };
    for component in copy.components.iter_mut().filter(|c| is_instance(c)) {
        let rid = component.property("RECURRENCE-ID").map(|p| p.value.clone());
        let previous = instances(old).find(|c| {
            c.name == component.name && c.property("RECURRENCE-ID").map(|p| p.value.clone()) == rid
        });
        if let Some(previous) = previous {
            component.components.extend(
                previous
                    .components
                    .iter()
                    .filter(|c| c.name == "VALARM")
                    .cloned(),
            );
        }
    }
    copy
}

/// Keeps the participation status the server recorded for attendees it
/// schedules (`keep(value)`) when the organizer's client writes an older
/// one: replies change the organizer's copy without changing its
/// Schedule-Tag, so a client using `If-Schedule-Tag-Match` may not have
/// seen them (RFC 6638 §3.2.10). With `reset(value)` the organizer's client
/// may set the attendee back to `NEEDS-ACTION` (a rescheduled event).
pub fn preserve_partstats(
    calendar: &mut Component,
    old: &Component,
    keep: &dyn Fn(&str) -> bool,
    reset: &dyn Fn(&str) -> bool,
) {
    let old_master = instances(old).find(|c| c.property("RECURRENCE-ID").is_none());
    for component in calendar.components.iter_mut().filter(|c| is_instance(c)) {
        let rid = component.property("RECURRENCE-ID").map(|p| p.value.clone());
        let previous = instances(old)
            .find(|c| c.property("RECURRENCE-ID").map(|p| p.value.clone()) == rid)
            .or(old_master);
        let Some(previous) = previous else { continue };
        for property in &mut component.properties {
            if property.name != "ATTENDEE" || !keep(&property.value) {
                continue;
            }
            let Some(old_property) = previous
                .properties_named("ATTENDEE")
                .find(|p| p.value == property.value)
            else {
                continue;
            };
            let new = param(property, "PARTSTAT").unwrap_or("NEEDS-ACTION");
            let before = param(old_property, "PARTSTAT").unwrap_or("NEEDS-ACTION");
            if new.eq_ignore_ascii_case(before)
                || (new.eq_ignore_ascii_case("NEEDS-ACTION") && reset(&property.value))
            {
                continue;
            }
            let before = before.to_owned();
            set_param(property, "PARTSTAT", Some(&before));
        }
    }
}

/// One participation status an attendee's copy reports for an instance
/// (`None`: the master).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reply {
    instance: Option<InstantKey>,
    pub partstat: String,
}

impl Reply {
    /// Whether the reply is about the whole series (the master).
    pub fn is_master(&self) -> bool {
        self.instance.is_none()
    }
}

/// The attendee's participation statuses in their copy: one per component
/// that lists them, plus `DECLINED` for every instance their client
/// excluded with an EXDATE the organizer's copy does not have.
pub fn replies(
    attendee_copy: &Component,
    organizer_copy: &Component,
    is_attendee: &dyn Fn(&str) -> bool,
) -> Vec<Reply> {
    let mut keys = InstantKeys::new(attendee_copy);
    let mut out = Vec::new();
    for component in instances(attendee_copy) {
        let Some(status) = partstat(component, is_attendee) else {
            continue;
        };
        if !valid_partstat(&status) {
            continue;
        }
        let instance = match component.property("RECURRENCE-ID") {
            Some(rid) => match keys.key(rid, &rid.value) {
                Some(key) => Some(key),
                None => continue,
            },
            None => None,
        };
        out.push(Reply {
            instance,
            partstat: status,
        });
    }
    let master = instances(attendee_copy).find(|c| c.property("RECURRENCE-ID").is_none());
    let organizer_master =
        instances(organizer_copy).find(|c| c.property("RECURRENCE-ID").is_none());
    if let (Some(master), Some(organizer_master)) = (master, organizer_master) {
        let mut organizer_keys = InstantKeys::new(organizer_copy);
        let mut known = Vec::new();
        for property in organizer_master.properties_named("EXDATE") {
            for raw in property.value.split(',') {
                if let Some(key) = organizer_keys.key(property, raw) {
                    known.push(key);
                }
            }
        }
        for property in master.properties_named("EXDATE") {
            for raw in property.value.split(',') {
                if let Some(key) = keys.key(property, raw)
                    && !known.contains(&key)
                    && !out.iter().any(|r| r.instance.as_ref() == Some(&key))
                {
                    out.push(Reply {
                        instance: Some(key),
                        partstat: "DECLINED".to_owned(),
                    });
                }
            }
        }
    }
    out
}

/// Replies for declining everything the attendee is invited to (the
/// attendee deleted their copy).
pub fn decline_all(organizer_copy: &Component) -> Vec<Reply> {
    let mut keys = InstantKeys::new(organizer_copy);
    instances(organizer_copy)
        .filter_map(|c| match c.property("RECURRENCE-ID") {
            Some(rid) => keys.key(rid, &rid.value).map(Some),
            None => Some(None),
        })
        .map(|instance| Reply {
            instance,
            partstat: "DECLINED".to_owned(),
        })
        .collect()
}

/// Merges replies into the organizer's copy: only the `PARTSTAT` of the
/// attendee's own ATTENDEE properties changes. A reply for an instance the
/// copy has no override for adds one (a copy of the master for that
/// instance), but only if the master lists the attendee and really has
/// that instance. Returns the indices (in `components`) of the changed or
/// added components.
pub fn apply_replies(
    organizer_copy: &mut Component,
    is_attendee: &dyn Fn(&str) -> bool,
    replies: &[Reply],
) -> Vec<usize> {
    let mut budget = Budget::new(OBJECT_BUDGET.saturating_mul(4));
    // Keys of the existing components, computed before anything changes.
    let existing: Vec<(usize, Option<InstantKey>)> = {
        let mut keys = InstantKeys::new(organizer_copy);
        organizer_copy
            .components
            .iter()
            .enumerate()
            .filter(|(_, c)| is_instance(c))
            .filter_map(|(i, c)| match c.property("RECURRENCE-ID") {
                Some(rid) => keys.key(rid, &rid.value).map(|key| (i, Some(key))),
                None => Some((i, None)),
            })
            .collect()
    };
    let master = existing
        .iter()
        .find(|(_, key)| key.is_none())
        .map(|(i, _)| *i);

    let mut updates: Vec<(usize, String)> = Vec::new();
    let mut additions: Vec<(String, String)> = Vec::new();
    for reply in replies {
        let target = existing
            .iter()
            .find(|(_, key)| *key == reply.instance)
            .map(|(i, _)| *i);
        match (target, &reply.instance) {
            (Some(index), _) => {
                let current = partstat(&organizer_copy.components[index], is_attendee);
                if current.is_some_and(|c| c != reply.partstat) {
                    updates.push((index, reply.partstat.clone()));
                }
            }
            (None, Some(instance)) => {
                let Some(master) = master else { continue };
                let current = partstat(&organizer_copy.components[master], is_attendee);
                if current.is_none_or(|c| c == reply.partstat) {
                    continue;
                }
                match recurrence::master_instance(organizer_copy, instance, &mut budget) {
                    Ok(Some(value)) if !additions.iter().any(|(v, _)| *v == value) => {
                        additions.push((value, reply.partstat.clone()));
                    }
                    _ => {}
                }
            }
            (None, None) => {}
        }
    }

    let mut changed = Vec::new();
    for (index, status) in updates {
        let component = &mut organizer_copy.components[index];
        for property in &mut component.properties {
            if property.name == "ATTENDEE" && is_attendee(&property.value) {
                set_param(property, "PARTSTAT", Some(&status));
            }
        }
        if !changed.contains(&index) {
            changed.push(index);
        }
    }
    if let Some(master) = master {
        for (value, status) in additions {
            let mut component = override_of(&organizer_copy.components[master], &value);
            for property in &mut component.properties {
                if property.name == "ATTENDEE" && is_attendee(&property.value) {
                    set_param(property, "PARTSTAT", Some(&status));
                }
            }
            organizer_copy.components.push(component);
            changed.push(organizer_copy.components.len() - 1);
        }
    }
    changed
}

/// A copy of the master for the instance starting at `value` (formatted
/// like the master's DTSTART).
fn override_of(master: &Component, value: &str) -> Component {
    let mut component = cleaned(master);
    component.components.clear();
    let start = master.property("DTSTART");
    let duration = match (start, master.property("DTEND"), master.property("DUE")) {
        (Some(start), Some(end), _) | (Some(start), None, Some(end)) => {
            match (
                parse_date_or_datetime(&start.value),
                parse_date_or_datetime(&end.value),
            ) {
                (Ok(s), Ok(e)) if e.seconds >= s.seconds => Some(e.seconds - s.seconds),
                _ => None,
            }
        }
        _ => None,
    };
    let end_name = if master.property("DUE").is_some() {
        "DUE"
    } else {
        "DTEND"
    };
    component.properties.retain(|p| {
        !matches!(
            p.name.as_str(),
            "RRULE" | "RDATE" | "EXDATE" | "EXRULE" | "DTSTART" | "RECURRENCE-ID"
        ) && p.name != end_name
    });
    let params: Vec<Parameter> = start
        .map(|s| {
            s.parameters
                .iter()
                .filter(|p| p.name == "TZID" || p.name == "VALUE")
                .cloned()
                .collect()
        })
        .unwrap_or_default();
    for name in ["DTSTART", "RECURRENCE-ID"] {
        component.properties.push(Property {
            name: name.to_owned(),
            parameters: params.clone(),
            value: value.to_owned(),
        });
    }
    if let Some(seconds) = duration
        && master.property("DURATION").is_none()
    {
        let is_date = start.is_some_and(|s| s.value.len() == 8);
        let value = if is_date && seconds % 86_400 == 0 {
            format!("P{}D", seconds / 86_400)
        } else {
            format!("PT{seconds}S")
        };
        component.properties.push(simple("DURATION", &value));
    }
    component
}

/// The REPLY message for the components at `indices` of the organizer's
/// copy: the identifying properties plus the attendee's own ATTENDEE.
pub fn reply_message(
    organizer_copy: &Component,
    indices: &[usize],
    is_attendee: &dyn Fn(&str) -> bool,
    dtstamp: &str,
) -> Component {
    let mut body = Component {
        name: "VCALENDAR".to_owned(),
        properties: Vec::new(),
        components: organizer_copy
            .components
            .iter()
            .filter(|c| c.name == "VTIMEZONE")
            .cloned()
            .collect(),
    };
    for &index in indices {
        let Some(component) = organizer_copy.components.get(index) else {
            continue;
        };
        let mut reply = Component {
            name: component.name.clone(),
            properties: Vec::new(),
            components: Vec::new(),
        };
        for property in &component.properties {
            let keep = match property.name.as_str() {
                "UID" | "RECURRENCE-ID" | "DTSTART" | "SEQUENCE" | "SUMMARY" | "ORGANIZER" => true,
                "ATTENDEE" => is_attendee(&property.value),
                _ => false,
            };
            if keep {
                let mut property = property.clone();
                property
                    .parameters
                    .retain(|p| p.name != "SCHEDULE-STATUS" && p.name != "SCHEDULE-FORCE-SEND");
                reply.properties.push(property);
            }
        }
        body.components.push(reply);
    }
    message("REPLY", &body, dtstamp)
}

/// Free/busy type of a busy period (RFC 5545 §3.2.9).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum BusyType {
    Busy,
    Tentative,
}

impl BusyType {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Busy => "BUSY",
            Self::Tentative => "BUSY-TENTATIVE",
        }
    }
}

/// How an event counts for the free/busy time of the user: `None` if it
/// does not (transparent, cancelled, or declined by the user).
fn busy_type(component: &Component, is_user: &dyn Fn(&str) -> bool) -> Option<BusyType> {
    let value = |name: &str| {
        component
            .property(name)
            .map(|p| p.value.to_ascii_uppercase())
    };
    if value("TRANSP").as_deref() == Some("TRANSPARENT")
        || value("STATUS").as_deref() == Some("CANCELLED")
    {
        return None;
    }
    let mut kind = if value("STATUS").as_deref() == Some("TENTATIVE") {
        BusyType::Tentative
    } else {
        BusyType::Busy
    };
    match partstat(component, is_user).as_deref() {
        Some("DECLINED") => return None,
        Some("NEEDS-ACTION" | "TENTATIVE") => kind = BusyType::Tentative,
        _ => {}
    }
    Some(kind)
}

/// Busy periods of one stored object within `[start, end)` (UTC seconds),
/// clipped to the range. `None` if they cannot be computed exactly within
/// the budget (callers then count the object's whole span as busy).
pub fn busy_periods(
    data: &[u8],
    start: i64,
    end: i64,
    is_user: &dyn Fn(&str) -> bool,
    budget: &mut Budget,
) -> Option<Vec<(i64, i64, BusyType)>> {
    let root = crate::parse(data).ok()?;
    let mut periods = Vec::new();
    recurrence::busy_instances(&root, start, end, budget, &mut |component, lo, hi| {
        if let Some(kind) = busy_type(component, is_user)
            && hi > lo
        {
            periods.push((lo, hi, kind));
        }
    })
    .ok()?;
    Some(periods)
}

/// Sorts busy periods and merges overlapping ones of the same type.
pub fn merge_periods(mut periods: Vec<(i64, i64, BusyType)>) -> Vec<(i64, i64, BusyType)> {
    periods.sort_by_key(|&(lo, hi, kind)| (kind, lo, hi));
    let mut out: Vec<(i64, i64, BusyType)> = Vec::new();
    for (lo, hi, kind) in periods {
        match out.last_mut() {
            Some(last) if last.2 == kind && lo <= last.1 => last.1 = last.1.max(hi),
            _ => out.push((lo, hi, kind)),
        }
    }
    out.sort_by_key(|&(lo, hi, kind)| (lo, hi, kind));
    out
}

/// `YYYYMMDDTHHMMSSZ` for a UTC instant.
pub fn utc_stamp(seconds: i64) -> Option<String> {
    format_date_or_datetime(seconds, false, true)
}

#[cfg(test)]
mod tests;
