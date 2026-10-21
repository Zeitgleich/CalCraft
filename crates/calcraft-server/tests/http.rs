#![allow(clippy::unwrap_used)]

use axum::{
    Router,
    body::Body,
    http::{Method, Request, StatusCode, header},
};
use calcraft_server::{Config, router};
use tower::ServiceExt;

async fn send(app: Router, method: Method, uri: &str, body: Body) -> axum::response::Response {
    let request = Request::builder()
        .method(method)
        .uri(uri)
        .body(body)
        .unwrap();
    app.oneshot(request).await.unwrap()
}

async fn get(config: &Config, uri: &str) -> axum::response::Response {
    send(router(config), Method::GET, uri, Body::empty()).await
}

#[tokio::test]
async fn health_endpoints_respond() {
    let config = Config::default();
    for uri in ["/healthz", "/readyz"] {
        assert_eq!(get(&config, uri).await.status(), StatusCode::OK, "{uri}");
    }
}

#[tokio::test]
async fn security_headers_on_every_response() {
    let config = Config::default();
    for uri in ["/healthz", "/does-not-exist", "/dav/"] {
        let response = get(&config, uri).await;
        let headers = response.headers();
        assert!(
            headers.contains_key(header::CONTENT_SECURITY_POLICY),
            "{uri}"
        );
        assert_eq!(headers[header::X_CONTENT_TYPE_OPTIONS], "nosniff");
        assert_eq!(headers[header::X_FRAME_OPTIONS], "DENY");
        assert_eq!(headers[header::REFERRER_POLICY], "no-referrer");
        assert!(!headers.contains_key(header::STRICT_TRANSPORT_SECURITY));
    }
}

#[tokio::test]
async fn hsts_only_when_enabled() {
    let mut config = Config::default();
    config.security.hsts = true;
    let response = get(&config, "/healthz").await;
    assert!(
        response
            .headers()
            .contains_key(header::STRICT_TRANSPORT_SECURITY)
    );
}

#[tokio::test]
async fn well_known_redirects_for_any_method() {
    let config = Config::default();
    let propfind = Method::from_bytes(b"PROPFIND").unwrap();
    for uri in ["/.well-known/caldav", "/.well-known/carddav"] {
        for method in [Method::GET, propfind.clone()] {
            let response = send(router(&config), method, uri, Body::empty()).await;
            assert_eq!(response.status(), StatusCode::MOVED_PERMANENTLY);
            assert_eq!(response.headers()[header::LOCATION], "/dav/");
        }
    }
}

#[tokio::test]
async fn oversized_body_is_rejected() {
    let mut config = Config::default();
    config.server.max_body_bytes = 16;
    let request = Request::builder()
        .method(Method::PUT)
        .uri("/dav/calendar.ics")
        .header(header::CONTENT_LENGTH, "17")
        .body(Body::from(vec![b'x'; 17]))
        .unwrap();
    let response = router(&config).oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
    assert!(
        response
            .headers()
            .contains_key(header::CONTENT_SECURITY_POLICY)
    );
}

#[tokio::test]
async fn unknown_paths_return_404() {
    let response = get(&Config::default(), "/nope").await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

#[cfg(feature = "web-ui")]
#[tokio::test]
async fn web_ui_can_be_disabled() {
    use http_body_util::BodyExt;

    let mut config = Config::default();
    let response = get(&config, "/").await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = response.into_body().collect().await.unwrap().to_bytes();
    let html = String::from_utf8(body.to_vec()).unwrap();
    assert!(
        html.contains(calcraft_core::SOURCE_URL),
        "AGPL source link missing"
    );

    config.web_ui.enabled = false;
    assert_eq!(get(&config, "/").await.status(), StatusCode::NOT_FOUND);
}

#[cfg(feature = "api")]
#[tokio::test]
async fn api_can_be_disabled() {
    use http_body_util::BodyExt;

    let mut config = Config::default();
    let response = get(&config, "/api/v1/version").await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = response.into_body().collect().await.unwrap().to_bytes();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["name"], "CalCraft");

    config.api.enabled = false;
    assert_eq!(
        get(&config, "/api/v1/version").await.status(),
        StatusCode::NOT_FOUND
    );
}
