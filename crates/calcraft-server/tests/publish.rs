//! Public read-only calendar links (`[publish]`, docs/sharing.md): off by
//! default, configuration, the REST API, headers, ETag/304, what is
//! published (busy only, private events, no attachments, alarms or
//! attendee addresses unless opted in), revocation, disabled owners,
//! uniform 404 for every wrong token and the per-client miss limit.

// The shares and links are set up through the REST API.
#![cfg(feature = "api")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::net::{IpAddr, Ipv4Addr, SocketAddr};

use axum::{
    Router,
    body::Body,
    extract::ConnectInfo,
    http::{Method, Request},
};
use calcraft_core::{CollectionKind, api::CSRF_HEADER};
use calcraft_server::{
    AppState, Config, auth::now, publish::MISSES_PER_CLIENT, router, secrets::hash_secret,
};
use calcraft_store::{NewItem, Precondition, Store, User};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;

const BASE: &str = "https://cal.example.org";
const SESSION: &str = "alice-session-token-0123456789abcdef";

#[derive(Debug, Clone, PartialEq, Eq)]
struct Reply {
    status: u16,
    headers: Vec<(String, String)>,
    body: String,
}

impl Reply {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
    }
}

struct Fx {
    app: Router,
    store: Store,
    alice: User,
}

fn enabled() -> Config {
    let mut config = Config::default();
    config.publish.enabled = true;
    config.publish.base_url = Some(BASE.to_owned());
    config.server.client_rate_burst = u32::MAX;
    config.server.client_rate_per_second = u32::MAX;
    config
}

async fn fixture(config: &Config) -> Fx {
    let store = Store::in_memory().await.unwrap();
    let alice = store
        .create_user(&"alice".parse().unwrap(), "unused", None)
        .await
        .unwrap();
    store
        .create_session(alice.id, &hash_secret(SESSION), now(), now() + 3600)
        .await
        .unwrap();
    let cal = store
        .collection(
            alice.id,
            CollectionKind::Calendar,
            &"default".parse().unwrap(),
        )
        .await
        .unwrap()
        .unwrap();
    store
        .update_collection(cal.id, Some(Some("Alice's calendar")), None, None)
        .await
        .unwrap();
    let app = router(config, AppState::new(store.clone()).unwrap());
    Fx { app, store, alice }
}

async fn send(
    app: &Router,
    method: &str,
    uri: &str,
    headers: &[(&str, &str)],
    body: &str,
    ip: u8,
) -> Reply {
    let mut builder = Request::builder()
        .method(Method::from_bytes(method.as_bytes()).unwrap())
        .uri(uri);
    for (name, value) in headers {
        builder = builder.header(*name, *value);
    }
    let mut request = builder.body(Body::from(body.to_owned())).unwrap();
    request.extensions_mut().insert(ConnectInfo(SocketAddr::new(
        IpAddr::V4(Ipv4Addr::new(192, 0, 2, ip)),
        40_000,
    )));
    let response = app.clone().oneshot(request).await.unwrap();
    let status = response.status().as_u16();
    let mut headers: Vec<(String, String)> = response
        .headers()
        .iter()
        .map(|(k, v)| {
            (
                k.to_string(),
                String::from_utf8_lossy(v.as_bytes()).into_owned(),
            )
        })
        .collect();
    headers.sort();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    Reply {
        status,
        headers,
        body: String::from_utf8_lossy(&bytes).into_owned(),
    }
}

impl Fx {
    async fn api(&self, method: &str, uri: &str, body: Option<Value>) -> Reply {
        let cookie = format!("calcraft_session={SESSION}");
        let mut headers = vec![("cookie", cookie.as_str()), (CSRF_HEADER, "1")];
        if body.is_some() {
            headers.push(("content-type", "application/json"));
        }
        let body = body.map(|b| b.to_string()).unwrap_or_default();
        send(&self.app, method, uri, &headers, &body, 1).await
    }

