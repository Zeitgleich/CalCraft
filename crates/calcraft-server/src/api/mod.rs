//! REST API (JSON) used by the web UI.
//!
//! Authentication is a session cookie (`HttpOnly`, `SameSite=Strict`).
//! State-changing requests additionally need the [`CSRF_HEADER`], which a
//! cross-site form cannot send. DAV endpoints never accept the cookie, so a
//! browser session cannot be abused against them.

mod account;
mod admin;
mod collections;
#[cfg(feature = "oidc")]
mod oidc;
mod transfer;
mod trash;

use axum::{
    Json, Router,
    body::Bytes,
    extract::{DefaultBodyLimit, FromRequest, FromRequestParts, Request, State},
    http::{HeaderMap, Method, StatusCode, header, request::Parts},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{delete, get, patch, post},
};
use calcraft_core::api::{AuthMethods, CSRF_HEADER, ErrorBody, VersionInfo};
use calcraft_store::{StoreError, User};
use serde::de::DeserializeOwned;

use crate::{app::AppState, auth::now, net::ClientKey, secrets::hash_secret};

pub const SESSION_COOKIE: &str = "calcraft_session";
/// Lifetime of a web session.
pub const SESSION_TTL: i64 = 12 * 3600;
const MAX_JSON_BODY: usize = 64 * 1024;

/// The OpenAPI description, written by hand and checked against the
/// router in tests.
pub const OPENAPI: &str = include_str!("openapi.json");

/// Routes as `(method, path)`, used by the OpenAPI coverage test.
pub const ROUTES: &[(&str, &str)] = &[
    ("get", "/v1/version"),
    ("get", "/v1/auth/methods"),
    ("post", "/v1/auth/login"),
    ("get", "/v1/auth/oidc/login"),
    ("get", "/v1/auth/oidc/callback"),
    ("post", "/v1/auth/logout"),
    ("get", "/v1/me"),
    ("post", "/v1/me/password"),
    ("post", "/v1/me/totp/setup"),
    ("post", "/v1/me/totp/enable"),
    ("post", "/v1/me/totp/disable"),
    ("get", "/v1/app-passwords"),
    ("post", "/v1/app-passwords"),
    ("delete", "/v1/app-passwords/{id}"),
    ("get", "/v1/collections"),
    ("post", "/v1/collections"),
    ("patch", "/v1/collections/{kind}/{slug}"),
    ("delete", "/v1/collections/{kind}/{slug}"),
    ("get", "/v1/collections/{kind}/{slug}/export"),
    ("post", "/v1/collections/{kind}/{slug}/import"),
    ("get", "/v1/trash"),
    ("delete", "/v1/trash"),
    ("post", "/v1/trash/{id}/restore"),
    ("delete", "/v1/trash/{id}"),
    ("get", "/v1/admin/users"),
    ("post", "/v1/admin/users"),
    ("patch", "/v1/admin/users/{username}"),
    ("get", "/v1/admin/update"),
];

/// Builds the API router, to be nested under `/api`.
pub fn router() -> Router<AppState> {
    let router = Router::new()
        .route("/openapi.json", get(openapi))
        .route("/v1/version", get(version))
        .route("/v1/auth/methods", get(auth_methods))
        .route("/v1/auth/login", post(account::login))
        .route("/v1/auth/logout", post(account::logout))
        .route("/v1/me", get(account::me))
        .route("/v1/me/password", post(account::change_password))
        .route("/v1/me/totp/setup", post(account::totp_setup))
        .route("/v1/me/totp/enable", post(account::totp_enable))
        .route("/v1/me/totp/disable", post(account::totp_disable))
        .route(
            "/v1/app-passwords",
            get(account::list_app_passwords).post(account::create_app_password),
        )
        .route(
            "/v1/app-passwords/{id}",
            delete(account::delete_app_password),
        )
        .route(
            "/v1/collections",
            get(collections::list).post(collections::create),
        )
        .route(
            "/v1/collections/{kind}/{slug}",
            patch(collections::update).delete(collections::delete),
        )
        .route(
            "/v1/collections/{kind}/{slug}/export",
            get(transfer::export),
        )
        .route(
            "/v1/collections/{kind}/{slug}/import",
            post(transfer::import),
        )
        .route("/v1/trash", get(trash::list).delete(trash::empty))
        .route("/v1/trash/{id}/restore", post(trash::restore))
        .route("/v1/trash/{id}", delete(trash::delete_entry))
        .route("/v1/admin/users", get(admin::list).post(admin::create))
        .route("/v1/admin/users/{username}", patch(admin::update))
        .route("/v1/admin/update", get(admin::update_check));
    // Without the `oidc` feature these paths fall through to 404.
    #[cfg(feature = "oidc")]
    let router = router
        .route("/v1/auth/oidc/login", get(oidc::login))
        .route("/v1/auth/oidc/callback", get(oidc::callback));
    router
        .fallback(|| async { ApiError::NotFound })
        .layer(middleware::from_fn(require_csrf_header))
        .layer(DefaultBodyLimit::max(MAX_JSON_BODY))
}

