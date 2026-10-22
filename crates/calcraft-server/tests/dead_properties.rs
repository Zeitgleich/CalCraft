//! Dead properties (RFC 4918 §4) and Apple's `calendar-order` on collections.
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
const CAL: &str = "/dav/calendars/alice/default/";
const BOOK: &str = "/dav/addressbooks/alice/default/";
const NS: &str = r#"xmlns:d="DAV:" xmlns:c="urn:ietf:params:xml:ns:caldav" xmlns:cs="http://calendarserver.org/ns/" xmlns:ic="http://apple.com/ns/ical/" xmlns:x="urn:example:x""#;

async fn app() -> Router {
    let store = Store::in_memory().await.unwrap();
    for (user, password) in [ALICE, BOB] {
        let hash = hash_password(password).unwrap();
        store
            .create_user(&user.parse().unwrap(), &hash, None)
            .await
            .unwrap();
    }
    router(&Config::default(), AppState::new(store).unwrap())
}

async fn send(
    app: &Router,
    (user, password): (&str, &str),
    method: &str,
    uri: &str,
    depth: Option<&str>,
    body: &str,
) -> (StatusCode, String) {
    let mut builder = Request::builder()
        .method(Method::from_bytes(method.as_bytes()).unwrap())
        .uri(uri)
        .header(
            header::AUTHORIZATION,
            format!("Basic {}", STANDARD.encode(format!("{user}:{password}"))),
        );
    if let Some(depth) = depth {
        builder = builder.header("depth", depth);
    }
    let response = app
        .clone()
        .oneshot(builder.body(Body::from(body.to_owned())).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let body = response.into_body().collect().await.unwrap().to_bytes();
    (status, String::from_utf8(body.to_vec()).unwrap())
}

async fn proppatch(app: &Router, uri: &str, set: &str, remove: &str) -> String {
    let mut body = format!("<d:propertyupdate {NS}>");
    if !set.is_empty() {
        body += &format!("<d:set><d:prop>{set}</d:prop></d:set>");
    }
    if !remove.is_empty() {
        body += &format!("<d:remove><d:prop>{remove}</d:prop></d:remove>");
    }
    body += "</d:propertyupdate>";
    let (status, reply) = send(app, ALICE, "PROPPATCH", uri, None, &body).await;
    assert_eq!(status, StatusCode::MULTI_STATUS, "{reply}");
    reply
}

async fn propfind(app: &Router, uri: &str, depth: &str, query: &str) -> String {
    let body = match query {
        "" => String::new(),
        "propname" => format!("<d:propfind {NS}><d:propname/></d:propfind>"),
        props => format!("<d:propfind {NS}><d:prop>{props}</d:prop></d:propfind>"),
    };
    let (status, reply) = send(app, ALICE, "PROPFIND", uri, Some(depth), &body).await;
    assert_eq!(status, StatusCode::MULTI_STATUS, "{reply}");
    reply
}

fn ok_only(reply: &str) {
    assert!(reply.contains("HTTP/1.1 200 OK"), "{reply}");
    assert!(!reply.contains("HTTP/1.1 4"), "{reply}");
    assert!(!reply.contains("HTTP/1.1 5"), "{reply}");
}

#[tokio::test]
async fn calendar_order_round_trip() {
    let app = app().await;
    let mkcalendar = format!(
        r##"<c:mkcalendar {NS}><d:set><d:prop>
            <d:displayname>Work</d:displayname>
            <ic:calendar-color>#336699FF</ic:calendar-color>
            <ic:calendar-order>3</ic:calendar-order>
            <c:calendar-timezone>BEGIN:VCALENDAR&#13;
END:VCALENDAR</c:calendar-timezone>
        </d:prop></d:set></c:mkcalendar>"##
    );
    let work = "/dav/calendars/alice/work/";
    let (status, reply) = send(&app, ALICE, "MKCALENDAR", work, None, &mkcalendar).await;
    assert_eq!(status, StatusCode::CREATED, "{reply}");

    let order = "<ic:calendar-order/><ic:calendar-color/><c:calendar-timezone/>";
    let reply = propfind(&app, work, "0", order).await;
    assert!(
        reply.contains("<ic:calendar-order>3</ic:calendar-order>"),
        "{reply}"
    );
    assert!(
        reply.contains("<ic:calendar-color>#336699FF</ic:calendar-color>"),
        "{reply}"
    );
    assert!(
        reply.contains(
            "<c:calendar-timezone>BEGIN:VCALENDAR&#13;\nEND:VCALENDAR</c:calendar-timezone>"
        ),
        "{reply}"
    );

    // Apple reorders with PROPPATCH; the value is kept exactly.
    ok_only(&proppatch(&app, work, "<ic:calendar-order>-12</ic:calendar-order>", "").await);
    ok_only(&proppatch(&app, CAL, "<ic:calendar-order>7</ic:calendar-order>", "").await);
    let home = propfind(&app, "/dav/calendars/alice/", "1", order).await;
    assert!(
        home.contains("<ic:calendar-order>-12</ic:calendar-order>"),
        "{home}"
    );
    assert!(
        home.contains("<ic:calendar-order>7</ic:calendar-order>"),
        "{home}"
    );
    let allprop = propfind(&app, "/dav/calendars/alice/", "1", "").await;
    assert!(
        allprop.contains("<ic:calendar-order>-12</ic:calendar-order>"),
        "{allprop}"
    );
    let names = propfind(&app, work, "0", "propname").await;
    assert!(names.contains("<ic:calendar-order/>"), "{names}");
    assert!(names.contains("<c:calendar-timezone/>"), "{names}");

    // Not an integer, or on an address book: rejected, nothing changes.
    let reply = proppatch(
        &app,
        work,
        "<ic:calendar-order>first</ic:calendar-order><d:displayname>Renamed</d:displayname>",
        "",
    )
    .await;
    assert!(reply.contains("HTTP/1.1 409 Conflict"), "{reply}");
    assert!(reply.contains("HTTP/1.1 424 Failed Dependency"), "{reply}");
    let reply = proppatch(&app, BOOK, "<ic:calendar-order>1</ic:calendar-order>", "").await;
    assert!(reply.contains("HTTP/1.1 403 Forbidden"), "{reply}");
    let reply = propfind(&app, work, "0", "<ic:calendar-order/><d:displayname/>").await;
    assert!(
        reply.contains("<ic:calendar-order>-12</ic:calendar-order>"),
        "{reply}"
    );
    assert!(
        reply.contains("<d:displayname>Work</d:displayname>"),
        "{reply}"
    );

    // Removal.
    ok_only(&proppatch(&app, work, "", "<ic:calendar-order/>").await);
    let reply = propfind(&app, work, "0", "<ic:calendar-order/>").await;
    assert!(
        reply.contains("<ic:calendar-order/></d:prop><d:status>HTTP/1.1 404 Not Found"),
        "{reply}"
    );
}

#[tokio::test]
async fn xml_values_round_trip() {
    let app = app().await;
    let set = r#"<x:nested>
            <x:child a="1 &amp; &quot;2&quot;" x:prefixed="dropped">text &lt;b&gt;</x:child>
            <o:other xmlns:o="urn:example:other"><x:back/><plain xmlns="">p</plain></o:other>
        </x:nested>
        <x:text>&lt;/x:text&gt;&lt;d:owner/&gt;</x:text>
        <bare xmlns="">value</bare>
        <x:empty/>"#;
    ok_only(&proppatch(&app, CAL, set, "").await);
    let props = r#"<x:nested/><x:text/><bare xmlns=""/><x:empty/>"#;
    let reply = propfind(&app, CAL, "0", props).await;
    assert!(
        reply.contains(
            r#"<x:nested xmlns:x="urn:example:x"><child xmlns="urn:example:x" a="1 &amp; &quot;2&quot;">text &lt;b&gt;</child><other xmlns="urn:example:other"><back xmlns="urn:example:x"/><plain xmlns="">p</plain></other></x:nested>"#
        ),
        "{reply}"
    );
    // Prefixed attributes are not kept.
    assert!(!reply.contains("dropped"), "{reply}");
    assert!(
        reply.contains(
            r#"<x:text xmlns:x="urn:example:x">&lt;/x:text&gt;&lt;d:owner/&gt;</x:text>"#
        ),
        "{reply}"
    );
    assert!(reply.contains("<bare>value</bare>"), "{reply}");
    assert!(
        reply.contains(r#"<x:empty xmlns:x="urn:example:x"></x:empty>"#),
        "{reply}"
    );
    assert!(!reply.contains("404 Not Found"), "{reply}");

    // Replacing and removing.
    ok_only(&proppatch(&app, CAL, "<x:text>new</x:text>", r#"<bare xmlns=""/>"#).await);
    let reply = propfind(&app, CAL, "0", props).await;
    assert!(reply.contains(">new</x:text>"), "{reply}");
    assert!(
        reply.contains("<bare/></d:prop><d:status>HTTP/1.1 404 Not Found"),
        "{reply}"
    );
    // Removing a property that does not exist succeeds.
    ok_only(&proppatch(&app, CAL, "", "<x:never/>").await);

    // Other collections and other users never see them.
    let reply = propfind(&app, BOOK, "0", "").await;
    assert!(!reply.contains("urn:example:x"), "{reply}");
    let (status, _) = send(&app, BOB, "PROPFIND", CAL, Some("0"), "").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let body = format!(
        "<d:propertyupdate {NS}><d:set><d:prop><x:text>bob</x:text></d:prop></d:set></d:propertyupdate>"
    );
    let (status, _) = send(&app, BOB, "PROPPATCH", CAL, None, &body).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let reply = propfind(&app, CAL, "0", "<x:text/>").await;
    assert!(reply.contains(">new</x:text>"), "{reply}");
}

#[tokio::test]
async fn protected_properties_fail_the_whole_proppatch() {
    let app = app().await;
    for protected in [
        "<d:resourcetype><d:collection/></d:resourcetype>",
        "<d:getetag>\"x\"</d:getetag>",
        "<d:sync-token>urn:x</d:sync-token>",
        "<cs:getctag>1</cs:getctag>",
        "<d:current-user-privilege-set/>",
        "<c:supported-calendar-component-set><c:comp name=\"VTODO\"/></c:supported-calendar-component-set>",
        "<d:quota-used-bytes>0</d:quota-used-bytes>",
        "<d:quota-available-bytes>0</d:quota-available-bytes>",
        "<d:owner><d:href>/dav/principals/bob/</d:href></d:owner>",
        "<d:principal-URL><d:href>/dav/principals/bob/</d:href></d:principal-URL>",
        "<d:acl/>",
        "<d:unknown-dav-property>x</d:unknown-dav-property>",
        "<c:max-resource-size>1</c:max-resource-size>",
    ] {
        let reply = proppatch(
            &app,
            CAL,
            &format!("<x:dead>v</x:dead>{protected}<d:displayname>Hijacked</d:displayname>"),
            "",
        )
        .await;
        assert!(
            reply.contains(
                "<d:status>HTTP/1.1 403 Forbidden</d:status><d:error><d:cannot-modify-protected-property/></d:error>"
            ),
            "{protected}: {reply}"
        );
        assert!(
            reply.contains(
                "<x:dead xmlns:x=\"urn:example:x\"/><d:displayname/></d:prop><d:status>HTTP/1.1 424 Failed Dependency"
            ),
            "{protected}: {reply}"
        );
    }
    let reply = proppatch(&app, CAL, "", "<d:getetag/><x:dead/>").await;
    assert!(
        reply.contains("cannot-modify-protected-property"),
        "{reply}"
    );
    // Nothing was applied.
    let reply = propfind(&app, CAL, "0", "").await;
    assert!(!reply.contains("Hijacked"), "{reply}");
    assert!(!reply.contains("urn:example:x"), "{reply}");
}

#[tokio::test]
async fn limits() {
    let app = app().await;
    // Names: too long, relative namespace.
    let long = format!("<x:{}>v</x:{0}>", "n".repeat(129));
    let reply = proppatch(&app, CAL, &format!("{long}<x:ok>v</x:ok>"), "").await;
    assert!(reply.contains("HTTP/1.1 403 Forbidden"), "{reply}");
    assert!(reply.contains("HTTP/1.1 424 Failed Dependency"), "{reply}");
    let reply = proppatch(&app, CAL, r#"<r:p xmlns:r="relative/ns">v</r:p>"#, "").await;
    assert!(reply.contains("HTTP/1.1 403 Forbidden"), "{reply}");
    assert!(
        !reply.contains("cannot-modify-protected-property"),
        "{reply}"
    );
    ok_only(&proppatch(&app, CAL, &format!("<x:{}>v</x:{0}>", "n".repeat(128)), "").await);
    // Characters XML does not allow.
    let reply = proppatch(&app, CAL, "<x:ctl>a&#1;b</x:ctl>", "").await;
    assert!(reply.contains("HTTP/1.1 409 Conflict"), "{reply}");

    // Value size.
    let big = format!("<x:big>{}</x:big>", "v".repeat(16 * 1024 + 1));
    let reply = proppatch(&app, CAL, &big, "").await;
    assert!(
        reply.contains("HTTP/1.1 507 Insufficient Storage"),
        "{reply}"
    );

    // Count: 64 per collection (one is already set above).
    let many: String = (0..63).map(|i| format!("<x:p{i}>{i}</x:p{i}>")).collect();
    ok_only(&proppatch(&app, CAL, &many, "").await);
    let reply = proppatch(
        &app,
        CAL,
        "<x:one-more>v</x:one-more><d:displayname>N</d:displayname>",
        "",
    )
    .await;
    assert!(
        reply.contains("<x:one-more xmlns:x=\"urn:example:x\"/></d:prop><d:status>HTTP/1.1 507"),
        "{reply}"
    );
    assert!(
        reply.contains("<d:displayname/></d:prop><d:status>HTTP/1.1 424"),
        "{reply}"
    );
    // Replacing an existing one still works.
    ok_only(&proppatch(&app, CAL, "<x:p0>zero</x:p0>", "").await);

    // Total size: 64 KiB per collection.
    let chunk = "v".repeat(15 * 1024);
    let set: String = (0..4)
        .map(|i| format!("<x:b{i}>{chunk}</x:b{i}>"))
        .collect();
    ok_only(&proppatch(&app, BOOK, &set, "").await);
    let reply = proppatch(&app, BOOK, &format!("<x:b4>{chunk}</x:b4>"), "").await;
    assert!(reply.contains("HTTP/1.1 507"), "{reply}");

    // MKCALENDAR: too large is refused and creates nothing.
    let body = format!("<c:mkcalendar {NS}><d:set><d:prop>{big}</d:prop></d:set></c:mkcalendar>");
    let uri = "/dav/calendars/alice/big/";
    let (status, _) = send(&app, ALICE, "MKCALENDAR", uri, None, &body).await;
    assert_eq!(status, StatusCode::INSUFFICIENT_STORAGE);
    let (status, _) = send(&app, ALICE, "PROPFIND", uri, Some("0"), "").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

fn between<'a>(text: &'a str, start: &str, end: &str) -> &'a str {
    let from = text.find(start).unwrap() + start.len();
    let to = from + text[from..].find(end).unwrap();
    &text[from..to]
}

#[tokio::test]
async fn property_changes_do_not_report_object_changes() {
    let app = app().await;
    let before = propfind(&app, CAL, "0", "<d:sync-token/><cs:getctag/>").await;
    let token = between(&before, "<d:sync-token>", "</d:sync-token>").to_owned();
    let ctag = between(&before, "<cs:getctag>", "</cs:getctag>").to_owned();
    ok_only(&proppatch(&app, CAL, "<ic:calendar-order>2</ic:calendar-order>", "").await);
    let after = propfind(&app, CAL, "0", "<cs:getctag/>").await;
    // As for displayname or color: the ctag moves, so clients re-read the
    // collection's properties …
    assert_ne!(between(&after, "<cs:getctag>", "</cs:getctag>"), ctag);
    // … but a sync from the old token reports no objects.
    let report = format!(
        "<d:sync-collection {NS}><d:sync-token>{token}</d:sync-token><d:sync-level>1</d:sync-level><d:prop><d:getetag/></d:prop></d:sync-collection>"
    );
    let (status, reply) = send(&app, ALICE, "REPORT", CAL, None, &report).await;
    assert_eq!(status, StatusCode::MULTI_STATUS, "{reply}");
    assert!(!reply.contains("<d:response>"), "{reply}");
}

#[cfg(feature = "api")]
#[tokio::test]
async fn web_ui_lists_calendars_by_order() {
    let app = app().await;
    for (slug, order) in [
        ("b-second", Some(2)),
        ("a-unordered", None),
        ("c-first", Some(1)),
    ] {
        let set = order
            .map(|o| format!("<ic:calendar-order>{o}</ic:calendar-order>"))
            .unwrap_or_default();
        let body = format!(
            "<c:mkcalendar {NS}><d:set><d:prop><d:displayname>{slug}</d:displayname>{set}</d:prop></d:set></c:mkcalendar>"
        );
        let uri = format!("/dav/calendars/alice/{slug}/");
        let (status, _) = send(&app, ALICE, "MKCALENDAR", &uri, None, &body).await;
        assert_eq!(status, StatusCode::CREATED);
    }

    let login = Request::builder()
        .method(Method::POST)
        .uri("/api/v1/auth/login")
        .header("x-calcraft-request", "1")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(
            r#"{"username":"alice","password":"alice password 123"}"#,
        ))
        .unwrap();
    let response = app.clone().oneshot(login).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let cookie = response.headers()[header::SET_COOKIE]
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_owned();
    let list = Request::builder()
        .uri("/api/v1/collections")
        .header("x-calcraft-request", "1")
        .header(header::COOKIE, cookie)
        .body(Body::empty())
        .unwrap();
    let response = app.clone().oneshot(list).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let list: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    let calendars: Vec<(&str, Option<i64>)> = list
        .as_array()
        .unwrap()
        .iter()
        .filter(|c| c["kind"] == "calendar")
        .map(|c| (c["slug"].as_str().unwrap(), c["order"].as_i64()))
        .collect();
    assert_eq!(
        calendars,
        [
            ("c-first", Some(1)),
            ("b-second", Some(2)),
            ("a-unordered", None),
            ("default", None),
        ]
    );
}