    /// Creates a link; returns its id and path.
    async fn link(&self, options: Value) -> (String, String) {
        let reply = self
            .api(
                "POST",
                "/api/v1/collections/calendar/default/publish",
                Some(options),
            )
            .await;
        assert_eq!(reply.status, 201, "{}", reply.body);
        let created: Value = serde_json::from_str(&reply.body).unwrap();
        let url = created["url"].as_str().unwrap();
        assert!(url.starts_with(&format!("{BASE}/public/")), "{url}");
        assert!(url.ends_with(".ics"));
        assert_eq!(
            created["webcal_url"].as_str().unwrap(),
            url.replace("https://", "webcal://")
        );
        // 256 bits.
        let token = url.rsplit('/').next().unwrap().trim_end_matches(".ics");
        assert_eq!(token.len(), 64);
        (
            created["link"]["id"].as_str().unwrap().to_owned(),
            url.strip_prefix(BASE).unwrap().to_owned(),
        )
    }

    async fn get(&self, path: &str, ip: u8) -> Reply {
        send(&self.app, "GET", path, &[], "", ip).await
    }

    async fn put(&self, name: &str, content: &str) {
        let cal = self
            .store
            .collection(
                self.alice.id,
                CollectionKind::Calendar,
                &"default".parse().unwrap(),
            )
            .await
            .unwrap()
            .unwrap();
        let object = calcraft_ical::CalendarObject::parse(content.as_bytes()).unwrap();
        let etag = format!("\"{name}\"");
        self.store
            .put_item(
                cal.id,
                name,
                &NewItem {
                    uid: &object.uid,
                    component: object.kind.as_str(),
                    range_start: None,
                    range_end: None,
                    recurring: false,
                    index_version: 0,
                    etag: &etag,
                    content: content.as_bytes(),
                    attachments: &[],
                },
                &Precondition::None,
            )
            .await
            .unwrap();
    }
}

fn calendar(body: &str) -> String {
    format!("BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//Test//EN\r\n{body}END:VCALENDAR\r\n")
}

fn vevent(uid: &str, summary: &str, extra: &str) -> String {
    format!(
        "BEGIN:VEVENT\r\nUID:{uid}\r\nDTSTAMP:20261001T000000Z\r\nDTSTART:20261021T100000Z\r\nDTEND:20261021T110000Z\r\nSUMMARY:{summary}\r\n{extra}END:VEVENT\r\n"
    )
}

/// Alice's calendar: a public meeting with attendees, an alarm and an
/// attachment, a private and a confidential event, a free (transparent)
/// and a cancelled one, a to-do, and an event with a time zone.
async fn seed(fx: &Fx) {
    let meeting = vevent(
        "meeting-uid@alice.example",
        "Public meeting",
        "DESCRIPTION:Agenda\r\nLOCATION:Room 1\r\nORGANIZER;CN=Alice:mailto:alice@mail.example\r\nATTENDEE;CN=Bob:mailto:bob@mail.example\r\nATTACH;FMTTYPE=text/plain:https://cal.example.org/dav/attachments/0123456789abcdef0123456789abcdef\r\nBEGIN:VALARM\r\nACTION:DISPLAY\r\nDESCRIPTION:Alarm text\r\nTRIGGER:-PT15M\r\nEND:VALARM\r\n",
    );
    fx.put("meeting.ics", &calendar(&meeting)).await;
    fx.put(
        "private.ics",
        &calendar(&vevent(
            "private-uid",
            "Doctor",
            "CLASS:PRIVATE\r\nLOCATION:Clinic\r\n",
        )),
    )
    .await;
    fx.put(
        "confidential.ics",
        &calendar(&vevent("conf-uid", "Interview", "CLASS:CONFIDENTIAL\r\n")),
    )
    .await;
    fx.put(
        "free.ics",
        &calendar(&vevent(
            "free-uid",
            "Reminder only",
            "TRANSP:TRANSPARENT\r\n",
        )),
    )
    .await;
    fx.put(
        "cancelled.ics",
        &calendar(&vevent(
            "cancelled-uid",
            "Called off",
            "STATUS:CANCELLED\r\n",
        )),
    )
    .await;
    fx.put(
        "todo.ics",
        &calendar("BEGIN:VTODO\r\nUID:todo-uid\r\nDTSTAMP:20261001T000000Z\r\nSUMMARY:Secret task\r\nEND:VTODO\r\n"),
    )
    .await;
    let tz = "BEGIN:VTIMEZONE\r\nTZID:Europe/Berlin\r\nBEGIN:STANDARD\r\nDTSTART:19701025T030000\r\nTZOFFSETFROM:+0200\r\nTZOFFSETTO:+0100\r\nRRULE:FREQ=YEARLY;BYMONTH=10;BYDAY=-1SU\r\nEND:STANDARD\r\nEND:VTIMEZONE\r\n";
    let local = "BEGIN:VEVENT\r\nUID:local-uid\r\nDTSTAMP:20261001T000000Z\r\nDTSTART;TZID=Europe/Berlin:20261101T090000\r\nDTEND;TZID=Europe/Berlin:20261101T100000\r\nSUMMARY:Local\r\nEND:VEVENT\r\n";
    fx.put("local.ics", &calendar(&format!("{tz}{local}")))
        .await;
}

