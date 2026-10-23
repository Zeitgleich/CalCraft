//! DAV request handling.
//!
//! Every resource below `/dav/` belongs to exactly one user. Requests for
//! another user's resources are answered with 404, so that account names
//! cannot be probed. Resolution and the ownership check live in [`paths`];
//! collections shared with a user are part of that user's own home
//! ([`sharing`]).

mod attachments;
mod collections;
mod copymove;
mod objects;
mod paths;
mod propfind;
mod props;
mod report;
#[cfg(feature = "api")]
pub(crate) mod rest;
mod scheduling;
mod sharing;
mod sync;

use axum::{
    body::{Body, Bytes, to_bytes},
    extract::{Request, State},
    http::{
        HeaderValue, Method, StatusCode,
        header::{ALLOW, AUTHORIZATION, CONTENT_TYPE, RETRY_AFTER, WWW_AUTHENTICATE},
    },
    response::{IntoResponse, Response},
};
use calcraft_dav::{DavError, Multistatus};
use calcraft_store::StoreError;

use crate::{app::AppState, auth::AuthOutcome, net::ClientKey};
use paths::Resource;

pub(crate) use objects::{Rejection, Validated, check_object, etag};
pub use paths::{principal_href, valid_item_name};
#[cfg(feature = "api")]
pub(crate) use props::calendar_order;
pub use scheduling::SCHEDULE_TAG;

/// Methods the DAV handler routes, as advertised in `Allow`. The
/// authorization matrix test (`tests/authz_matrix.rs`) fails for any method
/// listed here that it does not cover.
pub const ALLOWED_METHODS: &str = "OPTIONS, GET, HEAD, PUT, DELETE, PROPFIND, PROPPATCH, REPORT, MKCALENDAR, MKCOL, COPY, MOVE, POST";
/// DAV compliance classes.
const DAV_CLASSES: &str = "1, 3, calendar-access, addressbook, extended-mkcol";
/// Compliance class with scheduling enabled (RFC 6638 §2).
const CLASS_SCHEDULING: &str = ", calendar-auto-schedule";
/// Compliance class with managed attachments enabled (RFC 8607 §6.1).
const CLASS_ATTACHMENTS: &str = ", calendar-managed-attachments";
/// Apple's sharing (CalendarServer), with sharing enabled.
const CLASS_SHARING: &str = ", calendarserver-sharing";
const WWW_AUTHENTICATE_VALUE: &str = "Basic realm=\"CalCraft\", charset=\"UTF-8\"";
const XML_CONTENT_TYPE: &str = "application/xml; charset=utf-8";

/// Entry point for every request below `/dav`.
pub async fn handle(State(state): State<AppState>, request: Request) -> Response {
    if request.method() == Method::OPTIONS {
        return options(
            state.scheduling.is_some(),
            state.attachments.is_some(),
            state.sharing,
        );
    }

    let header = request
        .headers()
        .get(AUTHORIZATION)
        .map(HeaderValue::as_bytes);
    let client = request.extensions().get::<ClientKey>().copied();
    let user = match state.auth.authenticate(header, client).await {
        AuthOutcome::Authenticated(user) => user,
        AuthOutcome::Denied => return unauthorized(),
        // A locked account or a client with too many failed logins looks
        // like wrong credentials (no account oracle).
        AuthOutcome::LockedOut => return unauthorized(),
        AuthOutcome::Error => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
        AuthOutcome::Busy => return service_unavailable(),
    };

    let path = request.uri().path().to_owned();
    let resource = match paths::resolve(&state, &user, &path).await {
        Ok(Some(resource)) => resource,
        Ok(None) => return not_found(),
        Err(err) => return store_error(err),
    };

    let ctx = Ctx {
        state: &state,
        user: &user,
    };
    // Attachments are only downloaded; nothing else applies to them.
    if let (Resource::Attachment(meta), Some(managed)) = (&resource, &state.attachments) {
        return match request.method().as_str() {
            "GET" => attachments::get(ctx, managed, meta, request.headers(), true).await,
            "HEAD" => attachments::get(ctx, managed, meta, request.headers(), false).await,
            _ => method_not_allowed(),
        };
    }
    if matches!(
        resource,
        Resource::Notifications | Resource::Notification(_)
    ) {
        return sharing::notifications(ctx, resource, request).await;
    }
    let sharing_post = state.sharing && sharing::is_xml(request.headers());
    match request.method().as_str() {
        "PROPFIND" => propfind::handle(ctx, resource, request).await,
        "PROPPATCH" => collections::proppatch(ctx, resource, request).await,
        "MKCALENDAR" => collections::mkcalendar(ctx, resource, request).await,
        "MKCOL" => collections::mkcol(ctx, resource, request).await,
        "REPORT" => report::handle(ctx, resource, request).await,
        "GET" => objects::get(ctx, resource, true).await,
        "HEAD" => objects::get(ctx, resource, false).await,
        "PUT" => objects::put(ctx, resource, request).await,
        "DELETE" => objects::delete(ctx, resource, request).await,
        "COPY" => copymove::handle(ctx, resource, request, copymove::Mode::Copy).await,
        "MOVE" => copymove::handle(ctx, resource, request, copymove::Mode::Move).await,
        "POST" => match (resource, &state.scheduling, &state.attachments) {
            (Resource::Outbox, Some(scheduling), _) => {
                scheduling::post(ctx, scheduling, request).await
            }
            (Resource::Collection(collection), ..) if sharing_post => {
                sharing::post_share(ctx, collection, request).await
            }
            (Resource::Home(kind), ..) if sharing_post => {
                sharing::post_invite_reply(ctx, kind, request).await
            }
            (Resource::Item(collection, name), _, Some(managed))
                if attachments::is_attachment_post(request.uri().query()) =>
            {
                attachments::post(ctx, managed, collection, name, request).await
            }
            _ => method_not_allowed(),
        },
        _ => method_not_allowed(),
    }
}