async fn openapi() -> impl IntoResponse {
    ([(header::CONTENT_TYPE, "application/json")], OPENAPI)
}

async fn version() -> Json<VersionInfo> {
    Json(VersionInfo {
        name: calcraft_core::NAME.to_owned(),
        version: calcraft_core::VERSION.to_owned(),
        source: calcraft_core::SOURCE_URL.to_owned(),
    })
}

async fn auth_methods(State(state): State<AppState>) -> Json<AuthMethods> {
    #[cfg(feature = "oidc")]
    let oidc = state.oidc.as_ref().map(|oidc| oidc.method());
    #[cfg(not(feature = "oidc"))]
    let oidc = None;
    Json(AuthMethods {
        password: state.auth.password_login(),
        oidc,
    })
}

/// Rejects state-changing requests without the CSRF header.
async fn require_csrf_header(request: Request, next: Next) -> Response {
    let safe = matches!(
        *request.method(),
        Method::GET | Method::HEAD | Method::OPTIONS
    );
    if !safe && !request.headers().contains_key(CSRF_HEADER) {
        return ApiError::Csrf.into_response();
    }
    next.run(request).await
}

/// Errors returned as `{"error": code, "message": text}`.
#[derive(Debug)]
pub enum ApiError {
    BadRequest(&'static str),
    InvalidJson(String),
    Unauthorized,
    InvalidCredentials,
    TotpRequired,
    PasswordLoginDisabled,
    Forbidden,
    Csrf,
    NotFound,
    Conflict(&'static str),
    /// A trash entry cannot be restored; the code says why.
    RestoreConflict(calcraft_store::TrashConflict),
    Limit(&'static str),
    TooLarge,
    /// An uploaded file that cannot be used, with the reason.
    InvalidData(String),
    /// Temporarily unavailable (database busy); retry later.
    Unavailable,
    Internal,
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let retry = matches!(self, Self::Unavailable);
        let (status, code, message) = match self {
            Self::BadRequest(message) => {
                (StatusCode::BAD_REQUEST, "bad_request", message.to_owned())
            }
            Self::InvalidJson(message) => (StatusCode::BAD_REQUEST, "invalid_json", message),
            Self::Unauthorized => (
                StatusCode::UNAUTHORIZED,
                "unauthorized",
                "login required".to_owned(),
            ),
            Self::InvalidCredentials => (
                StatusCode::UNAUTHORIZED,
                "invalid_credentials",
                "wrong username, password or code, or too many attempts".to_owned(),
            ),
            Self::TotpRequired => (
                StatusCode::UNAUTHORIZED,
                "totp_required",
                "a two-factor code is required".to_owned(),
            ),
            Self::PasswordLoginDisabled => (
                StatusCode::FORBIDDEN,
                "password_login_disabled",
                "password login is disabled; use single sign-on".to_owned(),
            ),
            Self::Forbidden => (StatusCode::FORBIDDEN, "forbidden", "not allowed".to_owned()),
            Self::Csrf => (
                StatusCode::FORBIDDEN,
                "csrf",
                format!("missing {CSRF_HEADER} header"),
            ),
            Self::NotFound => (StatusCode::NOT_FOUND, "not_found", "not found".to_owned()),
            Self::Conflict(message) => (StatusCode::CONFLICT, "conflict", message.to_owned()),
            Self::RestoreConflict(conflict) => (
                StatusCode::CONFLICT,
                conflict.code(),
                conflict.message().to_owned(),
            ),
            Self::Limit(message) => (
                StatusCode::INSUFFICIENT_STORAGE,
                "limit",
                message.to_owned(),
            ),
            Self::TooLarge => (
                StatusCode::PAYLOAD_TOO_LARGE,
                "too_large",
                "request body too large".to_owned(),
            ),
            Self::InvalidData(message) => (StatusCode::BAD_REQUEST, "invalid_data", message),
            Self::Unavailable => (
                StatusCode::SERVICE_UNAVAILABLE,
                "unavailable",
                "temporarily unavailable, try again".to_owned(),
            ),
            Self::Internal => (
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal",
                "internal error".to_owned(),
            ),
        };
        let body = ErrorBody {
            error: code.to_owned(),
            message,
        };
        let mut response = (status, Json(body)).into_response();
        if retry {
            response.headers_mut().insert(
                header::RETRY_AFTER,
                axum::http::HeaderValue::from_static("1"),
            );
        }
        response
    }
}

impl From<StoreError> for ApiError {
    fn from(err: StoreError) -> Self {
        match err {
            StoreError::Limit(message) => Self::Limit(message),
            StoreError::Full => {
                tracing::error!("storage full: database or disk has no space left");
                Self::Limit("storage is full")
            }
            StoreError::CollectionExists => Self::Conflict("collection already exists"),
            StoreError::TrashConflict(conflict) => Self::RestoreConflict(conflict),
            StoreError::UserExists(_) => Self::Conflict("user already exists"),
            StoreError::Busy => {
                tracing::error!("storage busy: database stayed locked");
                Self::Unavailable
            }
            err => {
                tracing::error!(%err, sqlite_code = err.sqlite_code(), "storage error");
                Self::Internal
            }
        }
    }
}

/// JSON body extractor with our error format.
pub struct ApiJson<T>(pub T);

impl<T: DeserializeOwned, S: Send + Sync> FromRequest<S> for ApiJson<T> {
    type Rejection = ApiError;

    async fn from_request(request: Request, state: &S) -> Result<Self, Self::Rejection> {
        let is_json = request
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| v.to_ascii_lowercase().starts_with("application/json"));
        if !is_json {
            return Err(ApiError::BadRequest("expected application/json"));
        }
        let bytes = Bytes::from_request(request, state)
            .await
            .map_err(|_| ApiError::BadRequest("request body too large"))?;
        serde_json::from_slice(&bytes)
            .map(ApiJson)
            .map_err(|err| ApiError::InvalidJson(err.to_string()))
    }
}

/// Reads the session token from the `Cookie` header.
fn session_token(headers: &HeaderMap) -> Option<&str> {
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .filter_map(|pair| pair.trim().split_once('='))
        .find(|(name, _)| *name == SESSION_COOKIE)
        .map(|(_, value)| value)
}

/// The authenticated user of the request (valid session, account enabled).
pub struct SessionUser {
    pub user: User,
    /// Hash of the session token, for logout.
    pub token_hash: String,
}

impl FromRequestParts<AppState> for SessionUser {
    type Rejection = ApiError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        let token = session_token(&parts.headers).ok_or(ApiError::Unauthorized)?;
        let token_hash = hash_secret(token);
        let session = state
            .store
            .session(&token_hash, now())
            .await?
            .ok_or(ApiError::Unauthorized)?;
        if session.user.disabled {
            return Err(ApiError::Unauthorized);
        }
        Ok(Self {
            user: session.user,
            token_hash,
        })
    }
}

