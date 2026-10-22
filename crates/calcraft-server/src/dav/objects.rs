//! GET, HEAD, PUT and DELETE of calendar objects (RFC 4791 §5.3.2).

use axum::{
    body::Body,
    extract::Request,
    http::{
        HeaderMap, StatusCode,
        header::{CONTENT_TYPE, ETAG, IF_MATCH, IF_NONE_MATCH},
    },
    response::{IntoResponse, Response},
};
use calcraft_core::CollectionKind;
use calcraft_ical::{CalendarObject, INDEX_VERSION, IcalError, MAX_OBJECT_SIZE, VCard};
use calcraft_store::{Collection, NewItem, Precondition, PutOutcome, StoreError};

use crate::auth::now;

use super::{
    Ctx, dav_error, method_not_allowed, need_privileges, not_found,
    paths::{Resource, item_href},
    props::{components, content_type},
    read_body, store_error,
};

/// Strong entity tag derived from the stored bytes.
pub(crate) fn etag(content: &[u8]) -> String {
    format!("\"{}\"", &blake3::hash(content).to_hex()[..32])
}

pub(super) fn header(
    headers: &HeaderMap,
    name: impl axum::http::header::AsHeaderName,
) -> Option<&str> {
    headers
        .get(name)
        .and_then(|v| v.to_str().ok())
        .map(str::trim)
}

/// A conditional header in a form that is not supported.
pub(super) struct UnsupportedCondition;

impl IntoResponse for UnsupportedCondition {
    fn into_response(self) -> Response {
        (StatusCode::BAD_REQUEST, "malformed conditional header").into_response()
    }
}

/// Parses an entity-tag list (`"a", W/"b"`). With `weak`, the `W/` prefix
/// is dropped (weak comparison); otherwise weak tags are kept as they are
/// and can never equal the strong tags this server issues.
fn etag_list(value: &str, weak: bool) -> Result<Vec<String>, UnsupportedCondition> {
    const MAX_TAGS: usize = 32;
    let tags: Vec<String> = value
        .split(',')
        .map(str::trim)
        .map(|tag| {
            let opaque = tag.strip_prefix("W/").unwrap_or(tag);
            let valid = opaque.len() >= 2
                && opaque.starts_with('"')
                && opaque.ends_with('"')
                && !opaque[1..opaque.len() - 1].contains('"');
            if !valid {
                return Err(UnsupportedCondition);
            }
            Ok(if weak { opaque } else { tag }.to_owned())
        })
        .collect::<Result<_, _>>()?;
    if tags.is_empty() || tags.len() > MAX_TAGS {
        return Err(UnsupportedCondition);
    }
    Ok(tags)
}

/// Reads `If-Match` / `If-None-Match` (RFC 9110 §13.1.1, §13.1.2).
pub(super) fn precondition(headers: &HeaderMap) -> Result<Precondition, UnsupportedCondition> {
    match (header(headers, IF_MATCH), header(headers, IF_NONE_MATCH)) {
        (None, None) => Ok(Precondition::None),
        (None, Some("*")) => Ok(Precondition::MustNotExist),
        (None, Some(tags)) => Ok(Precondition::IfNoneMatch(etag_list(tags, true)?)),
        (Some("*"), None) => Ok(Precondition::MustExist),
        (Some(tags), None) => Ok(Precondition::IfMatch(etag_list(tags, false)?)),
        // Both headers at once are not used by DAV clients.
        (Some(_), Some(_)) => Err(UnsupportedCondition),
    }
}

pub async fn get(ctx: Ctx<'_>, resource: Resource, with_body: bool) -> Response {
    let (collection, name) = match resource {
        Resource::Item(collection, name) => (collection, name),
        Resource::InboxItem(name) => {
            return super::scheduling::get_message(ctx, &name, with_body).await;
        }
        _ => return method_not_allowed(),
    };
    match ctx
        .state
        .store
        .item_with_schedule_tag(collection.id, &name)
        .await
    {
        Ok(Some((item, schedule_tag))) => {
            let content_type = content_type(collection.kind);
            let body = if with_body {
                Body::from(item.content)
            } else {
                Body::empty()
            };
            let mut response = (
                StatusCode::OK,
                [(CONTENT_TYPE, content_type), (ETAG, item.etag.as_str())],
                body,
            )
                .into_response();
            // Only shown while scheduling is on (RFC 6638 §8.2).
            if ctx.state.scheduling.is_some()
                && let Some(tag) = schedule_tag
                && let Ok(value) = axum::http::HeaderValue::from_str(&tag)
            {
                response
                    .headers_mut()
                    .insert(super::scheduling::SCHEDULE_TAG, value);
            }
            response
        }
        Ok(None) => not_found(),
        Err(err) => store_error(err),
    }
}

