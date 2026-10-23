//! CalDAV managed attachments (RFC 8607): uploads, updates, removals,
//! recurrence instances, limits and quota, downloads and their headers,
//! access by other users and attendees, garbage collection, MOVE/COPY,
//! backups, concurrency, and the attacks the design has to stop (foreign
//! managed ids in an own event, tampered ATTACH properties, hostile file
//! names and media types).

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::atomic::{AtomicU32, Ordering};

use axum::{
    Router,
    body::Body,
    http::{HeaderMap, Method, Request, StatusCode, header},
};
use base64::{Engine, engine::general_purpose::STANDARD};
use calcraft_server::{
    AppState, Config,
    auth::hash_password,
    config::{AttachmentsConfig, SchedulingConfig},
    router,
};
use calcraft_store::{Store, User};
use http_body_util::BodyExt;
use tower::ServiceExt;

const PASSWORD: &str = "a long password 123";
const BASE: &str = "https://cal.example.org";
const CAL: &str = "/dav/calendars/alice/default/";
const EVENT: &str = "/dav/calendars/alice/default/e.ics";

struct Fx {
    app: Router,
    store: Store,
    alice: User,
    bob: User,
}

fn attachments_config() -> AttachmentsConfig {
    AttachmentsConfig {
        enabled: true,
        base_url: Some(BASE.to_owned()),
        max_bytes: 64 * 1024,
        max_per_object: 3,
        ..AttachmentsConfig::default()
    }
}

fn config(attachments: AttachmentsConfig, scheduling: bool) -> Config {
    let mut config = Config {
        attachments,
        ..Config::default()
    };
    if scheduling {
        config.scheduling = SchedulingConfig {
            enabled: true,
            domain: Some("example.org".to_owned()),
            ..SchedulingConfig::default()
        };
    }
    config.server.max_concurrent_requests_per_client = u32::MAX;
    config.server.client_rate_burst = u32::MAX;
    config.server.client_rate_per_second = u32::MAX;
    config
}

async fn users(store: &Store) -> (User, User) {
    let hash = hash_password(PASSWORD).unwrap();
    let mut users = Vec::new();
    for name in ["alice", "bob", "carol"] {
        users.push(
            store
                .create_user(&name.parse().unwrap(), &hash, None)
                .await
                .unwrap(),
        );
    }
    (users.remove(0), users.remove(0))
}

async fn fixture_with(config: Config) -> Fx {
    let store = Store::in_memory().await.unwrap();
    let (alice, bob) = users(&store).await;
    let app = router(&config, AppState::new(store.clone()).unwrap());
    Fx {
        app,
        store,
        alice,
        bob,
    }
}

async fn fixture() -> Fx {
    fixture_with(config(attachments_config(), false)).await
}

fn basic(user: &str) -> String {
    format!("Basic {}", STANDARD.encode(format!("{user}:{PASSWORD}")))
}

#[derive(Debug)]
struct Reply {
    status: StatusCode,
    headers: HeaderMap,
    body: Vec<u8>,
}

impl Reply {
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }

    fn header(&self, name: &str) -> Option<&str> {
        self.headers.get(name).map(|v| v.to_str().unwrap())
    }
}

async fn send(
    app: &Router,
    user: &str,
    method: &str,
    uri: &str,
    headers: &[(&str, &str)],
    body: impl Into<Vec<u8>>,
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
        .oneshot(builder.body(Body::from(body.into())).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let headers = response.headers().clone();
    let body = response.into_body().collect().await.unwrap().to_bytes();
    Reply {
        status,
        headers,
        body: body.to_vec(),
    }
}

fn calendar(body: &str) -> String {
    format!("BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//Test//EN\r\n{body}END:VCALENDAR\r\n")
}

fn event(uid: &str, extra: &str) -> String {
    calendar(&format!(
        "BEGIN:VEVENT\r\nUID:{uid}\r\nDTSTAMP:20261001T000000Z\r\nDTSTART:20261105T100000Z\r\nDURATION:PT1H\r\nSUMMARY:Meeting\r\n{extra}END:VEVENT\r\n"
    ))
}

async fn put(app: &Router, user: &str, uri: &str, body: &str) -> Reply {
    send(
        app,
        user,
        "PUT",
        uri,
        &[("content-type", "text/calendar")],
        body,
    )
    .await
}

async fn add(
    app: &Router,
    user: &str,
    uri: &str,
    query: &str,
    filename: &str,
    body: &[u8],
) -> Reply {
    let disposition = format!("attachment; filename=\"{filename}\"");
    send(
        app,
        user,
        "POST",
        &format!("{uri}?action=attachment-add{query}"),
        &[
            ("content-type", "application/pdf"),
            ("content-disposition", &disposition),
        ],
        body.to_vec(),
    )
    .await
}

async fn get_text(app: &Router, user: &str, uri: &str) -> String {
    let reply = send(app, user, "GET", uri, &[], Vec::new()).await;
    assert_eq!(reply.status, StatusCode::OK, "{uri}");
    reply.text()
}

fn unfold(text: &str) -> String {
    text.replace("\r\n ", "")
}

fn attachment_path(id: &str) -> String {
    format!("/dav/attachments/{id}")
}

/// Adds a PDF to alice's event and returns the managed id.
async fn added(fx: &Fx, uri: &str, content: &[u8]) -> String {
    let reply = add(&fx.app, "alice", uri, "", "report.pdf", content).await;
    assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.text());
    reply.header("cal-managed-id").unwrap().to_owned()
}

#[tokio::test]
async fn disabled_by_default() {
    let fx = fixture_with(config(AttachmentsConfig::default(), false)).await;
    let options = send(&fx.app, "alice", "OPTIONS", "/dav/", &[], Vec::new()).await;
    assert!(
        !options
            .header("dav")
            .unwrap()
            .contains("managed-attachments")
    );
    assert_eq!(
        put(&fx.app, "alice", EVENT, &event("e", "")).await.status,
        StatusCode::CREATED
    );
    let reply = add(&fx.app, "alice", EVENT, "", "a.pdf", b"x").await;
    assert_eq!(reply.status, StatusCode::METHOD_NOT_ALLOWED);
    let reply = send(
        &fx.app,
        "alice",
        "GET",
        &attachment_path(&"a".repeat(32)),
        &[],
        Vec::new(),
    )
    .await;
    assert_eq!(reply.status, StatusCode::NOT_FOUND);
    let props = send(
        &fx.app,
        "alice",
        "PROPFIND",
        CAL,
        &[("depth", "0")],
        Vec::new(),
    )
    .await;
    assert!(!props.text().contains("max-attachment-size"));
}