/// The client of the request for per-client login limits (`None` only if
/// the limits middleware did not run).
pub struct Client(pub Option<ClientKey>);

impl<S: Send + Sync> FromRequestParts<S> for Client {
    type Rejection = std::convert::Infallible;

    async fn from_request_parts(parts: &mut Parts, _: &S) -> Result<Self, Self::Rejection> {
        Ok(Self(parts.extensions.get::<ClientKey>().copied()))
    }
}

/// An authenticated administrator.
pub struct AdminUser(pub User);

impl FromRequestParts<AppState> for AdminUser {
    type Rejection = ApiError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        let session = SessionUser::from_request_parts(parts, state).await?;
        if !session.user.is_admin {
            return Err(ApiError::Forbidden);
        }
        Ok(Self(session.user))
    }
}

/// `Set-Cookie` value for a new session, or for clearing it.
pub fn session_cookie(token: Option<&str>, secure: bool) -> String {
    let (value, max_age) = match token {
        Some(token) => (token, SESSION_TTL),
        None => ("", 0),
    };
    let secure = if secure { "; Secure" } else { "" };
    format!(
        "{SESSION_COOKIE}={value}; Path=/; HttpOnly; SameSite=Strict; Max-Age={max_age}{secure}"
    )
}

#[cfg(test)]
mod tests;
