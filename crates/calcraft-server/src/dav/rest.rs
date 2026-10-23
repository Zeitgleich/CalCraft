//! The DAV object handlers as the REST API uses them (`api/events.rs`).
//!
//! The web UI's event changes must not get a second, weaker write path.
//! The API therefore builds the request a DAV client would send (PUT,
//! DELETE or MOVE of one object, with `If-Match`) and runs it through the
//! very same handlers, after resolving the path with the same
//! authorization rules ([`super::paths::resolve`]): read-only shares and
//! subscriptions, sharee rules for scheduling objects, validation,
//! managed attachments, quota, trash and scheduling (invitations,
//! cancellations) are exactly those of DAV. The API only translates the
//! response.

use axum::{
    body::Body,
    extract::Request,
    http::{
        HeaderMap, Method,
        header::{CONTENT_TYPE, IF_MATCH, IF_NONE_MATCH},
    },
    response::Response,
};
use calcraft_core::{CollectionKind, Slug};
use calcraft_store::{Collection, Item, StoreError, User};

use super::{
    Ctx, not_found,
    paths::{self, Resource},
};
use crate::app::AppState;

/// A calendar of the user's home at `slug`: own (subscriptions included)
/// or shared with them and accepted.
pub(crate) async fn calendar(
    state: &AppState,
    user: &User,
    slug: &Slug,
) -> Result<Option<Collection>, StoreError> {
    let path = format!(
        "{}{slug}/",
        paths::home_href(CollectionKind::Calendar, &user.username)
    );
    Ok(match paths::resolve(state, user, &path).await? {
        Some(Resource::Collection(collection)) => Some(collection),
        _ => None,
    })
}

/// The object as the user may see it (busy stubs for read-only sharees).
pub(crate) fn visible_item(collection: &Collection, item: Item) -> Item {
    super::sharing::sharee_item(collection, item)
}

/// Whether private events of `collection` are busy stubs for the user.
pub(crate) fn hides_private(collection: &Collection) -> bool {
    super::sharing::hides_private(collection)
}

/// The precondition an API change sends: `If-Match` with the ETag the UI
/// last saw, or `If-None-Match: *` for a new object.
#[derive(Debug, Clone, Copy)]
pub(crate) enum Expect<'a> {
    Etag(&'a str),
    New,
}

fn request(method: Method, path: &str, expect: Expect<'_>, body: Body) -> Request {
    let mut builder = Request::builder().method(method).uri(path);
    builder = match expect {
        Expect::Etag(etag) => builder.header(IF_MATCH, etag),
        Expect::New => builder.header(IF_NONE_MATCH, "*"),
    };
    builder
        .header(CONTENT_TYPE, "text/calendar; charset=utf-8")
        .body(body)
        .unwrap_or_default()
}

async fn item(
    state: &AppState,
    user: &User,
    collection: &Collection,
    name: &str,
) -> Option<Resource> {
    if !super::valid_item_name(name) {
        return None;
    }
    let path = paths::item_href(collection, &user.username, name);
    match paths::resolve(state, user, &path).await {
        Ok(Some(resource @ Resource::Item(..))) => Some(resource),
        _ => None,
    }
}

/// PUT of an object, as a DAV client would send it.
pub(crate) async fn put(
    state: &AppState,
    user: &User,
    collection: &Collection,
    name: &str,
    body: String,
    expect: Expect<'_>,
) -> Response {
    let Some(resource) = item(state, user, collection, name).await else {
        return not_found();
    };
    let path = paths::item_href(collection, &user.username, name);
    let ctx = Ctx { state, user };
    super::objects::put(
        ctx,
        resource,
        request(Method::PUT, &path, expect, Body::from(body)),
    )
    .await
}

/// DELETE of an object (into the trash), as a DAV client would send it.
pub(crate) async fn delete(
    state: &AppState,
    user: &User,
    collection: &Collection,
    name: &str,
    etag: &str,
) -> Response {
    let Some(resource) = item(state, user, collection, name).await else {
        return not_found();
    };
    let path = paths::item_href(collection, &user.username, name);
    let ctx = Ctx { state, user };
    super::objects::delete(
        ctx,
        resource,
        request(Method::DELETE, &path, Expect::Etag(etag), Body::empty()),
    )
    .await
}

/// MOVE of an object to `target` (same name, no overwrite).
pub(crate) async fn move_to(
    state: &AppState,
    user: &User,
    collection: &Collection,
    name: &str,
    target: &Slug,
    etag: &str,
) -> Response {
    let Some(resource) = item(state, user, collection, name).await else {
        return not_found();
    };
    let path = paths::item_href(collection, &user.username, name);
    let destination = format!(
        "{}{target}/{name}",
        paths::home_href(CollectionKind::Calendar, &user.username)
    );
    let mut request = request(
        Method::from_bytes(b"MOVE").unwrap_or_default(),
        &path,
        Expect::Etag(etag),
        Body::empty(),
    );
    let headers = request.headers_mut();
    if let (Ok(destination), Ok(depth), Ok(overwrite)) =
        (destination.parse(), "0".parse(), "F".parse())
    {
        headers.insert("destination", destination);
        headers.insert("depth", depth);
        headers.insert("overwrite", overwrite);
    }
    let ctx = Ctx { state, user };
    super::copymove::handle(ctx, resource, request, super::copymove::Mode::Move).await
}

/// Download of a managed attachment the user may read (the DAV GET with
/// its headers: always a download, sandboxed). `None` if attachments are
/// off or the user may not read it.
pub(crate) async fn attachment(
    state: &AppState,
    user: &User,
    managed_id: &str,
    headers: &HeaderMap,
) -> Result<Option<Response>, StoreError> {
    let Some(managed) = &state.attachments else {
        return Ok(None);
    };
    let path = paths::attachment_href(managed_id);
    let Some(Resource::Attachment(meta)) = paths::resolve(state, user, &path).await? else {
        return Ok(None);
    };
    let ctx = Ctx { state, user };
    Ok(Some(
        super::attachments::get(ctx, managed, &meta, headers, true).await,
    ))
}

/// Whether `name` is an object name the server accepts.
pub(crate) fn valid_name(name: &str) -> bool {
    super::valid_item_name(name)
}