/// What a PUT body was validated as, with the data needed for storage.
pub(crate) struct Validated {
    pub uid: String,
    pub component: &'static str,
    pub range_start: Option<i64>,
    pub range_end: Option<i64>,
    pub recurring: bool,
}

/// Why an object was not accepted by [`check_object`].
#[derive(Debug)]
pub(crate) enum Rejection {
    TooLarge,
    Invalid(IcalError),
    UnsupportedComponent,
}

impl std::fmt::Display for Rejection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::TooLarge => f.write_str("object too large"),
            Self::Invalid(err) => err.fmt(f),
            Self::UnsupportedComponent => {
                f.write_str("component type not supported by this calendar")
            }
        }
    }
}

/// XML prefix of the precondition namespace for a collection kind.
pub(super) fn ns(kind: CollectionKind) -> &'static str {
    match kind {
        CollectionKind::Calendar => "c",
        CollectionKind::Addressbook => "card",
    }
}

/// Largest accepted object for a collection kind.
pub(crate) fn max_object_size(kind: CollectionKind, max_vcard_bytes: usize) -> usize {
    match kind {
        CollectionKind::Calendar => MAX_OBJECT_SIZE,
        CollectionKind::Addressbook => max_vcard_bytes,
    }
}

/// Validates an object for the target collection. This is the one check
/// for stored objects: DAV PUT and imports both use it. vCards may be up
/// to `max_vcard_bytes` (`storage.max_vcard_bytes`) large.
pub(crate) fn check_object(
    collection: &Collection,
    body: &[u8],
    max_vcard_bytes: usize,
) -> Result<Validated, Rejection> {
    if body.len() > max_object_size(collection.kind, max_vcard_bytes) {
        return Err(Rejection::TooLarge);
    }
    let invalid = |err| match err {
        IcalError::TooLarge => Rejection::TooLarge,
        err => Rejection::Invalid(err),
    };
    match collection.kind {
        CollectionKind::Calendar => {
            let object = CalendarObject::parse(body).map_err(invalid)?;
            if !components(collection).contains(&object.kind.as_str()) {
                return Err(Rejection::UnsupportedComponent);
            }
            Ok(Validated {
                uid: object.uid,
                component: object.kind.as_str(),
                range_start: object.range.start,
                range_end: object.range.end,
                recurring: object.recurring,
            })
        }
        CollectionKind::Addressbook => {
            let card = VCard::parse(body).map_err(invalid)?;
            Ok(Validated {
                uid: card.uid,
                component: "VCARD",
                range_start: None,
                range_end: None,
                recurring: false,
            })
        }
    }
}

/// Validates the body for the target collection. On failure returns the
/// precondition element to report (RFC 4791 §5.3.2.1, RFC 6352 §6.3.2.1).
fn validate(
    collection: &Collection,
    body: &[u8],
    max_vcard_bytes: usize,
) -> Result<Validated, &'static str> {
    let calendar = collection.kind == CollectionKind::Calendar;
    check_object(collection, body, max_vcard_bytes).map_err(|rejection| match rejection {
        Rejection::TooLarge if calendar => "<c:max-resource-size/>",
        Rejection::TooLarge => "<card:max-resource-size/>",
        Rejection::UnsupportedComponent => "<c:supported-calendar-component/>",
        Rejection::Invalid(err) => {
            tracing::debug!(%err, "rejected object");
            if calendar {
                "<c:valid-calendar-data/>"
            } else {
                "<card:valid-address-data/>"
            }
        }
    })
}

