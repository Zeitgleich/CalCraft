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

#[cfg(feature = "web-ui")]
async fn body_text(response: axum::response::Response) -> String {
    use http_body_util::BodyExt;

    let body = response.into_body().collect().await.unwrap().to_bytes();
    String::from_utf8(body.to_vec()).unwrap()
}

/// Asset file names referenced as `"/assets/<name>"` or `"./<name>"`.
#[cfg(feature = "web-ui")]
fn referenced_assets(text: &str) -> Vec<String> {
    let mut names = Vec::new();
    for prefix in ["\"/assets/", "\"./"] {
        for part in text.split(prefix).skip(1) {
            if let Some((name, _)) = part.split_once('"') {
                names.push(name.to_owned());
            }
        }
    }
    names
}

#[cfg(feature = "web-ui")]
#[tokio::test]
async fn web_ui_shell_and_assets() {
    use calcraft_server::security::CSP;

    let app = app(&Config::default()).await;
    let response = get(&app, "/").await;
    assert_eq!(response.status(), StatusCode::OK);
    let headers = response.headers();
    assert_eq!(headers[header::CONTENT_TYPE], "text/html; charset=utf-8");
    assert_eq!(headers[header::CACHE_CONTROL], "no-cache");
    assert_eq!(headers[header::CONTENT_SECURITY_POLICY], CSP);
    let html = body_text(response).await;
    assert!(!html.contains("style="), "inline style attribute");
    assert!(!html.contains("<style"), "inline stylesheet");

    let mut pending = referenced_assets(&html);
    if !calcraft_web::ASSETS_EMBEDDED {
        assert!(pending.is_empty(), "placeholder must not reference assets");
        return;
    }
    // Follow the references of the page and of the bootstrap script.
    let mut seen = Vec::new();
    while let Some(name) = pending.pop() {
        if seen.contains(&name) {
            continue;
        }
        let response = get(&app, &format!("/assets/{name}")).await;
        assert_eq!(response.status(), StatusCode::OK, "{name}");
        let headers = response.headers();
        let expected = match name.rsplit_once('.').map(|(_, ext)| ext) {
            Some("js") => "text/javascript; charset=utf-8",
            Some("wasm") => "application/wasm",
            Some("css") => "text/css; charset=utf-8",
            Some("svg") => "image/svg+xml",
            Some("ico") => "image/x-icon",
            Some("png") => "image/png",
            Some("webmanifest") => "application/manifest+json",
            other => panic!("unexpected asset type {other:?}"),
        };
        assert_eq!(headers[header::CONTENT_TYPE], expected, "{name}");
        assert_eq!(
            headers[header::CACHE_CONTROL],
            "public, max-age=31536000, immutable",
            "{name}"
        );
        assert_eq!(headers[header::CONTENT_SECURITY_POLICY], CSP, "{name}");
        assert_eq!(headers[header::X_CONTENT_TYPE_OPTIONS], "nosniff");
        if name.starts_with("boot-") || name.starts_with("manifest-") {
            pending.extend(referenced_assets(&body_text(response).await));
        }
        seen.push(name);
    }
    // Page -> stylesheet, icons, manifest and bootstrap; manifest -> app
    // icons; bootstrap -> glue and module. Nothing is embedded unused.
    assert_eq!(seen.len(), 10, "{seen:?}");
    assert_eq!(seen.len(), calcraft_web::assets().len());
}

#[cfg(feature = "web-ui")]
#[tokio::test]
async fn web_ui_favicon_and_manifest() {
    let mut config = Config::default();
    let app_on = app(&config).await;
    let response = get(&app_on, "/favicon.ico").await;
    if !calcraft_web::ASSETS_EMBEDDED {
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        return;
    }
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()[header::CONTENT_TYPE], "image/x-icon");
    assert_eq!(
        response.headers()[header::CACHE_CONTROL],
        "public, max-age=86400"
    );

    // The manifest names the app and only uses same-origin icons.
    let html = body_text(get(&app_on, "/").await).await;
    let manifest = referenced_assets(&html)
        .into_iter()
        .find(|name| name.ends_with(".webmanifest"))
        .unwrap();
    let response = get(&app_on, &format!("/assets/{manifest}")).await;
    let json: serde_json::Value = serde_json::from_str(&body_text(response).await).unwrap();
    assert_eq!(json["name"], "CalCraft");
    assert_eq!(json["display"], "standalone");
    let icons = json["icons"].as_array().unwrap();
    assert_eq!(icons.len(), 2);
    for icon in icons {
        assert!(icon["src"].as_str().unwrap().starts_with("./icon-"));
    }

    config.web_ui.enabled = false;
    let app_off = app(&config).await;
    assert_eq!(
        get(&app_off, "/favicon.ico").await.status(),
        StatusCode::NOT_FOUND
    );
}

#[cfg(feature = "web-ui")]
#[tokio::test]
async fn web_ui_assets_unknown_or_disabled() {
    let mut config = Config::default();
    let app_on = app(&config).await;
    for uri in [
        "/assets/app.js",
        "/assets/nope-0000000000000000.js",
        "/assets/",
    ] {
        assert_eq!(
            get(&app_on, uri).await.status(),
            StatusCode::NOT_FOUND,
            "{uri}"
        );
    }

    config.web_ui.enabled = false;
    let app_off = app(&config).await;
    for asset in calcraft_web::assets() {
        let uri = format!("/assets/{}", asset.name);
        assert_eq!(get(&app_on, &uri).await.status(), StatusCode::OK, "{uri}");
        assert_eq!(
            get(&app_off, &uri).await.status(),
            StatusCode::NOT_FOUND,
            "{uri}"
        );
    }
}

#[cfg(not(feature = "web-ui"))]
#[tokio::test]
async fn no_web_ui_without_feature() {
    let app = app(&Config::default()).await;
    for uri in ["/", "/assets/app.js"] {
        assert_eq!(
            get(&app, uri).await.status(),
            StatusCode::NOT_FOUND,
            "{uri}"
        );
    }
}
