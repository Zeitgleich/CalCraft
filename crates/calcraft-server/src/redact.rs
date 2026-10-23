//! Hiding the details of private events: the busy stubs read-only sharees
//! get for `CLASS:PRIVATE`/`CONFIDENTIAL` events (docs/sharing.md) and the
//! property whitelists of public links ([`crate::publish`]).
//!
//! Everything here is a whitelist: a property is only kept if it is named.
//! A component's class is its own `CLASS`; an override (`RECURRENCE-ID`)
//! without one inherits the class of the series' master. Every class other
//! than `PUBLIC` counts as private (RFC 5545 §3.8.1.3).

use calcraft_ical::{Component, Property, parse, write::to_string};

/// Components that carry an object's content.
pub const OBJECT_COMPONENTS: [&str; 3] = ["VEVENT", "VTODO", "VJOURNAL"];

/// What a read-only sharee sees of a private event: its times, recurrence,
/// status and class, and `SUMMARY:Busy`.
pub const SHAREE_STUB_PROPERTIES: [&str; 15] = [
    "UID",
    "DTSTART",
    "DTEND",
    "DURATION",
    "DUE",
    "RRULE",
    "RDATE",
    "EXDATE",
    "RECURRENCE-ID",
    "STATUS",
    "TRANSP",
    "CLASS",
    "DTSTAMP",
    "SEQUENCE",
    "SUMMARY",
];

/// Properties of the VCALENDAR kept around stubs.
const CALENDAR_PROPERTIES: [&str; 3] = ["VERSION", "PRODID", "CALSCALE"];

/// The text that replaces the summary of a hidden event.
pub const BUSY: &str = "Busy";

/// `Some(true)` for an own class other than PUBLIC, `None` without CLASS.
fn own_class_private(component: &Component) -> Option<bool> {
    component
        .property("CLASS")
        .map(|class| !class.value.trim().eq_ignore_ascii_case("PUBLIC"))
}

/// Whether the series' master (the object component without
/// `RECURRENCE-ID`) is private; overrides without CLASS inherit it.
pub fn master_private(tree: &Component) -> bool {
    tree.components
        .iter()
        .find(|c| {
            OBJECT_COMPONENTS.contains(&c.name.as_str()) && c.property("RECURRENCE-ID").is_none()
        })
        .and_then(own_class_private)
        .unwrap_or(false)
}

/// Whether a component is private, `master_private` from [`master_private`].
pub fn is_private(component: &Component, master_private: bool) -> bool {
    own_class_private(component).unwrap_or(master_private)
}

pub fn property(name: &str, value: &str) -> Property {
    Property {
        name: name.to_owned(),
        parameters: Vec::new(),
        value: value.to_owned(),
    }
}

/// A copy of `component` with only the properties named in `keep` (and no
/// nested components such as alarms), its summary replaced by
/// [`BUSY`].
pub fn busy_stub(component: &Component, keep: &[&str]) -> Component {
    let mut properties: Vec<Property> = component
        .properties
        .iter()
        .filter(|p| p.name != "SUMMARY" && keep.contains(&p.name.as_str()))
        .cloned()
        .collect();
    properties.push(property("SUMMARY", BUSY));
    Component {
        name: component.name.clone(),
        properties,
        components: Vec::new(),
    }
}

/// What a read-only sharee gets for a stored calendar object: `None` if it
/// has no private component (served unchanged), else the object with every
/// private component replaced by its busy stub. Public components of the
/// same object stay as they are.
pub fn sharee_view(content: &[u8]) -> Option<Vec<u8>> {
    // Stored objects passed the parser; anything else is served as it is.
    let tree = parse(content).ok()?;
    let master = master_private(&tree);
    let private =
        |c: &Component| OBJECT_COMPONENTS.contains(&c.name.as_str()) && is_private(c, master);
    if !tree.components.iter().any(private) {
        return None;
    }
    let calendar = Component {
        name: tree.name.clone(),
        properties: tree
            .properties
            .iter()
            .filter(|p| CALENDAR_PROPERTIES.contains(&p.name.as_str()))
            .cloned()
            .collect(),
        components: tree
            .components
            .iter()
            .map(|c| {
                if private(c) {
                    busy_stub(c, &SHAREE_STUB_PROPERTIES)
                } else {
                    c.clone()
                }
            })
            .collect(),
    };
    // A stub that cannot be written is never replaced by the original.
    Some(to_string(&calendar).map_or_else(|_| Vec::new(), String::into_bytes))
}

#[cfg(test)]
mod tests;
