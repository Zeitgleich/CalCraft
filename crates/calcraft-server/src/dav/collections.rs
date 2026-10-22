//! MKCALENDAR (RFC 4791 §5.3.1) and PROPPATCH (RFC 4918 §9.2).
//!
//! A fixed set of descriptive live properties can be written; ACL-,
//! membership- and server-computed properties – everything else in the
//! `DAV:`, CalDAV and CardDAV namespaces – are protected, so a PROPPATCH
//! can never change who has access. Properties in other namespaces are
//! stored as dead properties (RFC 4918 §4), within the limits of
//! [`calcraft_dav::dead`] and [`calcraft_store::MAX_DEAD_PROPERTIES`].

use axum::{
    extract::Request,
    http::StatusCode,
    response::{IntoResponse, Response},
};
use calcraft_core::{CollectionKind, Slug};
use calcraft_dav::{
    MAX_PROPPATCH_BODY, Multistatus, PropName, PropertyUpdate, Response as DavResponse,
    XmlFragment,
    dead::{self, NameError, ValueError},
    tree::Node,
    xml::{NS_APPLE_ICAL, NS_CALDAV, NS_CALENDARSERVER, NS_CARDDAV, NS_DAV},
};
use calcraft_ical::ComponentKind;
use calcraft_store::{PropertyChange, StoreError};

use crate::validate::{MAX_DESCRIPTION, MAX_DISPLAY_NAME, clean_color, clean_text};

use super::{
    Ctx, bad_request, dav_error, method_not_allowed, multistatus,
    paths::{Resource, href_of},
    props::{CALENDAR_ORDER, WRITABLE},
    read_body, store_error,
};

/// Precondition for protected properties (RFC 4918 §16).
const PROTECTED: &str = "cannot-modify-protected-property";

/// Properties in the reserved namespaces (`DAV:`, CalDAV, CardDAV) that
/// clients set but CalCraft does not interpret; they are stored as dead
/// properties. Apple Calendar sets them on calendars.
const DEAD_IN_RESERVED: [(&str, &str); 4] = [
    (NS_CALDAV, "calendar-timezone"),
    (NS_CALDAV, "schedule-calendar-transp"),
    (NS_CALDAV, "default-alarm-vevent-date"),
    (NS_CALDAV, "default-alarm-vevent-datetime"),
];

/// Validated changes to the properties of one collection.
#[derive(Debug, Default)]
struct Changes {
    display_name: Option<Option<String>>,
    description: Option<Option<String>>,
    color: Option<Option<String>>,
    /// Dead properties in request order; `None` removes one.
    dead: Vec<(PropName, Option<XmlFragment>)>,
}

impl Changes {
    fn dead_changes(&self) -> Vec<PropertyChange<'_>> {
        self.dead
            .iter()
            .map(|(name, value)| PropertyChange {
                namespace: &name.namespace,
                name: &name.name,
                value: value.as_ref().map(XmlFragment::as_str),
            })
            .collect()
    }
}

/// Outcome of one property operation, as a propstat status.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Status {
    Ok,
    /// 403 with `cannot-modify-protected-property`.
    Protected,
    /// 403: not applicable to this collection or not an allowed name.
    Forbidden,
    /// 409: the value is not acceptable.
    Conflict,
    /// 507: the value is too large.
    TooLarge,
}

impl Status {
    fn code(self) -> (u16, Option<&'static str>) {
        match self {
            Self::Ok => (200, None),
            Self::Protected => (403, Some(PROTECTED)),
            Self::Forbidden => (403, None),
            Self::Conflict => (409, None),
            Self::TooLarge => (507, None),
        }
    }
}

/// How a property name is handled on a collection of `kind`.
enum Class {
    Live,
    /// `calendar-order`: a dead property with an integer value.
    Order,
    Dead,
    Protected,
    Forbidden,
}

fn classify(name: &PropName, kind: CollectionKind) -> Class {
    let calendar_only = name.is(NS_APPLE_ICAL, "calendar-color")
        || name.is(CALENDAR_ORDER.0, CALENDAR_ORDER.1)
        || name.is(NS_CALDAV, "calendar-description");
    if kind == CollectionKind::Addressbook && calendar_only
        || kind == CollectionKind::Calendar && name.is(NS_CARDDAV, "addressbook-description")
    {
        return Class::Forbidden;
    }
    if WRITABLE.iter().any(|(ns, n)| name.is(ns, n)) {
        Class::Live
    } else if name.is(CALENDAR_ORDER.0, CALENDAR_ORDER.1) {
        Class::Order
    } else if DEAD_IN_RESERVED.iter().any(|(ns, n)| name.is(ns, n)) {
        Class::Dead
    } else if [NS_DAV, NS_CALDAV, NS_CARDDAV].contains(&name.namespace.as_str())
        || name.is(NS_CALENDARSERVER, "getctag")
    {
        Class::Protected
    } else {
        Class::Dead
    }
}

