#![allow(clippy::unwrap_used)]

use axum::{
    Router,
    body::Body,
    http::{Method, Request, StatusCode, header},
};
use base64::{Engine, engine::general_purpose::STANDARD};
use calcraft_server::{AppState, Config, auth::hash_password, router};
use calcraft_store::Store;
use http_body_util::BodyExt;
use tower::ServiceExt;

const ALICE: (&str, &str) = ("alice", "alice password 123");
const BOB: (&str, &str) = ("bob", "bob password 1234");

async fn app() -> Router {
    let store = Store::in_memory().await.unwrap();
    for (user, password) in [ALICE, BOB] {
        let hash = hash_password(password).unwrap();
        store
            .create_user(&user.parse().unwrap(), &hash, Some("Alice Example"))
            .await
            .unwrap();
    }
    router(&Config::default(), AppState::new(store).unwrap())
}

fn basic((user, password): (&str, &str)) -> String {
    format!("Basic {}", STANDARD.encode(format!("{user}:{password}")))
}

fn propfind(
    uri: &str,
    depth: Option<&str>,
    credentials: Option<(&str, &str)>,
    body: &str,
) -> Request<Body> {
    let mut builder = Request::builder()
        .method(Method::from_bytes(b"PROPFIND").unwrap())
        .uri(uri)
        .header(header::CONTENT_TYPE, "application/xml");
    if let Some(depth) = depth {
        builder = builder.header("depth", depth);
    }
    if let Some(credentials) = credentials {
        builder = builder.header(header::AUTHORIZATION, basic(credentials));
    }
    builder.body(Body::from(body.to_owned())).unwrap()
}

async fn call(app: &Router, request: Request<Body>) -> (StatusCode, String) {
    let response = app.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let body = response.into_body().collect().await.unwrap().to_bytes();
    (status, String::from_utf8(body.to_vec()).unwrap())
}

const PRINCIPAL_QUERY: &str = r#"<?xml version="1.0" encoding="utf-8"?>
<d:propfind xmlns:d="DAV:"><d:prop><d:current-user-principal/></d:prop></d:propfind>"#;

const HOME_QUERY: &str = r#"<?xml version="1.0" encoding="utf-8"?>
<d:propfind xmlns:d="DAV:" xmlns:c="urn:ietf:params:xml:ns:caldav" xmlns:card="urn:ietf:params:xml:ns:carddav">
  <d:prop><c:calendar-home-set/><card:addressbook-home-set/><d:displayname/></d:prop>
</d:propfind>"#;

const COLLECTION_QUERY: &str = r#"<?xml version="1.0" encoding="utf-8"?>
<d:propfind xmlns:d="DAV:" xmlns:cs="http://calendarserver.org/ns/" xmlns:c="urn:ietf:params:xml:ns:caldav">
  <d:prop><d:resourcetype/><d:displayname/><cs:getctag/><c:supported-calendar-component-set/><d:getlastmodified/></d:prop>
</d:propfind>"#;

#[tokio::test]
async fn options_is_public_and_advertises_dav() {
    let app = app().await;
    let request = Request::options("/dav/").body(Body::empty()).unwrap();
    let response = app.oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert!(
        response.headers()["dav"]
            .to_str()
            .unwrap()
            .contains("calendar-access")
    );
    assert!(
        response.headers()[header::ALLOW]
            .to_str()
            .unwrap()
            .contains("PROPFIND")
    );
}

#[tokio::test]
async fn dav_methods_on_server_root_are_redirected() {
    let app = app().await;
    let response = app
        .clone()
        .oneshot(propfind("/", Some("0"), Some(ALICE), PRINCIPAL_QUERY))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::MOVED_PERMANENTLY);
    assert_eq!(response.headers()[header::LOCATION], "/dav/");
}

