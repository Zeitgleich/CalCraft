//! Sharing on the DAV side (docs/sharing.md), in the form Apple Calendar
//! uses (CalendarServer `caldav-sharing`):
//!
//! * `POST` of `CS:share` to one of the owner's collections adds, changes
//!   or removes sharees (by `mailto:` address or principal URL);
//! * every sharee gets an invitation in their notification collection
//!   (`/dav/notifications/<user>/`, `CS:notification-URL`), one resource
//!   per pending share, which they accept or decline with a `POST` of
//!   `CS:invite-reply` to their calendar or address book home (or by
//!   deleting the notification, which declines);
//! * accepted shares are collections in the sharee's home (see
//!   [`super::paths`]) with `CS:shared` in their resource type, the owner as
//!   `DAV:owner` and a `CS:invite` naming the owner and the sharee only;
//!   the owner's collection lists all sharees in `CS:invite` and has
//!   `CS:shared-owner`.
//!
//! The same shares are managed through the REST API and the web UI.

// Several helpers return a complete HTTP response as their error.
#![allow(clippy::result_large_err)]

use axum::{
    body::Body,
    extract::Request,
    http::{HeaderMap, StatusCode, header::CONTENT_TYPE},
    response::{IntoResponse, Response},
};
use calcraft_core::{CollectionKind, Username};
use calcraft_dav::{
    DavError, Element, Multistatus, PropName, PropValue, Propfind,
    sharing::{InviteReply, MAX_SHARE_BODY, ShareRequest, SharingPost},
    xml::{NS_CALDAV, NS_CALENDARSERVER as CS, NS_DAV, escape_text},
};
use calcraft_store::{Collection, Share, ShareAccess, StoreError, User};

use super::{
    Ctx, XML_CONTENT_TYPE, bad_request, method_not_allowed, multistatus, need_privileges,
    not_found,
    paths::{
        Resource, collection_href, home_href, href_of, notification_href, notifications_href,
        principal_href,
    },
    props::respond,
    read_body, store_error,
};
use crate::auth::now;