/// Apple sends small integers; anything else is rejected rather than
/// stored, so that sorting by it stays well-defined.
fn valid_order(value: &Node) -> bool {
    value.children.is_empty() && value.text.trim().parse::<i64>().is_ok()
}

/// Applies one operation to `changes`.
fn apply(
    changes: &mut Changes,
    name: &PropName,
    value: Option<&Node>,
    kind: CollectionKind,
) -> Status {
    match classify(name, kind) {
        Class::Protected => return Status::Protected,
        Class::Forbidden => return Status::Forbidden,
        Class::Order if value.is_some_and(|v| !valid_order(v)) => return Status::Conflict,
        Class::Order | Class::Dead => return apply_dead(changes, name, value),
        Class::Live => {}
    }
    let text = |max| match value {
        None => Some(None),
        Some(node) => clean_text(&node.text, max).map(|t| (!t.is_empty()).then_some(t)),
    };
    let result = if name.is(NS_DAV, "displayname") {
        text(MAX_DISPLAY_NAME).map(|v| changes.display_name = Some(v))
    } else if name.is(NS_APPLE_ICAL, "calendar-color") {
        match value {
            None => Some(None),
            Some(node) => clean_color(&node.text).map(Some),
        }
        .map(|v| changes.color = Some(v))
    } else {
        text(MAX_DESCRIPTION).map(|v| changes.description = Some(v))
    };
    if result.is_some() {
        Status::Ok
    } else {
        Status::Conflict
    }
}

fn apply_dead(changes: &mut Changes, name: &PropName, value: Option<&Node>) -> Status {
    match dead::check_name(name) {
        Ok(()) => {}
        Err(NameError::TooLong | NameError::NotAbsolute) => return Status::Forbidden,
    }
    let fragment = match value.map(XmlFragment::from_property).transpose() {
        Ok(fragment) => fragment,
        Err(ValueError::TooLarge) => return Status::TooLarge,
        Err(ValueError::InvalidCharacter) => return Status::Conflict,
    };
    changes.dead.push((name.clone(), fragment));
    Status::Ok
}

pub async fn proppatch(ctx: Ctx<'_>, resource: Resource, request: Request) -> Response {
    let Resource::Collection(collection) = &resource else {
        return method_not_allowed();
    };
    let body = match read_body(request, MAX_PROPPATCH_BODY).await {
        Ok(body) => body,
        Err(err) => return err.into_response(),
    };
    let update = match PropertyUpdate::parse_proppatch(&body) {
        Ok(update) => update,
        Err(err) => return bad_request(&err),
    };

    let mut changes = Changes::default();
    let mut results: Vec<(PropName, Status)> = update
        .operations
        .iter()
        .map(|(name, value)| {
            let status = apply(&mut changes, name, value.as_ref(), collection.kind);
            (name.clone(), status)
        })
        .collect();

    // PROPPATCH is atomic: if anything fails, nothing is applied.
    if results.iter().all(|(_, status)| *status == Status::Ok) {
        let stored = ctx
            .state
            .store
            .update_collection_with_properties(
                collection.id,
                changes.display_name.as_ref().map(Option::as_deref),
                changes.description.as_ref().map(Option::as_deref),
                changes.color.as_ref().map(Option::as_deref),
                &changes.dead_changes(),
            )
            .await;
        match stored {
            Ok(()) => {}
            // Too many or too large dead properties: the transaction was
            // rolled back; the properties that would be set get 507.
            Err(StoreError::Limit(_)) => {
                for (name, status) in &mut results {
                    let sets_dead = changes
                        .dead
                        .iter()
                        .any(|(dead, value)| dead == name && value.is_some());
                    if sets_dead {
                        *status = Status::TooLarge;
                    }
                }
            }
            Err(err) => return store_error(err),
        }
    }
    let failed = results.iter().any(|(_, status)| *status != Status::Ok);
    let mut response = DavResponse::new(href_of(&resource, &ctx.user.username));
    for (name, status) in results {
        match (failed, status) {
            (false, _) => response.found.push((name, calcraft_dav::PropValue::Empty)),
            (true, Status::Ok) => response.failed.push((name, 424, None)),
            (true, status) => {
                let (code, error) = status.code();
                response.failed.push((name, code, error));
            }
        }
    }
    multistatus(&Multistatus {
        responses: vec![response],
        sync_token: None,
    })
}

