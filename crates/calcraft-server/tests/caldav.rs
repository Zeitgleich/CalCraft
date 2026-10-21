#![allow(clippy::unwrap_used)]

use axum::{
    Router,
    body::Body,
    http::{HeaderMap, Method, Request, StatusCode, header},
};
use base64::{Engine, engine::general_purpose::STANDARD};
use calcraft_server::{AppState, Config, auth::hash_password, router};
use calcraft_store::Store;
use http_body_util::BodyExt;
use tower::ServiceExt;

const ALICE: (&str, &str) = ("alice", "alice password 123");
const BOB: (&str, &str) = ("bob", "bob password 1234");
const CAL: &str = "/dav/calendars/alice/default/";

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

fn basic((user, password): (&str, &str)) -> String {
    format!("Basic {}", STANDARD.encode(format!("{user}:{password}")))
}

struct Reply {
    status: StatusCode,
    headers: HeaderMap,
    body: String,
}

async fn send(
    app: &Router,
    as_user: (&str, &str),
    method: &str,
    uri: &str,
    headers: &[(&str, &str)],
    body: &str,
) -> Reply {
    let mut builder = Request::builder()
        .method(Method::from_bytes(method.as_bytes()).unwrap())
        .uri(uri)
        .header(header::AUTHORIZATION, basic(as_user));
    for (name, value) in headers {
        builder = builder.header(*name, *value);
    }
    let response = app
        .clone()
        .oneshot(builder.body(Body::from(body.to_owned())).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let headers = response.headers().clone();
    let body = response.into_body().collect().await.unwrap().to_bytes();
    Reply {
        status,
        headers,
        body: String::from_utf8(body.to_vec()).unwrap(),
    }
}

fn event(uid: &str, start: &str, end: &str, summary: &str) -> String {
    format!(
        "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//Test//EN\r\nBEGIN:VEVENT\r\nUID:{uid}\r\nDTSTAMP:20261001T000000Z\r\nDTSTART:{start}\r\nDTEND:{end}\r\nSUMMARY:{summary}\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n"
    )
}

async fn put(
    app: &Router,
    as_user: (&str, &str),
    uri: &str,
    body: &str,
    headers: &[(&str, &str)],
) -> Reply {
    let mut all = vec![("content-type", "text/calendar; charset=utf-8")];
    all.extend_from_slice(headers);
    send(app, as_user, "PUT", uri, &all, body).await
}

async fn ctag(app: &Router, uri: &str) -> String {
    let body = r#"<d:propfind xmlns:d="DAV:" xmlns:cs="http://calendarserver.org/ns/"><d:prop><cs:getctag/></d:prop></d:propfind>"#;
    let reply = send(app, ALICE, "PROPFIND", uri, &[("depth", "0")], body).await;
    let start = reply.body.find("<cs:getctag>").unwrap() + "<cs:getctag>".len();
    let end = reply.body.find("</cs:getctag>").unwrap();
    reply.body[start..end].to_owned()
}

#[tokio::test]
async fn put_get_update_with_etags() {
    let app = app().await;
    let uri = format!("{CAL}meeting.ics");
    let v1 = event("m1", "20261021T100000Z", "20261021T110000Z", "Kickoff");

    let created = put(&app, ALICE, &uri, &v1, &[("if-none-match", "*")]).await;
    assert_eq!(created.status, StatusCode::CREATED);
    let etag1 = created.headers[header::ETAG].to_str().unwrap().to_owned();

    let fetched = send(&app, ALICE, "GET", &uri, &[], "").await;
    assert_eq!(fetched.status, StatusCode::OK);
    assert_eq!(fetched.body, v1);
    assert_eq!(fetched.headers[header::ETAG].to_str().unwrap(), etag1);
    assert!(
        fetched.headers[header::CONTENT_TYPE]
            .to_str()
            .unwrap()
            .starts_with("text/calendar")
    );

    let again = put(&app, ALICE, &uri, &v1, &[("if-none-match", "*")]).await;
    assert_eq!(again.status, StatusCode::PRECONDITION_FAILED);

    let v2 = event(
        "m1",
        "20261021T100000Z",
        "20261021T120000Z",
        "Kickoff (long)",
    );
    let stale = put(&app, ALICE, &uri, &v2, &[("if-match", "\"stale\"")]).await;
    assert_eq!(stale.status, StatusCode::PRECONDITION_FAILED);
    let updated = put(&app, ALICE, &uri, &v2, &[("if-match", &etag1)]).await;
    assert_eq!(updated.status, StatusCode::NO_CONTENT);
    assert_ne!(updated.headers[header::ETAG].to_str().unwrap(), etag1);

    let head = send(&app, ALICE, "HEAD", &uri, &[], "").await;
    assert_eq!(head.status, StatusCode::OK);
    assert!(head.body.is_empty());
}

#[tokio::test]
async fn invalid_objects_are_rejected() {
    let app = app().await;
    let uri = format!("{CAL}bad.ics");
    let cases = [
        ("not ical at all", "valid-calendar-data"),
        (
            "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nMETHOD:REQUEST\r\nBEGIN:VEVENT\r\nUID:x\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n",
            "valid-calendar-data",
        ),
        (
            "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nBEGIN:VJOURNAL\r\nUID:j\r\nEND:VJOURNAL\r\nEND:VCALENDAR\r\n",
            "supported-calendar-component",
        ),
        (
            "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nBEGIN:VEVENT\r\nUID:r\r\nDTSTART:20261021T100000Z\r\nRRULE:FREQ=SECONDLY;INTERVAL=0\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n",
            "valid-calendar-data",
        ),
    ];
    for (body, condition) in cases {
        let reply = put(&app, ALICE, &uri, body, &[]).await;
        assert_eq!(reply.status, StatusCode::FORBIDDEN, "{body}");
        assert!(reply.body.contains(condition), "{body}: {}", reply.body);
    }

    let nested = format!(
        "BEGIN:VCALENDAR\r\nVERSION:2.0\r\n{}{}END:VCALENDAR\r\n",
        "BEGIN:X\r\n".repeat(50),
        "END:X\r\n".repeat(50)
    );
    assert_eq!(
        put(&app, ALICE, &uri, &nested, &[]).await.status,
        StatusCode::FORBIDDEN
    );

    let wrong_type = send(
        &app,
        ALICE,
        "PUT",
        &uri,
        &[("content-type", "text/html")],
        &event("h", "20261021T100000Z", "20261021T110000Z", "x"),
    )
    .await;
    assert_eq!(wrong_type.status, StatusCode::FORBIDDEN);

    let huge = format!(
        "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nBEGIN:VEVENT\r\nUID:big\r\nDESCRIPTION:{}\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n",
        "x".repeat(2 * 1024 * 1024)
    );
    let reply = put(&app, ALICE, &uri, &huge, &[]).await;
    assert_eq!(reply.status, StatusCode::FORBIDDEN);
    assert!(reply.body.contains("max-resource-size"));

    // Nothing was stored.
    assert_eq!(
        send(&app, ALICE, "GET", &uri, &[], "").await.status,
        StatusCode::NOT_FOUND
    );
}

#[tokio::test]
async fn uid_conflicts_are_reported() {
    let app = app().await;
    let body = event("dup", "20261021T100000Z", "20261021T110000Z", "A");
    assert_eq!(
        put(&app, ALICE, &format!("{CAL}a.ics"), &body, &[])
            .await
            .status,
        StatusCode::CREATED
    );
    let reply = put(&app, ALICE, &format!("{CAL}b.ics"), &body, &[]).await;
    assert_eq!(reply.status, StatusCode::FORBIDDEN);
    assert!(
        reply
            .body
            .contains("<c:no-uid-conflict><d:href>/dav/calendars/alice/default/a.ics</d:href>"),
        "{}",
        reply.body
    );
}

#[tokio::test]
async fn propfind_lists_objects_and_ctag_changes() {
    let app = app().await;
    let before = ctag(&app, CAL).await;
    put(
        &app,
        ALICE,
        &format!("{CAL}one.ics"),
        &event("1", "20261021T100000Z", "20261021T110000Z", "One"),
        &[],
    )
    .await;
    let after = ctag(&app, CAL).await;
    assert_ne!(before, after);

    let body =
        r#"<d:propfind xmlns:d="DAV:"><d:prop><d:getetag/><d:resourcetype/></d:prop></d:propfind>"#;
    let reply = send(&app, ALICE, "PROPFIND", CAL, &[("depth", "1")], body).await;
    assert_eq!(reply.status, StatusCode::MULTI_STATUS);
    assert!(
        reply
            .body
            .contains("<d:href>/dav/calendars/alice/default/one.ics</d:href>"),
        "{}",
        reply.body
    );
    assert!(reply.body.contains("<d:getetag>&quot;"), "{}", reply.body);

    let props =
        r#"<d:propfind xmlns:d="DAV:"><d:prop><d:supported-report-set/></d:prop></d:propfind>"#;
    let reply = send(&app, ALICE, "PROPFIND", CAL, &[("depth", "0")], props).await;
    assert!(
        reply.body.contains("<c:calendar-multiget/>"),
        "{}",
        reply.body
    );
}

#[tokio::test]
async fn multiget_checks_every_href() {
    let app = app().await;
    put(
        &app,
        ALICE,
        &format!("{CAL}mine.ics"),
        &event("mine", "20261021T100000Z", "20261021T110000Z", "Mine"),
        &[],
    )
    .await;
    put(
        &app,
        BOB,
        "/dav/calendars/bob/default/secret.ics",
        &event(
            "secret",
            "20261021T100000Z",
            "20261021T110000Z",
            "Bob secret",
        ),
        &[],
    )
    .await;

    let body = r#"<c:calendar-multiget xmlns:d="DAV:" xmlns:c="urn:ietf:params:xml:ns:caldav">
        <d:prop><d:getetag/><c:calendar-data/></d:prop>
        <d:href>/dav/calendars/alice/default/mine.ics</d:href>
        <d:href>/dav/calendars/alice/default/missing.ics</d:href>
        <d:href>/dav/calendars/bob/default/secret.ics</d:href>
        <d:href>/dav/calendars/alice/default/../../bob/default/secret.ics</d:href>
    </c:calendar-multiget>"#;
    let reply = send(&app, ALICE, "REPORT", CAL, &[("depth", "1")], body).await;
    assert_eq!(reply.status, StatusCode::MULTI_STATUS);
    assert!(reply.body.contains("SUMMARY:Mine"), "{}", reply.body);
    assert!(!reply.body.contains("Bob secret"), "{}", reply.body);
    assert_eq!(
        reply.body.matches("HTTP/1.1 404 Not Found").count(),
        3,
        "{}",
        reply.body
    );
}

#[tokio::test]
async fn multiget_duplicates_are_collapsed() {
    let app = app().await;
    put(
        &app,
        ALICE,
        &format!("{CAL}one.ics"),
        &event("one", "20261021T100000Z", "20261021T110000Z", "Only once"),
        &[],
    )
    .await;
    let hrefs = "<d:href>/dav/calendars/alice/default/one.ics</d:href>".repeat(1500);
    let body = format!(
        r#"<c:calendar-multiget xmlns:d="DAV:" xmlns:c="urn:ietf:params:xml:ns:caldav"><d:prop><c:calendar-data/></d:prop>{hrefs}</c:calendar-multiget>"#
    );
    let reply = send(&app, ALICE, "REPORT", CAL, &[], &body).await;
    assert_eq!(reply.status, StatusCode::MULTI_STATUS);
    assert_eq!(reply.body.matches("SUMMARY:Only once").count(), 1);
}

#[tokio::test]
async fn conditional_headers_follow_rfc() {
    let app = app().await;
    let uri = format!("{CAL}cond.ics");
    let created = put(
        &app,
        ALICE,
        &uri,
        &event("c", "20261021T100000Z", "20261021T110000Z", "C"),
        &[],
    )
    .await;
    let etag = created.headers[header::ETAG].to_str().unwrap().to_owned();
    let body = event("c", "20261021T100000Z", "20261021T120000Z", "C2");

    // A list with the current tag matches; a weak tag never matches If-Match.
    let list = format!("\"nope\", {etag}");
    assert_eq!(
        put(&app, ALICE, &uri, &body, &[("if-match", &list)])
            .await
            .status,
        StatusCode::NO_CONTENT
    );
    let weak = format!(
        "W/{}",
        put(&app, ALICE, &uri, &body, &[]).await.headers[header::ETAG]
            .to_str()
            .unwrap()
    );
    assert_eq!(
        put(&app, ALICE, &uri, &body, &[("if-match", &weak)])
            .await
            .status,
        StatusCode::PRECONDITION_FAILED
    );

    let current = send(&app, ALICE, "GET", &uri, &[], "").await.headers[header::ETAG]
        .to_str()
        .unwrap()
        .to_owned();
    assert_eq!(
        put(&app, ALICE, &uri, &body, &[("if-none-match", &current)])
            .await
            .status,
        StatusCode::PRECONDITION_FAILED
    );
    assert_eq!(
        put(&app, ALICE, &uri, &body, &[("if-none-match", "\"other\"")])
            .await
            .status,
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        put(&app, ALICE, &uri, &body, &[("if-match", "garbage")])
            .await
            .status,
        StatusCode::BAD_REQUEST
    );
}

#[tokio::test]
async fn calendar_query_filters_by_time_range() {
    let app = app().await;
    put(
        &app,
        ALICE,
        &format!("{CAL}oct.ics"),
        &event("oct", "20261021T100000Z", "20261021T110000Z", "October"),
        &[],
    )
    .await;
    put(
        &app,
        ALICE,
        &format!("{CAL}dec.ics"),
        &event("dec", "20261221T100000Z", "20261221T110000Z", "December"),
        &[],
    )
    .await;

    let body = r#"<c:calendar-query xmlns:d="DAV:" xmlns:c="urn:ietf:params:xml:ns:caldav">
        <d:prop><d:getetag/><c:calendar-data/></d:prop>
        <c:filter><c:comp-filter name="VCALENDAR"><c:comp-filter name="VEVENT">
            <c:time-range start="20261001T000000Z" end="20261101T000000Z"/>
        </c:comp-filter></c:comp-filter></c:filter>
    </c:calendar-query>"#;
    let reply = send(&app, ALICE, "REPORT", CAL, &[("depth", "1")], body).await;
    assert_eq!(reply.status, StatusCode::MULTI_STATUS);
    assert!(reply.body.contains("SUMMARY:October"), "{}", reply.body);
    assert!(!reply.body.contains("December"), "{}", reply.body);

    let todos = r#"<c:calendar-query xmlns:d="DAV:" xmlns:c="urn:ietf:params:xml:ns:caldav">
        <d:prop><d:getetag/></d:prop>
        <c:filter><c:comp-filter name="VCALENDAR"><c:comp-filter name="VTODO"/></c:comp-filter></c:filter>
    </c:calendar-query>"#;
    let reply = send(&app, ALICE, "REPORT", CAL, &[("depth", "1")], todos).await;
    assert!(!reply.body.contains("<d:response>"), "{}", reply.body);
}

#[tokio::test]
async fn unsupported_reports_and_filters() {
    let app = app().await;
    let filter = r#"<c:calendar-query xmlns:c="urn:ietf:params:xml:ns:caldav"><c:filter><c:comp-filter name="VCALENDAR"><c:comp-filter name="VEVENT"><c:prop-filter name="SUMMARY"/></c:comp-filter></c:comp-filter></c:filter></c:calendar-query>"#;
    let reply = send(&app, ALICE, "REPORT", CAL, &[], filter).await;
    assert_eq!(reply.status, StatusCode::FORBIDDEN);
    assert!(reply.body.contains("supported-filter"));

    let sync = r#"<d:sync-collection xmlns:d="DAV:"><d:sync-token/><d:prop><d:getetag/></d:prop></d:sync-collection>"#;
    let reply = send(&app, ALICE, "REPORT", CAL, &[], sync).await;
    assert_eq!(reply.status, StatusCode::FORBIDDEN);
    assert!(reply.body.contains("supported-report"));
}

#[tokio::test]
async fn mkcalendar_and_proppatch() {
    let app = app().await;
    let body = r##"<c:mkcalendar xmlns:d="DAV:" xmlns:c="urn:ietf:params:xml:ns:caldav" xmlns:ic="http://apple.com/ns/ical/">
        <d:set><d:prop>
            <d:displayname>Work &amp; Travel</d:displayname>
            <ic:calendar-color>#336699</ic:calendar-color>
            <c:supported-calendar-component-set><c:comp name="VEVENT"/></c:supported-calendar-component-set>
            <ic:calendar-order>3</ic:calendar-order>
        </d:prop></d:set></c:mkcalendar>"##;
    let reply = send(
        &app,
        ALICE,
        "MKCALENDAR",
        "/dav/calendars/alice/work/",
        &[],
        body,
    )
    .await;
    assert_eq!(reply.status, StatusCode::CREATED);
    let again = send(
        &app,
        ALICE,
        "MKCALENDAR",
        "/dav/calendars/alice/work/",
        &[],
        "",
    )
    .await;
    assert_eq!(again.status, StatusCode::METHOD_NOT_ALLOWED);
    let foreign = send(
        &app,
        ALICE,
        "MKCALENDAR",
        "/dav/calendars/bob/evil/",
        &[],
        "",
    )
    .await;
    assert_eq!(foreign.status, StatusCode::NOT_FOUND);

    let reply = send(
        &app,
        ALICE,
        "PROPFIND",
        "/dav/calendars/alice/work/",
        &[("depth", "0")],
        "",
    )
    .await;
    assert!(
        reply
            .body
            .contains("<d:displayname>Work &amp; Travel</d:displayname>"),
        "{}",
        reply.body
    );
    assert!(
        reply
            .body
            .contains("<ic:calendar-color>#336699</ic:calendar-color>"),
        "{}",
        reply.body
    );
    assert!(!reply.body.contains(r#"name="VTODO""#), "{}", reply.body);

    let rename = r#"<d:propertyupdate xmlns:d="DAV:"><d:set><d:prop><d:displayname>Job</d:displayname></d:prop></d:set></d:propertyupdate>"#;
    let reply = send(
        &app,
        ALICE,
        "PROPPATCH",
        "/dav/calendars/alice/work/",
        &[],
        rename,
    )
    .await;
    assert_eq!(reply.status, StatusCode::MULTI_STATUS);
    assert!(reply.body.contains("HTTP/1.1 200 OK"), "{}", reply.body);
    let reply = send(
        &app,
        ALICE,
        "PROPFIND",
        "/dav/calendars/alice/work/",
        &[("depth", "0")],
        "",
    )
    .await;
    assert!(reply.body.contains("<d:displayname>Job</d:displayname>"));
}

#[tokio::test]
async fn proppatch_cannot_change_access() {
    let app = app().await;
    // Nextcloud CVE-2026-45281 pattern: writing membership/ACL properties.
    let attack = r#"<d:propertyupdate xmlns:d="DAV:"><d:set><d:prop>
        <d:displayname>Hijacked</d:displayname>
        <d:group-member-set><d:href>/dav/principals/bob/</d:href></d:group-member-set>
        <d:owner><d:href>/dav/principals/bob/</d:href></d:owner>
    </d:prop></d:set></d:propertyupdate>"#;
    let reply = send(&app, ALICE, "PROPPATCH", CAL, &[], attack).await;
    assert_eq!(reply.status, StatusCode::MULTI_STATUS);
    assert!(
        reply.body.contains("HTTP/1.1 403 Forbidden"),
        "{}",
        reply.body
    );
    assert!(
        reply.body.contains("HTTP/1.1 424 Failed Dependency"),
        "{}",
        reply.body
    );
    // Atomic: the display name was not changed either.
    let reply = send(&app, ALICE, "PROPFIND", CAL, &[("depth", "0")], "").await;
    assert!(!reply.body.contains("Hijacked"));

    // Bob cannot PROPPATCH Alice's calendar or principal at all.
    let rename = r#"<d:propertyupdate xmlns:d="DAV:"><d:set><d:prop><d:displayname>x</d:displayname></d:prop></d:set></d:propertyupdate>"#;
    for path in [CAL, "/dav/principals/alice/"] {
        let reply = send(&app, BOB, "PROPPATCH", path, &[], rename).await;
        assert_eq!(reply.status, StatusCode::NOT_FOUND, "{path}");
    }

    let bidi = "<d:propertyupdate xmlns:d=\"DAV:\"><d:set><d:prop><d:displayname>evil\u{202E}txt.exe</d:displayname></d:prop></d:set></d:propertyupdate>";
    let reply = send(&app, ALICE, "PROPPATCH", CAL, &[], bidi).await;
    assert!(
        reply.body.contains("HTTP/1.1 409 Conflict"),
        "{}",
        reply.body
    );

    let bad_color = r#"<d:propertyupdate xmlns:d="DAV:" xmlns:ic="http://apple.com/ns/ical/"><d:set><d:prop><ic:calendar-color>javascript:alert(1)</ic:calendar-color></d:prop></d:set></d:propertyupdate>"#;
    let reply = send(&app, ALICE, "PROPPATCH", CAL, &[], bad_color).await;
    assert!(
        reply.body.contains("HTTP/1.1 409 Conflict"),
        "{}",
        reply.body
    );
}

#[tokio::test]
async fn users_cannot_touch_each_others_objects() {
    let app = app().await;
    let bob_uri = "/dav/calendars/bob/default/private.ics";
    put(
        &app,
        BOB,
        bob_uri,
        &event("p", "20261021T100000Z", "20261021T110000Z", "Private"),
        &[],
    )
    .await;

    for method in ["GET", "HEAD", "DELETE", "PROPFIND", "PROPPATCH", "REPORT"] {
        let reply = send(&app, ALICE, method, bob_uri, &[("depth", "0")], "").await;
        assert_eq!(reply.status, StatusCode::NOT_FOUND, "{method}");
        assert!(!reply.body.contains("Private"), "{method}");
    }
    let overwrite = put(
        &app,
        ALICE,
        bob_uri,
        &event("p", "20261021T100000Z", "20261021T110000Z", "Owned"),
        &[],
    )
    .await;
    assert_eq!(overwrite.status, StatusCode::NOT_FOUND);
    let delete_cal = send(
        &app,
        ALICE,
        "DELETE",
        "/dav/calendars/bob/default/",
        &[],
        "",
    )
    .await;
    assert_eq!(delete_cal.status, StatusCode::NOT_FOUND);

    let still_there = send(&app, BOB, "GET", bob_uri, &[], "").await;
    assert_eq!(still_there.status, StatusCode::OK);
    assert!(still_there.body.contains("Private"));
}

#[tokio::test]
async fn delete_objects_and_calendars() {
    let app = app().await;
    let uri = format!("{CAL}gone.ics");
    let created = put(
        &app,
        ALICE,
        &uri,
        &event("g", "20261021T100000Z", "20261021T110000Z", "Gone"),
        &[],
    )
    .await;
    let etag = created.headers[header::ETAG].to_str().unwrap().to_owned();

    assert_eq!(
        send(
            &app,
            ALICE,
            "DELETE",
            &uri,
            &[("if-match", "\"wrong\"")],
            ""
        )
        .await
        .status,
        StatusCode::PRECONDITION_FAILED
    );
    assert_eq!(
        send(&app, ALICE, "DELETE", &uri, &[("if-match", &etag)], "")
            .await
            .status,
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        send(&app, ALICE, "DELETE", &uri, &[], "").await.status,
        StatusCode::NOT_FOUND
    );

    send(
        &app,
        ALICE,
        "MKCALENDAR",
        "/dav/calendars/alice/temp/",
        &[],
        "",
    )
    .await;
    assert_eq!(
        send(&app, ALICE, "DELETE", "/dav/calendars/alice/temp/", &[], "")
            .await
            .status,
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        send(
            &app,
            ALICE,
            "PROPFIND",
            "/dav/calendars/alice/temp/",
            &[("depth", "0")],
            ""
        )
        .await
        .status,
        StatusCode::NOT_FOUND
    );

    // The principal and home collections cannot be deleted.
    assert_eq!(
        send(&app, ALICE, "DELETE", "/dav/calendars/alice/", &[], "")
            .await
            .status,
        StatusCode::METHOD_NOT_ALLOWED
    );
}