/// Request context passed to the handlers.
#[derive(Clone, Copy)]
struct Ctx<'a> {
    state: &'a AppState,
    user: &'a calcraft_store::User,
}

fn options(scheduling: bool, attachments: bool, sharing: bool) -> Response {
    let mut classes = DAV_CLASSES.to_owned();
    if scheduling {
        classes.push_str(CLASS_SCHEDULING);
    }
    if attachments {
        classes.push_str(CLASS_ATTACHMENTS);
    }
    if sharing {
        classes.push_str(CLASS_SHARING);
    }
    (
        StatusCode::OK,
        [("dav", classes.as_str()), (ALLOW.as_str(), ALLOWED_METHODS)],
    )
        .into_response()
}

fn unauthorized() -> Response {
    (
        StatusCode::UNAUTHORIZED,
        [(WWW_AUTHENTICATE, WWW_AUTHENTICATE_VALUE)],
        "authentication required",
    )
        .into_response()
}

fn not_found() -> Response {
    (StatusCode::NOT_FOUND, "not found").into_response()
}

fn method_not_allowed() -> Response {
    (StatusCode::METHOD_NOT_ALLOWED, [(ALLOW, ALLOWED_METHODS)]).into_response()
}

fn bad_request(err: &DavError) -> Response {
    match err {
        DavError::TooLarge => StatusCode::PAYLOAD_TOO_LARGE.into_response(),
        DavError::BadRequest(reason) => (StatusCode::BAD_REQUEST, *reason).into_response(),
        DavError::Unsupported(_) => dav_error(StatusCode::FORBIDDEN, "<c:supported-filter/>"),
    }
}

/// A `DAV:error` body with a precondition element (RFC 4918 §16).
fn dav_error(status: StatusCode, condition: &str) -> Response {
    (
        status,
        [(CONTENT_TYPE, XML_CONTENT_TYPE)],
        format!(
            r#"<?xml version="1.0" encoding="utf-8"?><d:error xmlns:d="DAV:" xmlns:c="urn:ietf:params:xml:ns:caldav" xmlns:card="urn:ietf:params:xml:ns:carddav">{condition}</d:error>"#
        ),
    )
        .into_response()
}

/// `403` with `DAV:need-privileges` (RFC 3744 §7.1.1): the resource (a
/// subscription calendar or one of its objects) is read-only.
fn need_privileges(href: Option<&str>, privilege: &str) -> Response {
    let resource = href.map_or_else(String::new, |href| {
        format!(
            "<d:resource><d:href>{href}</d:href><d:privilege><d:{privilege}/></d:privilege></d:resource>"
        )
    });
    dav_error(
        StatusCode::FORBIDDEN,
        &format!("<d:need-privileges>{resource}</d:need-privileges>"),
    )
}

/// Transient overload: clients retry (RFC 9110 §15.6.4, §10.2.3).
fn service_unavailable() -> Response {
    (StatusCode::SERVICE_UNAVAILABLE, [(RETRY_AFTER, "1")]).into_response()
}

fn store_error(err: StoreError) -> Response {
    match err {
        StoreError::PreconditionFailed => StatusCode::PRECONDITION_FAILED.into_response(),
        StoreError::Limit(reason) => (StatusCode::INSUFFICIENT_STORAGE, reason).into_response(),
        // RFC 4918 §11.5; the transaction was rolled back.
        StoreError::Full => {
            tracing::error!("storage full: database or disk has no space left");
            StatusCode::INSUFFICIENT_STORAGE.into_response()
        }
        StoreError::CollectionExists => method_not_allowed(),
        StoreError::ReadOnly => need_privileges(None, "write-content"),
        // The share went away while the request was in flight: the
        // collection is gone for this user, like any path they cannot see.
        StoreError::ShareRevoked => not_found(),
        StoreError::Busy => {
            tracing::error!("storage busy: database stayed locked");
            service_unavailable()
        }
        err => {
            tracing::error!(%err, sqlite_code = err.sqlite_code(), "storage error");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

fn multistatus(multistatus: &Multistatus) -> Response {
    (
        StatusCode::MULTI_STATUS,
        [(CONTENT_TYPE, XML_CONTENT_TYPE)],
        Body::from(multistatus.to_xml()),
    )
        .into_response()
}

/// The request body exceeded its limit.
struct BodyTooLarge;

impl IntoResponse for BodyTooLarge {
    fn into_response(self) -> Response {
        StatusCode::PAYLOAD_TOO_LARGE.into_response()
    }
}

/// Reads the request body up to `limit` bytes.
async fn read_body(request: Request, limit: usize) -> Result<Bytes, BodyTooLarge> {
    to_bytes(request.into_body(), limit)
        .await
        .map_err(|_| BodyTooLarge)
}