#[tokio::test]
async fn propfind_requires_authentication() {
    let app = app().await;
    let response = app
        .clone()
        .oneshot(propfind("/dav/", Some("0"), None, PRINCIPAL_QUERY))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    assert!(
        response.headers()[header::WWW_AUTHENTICATE]
            .to_str()
            .unwrap()
            .starts_with("Basic")
    );

    let (status, _) = call(
        &app,
        propfind(
            "/dav/",
            Some("0"),
            Some(("alice", "wrong password!")),
            PRINCIPAL_QUERY,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn discovery_flow() {
    let app = app().await;

    // 1. current-user-principal on the root.
    let (status, body) = call(
        &app,
        propfind("/dav/", Some("0"), Some(ALICE), PRINCIPAL_QUERY),
    )
    .await;
    assert_eq!(status, StatusCode::MULTI_STATUS);
    assert!(
        body.contains("<d:current-user-principal><d:href>/dav/principals/alice/</d:href>"),
        "{body}"
    );

    // 2. Home sets on the principal.
    let (status, body) = call(
        &app,
        propfind("/dav/principals/alice/", Some("0"), Some(ALICE), HOME_QUERY),
    )
    .await;
    assert_eq!(status, StatusCode::MULTI_STATUS);
    assert!(
        body.contains("<c:calendar-home-set><d:href>/dav/calendars/alice/</d:href>"),
        "{body}"
    );
    assert!(
        body.contains("<card:addressbook-home-set><d:href>/dav/addressbooks/alice/</d:href>"),
        "{body}"
    );
    assert!(
        body.contains("<d:displayname>Alice Example</d:displayname>"),
        "{body}"
    );

    // 3. Calendars in the home.
    let (status, body) = call(
        &app,
        propfind(
            "/dav/calendars/alice/",
            Some("1"),
            Some(ALICE),
            COLLECTION_QUERY,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::MULTI_STATUS);
    assert!(
        body.contains("<d:href>/dav/calendars/alice/default/</d:href>"),
        "{body}"
    );
    assert!(
        body.contains("<d:resourcetype><d:collection/><c:calendar/></d:resourcetype>"),
        "{body}"
    );
    assert!(body.contains(r#"<c:comp name="VEVENT"/>"#), "{body}");
    assert!(body.contains("<cs:getctag>0</cs:getctag>"), "{body}");
    // Unsupported properties are reported as 404 inside the multistatus.
    assert!(body.contains("<d:getlastmodified/>"), "{body}");
    assert!(body.contains("HTTP/1.1 404 Not Found"), "{body}");

    // 4. Address books.
    let (status, body) = call(
        &app,
        propfind(
            "/dav/addressbooks/alice/",
            Some("1"),
            Some(ALICE),
            COLLECTION_QUERY,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::MULTI_STATUS);
    assert!(body.contains("<card:addressbook/>"), "{body}");
}

#[tokio::test]
async fn users_cannot_see_each_other() {
    let app = app().await;
    for path in [
        "/dav/principals/bob/",
        "/dav/calendars/bob/",
        "/dav/calendars/bob/default/",
        "/dav/addressbooks/bob/default/",
    ] {
        let (status, body) = call(&app, propfind(path, Some("0"), Some(ALICE), "")).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{path}");
        assert!(!body.contains("bob"), "{path}: {body}");
    }
    // Same response for a user that does not exist.
    let (status, _) = call(
        &app,
        propfind("/dav/calendars/nobody/", Some("0"), Some(ALICE), ""),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn path_tricks_are_rejected() {
    let app = app().await;
    for path in [
        "/dav/calendars/alice/../bob/default/",
        "/dav/calendars/alice/%2e%2e/",
        "/dav/calendars/ALICE/",
        "/dav/calendars/alice/missing/",
    ] {
        let (status, _) = call(&app, propfind(path, Some("0"), Some(ALICE), "")).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{path}");
    }
}

#[tokio::test]
async fn infinite_depth_is_refused() {
    let app = app().await;
    for depth in [None, Some("infinity")] {
        let (status, body) = call(
            &app,
            propfind("/dav/calendars/alice/", depth, Some(ALICE), ""),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert!(body.contains("propfind-finite-depth"));
    }
}

#[tokio::test]
async fn hostile_xml_is_rejected() {
    let app = app().await;
    let xxe = r#"<?xml version="1.0"?><!DOCTYPE d [<!ENTITY x SYSTEM "file:///etc/passwd">]>
        <d:propfind xmlns:d="DAV:"><d:prop>&x;</d:prop></d:propfind>"#;
    let (status, body) = call(&app, propfind("/dav/", Some("0"), Some(ALICE), xxe)).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(!body.contains("root:"));

    let huge = format!(
        r#"<d:propfind xmlns:d="DAV:"><d:prop>{}</d:prop></d:propfind>"#,
        "<d:displayname/>".repeat(10_000)
    );
    let (status, _) = call(&app, propfind("/dav/", Some("0"), Some(ALICE), &huge)).await;
    assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);
}

#[tokio::test]
async fn allprop_and_propname() {
    let app = app().await;
    let (status, body) = call(
        &app,
        propfind("/dav/calendars/alice/default/", Some("0"), Some(ALICE), ""),
    )
    .await;
    assert_eq!(status, StatusCode::MULTI_STATUS);
    assert!(
        body.contains("<d:displayname>Calendar</d:displayname>"),
        "{body}"
    );

    let query = r#"<d:propfind xmlns:d="DAV:"><d:propname/></d:propfind>"#;
    let (status, body) = call(
        &app,
        propfind(
            "/dav/calendars/alice/default/",
            Some("0"),
            Some(ALICE),
            query,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::MULTI_STATUS);
    assert!(body.contains("<d:displayname/>"), "{body}");
}

#[tokio::test]
async fn unsupported_methods_return_405() {
    let app = app().await;
    let request = Request::builder()
        .method(Method::from_bytes(b"LOCK").unwrap())
        .uri("/dav/calendars/alice/default/x.ics")
        .header(header::AUTHORIZATION, basic(ALICE))
        .body(Body::empty())
        .unwrap();
    let response = app.oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::METHOD_NOT_ALLOWED);
    assert!(response.headers().contains_key(header::ALLOW));
}

#[tokio::test]
async fn repeated_failures_lock_the_account() {
    let app = app().await;
    let wrong = ("alice", "definitely wrong");
    for _ in 0..5 {
        let (status, _) = call(&app, propfind("/dav/", Some("0"), Some(wrong), "")).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }
    // Locked: even the correct password is refused, indistinguishable from a wrong one.
    let (status, _) = call(&app, propfind("/dav/", Some("0"), Some(ALICE), "")).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    // Bob is not affected.
    let (status, _) = call(&app, propfind("/dav/", Some("0"), Some(BOB), "")).await;
    assert_eq!(status, StatusCode::MULTI_STATUS);
}