#[tokio::test]
async fn configuration_is_validated() {
    let mut config = Config::default();
    config.attachments.enabled = true;
    assert!(config.validate().is_err(), "base_url is required");
    for (url, ok) in [
        ("https://cal.example.org", true),
        ("https://cal.example.org/", true),
        ("http://127.0.0.1:5232", true),
        ("https://[::1]:8443", true),
        ("https://cal.example.org/dav", false),
        ("https://user@cal.example.org", false),
        ("ftp://cal.example.org", false),
        ("https://cal.example.org?x", false),
        ("https://Cal.Example.org", false),
        ("https://cal.example.org:0", false),
        ("https://cal.example.org:99999", false),
        ("https://", false),
    ] {
        config.attachments.base_url = Some(url.to_owned());
        assert_eq!(config.validate().is_ok(), ok, "{url}");
    }
    config.attachments.base_url = Some(BASE.to_owned());
    config.attachments.max_bytes = 10;
    assert!(config.validate().is_err());
    config.attachments.max_bytes = 1024 * 1024;
    config.attachments.max_per_object = 0;
    assert!(config.validate().is_err());
    config.attachments.max_per_object = 5;
    config.attachments.max_bytes_per_user = 1024;
    assert!(config.validate().is_err());
    config.attachments.max_bytes_per_user = 1 << 30;
    assert!(config.validate().is_ok());
}

#[tokio::test]
async fn discovery_properties() {
    let fx = fixture().await;
    let options = send(
        &fx.app,
        "alice",
        "OPTIONS",
        "/dav/calendars/alice/",
        &[],
        Vec::new(),
    )
    .await;
    assert!(
        options
            .header("dav")
            .unwrap()
            .contains("calendar-managed-attachments")
    );
    assert!(!options.header("dav").unwrap().contains("no-recurrence"));
    let home = send(
        &fx.app,
        "alice",
        "PROPFIND",
        "/dav/calendars/alice/",
        &[("depth", "1")],
        Vec::new(),
    )
    .await;
    let text = home.text();
    assert!(text.contains("managed-attachments-server-URL"), "{text}");
    assert!(text.contains(">65536</"), "{text}");
    assert!(text.contains("max-attachments-per-resource"), "{text}");
    // Address books have none of them.
    let book = send(
        &fx.app,
        "alice",
        "PROPFIND",
        "/dav/addressbooks/alice/default/",
        &[("depth", "0")],
        Vec::new(),
    )
    .await;
    assert!(!book.text().contains("attachment"), "{}", book.text());
}

#[tokio::test]
async fn add_download_update_and_remove() {
    let fx = fixture().await;
    assert_eq!(
        put(&fx.app, "alice", EVENT, &event("e", "")).await.status,
        StatusCode::CREATED
    );
    let pdf = b"%PDF-1.4 not really".to_vec();
    let reply = add(&fx.app, "alice", EVENT, "", "report.pdf", &pdf).await;
    assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.text());
    let id = reply.header("cal-managed-id").unwrap().to_owned();
    assert_eq!(id.len(), 32);
    let etag = reply.header("etag").unwrap().to_owned();

    let object = send(&fx.app, "alice", "GET", EVENT, &[], Vec::new()).await;
    assert_eq!(object.header("etag").unwrap(), etag);
    let text = unfold(&object.text());
    assert!(
        text.contains(&format!(
            "ATTACH;FMTTYPE=application/pdf;SIZE={};FILENAME=report.pdf;MANAGED-ID={id}:{BASE}/dav/attachments/{id}",
            pdf.len()
        )),
        "{text}"
    );

    let download = send(
        &fx.app,
        "alice",
        "GET",
        &attachment_path(&id),
        &[],
        Vec::new(),
    )
    .await;
    assert_eq!(download.status, StatusCode::OK);
    assert_eq!(download.body, pdf);
    assert_eq!(download.header("content-type"), Some("application/pdf"));
    assert_eq!(download.header("x-content-type-options"), Some("nosniff"));
    assert_eq!(
        download.header("content-security-policy"),
        Some("default-src 'none'; sandbox")
    );
    assert!(
        download
            .header("content-disposition")
            .unwrap()
            .starts_with("attachment; filename=\"report.pdf\"")
    );
    let tag = download.header("etag").unwrap().to_owned();
    let head = send(
        &fx.app,
        "alice",
        "HEAD",
        &attachment_path(&id),
        &[],
        Vec::new(),
    )
    .await;
    assert_eq!(head.status, StatusCode::OK);
    assert!(head.body.is_empty());
    assert_eq!(
        head.header("content-length"),
        Some(pdf.len().to_string().as_str())
    );
    let cached = send(
        &fx.app,
        "alice",
        "GET",
        &attachment_path(&id),
        &[("if-none-match", &tag)],
        Vec::new(),
    )
    .await;
    assert_eq!(cached.status, StatusCode::NOT_MODIFIED);
    assert!(cached.body.is_empty());
    // Attachments cannot be changed through their URL.
    for method in [
        "PUT",
        "DELETE",
        "PROPFIND",
        "PROPPATCH",
        "POST",
        "MOVE",
        "COPY",
        "MKCOL",
    ] {
        let reply = send(&fx.app, "alice", method, &attachment_path(&id), &[], "x").await;
        assert_eq!(reply.status, StatusCode::METHOD_NOT_ALLOWED, "{method}");
    }

    // Update: new content, new managed id, the old one is gone.
    let update = send(
        &fx.app,
        "alice",
        "POST",
        &format!("{EVENT}?action=attachment-update&managed-id={id}"),
        &[
            ("content-type", "text/plain"),
            ("content-disposition", "attachment; filename=notes.txt"),
        ],
        "second version",
    )
    .await;
    assert_eq!(update.status, StatusCode::OK, "{}", update.text());
    let new_id = update.header("cal-managed-id").unwrap().to_owned();
    assert_ne!(new_id, id);
    let gone = send(
        &fx.app,
        "alice",
        "GET",
        &attachment_path(&id),
        &[],
        Vec::new(),
    )
    .await;
    assert_eq!(gone.status, StatusCode::NOT_FOUND);
    let text = unfold(&get_text(&fx.app, "alice", EVENT).await);
    assert!(text.contains(&format!("MANAGED-ID={new_id}")), "{text}");
    assert!(text.contains("FILENAME=notes.txt"), "{text}");
    assert!(!text.contains(&id), "{text}");
    assert_eq!(
        send(
            &fx.app,
            "alice",
            "GET",
            &attachment_path(&new_id),
            &[],
            Vec::new()
        )
        .await
        .body,
        b"second version"
    );
    // Updating or removing an id the object does not have.
    for action in ["update", "remove"] {
        let reply = send(
            &fx.app,
            "alice",
            "POST",
            &format!("{EVENT}?action=attachment-{action}&managed-id={id}"),
            &[],
            "x",
        )
        .await;
        assert_eq!(reply.status, StatusCode::FORBIDDEN, "{action}");
        assert!(
            reply.text().contains("valid-managed-id"),
            "{}",
            reply.text()
        );
    }

    let remove = send(
        &fx.app,
        "alice",
        "POST",
        &format!("{EVENT}?action=attachment-remove&managed-id={new_id}"),
        &[],
        Vec::new(),
    )
    .await;
    assert_eq!(remove.status, StatusCode::NO_CONTENT, "{}", remove.text());
    assert!(!get_text(&fx.app, "alice", EVENT).await.contains("ATTACH"));
    let gone = send(
        &fx.app,
        "alice",
        "GET",
        &attachment_path(&new_id),
        &[],
        Vec::new(),
    )
    .await;
    assert_eq!(gone.status, StatusCode::NOT_FOUND);
    assert_eq!(
        fx.store.attachment_usage(fx.alice.id).await.unwrap(),
        (0, 0)
    );

    // A missing object, an address book object, a malformed request.
    let reply = add(
        &fx.app,
        "alice",
        &format!("{CAL}missing.ics"),
        "",
        "a",
        b"x",
    )
    .await;
    assert_eq!(reply.status, StatusCode::NOT_FOUND);
    let reply = add(&fx.app, "alice", EVENT, "&managed-id=x", "a", b"x").await;
    assert_eq!(reply.status, StatusCode::BAD_REQUEST);
    let reply = send(
        &fx.app,
        "alice",
        "POST",
        &format!("{EVENT}?action=attachment-rename"),
        &[],
        "x",
    )
    .await;
    assert_eq!(reply.status, StatusCode::FORBIDDEN);
    assert!(reply.text().contains("valid-action"));
    let reply = add(&fx.app, "alice", EVENT, "", "a", b"").await;
    assert_eq!(reply.status, StatusCode::BAD_REQUEST);
    // If-Match on the object.
    let reply = send(
        &fx.app,
        "alice",
        "POST",
        &format!("{EVENT}?action=attachment-add"),
        &[("if-match", "\"stale\"")],
        "x",
    )
    .await;
    assert_eq!(reply.status, StatusCode::PRECONDITION_FAILED);
    assert_eq!(
        fx.store.attachment_usage(fx.alice.id).await.unwrap(),
        (0, 0)
    );
}

