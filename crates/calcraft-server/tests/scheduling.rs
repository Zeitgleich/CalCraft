//! CalDAV scheduling (RFC 6638) between users of one server: invitations,
//! replies, cancellations, recurrence, Schedule-Tag, free/busy, limits and
//! the attacks the design has to stop (organizer spoofing, attendees
//! tampering with the organizer's event, UID hijacking).

#![allow(clippy::unwrap_used, clippy::expect_used)]

use axum::{
    Router,
    body::Body,
    http::{HeaderMap, Method, Request, StatusCode, header},
};
use base64::{Engine, engine::general_purpose::STANDARD};
use calcraft_core::CollectionKind;
use calcraft_server::{AppState, Config, auth::hash_password, config::SchedulingConfig, router};
use calcraft_store::{Store, User};
use http_body_util::BodyExt;
use tower::ServiceExt;

const PASSWORD: &str = "a long password 123";
const DOMAIN: &str = "example.org";

struct Fx {
    app: Router,
    store: Store,
    alice: User,
    bob: User,
    carol: User,
    mallory: User,
}

async fn fixture_with(config: SchedulingConfig) -> Fx {
    let store = Store::in_memory().await.unwrap();
    let hash = hash_password(PASSWORD).unwrap();
    let mut users = Vec::new();
    for name in ["alice", "bob", "carol", "mallory", "dave"] {
        users.push(
            store
                .create_user(&name.parse().unwrap(), &hash, None)
                .await
                .unwrap(),
        );
    }
    // Dave is disabled: he gets nothing, like an unknown user.
    store
        .set_disabled(&"dave".parse().unwrap(), true)
        .await
        .unwrap();
    let mut full = Config {
        scheduling: config,
        ..Config::default()
    };
    full.server.max_concurrent_requests_per_client = u32::MAX;
    full.server.client_rate_burst = u32::MAX;
    full.server.client_rate_per_second = u32::MAX;
    let app = router(&full, AppState::new(store.clone()).unwrap());
    let mut users = users.into_iter();
    Fx {
        app,
        alice: users.next().unwrap(),
        bob: users.next().unwrap(),
        carol: users.next().unwrap(),
        mallory: users.next().unwrap(),
        store,
    }
}

fn enabled() -> SchedulingConfig {
    SchedulingConfig {
        enabled: true,
        domain: Some(DOMAIN.to_owned()),
        ..SchedulingConfig::default()
    }
}

async fn fixture() -> Fx {
    fixture_with(enabled()).await
}

fn basic(user: &str) -> String {
    format!("Basic {}", STANDARD.encode(format!("{user}:{PASSWORD}")))
}

struct Reply {
    status: StatusCode,
    headers: HeaderMap,
    body: String,
}

async fn send(
    app: &Router,
    user: &str,
    method: &str,
    uri: &str,
    headers: &[(&str, &str)],
    body: &str,
) -> Reply {
    let mut builder = Request::builder()
        .method(Method::from_bytes(method.as_bytes()).unwrap())
        .uri(uri)
        .header(header::AUTHORIZATION, basic(user));
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
        body: String::from_utf8_lossy(&body).into_owned(),
    }
}

async fn put(app: &Router, user: &str, uri: &str, body: &str, headers: &[(&str, &str)]) -> Reply {
    let mut all = vec![("content-type", "text/calendar")];
    all.extend_from_slice(headers);
    send(app, user, "PUT", uri, &all, body).await
}

fn calendar(body: &str) -> String {
    format!("BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//Test//EN\r\n{body}END:VCALENDAR\r\n")
}

/// A meeting organized by alice with the given attendee lines.
fn meeting(uid: &str, start: &str, summary: &str, attendees: &str) -> String {
    calendar(&format!(
        "BEGIN:VEVENT\r\nUID:{uid}\r\nDTSTAMP:20261001T000000Z\r\nDTSTART:{start}\r\nDURATION:PT1H\r\nSUMMARY:{summary}\r\nORGANIZER:mailto:alice@example.org\r\nATTENDEE;PARTSTAT=ACCEPTED:mailto:alice@example.org\r\n{attendees}END:VEVENT\r\n"
    ))
}

const BOB_LINE: &str = "ATTENDEE;PARTSTAT=NEEDS-ACTION;RSVP=TRUE:mailto:bob@example.org\r\n";
const CAROL_LINE: &str = "ATTENDEE;PARTSTAT=NEEDS-ACTION;RSVP=TRUE:mailto:carol@example.org\r\n";
const ALICE_CAL: &str = "/dav/calendars/alice/default/";

/// The copy of `uid` in a user's calendars.
async fn copy(fx: &Fx, user: &User, uid: &str) -> Option<String> {
    fx.store
        .scheduling_object(user.id, uid)
        .await
        .unwrap()
        .map(|o| String::from_utf8(o.content).unwrap())
}

async fn copy_href(fx: &Fx, user: &User, uid: &str) -> String {
    let object = fx
        .store
        .scheduling_object(user.id, uid)
        .await
        .unwrap()
        .unwrap();
    format!(
        "/dav/calendars/{}/{}/{}",
        user.username, object.collection.slug, object.name
    )
}

async fn inbox(fx: &Fx, user: &User) -> Vec<String> {
    let mut out = Vec::new();
    for meta in fx.store.inbox_messages(user.id, 100).await.unwrap() {
        let item = fx
            .store
            .inbox_message(user.id, &meta.name)
            .await
            .unwrap()
            .unwrap();
        out.push(String::from_utf8(item.content).unwrap());
    }
    out
}

/// Unfolds content lines for substring checks.
fn unfold(text: &str) -> String {
    text.replace("\r\n ", "").replace("&#13;\n ", "")
}

#[tokio::test]
async fn disabled_by_default() {
    let fx = fixture_with(SchedulingConfig::default()).await;
    let options = send(&fx.app, "alice", "OPTIONS", "/dav/", &[], "").await;
    let dav = options.headers["dav"].to_str().unwrap();
    assert!(!dav.contains("calendar-auto-schedule"));
    for path in [
        "/dav/scheduling/alice/inbox/",
        "/dav/scheduling/alice/outbox/",
    ] {
        let reply = send(&fx.app, "alice", "PROPFIND", path, &[("depth", "0")], "").await;
        assert_eq!(reply.status, StatusCode::NOT_FOUND, "{path}");
    }
    let principal = send(
        &fx.app,
        "alice",
        "PROPFIND",
        "/dav/principals/alice/",
        &[("depth", "0")],
        "",
    )
    .await;
    assert!(!principal.body.contains("calendar-user-address-set"));
    let reply = put(
        &fx.app,
        "alice",
        &format!("{ALICE_CAL}m.ics"),
        &meeting("off-1", "20261101T100000Z", "Off", BOB_LINE),
        &[],
    )
    .await;
    assert_eq!(reply.status, StatusCode::CREATED);
    // Stored exactly as sent, with its ETag, and nobody was invited.
    assert!(reply.headers.contains_key("etag"));
    assert!(!reply.headers.contains_key("schedule-tag"));
    assert!(copy(&fx, &fx.bob, "off-1").await.is_none());
    assert!(inbox(&fx, &fx.bob).await.is_empty());
}