#[tokio::test]
async fn off_by_default_and_validated() {
    let fx = fixture(&Config::default()).await;
    let token = "0".repeat(64);
    let reply = fx.get(&format!("/public/{token}.ics"), 1).await;
    let unknown = fx.get("/no-such-path", 1).await;
    assert_eq!(reply, unknown);
    assert_eq!(reply.status, 404);
    for (method, body) in [("GET", None), ("POST", Some(json!({})))] {
        let reply = fx
            .api(method, "/api/v1/collections/calendar/default/publish", body)
            .await;
        assert_eq!(reply.status, 404, "{method}");
    }
    let me: Value = serde_json::from_str(&fx.api("GET", "/api/v1/me", None).await.body).unwrap();
    assert_eq!(me["publish"], false);
    assert_eq!(me["sharing"], true);

    let mut config = Config::default();
    config.publish.enabled = true;
    assert!(config.validate().is_err(), "base_url is required");
    for url in [
        "cal.example.org",
        "https://cal.example.org/path",
        "ftp://x",
        "https://user@x",
    ] {
        config.publish.base_url = Some(url.to_owned());
        assert!(config.validate().is_err(), "{url}");
    }
    config.publish.base_url = Some(BASE.to_owned());
    assert!(config.validate().is_ok());
    config.publish.max_bytes = 10;
    assert!(config.validate().is_err());
    config.publish.max_bytes = 1 << 20;
    config.publish.max_objects = 0;
    assert!(config.validate().is_err());
}

