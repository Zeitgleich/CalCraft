//! HTTP routing and middleware stack.

use std::sync::Arc;

use axum::{
    Router,
    extract::{DefaultBodyLimit, State},
    http::{StatusCode, header::LOCATION},
    middleware,
    response::{IntoResponse, Response},
    routing::{any, get},
};
use tower_http::{
    catch_panic::CatchPanicLayer, limit::RequestBodyLimitLayer, timeout::TimeoutLayer,
    trace::TraceLayer,
};

use calcraft_store::Store;

use crate::{auth::Authenticator, config::Config, dav, security};

/// Root path of the DAV tree. `.well-known` discovery points here.
pub const DAV_ROOT: &str = "/dav/";

/// Shared state of all handlers.
#[derive(Clone)]
pub struct AppState {
    pub store: Store,
    pub auth: Arc<Authenticator>,
    /// Mark session cookies `Secure` (HTTPS only).
    pub secure_cookies: bool,
    /// Single sign-on, if configured.
    #[cfg(feature = "oidc")]
    pub oidc: Option<Arc<crate::oidc::Oidc>>,
}

impl AppState {
    pub fn new(store: Store) -> anyhow::Result<Self> {
        Ok(Self {
            auth: Authenticator::new(store.clone())?,
            store,
            secure_cookies: true,
            #[cfg(feature = "oidc")]
            oidc: None,
        })
    }
}

/// Builds the complete application router for the given configuration.
pub fn router(config: &Config, state: AppState) -> Router {
    let web_ui = config.web_ui.enabled && cfg!(feature = "web-ui");
    let app = Router::new()
        // GET / shows the web UI; DAV clients configured with the bare
        // server URL send PROPFIND & co. to / and are redirected like
        // .well-known discovery.
        .route("/", get(move || root_page(web_ui)).fallback(well_known))
        .route("/healthz", get(health))
        .route("/readyz", get(ready))
        .route("/.well-known/caldav", any(well_known))
        .route("/.well-known/carddav", any(well_known))
        .route("/dav", any(dav::handle))
        .route("/dav/", any(dav::handle))
        .route("/dav/{*path}", any(dav::handle));

    #[cfg(feature = "api")]
    let app = if config.api.enabled {
        app.nest("/api", crate::api::router())
    } else {
        app
    };
    let app = app.with_state(state);

    #[cfg(feature = "web-ui")]
    let app = if config.web_ui.enabled {
        app.merge(calcraft_web::router())
    } else {
        app
    };

    // Layers added later wrap the earlier ones, so the security headers are
    // outermost and also apply to timeouts, rejected bodies and panics.
    app.fallback(not_found)
        .layer(DefaultBodyLimit::disable())
        .layer(RequestBodyLimitLayer::new(config.server.max_body_bytes))
        .layer(TimeoutLayer::with_status_code(
            StatusCode::REQUEST_TIMEOUT,
            config.server.request_timeout(),
        ))
        .layer(CatchPanicLayer::new())
        // Spans record the path only: query strings can carry secrets
        // (OIDC code and state).
        .layer(TraceLayer::new_for_http().make_span_with(
            |request: &axum::http::Request<axum::body::Body>| {
                tracing::debug_span!("request", method = %request.method(), path = %request.uri().path())
            },
        ))
        .layer(middleware::from_fn_with_state(
            config.security.hsts,
            security::headers,
        ))
}

async fn health() -> &'static str {
    "ok"
}

/// RFC 6764 service discovery: redirect to the DAV root for every method.
async fn well_known() -> impl IntoResponse {
    (StatusCode::MOVED_PERMANENTLY, [(LOCATION, DAV_ROOT)])
}

async fn ready(State(state): State<AppState>) -> impl IntoResponse {
    match state.store.ping().await {
        Ok(()) => (StatusCode::OK, "ok"),
        Err(err) => {
            tracing::error!(%err, "readiness check failed");
            (StatusCode::SERVICE_UNAVAILABLE, "database unavailable")
        }
    }
}

async fn root_page(web_ui: bool) -> Response {
    #[cfg(feature = "web-ui")]
    if web_ui {
        return calcraft_web::index().await.into_response();
    }
    let _ = web_ui;
    not_found().await.into_response()
}

async fn not_found() -> impl IntoResponse {
    (StatusCode::NOT_FOUND, "not found")
}
