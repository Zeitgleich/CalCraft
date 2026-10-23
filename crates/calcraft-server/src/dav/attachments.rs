//! CalDAV managed attachments (RFC 8607) on the DAV side:
//!
//! * `POST <object>?action=attachment-add|attachment-update|attachment-remove`
//!   (with `managed-id` and `rid`) changes the object's `ATTACH` properties
//!   and stores the uploaded file; the changed object goes through the same
//!   path as a PUT ([`super::objects::store_body`]), so validation, quota,
//!   ETags and scheduling (invitations to attendees) are the same.
//! * `GET`/`HEAD /dav/attachments/<managed-id>` serves a file to users who
//!   may read it ([`super::paths::resolve`]): always as a download, never
//!   rendered on the server's origin.
//! * PUTs may keep, drop or copy managed `ATTACH` properties of attachments
//!   the object may reference ([`check_object_attachments`]); other managed
//!   ids are refused.
//!
//! Only the store decides which attachments an object references (see
//! `calcraft-store/src/attachments.rs`); docs/attachments.md has the design.

// Several helpers return a complete HTTP response as their error.
#![allow(clippy::result_large_err)]

use axum::{
    body::{Body, Bytes},
    extract::Request,
    http::{
        HeaderMap, HeaderValue, StatusCode,
        header::{
            ACCEPT_RANGES, CACHE_CONTROL, CONTENT_DISPOSITION, CONTENT_LENGTH,
            CONTENT_SECURITY_POLICY, CONTENT_TYPE, ETAG, IF_NONE_MATCH, X_CONTENT_TYPE_OPTIONS,
        },
    },
    response::{IntoResponse, Response},
};
use calcraft_core::CollectionKind;
use calcraft_ical::{
    Component,
    attach::{self, AttachError, Rid},
    parse,
    write::to_string,
};
use calcraft_store::{AttachmentMeta, Collection, NewAttachment, Precondition, StoreError};
use http_body_util::{BodyExt, Full};

use super::{
    Ctx, dav_error, need_privileges, not_found,
    objects::{header, precondition},
    paths::item_href,
    read_body, service_unavailable, store_error,
};
use crate::{
    attachments::{Attachments, new_managed_id},
    auth::now,
};

/// Response header with the managed id of an added or updated attachment.
pub const CAL_MANAGED_ID: &str = "cal-managed-id";
/// Content Security Policy of downloads: nothing may run or load.
pub const DOWNLOAD_CSP: &str = "default-src 'none'; sandbox";
/// Attempts when the object changes while an attachment is added.
const ATTEMPTS: usize = 5;

/// The `action` of an attachment POST (RFC 8607 §3.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Action {
    Add,
    Update,
    Remove,
}

/// The query of an attachment POST.
#[derive(Debug)]
struct Query {
    action: Action,
    managed_id: Option<String>,
    rids: Option<Vec<Rid>>,
}

/// Whether a POST to an object is an attachment request at all (has an
/// `action` parameter); other POSTs are not supported there.
pub(super) fn is_attachment_post(query: Option<&str>) -> bool {
    query.is_some_and(|query| {
        query
            .split('&')
            .any(|pair| pair.split('=').next() == Some("action"))
    })
}

/// Decodes `%XX` escapes of a query value; only printable ASCII results.
fn decode(value: &str) -> Option<String> {
    let bytes = value.as_bytes();
    let mut out = String::with_capacity(value.len());
    let mut i = 0;
    while i < bytes.len() {
        let byte = if bytes[i] == b'%' {
            let byte = u8::from_str_radix(value.get(i + 1..i + 3)?, 16).ok()?;
            i += 3;
            byte
        } else {
            i += 1;
            bytes[i - 1]
        };
        if !byte.is_ascii_graphic() {
            return None;
        }
        out.push(char::from(byte));
    }
    Some(out)
}

fn precondition_error(condition: &str) -> Response {
    dav_error(StatusCode::FORBIDDEN, &format!("<c:{condition}/>"))
}