#[tokio::test]
async fn full_link_hides_attachments_alarms_and_addresses() {
    let fx = fixture(&enabled()).await;
    seed(&fx).await;
    let (_, path) = fx.link(json!({"private_events": "busy"})).await;
    let reply = fx.get(&path, 1).await;
    assert_eq!(reply.status, 200);
    assert_eq!(
        reply.header("content-type"),
        Some("text/calendar; charset=utf-8")
    );
    assert_eq!(reply.header("x-content-type-options"), Some("nosniff"));
    assert_eq!(
        reply.header("content-security-policy"),
        Some("default-src 'none'; sandbox")
    );
    assert_eq!(reply.header("cache-control"), Some("private, max-age=300"));
    assert_eq!(
        reply.header("content-disposition"),
        Some("inline; filename=\"calendar.ics\"")
    );
    let body = reply.body.replace("\r\n ", "");
    assert!(body.starts_with("BEGIN:VCALENDAR\r\n"), "{body}");
    assert!(body.contains("X-WR-CALNAME:Alice's calendar"), "{body}");
    assert!(body.contains("SUMMARY:Public meeting"));
    assert!(body.contains("DESCRIPTION:Agenda") && body.contains("LOCATION:Room 1"));
    for hidden in [
        "VALARM",
        "Alarm text",
        "ATTACH",
        "/dav/attachments/",
        "ATTENDEE",
        "ORGANIZER",
        "mail.example",
        "meeting-uid",
        "private-uid",
        "Doctor",
        "Clinic",
        "Interview",
        "VTODO",
        "Secret task",
    ] {
        assert!(!body.contains(hidden), "{hidden} published:\n{body}");
    }
    // Private and confidential events are busy blocks.
    assert_eq!(body.matches("SUMMARY:Busy").count(), 2, "{body}");
    // The time zone of the local event comes along.
    assert!(body.contains("BEGIN:VTIMEZONE") && body.contains("TZID=Europe/Berlin"));
    // Free and cancelled events are kept outside busy blocks.
    assert!(body.contains("Reminder only") && body.contains("Called off"));
    // The result parses as iCalendar.
    assert!(calcraft_ical::parse(body.as_bytes()).is_ok());

    // ETag and 304, HEAD.
    let etag = reply.header("etag").unwrap().to_owned();
    let again = send(&fx.app, "GET", &path, &[("if-none-match", &etag)], "", 1).await;
    assert_eq!(again.status, 304);
    assert!(again.body.is_empty());
    let head = send(&fx.app, "HEAD", &path, &[], "", 1).await;
    assert_eq!(head.status, 200);
    assert!(head.body.is_empty());
    assert_eq!(head.header("etag"), Some(etag.as_str()));
    // A change gives a new ETag.
    fx.put("new.ics", &calendar(&vevent("new-uid", "New", "")))
        .await;
    let changed = send(&fx.app, "GET", &path, &[("if-none-match", &etag)], "", 1).await;
    assert_eq!(changed.status, 200);
    assert_ne!(changed.header("etag"), Some(etag.as_str()));
}

#[tokio::test]
async fn link_options() {
    let fx = fixture(&enabled()).await;
    seed(&fx).await;
    // Busy only: times and "Busy", nothing else; free and cancelled
    // events are no busy blocks.
    let (_, busy) = fx
        .link(json!({"busy_only": true, "private_events": "show"}))
        .await;
    let body = fx.get(&busy, 1).await.body.replace("\r\n ", "");
    for hidden in [
        "Public meeting",
        "Agenda",
        "Room 1",
        "Doctor",
        "Reminder only",
        "Called off",
        "DESCRIPTION",
        "LOCATION",
    ] {
        assert!(!body.contains(hidden), "{hidden}:\n{body}");
    }
    assert_eq!(body.matches("SUMMARY:Busy").count(), 4, "{body}");
    assert!(body.contains("DTSTART:20261021T100000Z"));
    // Private events left out.
    let (_, hide) = fx.link(json!({"private_events": "hide"})).await;
    let body = fx.get(&hide, 1).await.body;
    assert!(
        !body.contains("SUMMARY:Busy") && !body.contains("Doctor"),
        "{body}"
    );
    assert!(body.contains("Public meeting"));
    // Private events shown, attendees included on request.
    let (_, all) = fx
        .link(json!({"private_events": "show", "include_attendees": true}))
        .await;
    let body = fx.get(&all, 1).await.body.replace("\r\n ", "");
    assert!(body.contains("Doctor") && body.contains("Interview"));
    assert!(
        body.contains("ATTENDEE;CN=Bob:mailto:bob@mail.example"),
        "{body}"
    );
    assert!(body.contains("ORGANIZER"));
    // Still no attachments or alarms.
    assert!(!body.contains("ATTACH") && !body.contains("VALARM"));
    // UIDs differ per link.
    let uid = |body: &str| {
        body.lines()
            .find(|l| l.starts_with("UID:"))
            .unwrap()
            .to_owned()
    };
    let hide_body = fx.get(&hide, 1).await.body;
    assert_ne!(uid(&body), uid(&hide_body));
    // Listing shows the options, never the token.
    let list = fx
        .api("GET", "/api/v1/collections/calendar/default/publish", None)
        .await;
    let links: Value = serde_json::from_str(&list.body).unwrap();
    assert_eq!(links.as_array().unwrap().len(), 3);
    assert!(
        !list
            .body
            .contains(busy.trim_start_matches("/public/").trim_end_matches(".ics"))
    );
    assert_eq!(links[0]["busy_only"], true);
    // Address books cannot be published.
    let reply = fx
        .api(
            "POST",
            "/api/v1/collections/addressbook/default/publish",
            Some(json!({})),
        )
        .await;
    assert_eq!(reply.status, 404);
    let reply = fx
        .api(
            "POST",
            "/api/v1/collections/calendar/default/publish",
            Some(json!({"private_events": "everything"})),
        )
        .await;
    assert_eq!(reply.status, 400);
}

