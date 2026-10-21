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
const BOOK: &str = "/dav/addressbooks/alice/default/";

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

fn vcard(uid: &str, name: &str) -> String {
    format!(
        "BEGIN:VCARD\r\nVERSION:4.0\r\nUID:{uid}\r\nFN:{name}\r\nEMAIL:{uid}@example.com\r\nEND:VCARD\r\n"
    )
}

async fn put_card(app: &Router, as_user: (&str, &str), uri: &str, body: &str) -> Reply {
    send(
        app,
        as_user,
        "PUT",
        uri,
        &[("content-type", "text/vcard; charset=utf-8")],
        body,
    )
    .await
}

fn token_of(body: &str) -> String {
    let start = body.find("<d:sync-token>").unwrap() + "<d:sync-token>".len();
    let end = body[start..].find("</d:sync-token>").unwrap() + start;
    body[start..end].to_owned()
}

fn sync_body(token: &str) -> String {
    format!(
        r#"<d:sync-collection xmlns:d="DAV:"><d:sync-token>{token}</d:sync-token><d:sync-level>1</d:sync-level><d:prop><d:getetag/></d:prop></d:sync-collection>"#
    )
}

#[tokio::test]
async fn vcards_can_be_stored_and_read() {
    let app = app().await;
    let uri = format!("{BOOK}alice.vcf");
    let card = vcard("c1", "Carol Contact");
    let created = put_card(&app, ALICE, &uri, &card).await;
    assert_eq!(created.status, StatusCode::CREATED);
    let fetched = send(&app, ALICE, "GET", &uri, &[], "").await;
    assert_eq!(fetched.body, card);
    assert!(
        fetched.headers[header::CONTENT_TYPE]
            .to_str()
            .unwrap()
            .starts_with("text/vcard")
    );

    let reply = send(&app, ALICE, "PROPFIND", BOOK, &[("depth", "1")], "").await;
    assert!(
        reply
            .body
            .contains("<d:href>/dav/addressbooks/alice/default/alice.vcf</d:href>"),
        "{}",
        reply.body
    );
    assert!(
        reply.body.contains("<card:addressbook-multiget/>"),
        "{}",
        reply.body
    );
    assert!(
        reply
            .body
            .contains(r#"<card:address-data-type content-type="text/vcard" version="4.0"/>"#),
        "{}",
        reply.body
    );
}

#[tokio::test]
async fn invalid_contacts_are_rejected() {
    let app = app().await;
    let uri = format!("{BOOK}bad.vcf");
    let reply = put_card(
        &app,
        ALICE,
        &uri,
        "BEGIN:VCARD\r\nVERSION:4.0\r\nFN:No UID\r\nEND:VCARD\r\n",
    )
    .await;
    assert_eq!(reply.status, StatusCode::FORBIDDEN);
    assert!(reply.body.contains("valid-address-data"), "{}", reply.body);

    let ics = event("e", "20261021T100000Z", "20261021T110000Z", "x");
    let reply = put_card(&app, ALICE, &uri, &ics).await;
    assert!(reply.body.contains("valid-address-data"), "{}", reply.body);
    let reply = send(
        &app,
        ALICE,
        "PUT",
        &uri,
        &[("content-type", "text/calendar")],
        &ics,
    )
    .await;
    assert!(
        reply.body.contains("supported-address-data"),
        "{}",
        reply.body
    );

    // A vCard does not go into a calendar.
    let reply = put(
        &app,
        ALICE,
        &format!("{CAL}card.ics"),
        &vcard("v", "V"),
        &[],
    )
    .await;
    assert!(reply.body.contains("valid-calendar-data"), "{}", reply.body);

    put_card(&app, ALICE, &format!("{BOOK}one.vcf"), &vcard("dup", "One")).await;
    let reply = put_card(&app, ALICE, &format!("{BOOK}two.vcf"), &vcard("dup", "Two")).await;
    assert!(
        reply.body.contains(
            "<card:no-uid-conflict><d:href>/dav/addressbooks/alice/default/one.vcf</d:href>"
        ),
        "{}",
        reply.body
    );
}

#[tokio::test]
async fn addressbook_reports() {
    let app = app().await;
    put_card(&app, ALICE, &format!("{BOOK}a.vcf"), &vcard("a", "Anna")).await;
    put_card(
        &app,
        BOB,
        "/dav/addressbooks/bob/default/secret.vcf",
        &vcard("s", "Bob Secret"),
    )
    .await;

    let multiget = r#"<card:addressbook-multiget xmlns:d="DAV:" xmlns:card="urn:ietf:params:xml:ns:carddav">
        <d:prop><d:getetag/><card:address-data/></d:prop>
        <d:href>/dav/addressbooks/alice/default/a.vcf</d:href>
        <d:href>/dav/addressbooks/bob/default/secret.vcf</d:href>
    </card:addressbook-multiget>"#;
    let reply = send(&app, ALICE, "REPORT", BOOK, &[("depth", "1")], multiget).await;
    assert_eq!(reply.status, StatusCode::MULTI_STATUS);
    assert!(reply.body.contains("FN:Anna"), "{}", reply.body);
    assert!(!reply.body.contains("Bob Secret"));
    assert!(reply.body.contains("HTTP/1.1 404 Not Found"));

    let query = r#"<card:addressbook-query xmlns:d="DAV:" xmlns:card="urn:ietf:params:xml:ns:carddav"><d:prop><d:getetag/></d:prop></card:addressbook-query>"#;
    let reply = send(&app, ALICE, "REPORT", BOOK, &[("depth", "1")], query).await;
    assert!(reply.body.contains("a.vcf"), "{}", reply.body);
    let filtered = r#"<card:addressbook-query xmlns:card="urn:ietf:params:xml:ns:carddav"><card:filter><card:prop-filter name="FN"/></card:filter></card:addressbook-query>"#;
    let reply = send(&app, ALICE, "REPORT", BOOK, &[], filtered).await;
    assert_eq!(reply.status, StatusCode::FORBIDDEN);

    // Calendar reports do not apply to address books and vice versa.
    let calendar_query = r#"<c:calendar-query xmlns:c="urn:ietf:params:xml:ns:caldav"><c:filter><c:comp-filter name="VCALENDAR"/></c:filter></c:calendar-query>"#;
    let reply = send(&app, ALICE, "REPORT", BOOK, &[], calendar_query).await;
    assert!(reply.body.contains("supported-report"), "{}", reply.body);
    let reply = send(&app, ALICE, "REPORT", CAL, &[], query).await;
    assert!(reply.body.contains("supported-report"), "{}", reply.body);
}

#[tokio::test]
async fn extended_mkcol() {
    let app = app().await;
    let body = r#"<d:mkcol xmlns:d="DAV:" xmlns:card="urn:ietf:params:xml:ns:carddav"><d:set><d:prop>
        <d:resourcetype><d:collection/><card:addressbook/></d:resourcetype>
        <d:displayname>Team</d:displayname>
        <card:addressbook-description>Colleagues</card:addressbook-description>
    </d:prop></d:set></d:mkcol>"#;
    let reply = send(
        &app,
        ALICE,
        "MKCOL",
        "/dav/addressbooks/alice/team/",
        &[],
        body,
    )
    .await;
    assert_eq!(reply.status, StatusCode::CREATED);
    let reply = send(
        &app,
        ALICE,
        "PROPFIND",
        "/dav/addressbooks/alice/team/",
        &[("depth", "0")],
        "",
    )
    .await;
    assert!(
        reply.body.contains("<d:displayname>Team</d:displayname>"),
        "{}",
        reply.body
    );
    assert!(
        reply
            .body
            .contains("<card:addressbook-description>Colleagues</card:addressbook-description>"),
        "{}",
        reply.body
    );

    let wrong = r#"<d:mkcol xmlns:d="DAV:" xmlns:c="urn:ietf:params:xml:ns:caldav"><d:set><d:prop>
        <d:resourcetype><d:collection/><c:calendar/></d:resourcetype></d:prop></d:set></d:mkcol>"#;
    let reply = send(
        &app,
        ALICE,
        "MKCOL",
        "/dav/addressbooks/alice/wrong/",
        &[],
        wrong,
    )
    .await;
    assert_eq!(reply.status, StatusCode::FORBIDDEN);
    assert!(reply.body.contains("valid-resourcetype"));

    let reply = send(&app, ALICE, "MKCOL", "/dav/calendars/alice/plain/", &[], "").await;
    assert_eq!(reply.status, StatusCode::CREATED);
    let reply = send(&app, ALICE, "MKCOL", "/dav/addressbooks/bob/evil/", &[], "").await;
    assert_eq!(reply.status, StatusCode::NOT_FOUND);
    let reply = send(
        &app,
        ALICE,
        "MKCOL",
        "/dav/addressbooks/alice/team/",
        &[],
        "",
    )
    .await;
    assert_eq!(reply.status, StatusCode::METHOD_NOT_ALLOWED);
}

#[tokio::test]
async fn sync_collection_reports_changes_and_deletions() {
    let app = app().await;
    put(
        &app,
        ALICE,
        &format!("{CAL}keep.ics"),
        &event("keep", "20261021T100000Z", "20261021T110000Z", "Keep"),
        &[],
    )
    .await;
    put(
        &app,
        ALICE,
        &format!("{CAL}gone.ics"),
        &event("gone", "20261021T100000Z", "20261021T110000Z", "Gone"),
        &[],
    )
    .await;

    let initial = send(&app, ALICE, "REPORT", CAL, &[], &sync_body("")).await;
    assert_eq!(initial.status, StatusCode::MULTI_STATUS);
    assert!(
        initial.body.contains("keep.ics") && initial.body.contains("gone.ics"),
        "{}",
        initial.body
    );
    let token = token_of(&initial.body);

    let unchanged = send(&app, ALICE, "REPORT", CAL, &[], &sync_body(&token)).await;
    assert!(
        !unchanged.body.contains("<d:response>"),
        "{}",
        unchanged.body
    );
    assert_eq!(token_of(&unchanged.body), token);

    put(
        &app,
        ALICE,
        &format!("{CAL}keep.ics"),
        &event("keep", "20261021T100000Z", "20261021T120000Z", "Keep 2"),
        &[],
    )
    .await;
    send(&app, ALICE, "DELETE", &format!("{CAL}gone.ics"), &[], "").await;
    put(
        &app,
        ALICE,
        &format!("{CAL}new.ics"),
        &event("new", "20261022T100000Z", "20261022T110000Z", "New"),
        &[],
    )
    .await;

    let delta = send(&app, ALICE, "REPORT", CAL, &[], &sync_body(&token)).await;
    assert_eq!(delta.status, StatusCode::MULTI_STATUS);
    assert!(
        delta
            .body
            .contains("<d:href>/dav/calendars/alice/default/keep.ics</d:href><d:propstat>"),
        "{}",
        delta.body
    );
    assert!(
        delta
            .body
            .contains("<d:href>/dav/calendars/alice/default/new.ics</d:href><d:propstat>"),
        "{}",
        delta.body
    );
    assert!(delta.body.contains("<d:href>/dav/calendars/alice/default/gone.ics</d:href><d:status>HTTP/1.1 404 Not Found</d:status>"), "{}", delta.body);
    assert_ne!(token_of(&delta.body), token);

    // The collection advertises its current token.
    let props = r#"<d:propfind xmlns:d="DAV:"><d:prop><d:sync-token/></d:prop></d:propfind>"#;
    let reply = send(&app, ALICE, "PROPFIND", CAL, &[("depth", "0")], props).await;
    assert!(reply.body.contains(&token_of(&delta.body)));
}

#[tokio::test]
async fn sync_tokens_cannot_cross_collections() {
    let app = app().await;
    put(
        &app,
        BOB,
        "/dav/calendars/bob/default/b.ics",
        &event("b", "20261021T100000Z", "20261021T110000Z", "Bob private"),
        &[],
    )
    .await;
    let bob_token = token_of(
        &send(
            &app,
            BOB,
            "REPORT",
            "/dav/calendars/bob/default/",
            &[],
            &sync_body(""),
        )
        .await
        .body,
    );
    let alice_cal_token = token_of(
        &send(&app, ALICE, "REPORT", CAL, &[], &sync_body(""))
            .await
            .body,
    );

    // Bob's token on Alice's calendar, Alice's calendar token on her address
    // book, a forged future revision and garbage are all rejected.
    let future = format!(
        "{}9999",
        &alice_cal_token[..alice_cal_token.rfind(':').unwrap() + 1]
    );
    for (uri, token) in [
        (CAL, bob_token.as_str()),
        (BOOK, alice_cal_token.as_str()),
        (CAL, future.as_str()),
        (CAL, "http://example.com/sync/1"),
    ] {
        let reply = send(&app, ALICE, "REPORT", uri, &[], &sync_body(token)).await;
        assert_eq!(reply.status, StatusCode::FORBIDDEN, "{uri} {token}");
        assert!(reply.body.contains("valid-sync-token"));
        assert!(!reply.body.contains("Bob private"));
    }
    // Bob's calendar is still not reachable for Alice at all.
    let reply = send(
        &app,
        ALICE,
        "REPORT",
        "/dav/calendars/bob/default/",
        &[],
        &sync_body(&bob_token),
    )
    .await;
    assert_eq!(reply.status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn sync_truncates_and_continues() {
    let app = app().await;
    for i in 0..3 {
        put(
            &app,
            ALICE,
            &format!("{CAL}e{i}.ics"),
            &event(
                &format!("e{i}"),
                "20261021T100000Z",
                "20261021T110000Z",
                "E",
            ),
            &[],
        )
        .await;
    }
    let limited = |token: &str| {
        format!(
            r#"<d:sync-collection xmlns:d="DAV:"><d:sync-token>{token}</d:sync-token><d:sync-level>1</d:sync-level><d:limit><d:nresults>2</d:nresults></d:limit><d:prop><d:getetag/></d:prop></d:sync-collection>"#
        )
    };
    // First page: two objects plus a 507 entry for the collection.
    let first = send(&app, ALICE, "REPORT", CAL, &[], &limited("")).await;
    assert_eq!(first.status, StatusCode::MULTI_STATUS);
    assert!(
        first.body.contains("e0.ics") && first.body.contains("e1.ics"),
        "{}",
        first.body
    );
    assert!(!first.body.contains("e2.ics"), "{}", first.body);
    assert!(
        first.body.contains("<d:href>/dav/calendars/alice/default/</d:href><d:status>HTTP/1.1 507 Insufficient Storage</d:status>"),
        "{}",
        first.body
    );
    // Second page continues exactly after the first.
    let second = send(
        &app,
        ALICE,
        "REPORT",
        CAL,
        &[],
        &limited(&token_of(&first.body)),
    )
    .await;
    assert!(second.body.contains("e2.ics"), "{}", second.body);
    assert!(
        !second.body.contains("e0.ics") && !second.body.contains("507"),
        "{}",
        second.body
    );
    // Fully synced: nothing left.
    let third = send(
        &app,
        ALICE,
        "REPORT",
        CAL,
        &[],
        &sync_body(&token_of(&second.body)),
    )
    .await;
    assert!(!third.body.contains("<d:response>"), "{}", third.body);
}

#[tokio::test]
async fn token_of_deleted_collection_is_rejected_for_new_one() {
    let app = app().await;
    let uri = "/dav/calendars/alice/temp/";
    send(&app, ALICE, "MKCALENDAR", uri, &[], "").await;
    let old = token_of(
        &send(&app, ALICE, "REPORT", uri, &[], &sync_body(""))
            .await
            .body,
    );
    send(&app, ALICE, "DELETE", uri, &[], "").await;
    send(&app, ALICE, "MKCALENDAR", uri, &[], "").await;
    let reply = send(&app, ALICE, "REPORT", uri, &[], &sync_body(&old)).await;
    assert_eq!(reply.status, StatusCode::FORBIDDEN);
    assert!(reply.body.contains("valid-sync-token"));
}
