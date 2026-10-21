//! Live properties of DAV resources.

use calcraft_core::CollectionKind;
use calcraft_dav::{
    Element, PropName, PropValue, Propfind, Response as DavResponse,
    xml::{NS_APPLE_ICAL, NS_CALDAV, NS_CALENDARSERVER, NS_CARDDAV, NS_DAV},
};
use calcraft_ical::MAX_OBJECT_SIZE;
use calcraft_store::{Collection, User};

use super::paths::{Resource, home_href, principal_href};

pub const CALENDAR_CONTENT_TYPE: &str = "text/calendar; charset=utf-8";

/// Properties that clients can change with PROPPATCH (and set via MKCALENDAR).
pub const WRITABLE: [(&str, &str); 4] = [
    (NS_DAV, "displayname"),
    (NS_CALDAV, "calendar-description"),
    (NS_CARDDAV, "addressbook-description"),
    (NS_APPLE_ICAL, "calendar-color"),
];

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
fn owner_privileges() -> PropValue {
    PropValue::Elements(
        [
            "read",
            "write",
            "write-properties",
            "write-content",
            "bind",
            "unbind",
            "read-current-user-privilege-set",
        ]
        .into_iter()
        .map(|privilege| Element::new(NS_DAV, "privilege").child(Element::new(NS_DAV, privilege)))
        .collect(),
    )
}

fn common(user: &User) -> Vec<(PropName, PropValue)> {
    vec![
        prop(
            NS_DAV,
            "current-user-principal",
            PropValue::Href(principal_href(&user.username)),
        ),
        prop(NS_DAV, "current-user-privilege-set", owner_privileges()),
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

/// All live properties of a collection-level resource.
pub fn properties(resource: &Resource, user: &User) -> Vec<(PropName, PropValue)> {
    let username = &user.username;
    let principal = principal_href(username);
    let mut props = common(user);
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
        Resource::Home(_) => {
            props.extend([
                resourcetype(&[(NS_DAV, "collection")]),
                prop(NS_DAV, "owner", PropValue::Href(principal)),
            ]);
        }
        Resource::Collection(collection) => {
            props.extend(collection_properties(collection, principal));
        }
        // Unmapped paths and items are handled elsewhere.
        Resource::Unmapped(..) | Resource::Item(..) => {}
    }
    props
}

fn collection_properties(collection: &Collection, principal: String) -> Vec<(PropName, PropValue)> {
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
                PropValue::Elements(
                    ["calendar-multiget", "calendar-query"]
                        .into_iter()
                        .map(|report| {
                            Element::new(NS_DAV, "supported-report").child(
                                Element::new(NS_DAV, "report")
                                    .child(Element::new(NS_CALDAV, report)),
                            )
                        })
                        .collect(),
                ),
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
    props
}

/// Properties of a calendar object. `content` adds `calendar-data`.
pub fn item_properties(
    user: &User,
    etag: &str,
    size: usize,
    content: Option<&[u8]>,
) -> Vec<(PropName, PropValue)> {
    let mut props = common(user);
    props.extend([
        resourcetype(&[]),
        prop(NS_DAV, "getetag", PropValue::Text(etag.to_owned())),
        prop(
            NS_DAV,
            "getcontenttype",
            PropValue::Text(CALENDAR_CONTENT_TYPE.to_owned()),
        ),
        prop(
            NS_DAV,
            "getcontentlength",
            PropValue::Text(size.to_string()),
        ),
    ]);
    if let Some(content) = content {
        // Stored objects passed the UTF-8 check of the iCalendar parser.
        props.push(prop(
            NS_CALDAV,
            "calendar-data",
            PropValue::Text(String::from_utf8_lossy(content).into_owned()),
        ));
    }
    props
}