/// Reads `supported-calendar-component-set` from a MKCALENDAR request.
fn requested_components(node: &Node) -> Option<String> {
    let names: Vec<&str> = node
        .children_named(NS_CALDAV, "comp")
        .filter_map(|comp| comp.attribute("name"))
        .map(|name| ComponentKind::from_name(name).map(ComponentKind::as_str))
        .collect::<Option<_>>()?;
    (!names.is_empty()).then(|| names.join(","))
}

pub async fn mkcalendar(ctx: Ctx<'_>, resource: Resource, request: Request) -> Response {
    let Resource::Unmapped(CollectionKind::Calendar, slug) = resource else {
        return method_not_allowed();
    };
    let body = match read_body(request, MAX_PROPPATCH_BODY).await {
        Ok(body) => body,
        Err(err) => return err.into_response(),
    };
    match PropertyUpdate::parse_mkcalendar(&body) {
        Ok(update) => create(ctx, CollectionKind::Calendar, &slug, &update).await,
        Err(err) => bad_request(&err),
    }
}

/// Extended MKCOL (RFC 5689): creates an address book below the address
/// book home or a calendar below the calendar home.
pub async fn mkcol(ctx: Ctx<'_>, resource: Resource, request: Request) -> Response {
    let Resource::Unmapped(kind, slug) = resource else {
        return method_not_allowed();
    };
    let body = match read_body(request, MAX_PROPPATCH_BODY).await {
        Ok(body) => body,
        Err(err) => return err.into_response(),
    };
    let update = match PropertyUpdate::parse_mkcol(&body) {
        Ok(update) => update,
        Err(err) => return bad_request(&err),
    };
    // A resourcetype, if given, must match the home the collection is created in.
    if let Some((_, Some(resourcetype))) = update
        .operations
        .iter()
        .find(|(name, _)| name.is(NS_DAV, "resourcetype"))
    {
        let (ns, name) = match kind {
            CollectionKind::Calendar => (NS_CALDAV, "calendar"),
            CollectionKind::Addressbook => (NS_CARDDAV, "addressbook"),
        };
        let valid = resourcetype.child(NS_DAV, "collection").is_some()
            && resourcetype.child(ns, name).is_some()
            && resourcetype.children.len() == 2;
        if !valid {
            return dav_error(StatusCode::FORBIDDEN, "<d:valid-resourcetype/>");
        }
    }
    create(ctx, kind, &slug, &update).await
}

/// Creates a collection from MKCALENDAR / MKCOL properties.
async fn create(
    ctx: Ctx<'_>,
    kind: CollectionKind,
    slug: &Slug,
    update: &PropertyUpdate,
) -> Response {
    let mut changes = Changes::default();
    let mut components = match kind {
        CollectionKind::Calendar => "VEVENT,VTODO".to_owned(),
        CollectionKind::Addressbook => "VCARD".to_owned(),
    };
    for (name, value) in &update.operations {
        let Some(value) = value else { continue };
        if name.is(NS_DAV, "resourcetype") {
            continue;
        }
        if kind == CollectionKind::Calendar
            && name.is(NS_CALDAV, "supported-calendar-component-set")
        {
            match requested_components(value) {
                Some(requested) => components = requested,
                None => {
                    return (StatusCode::FORBIDDEN, "unsupported calendar component")
                        .into_response();
                }
            }
        } else {
            match apply(&mut changes, name, Some(value), kind) {
                // Protected and inapplicable properties are ignored, as
                // before dead properties were stored.
                Status::Ok | Status::Protected | Status::Forbidden => {}
                Status::Conflict => {
                    return (StatusCode::FORBIDDEN, "invalid property value").into_response();
                }
                Status::TooLarge => {
                    return (StatusCode::INSUFFICIENT_STORAGE, "property value too large")
                        .into_response();
                }
            }
        }
    }

    let flatten = |v: Option<Option<String>>| v.flatten();
    match ctx
        .state
        .store
        .create_collection_with_properties(
            ctx.user.id,
            kind,
            slug,
            flatten(changes.display_name.clone()).as_deref(),
            flatten(changes.description.clone()).as_deref(),
            flatten(changes.color.clone()).as_deref(),
            &components,
            &changes.dead_changes(),
        )
        .await
    {
        Ok(_) => StatusCode::CREATED.into_response(),
        Err(err) => store_error(err),
    }
}