#[tokio::test]
async fn hostile_file_names_and_media_types() {
    let fx = fixture().await;
    put(&fx.app, "alice", EVENT, &event("e", "")).await;
    let cases: &[(&str, &str, &str)] = &[
        (
            "attachment; filename*=UTF-8''..%2F..%2Fetc%2Fpasswd%0D%0ASet-Cookie%3A%20x%3D1",
            "text/html",
            "passwdSet-Cookie_ x=1",
        ),
        (
            "attachment; filename=\"invoice\u{202E}fdp.exe\"",
            "image/svg+xml",
            "invoicefdp.exe",
        ),
        (
            "attachment; filename=\"..\"",
            "application/xhtml+xml",
            "attachment",
        ),
        (
            "broken; filename=\"unterminated",
            "text/javascript",
            "attachment",
        ),
        (
            "attachment; filename*=UTF-8''%E6%97%A5%E6%9C%AC.txt",
            "text/plain; charset=utf-8",
            "日本.txt",
        ),
    ];
    for (disposition, media_type, expected) in cases {
        let reply = send(
            &fx.app,
            "alice",
            "POST",
            &format!("{EVENT}?action=attachment-add"),
            &[
                ("content-type", media_type),
                ("content-disposition", disposition),
            ],
            "<script>alert(1)</script>",
        )
        .await;
        assert_eq!(
            reply.status,
            StatusCode::CREATED,
            "{disposition}: {}",
            reply.text()
        );
        let id = reply.header("cal-managed-id").unwrap().to_owned();
        let download = send(
            &fx.app,
            "alice",
            "GET",
            &attachment_path(&id),
            &[],
            Vec::new(),
        )
        .await;
        assert_eq!(download.status, StatusCode::OK);
        assert!(download.headers.get("set-cookie").is_none());
        let content_type = download.header("content-type").unwrap();
        if media_type.starts_with("text/plain") {
            assert_eq!(content_type, "text/plain");
        } else {
            assert_eq!(content_type, "application/octet-stream", "{media_type}");
        }
        let disposition_out = download.header("content-disposition").unwrap();
        assert!(
            disposition_out.starts_with("attachment; "),
            "{disposition_out}"
        );
        assert!(
            disposition_out
                .bytes()
                .all(|b| b.is_ascii_graphic() || b == b' '),
            "{disposition_out}"
        );
        let parsed = calcraft_ical::attach::disposition_filename(disposition_out).unwrap();
        assert_eq!(parsed, *expected, "{disposition}");
        assert_eq!(
            download.header("content-security-policy"),
            Some("default-src 'none'; sandbox")
        );
        let object = unfold(&get_text(&fx.app, "alice", EVENT).await);
        assert!(!object.contains("Set-Cookie:"), "{object}");
        // Remove again: at most three per object in this configuration.
        let remove = send(
            &fx.app,
            "alice",
            "POST",
            &format!("{EVENT}?action=attachment-remove&managed-id={id}"),
            &[],
            Vec::new(),
        )
        .await;
        assert_eq!(remove.status, StatusCode::NO_CONTENT);
    }
}

const SERIES: &str = "BEGIN:VEVENT\r\nUID:s\r\nDTSTAMP:20261001T000000Z\r\nDTSTART;TZID=Europe/Berlin:20261105T100000\r\nDURATION:PT1H\r\nRRULE:FREQ=DAILY;COUNT=5\r\nSUMMARY:Series\r\nEND:VEVENT\r\n";