pub async fn put(ctx: Ctx<'_>, resource: Resource, request: Request) -> Response {
    let Resource::Item(collection, name) = resource else {
        return method_not_allowed();
    };
    if collection.read_only {
        let href = item_href(&collection, &ctx.user.username, &name);
        return need_privileges(Some(&href), "write-content");
    }
    let kind = collection.kind;
    let headers = request.headers().clone();
    let (expected_type, unsupported) = match kind {
        CollectionKind::Calendar => ("text/calendar", "<c:supported-calendar-data/>"),
        CollectionKind::Addressbook => ("text/vcard", "<card:supported-address-data/>"),
    };
    if let Some(content_type) = header(&headers, CONTENT_TYPE)
        && !content_type.to_ascii_lowercase().starts_with(expected_type)
    {
        return dav_error(StatusCode::FORBIDDEN, unsupported);
    }
    let precondition = match precondition(&headers) {
        Ok(precondition) => precondition,
        Err(err) => return err.into_response(),
    };
    let too_large = format!("<{}:max-resource-size/>", ns(kind));
    let max_vcard_bytes = ctx.state.max_vcard_bytes;
    let body = match read_body(request, max_object_size(kind, max_vcard_bytes)).await {
        Ok(body) => body,
        Err(_) => return dav_error(StatusCode::FORBIDDEN, &too_large),
    };
    let validated = match validate(&collection, &body, max_vcard_bytes) {
        Ok(validated) => validated,
        Err(condition) => return dav_error(StatusCode::FORBIDDEN, condition),
    };

    let etag = etag(&body);
    let item = NewItem {
        uid: &validated.uid,
        component: validated.component,
        range_start: validated.range_start,
        range_end: validated.range_end,
        recurring: validated.recurring,
        index_version: INDEX_VERSION,
        etag: &etag,
        content: &body,
    };
    if kind == CollectionKind::Calendar
        && let Some(scheduling) = &ctx.state.scheduling
    {
        let if_tag = match super::scheduling::if_schedule_tag(&headers) {
            Ok(tag) => tag,
            Err(response) => return response,
        };
        let body = body.clone();
        return super::scheduling::put(
            ctx,
            scheduling,
            &collection,
            &name,
            body,
            item,
            precondition,
            if_tag,
        )
        .await;
    }
    match ctx
        .state
        .store
        .put_item(collection.id, &name, &item, &precondition)
        .await
    {
        Ok(outcome) => {
            let status = match outcome {
                PutOutcome::Created => StatusCode::CREATED,
                PutOutcome::Updated => StatusCode::NO_CONTENT,
            };
            (status, [(ETAG, etag)]).into_response()
        }
        Err(StoreError::UidConflict(other)) => {
            let href = item_href(&collection, &ctx.user.username, &other);
            let prefix = ns(kind);
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

pub async fn delete(ctx: Ctx<'_>, resource: Resource, request: Request) -> Response {
    let precondition = match precondition(request.headers()) {
        Ok(precondition) => precondition,
        Err(err) => return err.into_response(),
    };
    match resource {
        Resource::InboxItem(name) => {
            super::scheduling::delete_message(ctx, &name, &precondition).await
        }
        Resource::Item(collection, name)
            if !collection.read_only && collection.kind == CollectionKind::Calendar =>
        {
            let Some(scheduling) = &ctx.state.scheduling else {
                return delete_item(ctx, collection, name, precondition).await;
            };
            let if_tag = match super::scheduling::if_schedule_tag(request.headers()) {
                Ok(tag) => tag,
                Err(response) => return response,
            };
            super::scheduling::delete(ctx, scheduling, &collection, &name, precondition, if_tag)
                .await
        }
        Resource::Item(collection, _) if collection.read_only => {
            let href = super::paths::collection_href(&collection, &ctx.user.username);
            need_privileges(Some(&href), "unbind")
        }
        Resource::Item(collection, name) => delete_item(ctx, collection, name, precondition).await,
        Resource::Collection(collection) => {
            // Collections have no entity tag: only `If-Match: *` can hold.
            if !matches!(precondition, Precondition::None | Precondition::MustExist) {
                return StatusCode::PRECONDITION_FAILED.into_response();
            }
            match ctx
                .state
                .store
                .delete_collection_to_trash(collection.id, now())
                .await
            {
                Ok(()) => StatusCode::NO_CONTENT.into_response(),
                Err(err) => store_error(err),
            }
        }
        Resource::Unmapped(..) => not_found(),
        _ => method_not_allowed(),
    }
}

/// DELETE of an object: into the trash (unless disabled); for sync clients
/// it is deleted either way.
async fn delete_item(
    ctx: Ctx<'_>,
    collection: Collection,
    name: String,
    precondition: Precondition,
) -> Response {
    let kind = collection.kind;
    match ctx
        .state
        .store
        .delete_item_to_trash(collection.id, &name, &precondition, now(), |content| {
            crate::trash::summary(kind, content)
        })
        .await
    {
        Ok(true) => StatusCode::NO_CONTENT.into_response(),
        Ok(false) => not_found(),
        Err(err) => store_error(err),
    }
}