fn parse_query(query: &str) -> Result<Query, Response> {
    let bad = || (StatusCode::BAD_REQUEST, "malformed attachment request").into_response();
    if query.len() > 4096 {
        return Err(bad());
    }
    let (mut action, mut managed_id, mut rids) = (None, None, None);
    for pair in query.split('&') {
        let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
        let value = decode(value).ok_or_else(bad)?;
        match key {
            "action" if action.is_none() => {
                action = Some(match value.as_str() {
                    "attachment-add" => Action::Add,
                    "attachment-update" => Action::Update,
                    "attachment-remove" => Action::Remove,
                    _ => return Err(precondition_error("valid-action")),
                });
            }
            // Exactly once (RFC 8607 §3.3).
            "action" => return Err(precondition_error("valid-action")),
            "managed-id" if managed_id.is_none() => managed_id = Some(value),
            "managed-id" => return Err(precondition_error("valid-managed-id")),
            "rid" if rids.is_none() => {
                rids =
                    Some(attach::parse_rids(&value).map_err(|_| precondition_error("valid-rid"))?);
            }
            "rid" => return Err(precondition_error("valid-rid")),
            _ => {}
        }
    }
    let action = action.ok_or_else(|| precondition_error("valid-action"))?;
    match action {
        Action::Add if managed_id.is_some() => Err(bad()),
        Action::Update | Action::Remove if managed_id.is_none() => {
            Err(precondition_error("valid-managed-id"))
        }
        Action::Update if rids.is_some() => Err(precondition_error("valid-rid")),
        _ => Ok(Query {
            action,
            managed_id,
            rids,
        }),
    }
}

fn attach_error(err: AttachError) -> Response {
    match err {
        AttachError::InvalidRid => precondition_error("valid-rid"),
        AttachError::UnknownManagedId => precondition_error("valid-managed-id"),
        // Too many overrides: the object would get too large.
        AttachError::TooManyComponents => precondition_error("max-resource-size"),
    }
}

/// Applies the action to a copy of the object; `attach` is the new
/// `ATTACH` (add, update).
fn apply(
    tree: &Component,
    query: &Query,
    attach: Option<&calcraft_ical::Property>,
    max_per_object: usize,
) -> Result<Component, Response> {
    let mut tree = tree.clone();
    let rids = query.rids.as_deref();
    let id = query.managed_id.as_deref().unwrap_or_default();
    let result = match (query.action, attach) {
        (Action::Add, Some(attach)) => attach::add(&mut tree, attach, rids),
        (Action::Update, Some(attach)) => attach::replace(&mut tree, id, attach),
        (Action::Remove, _) => attach::remove(&mut tree, id, rids),
        // Not reached: add and update always come with a property.
        _ => Err(AttachError::UnknownManagedId),
    };
    result.map_err(attach_error)?;
    if attach::managed_ids(&tree).len() > max_per_object {
        return Err(precondition_error("max-attachments-per-resource"));
    }
    Ok(tree)
}

/// The file name of the upload, sanitised (RFC 6266 §4.3).
fn upload_filename(headers: &HeaderMap) -> String {
    let raw = headers
        .get(CONTENT_DISPOSITION)
        // RFC 8187 values are ASCII; some clients send raw UTF-8.
        .and_then(|value| std::str::from_utf8(value.as_bytes()).ok())
        .and_then(attach::disposition_filename)
        .unwrap_or_default();
    attach::sanitize_filename(&raw)
}

fn upload_media_type(headers: &HeaderMap) -> String {
    header(headers, CONTENT_TYPE)
        .and_then(attach::media_type)
        .unwrap_or_else(|| attach::OCTET_STREAM.to_owned())
}