#[tokio::test]
async fn recurrence_instances() {
    let fx = fixture().await;
    let uri = format!("{CAL}s.ics");
    let berlin = "BEGIN:VTIMEZONE\r\nTZID:Europe/Berlin\r\nBEGIN:STANDARD\r\nDTSTART:19701025T030000\r\nRRULE:FREQ=YEARLY;BYMONTH=10;BYDAY=-1SU\r\nTZOFFSETFROM:+0200\r\nTZOFFSETTO:+0100\r\nEND:STANDARD\r\nBEGIN:DAYLIGHT\r\nDTSTART:19700329T020000\r\nRRULE:FREQ=YEARLY;BYMONTH=3;BYDAY=-1SU\r\nTZOFFSETFROM:+0100\r\nTZOFFSETTO:+0200\r\nEND:DAYLIGHT\r\nEND:VTIMEZONE\r\n";
    let body = calendar(&format!("{berlin}{SERIES}"));
    assert_eq!(
        put(&fx.app, "alice", &uri, &body).await.status,
        StatusCode::CREATED
    );
    let reply = add(
        &fx.app,
        "alice",
        &uri,
        "&rid=20261107T100000",
        "day3.pdf",
        b"3",
    )
    .await;
    assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.text());
    let text = unfold(&get_text(&fx.app, "alice", &uri).await);
    assert!(
        text.contains("RECURRENCE-ID;TZID=Europe/Berlin:20261107T100000"),
        "{text}"
    );
    assert_eq!(text.matches("FILENAME=day3.pdf").count(), 1, "{text}");
    // Master only, then everything (master and the new override).
    let reply = add(&fx.app, "alice", &uri, "&rid=M", "master.pdf", b"m").await;
    assert_eq!(reply.status, StatusCode::CREATED);
    let reply = add(&fx.app, "alice", &uri, "", "all.pdf", b"a").await;
    assert_eq!(reply.status, StatusCode::CREATED);
    let text = unfold(&get_text(&fx.app, "alice", &uri).await);
    assert_eq!(text.matches("FILENAME=master.pdf").count(), 1, "{text}");
    assert_eq!(text.matches("FILENAME=all.pdf").count(), 2, "{text}");
    // Instances that do not exist, and limits.
    for rid in ["20261120T100000", "20261105T110000", "garbage", "M,M"] {
        let reply = add(&fx.app, "alice", &uri, &format!("&rid={rid}"), "x", b"x").await;
        assert_eq!(reply.status, StatusCode::FORBIDDEN, "{rid}");
        assert!(
            reply.text().contains("valid-rid"),
            "{rid}: {}",
            reply.text()
        );
    }
    let reply = add(&fx.app, "alice", &uri, "", "four.pdf", b"4").await;
    assert_eq!(reply.status, StatusCode::FORBIDDEN);
    assert!(reply.text().contains("max-attachments-per-resource"));
    // Failed requests leave no uploads behind.
    assert_eq!(fx.store.attachment_usage(fx.alice.id).await.unwrap().0, 3);
}

#[tokio::test]
async fn size_limits_and_quota() {
    let mut config = config(attachments_config(), false);
    // Uploads may be larger than other request bodies.
    config.server.max_body_bytes = 16 * 1024;
    let fx = fixture_with(config).await;
    put(&fx.app, "alice", EVENT, &event("e", "")).await;
    let big = vec![b'a'; 40 * 1024];
    let reply = add(&fx.app, "alice", EVENT, "", "big.bin", &big).await;
    assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.text());
    let too_big = vec![b'a'; 64 * 1024 + 1];
    let reply = add(&fx.app, "alice", EVENT, "", "huge.bin", &too_big).await;
    assert_eq!(reply.status, StatusCode::FORBIDDEN);
    assert!(reply.text().contains("max-attachment-size"));
    let length = too_big.len().to_string();
    let reply = send(
        &fx.app,
        "alice",
        "POST",
        &format!("{EVENT}?action=attachment-add"),
        &[("content-length", &length)],
        too_big.clone(),
    )
    .await;
    assert_eq!(reply.status, StatusCode::FORBIDDEN);
    assert!(reply.text().contains("max-attachment-size"));
    // Everything else keeps the general limit.
    let padding = format!("DESCRIPTION:{}\r\n", "x".repeat(20 * 1024));
    let body = event("p", &padding);
    let length = body.len().to_string();
    let reply = send(
        &fx.app,
        "alice",
        "PUT",
        &format!("{CAL}p.ics"),
        &[
            ("content-type", "text/calendar"),
            ("content-length", &length),
        ],
        body,
    )
    .await;
    assert_eq!(reply.status, StatusCode::PAYLOAD_TOO_LARGE);
    let reply = send(
        &fx.app,
        "alice",
        "POST",
        &format!(
            "{EVENT}?action=attachment-remove&managed-id={}",
            "a".repeat(32)
        ),
        &[("content-length", &big.len().to_string())],
        big.clone(),
    )
    .await;
    assert_eq!(reply.status, StatusCode::PAYLOAD_TOO_LARGE);

    // The quota counts attachments, also while their object is in the trash.
    let (_, bytes) = fx.store.attachment_usage(fx.alice.id).await.unwrap();
    assert_eq!(bytes, 40 * 1024);
    fx.store.set_quota_for_tests(bytes + 8 * 1024);
    let reply = add(
        &fx.app,
        "alice",
        EVENT,
        "",
        "more.bin",
        &vec![b'b'; 10 * 1024],
    )
    .await;
    assert_eq!(reply.status, StatusCode::INSUFFICIENT_STORAGE);
    assert!(reply.text().contains("quota-not-exceeded"));
    let reply = send(&fx.app, "alice", "DELETE", EVENT, &[], Vec::new()).await;
    assert_eq!(reply.status, StatusCode::NO_CONTENT);
    assert_eq!(
        fx.store.attachment_usage(fx.alice.id).await.unwrap().1,
        bytes
    );
    put(&fx.app, "alice", EVENT, &event("e2", "")).await;
    let reply = add(
        &fx.app,
        "alice",
        EVENT,
        "",
        "more.bin",
        &vec![b'b'; 10 * 1024],
    )
    .await;
    assert_eq!(reply.status, StatusCode::INSUFFICIENT_STORAGE);
    // Emptying the trash frees it.
    fx.store.empty_trash(fx.alice.id).await.unwrap();
    while fx
        .store
        .purge_expired_trash(i64::MAX / 2, 100)
        .await
        .unwrap()
        > 0
    {}
    assert_eq!(
        fx.store.attachment_usage(fx.alice.id).await.unwrap(),
        (0, 0)
    );
    let reply = add(
        &fx.app,
        "alice",
        EVENT,
        "",
        "more.bin",
        &vec![b'b'; 10 * 1024],
    )
    .await;
    assert_eq!(reply.status, StatusCode::CREATED);
}

#[tokio::test]
async fn per_user_attachment_limit() {
    let mut attachments = attachments_config();
    attachments.max_bytes = 1024;
    attachments.max_bytes_per_user = 2048;
    let fx = fixture_with(config(attachments, false)).await;
    put(&fx.app, "alice", EVENT, &event("e", "")).await;
    for _ in 0..2 {
        let reply = add(&fx.app, "alice", EVENT, "", "k.bin", &[1; 1024]).await;
        assert_eq!(reply.status, StatusCode::CREATED);
    }
    let reply = add(&fx.app, "alice", EVENT, "", "k.bin", &[1; 10]).await;
    assert_eq!(reply.status, StatusCode::INSUFFICIENT_STORAGE);
}