#[tokio::test]
async fn discovery_properties() {
    let fx = fixture().await;
    fx.store
        .set_user_email(fx.alice.id, Some("alice.smith@corp.example"))
        .await
        .unwrap();
    let options = send(&fx.app, "alice", "OPTIONS", "/dav/", &[], "").await;
    assert!(
        options.headers["dav"]
            .to_str()
            .unwrap()
            .contains("calendar-auto-schedule")
    );
    let body = r#"<d:propfind xmlns:d="DAV:" xmlns:c="urn:ietf:params:xml:ns:caldav"><d:prop><c:calendar-user-address-set/><c:schedule-inbox-URL/><c:schedule-outbox-URL/><c:calendar-user-type/></d:prop></d:propfind>"#;
    let reply = send(
        &fx.app,
        "alice",
        "PROPFIND",
        "/dav/principals/alice/",
        &[("depth", "0")],
        body,
    )
    .await;
    assert_eq!(reply.status, StatusCode::MULTI_STATUS);
    for expected in [
        "<d:href>mailto:alice.smith@corp.example</d:href>",
        "<d:href>mailto:alice@example.org</d:href>",
        "<d:href>/dav/principals/alice/</d:href>",
        "/dav/scheduling/alice/inbox/",
        "/dav/scheduling/alice/outbox/",
        "INDIVIDUAL",
    ] {
        assert!(reply.body.contains(expected), "{expected}: {}", reply.body);
    }
    let reply = send(
        &fx.app,
        "alice",
        "PROPFIND",
        "/dav/scheduling/alice/inbox/",
        &[("depth", "0")],
        "",
    )
    .await;
    assert_eq!(reply.status, StatusCode::MULTI_STATUS);
    for expected in [
        "schedule-inbox",
        "schedule-deliver",
        "schedule-default-calendar-URL",
        "/dav/calendars/alice/default/",
    ] {
        assert!(reply.body.contains(expected), "{expected}: {}", reply.body);
    }
    let reply = send(
        &fx.app,
        "alice",
        "PROPFIND",
        "/dav/scheduling/alice/outbox/",
        &[("depth", "0")],
        "",
    )
    .await;
    assert!(reply.body.contains("schedule-outbox"));
    assert!(reply.body.contains("schedule-send"));
    // No PUT into the inbox or outbox, no MKCALENDAR, PROPPATCH or COPY.
    for (method, path) in [
        ("PUT", "/dav/scheduling/alice/inbox/x.ics"),
        ("PUT", "/dav/scheduling/alice/outbox/"),
        ("PROPPATCH", "/dav/scheduling/alice/inbox/"),
        ("MKCALENDAR", "/dav/scheduling/alice/inbox/"),
        ("POST", "/dav/scheduling/alice/inbox/"),
        ("POST", ALICE_CAL),
        ("DELETE", "/dav/scheduling/alice/outbox/"),
    ] {
        let reply = send(&fx.app, "alice", method, path, &[], "").await;
        assert_eq!(
            reply.status,
            StatusCode::METHOD_NOT_ALLOWED,
            "{method} {path}"
        );
    }
}