#[tokio::test]
async fn wrong_revoked_and_disabled_links_are_one_404() {
    let fx = fixture(&enabled()).await;
    seed(&fx).await;
    let (id, path) = fx.link(json!({})).await;
    assert_eq!(fx.get(&path, 1).await.status, 200);
    let token = path
        .trim_start_matches("/public/")
        .trim_end_matches(".ics")
        .to_owned();
    let unknown = fx.get(&format!("/public/{}.ics", "a".repeat(64)), 2).await;
    assert_eq!(unknown.status, 404);
    let nothing = fx.get("/no-such-path", 2).await;
    assert_eq!(unknown, nothing);
    // Other methods on a valid link: like an unknown path.
    for method in ["POST", "PUT", "DELETE", "PROPFIND", "OPTIONS"] {
        let reply = send(&fx.app, method, &path, &[], "", 2).await;
        assert_eq!(reply, nothing, "{method}");
    }
    let mut flipped = token.clone().into_bytes();
    flipped[63] = if flipped[63] == b'0' { b'1' } else { b'0' };
    for wrong in [
        format!("/public/{}.ics", String::from_utf8(flipped).unwrap()),
        format!("/public/{}.ics", token.to_uppercase()),
        format!("/public/{token}"),
        format!("/public/{token}.ics.html"),
        format!("/public/{}.ics", &token[1..]),
        format!("/public/{token}0.ics"),
        format!("/public/%2e%2e%2f{token}.ics"),
        format!("/public/{token}.ICS"),
        format!("/public/{}.ics", hash_secret(&token)),
    ] {
        let reply = fx.get(&wrong, 3).await;
        assert_eq!(reply, unknown, "{wrong}");
    }
    // The owner disabled: the link answers like an unknown one.
    fx.store
        .set_disabled(&fx.alice.username, true)
        .await
        .unwrap();
    assert_eq!(fx.get(&path, 4).await, unknown);
    fx.store
        .set_disabled(&fx.alice.username, false)
        .await
        .unwrap();
    assert_eq!(fx.get(&path, 4).await.status, 200);
    // Revoked: at once, like an unknown one.
    let reply = fx
        .api(
            "DELETE",
            &format!("/api/v1/collections/calendar/default/publish/{id}"),
            None,
        )
        .await;
    assert_eq!(reply.status, 204);
    assert_eq!(fx.get(&path, 5).await, unknown);
    let reply = fx
        .api(
            "DELETE",
            &format!("/api/v1/collections/calendar/default/publish/{id}"),
            None,
        )
        .await;
    assert_eq!(reply.status, 404);
    // A deleted calendar takes its links along.
    let (_, path) = fx.link(json!({})).await;
    let reply = fx
        .api("DELETE", "/api/v1/collections/calendar/default", None)
        .await;
    assert_eq!(reply.status, 204);
    assert_eq!(fx.get(&path, 6).await, unknown);
}

#[tokio::test]
async fn guessing_clients_are_blocked() {
    let fx = fixture(&enabled()).await;
    let (_, path) = fx.link(json!({})).await;
    for n in 0..MISSES_PER_CLIENT {
        let reply = fx.get(&format!("/public/{n:064x}.ics"), 7).await;
        assert_eq!(reply.status, 404);
    }
    // Now every publish request of that client is refused, even a valid one.
    let reply = fx.get(&format!("/public/{:064x}.ics", 999), 7).await;
    assert_eq!(reply.status, 429);
    assert!(reply.header("retry-after").is_some());
    assert_eq!(fx.get(&path, 7).await.status, 429);
    // Other clients are not affected.
    assert_eq!(fx.get(&path, 8).await.status, 200);
}