/// `POST` to a calendar object with an `action` parameter.
pub(super) async fn post(
    ctx: Ctx<'_>,
    attachments: &Attachments,
    collection: Collection,
    name: String,
    request: Request,
) -> Response {
    if collection.kind != CollectionKind::Calendar {
        return super::method_not_allowed();
    }
    let query = match parse_query(request.uri().query().unwrap_or_default()) {
        Ok(query) => query,
        Err(response) => return response,
    };
    if collection.read_only {
        let href = item_href(&collection, &ctx.user.username, &name);
        return need_privileges(Some(&href), "write-content");
    }
    let client_precondition = match precondition(request.headers()) {
        Ok(precondition) => precondition,
        Err(err) => return err.into_response(),
    };
    let store = &ctx.state.store;
    let max_per_object = attachments.max_per_object();

    // Everything that can fail without the upload is checked first.
    let item = match store.item(collection.id, &name).await {
        Ok(Some(item)) => item,
        Ok(None) => return not_found(),
        Err(err) => return store_error(err),
    };
    if !client_precondition.holds(Some(&item.etag)) {
        return StatusCode::PRECONDITION_FAILED.into_response();
    }
    let Ok(tree) = parse(&item.content) else {
        return precondition_error("valid-calendar-data");
    };
    if let Some(scheduling) = &ctx.state.scheduling {
        match super::scheduling::is_attendee(ctx, scheduling, &tree).await {
            Ok(false) => {}
            Ok(true) => {
                let href = item_href(&collection, &ctx.user.username, &name);
                return need_privileges(Some(&href), "write-content");
            }
            Err(response) => return response,
        }
    }
    let placeholder = attach::managed_property("x", "x", "x", attach::OCTET_STREAM, 1);
    let probe = (query.action != Action::Remove).then_some(&placeholder);
    if let Err(response) = apply(&tree, &query, probe, max_per_object) {
        return response;
    }

    // The upload (add, update).
    let upload = if query.action == Action::Remove {
        None
    } else {
        let headers = request.headers().clone();
        let max = attachments.config.max_bytes;
        let declared = header(&headers, CONTENT_LENGTH).and_then(|v| v.parse::<u64>().ok());
        if declared.is_some_and(|length| length > u64::try_from(max).unwrap_or(u64::MAX)) {
            return precondition_error("max-attachment-size");
        }
        // Bounds the memory of uploads in flight. Never waits: a user with
        // slow uploads holds at most a few of the slots, everybody else gets
        // `503` with `Retry-After` while the server is at its limit.
        let Some(_slot) = attachments.uploads.try_take(ctx.user.id) else {
            return service_unavailable();
        };
        let body = match read_body(request, max).await {
            Ok(body) => body,
            Err(_) => return precondition_error("max-attachment-size"),
        };
        if body.is_empty() {
            return (StatusCode::BAD_REQUEST, "empty attachment").into_response();
        }
        let managed_id = new_managed_id();
        let filename = upload_filename(&headers);
        let media_type = upload_media_type(&headers);
        let new = NewAttachment {
            managed_id: &managed_id,
            owner_id: ctx.user.id,
            filename: &filename,
            media_type: &media_type,
            content: &body,
            now: now(),
        };
        match store
            .insert_attachment(&new, attachments.max_user_bytes())
            .await
        {
            Ok(()) => {}
            Err(StoreError::Limit(_)) => {
                return dav_error(StatusCode::INSUFFICIENT_STORAGE, "<d:quota-not-exceeded/>");
            }
            Err(err) => return store_error(err),
        }
        let meta = AttachmentMeta {
            managed_id,
            owner_id: ctx.user.id,
            filename,
            media_type,
            size: i64::try_from(body.len()).unwrap_or(i64::MAX),
        };
        Some(meta)
    };

    let response = change_object(
        ctx,
        attachments,
        &collection,
        &name,
        &query,
        &client_precondition,
        upload.as_ref(),
        item,
    )
    .await;
    if let Some(meta) = &upload
        && !response.status().is_success()
        && let Err(err) = store
            .delete_unreferenced_attachment(ctx.user.id, &meta.managed_id)
            .await
    {
        // Removed by the background purge later.
        tracing::warn!(%err, "removing a failed attachment upload failed");
    }
    response
}