#[tokio::test]
async fn invitation_reply_update_and_cancel() {
    let fx = fixture().await;
    let attendees = format!(
        "{BOB_LINE}{CAROL_LINE}ATTENDEE:mailto:nobody@example.org\r\nATTENDEE:mailto:x@elsewhere.example\r\nATTENDEE:mailto:dave@example.org\r\n"
    );
    let uri = format!("{ALICE_CAL}meet.ics");
    let reply = put(
        &fx.app,
        "alice",
        &uri,
        &meeting("m1", "20261101T100000Z", "Planning", &attendees),
        &[],
    )
    .await;
    assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.body);
    // The server changed the object: no ETag, but a Schedule-Tag.
    assert!(!reply.headers.contains_key("etag"));
    let tag = reply.headers["schedule-tag"].to_str().unwrap().to_owned();

    let organizer = unfold(&send(&fx.app, "alice", "GET", &uri, &[], "").await.body);
    assert!(
        organizer.contains("SCHEDULE-STATUS=1.2:mailto:bob@example.org"),
        "{organizer}"
    );
    assert!(organizer.contains("SCHEDULE-STATUS=1.2:mailto:carol@example.org"));
    assert!(organizer.contains("SCHEDULE-STATUS=3.7:mailto:nobody@example.org"));
    assert!(organizer.contains("SCHEDULE-STATUS=5.3:mailto:x@elsewhere.example"));
    // A disabled user is indistinguishable from an unknown one.
    assert!(organizer.contains("SCHEDULE-STATUS=3.7:mailto:dave@example.org"));

    let bob_copy = copy(&fx, &fx.bob, "m1")
        .await
        .expect("bob got the invitation");
    assert!(bob_copy.contains("SUMMARY:Planning"));
    assert!(bob_copy.contains("PARTSTAT=NEEDS-ACTION;RSVP=TRUE:mailto:bob@example.org"));
    assert!(!bob_copy.contains("SCHEDULE-STATUS"));
    let messages = inbox(&fx, &fx.bob).await;
    assert_eq!(messages.len(), 1);
    assert!(messages[0].contains("METHOD:REQUEST"));

    // Bob accepts, and tries to change everything else as well.
    let bob_uri = copy_href(&fx, &fx.bob, "m1").await;
    let get = send(&fx.app, "bob", "GET", &bob_uri, &[], "").await;
    let bob_tag = get.headers["schedule-tag"].to_str().unwrap().to_owned();
    let accepted = unfold(&get.body)
        .replace(
            "PARTSTAT=NEEDS-ACTION;RSVP=TRUE:mailto:bob@example.org",
            "PARTSTAT=ACCEPTED;RSVP=TRUE:mailto:bob@example.org",
        )
        .replace(
            "PARTSTAT=NEEDS-ACTION;RSVP=TRUE:mailto:carol@example.org",
            "PARTSTAT=DECLINED:mailto:carol@example.org",
        )
        .replace("SUMMARY:Planning", "SUMMARY:Hijacked")
        .replace("DTSTART:20261101T100000Z", "DTSTART:20261102T100000Z");
    let reply = put(
        &fx.app,
        "bob",
        &bob_uri,
        &accepted,
        &[("if-schedule-tag-match", &bob_tag)],
    )
    .await;
    assert_eq!(reply.status, StatusCode::NO_CONTENT, "{}", reply.body);
    let organizer = unfold(&send(&fx.app, "alice", "GET", &uri, &[], "").await.body);
    assert!(
        organizer
            .contains("PARTSTAT=ACCEPTED;RSVP=TRUE;SCHEDULE-STATUS=1.2:mailto:bob@example.org"),
        "{organizer}"
    );
    assert!(organizer.contains("SUMMARY:Planning"));
    assert!(organizer.contains("DTSTART:20261101T100000Z"));
    assert!(
        organizer.contains(
            "PARTSTAT=NEEDS-ACTION;RSVP=TRUE;SCHEDULE-STATUS=1.2:mailto:carol@example.org"
        )
    );
    // The reply did not change the organizer's Schedule-Tag.
    let get = send(&fx.app, "alice", "GET", &uri, &[], "").await;
    assert_eq!(get.headers["schedule-tag"].to_str().unwrap(), tag);
    let alice_inbox = inbox(&fx, &fx.alice).await;
    assert_eq!(alice_inbox.len(), 1);
    assert!(alice_inbox[0].contains("METHOD:REPLY"));
    assert!(!alice_inbox[0].contains("carol"));
    let bob_now = copy(&fx, &fx.bob, "m1").await.unwrap();
    assert!(unfold(&bob_now).contains("SCHEDULE-STATUS=1.2:mailto:alice@example.org"));

    // Alice's client saw neither the reply nor the new ETag: with her
    // Schedule-Tag the update goes through and bob's answer is kept.
    let moved = meeting("m1", "20261101T140000Z", "Planning", &attendees);
    let reply = put(
        &fx.app,
        "alice",
        &uri,
        &moved,
        &[("if-schedule-tag-match", &tag)],
    )
    .await;
    assert_eq!(reply.status, StatusCode::NO_CONTENT, "{}", reply.body);
    let organizer = unfold(&send(&fx.app, "alice", "GET", &uri, &[], "").await.body);
    assert!(organizer.contains("PARTSTAT=ACCEPTED"), "{organizer}");
    let bob_now = unfold(&copy(&fx, &fx.bob, "m1").await.unwrap());
    assert!(bob_now.contains("DTSTART:20261101T140000Z"), "{bob_now}");
    // A stale Schedule-Tag fails.
    let reply = put(
        &fx.app,
        "alice",
        &uri,
        &moved,
        &[("if-schedule-tag-match", &tag)],
    )
    .await;
    assert_eq!(reply.status, StatusCode::PRECONDITION_FAILED);

    // Carol is removed: her copy is cancelled, bob's is not.
    let without_carol = meeting("m1", "20261101T140000Z", "Planning", BOB_LINE);
    let reply = put(&fx.app, "alice", &uri, &without_carol, &[]).await;
    assert_eq!(reply.status, StatusCode::NO_CONTENT);
    assert!(
        copy(&fx, &fx.carol, "m1")
            .await
            .unwrap()
            .contains("STATUS:CANCELLED")
    );
    assert!(
        !copy(&fx, &fx.bob, "m1")
            .await
            .unwrap()
            .contains("STATUS:CANCELLED")
    );

    // Alice deletes the meeting: bob's copy is cancelled.
    let reply = send(&fx.app, "alice", "DELETE", &uri, &[], "").await;
    assert_eq!(reply.status, StatusCode::NO_CONTENT);
    assert!(
        copy(&fx, &fx.bob, "m1")
            .await
            .unwrap()
            .contains("STATUS:CANCELLED")
    );
    assert!(
        inbox(&fx, &fx.bob)
            .await
            .iter()
            .any(|m| m.contains("METHOD:CANCEL"))
    );
}

#[tokio::test]
async fn unchanged_updates_send_nothing_and_force_send_does() {
    let fx = fixture().await;
    let uri = format!("{ALICE_CAL}quiet.ics");
    put(
        &fx.app,
        "alice",
        &uri,
        &meeting("q1", "20261101T100000Z", "Q", BOB_LINE),
        &[],
    )
    .await;
    let first = fx.store.inbox_revision(fx.bob.id).await.unwrap();
    // Only DTSTAMP changes: not significant.
    let same = meeting("q1", "20261101T100000Z", "Q", BOB_LINE)
        .replace("DTSTAMP:20261001T000000Z", "DTSTAMP:20261005T000000Z");
    put(&fx.app, "alice", &uri, &same, &[]).await;
    assert_eq!(fx.store.inbox_revision(fx.bob.id).await.unwrap(), first);
    let forced = meeting(
        "q1",
        "20261101T100000Z",
        "Q",
        "ATTENDEE;SCHEDULE-FORCE-SEND=REQUEST:mailto:bob@example.org\r\n",
    );
    put(&fx.app, "alice", &uri, &forced, &[]).await;
    assert!(fx.store.inbox_revision(fx.bob.id).await.unwrap() > first);
    // SCHEDULE-FORCE-SEND is never stored.
    let stored = send(&fx.app, "alice", "GET", &uri, &[], "").await.body;
    assert!(!stored.contains("SCHEDULE-FORCE-SEND"));
}

#[tokio::test]
async fn schedule_agent_client_and_none_are_respected() {
    let fx = fixture().await;
    let attendees = "ATTENDEE;SCHEDULE-AGENT=CLIENT;SCHEDULE-STATUS=1.1:mailto:bob@example.org\r\nATTENDEE;SCHEDULE-AGENT=NONE:mailto:carol@example.org\r\n";
    let uri = format!("{ALICE_CAL}client.ics");
    let reply = put(
        &fx.app,
        "alice",
        &uri,
        &meeting("c1", "20261101T100000Z", "C", attendees),
        &[],
    )
    .await;
    assert_eq!(reply.status, StatusCode::CREATED);
    assert!(copy(&fx, &fx.bob, "c1").await.is_none());
    assert!(copy(&fx, &fx.carol, "c1").await.is_none());
    // The client's own status for its attendee is kept.
    let stored = unfold(&send(&fx.app, "alice", "GET", &uri, &[], "").await.body);
    assert!(stored.contains("SCHEDULE-STATUS=1.1"), "{stored}");
}

