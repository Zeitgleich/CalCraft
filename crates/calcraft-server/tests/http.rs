#![allow(clippy::unwrap_used)]

use axum::{
    Router,
    body::Body,
    http::{Method, Request, StatusCode, header},
};
use calcraft_server::{AppState, Config, auth::hash_password, router};
use calcraft_store::Store;
use tower::ServiceExt;

async fn app(config: &Config) -> Router {
    let store = Store::in_memory().await.unwrap();
    for user in ["alice", "bob"] {
        let hash = hash_password(&format!("{user} password 123")).unwrap();
        store
            .create_user(&user.parse().unwrap(), &hash, None)
            .await
            .unwrap();
    }
    router(config, AppState::new(store).unwrap())
}

async fn send(app: &Router, request: Request<Body>) -> axum::response::Response {
    app.clone().oneshot(request).await.unwrap()
}

async fn get(app: &Router, uri: &str) -> axum::response::Response {
    send(app, Request::get(uri).body(Body::empty()).unwrap()).await
}

#[tokio::test]
async fn health_endpoints_respond() {
    let app = app(&Config::default()).await;
    for uri in ["/healthz", "/readyz"] {
        assert_eq!(get(&app, uri).await.status(), StatusCode::OK, "{uri}");
    }
}

#[tokio::test]
async fn security_headers_on_every_response() {
    let app = app(&Config::default()).await;
    for uri in ["/healthz", "/does-not-exist", "/dav/"] {
        let response = get(&app, uri).await;
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
    let response = get(&app(&config).await, "/healthz").await;
    assert!(
        response
            .headers()
            .contains_key(header::STRICT_TRANSPORT_SECURITY)
    );
}

#[tokio::test]
async fn well_known_redirects_for_any_method() {
    let app = app(&Config::default()).await;
    let propfind = Method::from_bytes(b"PROPFIND").unwrap();
    for uri in ["/.well-known/caldav", "/.well-known/carddav"] {
        for method in [Method::GET, propfind.clone()] {
            let request = Request::builder()
                .method(method)
                .uri(uri)
                .body(Body::empty())
                .unwrap();
            let response = send(&app, request).await;
            assert_eq!(response.status(), StatusCode::MOVED_PERMANENTLY);
            assert_eq!(response.headers()[header::LOCATION], "/dav/");
        }
    }
}

#[tokio::test]
async fn oversized_body_is_rejected() {
    let mut config = Config::default();
    config.server.max_body_bytes = 16;
    let request = Request::put("/dav/calendar.ics")
        .header(header::CONTENT_LENGTH, "17")
        .body(Body::from(vec![b'x'; 17]))
        .unwrap();
    let response = send(&app(&config).await, request).await;
    assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
    assert!(
        response
            .headers()
            .contains_key(header::CONTENT_SECURITY_POLICY)
    );
}

#[tokio::test]
async fn unknown_paths_return_404() {
    let response = get(&app(&Config::default()).await, "/nope").await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

#[cfg(feature = "web-ui")]
#[tokio::test]
async fn web_ui_can_be_disabled() {
    use http_body_util::BodyExt;

    let mut config = Config::default();
    let response = get(&app(&config).await, "/").await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = response.into_body().collect().await.unwrap().to_bytes();
    let html = String::from_utf8(body.to_vec()).unwrap();
    assert!(
        html.contains(calcraft_core::SOURCE_URL),
        "AGPL source link missing"
    );

    config.web_ui.enabled = false;
    assert_eq!(
        get(&app(&config).await, "/").await.status(),
        StatusCode::NOT_FOUND
    );
}

#[cfg(feature = "api")]
#[tokio::test]
async fn api_can_be_disabled() {
    use http_body_util::BodyExt;

    let mut config = Config::default();
    let response = get(&app(&config).await, "/api/v1/version").await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = response.into_body().collect().await.unwrap().to_bytes();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["name"], "CalCraft");

    config.api.enabled = false;
    assert_eq!(
        get(&app(&config).await, "/api/v1/version").await.status(),
        StatusCode::NOT_FOUND
    );
}