#[tokio::test]
async fn foreign_managed_ids_cannot_be_injected() {
    let fx = fixture().await;
    put(&fx.app, "alice", EVENT, &event("e", "")).await;
    let id = added(&fx, EVENT, b"alice's secret").await;
    let url = format!("{BASE}/dav/attachments/{id}");

    // Bob references alice's attachment in an event of his own.
    let line = format!("ATTACH;MANAGED-ID={id};FILENAME=x.pdf:{url}\r\n");
    let bob_event = "/dav/calendars/bob/default/steal.ics";
    let reply = put(&fx.app, "bob", bob_event, &event("steal", &line)).await;
    assert_eq!(reply.status, StatusCode::FORBIDDEN);
    assert!(
        reply.text().contains("valid-managed-id-parameter"),
        "{}",
        reply.text()
    );
    // A plain URL (no MANAGED-ID) is just a link: stored, but grants nothing.
    let plain = format!("ATTACH:{url}\r\n");
    let reply = put(&fx.app, "bob", bob_event, &event("steal", &plain)).await;
    assert_eq!(reply.status, StatusCode::CREATED);
    let foreign = send(
        &fx.app,
        "bob",
        "GET",
        &attachment_path(&id),
        &[],
        Vec::new(),
    )
    .await;
    let missing = send(
        &fx.app,
        "bob",
        "GET",
        &attachment_path(&"0".repeat(32)),
        &[],
        Vec::new(),
    )
    .await;
    assert_eq!(foreign.status, StatusCode::NOT_FOUND);
    assert_eq!(foreign.status, missing.status);
    assert_eq!(foreign.body, missing.body);
    assert_eq!(foreign.headers, missing.headers);
    // Nor through the attachment POST on his event.
    let reply = send(
        &fx.app,
        "bob",
        "POST",
        &format!("{bob_event}?action=attachment-remove&managed-id={id}"),
        &[],
        Vec::new(),
    )
    .await;
    assert_eq!(reply.status, StatusCode::FORBIDDEN);
    // Alice's attachments through bob's paths: like any foreign path.
    let reply = add(&fx.app, "bob", EVENT, "", "x", b"x").await;
    assert_eq!(reply.status, StatusCode::NOT_FOUND);

    // Unknown ids are refused for alice too, her own ones are fine and
    // their ATTACH properties are rewritten to the server's form.
    let unknown = format!("ATTACH;MANAGED-ID={}:{BASE}/x\r\n", "1".repeat(32));
    let reply = put(
        &fx.app,
        "alice",
        &format!("{CAL}u.ics"),
        &event("u", &unknown),
    )
    .await;
    assert_eq!(reply.status, StatusCode::FORBIDDEN);
    let tampered =
        format!("ATTACH;MANAGED-ID={id};SIZE=1;FILENAME=evil.exe:https://evil.example/x\r\n");
    let copy = format!("{CAL}copy.ics");
    let reply = put(&fx.app, "alice", &copy, &event("copy", &tampered)).await;
    assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.text());
    // The server changed the object: no ETag (RFC 4791 §5.3.4).
    assert!(reply.header("etag").is_none());
    let text = unfold(&get_text(&fx.app, "alice", &copy).await);
    assert!(
        text.contains(&format!("FILENAME=report.pdf;MANAGED-ID={id}:{url}")),
        "{text}"
    );
    assert!(!text.contains("evil"), "{text}");
    // Shared by both objects: kept until both drop it.
    put(&fx.app, "alice", EVENT, &event("e", "")).await;
    assert_eq!(
        send(
            &fx.app,
            "alice",
            "GET",
            &attachment_path(&id),
            &[],
            Vec::new()
        )
        .await
        .status,
        StatusCode::OK
    );
    put(&fx.app, "alice", &copy, &event("copy", "")).await;
    assert_eq!(
        send(
            &fx.app,
            "alice",
            "GET",
            &attachment_path(&id),
            &[],
            Vec::new()
        )
        .await
        .status,
        StatusCode::NOT_FOUND
    );
}

#[tokio::test]
async fn imported_foreign_ids_stay_text_and_do_not_block_edits() {
    let fx = fixture().await;
    let foreign = "ATTACH;MANAGED-ID=other-server-id;FILENAME=a.pdf:https://other.example/a\r\n";
    let body = event("imp", foreign);
    // Stored without the attachments being known (an import or migration).
    let collection = fx
        .store
        .collection(
            fx.alice.id,
            calcraft_core::CollectionKind::Calendar,
            &"default".parse().unwrap(),
        )
        .await
        .unwrap()
        .unwrap();
    fx.store
        .put_item(
            collection.id,
            "imp.ics",
            &calcraft_store::NewItem {
                uid: "imp",
                component: "VEVENT",
                range_start: None,
                range_end: None,
                recurring: false,
                index_version: calcraft_ical::INDEX_VERSION,
                etag: "\"imp\"",
                content: body.as_bytes(),
                attachments: &["other-server-id".to_owned()],
            },
            &calcraft_store::Precondition::None,
        )
        .await
        .unwrap();
    // The client edits the summary and keeps the ATTACH as it was.
    let edited = body.replace("SUMMARY:Meeting", "SUMMARY:Edited");
    let reply = put(&fx.app, "alice", &format!("{CAL}imp.ics"), &edited).await;
    assert_eq!(reply.status, StatusCode::NO_CONTENT, "{}", reply.text());
    assert!(
        get_text(&fx.app, "alice", &format!("{CAL}imp.ics"))
            .await
            .contains("other-server-id")
    );
}

fn scheduling_fixture_config() -> Config {
    config(attachments_config(), true)
}

fn meeting(attendees: &str) -> String {
    event(
        "m",
        &format!(
            "ORGANIZER:mailto:alice@example.org\r\nATTENDEE;PARTSTAT=ACCEPTED:mailto:alice@example.org\r\n{attendees}"
        ),
    )
}

const BOB_LINE: &str = "ATTENDEE;PARTSTAT=NEEDS-ACTION;RSVP=TRUE:mailto:bob@example.org\r\n";