#[tokio::test]
async fn client_sent_schedule_status_is_replaced() {
    let fx = fixture().await;
    let uri = format!("{ALICE_CAL}status.ics");
    let attendees = "ATTENDEE;SCHEDULE-STATUS=2.0:mailto:nobody@example.org\r\n";
    put(
        &fx.app,
        "alice",
        &uri,
        &meeting("st1", "20261101T100000Z", "S", attendees),
        &[],
    )
    .await;
    let stored = unfold(&send(&fx.app, "alice", "GET", &uri, &[], "").await.body);
    assert!(stored.contains("SCHEDULE-STATUS=3.7"), "{stored}");
    assert!(!stored.contains("SCHEDULE-STATUS=2.0"));
}

#[tokio::test]
async fn organizer_spoofing_reaches_nobody() {
    let fx = fixture().await;
    // Mallory claims alice organizes and invites bob.
    let reply = put(
        &fx.app,
        "mallory",
        "/dav/calendars/mallory/default/spoof.ics",
        &meeting("spoof-1", "20261101T100000Z", "Spoof", BOB_LINE),
        &[],
    )
    .await;
    assert_eq!(reply.status, StatusCode::CREATED);
    // Stored as sent (a plain object for mallory), nothing delivered.
    assert!(reply.headers.contains_key("etag"));
    assert!(copy(&fx, &fx.bob, "spoof-1").await.is_none());
    assert!(inbox(&fx, &fx.bob).await.is_empty());
    assert!(inbox(&fx, &fx.alice).await.is_empty());

    // Mallory as an attendee of a made-up event of alice's: no reply lands.
    let fake = meeting(
        "spoof-2",
        "20261101T100000Z",
        "Fake",
        "ATTENDEE;PARTSTAT=ACCEPTED:mailto:mallory@example.org\r\n",
    );
    let reply = put(
        &fx.app,
        "mallory",
        "/dav/calendars/mallory/default/fake.ics",
        &fake,
        &[],
    )
    .await;
    assert_eq!(reply.status, StatusCode::CREATED);
    assert!(copy(&fx, &fx.alice, "spoof-2").await.is_none());
    let stored = unfold(
        &send(
            &fx.app,
            "mallory",
            "GET",
            "/dav/calendars/mallory/default/fake.ics",
            &[],
            "",
        )
        .await
        .body,
    );
    assert!(stored.contains("SCHEDULE-STATUS=3.8"), "{stored}");

    // Alice invites bob; mallory then organizes an event with the same UID
    // and invites bob too: bob's copy of alice's event stays untouched.
    put(
        &fx.app,
        "alice",
        &format!("{ALICE_CAL}real.ics"),
        &meeting("shared-uid", "20261101T100000Z", "Real", BOB_LINE),
        &[],
    )
    .await;
    let before = copy(&fx, &fx.bob, "shared-uid").await.unwrap();
    let hijack = calendar(
        "BEGIN:VEVENT\r\nUID:shared-uid\r\nDTSTART:20261101T100000Z\r\nSUMMARY:Hijack\r\nORGANIZER:mailto:mallory@example.org\r\nATTENDEE:mailto:bob@example.org\r\nEND:VEVENT\r\n",
    );
    let reply = put(
        &fx.app,
        "mallory",
        "/dav/calendars/mallory/default/h.ics",
        &hijack,
        &[],
    )
    .await;
    assert_eq!(reply.status, StatusCode::CREATED);
    assert_eq!(copy(&fx, &fx.bob, "shared-uid").await.unwrap(), before);
    let stored = unfold(
        &send(
            &fx.app,
            "mallory",
            "GET",
            "/dav/calendars/mallory/default/h.ics",
            &[],
            "",
        )
        .await
        .body,
    );
    assert!(
        stored.contains("SCHEDULE-STATUS=5.1:mailto:bob@example.org"),
        "{stored}"
    );
}

#[tokio::test]
async fn attendee_cannot_reply_for_others_or_add_instances() {
    let fx = fixture().await;
    let uri = format!("{ALICE_CAL}series.ics");
    let series = calendar(&format!(
        "BEGIN:VEVENT\r\nUID:r1\r\nDTSTAMP:20261001T000000Z\r\nDTSTART:20261101T100000Z\r\nDURATION:PT1H\r\nRRULE:FREQ=WEEKLY;COUNT=4\r\nSUMMARY:Weekly\r\nORGANIZER:mailto:alice@example.org\r\n{BOB_LINE}{CAROL_LINE}END:VEVENT\r\n"
    ));
    put(&fx.app, "alice", &uri, &series, &[]).await;
    let bob_uri = copy_href(&fx, &fx.bob, "r1").await;
    let bob_copy = unfold(&send(&fx.app, "bob", "GET", &bob_uri, &[], "").await.body);
    // Bob declines the second instance and invents a fifth one; he also
    // claims carol accepted.
    let master_end = bob_copy.rfind("END:VEVENT").unwrap() + "END:VEVENT\r\n".len();
    let extra = "BEGIN:VEVENT\r\nUID:r1\r\nRECURRENCE-ID:20261108T100000Z\r\nDTSTART:20261108T100000Z\r\nDURATION:PT1H\r\nORGANIZER:mailto:alice@example.org\r\nATTENDEE;PARTSTAT=DECLINED:mailto:bob@example.org\r\nATTENDEE;PARTSTAT=ACCEPTED:mailto:carol@example.org\r\nEND:VEVENT\r\nBEGIN:VEVENT\r\nUID:r1\r\nRECURRENCE-ID:20261129T100000Z\r\nDTSTART:20261129T100000Z\r\nDURATION:PT1H\r\nORGANIZER:mailto:alice@example.org\r\nATTENDEE;PARTSTAT=DECLINED:mailto:bob@example.org\r\nEND:VEVENT\r\n";
    let changed = format!(
        "{}{extra}{}",
        &bob_copy[..master_end],
        &bob_copy[master_end..]
    );
    let reply = put(&fx.app, "bob", &bob_uri, &changed, &[]).await;
    assert_eq!(reply.status, StatusCode::NO_CONTENT, "{}", reply.body);
    let organizer = unfold(&send(&fx.app, "alice", "GET", &uri, &[], "").await.body);
    assert!(
        organizer.contains("RECURRENCE-ID:20261108T100000Z"),
        "{organizer}"
    );
    assert!(!organizer.contains("20261129T100000Z"), "{organizer}");
    assert!(!organizer.contains("PARTSTAT=ACCEPTED"), "{organizer}");
    assert!(organizer.contains("PARTSTAT=DECLINED;RSVP=TRUE:mailto:bob@example.org"));

    // An EXDATE in bob's copy declines that instance too.
    let bob_copy = unfold(&send(&fx.app, "bob", "GET", &bob_uri, &[], "").await.body);
    let exdate = bob_copy.replacen(
        "SUMMARY:Weekly",
        "EXDATE:20261115T100000Z\r\nSUMMARY:Weekly",
        1,
    );
    let reply = put(&fx.app, "bob", &bob_uri, &exdate, &[]).await;
    assert_eq!(reply.status, StatusCode::NO_CONTENT);
    let organizer = unfold(&send(&fx.app, "alice", "GET", &uri, &[], "").await.body);
    assert!(
        organizer.contains("RECURRENCE-ID:20261115T100000Z"),
        "{organizer}"
    );
}

