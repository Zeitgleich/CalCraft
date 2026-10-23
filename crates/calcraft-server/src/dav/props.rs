//! Live and dead properties of DAV resources.

use calcraft_core::CollectionKind;
use calcraft_dav::{
    Element, PropName, PropValue, Propfind, Response as DavResponse, XmlFragment, dead,
    xml::{NS_APPLE_ICAL, NS_CALDAV, NS_CALENDARSERVER, NS_CARDDAV, NS_DAV},
};
use calcraft_ical::{MAX_OBJECT_SIZE, xml::is_xml_name};
use calcraft_store::{Collection, DeadProperty, User};

use super::paths::{Resource, home_href, principal_href};

pub const CALENDAR_CONTENT_TYPE: &str = "text/calendar; charset=utf-8";
pub const VCARD_CONTENT_TYPE: &str = "text/vcard; charset=utf-8";

/// Content type of objects in a collection of this kind.
pub fn content_type(kind: CollectionKind) -> &'static str {
    match kind {
        CollectionKind::Calendar => CALENDAR_CONTENT_TYPE,
        CollectionKind::Addressbook => VCARD_CONTENT_TYPE,
    }
}

/// The property carrying object content in REPORT responses.
pub fn data_property(kind: CollectionKind) -> (&'static str, &'static str) {
    match kind {
        CollectionKind::Calendar => (NS_CALDAV, "calendar-data"),
        CollectionKind::Addressbook => (NS_CARDDAV, "address-data"),
    }
}

fn supported_reports(reports: &[(&'static str, &'static str)]) -> PropValue {
    PropValue::Elements(
        reports
            .iter()
            .map(|(ns, report)| {
                Element::new(NS_DAV, "supported-report")
                    .child(Element::new(NS_DAV, "report").child(Element::new(ns, report)))
            })
            .collect(),
    )
}

/// Properties that clients can change with PROPPATCH (and set via MKCALENDAR).
pub const WRITABLE: [(&str, &str); 4] = [
    (NS_DAV, "displayname"),
    (NS_CALDAV, "calendar-description"),
    (NS_CARDDAV, "addressbook-description"),
    (NS_APPLE_ICAL, "calendar-color"),
];

/// Apple's calendar order: a dead property with an integer value, also
/// used to sort calendars in the web UI.
pub const CALENDAR_ORDER: (&str, &str) = (NS_APPLE_ICAL, "calendar-order");

/// Stored dead properties of collection `collection_id` as response
/// properties. Names and values are re-validated (a name must be a valid
/// XML name, see [`XmlFragment::from_stored`] for values); a property that
/// fails is left out, so one corrupt row cannot break a whole multistatus.
pub fn dead_properties<'a>(
    collection_id: i64,
    stored: impl IntoIterator<Item = &'a DeadProperty>,
) -> impl Iterator<Item = (PropName, PropValue)> {
    stored.into_iter().filter_map(move |property| {
        let name = PropName::new(&property.namespace, &property.name);
        let value = (is_xml_name(&name.name) && dead::check_name(&name).is_ok())
            .then(|| XmlFragment::from_stored(&property.value))
            .flatten();
        let Some(value) = value else {
            // Logged by collection id only: names and values are client data.
            tracing::warn!(collection_id, "invalid stored dead property left out");
            return None;
        };
        Some((name, PropValue::Xml(value)))
    })
}

/// The `calendar-order` among a collection's dead properties.
#[cfg(feature = "api")]
pub fn calendar_order<'a>(stored: impl IntoIterator<Item = &'a DeadProperty>) -> Option<i64> {
    stored
        .into_iter()
        .find(|p| p.namespace == CALENDAR_ORDER.0 && p.name == CALENDAR_ORDER.1)
        .and_then(|p| p.value.trim().parse().ok())
}

/// Filters the available properties according to the request.
pub fn respond(href: &str, available: &[(PropName, PropValue)], query: &Propfind) -> DavResponse {
    let mut response = DavResponse::new(href);
    match query {
        Propfind::AllProp => response.found = available.to_vec(),
        Propfind::PropName => {
            response.found = available
                .iter()
                .map(|(name, _)| (name.clone(), PropValue::Empty))
                .collect();
        }
        Propfind::Prop(requested) => {
            for name in requested {
                match available.iter().find(|(candidate, _)| candidate == name) {
                    Some(found) => response.found.push(found.clone()),
                    None => response.not_found.push(name.clone()),
                }
            }
        }
    }
    response
}

