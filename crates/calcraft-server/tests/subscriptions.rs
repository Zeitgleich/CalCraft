//! Subscription calendars are read-only for every DAV client, whether or
//! not the `subscriptions` feature (the fetcher) is compiled in: the store
//! marks them, the DAV layer refuses writes with `DAV:need-privileges` and
//! advertises read-only privileges, reads and sync-collection work.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use axum::{
    Router,
    body::Body,
    http::{Method, Request},
};
use base64::{Engine, engine::general_purpose::STANDARD};
use calcraft_server::{AppState, Config, auth::hash_password, auth::now, router};
use calcraft_store::{FeedItem, FeedState, NewSubscription, Store};
use http_body_util::BodyExt;
use tower::ServiceExt;

const PASSWORD: &str = "alice password 123";

fn event(uid: &str) -> String {
    format!(
        "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//Test//EN\r\nBEGIN:VEVENT\r\nUID:{uid}\r\n\
         DTSTAMP:20261001T000000Z\r\nDTSTART:20261021T100000Z\r\nDTEND:20261021T110000Z\r\n\
         SUMMARY:{uid}\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n"
    )
}

fn item(uid: &str) -> FeedItem {
    FeedItem {
        name: format!("{uid}.ics"),
        uid: uid.to_owned(),
        component: "VEVENT".to_owned(),
        range_start: None,
        range_end: None,
        recurring: false,
        index_version: 0,
        etag: format!("\"{uid}\""),
        content: event(uid).into_bytes(),
    }
}

fn state(now: i64) -> FeedState<'static> {
    FeedState {
        feed_ttl_secs: None,
        http_etag: None,
        http_last_modified: None,
        content_hash: "x",
        skipped: 0,
        now,
        next_refresh_at: now + 3600,
    }
}

async fn send(
    app: &Router,
    method: &str,
    uri: &str,
    headers: &[(&str, &str)],
    body: &str,
) -> (u16, String) {
    let mut builder = Request::builder()
        .method(Method::from_bytes(method.as_bytes()).unwrap())
        .uri(uri);
    for (name, value) in headers {
        builder = builder.header(*name, *value);
    }
    let response = app
        .clone()
        .oneshot(builder.body(Body::from(body.to_owned())).unwrap())
        .await
        .unwrap();
    let status = response.status().as_u16();
    let body = response.into_body().collect().await.unwrap().to_bytes();
    (status, String::from_utf8(body.to_vec()).unwrap())
}