#[tokio::test]
async fn attendees_read_but_cannot_change_and_lose_access_on_cancel() {
    let fx = fixture_with(scheduling_fixture_config()).await;
    let uri = format!("{CAL}m.ics");
    let reply = put(&fx.app, "alice", &uri, &meeting(BOB_LINE)).await;
    assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.text());
    let reply = add(&fx.app, "alice", &uri, "", "agenda.pdf", b"agenda").await;
    assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.text());
    assert!(reply.header("schedule-tag").is_some());
    let id = reply.header("cal-managed-id").unwrap().to_owned();

    // The invitation carries it; bob may download it, carol may not.
    let object = fx
        .store
        .scheduling_object(fx.bob.id, "m")
        .await
        .unwrap()
        .unwrap();
    let bob_uri = format!(
        "/dav/calendars/bob/{}/{}",
        object.collection.slug, object.name
    );
    let copy = unfold(&String::from_utf8(object.content).unwrap());
    assert!(copy.contains(&format!("MANAGED-ID={id}")), "{copy}");
    let download = send(
        &fx.app,
        "bob",
        "GET",
        &attachment_path(&id),
        &[],
        Vec::new(),
    )
    .await;
    assert_eq!(download.status, StatusCode::OK);
    assert_eq!(download.body, b"agenda");
    let carol = send(
        &fx.app,
        "carol",
        "GET",
        &attachment_path(&id),
        &[],
        Vec::new(),
    )
    .await;
    assert_eq!(carol.status, StatusCode::NOT_FOUND);

    // Bob can answer (PUT keeping the attachment) but not change it.
    let accepted = String::from_utf8(
        fx.store
            .scheduling_object(fx.bob.id, "m")
            .await
            .unwrap()
            .unwrap()
            .content,
    )
    .unwrap()
    .replace("PARTSTAT=NEEDS-ACTION", "PARTSTAT=ACCEPTED");
    let reply = put(&fx.app, "bob", &bob_uri, &accepted).await;
    assert!(reply.status.is_success(), "{}", reply.text());
    for query in [
        "?action=attachment-add".to_owned(),
        format!("?action=attachment-remove&managed-id={id}"),
        format!("?action=attachment-update&managed-id={id}"),
    ] {
        let reply = send(
            &fx.app,
            "bob",
            "POST",
            &format!("{bob_uri}{query}"),
            &[],
            "x",
        )
        .await;
        assert_eq!(reply.status, StatusCode::FORBIDDEN, "{query}");
        assert!(reply.text().contains("need-privileges"), "{}", reply.text());
    }
    assert_eq!(
        send(
            &fx.app,
            "bob",
            "GET",
            &attachment_path(&id),
            &[],
            Vec::new()
        )
        .await
        .status,
        StatusCode::OK
    );
    assert_eq!(fx.store.attachment_usage(fx.bob.id).await.unwrap(), (0, 0));

    // Alice removes bob: his copy is cancelled (he answered, so it is kept)
    // without the attachment, and the download is gone for him.
    let current = unfold(&get_text(&fx.app, "alice", &uri).await);
    let without_bob: String = current
        .split_inclusive("\r\n")
        .filter(|line| !line.contains("bob@example.org"))
        .collect();
    assert!(without_bob.contains(&id));
    let reply = put(&fx.app, "alice", &uri, &without_bob).await;
    assert!(reply.status.is_success(), "{}", reply.text());
    let copy = String::from_utf8(
        fx.store
            .scheduling_object(fx.bob.id, "m")
            .await
            .unwrap()
            .unwrap()
            .content,
    )
    .unwrap();
    assert!(copy.contains("STATUS:CANCELLED"), "{copy}");
    assert!(!copy.contains("MANAGED-ID"), "{copy}");
    let bob = send(
        &fx.app,
        "bob",
        "GET",
        &attachment_path(&id),
        &[],
        Vec::new(),
    )
    .await;
    assert_eq!(bob.status, StatusCode::NOT_FOUND);
    // Alice still has it.
    assert_eq!(
        send(
            &fx.app,
            "alice",
            "GET",
            &attachment_path(&id),
            &[],
            Vec::new()
        )
        .await
        .status,
        StatusCode::OK
    );
}

#[tokio::test]
async fn organizer_removal_reaches_attendees() {
    let fx = fixture_with(scheduling_fixture_config()).await;
    let uri = format!("{CAL}m.ics");
    put(&fx.app, "alice", &uri, &meeting(BOB_LINE)).await;
    let id = added(&fx, &uri, b"agenda").await;
    let reply = send(
        &fx.app,
        "alice",
        "POST",
        &format!("{uri}?action=attachment-remove&managed-id={id}"),
        &[],
        Vec::new(),
    )
    .await;
    assert_eq!(reply.status, StatusCode::NO_CONTENT, "{}", reply.text());
    let copy = String::from_utf8(
        fx.store
            .scheduling_object(fx.bob.id, "m")
            .await
            .unwrap()
            .unwrap()
            .content,
    )
    .unwrap();
    assert!(!copy.contains("MANAGED-ID"), "{copy}");
    assert_eq!(
        fx.store.attachment_usage(fx.alice.id).await.unwrap(),
        (0, 0)
    );
    // An organizer DELETE cancels: bob's untouched copy goes, and with it
    // his access.
    let id = added(&fx, &uri, b"v2").await;
    assert_eq!(
        send(
            &fx.app,
            "bob",
            "GET",
            &attachment_path(&id),
            &[],
            Vec::new()
        )
        .await
        .status,
        StatusCode::OK
    );
    send(&fx.app, "alice", "DELETE", &uri, &[], Vec::new()).await;
    assert_eq!(
        send(
            &fx.app,
            "bob",
            "GET",
            &attachment_path(&id),
            &[],
            Vec::new()
        )
        .await
        .status,
        StatusCode::NOT_FOUND
    );
    // In alice's trash it stays hers.
    assert_eq!(
        send(
            &fx.app,
            "alice",
            "GET",
            &attachment_path(&id),
            &[],
            Vec::new()
        )
        .await
        .status,
        StatusCode::OK
    );
}

#[tokio::test]
async fn trash_restore_and_purge() {
    let fx = fixture().await;
    put(&fx.app, "alice", EVENT, &event("e", "")).await;
    let id = added(&fx, EVENT, b"keep me").await;
    assert_eq!(
        send(&fx.app, "alice", "DELETE", EVENT, &[], Vec::new())
            .await
            .status,
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        send(
            &fx.app,
            "alice",
            "GET",
            &attachment_path(&id),
            &[],
            Vec::new()
        )
        .await
        .status,
        StatusCode::OK
    );
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
        .unwrap()
        .unwrap();
    assert!(unfold(&get_text(&fx.app, "alice", EVENT).await).contains(&id));
    send(&fx.app, "alice", "DELETE", EVENT, &[], Vec::new()).await;
    fx.store.empty_trash(fx.alice.id).await.unwrap();
    while fx
        .store
        .purge_expired_trash(i64::MAX / 2, 100)
        .await
        .unwrap()
        > 0
    {}
    assert_eq!(
        send(
            &fx.app,
            "alice",
            "GET",
            &attachment_path(&id),
            &[],
            Vec::new()
        )
        .await
        .status,
        StatusCode::NOT_FOUND
    );
}

#[tokio::test]
async fn move_and_copy() {
    let fx = fixture().await;
    let mkcal = send(
        &fx.app,
        "alice",
        "MKCALENDAR",
        "/dav/calendars/alice/work/",
        &[],
        Vec::new(),
    )
    .await;
    assert_eq!(mkcal.status, StatusCode::CREATED);
    put(&fx.app, "alice", EVENT, &event("e", "")).await;
    let id = added(&fx, EVENT, b"moving").await;
    let reply = send(
        &fx.app,
        "alice",
        "MOVE",
        EVENT,
        &[("destination", "/dav/calendars/alice/work/e.ics")],
        Vec::new(),
    )
    .await;
    assert_eq!(reply.status, StatusCode::CREATED);
    let reply = send(
        &fx.app,
        "alice",
        "COPY",
        "/dav/calendars/alice/work/e.ics",
        &[("destination", "/dav/calendars/alice/default/c.ics")],
        Vec::new(),
    )
    .await;
    assert_eq!(reply.status, StatusCode::CREATED);
    // One blob for both, counted once.
    assert_eq!(
        fx.store.attachment_usage(fx.alice.id).await.unwrap(),
        (1, 6)
    );
    // Removing it from the copy keeps it for the original.
    let reply = send(
        &fx.app,
        "alice",
        "POST",
        &format!("{CAL}c.ics?action=attachment-remove&managed-id={id}"),
        &[],
        Vec::new(),
    )
    .await;
    assert_eq!(reply.status, StatusCode::NO_CONTENT);
    assert_eq!(
        send(
            &fx.app,
            "alice",
            "GET",
            &attachment_path(&id),
            &[],
            Vec::new()
        )
        .await
        .body,
        b"moving"
    );
}