fn prop(namespace: &str, name: &str, value: PropValue) -> (PropName, PropValue) {
    (PropName::new(namespace, name), value)
}

fn resourcetype(types: &[(&'static str, &'static str)]) -> (PropName, PropValue) {
    prop(
        NS_DAV,
        "resourcetype",
        PropValue::Elements(
            types
                .iter()
                .map(|(ns, name)| Element::new(ns, name))
                .collect(),
        ),
    )
}

/// Privileges of the owner on their own resources.
const OWNER_PRIVILEGES: [&str; 7] = [
    "read",
    "write",
    "write-properties",
    "write-content",
    "bind",
    "unbind",
    "read-current-user-privilege-set",
];
/// Privileges of the owner on a subscription and its objects: only the
/// server's feed refresher changes them (the subscription itself can be
/// deleted, which is the home's `unbind`).
const READ_ONLY_PRIVILEGES: [&str; 2] = ["read", "read-current-user-privilege-set"];

fn privileges(read_only: bool) -> PropValue {
    let names: &[&str] = if read_only {
        &READ_ONLY_PRIVILEGES
    } else {
        &OWNER_PRIVILEGES
    };
    PropValue::Elements(
        names
            .iter()
            .map(|privilege| {
                Element::new(NS_DAV, "privilege").child(Element::new(NS_DAV, privilege))
            })
            .collect(),
    )
}

fn common(user: &User, read_only: bool) -> Vec<(PropName, PropValue)> {
    vec![
        prop(
            NS_DAV,
            "current-user-principal",
            PropValue::Href(principal_href(&user.username)),
        ),
        prop(NS_DAV, "current-user-privilege-set", privileges(read_only)),
    ]
}

/// Component types of a calendar, parsed from the stored list.
pub fn components(collection: &Collection) -> Vec<&str> {
    collection
        .components
        .split(',')
        .filter(|c| !c.is_empty())
        .collect()
}

/// Limits of managed attachments (RFC 8607 §6.2, §6.3), reported on
/// writable calendars while attachments are enabled.
#[derive(Debug, Clone, Copy)]
pub struct AttachmentLimits {
    pub max_size: usize,
    pub max_per_resource: u32,
}

/// All live properties of a collection-level resource; `max_vcard_bytes`
/// is reported as the address books' `CARDDAV:max-resource-size`.
pub fn properties(
    resource: &Resource,
    user: &User,
    max_vcard_bytes: usize,
    attachments: Option<AttachmentLimits>,
) -> Vec<(PropName, PropValue)> {
    let username = &user.username;
    let principal = principal_href(username);
    let read_only = matches!(resource, Resource::Collection(c) if c.read_only);
    let mut props = common(user, read_only);
    match resource {
        Resource::Root => props.push(resourcetype(&[(NS_DAV, "collection")])),
        Resource::Principal => {
            props.extend([
                resourcetype(&[(NS_DAV, "collection"), (NS_DAV, "principal")]),
                prop(
                    NS_DAV,
                    "displayname",
                    PropValue::Text(
                        user.display_name
                            .clone()
                            .unwrap_or_else(|| username.to_string()),
                    ),
                ),
                prop(NS_DAV, "principal-URL", PropValue::Href(principal.clone())),
                prop(
                    NS_CALDAV,
                    "calendar-home-set",
                    PropValue::Href(home_href(CollectionKind::Calendar, username)),
                ),
                prop(
                    NS_CARDDAV,
                    "addressbook-home-set",
                    PropValue::Href(home_href(CollectionKind::Addressbook, username)),
                ),
            ]);
        }
        Resource::Home(kind) => {
            props.extend([
                resourcetype(&[(NS_DAV, "collection")]),
                prop(NS_DAV, "owner", PropValue::Href(principal)),
            ]);
            // Empty: attachments live on this server (RFC 8607 §6.1).
            if *kind == CollectionKind::Calendar && attachments.is_some() {
                props.push(prop(
                    NS_CALDAV,
                    "managed-attachments-server-URL",
                    PropValue::Empty,
                ));
            }
        }
        Resource::Collection(collection) => {
            props.extend(collection_properties(
                collection,
                principal,
                max_vcard_bytes,
            ));
            if let Some(limits) = attachments
                && collection.kind == CollectionKind::Calendar
                && !collection.read_only
            {
                props.extend([
                    prop(
                        NS_CALDAV,
                        "max-attachment-size",
                        PropValue::Text(limits.max_size.to_string()),
                    ),
                    prop(
                        NS_CALDAV,
                        "max-attachments-per-resource",
                        PropValue::Text(limits.max_per_resource.to_string()),
                    ),
                ]);
            }
        }
        // Unmapped paths and items are handled elsewhere.
        Resource::Unmapped(..)
        | Resource::Item(..)
        | Resource::Inbox
        | Resource::InboxItem(_)
        | Resource::Outbox
        | Resource::Attachment(_) => {}
    }
    props
}