#[tokio::test]
async fn overrides_reach_only_their_attendees() {
    let fx = fixture().await;
    let uri = format!("{ALICE_CAL}ov.ics");
    // Carol is invited to one instance only; bob is not invited to it.
    let body = calendar(&format!(
        "BEGIN:VEVENT\r\nUID:o1\r\nDTSTART:20261101T100000Z\r\nDURATION:PT1H\r\nRRULE:FREQ=DAILY;COUNT=3\r\nORGANIZER:mailto:alice@example.org\r\n{BOB_LINE}END:VEVENT\r\nBEGIN:VEVENT\r\nUID:o1\r\nRECURRENCE-ID:20261102T100000Z\r\nDTSTART:20261102T120000Z\r\nDURATION:PT1H\r\nORGANIZER:mailto:alice@example.org\r\n{CAROL_LINE}END:VEVENT\r\n"
    ));
    let reply = put(&fx.app, "alice", &uri, &body, &[]).await;
    assert_eq!(reply.status, StatusCode::CREATED);
    let bob = unfold(&copy(&fx, &fx.bob, "o1").await.unwrap());
    assert!(bob.contains("EXDATE:20261102T100000Z"), "{bob}");
    assert!(!bob.contains("carol"));
    let carol = unfold(&copy(&fx, &fx.carol, "o1").await.unwrap());
    assert!(carol.contains("RECURRENCE-ID:20261102T100000Z"));
    assert!(!carol.contains("RRULE"));
}

#[tokio::test]
async fn attendee_delete_declines() {
    let fx = fixture().await;
    let uri = format!("{ALICE_CAL}d.ics");
    put(
        &fx.app,
        "alice",
        &uri,
        &meeting("d1", "20261101T100000Z", "D", BOB_LINE),
        &[],
    )
    .await;
    let bob_uri = copy_href(&fx, &fx.bob, "d1").await;
    let reply = send(&fx.app, "bob", "DELETE", &bob_uri, &[], "").await;
    assert_eq!(reply.status, StatusCode::NO_CONTENT);
    let organizer = unfold(&send(&fx.app, "alice", "GET", &uri, &[], "").await.body);
    assert!(organizer.contains("PARTSTAT=DECLINED"), "{organizer}");
    // Wrong Schedule-Tag on DELETE.
    let reply = send(
        &fx.app,
        "alice",
        "DELETE",
        &uri,
        &[("if-schedule-tag-match", "\"nope\"")],
        "",
    )
    .await;
    assert_eq!(reply.status, StatusCode::PRECONDITION_FAILED);
}

#[tokio::test]
async fn unique_scheduling_object_per_uid() {
    let fx = fixture().await;
    let body = meeting("u1", "20261101T100000Z", "U", BOB_LINE);
    put(&fx.app, "alice", &format!("{ALICE_CAL}u.ics"), &body, &[]).await;
    send(
        &fx.app,
        "alice",
        "MKCALENDAR",
        "/dav/calendars/alice/second/",
        &[],
        "",
    )
    .await;
    let reply = put(
        &fx.app,
        "alice",
        "/dav/calendars/alice/second/u.ics",
        &body,
        &[],
    )
    .await;
    assert_eq!(reply.status, StatusCode::FORBIDDEN);
    assert!(reply.body.contains("unique-scheduling-object-resource"));
    // MOVE keeps the object a scheduling object.
    let reply = send(
        &fx.app,
        "alice",
        "MOVE",
        &format!("{ALICE_CAL}u.ics"),
        &[
            ("destination", "/dav/calendars/alice/second/u.ics"),
            ("host", "localhost"),
        ],
        "",
    )
    .await;
    assert_eq!(reply.status, StatusCode::CREATED);
    let get = send(
        &fx.app,
        "alice",
        "GET",
        "/dav/calendars/alice/second/u.ics",
        &[],
        "",
    )
    .await;
    assert!(get.headers.contains_key("schedule-tag"));
}

#[tokio::test]
async fn limits() {
    let config = SchedulingConfig {
        max_attendees: 2,
        max_recipients_per_hour: 3,
        ..enabled()
    };
    let fx = fixture_with(config).await;
    let many = format!("{BOB_LINE}{CAROL_LINE}ATTENDEE:mailto:mallory@example.org\r\n");
    let reply = put(
        &fx.app,
        "alice",
        &format!("{ALICE_CAL}big.ics"),
        &meeting("big", "20261101T100000Z", "Big", &many),
        &[],
    )
    .await;
    assert_eq!(reply.status, StatusCode::FORBIDDEN);
    assert!(reply.body.contains("max-attendees-per-instance"));

    // Rate limit: three recipients per hour, the fourth is not delivered,
    // but the organizer's object is stored.
    let pair = format!("{BOB_LINE}{CAROL_LINE}");
    put(
        &fx.app,
        "alice",
        &format!("{ALICE_CAL}r1.ics"),
        &meeting("rl1", "20261101T100000Z", "1", &pair),
        &[],
    )
    .await;
    let reply = put(
        &fx.app,
        "alice",
        &format!("{ALICE_CAL}r2.ics"),
        &meeting("rl2", "20261101T100000Z", "2", &pair),
        &[],
    )
    .await;
    assert_eq!(reply.status, StatusCode::CREATED);
    let stored = unfold(
        &send(
            &fx.app,
            "alice",
            "GET",
            &format!("{ALICE_CAL}r2.ics"),
            &[],
            "",
        )
        .await
        .body,
    );
    assert!(
        stored.contains("SCHEDULE-STATUS=1.2:mailto:bob@example.org"),
        "{stored}"
    );
    assert!(
        stored.contains("SCHEDULE-STATUS=5.1:mailto:carol@example.org"),
        "{stored}"
    );
    assert!(copy(&fx, &fx.carol, "rl2").await.is_none());
}