/// Stores the object with the action applied; retried when it changes
/// concurrently (two uploads at once both end up in it).
#[allow(clippy::too_many_arguments)]
async fn change_object(
    ctx: Ctx<'_>,
    attachments: &Attachments,
    collection: &Collection,
    name: &str,
    query: &Query,
    client_precondition: &Precondition,
    upload: Option<&AttachmentMeta>,
    mut item: calcraft_store::Item,
) -> Response {
    let attach = upload.map(|meta| attachments.property(meta));
    for attempt in 0..ATTEMPTS {
        if attempt > 0 {
            crate::scheduling::deliver::backoff(attempt).await;
            item = match ctx.state.store.item(collection.id, name).await {
                Ok(Some(item)) => item,
                Ok(None) => return not_found(),
                Err(err) => return store_error(err),
            };
            if !client_precondition.holds(Some(&item.etag)) {
                return StatusCode::PRECONDITION_FAILED.into_response();
            }
        }
        let Ok(tree) = parse(&item.content) else {
            return precondition_error("valid-calendar-data");
        };
        let tree = match apply(&tree, query, attach.as_ref(), attachments.max_per_object()) {
            Ok(tree) => tree,
            Err(response) => return response,
        };
        let Ok(text) = to_string(&tree) else {
            return precondition_error("valid-calendar-data");
        };
        let guard = Precondition::IfMatch(vec![item.etag.clone()]);
        let stored = super::objects::store_body(
            ctx,
            collection,
            name,
            Bytes::from(text.into_bytes()),
            guard,
            None,
        )
        .await;
        if stored.status() == StatusCode::PRECONDITION_FAILED {
            continue;
        }
        if !stored.status().is_success() {
            return stored;
        }
        let status = match query.action {
            Action::Add => StatusCode::CREATED,
            Action::Update => StatusCode::OK,
            Action::Remove => StatusCode::NO_CONTENT,
        };
        let mut response = status.into_response();
        let headers = response.headers_mut();
        for name in [
            ETAG,
            axum::http::HeaderName::from_static(super::SCHEDULE_TAG),
        ] {
            if let Some(value) = stored.headers().get(&name) {
                headers.insert(name, value.clone());
            }
        }
        if let Some(meta) = upload
            && let Ok(value) = HeaderValue::from_str(&meta.managed_id)
        {
            headers.insert(CAL_MANAGED_ID, value);
        }
        return response;
    }
    service_unavailable()
}

/// Checks the managed attachments of an object about to be stored: at most
/// `max_per_object`, and only those it may reference (the owner's own,
/// those it references already) or that its stored version already
/// mentions (kept as they are, e.g. after an import; they grant nothing).
/// Anything else is `CALDAV:valid-managed-id-parameter` (RFC 8607 §3.11).
/// Returns the object with the referenced `ATTACH` properties in the
/// server's form if that differs from the body.
pub(super) async fn check_object_attachments(
    ctx: Ctx<'_>,
    collection: &Collection,
    name: &str,
    body: &[u8],
    validated: &super::Validated,
) -> Result<Option<Bytes>, Response> {
    let ids = &validated.managed_ids;
    let Some(attachments) = &ctx.state.attachments else {
        return Ok(None);
    };
    if ids.is_empty() || collection.kind != CollectionKind::Calendar {
        return Ok(None);
    }
    if ids.len() > attachments.max_per_object() {
        return Err(precondition_error("max-attachments-per-resource"));
    }
    let store = &ctx.state.store;
    let referable = store
        .referable_attachments(collection.id, name, collection.owner_id, ids)
        .await
        .map_err(store_error)?;
    let unknown: Vec<&String> = ids
        .iter()
        .filter(|id| !referable.iter().any(|m| &m.managed_id == *id))
        .collect();
    if !unknown.is_empty() {
        let mentioned = match store.item(collection.id, name).await {
            Ok(Some(old)) => parse(&old.content)
                .map(|tree| attach::managed_ids(&tree))
                .unwrap_or_default(),
            Ok(None) => Vec::new(),
            Err(err) => return Err(store_error(err)),
        };
        if unknown.iter().any(|id| !mentioned.contains(id)) {
            return Err(precondition_error("valid-managed-id-parameter"));
        }
    }
    if referable.is_empty() {
        return Ok(None);
    }
    let Ok(mut tree) = parse(body) else {
        return Ok(None);
    };
    let canonical = |id: &str| {
        referable
            .iter()
            .find(|meta| meta.managed_id == id)
            .map(|meta| attachments.property(meta))
    };
    if !attach::canonicalize(&mut tree, &canonical) {
        return Ok(None);
    }
    match to_string(&tree) {
        Ok(text) => Ok(Some(Bytes::from(text.into_bytes()))),
        Err(_) => Err(precondition_error("valid-calendar-data")),
    }
}

