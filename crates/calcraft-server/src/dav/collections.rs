//! MKCALENDAR (RFC 4791 §5.3.1) and PROPPATCH (RFC 4918 §9.2).
//!
//! Only a fixed set of descriptive properties can be written. Everything
//! else – in particular ACL- and membership-related properties – is
//! protected, so a PROPPATCH can never change who has access.

use axum::{
    extract::Request,
    http::StatusCode,
    response::{IntoResponse, Response},
};
use calcraft_core::{CollectionKind, Slug};
use calcraft_dav::{
    MAX_PROPPATCH_BODY, Multistatus, PropName, PropertyUpdate, Response as DavResponse,
    tree::Node,
    xml::{NS_APPLE_ICAL, NS_CALDAV, NS_CARDDAV, NS_DAV},
};
use calcraft_ical::ComponentKind;

use super::{
    Ctx, bad_request, dav_error, method_not_allowed, multistatus,
    paths::{Resource, href_of},
    props::WRITABLE,
    read_body, store_error,
};

const MAX_DISPLAY_NAME: usize = 256;
const MAX_DESCRIPTION: usize = 4096;

/// A validated change to one editable collection property.
#[derive(Debug, Default)]
struct Changes {
    display_name: Option<Option<String>>,
    description: Option<Option<String>>,
    color: Option<Option<String>>,
}

/// Invisible formatting characters that can disguise names: zero-width
/// characters, bidirectional overrides and isolates, byte order mark.
fn is_format_char(c: char) -> bool {
    matches!(
        c,
        '\u{061C}' | '\u{200B}'..='\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2060}'..='\u{2069}' | '\u{FEFF}'
    )
}

fn clean_text(node: &Node, max: usize) -> Option<String> {
    let text = node.text.trim();
    (text.chars().count() <= max && !text.chars().any(|c| c.is_control() || is_format_char(c)))
        .then(|| text.to_owned())
}

/// `#RRGGBB` or `#RRGGBBAA`, as used by Apple and Thunderbird.
fn clean_color(node: &Node) -> Option<String> {
    let text = node.text.trim();
    let hex = text.strip_prefix('#')?;
    (matches!(hex.len(), 6 | 8) && hex.bytes().all(|b| b.is_ascii_hexdigit()))
        .then(|| text.to_ascii_uppercase())
}

fn is_writable(name: &PropName, kind: CollectionKind) -> bool {
    WRITABLE.iter().any(|(ns, n)| name.is(ns, n))
        && !(kind == CollectionKind::Calendar && name.is(NS_CARDDAV, "addressbook-description"))
        && !(kind == CollectionKind::Addressbook
            && (name.is(NS_CALDAV, "calendar-description")
                || name.is(NS_APPLE_ICAL, "calendar-color")))
}

/// Applies one operation to `changes`; returns the status code for it.
fn apply(
    changes: &mut Changes,
    name: &PropName,
    value: Option<&Node>,
    kind: CollectionKind,
) -> u16 {
    if !is_writable(name, kind) {
        return 403;
    }
    let text = |max| match value {
        None => Some(None),
        Some(node) => clean_text(node, max).map(|t| (!t.is_empty()).then_some(t)),
    };
    let result = if name.is(NS_DAV, "displayname") {
        text(MAX_DISPLAY_NAME).map(|v| changes.display_name = Some(v))
    } else if name.is(NS_APPLE_ICAL, "calendar-color") {
        match value {
            None => Some(None),
            Some(node) => clean_color(node).map(Some),
        }
        .map(|v| changes.color = Some(v))
    } else {
        text(MAX_DESCRIPTION).map(|v| changes.description = Some(v))
    };
    if result.is_some() { 200 } else { 409 }
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
    let results: Vec<(PropName, u16)> = update
        .operations
        .iter()
        .map(|(name, value)| {
            let status = apply(&mut changes, name, value.as_ref(), collection.kind);
            (name.clone(), status)
        })
        .collect();

    // PROPPATCH is atomic: if anything fails, nothing is applied.
    let failed = results.iter().any(|(_, status)| *status != 200);
    let mut response = DavResponse::new(href_of(&resource, &ctx.user.username));
    for (name, status) in results {
        match (failed, status) {
            (false, _) => response.found.push((name, calcraft_dav::PropValue::Empty)),
            (true, 200) => response.failed.push((name, 424)),
            (true, status) => response.failed.push((name, status)),
        }
    }
    if !failed
        && let Err(err) = ctx
            .state
            .store
            .update_collection(
                collection.id,
                changes.display_name.as_ref().map(Option::as_deref),
                changes.description.as_ref().map(Option::as_deref),
                changes.color.as_ref().map(Option::as_deref),
            )
            .await
    {
        return store_error(err);
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
        } else if apply(&mut changes, name, Some(value), kind) == 409 {
            return (StatusCode::FORBIDDEN, "invalid property value").into_response();
        }
        // Other properties (e.g. calendar-timezone, calendar-order) are not stored.
    }

    let flatten = |v: Option<Option<String>>| v.flatten();
    match ctx
        .state
        .store
        .create_collection(
            ctx.user.id,
            kind,
            slug,
            flatten(changes.display_name).as_deref(),
            flatten(changes.description).as_deref(),
            flatten(changes.color).as_deref(),
            &components,
        )
        .await
    {
        Ok(_) => StatusCode::CREATED.into_response(),
        Err(err) => store_error(err),
    }
}