#[tokio::test]
async fn full_recipient_is_refused_without_failing_the_organizer() {
    let fx = fixture().await;
    fx.store.set_quota_for_tests(3000);
    // Bob's calendar is nearly full.
    let filler = calendar(&format!(
        "BEGIN:VEVENT\r\nUID:filler\r\nDTSTART:20261101T100000Z\r\nDESCRIPTION:{}\r\nEND:VEVENT\r\n",
        "x".repeat(2600)
    ));
    let reply = put(
        &fx.app,
        "bob",
        "/dav/calendars/bob/default/filler.ics",
        &filler,
        &[],
    )
    .await;
    assert_eq!(reply.status, StatusCode::CREATED);
    let uri = format!("{ALICE_CAL}quota.ics");
    let reply = put(
        &fx.app,
        "alice",
        &uri,
        &meeting("qu1", "20261101T100000Z", "Q", BOB_LINE),
        &[],
    )
    .await;
    assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.body);
    let stored = unfold(&send(&fx.app, "alice", "GET", &uri, &[], "").await.body);
    assert!(
        stored.contains("SCHEDULE-STATUS=5.1:mailto:bob@example.org"),
        "{stored}"
    );
    assert!(copy(&fx, &fx.bob, "qu1").await.is_none());
}

#[tokio::test]
async fn subscriptions_are_never_targets() {
    let fx = fixture().await;
    // Bob's only calendar is a subscription.
    let calendar_of_bob = fx
        .store
        .collection(
            fx.bob.id,
            CollectionKind::Calendar,
            &"default".parse().unwrap(),
        )
        .await
        .unwrap()
        .unwrap();
    fx.store
        .delete_collection(calendar_of_bob.id)
        .await
        .unwrap();
    fx.store
        .create_subscription(
            &calcraft_store::NewSubscription {
                owner_id: fx.bob.id,
                slug: &"feed".parse().unwrap(),
                display_name: None,
                color: None,
                components: "VEVENT,VTODO",
                source_url: "https://feed.example/cal.ics",
                refresh_secs: None,
                max_per_user: 20,
            },
            &[],
            &calcraft_store::FeedState {
                feed_ttl_secs: None,
                http_etag: None,
                http_last_modified: None,
                content_hash: "x",
                skipped: 0,
                now: 0,
                next_refresh_at: 3600,
            },
        )
        .await
        .unwrap();
    let uri = format!("{ALICE_CAL}sub.ics");
    put(
        &fx.app,
        "alice",
        &uri,
        &meeting("sub1", "20261101T100000Z", "S", BOB_LINE),
        &[],
    )
    .await;
    let stored = unfold(&send(&fx.app, "alice", "GET", &uri, &[], "").await.body);
    assert!(
        stored.contains("SCHEDULE-STATUS=5.3:mailto:bob@example.org"),
        "{stored}"
    );
    let feed = fx
        .store
        .collection(
            fx.bob.id,
            CollectionKind::Calendar,
            &"feed".parse().unwrap(),
        )
        .await
        .unwrap()
        .unwrap();
    assert!(fx.store.items(feed.id).await.unwrap().is_empty());
}

#[tokio::test]
async fn inbox_messages_can_be_read_and_deleted() {
    let fx = fixture().await;
    put(
        &fx.app,
        "alice",
        &format!("{ALICE_CAL}i.ics"),
        &meeting("i1", "20261101T100000Z", "I", BOB_LINE),
        &[],
    )
    .await;
    let listing = send(
        &fx.app,
        "bob",
        "PROPFIND",
        "/dav/scheduling/bob/inbox/",
        &[("depth", "1")],
        "",
    )
    .await;
    assert_eq!(listing.status, StatusCode::MULTI_STATUS);
    let name = fx.store.inbox_messages(fx.bob.id, 10).await.unwrap()[0]
        .name
        .clone();
    let href = format!("/dav/scheduling/bob/inbox/{name}");
    assert!(listing.body.contains(&href));
    let get = send(&fx.app, "bob", "GET", &href, &[], "").await;
    assert_eq!(get.status, StatusCode::OK);
    assert!(get.body.contains("METHOD:REQUEST"));
    let multiget = format!(
        r#"<c:calendar-multiget xmlns:d="DAV:" xmlns:c="urn:ietf:params:xml:ns:caldav"><d:prop><d:getetag/><c:calendar-data/></d:prop><d:href>{href}</d:href><d:href>/dav/scheduling/bob/inbox/none.ics</d:href></c:calendar-multiget>"#
    );
    let report = send(
        &fx.app,
        "bob",
        "REPORT",
        "/dav/scheduling/bob/inbox/",
        &[("depth", "1")],
        &multiget,
    )
    .await;
    assert_eq!(report.status, StatusCode::MULTI_STATUS);
    assert!(report.body.contains("METHOD:REQUEST"));
    assert!(report.body.contains("404"));
    // Alice cannot see bob's inbox: like a path that does not exist.
    let theirs = send(&fx.app, "alice", "GET", &href, &[], "").await;
    let missing = send(
        &fx.app,
        "alice",
        "GET",
        "/dav/scheduling/carol-x/inbox/x.ics",
        &[],
        "",
    )
    .await;
    assert_eq!(theirs.status, StatusCode::NOT_FOUND);
    assert_eq!(theirs.body, missing.body);
    let reply = send(&fx.app, "bob", "DELETE", &href, &[], "").await;
    assert_eq!(reply.status, StatusCode::NO_CONTENT);
    assert!(
        fx.store
            .inbox_messages(fx.bob.id, 10)
            .await
            .unwrap()
            .is_empty()
    );
    // Deleting a message is final: nothing in the trash.
    assert!(
        fx.store
            .trash_entries(fx.bob.id, 10, 0)
            .await
            .unwrap()
            .entries
            .is_empty()
    );
}

fn freebusy_request(organizer: &str, attendees: &[&str], start: &str, end: &str) -> String {
    let attendees: String = attendees
        .iter()
        .map(|a| format!("ATTENDEE:{a}\r\n"))
        .collect();
    format!(
        "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//Test//EN\r\nMETHOD:REQUEST\r\nBEGIN:VFREEBUSY\r\nUID:fb-1\r\nDTSTAMP:20261001T000000Z\r\nDTSTART:{start}\r\nDTEND:{end}\r\nORGANIZER:{organizer}\r\n{attendees}END:VFREEBUSY\r\nEND:VCALENDAR\r\n"
    )
}