#[tokio::test]
async fn parallel_uploads_lose_nothing() {
    let fx = fixture().await;
    put(&fx.app, "alice", EVENT, &event("e", "")).await;
    // Two at a time: the most one user may upload in parallel.
    let (a, b) = tokio::join!(
        add(&fx.app, "alice", EVENT, "", "p0.txt", b"parallel"),
        add(&fx.app, "alice", EVENT, "", "p1.txt", b"parallel"),
    );
    let replies = [a, b];
    let mut ids = Vec::new();
    for reply in replies {
        assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.text());
        ids.push(reply.header("cal-managed-id").unwrap().to_owned());
    }
    let text = unfold(&get_text(&fx.app, "alice", EVENT).await);
    for id in &ids {
        assert!(text.contains(id.as_str()), "{text}");
    }
    assert_eq!(fx.store.attachment_usage(fx.alice.id).await.unwrap().0, 2);
}

#[tokio::test]
async fn upload_racing_a_delete_leaves_no_orphans() {
    let fx = fixture().await;
    for round in 0..4 {
        let uri = format!("{CAL}r{round}.ics");
        put(&fx.app, "alice", &uri, &event(&format!("r{round}"), "")).await;
        let (upload, delete) = tokio::join!(
            add(&fx.app, "alice", &uri, "", "race.bin", b"race"),
            send(&fx.app, "alice", "DELETE", &uri, &[], Vec::new()),
        );
        assert!(
            upload.status == StatusCode::CREATED || upload.status == StatusCode::NOT_FOUND,
            "{:?}",
            upload.status
        );
        assert_eq!(delete.status, StatusCode::NO_CONTENT);
    }
    // Every attachment left is referenced (from the trash): nothing to purge.
    let later = i64::MAX / 2;
    assert_eq!(
        fx.store
            .purge_unreferenced_attachments(later, 100)
            .await
            .unwrap(),
        0
    );
    fx.store.empty_trash(fx.alice.id).await.unwrap();
    while fx.store.purge_expired_trash(later, 100).await.unwrap() > 0 {}
    assert_eq!(
        fx.store.attachment_usage(fx.alice.id).await.unwrap(),
        (0, 0)
    );
}

struct TempDir(std::path::PathBuf);