/// Whether a request body is XML (sharing POSTs; other POSTs are
/// iCalendar or attachments).
pub(super) fn is_xml(headers: &HeaderMap) -> bool {
    headers
        .get(CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .map(|value| value.trim().to_ascii_lowercase())
        .is_some_and(|value| value.starts_with("application/xml") || value.starts_with("text/xml"))
}

fn xml_response(status: StatusCode, body: String) -> Response {
    (status, [(CONTENT_TYPE, XML_CONTENT_TYPE)], Body::from(body)).into_response()
}

/// `403` with a CalendarServer precondition element.
fn cs_error(condition: &str) -> Response {
    xml_response(
        StatusCode::FORBIDDEN,
        format!(
            r#"<?xml version="1.0" encoding="utf-8"?><d:error xmlns:d="DAV:" xmlns:cs="{CS}"><cs:{condition}/></d:error>"#
        ),
    )
}

/// The path of an href (absolute URLs lose scheme and host).
fn href_path(href: &str) -> Option<&str> {
    match href.split_once("://") {
        Some((_, rest)) => rest.find('/').map(|i| &rest[i..]),
        None => href.starts_with('/').then_some(href),
    }
}

/// The user an href of a sharing request names: a principal URL of this
/// server, or a calendar user address (`mailto:`) of a user. Disabled
/// users and unknown addresses are `None`.
async fn sharee(ctx: Ctx<'_>, href: &str) -> Result<Option<User>, StoreError> {
    let store = &ctx.state.store;
    let user = if let Some(address) = href
        .get(..7)
        .filter(|scheme| scheme.eq_ignore_ascii_case("mailto:"))
        .map(|_| &href[7..])
    {
        let address = address.trim().to_ascii_lowercase();
        match &ctx.state.scheduling {
            Some(scheduling) => match scheduling
                .resolve(store, &format!("mailto:{address}"))
                .await?
            {
                crate::scheduling::Resolution::Local(user) => Some(user),
                _ => None,
            },
            None => store.user_by_email(&address).await?,
        }
    } else {
        let Some(name) = href_path(href)
            .and_then(|path| path.strip_prefix("/dav/principals/"))
            .map(|rest| rest.strip_suffix('/').unwrap_or(rest))
            .and_then(|name| name.parse::<Username>().ok())
        else {
            return Ok(None);
        };
        store.user_by_name(&name).await?
    };
    Ok(user.filter(|user| !user.disabled))
}

/// `POST` of `CS:share` to one of the owner's collections. All sharees
/// must be users of this server (else `403 CS:valid-sharee` and nothing
/// changes); the owner cannot share with themselves.
pub(super) async fn post_share(ctx: Ctx<'_>, collection: Collection, request: Request) -> Response {
    let href = collection_href(&collection, &ctx.user.username);
    // Only the owner shares (a sharee's view of the collection cannot).
    if collection.shared.is_some() {
        return need_privileges(Some(&href), "share");
    }
    let body = match read_body(request, MAX_SHARE_BODY).await {
        Ok(body) => body,
        Err(err) => return err.into_response(),
    };
    let request: ShareRequest = match SharingPost::parse(&body) {
        Ok(SharingPost::Share(request)) => request,
        Ok(SharingPost::InviteReply(_)) => {
            return bad_request(&DavError::BadRequest(
                "invite replies go to the calendar home",
            ));
        }
        Err(err) => return bad_request(&err),
    };
    let mut set = Vec::new();
    for entry in &request.set {
        match sharee(ctx, &entry.href).await {
            Ok(Some(user)) if user.id != ctx.user.id => {
                let access = if entry.read_write {
                    ShareAccess::ReadWrite
                } else {
                    ShareAccess::Read
                };
                set.push((user, access));
            }
            Ok(_) => return cs_error("valid-sharee"),
            Err(err) => return store_error(err),
        }
    }
    let mut remove = Vec::new();
    for href in &request.remove {
        match sharee(ctx, href).await {
            Ok(Some(user)) => remove.push(user),
            // Removing someone who is not a sharee changes nothing.
            Ok(None) => {}
            Err(err) => return store_error(err),
        }
    }
    // A subscription is shared read-only only; refused before anything
    // changes.
    if collection.read_only && set.iter().any(|(_, access)| access.writable()) {
        return cs_error("read-write-not-allowed");
    }
    // All entries in one transaction: any failure changes nothing.
    let set: Vec<(i64, ShareAccess)> = set
        .iter()
        .map(|(user, access)| (user.id, *access))
        .collect();
    let remove: Vec<i64> = remove.iter().map(|user| user.id).collect();
    match ctx
        .state
        .store
        .apply_shares(collection.id, ctx.user.id, &set, &remove, now())
        .await
    {
        Ok(Some(())) => StatusCode::OK.into_response(),
        Ok(None) => not_found(),
        Err(StoreError::ReadOnly) => cs_error("read-write-not-allowed"),
        Err(StoreError::NoSuchSharee) => cs_error("valid-sharee"),
        // The sharee declined or left a share of this owner recently.
        Err(StoreError::ShareDeclined) => cs_error("invite-declined-recently"),
        Err(StoreError::Limit(_)) => cs_error("too-many-invitations"),
        Err(err) => store_error(err),
    }
}

/// The pending or accepted share an invite reply answers.
async fn answered_share(
    ctx: Ctx<'_>,
    kind: CollectionKind,
    reply: &InviteReply,
) -> Result<Option<Share>, StoreError> {
    let store = &ctx.state.store;
    if let Some(id) = reply.in_reply_to.as_deref() {
        let share = if calcraft_store::valid_managed_id(id) {
            store.incoming_share(ctx.user.id, id).await?
        } else {
            None
        };
        return Ok(share.filter(|share| share.kind == kind));
    }
    // `CS:hosturl`: the collection's path in the owner's home.
    let Some(path) = reply.host_url.as_deref().and_then(href_path) else {
        return Ok(None);
    };
    Ok(store
        .incoming_shares(ctx.user.id)
        .await?
        .into_iter()
        .find(|share| {
            let owner_path = format!(
                "{}{}/",
                home_href(share.kind, &share.owner),
                share.owner_slug
            );
            share.kind == kind && (path == owner_path || Some(path) == owner_path.strip_suffix('/'))
        }))
}

/// `POST` of `CS:invite-reply` to the sharee's home: accepts (answering
/// with the new collection's URL in `CS:shared-as`) or declines (the share
/// is removed).
pub(super) async fn post_invite_reply(
    ctx: Ctx<'_>,
    kind: CollectionKind,
    request: Request,
) -> Response {
    let body = match read_body(request, MAX_SHARE_BODY).await {
        Ok(body) => body,
        Err(err) => return err.into_response(),
    };
    let reply = match SharingPost::parse(&body) {
        Ok(SharingPost::InviteReply(reply)) => reply,
        Ok(SharingPost::Share(_)) => {
            return bad_request(&DavError::BadRequest("share requests go to the collection"));
        }
        Err(err) => return bad_request(&err),
    };
    let share = match answered_share(ctx, kind, &reply).await {
        Ok(Some(share)) => share,
        Ok(None) => return cs_error("valid-invite"),
        Err(err) => return store_error(err),
    };
    let store = &ctx.state.store;
    if !reply.accepted {
        return match store
            .leave_share(ctx.user.id, &share.public_id, now())
            .await
        {
            Ok(_) => StatusCode::NO_CONTENT.into_response(),
            Err(err) => store_error(err),
        };
    }
    match store
        .accept_share(ctx.user.id, &share.public_id, now())
        .await
    {
        Ok(Some(share)) => {
            let slug = share
                .slug
                .as_ref()
                .map(ToString::to_string)
                .unwrap_or_default();
            let href = format!("{}{slug}/", home_href(share.kind, &ctx.user.username));
            xml_response(
                StatusCode::OK,
                format!(
                    r#"<?xml version="1.0" encoding="utf-8"?><cs:shared-as xmlns:d="DAV:" xmlns:cs="{CS}"><d:href>{}</d:href></cs:shared-as>"#,
                    escape_text(&href)
                ),
            )
        }
        Ok(None) => cs_error("valid-invite"),
        Err(err) => store_error(err),
    }
}

fn prop(namespace: &str, name: &str, value: PropValue) -> (PropName, PropValue) {
    (PropName::new(namespace, name), value)
}

fn access_element(access: ShareAccess) -> Element {
    Element::new(CS, "access").child(Element::new(
        CS,
        match access {
            ShareAccess::Read => "read",
            ShareAccess::ReadWrite => "read-write",
        },
    ))
}

fn person_name(username: &Username, display_name: Option<&str>) -> String {
    display_name.unwrap_or(username.as_str()).to_owned()
}

/// A `CS:user` entry of `CS:invite`.
fn invite_user(share: &Share) -> Element {
    let status = if share.accepted() {
        "invite-accepted"
    } else {
        "invite-noresponse"
    };
    Element::new(CS, "user")
        .child(Element::new(NS_DAV, "href").text(principal_href(&share.sharee)))
        .child(Element::new(CS, "common-name").text(person_name(
            &share.sharee,
            share.sharee_display_name.as_deref(),
        )))
        .child(Element::new(CS, status))
        .child(access_element(share.access))
}

/// Adds a resource type element to the `DAV:resourcetype` among `props`.
fn add_resourcetype(props: &mut [(PropName, PropValue)], element: Element) {
    if let Some((_, PropValue::Elements(types))) = props
        .iter_mut()
        .find(|(name, _)| name.is(NS_DAV, "resourcetype"))
    {
        types.push(element);
    }
}

/// Sharing properties of a collection: for the owner (`shares` are the
/// collection's), or for a sharee (`collection.shared`).
pub(super) fn collection_properties(
    props: &mut Vec<(PropName, PropValue)>,
    collection: &Collection,
    shares: &[&Share],
    user: &User,
) {
    if let Some(via) = &collection.shared {
        add_resourcetype(props, Element::new(CS, "shared"));
        let organizer = Element::new(CS, "organizer")
            .child(Element::new(NS_DAV, "href").text(principal_href(&via.owner)))
            .child(
                Element::new(CS, "common-name")
                    .text(person_name(&via.owner, via.owner_display_name.as_deref())),
            );
        let me = Element::new(CS, "user")
            .child(Element::new(NS_DAV, "href").text(principal_href(&user.username)))
            .child(Element::new(CS, "invite-accepted"))
            .child(access_element(via.access));
        props.push(prop(CS, "invite", PropValue::Elements(vec![organizer, me])));
        return;
    }
    if collection.kind == CollectionKind::Calendar {
        props.push(prop(
            CS,
            "allowed-sharing-modes",
            PropValue::Elements(vec![Element::new(CS, "can-be-shared")]),
        ));
    }
    if !shares.is_empty() {
        add_resourcetype(props, Element::new(CS, "shared-owner"));
    }
    props.push(prop(
        CS,
        "invite",
        PropValue::Elements(shares.iter().map(|share| invite_user(share)).collect()),
    ));
}

// --- notifications --------------------------------------------------------

/// An entity tag for the list of pending shares (the notification
/// collection's `getctag`).
fn list_tag(shares: &[Share]) -> String {
    let ids: Vec<&str> = shares.iter().map(|s| s.public_id.as_str()).collect();
    blake3::hash(ids.join(",").as_bytes()).to_hex()[..32].to_owned()
}

fn notification_etag(share: &Share) -> String {
    let input = format!("{}:{}", share.public_id, share.access.as_str());
    format!("\"{}\"", &blake3::hash(input.as_bytes()).to_hex()[..32])
}

fn shared_type(kind: CollectionKind) -> &'static str {
    match kind {
        CollectionKind::Calendar => "calendar",
        CollectionKind::Addressbook => "addressbook",
    }
}