#[tokio::test]
async fn subscriptions_are_read_only_over_dav() {
    let store = Store::in_memory().await.unwrap();
    let alice = store
        .create_user(
            &"alice".parse().unwrap(),
            &hash_password(PASSWORD).unwrap(),
            None,
        )
        .await
        .unwrap();
    let slug = "feed".parse().unwrap();
    let collection = store
        .create_subscription(
            &NewSubscription {
                owner_id: alice.id,
                slug: &slug,
                display_name: Some("Feed"),
                color: None,
                components: "VEVENT,VTODO",
                source_url: "https://feeds.example/cal.ics",
                refresh_secs: None,
                max_per_user: 20,
            },
            &[item("a"), item("b")],
            &state(now()),
        )
        .await
        .unwrap();
    let app = router(&Config::default(), AppState::new(store.clone()).unwrap());
    let auth = format!("Basic {}", STANDARD.encode(format!("alice:{PASSWORD}")));
    let auth = ("authorization", auth.as_str());
    let ics = ("content-type", "text/calendar");

    // Reading works.
    let (status, body) = send(&app, "GET", "/dav/calendars/alice/feed/a.ics", &[auth], "").await;
    assert_eq!(status, 200);
    assert!(body.contains("UID:a"));

    // Every write is refused with need-privileges, and nothing changes.
    let proppatch = r#"<?xml version="1.0"?><d:propertyupdate xmlns:d="DAV:" xmlns:c="urn:ietf:params:xml:ns:caldav"><d:set><d:prop><c:calendar-description>x</c:calendar-description></d:prop></d:set></d:propertyupdate>"#;
    let own = event("own");
    let (status, _) = send(
        &app,
        "PUT",
        "/dav/calendars/alice/default/own.ics",
        &[auth, ics],
        &own,
    )
    .await;
    assert_eq!(status, 201);
    for (method, uri, headers, body) in [
        (
            "PUT",
            "/dav/calendars/alice/feed/a.ics",
            vec![auth, ics],
            event("a"),
        ),
        (
            "PUT",
            "/dav/calendars/alice/feed/new.ics",
            vec![auth, ics],
            event("new"),
        ),
        (
            "DELETE",
            "/dav/calendars/alice/feed/a.ics",
            vec![auth],
            String::new(),
        ),
        (
            "DELETE",
            "/dav/calendars/alice/feed/missing.ics",
            vec![auth],
            String::new(),
        ),
        (
            "PROPPATCH",
            "/dav/calendars/alice/feed/",
            vec![auth],
            proppatch.to_owned(),
        ),
        (
            "MOVE",
            "/dav/calendars/alice/feed/a.ics",
            vec![auth, ("destination", "/dav/calendars/alice/default/a.ics")],
            String::new(),
        ),
        (
            "MOVE",
            "/dav/calendars/alice/default/own.ics",
            vec![auth, ("destination", "/dav/calendars/alice/feed/own.ics")],
            String::new(),
        ),
        (
            "COPY",
            "/dav/calendars/alice/default/own.ics",
            vec![auth, ("destination", "/dav/calendars/alice/feed/own.ics")],
            String::new(),
        ),
    ] {
        let (status, body) = send(&app, method, uri, &headers, &body).await;
        assert_eq!(status, 403, "{method} {uri}: {body}");
        assert!(
            body.contains("<d:need-privileges>"),
            "{method} {uri}: {body}"
        );
    }
    let after = store
        .subscription_by_collection(collection.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(after.items, 2);
    assert_eq!(after.collection.sync_revision, collection.sync_revision);
    assert_eq!(after.collection.description, None);

    // Copying out of a subscription is allowed: the copy is the user's own.
    let (status, _) = send(
        &app,
        "COPY",
        "/dav/calendars/alice/feed/a.ics",
        &[
            auth,
            ("destination", "/dav/calendars/alice/default/a-copy.ics"),
        ],
        "",
    )
    .await;
    assert_eq!(status, 201);

    // Privileges: read only, for the collection and its objects.
    let propfind = r#"<?xml version="1.0"?><d:propfind xmlns:d="DAV:"><d:prop><d:current-user-privilege-set/></d:prop></d:propfind>"#;
    let (status, body) = send(
        &app,
        "PROPFIND",
        "/dav/calendars/alice/feed/",
        &[auth, ("depth", "1")],
        propfind,
    )
    .await;
    assert_eq!(status, 207);
    assert_eq!(body.matches("<d:read/>").count(), 3, "{body}");
    assert!(!body.contains("write") && !body.contains("bind"), "{body}");

    // sync-collection lists the objects.
    let sync = r#"<?xml version="1.0"?><d:sync-collection xmlns:d="DAV:"><d:sync-token/><d:sync-level>1</d:sync-level><d:prop><d:getetag/></d:prop></d:sync-collection>"#;
    let (status, body) = send(&app, "REPORT", "/dav/calendars/alice/feed/", &[auth], sync).await;
    assert_eq!(status, 207, "{body}");
    assert!(
        body.contains("/dav/calendars/alice/feed/a.ics") && body.contains("feed/b.ics"),
        "{body}"
    );

    // Deleting the subscription calendar is allowed and bypasses the trash.
    let (status, _) = send(&app, "DELETE", "/dav/calendars/alice/feed/", &[auth], "").await;
    assert_eq!(status, 204);
    assert!(store.subscriptions(alice.id).await.unwrap().is_empty());
    assert_eq!(store.trash_entries(alice.id, 10, 0).await.unwrap().total, 0);
}
