//! DAV request handling.
//!
//! Every resource below `/dav/` belongs to exactly one user. Requests for
//! another user's resources are answered with 404, so that account names
//! cannot be probed. Resolution and the ownership check live in [`paths`].

mod collections;
mod objects;
mod paths;
mod propfind;
mod props;
mod report;

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

use crate::{app::AppState, auth::AuthOutcome};

pub use paths::valid_item_name;

const ALLOWED_METHODS: &str =
    "OPTIONS, GET, HEAD, PUT, DELETE, PROPFIND, PROPPATCH, REPORT, MKCALENDAR";
/// DAV compliance classes. `addressbook` follows with CardDAV.
const DAV_CLASSES: &str = "1, 3, calendar-access";
const WWW_AUTHENTICATE_VALUE: &str = "Basic realm=\"CalCraft\", charset=\"UTF-8\"";
const XML_CONTENT_TYPE: &str = "application/xml; charset=utf-8";

/// Entry point for every request below `/dav`.
pub async fn handle(State(state): State<AppState>, request: Request) -> Response {
    if request.method() == Method::OPTIONS {
        return options();
    }

    let header = request
        .headers()
        .get(AUTHORIZATION)
        .map(HeaderValue::as_bytes);
    let user = match state.auth.authenticate(header).await {
        AuthOutcome::Authenticated(user) => user,
        AuthOutcome::Denied => return unauthorized(),
        AuthOutcome::LockedOut => {
            return (
                StatusCode::TOO_MANY_REQUESTS,
                [(RETRY_AFTER, "900")],
                "too many failed login attempts",
            )
                .into_response();
        }
        AuthOutcome::Error => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
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
    match request.method().as_str() {
        "PROPFIND" => propfind::handle(ctx, resource, request).await,
        "PROPPATCH" => collections::proppatch(ctx, resource, request).await,
        "MKCALENDAR" => collections::mkcalendar(ctx, resource, request).await,
        "REPORT" => report::handle(ctx, resource, request).await,
        "GET" => objects::get(ctx, resource, true).await,
        "HEAD" => objects::get(ctx, resource, false).await,
        "PUT" => objects::put(ctx, resource, request).await,
        "DELETE" => objects::delete(ctx, resource, request).await,
        _ => method_not_allowed(),
    }
}

/// Request context passed to the handlers.
#[derive(Clone, Copy)]
struct Ctx<'a> {
    state: &'a AppState,
    user: &'a calcraft_store::User,
}

fn options() -> Response {
    (
        StatusCode::OK,
        [("dav", DAV_CLASSES), (ALLOW.as_str(), ALLOWED_METHODS)],
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
            r#"<?xml version="1.0" encoding="utf-8"?><d:error xmlns:d="DAV:" xmlns:c="urn:ietf:params:xml:ns:caldav">{condition}</d:error>"#
        ),
    )
        .into_response()
}

fn store_error(err: StoreError) -> Response {
    match err {
        StoreError::PreconditionFailed => StatusCode::PRECONDITION_FAILED.into_response(),
        StoreError::Limit(reason) => (StatusCode::INSUFFICIENT_STORAGE, reason).into_response(),
        StoreError::CollectionExists => method_not_allowed(),
        err => {
            tracing::error!(%err, "storage error");
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