/// The entity tag of an attachment: its content never changes.
fn attachment_etag(meta: &AttachmentMeta) -> String {
    format!("\"{}\"", meta.managed_id)
}

fn download_headers(meta: &AttachmentMeta, response: &mut Response) {
    let headers = response.headers_mut();
    let pairs = [
        (
            CONTENT_TYPE,
            attach::served_media_type(&meta.media_type).to_owned(),
        ),
        (
            CONTENT_DISPOSITION,
            attach::disposition_header(&meta.filename),
        ),
        (X_CONTENT_TYPE_OPTIONS, "nosniff".to_owned()),
        (CONTENT_SECURITY_POLICY, DOWNLOAD_CSP.to_owned()),
        (CACHE_CONTROL, "private, no-store".to_owned()),
        (ACCEPT_RANGES, "none".to_owned()),
        (ETAG, attachment_etag(meta)),
    ];
    for (name, value) in pairs {
        if let Ok(value) = HeaderValue::from_str(&value) {
            headers.insert(name, value);
        }
    }
}

/// `GET`/`HEAD` of an attachment the user may read.
pub(super) async fn get(
    ctx: Ctx<'_>,
    attachments: &Attachments,
    meta: &AttachmentMeta,
    headers: &HeaderMap,
    with_body: bool,
) -> Response {
    let etag = attachment_etag(meta);
    let matches = header(headers, IF_NONE_MATCH).is_some_and(|value| {
        value == "*"
            || value
                .split(',')
                .map(|tag| tag.trim().trim_start_matches("W/"))
                .any(|tag| tag == etag)
    });
    if matches {
        let mut response = StatusCode::NOT_MODIFIED.into_response();
        download_headers(meta, &mut response);
        response.headers_mut().remove(CONTENT_TYPE);
        return response;
    }
    let body = if with_body {
        // The file stays in memory until it is sent: bounded like uploads.
        let Some(slot) = attachments.downloads.try_take(ctx.user.id) else {
            return service_unavailable();
        };
        match ctx
            .state
            .store
            .attachment_content(&meta.managed_id, ctx.user.id)
            .await
        {
            Ok(Some(content)) => {
                // The slot is released when the body is dropped (sent or
                // the connection closed), not when this handler returns.
                let body = Full::new(Bytes::from(content)).map_frame(move |frame| {
                    let _ = &slot;
                    frame
                });
                Body::new(body)
            }
            // Removed or no longer readable since the path was resolved.
            Ok(None) => return not_found(),
            Err(err) => return store_error(err),
        }
    } else {
        Body::empty()
    };
    let mut response = (StatusCode::OK, body).into_response();
    download_headers(meta, &mut response);
    if !with_body && let Ok(length) = HeaderValue::from_str(&meta.size.to_string()) {
        response.headers_mut().insert(CONTENT_LENGTH, length);
    }
    response
}

#[cfg(test)]
mod tests;
