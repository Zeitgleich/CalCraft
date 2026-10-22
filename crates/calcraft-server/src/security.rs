//! HTTP security headers applied to every response.

use axum::{
    extract::{Request, State},
    http::{
        HeaderName, HeaderValue,
        header::{
            CONTENT_SECURITY_POLICY, REFERRER_POLICY, STRICT_TRANSPORT_SECURITY,
            X_CONTENT_TYPE_OPTIONS, X_FRAME_OPTIONS,
        },
    },
    middleware::Next,
    response::Response,
};

/// Content Security Policy for all responses.
///
/// `'wasm-unsafe-eval'` is required for the Leptos (WebAssembly) web UI,
/// `manifest-src 'self'` for its web app manifest (home screen icon).
/// Inline scripts and styles are never allowed.
pub const CSP: &str = "default-src 'none'; script-src 'self' 'wasm-unsafe-eval'; \
    style-src 'self'; img-src 'self' data:; font-src 'self'; connect-src 'self'; \
    manifest-src 'self'; form-action 'self'; frame-ancestors 'none'; base-uri 'none'";

const HSTS: &str = "max-age=63072000; includeSubDomains";

fn base_headers() -> [(HeaderName, HeaderValue); 7] {
    [
        (CONTENT_SECURITY_POLICY, HeaderValue::from_static(CSP)),
        (X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff")),
        (X_FRAME_OPTIONS, HeaderValue::from_static("DENY")),
        (REFERRER_POLICY, HeaderValue::from_static("no-referrer")),
        (
            HeaderName::from_static("permissions-policy"),
            HeaderValue::from_static("camera=(), microphone=(), geolocation=(), payment=()"),
        ),
        (
            HeaderName::from_static("cross-origin-opener-policy"),
            HeaderValue::from_static("same-origin"),
        ),
        (
            HeaderName::from_static("cross-origin-resource-policy"),
            HeaderValue::from_static("same-origin"),
        ),
    ]
}

/// Adds security headers unless a handler already set a more specific value.
/// The state flag enables HSTS.
pub async fn headers(State(hsts): State<bool>, request: Request, next: Next) -> Response {
    let mut response = next.run(request).await;
    let headers = response.headers_mut();
    for (name, value) in base_headers() {
        headers.entry(name).or_insert(value);
    }
    if hsts {
        headers
            .entry(STRICT_TRANSPORT_SECURITY)
            .or_insert(HeaderValue::from_static(HSTS));
    }
    response
}

#[cfg(test)]
mod tests;