#[tokio::test]
async fn free_busy_lookup() {
    let fx = fixture().await;
    let bob_cal = "/dav/calendars/bob/default/";
    let event = |uid: &str, start: &str, extra: &str| {
        calendar(&format!(
            "BEGIN:VEVENT\r\nUID:{uid}\r\nDTSTART:{start}\r\nDURATION:PT1H\r\nSUMMARY:zqsecret-{uid}\r\n{extra}END:VEVENT\r\n"
        ))
    };
    put(
        &fx.app,
        "bob",
        &format!("{bob_cal}a.ics"),
        &event("a", "20261101T090000Z", ""),
        &[],
    )
    .await;
    put(
        &fx.app,
        "bob",
        &format!("{bob_cal}b.ics"),
        &event("b", "20261101T130000Z", "TRANSP:TRANSPARENT\r\n"),
        &[],
    )
    .await;
    put(
        &fx.app,
        "bob",
        &format!("{bob_cal}c.ics"),
        &event("c", "20261101T150000Z", "STATUS:TENTATIVE\r\n"),
        &[],
    )
    .await;
    put(
        &fx.app,
        "bob",
        &format!("{bob_cal}d.ics"),
        &event("d", "20261101T170000Z", "RRULE:FREQ=DAILY;COUNT=3\r\n"),
        &[],
    )
    .await;
    // A calendar marked transparent does not count.
    send(
        &fx.app,
        "bob",
        "MKCALENDAR",
        "/dav/calendars/bob/holidays/",
        &[],
        "",
    )
    .await;
    let transp = r#"<d:propertyupdate xmlns:d="DAV:" xmlns:c="urn:ietf:params:xml:ns:caldav"><d:set><d:prop><c:schedule-calendar-transp><c:transparent/></c:schedule-calendar-transp></d:prop></d:set></d:propertyupdate>"#;
    let reply = send(
        &fx.app,
        "bob",
        "PROPPATCH",
        "/dav/calendars/bob/holidays/",
        &[],
        transp,
    )
    .await;
    assert_eq!(reply.status, StatusCode::MULTI_STATUS);
    put(
        &fx.app,
        "bob",
        "/dav/calendars/bob/holidays/h.ics",
        &event("h", "20261101T110000Z", ""),
        &[],
    )
    .await;

    let request = freebusy_request(
        "mailto:alice@example.org",
        &[
            "mailto:bob@example.org",
            "mailto:nobody@example.org",
            "mailto:x@elsewhere.example",
        ],
        "20261101T000000Z",
        "20261103T000000Z",
    );
    let reply = send(
        &fx.app,
        "alice",
        "POST",
        "/dav/scheduling/alice/outbox/",
        &[("content-type", "text/calendar")],
        &request,
    )
    .await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
    let body = unfold(&reply.body);
    assert!(body.contains("schedule-response"));
    assert!(body.contains("2.0;Success"));
    assert!(body.contains("3.7;Invalid calendar user"));
    assert!(body.contains("5.3;No scheduling support for user"));
    assert!(body.contains("FBTYPE=BUSY:20261101T090000Z/20261101T100000Z,20261101T170000Z/20261101T180000Z,20261102T170000Z/20261102T180000Z"), "{body}");
    assert!(
        body.contains("FBTYPE=BUSY-TENTATIVE:20261101T150000Z/20261101T160000Z"),
        "{body}"
    );
    assert!(
        !body.contains("20261101T130000Z/"),
        "transparent event shown: {body}"
    );
    assert!(
        !body.contains("20261101T110000Z/"),
        "transparent calendar shown: {body}"
    );
    // Only busy periods: no summaries, UIDs or other event data.
    assert!(!body.contains("zqsecret"));

    // Only with one's own address as organizer, only one's own outbox.
    let spoofed = freebusy_request(
        "mailto:carol@example.org",
        &["mailto:bob@example.org"],
        "20261101T000000Z",
        "20261102T000000Z",
    );
    let reply = send(
        &fx.app,
        "alice",
        "POST",
        "/dav/scheduling/alice/outbox/",
        &[("content-type", "text/calendar")],
        &spoofed,
    )
    .await;
    assert_eq!(reply.status, StatusCode::FORBIDDEN);
    assert!(reply.body.contains("organizer-allowed"));
    let reply = send(
        &fx.app,
        "alice",
        "POST",
        "/dav/scheduling/carol/outbox/",
        &[("content-type", "text/calendar")],
        &request,
    )
    .await;
    assert_eq!(reply.status, StatusCode::NOT_FOUND);
    // At most `max_freebusy_days`.
    let long = freebusy_request(
        "mailto:alice@example.org",
        &["mailto:bob@example.org"],
        "20260101T000000Z",
        "20280101T000000Z",
    );
    let reply = send(
        &fx.app,
        "alice",
        "POST",
        "/dav/scheduling/alice/outbox/",
        &[("content-type", "text/calendar")],
        &long,
    )
    .await;
    assert_eq!(reply.status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn free_busy_can_be_switched_off() {
    let fx = fixture_with(SchedulingConfig {
        free_busy: false,
        ..enabled()
    })
    .await;
    let request = freebusy_request(
        "mailto:alice@example.org",
        &["mailto:bob@example.org"],
        "20261101T000000Z",
        "20261102T000000Z",
    );
    let reply = send(
        &fx.app,
        "alice",
        "POST",
        "/dav/scheduling/alice/outbox/",
        &[("content-type", "text/calendar")],
        &request,
    )
    .await;
    assert_eq!(reply.status, StatusCode::OK);
    assert!(reply.body.contains("3.8"));
    assert!(!reply.body.contains("FREEBUSY"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_replies_lose_nothing() {
    let fx = fixture().await;
    let uri = format!("{ALICE_CAL}conc.ics");
    put(
        &fx.app,
        "alice",
        &uri,
        &meeting(
            "cc1",
            "20261101T100000Z",
            "C",
            &format!("{BOB_LINE}{CAROL_LINE}"),
        ),
        &[],
    )
    .await;
    let mut tasks = Vec::new();
    for user in [&fx.bob, &fx.carol] {
        let href = copy_href(&fx, user, "cc1").await;
        let body = unfold(&copy(&fx, user, "cc1").await.unwrap()).replace(
            &format!(
                "PARTSTAT=NEEDS-ACTION;RSVP=TRUE:mailto:{}@example.org",
                user.username
            ),
            &format!(
                "PARTSTAT=ACCEPTED;RSVP=TRUE:mailto:{}@example.org",
                user.username
            ),
        );
        let app = fx.app.clone();
        let name = user.username.to_string();
        tasks.push(tokio::spawn(async move {
            put(&app, &name, &href, &body, &[]).await.status
        }));
    }
    for task in tasks {
        assert_eq!(task.await.unwrap(), StatusCode::NO_CONTENT);
    }
    let organizer = unfold(&send(&fx.app, "alice", "GET", &uri, &[], "").await.body);
    assert!(
        organizer
            .contains("PARTSTAT=ACCEPTED;RSVP=TRUE;SCHEDULE-STATUS=1.2:mailto:bob@example.org"),
        "{organizer}"
    );
    assert!(
        organizer
            .contains("PARTSTAT=ACCEPTED;RSVP=TRUE;SCHEDULE-STATUS=1.2:mailto:carol@example.org"),
        "{organizer}"
    );
}

#[tokio::test]
async fn emails_resolve_and_ambiguous_addresses_are_refused() {
    let fx = fixture().await;
    fx.store
        .set_user_email(fx.bob.id, Some("robert@corp.example"))
        .await
        .unwrap();
    // Carol's e-mail address is mallory's derived address (an operator
    // mistake the admin interfaces refuse): nobody gets it.
    fx.store
        .set_user_email(fx.carol.id, Some("mallory@example.org"))
        .await
        .unwrap();
    let uri = format!("{ALICE_CAL}e.ics");
    let attendees =
        "ATTENDEE:mailto:Robert@Corp.Example\r\nATTENDEE:mailto:mallory@example.org\r\n";
    put(
        &fx.app,
        "alice",
        &uri,
        &meeting("e1", "20261101T100000Z", "E", attendees),
        &[],
    )
    .await;
    assert!(copy(&fx, &fx.bob, "e1").await.is_some());
    assert!(copy(&fx, &fx.carol, "e1").await.is_none());
    assert!(copy(&fx, &fx.mallory, "e1").await.is_none());
    let stored = unfold(&send(&fx.app, "alice", "GET", &uri, &[], "").await.body);
    assert!(
        stored.contains("SCHEDULE-STATUS=3.7:mailto:mallory@example.org"),
        "{stored}"
    );
}

#[tokio::test]
async fn trash_restore_does_not_resend() {
    let fx = fixture().await;
    let uri = format!("{ALICE_CAL}t.ics");
    put(
        &fx.app,
        "alice",
        &uri,
        &meeting("t1", "20261101T100000Z", "T", BOB_LINE),
        &[],
    )
    .await;
    send(&fx.app, "alice", "DELETE", &uri, &[], "").await;
    let revision = fx.store.inbox_revision(fx.bob.id).await.unwrap();
    let entry = fx
        .store
        .trash_entries(fx.alice.id, 10, 0)
        .await
        .unwrap()
        .entries[0]
        .id
        .clone();
    fx.store
        .restore_trash_entry(fx.alice.id, &entry, None)
        .await
        .unwrap();
    assert_eq!(fx.store.inbox_revision(fx.bob.id).await.unwrap(), revision);
    assert!(
        copy(&fx, &fx.bob, "t1")
            .await
            .unwrap()
            .contains("STATUS:CANCELLED")
    );
}

#[tokio::test]
async fn imports_never_schedule_and_the_inbox_is_no_target() {
    let fx = fixture().await;
    let calendar_of_alice = fx
        .store
        .collection(
            fx.alice.id,
            CollectionKind::Calendar,
            &"default".parse().unwrap(),
        )
        .await
        .unwrap()
        .unwrap();
    let data = meeting("imp1", "20261101T100000Z", "Imported", BOB_LINE);
    let slot = calcraft_server::transfer::import_slot().await.unwrap();
    calcraft_server::transfer::import(
        &fx.store,
        &calendar_of_alice,
        calcraft_ical::Format::ICalendar,
        data.into_bytes().into(),
        calcraft_server::transfer::OnConflict::Skip,
        1024 * 1024,
        slot,
    )
    .await
    .unwrap();
    assert!(copy(&fx, &fx.bob, "imp1").await.is_none());
    assert!(inbox(&fx, &fx.bob).await.is_empty());
    // Deleting the imported object (never scheduled) sends nothing either.
    let name = fx.store.items(calendar_of_alice.id).await.unwrap()[0]
        .name
        .clone();
    let reply = send(
        &fx.app,
        "alice",
        "DELETE",
        &format!("{ALICE_CAL}{name}"),
        &[],
        "",
    )
    .await;
    assert_eq!(reply.status, StatusCode::NO_CONTENT);
    assert!(inbox(&fx, &fx.bob).await.is_empty());

    // Objects cannot be copied or moved into an inbox.
    let plain = calendar("BEGIN:VEVENT\r\nUID:p1\r\nDTSTART:20261101T100000Z\r\nEND:VEVENT\r\n");
    put(
        &fx.app,
        "alice",
        &format!("{ALICE_CAL}plain.ics"),
        &plain,
        &[],
    )
    .await;
    // Her own inbox is no object collection (403); another user's inbox
    // is like any foreign destination (409).
    for (method, destination, status) in [
        (
            "COPY",
            "/dav/scheduling/alice/inbox/x.ics",
            StatusCode::FORBIDDEN,
        ),
        (
            "MOVE",
            "/dav/scheduling/alice/inbox/x.ics",
            StatusCode::FORBIDDEN,
        ),
        (
            "MOVE",
            "/dav/scheduling/bob/inbox/x.ics",
            StatusCode::CONFLICT,
        ),
        ("COPY", "/dav/scheduling/bob/outbox/", StatusCode::CONFLICT),
    ] {
        let reply = send(
            &fx.app,
            "alice",
            method,
            &format!("{ALICE_CAL}plain.ics"),
            &[("destination", destination), ("host", "localhost")],
            "",
        )
        .await;
        assert_eq!(reply.status, status, "{method} {destination}");
    }
    assert!(
        fx.store
            .inbox_messages(fx.alice.id, 10)
            .await
            .unwrap()
            .is_empty()
    );
    assert!(
        fx.store
            .inbox_messages(fx.bob.id, 10)
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn removing_every_attendee_cancels() {
    let fx = fixture().await;
    let uri = format!("{ALICE_CAL}all.ics");
    put(
        &fx.app,
        "alice",
        &uri,
        &meeting("all1", "20261101T100000Z", "A", BOB_LINE),
        &[],
    )
    .await;
    let alone = calendar(
        "BEGIN:VEVENT\r\nUID:all1\r\nDTSTART:20261101T100000Z\r\nDURATION:PT1H\r\nSUMMARY:A\r\nEND:VEVENT\r\n",
    );
    let reply = put(&fx.app, "alice", &uri, &alone, &[]).await;
    assert_eq!(reply.status, StatusCode::NO_CONTENT);
    assert!(
        copy(&fx, &fx.bob, "all1")
            .await
            .unwrap()
            .contains("STATUS:CANCELLED")
    );
}
