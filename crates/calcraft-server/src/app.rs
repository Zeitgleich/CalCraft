//! HTTP routing and middleware stack.

use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use axum::{
    Router,
    extract::{DefaultBodyLimit, State},
    http::{
        StatusCode,
        header::{LOCATION, RETRY_AFTER},
    },
    middleware,
    response::{IntoResponse, Response},
    routing::{any, get},
};
use tower_http::{
    catch_panic::CatchPanicLayer, limit::RequestBodyLimitLayer, timeout::TimeoutLayer,
    trace::TraceLayer,
};

use calcraft_store::{Readiness, Store};

use crate::{auth::Authenticator, config::Config, dav, limits, net::ClientIp, security};

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
    /// Update check, if enabled (`[updates] check`).
    #[cfg(feature = "update-check")]
    pub updates: Option<Arc<crate::updates::UpdateCheck>>,
    /// Largest accepted vCard; [`router`] sets it from `storage.max_vcard_bytes`.
    pub max_vcard_bytes: usize,
    /// Bounds concurrent recurrence checks of calendar-query (CPU work on
    /// the blocking pool), separately from password hashing.
    pub expansion: Arc<tokio::sync::Semaphore>,
    /// Cached result of the readiness probe (`/readyz`).
    pub readiness: Arc<ReadinessCache>,
}

impl AppState {
    pub fn new(store: Store) -> anyhow::Result<Self> {
        Ok(Self {
            auth: Authenticator::new(store.clone())?,
            store,
            secure_cookies: true,
            #[cfg(feature = "oidc")]
            oidc: None,
            #[cfg(feature = "update-check")]
            updates: None,
            max_vcard_bytes: crate::config::DEFAULT_MAX_VCARD_BYTES,
            expansion: Arc::new(tokio::sync::Semaphore::new(expansion_permits())),
            readiness: Arc::new(ReadinessCache::new(READINESS_TTL)),
        })
    }
}

/// Concurrent recurrence checks: half the cores, at least one.
fn expansion_permits() -> usize {
    std::thread::available_parallelism().map_or(1, |n| (n.get() / 2).max(1))
}

/// Builds the complete application router for the given configuration.
pub fn router(config: &Config, mut state: AppState) -> Router {
    state.max_vcard_bytes = config.max_vcard_bytes();
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
    state.auth.set_client_failure_limit(
        config.server.max_login_failures_per_client,
        config.server.max_tracked_clients,
    );
    let app = app.with_state(state);

    #[cfg(feature = "web-ui")]
    let app = if config.web_ui.enabled {
        app.merge(calcraft_web::router())
    } else {
        app
    };

    // Layers added later wrap the earlier ones, so the security headers are
    // outermost and also apply to timeouts, rejected bodies, panics and
    // rejections by the request limits. The client address is resolved
    // before the trace span is created, the limits are enforced right
    // inside it.
    let limiter = limits::Limiter::new(&config.server);
    app.fallback(not_found)
        .layer(DefaultBodyLimit::disable())
        .layer(RequestBodyLimitLayer::new(config.server.max_body_bytes))
        .layer(TimeoutLayer::with_status_code(
            StatusCode::REQUEST_TIMEOUT,
            config.server.request_timeout(),
        ))
        .layer(CatchPanicLayer::new())
        .layer(middleware::from_fn_with_state(
            limiter.clone(),
            limits::enforce,
        ))
        // Spans record the path only: query strings can carry secrets
        // (OIDC code and state).
        .layer(TraceLayer::new_for_http().make_span_with(
            |request: &axum::http::Request<axum::body::Body>| {
                // Set by `limits::resolve_client`, which runs first.
                let client = request
                    .extensions()
                    .get::<ClientIp>()
                    .map(|client| client.ip);
                tracing::debug_span!(
                    "request",
                    method = %request.method(),
                    path = %request.uri().path(),
                    client = client.map(tracing::field::display),
                )
            },
        ))
        .layer(middleware::from_fn_with_state(
            limiter,
            limits::resolve_client,
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

/// `/readyz`: 200 while the database can be read and written, else 503
/// with a short reason and `Retry-After`. `/healthz` stays liveness only.
async fn ready(State(state): State<AppState>) -> Response {
    let readiness = state.readiness.check(&state.store).await;
    if readiness.is_ready() {
        (StatusCode::OK, "ok").into_response()
    } else {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            [(RETRY_AFTER, READINESS_RETRY_AFTER)],
            readiness.reason(),
        )
            .into_response()
    }
}

/// How long a readiness result is reused: load balancers and orchestrators
/// may probe often, the write probe needs the single writer connection.
pub const READINESS_TTL: Duration = Duration::from_secs(5);

/// `Retry-After` of a 503 from `/readyz` (seconds), matching [`READINESS_TTL`].
const READINESS_RETRY_AFTER: &str = "5";

/// The last readiness result, reused for a short time. One probe runs at a
/// time; concurrent `/readyz` requests wait for it and share its result.
#[derive(Debug)]
pub struct ReadinessCache {
    ttl: Duration,
    last: tokio::sync::Mutex<Option<CachedReadiness>>,
}

#[derive(Debug, Clone, Copy)]
struct CachedReadiness {
    at: Instant,
    /// [`Store::write_failures`] when probed: a newer write failure makes
    /// the result stale at once.
    failures: u64,
    readiness: Readiness,
}

impl ReadinessCache {
    /// `ttl` zero probes on every request.
    pub fn new(ttl: Duration) -> Self {
        Self {
            ttl,
            last: tokio::sync::Mutex::new(None),
        }
    }

    /// The cached result if fresh, else the result of a new probe.
    pub async fn check(&self, store: &Store) -> Readiness {
        let mut last = self.last.lock().await;
        if let Some(cached) = *last
            && cached.at.elapsed() < self.ttl
            && cached.failures == store.write_failures()
        {
            return cached.readiness;
        }
        let readiness = store.readiness().await;
        let previous = last.map(|cached| cached.readiness);
        if previous != Some(readiness) {
            if readiness.is_ready() {
                if previous.is_some() {
                    tracing::info!("instance ready again");
                }
            } else {
                tracing::warn!(reason = readiness.reason(), "instance not ready");
            }
        }
        *last = Some(CachedReadiness {
            at: Instant::now(),
            failures: store.write_failures(),
            readiness,
        });
        readiness
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
