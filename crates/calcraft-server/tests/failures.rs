//! Behaviour when the database cannot be written (read-only, disk full)
//! and when uploads are aborted: clean errors without details, no partial
//! state, reads keep working, and the server recovers by itself.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::{
    path::PathBuf,
    sync::atomic::{AtomicU32, Ordering},
    time::Duration,
};

use axum::{
    Router,
    body::Body,
    http::{Method, Request, StatusCode, header},
};
use base64::{Engine, engine::general_purpose::STANDARD};
use calcraft_server::{AppState, Config, auth::hash_password, router};
use calcraft_store::{SimulatedFailure, Store};
use http_body_util::BodyExt;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tower::ServiceExt;

const ALICE: (&str, &str) = ("alice", "alice password 123");
const CAL: &str = "/dav/calendars/alice/default/";

struct TempDir(PathBuf);

impl TempDir {
    fn new(name: &str) -> Self {
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let dir = std::env::temp_dir().join(format!(
            "calcraft-failures-{name}-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }

    fn db(&self) -> PathBuf {
        self.0.join("calcraft.db")
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

async fn setup(dir: &TempDir) -> (Store, Router) {
    let store = Store::open(&dir.db()).await.unwrap();
    let hash = hash_password(ALICE.1).unwrap();
    store
        .create_user(&ALICE.0.parse().unwrap(), &hash, None)
        .await
        .unwrap();
    let app = router(&Config::default(), AppState::new(store.clone()).unwrap());
    (store, app)
}

fn basic() -> String {
    format!(
        "Basic {}",
        STANDARD.encode(format!("{}:{}", ALICE.0, ALICE.1))
    )
}

struct Reply {
    status: StatusCode,
    body: String,
}

async fn send(
    app: &Router,
    method: &str,
    uri: &str,
    headers: &[(&str, &str)],
    body: &str,
) -> Reply {
    let mut builder = Request::builder()
        .method(Method::from_bytes(method.as_bytes()).unwrap())
        .uri(uri)
        .header(header::AUTHORIZATION, basic());
    for (name, value) in headers {
        builder = builder.header(*name, *value);
    }
    let response = app
        .clone()
        .oneshot(builder.body(Body::from(body.to_owned())).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let body = response.into_body().collect().await.unwrap().to_bytes();
    Reply {
        status,
        body: String::from_utf8(body.to_vec()).unwrap(),
    }
}

/// An event; `padding` bytes of description make it need new database pages.
fn event(uid: &str, padding: usize) -> String {
    let mut description = String::from("DESCRIPTION:");
    for i in 0..padding {
        // Fold every 70 characters (RFC 5545 §3.1).
        if i > 0 && i % 70 == 0 {
            description.push_str("\r\n ");
        }
        description.push(char::from(b'a' + u8::try_from(i % 26).unwrap()));
    }
    format!(
        "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//Test//EN\r\nBEGIN:VEVENT\r\nUID:{uid}\r\nDTSTAMP:20261001T000000Z\r\nDTSTART:20261021T100000Z\r\nDTEND:20261021T110000Z\r\nSUMMARY:{uid}\r\n{description}\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n"
    )
}

async fn put(app: &Router, uri: &str, body: &str) -> Reply {
    send(app, "PUT", uri, &[("content-type", "text/calendar")], body).await
}

async fn status(app: &Router, uri: &str) -> StatusCode {
    let request = Request::get(uri).body(Body::empty()).unwrap();
    app.clone().oneshot(request).await.unwrap().status()
}

/// Current sync token of the default calendar and the objects changed since `token`.
async fn sync(app: &Router, token: &str) -> (String, usize) {
    let body = format!(
        r#"<d:sync-collection xmlns:d="DAV:"><d:sync-token>{token}</d:sync-token><d:sync-level>1</d:sync-level><d:prop><d:getetag/></d:prop></d:sync-collection>"#
    );
    let reply = send(app, "REPORT", CAL, &[], &body).await;
    assert_eq!(reply.status, StatusCode::MULTI_STATUS, "{}", reply.body);
    let start = reply.body.find("<d:sync-token>").unwrap() + "<d:sync-token>".len();
    let end = reply.body.find("</d:sync-token>").unwrap();
    let changes = reply.body.matches("<d:response>").count();
    (reply.body[start..end].to_owned(), changes)
}

/// A failed write must not tell the client anything about the cause.
fn assert_no_details(reply: &Reply) {
    let body = reply.body.to_ascii_lowercase();
    for word in [
        "sqlite",
        "database",
        "disk",
        "readonly",
        "read-only",
        "full",
        "error",
    ] {
        assert!(!body.contains(word), "{word} in {:?}", reply.body);
    }
}

const PARALLEL: usize = 16;

/// `PARALLEL` concurrent PUTs of new objects.
async fn concurrent_puts(app: &Router, prefix: &str, padding: usize) -> Vec<StatusCode> {
    let handles: Vec<_> = (0..PARALLEL)
        .map(|i| {
            let app = app.clone();
            let uid = format!("{prefix}-{i}");
            tokio::spawn(async move {
                put(&app, &format!("{CAL}{uid}.ics"), &event(&uid, padding))
                    .await
                    .status
            })
        })
        .collect();
    let mut statuses = Vec::new();
    for handle in handles {
        statuses.push(handle.await.unwrap());
    }
    statuses
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn full_disk_answers_507_without_partial_state_and_recovers() {
    let dir = TempDir::new("full");
    let (store, app) = setup(&dir).await;
    let small = format!("{CAL}small.ics");
    assert_eq!(
        put(&app, &small, &event("small", 10)).await.status,
        StatusCode::CREATED
    );
    let before = send(&app, "GET", &small, &[], "").await.body;
    let (token, _) = sync(&app, "").await;

    store
        .simulate_failure(SimulatedFailure::Full)
        .await
        .unwrap();
    let big = format!("{CAL}big.ics");
    let reply = put(&app, &big, &event("big", 300_000)).await;
    assert_eq!(reply.status, StatusCode::INSUFFICIENT_STORAGE);
    assert_no_details(&reply);
    // Replacing an existing object with a bigger one fails as a whole.
    let reply = put(&app, &small, &event("small", 300_000)).await;
    assert_eq!(reply.status, StatusCode::INSUFFICIENT_STORAGE);
    assert_no_details(&reply);
    // Failures on many pooled connections. A connection whose transaction
    // SQLite rolled back by itself is replaced; the new one gets the
    // simulated limit again (a really full disk needs no such help).
    for i in 0..PARALLEL {
        let uri = format!("{CAL}full-{i}.ics");
        let reply = put(&app, &uri, &event(&format!("full-{i}"), 300_000)).await;
        assert_eq!(reply.status, StatusCode::INSUFFICIENT_STORAGE);
        store
            .simulate_failure(SimulatedFailure::Full)
            .await
            .unwrap();
    }

    // Reads keep working and show the state before the failed writes.
    assert_eq!(send(&app, "GET", &small, &[], "").await.body, before);
    assert_eq!(
        send(&app, "GET", &big, &[], "").await.status,
        StatusCode::NOT_FOUND
    );
    assert_eq!(sync(&app, &token).await, (token.clone(), 0));
    assert_eq!(status(&app, "/readyz").await, StatusCode::OK);

    // Space freed: writes work again without a restart, on every pooled
    // connection (SQLite rolled the failed transactions back by itself).
    store
        .simulate_failure(SimulatedFailure::None)
        .await
        .unwrap();
    assert_eq!(
        put(&app, &big, &event("big", 300_000)).await.status,
        StatusCode::CREATED
    );
    for status in concurrent_puts(&app, "after", 10).await {
        assert_eq!(status, StatusCode::CREATED);
    }
    let (_, changes) = sync(&app, &token).await;
    assert_eq!(changes, 1 + PARALLEL);
}

#[cfg(feature = "api")]
#[tokio::test]
async fn full_disk_answers_507_in_the_api() {
    let dir = TempDir::new("full-api");
    let (store, app) = setup(&dir).await;
    store
        .simulate_failure(SimulatedFailure::Full)
        .await
        .unwrap();
    // A login needs a new session row; with luck it fits into a free page,
    // otherwise it must fail with 507, never 500.
    let request = Request::post("/api/v1/auth/login")
        .header("x-calcraft-request", "1")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(format!(
            r#"{{"username":"alice","password":"{}"}}"#,
            ALICE.1
        )))
        .unwrap();
    let status = app.clone().oneshot(request).await.unwrap().status();
    assert!(
        [StatusCode::OK, StatusCode::INSUFFICIENT_STORAGE].contains(&status),
        "{status}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn read_only_database_fails_writes_cleanly_and_recovers() {
    let dir = TempDir::new("readonly");
    let (store, app) = setup(&dir).await;
    let uri = format!("{CAL}existing.ics");
    assert_eq!(
        put(&app, &uri, &event("existing", 10)).await.status,
        StatusCode::CREATED
    );
    let (token, _) = sync(&app, "").await;

    store
        .simulate_failure(SimulatedFailure::ReadOnly)
        .await
        .unwrap();
    let rename = r#"<d:propertyupdate xmlns:d="DAV:"><d:set><d:prop><d:displayname>New</d:displayname></d:prop></d:set></d:propertyupdate>"#;
    let writes = [
        put(&app, &format!("{CAL}new.ics"), &event("new", 10)).await,
        put(&app, &uri, &event("existing", 20)).await,
        send(&app, "DELETE", &uri, &[], "").await,
        send(&app, "PROPPATCH", CAL, &[], rename).await,
        send(&app, "MKCALENDAR", "/dav/calendars/alice/other/", &[], "").await,
    ];
    for reply in &writes {
        assert_eq!(
            reply.status,
            StatusCode::INTERNAL_SERVER_ERROR,
            "{}",
            reply.body
        );
        assert_no_details(reply);
    }
    for status in concurrent_puts(&app, "readonly", 10).await {
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    }

    // Reading, listing and syncing still work; nothing changed.
    assert_eq!(
        send(&app, "GET", &uri, &[], "").await.status,
        StatusCode::OK
    );
    let listing = send(&app, "PROPFIND", CAL, &[("depth", "1")], "").await;
    assert_eq!(listing.status, StatusCode::MULTI_STATUS);
    assert!(!listing.body.contains("new.ics"));
    assert_eq!(sync(&app, &token).await, (token.clone(), 0));
    // The database answers, so the instance stays ready (it can serve reads).
    assert_eq!(status(&app, "/readyz").await, StatusCode::OK);

    store
        .simulate_failure(SimulatedFailure::None)
        .await
        .unwrap();
    assert_eq!(
        put(&app, &format!("{CAL}new.ics"), &event("new", 10))
            .await
            .status,
        StatusCode::CREATED
    );
    assert_eq!(
        send(&app, "DELETE", &uri, &[], "").await.status,
        StatusCode::NO_CONTENT
    );
    for status in concurrent_puts(&app, "writable", 10).await {
        assert_eq!(status, StatusCode::CREATED);
    }
    assert_eq!(sync(&app, &token).await.1, 2 + PARALLEL);
}

/// File permissions only matter when SQLite opens the files: a running
/// server keeps its open descriptors. So this checks a start with files and
/// directory made read-only (Unix permissions; Windows has no comparable
/// owner-proof read-only directory, and root ignores the bits).
#[cfg(unix)]
#[tokio::test]
async fn read_only_files_refuse_to_open_and_recover() {
    use std::os::unix::fs::PermissionsExt;

    let dir = TempDir::new("chmod");
    let (store, app) = setup(&dir).await;
    let uri = format!("{CAL}kept.ics");
    assert_eq!(
        put(&app, &uri, &event("kept", 10)).await.status,
        StatusCode::CREATED
    );
    drop(app);
    drop(store);

    let set_mode = |path: &std::path::Path, mode| {
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
    };
    let files: Vec<PathBuf> = std::fs::read_dir(&dir.0)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect();
    for file in &files {
        set_mode(file, 0o400);
    }
    set_mode(&dir.0, 0o500);
    let writable = std::fs::File::create(dir.0.join("probe")).is_ok();

    let result = Store::open(&dir.db()).await;
    set_mode(&dir.0, 0o700);
    for file in &files {
        set_mode(file, 0o600);
    }
    if writable {
        eprintln!("skipped: permissions are not enforced (running as root?)");
        return;
    }
    let err = result.expect_err("read-only database must not open for writing");
    eprintln!("refused as expected: {err}");
    assert!(err.to_string().contains("calcraft.db"), "{err}");

    let store = Store::open(&dir.db()).await.unwrap();
    let app = router(&Config::default(), AppState::new(store).unwrap());
    assert_eq!(
        send(&app, "GET", &uri, &[], "").await.status,
        StatusCode::OK
    );
    assert_eq!(
        put(&app, &uri, &event("kept", 20)).await.status,
        StatusCode::NO_CONTENT
    );
}

/// Sends raw bytes over a real connection, optionally hangs up (closes the
/// sending side) and returns whatever the server answered.
async fn raw_request(
    address: std::net::SocketAddr,
    head: &str,
    body: &[u8],
    hang_up: bool,
) -> String {
    let mut stream = tokio::net::TcpStream::connect(address).await.unwrap();
    stream.write_all(head.as_bytes()).await.unwrap();
    stream.write_all(body).await.unwrap();
    if hang_up {
        stream.shutdown().await.unwrap();
    }
    let mut answer = Vec::new();
    let _ = tokio::time::timeout(Duration::from_secs(5), stream.read_to_end(&mut answer)).await;
    String::from_utf8_lossy(&answer).into_owned()
}

#[tokio::test]
async fn aborted_uploads_leave_no_trace() {
    let dir = TempDir::new("aborted");
    let (_store, app) = setup(&dir).await;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn({
        let app = app.clone();
        async move { axum::serve(listener, app).await.unwrap() }
    });
    let (token, _) = sync(&app, "").await;

    let full = event("aborted", 2_000);
    let partial = &full.as_bytes()[..full.len() / 2];
    let uri = format!("{CAL}aborted.ics");
    let auth = basic();
    // Content-Length promises more than is sent before the client hangs up.
    let head = format!(
        "PUT {uri} HTTP/1.1\r\nHost: localhost\r\nAuthorization: {auth}\r\nContent-Type: text/calendar\r\nContent-Length: {}\r\n\r\n",
        full.len()
    );
    let answer = raw_request(address, &head, partial, true).await;
    assert!(!answer.contains(" 500 "), "{answer}");
    // A chunked upload that ends without the final chunk.
    let head = format!(
        "PUT {uri} HTTP/1.1\r\nHost: localhost\r\nAuthorization: {auth}\r\nContent-Type: text/calendar\r\nTransfer-Encoding: chunked\r\n\r\n"
    );
    let chunk = [
        format!("{:x}\r\n", partial.len()).as_bytes(),
        partial,
        b"\r\n",
    ]
    .concat();
    let answer = raw_request(address, &head, &chunk, true).await;
    assert!(!answer.contains(" 500 "), "{answer}");

    // No object, no change-log entry, no new revision.
    assert_eq!(
        send(&app, "GET", &uri, &[], "").await.status,
        StatusCode::NOT_FOUND
    );
    assert_eq!(sync(&app, &token).await, (token.clone(), 0));

    // The same upload completes normally afterwards.
    let head = format!(
        "PUT {uri} HTTP/1.1\r\nHost: localhost\r\nAuthorization: {auth}\r\nContent-Type: text/calendar\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        full.len()
    );
    let answer = raw_request(address, &head, full.as_bytes(), false).await;
    assert!(answer.starts_with("HTTP/1.1 201"), "{answer}");
    assert_eq!(sync(&app, &token).await.1, 1);
}
