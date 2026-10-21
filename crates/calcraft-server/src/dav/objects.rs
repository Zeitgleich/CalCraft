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
use calcraft_ical::{CalendarObject, IcalError, MAX_OBJECT_SIZE};
use calcraft_store::{NewItem, Precondition, PutOutcome, StoreError};

use super::{
    Ctx, dav_error, method_not_allowed, not_found,
    paths::{Resource, item_href},
    props::{CALENDAR_CONTENT_TYPE, components},
    read_body, store_error,
};

/// Strong entity tag derived from the stored bytes.
fn etag(content: &[u8]) -> String {
    format!("\"{}\"", &blake3::hash(content).to_hex()[..32])
}

fn header(headers: &HeaderMap, name: impl axum::http::header::AsHeaderName) -> Option<&str> {
    headers
        .get(name)
        .and_then(|v| v.to_str().ok())
        .map(str::trim)
}

/// A conditional header in a form that is not supported.
struct UnsupportedCondition;

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
fn precondition(headers: &HeaderMap) -> Result<Precondition, UnsupportedCondition> {
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
    let Resource::Item(collection, name) = resource else {
        return method_not_allowed();
    };
    match ctx.state.store.item(collection.id, &name).await {
        Ok(Some(item)) => {
            let content_type = match collection.kind {
                CollectionKind::Calendar => CALENDAR_CONTENT_TYPE,
                CollectionKind::Addressbook => "text/vcard; charset=utf-8",
            };
            let body = if with_body {
                Body::from(item.content)
            } else {
                Body::empty()
            };
            (
                StatusCode::OK,
                [(CONTENT_TYPE, content_type), (ETAG, item.etag.as_str())],
                body,
            )
                .into_response()
        }
        Ok(None) => not_found(),
        Err(err) => store_error(err),
    }
}

pub async fn put(ctx: Ctx<'_>, resource: Resource, request: Request) -> Response {
    let Resource::Item(collection, name) = resource else {
        return method_not_allowed();
    };
    if collection.kind != CollectionKind::Calendar {
        return (
            StatusCode::NOT_IMPLEMENTED,
            "address book objects are not supported yet",
        )
            .into_response();
    }
    let headers = request.headers().clone();
    if let Some(content_type) = header(&headers, CONTENT_TYPE)
        && !content_type
            .to_ascii_lowercase()
            .starts_with("text/calendar")
    {
        return dav_error(StatusCode::FORBIDDEN, "<c:supported-calendar-data/>");
    }
    let precondition = match precondition(&headers) {
        Ok(precondition) => precondition,
        Err(err) => return err.into_response(),
    };
    let body = match read_body(request, MAX_OBJECT_SIZE).await {
        Ok(body) => body,
        Err(_) => return dav_error(StatusCode::FORBIDDEN, "<c:max-resource-size/>"),
    };

    let object = match CalendarObject::parse(&body) {
        Ok(object) => object,
        Err(IcalError::TooLarge) => {
            return dav_error(StatusCode::FORBIDDEN, "<c:max-resource-size/>");
        }
        Err(err) => {
            tracing::debug!(%err, "rejected calendar object");
            return dav_error(StatusCode::FORBIDDEN, "<c:valid-calendar-data/>");
        }
    };
    if !components(&collection).contains(&object.kind.as_str()) {
        return dav_error(StatusCode::FORBIDDEN, "<c:supported-calendar-component/>");
    }

    let etag = etag(&body);
    let item = NewItem {
        uid: &object.uid,
        component: object.kind.as_str(),
        range_start: object.range.start,
        range_end: object.range.end,
        etag: &etag,
        content: &body,
    };
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
            dav_error(
                StatusCode::FORBIDDEN,
                &format!("<c:no-uid-conflict><d:href>{href}</d:href></c:no-uid-conflict>"),
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
        Resource::Item(collection, name) => {
            match ctx
                .state
                .store
                .delete_item(collection.id, &name, &precondition)
                .await
            {
                Ok(true) => StatusCode::NO_CONTENT.into_response(),
                Ok(false) => not_found(),
                Err(err) => store_error(err),
            }
        }
        Resource::Collection(collection) => {
            // Collections have no entity tag: only `If-Match: *` can hold.
            if !matches!(precondition, Precondition::None | Precondition::MustExist) {
                return StatusCode::PRECONDITION_FAILED.into_response();
            }
            match ctx.state.store.delete_collection(collection.id).await {
                Ok(()) => StatusCode::NO_CONTENT.into_response(),
                Err(err) => store_error(err),
            }
        }
        Resource::Unmapped(..) => not_found(),
        _ => method_not_allowed(),
    }
}