fn collection_properties(
    collection: &Collection,
    principal: String,
    max_vcard_bytes: usize,
) -> Vec<(PropName, PropValue)> {
    let (kind_ns, kind_name, description_ns, description_name) = match collection.kind {
        CollectionKind::Calendar => (NS_CALDAV, "calendar", NS_CALDAV, "calendar-description"),
        CollectionKind::Addressbook => (
            NS_CARDDAV,
            "addressbook",
            NS_CARDDAV,
            "addressbook-description",
        ),
    };
    let mut props = vec![
        resourcetype(&[(NS_DAV, "collection"), (kind_ns, kind_name)]),
        prop(NS_DAV, "owner", PropValue::Href(principal)),
        prop(
            NS_CALENDARSERVER,
            "getctag",
            PropValue::Text(collection.sync_revision.to_string()),
        ),
        prop(
            NS_DAV,
            "sync-token",
            PropValue::Text(super::sync::token(collection)),
        ),
    ];
    if let Some(name) = &collection.display_name {
        props.push(prop(NS_DAV, "displayname", PropValue::Text(name.clone())));
    }
    if let Some(description) = &collection.description {
        props.push(prop(
            description_ns,
            description_name,
            PropValue::Text(description.clone()),
        ));
    }
    if collection.kind == CollectionKind::Calendar {
        props.extend([
            prop(
                NS_CALDAV,
                "supported-calendar-component-set",
                PropValue::Elements(
                    components(collection)
                        .into_iter()
                        .map(|comp| Element::new(NS_CALDAV, "comp").attr("name", comp))
                        .collect(),
                ),
            ),
            prop(
                NS_DAV,
                "supported-report-set",
                supported_reports(&[
                    (NS_CALDAV, "calendar-multiget"),
                    (NS_CALDAV, "calendar-query"),
                    (NS_DAV, "sync-collection"),
                ]),
            ),
            prop(
                NS_CALDAV,
                "max-resource-size",
                PropValue::Text(MAX_OBJECT_SIZE.to_string()),
            ),
        ]);
        if let Some(color) = &collection.color {
            props.push(prop(
                NS_APPLE_ICAL,
                "calendar-color",
                PropValue::Text(color.clone()),
            ));
        }
    }
    if collection.kind == CollectionKind::Addressbook {
        props.extend([
            prop(
                NS_DAV,
                "supported-report-set",
                supported_reports(&[
                    (NS_CARDDAV, "addressbook-multiget"),
                    (NS_CARDDAV, "addressbook-query"),
                    (NS_DAV, "sync-collection"),
                ]),
            ),
            prop(
                NS_CARDDAV,
                "max-resource-size",
                PropValue::Text(max_vcard_bytes.to_string()),
            ),
            prop(
                NS_CARDDAV,
                "supported-address-data",
                PropValue::Elements(
                    ["3.0", "4.0"]
                        .into_iter()
                        .map(|version| {
                            Element::new(NS_CARDDAV, "address-data-type")
                                .attr("content-type", "text/vcard")
                                .attr("version", version)
                        })
                        .collect(),
                ),
            ),
        ]);
    }
    props
}

/// Properties of a calendar or address object. `content` adds
/// `calendar-data` / `address-data`.
pub fn item_properties(
    user: &User,
    collection: &Collection,
    etag: &str,
    size: usize,
    content: Option<&[u8]>,
) -> Vec<(PropName, PropValue)> {
    let kind = collection.kind;
    let mut props = common(user, collection.read_only);
    props.extend([
        resourcetype(&[]),
        prop(NS_DAV, "getetag", PropValue::Text(etag.to_owned())),
        prop(
            NS_DAV,
            "getcontenttype",
            PropValue::Text(content_type(kind).to_owned()),
        ),
        prop(
            NS_DAV,
            "getcontentlength",
            PropValue::Text(size.to_string()),
        ),
    ]);
    if let Some(content) = content {
        let (ns, name) = data_property(kind);
        // Stored objects passed the UTF-8 check of the parser.
        props.push(prop(
            ns,
            name,
            PropValue::Text(String::from_utf8_lossy(content).into_owned()),
        ));
    }
    props
}
