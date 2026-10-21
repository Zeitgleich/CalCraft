//! HTTP routing and middleware stack.

use axum::{
    Router,
    extract::DefaultBodyLimit,
    http::{StatusCode, header::LOCATION},
    middleware,
    response::IntoResponse,
    routing::{any, get},
};
use tower_http::{
    catch_panic::CatchPanicLayer, limit::RequestBodyLimitLayer, timeout::TimeoutLayer,
    trace::TraceLayer,
};

use crate::{config::Config, security};

/// Root path of the DAV tree. `.well-known` discovery points here.
pub const DAV_ROOT: &str = "/dav/";

/// Builds the complete application router for the given configuration.
pub fn router(config: &Config) -> Router {
    let app = Router::new()
        .route("/healthz", get(health))
        .route("/readyz", get(health))
        .route("/.well-known/caldav", any(well_known))
        .route("/.well-known/carddav", any(well_known))
        .route("/dav", any(dav))
        .route("/dav/", any(dav))
        .route("/dav/{*path}", any(dav));

    #[cfg(feature = "api")]
    let app = if config.api.enabled {
        app.nest("/api", calcraft_api::router())
    } else {
        app
    };

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
        .layer(TraceLayer::new_for_http())
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

async fn dav() -> impl IntoResponse {
    (
        StatusCode::NOT_IMPLEMENTED,
        "DAV support is not implemented yet (milestone M1)",
    )
}

async fn not_found() -> impl IntoResponse {
    (StatusCode::NOT_FOUND, "not found")
}