impl TempDir {
    fn new(name: &str) -> Self {
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let dir = std::env::temp_dir().join(format!(
            "calcraft-attachments-{name}-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[tokio::test]
async fn backups_contain_attachments() {
    let dir = TempDir::new("backup");
    let db = dir.0.join("calcraft.db");
    let store = Store::open(&db).await.unwrap();
    let (alice, _) = users(&store).await;
    let app = router(
        &config(attachments_config(), false),
        AppState::new(store.clone()).unwrap(),
    );
    put(&app, "alice", EVENT, &event("e", "")).await;
    let reply = add(&app, "alice", EVENT, "", "b.pdf", b"backed up").await;
    let id = reply.header("cal-managed-id").unwrap().to_owned();
    let backup = dir.0.join("backup.db");
    calcraft_store::backup_database(&db, &backup, false)
        .await
        .unwrap();
    // Gone from the live database …
    let reply = send(
        &app,
        "alice",
        "POST",
        &format!("{EVENT}?action=attachment-remove&managed-id={id}"),
        &[],
        Vec::new(),
    )
    .await;
    assert_eq!(reply.status, StatusCode::NO_CONTENT);
    drop(app);
    store.close().await;
    // … and back with the restore.
    calcraft_store::restore_database(&db, &backup)
        .await
        .unwrap();
    let store = Store::open(&db).await.unwrap();
    assert_eq!(
        store
            .attachment_content(&id, alice.id)
            .await
            .unwrap()
            .unwrap(),
        b"backed up"
    );
    store.close().await;
}

/// Alice's meeting with bob and an attachment; returns (managed id, bob's
/// path of his copy).
async fn invitation_with_attachment(fx: &Fx) -> (String, String) {
    let uri = format!("{CAL}m.ics");
    put(&fx.app, "alice", &uri, &meeting(BOB_LINE)).await;
    let id = added(fx, &uri, b"agenda").await;
    let object = fx
        .store
        .scheduling_object(fx.bob.id, "m")
        .await
        .unwrap()
        .unwrap();
    let bob_uri = format!(
        "/dav/calendars/bob/{}/{}",
        object.collection.slug, object.name
    );
    assert_eq!(
        send(
            &fx.app,
            "bob",
            "GET",
            &attachment_path(&id),
            &[],
            Vec::new()
        )
        .await
        .status,
        StatusCode::OK
    );
    (id, bob_uri)
}

async fn bob_copy(fx: &Fx) -> String {
    unfold(
        &String::from_utf8(
            fx.store
                .scheduling_object(fx.bob.id, "m")
                .await
                .unwrap()
                .unwrap()
                .content,
        )
        .unwrap(),
    )
}

#[tokio::test]
async fn a_detached_attendee_copy_loses_access() {
    let fx = fixture_with(scheduling_fixture_config()).await;
    let (id, bob_uri) = invitation_with_attachment(&fx).await;
    // Bob strips ORGANIZER and ATTENDEE: a plain event of his own.
    let detached: String = bob_copy(&fx)
        .await
        .split_inclusive("\r\n")
        .filter(|line| !line.starts_with("ORGANIZER") && !line.starts_with("ATTENDEE"))
        .collect();
    assert!(detached.contains(&id));
    let reply = put(&fx.app, "bob", &bob_uri, &detached).await;
    assert!(reply.status.is_success(), "{}", reply.text());
    assert_eq!(
        send(
            &fx.app,
            "bob",
            "GET",
            &attachment_path(&id),
            &[],
            Vec::new()
        )
        .await
        .status,
        StatusCode::NOT_FOUND
    );
    // Alice deletes her event: the file is gone and no longer charged.
    send(
        &fx.app,
        "alice",
        "DELETE",
        &format!("{CAL}m.ics"),
        &[],
        Vec::new(),
    )
    .await;
    fx.store.empty_trash(fx.alice.id).await.unwrap();
    while fx
        .store
        .purge_expired_trash(i64::MAX / 2, 100)
        .await
        .unwrap()
        > 0
    {}
    assert_eq!(
        fx.store.attachment_usage(fx.alice.id).await.unwrap(),
        (0, 0)
    );
}

#[tokio::test]
async fn an_attendee_cannot_reshare_the_organizer_attachment() {
    let fx = fixture_with(scheduling_fixture_config()).await;
    let (id, bob_uri) = invitation_with_attachment(&fx).await;
    // Bob makes himself organizer and invites carol, keeping the ATTACH.
    let pivot: String = bob_copy(&fx)
        .await
        .split_inclusive("\r\n")
        .map(|line| {
            if line.starts_with("ORGANIZER") {
                "ORGANIZER:mailto:bob@example.org\r\n".to_owned()
            } else if line.starts_with("ATTENDEE") {
                String::new()
            } else if line.starts_with("END:VEVENT") {
                format!(
                    "ATTENDEE;PARTSTAT=ACCEPTED:mailto:bob@example.org\r\nATTENDEE;RSVP=TRUE:mailto:carol@example.org\r\n{line}"
                )
            } else {
                line.to_owned()
            }
        })
        .collect();
    let reply = put(&fx.app, "bob", &bob_uri, &pivot).await;
    assert!(reply.status.is_success(), "{}", reply.text());
    let carol = fx
        .store
        .user_by_name(&"carol".parse().unwrap())
        .await
        .unwrap()
        .unwrap();
    // Carol got bob's invitation, but no access to alice's file.
    assert!(
        fx.store
            .scheduling_object(carol.id, "m")
            .await
            .unwrap()
            .is_some()
    );
    for user in ["carol", "bob"] {
        assert_eq!(
            send(&fx.app, user, "GET", &attachment_path(&id), &[], Vec::new())
                .await
                .status,
            StatusCode::NOT_FOUND,
            "{user}"
        );
    }
    // Alice removes it: nothing keeps it alive or charged to her.
    let reply = send(
        &fx.app,
        "alice",
        "POST",
        &format!("{CAL}m.ics?action=attachment-remove&managed-id={id}"),
        &[],
        Vec::new(),
    )
    .await;
    assert_eq!(reply.status, StatusCode::NO_CONTENT, "{}", reply.text());
    assert_eq!(
        fx.store.attachment_usage(fx.alice.id).await.unwrap(),
        (0, 0)
    );
}

#[tokio::test]
async fn removal_revokes_access_even_if_the_update_is_not_delivered() {
    let mut config = scheduling_fixture_config();
    // Room for the invitation (copy and inbox message), not for an update.
    config.scheduling.max_delivered_bytes_per_hour = 64 * 1024;
    let fx = fixture_with(config).await;
    let uri = format!("{CAL}m.ics");
    put(&fx.app, "alice", &uri, &event("m", "")).await;
    let id = added(&fx, &uri, b"agenda").await;
    let with_attach = unfold(&get_text(&fx.app, "alice", &uri).await);
    let padding = format!("DESCRIPTION:{}\r\n", "x".repeat(24 * 1024));
    let invite = with_attach.replace(
        "SUMMARY:Meeting\r\n",
        &format!(
            "SUMMARY:Meeting\r\n{padding}ORGANIZER:mailto:alice@example.org\r\nATTENDEE;PARTSTAT=ACCEPTED:mailto:alice@example.org\r\n{BOB_LINE}"
        ),
    );
    let reply = put(&fx.app, "alice", &uri, &invite).await;
    assert!(reply.status.is_success(), "{}", reply.text());
    assert!(bob_copy(&fx).await.contains(&id));
    assert_eq!(
        send(
            &fx.app,
            "bob",
            "GET",
            &attachment_path(&id),
            &[],
            Vec::new()
        )
        .await
        .status,
        StatusCode::OK
    );
    let reply = send(
        &fx.app,
        "alice",
        "POST",
        &format!("{uri}?action=attachment-remove&managed-id={id}"),
        &[],
        Vec::new(),
    )
    .await;
    assert_eq!(reply.status, StatusCode::NO_CONTENT, "{}", reply.text());
    // The update did not reach bob: his copy still names the file …
    assert!(bob_copy(&fx).await.contains(&id));
    // … but he cannot download it, and it is not charged to alice.
    assert_eq!(
        send(
            &fx.app,
            "bob",
            "GET",
            &attachment_path(&id),
            &[],
            Vec::new()
        )
        .await
        .status,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        fx.store.attachment_usage(fx.alice.id).await.unwrap(),
        (0, 0)
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn slow_uploads_of_one_user_do_not_block_others() {
    let fx = fixture().await;
    put(&fx.app, "alice", EVENT, &event("e", "")).await;
    put(
        &fx.app,
        "bob",
        "/dav/calendars/bob/default/b.ics",
        &event("b", ""),
    )
    .await;
    // Two uploads that send one byte and then nothing.
    let mut senders = Vec::new();
    let mut tasks = Vec::new();
    for _ in 0..2 {
        let (mut sender, body) = http_body_util::channel::Channel::<bytes::Bytes>::new(1);
        sender
            .send_data(bytes::Bytes::from_static(b"x"))
            .await
            .unwrap();
        senders.push(sender);
        let request = Request::builder()
            .method(Method::POST)
            .uri(format!("{EVENT}?action=attachment-add"))
            .header(header::AUTHORIZATION, basic("alice"))
            .body(Body::new(body))
            .unwrap();
        let app = fx.app.clone();
        tasks.push(tokio::spawn(async move {
            app.oneshot(request).await.unwrap().status()
        }));
    }
    // Let them reach the point where they hold their slots.
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    // A third upload of alice is refused at once (no waiting) …
    let mut refused = None;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    while std::time::Instant::now() < deadline {
        let reply = add(&fx.app, "alice", EVENT, "", "third.txt", b"3").await;
        if reply.status == StatusCode::SERVICE_UNAVAILABLE {
            refused = Some(reply);
            break;
        }
        assert_eq!(reply.status, StatusCode::CREATED);
        let id = reply.header("cal-managed-id").unwrap().to_owned();
        send(
            &fx.app,
            "alice",
            "POST",
            &format!("{EVENT}?action=attachment-remove&managed-id={id}"),
            &[],
            Vec::new(),
        )
        .await;
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    let refused = refused.expect("alice's third upload was not refused");
    assert_eq!(refused.header("retry-after"), Some("1"));
    // … while bob uploads as usual.
    let reply = add(
        &fx.app,
        "bob",
        "/dav/calendars/bob/default/b.ics",
        "",
        "b.txt",
        b"b",
    )
    .await;
    assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.text());
    // The slow bodies end: their uploads go through.
    drop(senders);
    for task in tasks {
        assert_eq!(task.await.unwrap(), StatusCode::CREATED);
    }
    let reply = add(&fx.app, "alice", EVENT, "", "fourth.txt", b"4").await;
    assert_eq!(reply.status, StatusCode::CREATED);
}