/// The body of an invitation (CalendarServer `CS:notification`).
fn notification_body(share: &Share, user: &User) -> String {
    let dtstamp = calcraft_ical::itip::utc_stamp(share.created_at)
        .unwrap_or_else(|| "19700101T000000Z".to_owned());
    let host = format!(
        "{}{}/",
        home_href(share.kind, &share.owner),
        share.owner_slug
    );
    let access = match share.access {
        ShareAccess::Read => "read",
        ShareAccess::ReadWrite => "read-write",
    };
    let summary = share
        .collection_display_name
        .as_deref()
        .unwrap_or(share.owner_slug.as_str());
    let components = if share.kind == CollectionKind::Calendar {
        let comps: String = share
            .components
            .split(',')
            .filter(|c| !c.is_empty())
            .map(|c| format!(r#"<c:comp name="{}"/>"#, escape_text(c)))
            .collect();
        format!("<c:supported-calendar-component-set>{comps}</c:supported-calendar-component-set>")
    } else {
        String::new()
    };
    format!(
        concat!(
            r#"<?xml version="1.0" encoding="utf-8"?>"#,
            r#"<cs:notification xmlns:d="DAV:" xmlns:cs="{cs}" xmlns:c="{caldav}">"#,
            "<cs:dtstamp>{dtstamp}</cs:dtstamp>",
            r#"<cs:invite-notification shared-type="{kind}">"#,
            "<cs:uid>{uid}</cs:uid>",
            "<d:href>{me}</d:href>",
            "<cs:invite-noresponse/>",
            "<cs:access><cs:{access}/></cs:access>",
            "<cs:hosturl><d:href>{host}</d:href></cs:hosturl>",
            "<cs:organizer><d:href>{owner}</d:href><cs:common-name>{owner_name}</cs:common-name></cs:organizer>",
            "<cs:summary>{summary}</cs:summary>",
            "{components}",
            "</cs:invite-notification></cs:notification>"
        ),
        cs = CS,
        caldav = NS_CALDAV,
        dtstamp = dtstamp,
        kind = shared_type(share.kind),
        uid = share.public_id,
        me = escape_text(&principal_href(&user.username)),
        access = access,
        host = escape_text(&host),
        owner = escape_text(&principal_href(&share.owner)),
        owner_name = escape_text(&person_name(
            &share.owner,
            share.owner_display_name.as_deref()
        )),
        summary = escape_text(summary),
        components = components,
    )
}

fn privileges(names: &[&'static str]) -> PropValue {
    PropValue::Elements(
        names
            .iter()
            .map(|name| Element::new(NS_DAV, "privilege").child(Element::new(NS_DAV, name)))
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
        prop(
            NS_DAV,
            "current-user-privilege-set",
            privileges(&["read", "unbind", "read-current-user-privilege-set"]),
        ),
    ]
}

fn collection_props(user: &User, pending: &[Share]) -> Vec<(PropName, PropValue)> {
    let mut props = common(user);
    props.extend([
        prop(
            NS_DAV,
            "resourcetype",
            PropValue::Elements(vec![
                Element::new(NS_DAV, "collection"),
                Element::new(CS, "notification"),
            ]),
        ),
        prop(
            NS_DAV,
            "owner",
            PropValue::Href(principal_href(&user.username)),
        ),
        prop(CS, "getctag", PropValue::Text(list_tag(pending))),
    ]);
    props
}

fn item_props(user: &User, share: &Share) -> Vec<(PropName, PropValue)> {
    let mut props = common(user);
    let length = notification_body(share, user).len();
    props.extend([
        prop(NS_DAV, "resourcetype", PropValue::Elements(Vec::new())),
        prop(NS_DAV, "getetag", PropValue::Text(notification_etag(share))),
        prop(
            NS_DAV,
            "getcontenttype",
            PropValue::Text(XML_CONTENT_TYPE.to_owned()),
        ),
        prop(
            NS_DAV,
            "getcontentlength",
            PropValue::Text(length.to_string()),
        ),
        prop(
            CS,
            "notificationtype",
            PropValue::Elements(vec![
                Element::new(CS, "invite-notification")
                    .attr("shared-type", shared_type(share.kind)),
            ]),
        ),
    ]);
    props
}

async fn pending(ctx: Ctx<'_>) -> Result<Vec<Share>, StoreError> {
    Ok(ctx
        .state
        .store
        .incoming_shares(ctx.user.id)
        .await?
        .into_iter()
        .filter(|share| !share.accepted())
        .collect())
}

/// Requests to the notification collection and its invitations: PROPFIND,
/// GET/HEAD, DELETE (declines). Everything else is not allowed.
pub(super) async fn notifications(ctx: Ctx<'_>, resource: Resource, request: Request) -> Response {
    let user = ctx.user;
    match (request.method().as_str(), &resource) {
        ("PROPFIND", _) => {
            let depth = match calcraft_dav::Depth::parse(
                request
                    .headers()
                    .get("depth")
                    .map(axum::http::HeaderValue::as_bytes),
            ) {
                Ok(calcraft_dav::Depth::Infinity) => {
                    return super::dav_error(StatusCode::FORBIDDEN, "<d:propfind-finite-depth/>");
                }
                Ok(depth) => depth,
                Err(err) => return bad_request(&err),
            };
            let body = match read_body(request, calcraft_dav::MAX_PROPFIND_BODY).await {
                Ok(body) => body,
                Err(err) => return err.into_response(),
            };
            let query = match Propfind::parse(&body) {
                Ok(query) => query,
                Err(err) => return bad_request(&err),
            };
            let mut responses = Vec::new();
            match &resource {
                Resource::Notification(share) => {
                    responses.push(respond(
                        &href_of(&resource, &user.username),
                        &item_props(user, share),
                        &query,
                    ));
                }
                _ => {
                    let pending = match pending(ctx).await {
                        Ok(pending) => pending,
                        Err(err) => return store_error(err),
                    };
                    responses.push(respond(
                        &notifications_href(&user.username),
                        &collection_props(user, &pending),
                        &query,
                    ));
                    if depth == calcraft_dav::Depth::One {
                        for share in &pending {
                            responses.push(respond(
                                &notification_href(&user.username, share),
                                &item_props(user, share),
                                &query,
                            ));
                        }
                    }
                }
            }
            multistatus(&Multistatus {
                responses,
                sync_token: None,
            })
        }
        ("GET" | "HEAD", Resource::Notification(share)) => {
            let body = notification_body(share, user);
            let with_body = request.method() == axum::http::Method::GET;
            (
                StatusCode::OK,
                [
                    (CONTENT_TYPE, XML_CONTENT_TYPE.to_owned()),
                    (axum::http::header::ETAG, notification_etag(share)),
                ],
                if with_body { body } else { String::new() },
            )
                .into_response()
        }
        ("DELETE", Resource::Notification(share)) => {
            match ctx
                .state
                .store
                .leave_share(user.id, &share.public_id, now())
                .await
            {
                Ok(_) => StatusCode::NO_CONTENT.into_response(),
                Err(err) => store_error(err),
            }
        }
        ("REPORT", _) => super::dav_error(StatusCode::FORBIDDEN, "<d:supported-report/>"),
        _ => method_not_allowed(),
    }
}

/// The principal's `CS:notification-URL`.
pub(super) fn principal_properties(user: &User) -> (PropName, PropValue) {
    prop(
        CS,
        "notification-URL",
        PropValue::Href(notifications_href(&user.username)),
    )
}

/// The share of `collection` among `shares`.
pub(super) fn shares_of<'a>(shares: &'a [Share], collection: &Collection) -> Vec<&'a Share> {
    shares
        .iter()
        .filter(|share| share.collection_id == collection.id)
        .collect()
}

/// The owner's dead properties a sharee sees: only the calendar's time
/// zone (the owner's client settings – alarms, order, anything else – stay
/// private); the sharee's own position as `calendar-order`.
pub(super) fn sharee_dead_properties(
    collection: &Collection,
    stored: Vec<calcraft_store::DeadProperty>,
) -> Vec<calcraft_store::DeadProperty> {
    let mut visible: Vec<calcraft_store::DeadProperty> = stored
        .into_iter()
        .filter(|p| p.namespace == NS_CALDAV && p.name == "calendar-timezone")
        .collect();
    if let Some(order) = collection.shared.as_ref().and_then(|via| via.order) {
        visible.push(calcraft_store::DeadProperty {
            namespace: super::props::CALENDAR_ORDER.0.to_owned(),
            name: super::props::CALENDAR_ORDER.1.to_owned(),
            value: order.to_string(),
        });
    }
    visible
}

/// Whether the user sees `collection` through a read-only share of a
/// calendar: private events are then served as busy stubs
/// ([`crate::redact::sharee_view`]).
pub(super) fn hides_private(collection: &Collection) -> bool {
    collection.kind == CollectionKind::Calendar
        && collection
            .shared
            .as_ref()
            .is_some_and(|via| !via.access.writable())
}

/// The object as the user may see it: for read-only sharees private
/// events are busy stubs, with an ETag of the stub's bytes (it changes when
/// the stub changes, and never reveals the hidden content).
pub(super) fn sharee_item(
    collection: &Collection,
    item: calcraft_store::Item,
) -> calcraft_store::Item {
    if !hides_private(collection) {
        return item;
    }
    match crate::redact::sharee_view(&item.content) {
        Some(stub) => calcraft_store::Item {
            etag: super::etag(&stub),
            content: stub,
            name: item.name,
        },
        None => item,
    }
}
