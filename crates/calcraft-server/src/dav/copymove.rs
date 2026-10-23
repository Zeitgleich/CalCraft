//! COPY and MOVE of calendar objects and vCards (RFC 4918 §9.8, §9.9)
//! between collections of the authenticated user.
//!
//! Scope: one object (Depth 0) into an existing collection of the same
//! user and kind. Collections cannot be copied or moved (403), other
//! servers are refused (502). A `Destination` that does not resolve to an
//! object path in one of the user's collections – another user's
//! collection, a user that does not exist, a missing collection – is
//! answered with the same 409, so the destination is no existence oracle.

use axum::{
    extract::Request,
    http::{
        HeaderMap, StatusCode,
        header::{ETAG, HOST},
    },
    response::{IntoResponse, Response},
};
use calcraft_core::CollectionKind;
use calcraft_store::{StoreError, Transfer};

use super::{
    Ctx, dav_error, method_not_allowed, need_privileges, not_found,
    objects::{header, ns, precondition},
    paths::{self, DestinationError, Resource, item_href},
    store_error,
};

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Copy,
    Move,
}

fn bad_request(reason: &'static str) -> Response {
    (StatusCode::BAD_REQUEST, reason).into_response()
}

/// The destination does not resolve to an object path in a collection of
/// the user. One response for every such case (RFC 4918 §9.8.5: 409 when
/// intermediate collections are missing).
fn conflict() -> Response {
    (
        StatusCode::CONFLICT,
        "destination collection does not exist",
    )
        .into_response()
}

/// `Overwrite` (RFC 4918 §10.6); absent means `T`.
fn overwrite(headers: &HeaderMap) -> Option<bool> {
    match header(headers, "overwrite") {
        None | Some("T") => Some(true),
        Some("F") => Some(false),
        Some(_) => None,
    }
}

pub async fn handle(ctx: Ctx<'_>, resource: Resource, request: Request, mode: Mode) -> Response {
    let (source, name) = match resource {
        Resource::Item(collection, name) => (collection, name),
        Resource::Collection(_) => {
            return (
                StatusCode::FORBIDDEN,
                "collections cannot be copied or moved",
            )
                .into_response();
        }
        Resource::Unmapped(..) => return not_found(),
        Resource::Root
        | Resource::Principal
        | Resource::Home(_)
        | Resource::Inbox
        | Resource::InboxItem(_)
        | Resource::Outbox
        | Resource::Attachment(_)
        | Resource::Notifications
        | Resource::Notification(_) => return method_not_allowed(),
    };
    // Objects cannot be moved out of a subscription (copies are fine).
    if mode == Mode::Move && source.read_only {
        let href = paths::collection_href(&source, &ctx.user.username);
        return need_privileges(Some(&href), "unbind");
    }
    let headers = request.headers();
    // Cheap early answer for a missing source; the store checks again.
    let meta = match ctx.state.store.item_meta(source.id, &name).await {
        Ok(Some(meta)) => meta,
        Ok(None) => return not_found(),
        Err(err) => return store_error(err),
    };

    if !matches!(header(headers, "depth"), None | Some("0")) {
        return bad_request("only Depth: 0 is supported for objects");
    }
    let Some(overwrite) = overwrite(headers) else {
        return bad_request("Overwrite must be T or F");
    };
    let source_precondition = match precondition(headers) {
        Ok(precondition) => precondition,
        Err(err) => return err.into_response(),
    };
    let Some(destination) = header(headers, "destination") else {
        return bad_request("Destination header required");
    };
    let authority = header(headers, HOST).or_else(|| request.uri().authority().map(|a| a.as_str()));
    let path = match paths::destination_path(destination, authority) {
        Ok(path) => path,
        Err(DestinationError::Malformed) => return bad_request("malformed Destination"),
        Err(DestinationError::OtherServer) => {
            return (StatusCode::BAD_GATEWAY, "Destination is on another server").into_response();
        }
    };
    let (target, target_name) = match paths::resolve(ctx.state, ctx.user, path).await {
        Ok(Some(Resource::Item(collection, name))) => (collection, name),
        // Collections, homes and the like: an object cannot replace them.
        Ok(Some(_)) => {
            return (
                StatusCode::FORBIDDEN,
                "Destination must be an object in a collection",
            )
                .into_response();
        }
        Ok(None) => return conflict(),
        Err(err) => return store_error(err),
    };
    if target.read_only {
        let href = paths::collection_href(&target, &ctx.user.username);
        return need_privileges(Some(&href), "bind");
    }
    // Objects stay with their owner: no copies or moves between the user's
    // own collections and collections shared with them, or between
    // collections of different owners (clients fall back to GET and PUT).
    if target.owner_id != source.owner_id {
        return (
            StatusCode::FORBIDDEN,
            "objects cannot be copied or moved to another owner's collection",
        )
            .into_response();
    }
    if target.kind != source.kind {
        let condition = match target.kind {
            CollectionKind::Calendar => "<c:supported-calendar-data/>",
            CollectionKind::Addressbook => "<card:supported-address-data/>",
        };
        return dav_error(StatusCode::FORBIDDEN, condition);
    }
    let prefix = ns(target.kind);
    let limit = super::objects::max_object_size(target.kind, ctx.state.max_vcard_bytes);
    if usize::try_from(meta.size).is_ok_and(|size| size > limit) {
        return dav_error(
            StatusCode::FORBIDDEN,
            &format!("<{prefix}:max-resource-size/>"),
        );
    }

    let transfer = Transfer {
        owner_id: source.owner_id,
        source_share: source.shared.as_ref().map(|via| via.share_id),
        destination_share: target.shared.as_ref().map(|via| via.share_id),
        source_collection: source.id,
        source_name: &name,
        source_precondition: &source_precondition,
        destination_collection: target.id,
        destination_name: &target_name,
        overwrite,
    };
    let result = match mode {
        Mode::Copy => ctx.state.store.copy_item(&transfer).await,
        Mode::Move => ctx.state.store.move_item(&transfer).await,
    };
    match result {
        Ok(Some(outcome)) => {
            let status = if outcome.created {
                StatusCode::CREATED
            } else {
                StatusCode::NO_CONTENT
            };
            (status, [(ETAG, outcome.etag)]).into_response()
        }
        Ok(None) => not_found(),
        Err(StoreError::SameResource) => {
            (StatusCode::FORBIDDEN, "source and destination are the same").into_response()
        }
        Err(StoreError::MissingDestination) => conflict(),
        Err(StoreError::UnsupportedComponent) => {
            dav_error(StatusCode::FORBIDDEN, "<c:supported-calendar-component/>")
        }
        Err(StoreError::UidConflict(other)) => {
            let href = item_href(&target, &ctx.user.username, &other);
            dav_error(
                StatusCode::FORBIDDEN,
                &format!(
                    "<{prefix}:no-uid-conflict><d:href>{href}</d:href></{prefix}:no-uid-conflict>"
                ),
            )
        }
        Err(err) => store_error(err),
    }
}
