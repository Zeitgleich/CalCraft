//! COPY and MOVE of objects between a user's own collections (RFC 4918
//! §9.8, §9.9): status codes, sync tokens of both collections, ETags,
//! preconditions, UID uniqueness, component sets, quota and parallel MOVEs.
//! Authorization of every Destination variant is in `authz_matrix.rs`.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::{
    path::PathBuf,
    sync::atomic::{AtomicU32, Ordering},
};

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
const HOST: &str = "cal.example.com";
const CAL: &str = "/dav/calendars/alice/default/";
const WORK: &str = "/dav/calendars/alice/work/";
const BOOK: &str = "/dav/addressbooks/alice/default/";

async fn app_with(store: Store) -> Router {
    for (user, password) in [ALICE, BOB] {
        let hash = hash_password(password).unwrap();
        store
            .create_user(&user.parse().unwrap(), &hash, None)
            .await
            .unwrap();
    }
    let mut config = Config::default();
    config.server.max_concurrent_requests_per_client = 1024;
    config.server.client_rate_burst = 1_000_000;
    config.server.client_rate_per_second = 1_000_000;
    let app = router(&config, AppState::new(store).unwrap());
    let reply = send(&app, ALICE, "MKCALENDAR", WORK, &[], "").await;
    assert_eq!(reply.status, StatusCode::CREATED);
    app
}

async fn app() -> (Router, Store) {
    let store = Store::in_memory().await.unwrap();
    (app_with(store.clone()).await, store)
}

fn basic((user, password): (&str, &str)) -> String {
    format!("Basic {}", STANDARD.encode(format!("{user}:{password}")))
}

#[derive(Debug)]
struct Reply {
    status: StatusCode,
    headers: HeaderMap,
    body: String,
}

impl Reply {
    fn etag(&self) -> Option<&str> {
        self.headers.get(header::ETAG).map(|v| v.to_str().unwrap())
    }
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
        .header(header::HOST, HOST)
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

/// COPY or MOVE as alice with a Destination and further headers.
async fn transfer(
    app: &Router,
    method: &str,
    from: &str,
    to: &str,
    headers: &[(&str, &str)],
) -> Reply {
    let mut all = vec![("destination", to)];
    all.extend_from_slice(headers);
    send(app, ALICE, method, from, &all, "").await
}

fn event(uid: &str, component: &str) -> String {
    let dates = if component == "VEVENT" {
        "DTSTART:20261021T100000Z\r\nDTEND:20261021T110000Z\r\n"
    } else {
        ""
    };
    format!(
        "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//Test//EN\r\nBEGIN:{component}\r\nUID:{uid}\r\nDTSTAMP:20261001T000000Z\r\n{dates}SUMMARY:{uid}\r\nEND:{component}\r\nEND:VCALENDAR\r\n"
    )
}

fn vcard(uid: &str) -> String {
    format!("BEGIN:VCARD\r\nVERSION:4.0\r\nUID:{uid}\r\nFN:{uid}\r\nEND:VCARD\r\n")
}

async fn put(app: &Router, uri: &str, body: &str) -> String {
    let content_type = if uri.contains("/addressbooks/") {
        "text/vcard"
    } else {
        "text/calendar"
    };
    let reply = send(
        app,
        ALICE,
        "PUT",
        uri,
        &[("content-type", content_type)],
        body,
    )
    .await;
    assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.body);
    reply.etag().unwrap().to_owned()
}

async fn get(app: &Router, uri: &str) -> Reply {
    send(app, ALICE, "GET", uri, &[], "").await
}

fn between<'a>(body: &'a str, open: &str, close: &str) -> &'a str {
    let start = body
        .find(open)
        .unwrap_or_else(|| panic!("{open} in {body}"))
        + open.len();
    let end = body[start..].find(close).unwrap() + start;
    &body[start..end]
}

async fn sync(app: &Router, collection: &str, token: &str) -> Reply {
    let body = format!(
        r#"<d:sync-collection xmlns:d="DAV:"><d:sync-token>{token}</d:sync-token><d:sync-level>1</d:sync-level><d:prop><d:getetag/></d:prop></d:sync-collection>"#
    );
    let reply = send(app, ALICE, "REPORT", collection, &[], &body).await;
    assert_eq!(reply.status, StatusCode::MULTI_STATUS, "{}", reply.body);
    reply
}

fn sync_token(reply: &Reply) -> String {
    between(&reply.body, "<d:sync-token>", "</d:sync-token>").to_owned()
}

/// The `<d:response>` of `href` in a multistatus body.
fn response_for<'a>(body: &'a str, href: &str) -> Option<&'a str> {
    body.split("<d:response>")
        .skip(1)
        .find(|r| between(r, "<d:href>", "</d:href>") == href)
}

#[tokio::test]
async fn move_between_calendars_updates_both_sync_logs() {
    let (app, _) = app().await;
    let source = format!("{CAL}a.ics");
    let target = format!("{WORK}b.ics");
    let etag = put(&app, &source, &event("move-uid", "VEVENT")).await;
    let cal_token = sync_token(&sync(&app, CAL, "").await);
    let work_token = sync_token(&sync(&app, WORK, "").await);

    let destination = format!("https://{HOST}{target}");
    let reply = transfer(&app, "MOVE", &source, &destination, &[]).await;
    assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.body);
    assert_eq!(reply.etag(), Some(etag.as_str()));

    assert_eq!(get(&app, &source).await.status, StatusCode::NOT_FOUND);
    let moved = get(&app, &target).await;
    assert_eq!(moved.status, StatusCode::OK);
    assert_eq!(moved.etag(), Some(etag.as_str()));
    assert!(moved.body.contains("UID:move-uid"));

    // The source calendar reports the removal, the destination the addition.
    let removed = sync(&app, CAL, &cal_token).await;
    assert_ne!(sync_token(&removed), cal_token);
    let entry = response_for(&removed.body, &source).expect("removal in source log");
    assert!(entry.contains("404"), "{entry}");
    let added = sync(&app, WORK, &work_token).await;
    assert_ne!(sync_token(&added), work_token);
    let entry = response_for(&added.body, &target).expect("addition in destination log");
    assert!(entry.contains(&etag.replace('"', "&quot;")), "{entry}");
    assert!(response_for(&added.body, &source).is_none());

    // Moving again: the source is gone.
    let again = transfer(&app, "MOVE", &source, &destination, &[]).await;
    assert_eq!(again.status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn copy_and_overwrite() {
    let (app, _) = app().await;
    let source = format!("{CAL}a.ics");
    let etag = put(&app, &source, &event("copy-uid", "VEVENT")).await;
    put(
        &app,
        &format!("{WORK}taken.ics"),
        &event("other-uid", "VEVENT"),
    )
    .await;

    let copy = format!("{WORK}copy.ics");
    let reply = transfer(&app, "COPY", &source, &copy, &[]).await;
    assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.body);
    assert_eq!(get(&app, &source).await.etag(), Some(etag.as_str()));
    assert_eq!(get(&app, &copy).await.etag(), Some(etag.as_str()));

    // Overwrite: F refuses an existing destination, T (and the default)
    // replaces it.
    let taken = format!("{WORK}taken.ics");
    let other = format!("{CAL}b.ics");
    put(&app, &other, &event("b-uid", "VEVENT")).await;
    let refused = transfer(&app, "COPY", &other, &taken, &[("overwrite", "F")]).await;
    assert_eq!(refused.status, StatusCode::PRECONDITION_FAILED);
    assert!(get(&app, &taken).await.body.contains("UID:other-uid"));
    let replaced = transfer(&app, "MOVE", &other, &taken, &[("overwrite", "T")]).await;
    assert_eq!(replaced.status, StatusCode::NO_CONTENT, "{}", replaced.body);
    assert!(get(&app, &taken).await.body.contains("UID:b-uid"));
    assert_eq!(get(&app, &other).await.status, StatusCode::NOT_FOUND);

    // Rename within a collection.
    let renamed = format!("{CAL}renamed.ics");
    let reply = transfer(&app, "MOVE", &source, &renamed, &[]).await;
    assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.body);
    assert_eq!(get(&app, &renamed).await.etag(), Some(etag.as_str()));

    // Address books work the same way.
    let card = format!("{BOOK}a.vcf");
    put(&app, &card, &vcard("card-uid")).await;
    let mkcol = r#"<d:mkcol xmlns:d="DAV:" xmlns:card="urn:ietf:params:xml:ns:carddav"><d:set><d:prop><d:resourcetype><d:collection/><card:addressbook/></d:resourcetype></d:prop></d:set></d:mkcol>"#;
    let book2 = "/dav/addressbooks/alice/second/";
    let reply = send(&app, ALICE, "MKCOL", book2, &[], mkcol).await;
    assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.body);
    let moved = format!("{book2}a.vcf");
    let reply = transfer(&app, "MOVE", &card, &moved, &[]).await;
    assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.body);
    assert!(get(&app, &moved).await.body.contains("UID:card-uid"));
}

#[tokio::test]
async fn source_preconditions() {
    let (app, _) = app().await;
    let source = format!("{CAL}a.ics");
    let etag = put(&app, &source, &event("pre-uid", "VEVENT")).await;
    let target = format!("{WORK}a.ics");
    for headers in [
        vec![("if-match", "\"stale\"")],
        vec![("if-none-match", "*")],
        vec![("if-none-match", etag.as_str())],
    ] {
        let reply = transfer(&app, "MOVE", &source, &target, &headers).await;
        assert_eq!(reply.status, StatusCode::PRECONDITION_FAILED, "{headers:?}");
    }
    assert_eq!(get(&app, &target).await.status, StatusCode::NOT_FOUND);
    let reply = transfer(&app, "MOVE", &source, &target, &[("if-match", &etag)]).await;
    assert_eq!(reply.status, StatusCode::CREATED);
}

#[tokio::test]
async fn uid_conflict_and_component_set() {
    let (app, _) = app().await;
    let source = format!("{CAL}a.ics");
    put(&app, &source, &event("dup-uid", "VEVENT")).await;
    put(
        &app,
        &format!("{WORK}existing.ics"),
        &event("dup-uid", "VEVENT"),
    )
    .await;
    let reply = transfer(&app, "MOVE", &source, &format!("{WORK}new.ics"), &[]).await;
    assert_eq!(reply.status, StatusCode::FORBIDDEN);
    assert!(reply.body.contains(&format!(
        "<c:no-uid-conflict><d:href>{WORK}existing.ics</d:href></c:no-uid-conflict>"
    )));
    // A copy within one calendar would duplicate the UID as well.
    let reply = transfer(&app, "COPY", &source, &format!("{CAL}b.ics"), &[]).await;
    assert_eq!(reply.status, StatusCode::FORBIDDEN);
    assert!(reply.body.contains("no-uid-conflict"), "{}", reply.body);

    // A calendar for to-dos only.
    let todos = "/dav/calendars/alice/todos/";
    let mkcalendar = r#"<c:mkcalendar xmlns:d="DAV:" xmlns:c="urn:ietf:params:xml:ns:caldav"><d:set><d:prop><c:supported-calendar-component-set><c:comp name="VTODO"/></c:supported-calendar-component-set></d:prop></d:set></c:mkcalendar>"#;
    let reply = send(&app, ALICE, "MKCALENDAR", todos, &[], mkcalendar).await;
    assert_eq!(reply.status, StatusCode::CREATED);
    let reply = transfer(&app, "COPY", &source, &format!("{todos}a.ics"), &[]).await;
    assert_eq!(reply.status, StatusCode::FORBIDDEN);
    assert!(reply.body.contains("<c:supported-calendar-component/>"));
    let task = format!("{CAL}task.ics");
    put(&app, &task, &event("task-uid", "VTODO")).await;
    let reply = transfer(&app, "MOVE", &task, &format!("{todos}task.ics"), &[]).await;
    assert_eq!(reply.status, StatusCode::CREATED);

    // Calendar objects do not go into address books, and back.
    let reply = transfer(&app, "COPY", &source, &format!("{BOOK}a.vcf"), &[]).await;
    assert_eq!(reply.status, StatusCode::FORBIDDEN);
    assert!(reply.body.contains("<card:supported-address-data/>"));
    let card = format!("{BOOK}c.vcf");
    put(&app, &card, &vcard("c-uid")).await;
    let reply = transfer(&app, "MOVE", &card, &format!("{WORK}c.ics"), &[]).await;
    assert_eq!(reply.status, StatusCode::FORBIDDEN);
    assert!(reply.body.contains("<c:supported-calendar-data/>"));
    assert_eq!(get(&app, &source).await.status, StatusCode::OK);
    assert_eq!(get(&app, &card).await.status, StatusCode::OK);
}

#[tokio::test]
async fn refused_requests_leave_the_source_alone() {
    let (app, _) = app().await;
    let source = format!("{CAL}a.ics");
    put(&app, &source, &event("keep-uid", "VEVENT")).await;
    let target = format!("{WORK}a.ics");
    let cases: Vec<(Vec<(&str, String)>, StatusCode)> = vec![
        (vec![], StatusCode::BAD_REQUEST),
        (
            vec![("destination", "relative/a.ics".into())],
            StatusCode::BAD_REQUEST,
        ),
        (
            vec![
                ("destination", target.clone()),
                ("depth", "infinity".into()),
            ],
            StatusCode::BAD_REQUEST,
        ),
        (
            vec![("destination", target.clone()), ("depth", "1".into())],
            StatusCode::BAD_REQUEST,
        ),
        (
            vec![("destination", target.clone()), ("overwrite", "yes".into())],
            StatusCode::BAD_REQUEST,
        ),
        (
            vec![("destination", format!("{CAL}../work/a.ics"))],
            StatusCode::BAD_REQUEST,
        ),
        (
            vec![("destination", format!("{target}?x=1"))],
            StatusCode::BAD_REQUEST,
        ),
        (
            vec![("destination", format!("https://evil.example{target}"))],
            StatusCode::BAD_GATEWAY,
        ),
        (
            vec![("destination", format!("https://{HOST}:8443{target}"))],
            StatusCode::BAD_GATEWAY,
        ),
        (
            vec![("destination", format!("https://{HOST}{source}"))],
            StatusCode::FORBIDDEN,
        ),
        (
            vec![("destination", WORK.to_owned())],
            StatusCode::FORBIDDEN,
        ),
        (
            vec![("destination", "/dav/calendars/alice/".to_owned())],
            StatusCode::FORBIDDEN,
        ),
        (
            vec![(
                "destination",
                "/dav/calendars/alice/missing/a.ics".to_owned(),
            )],
            StatusCode::CONFLICT,
        ),
        (
            vec![("destination", "/dav/calendars/bob/default/a.ics".to_owned())],
            StatusCode::CONFLICT,
        ),
    ];
    for method in ["COPY", "MOVE"] {
        for (headers, expected) in &cases {
            let headers: Vec<(&str, &str)> =
                headers.iter().map(|(k, v)| (*k, v.as_str())).collect();
            let reply = send(&app, ALICE, method, &source, &headers, "").await;
            assert_eq!(
                reply.status, *expected,
                "{method} {headers:?}: {}",
                reply.body
            );
        }
    }
    assert_eq!(get(&app, &source).await.status, StatusCode::OK);
    assert_eq!(get(&app, &target).await.status, StatusCode::NOT_FOUND);
    let bob_object = "/dav/calendars/bob/default/a.ics";
    let reply = send(&app, BOB, "GET", bob_object, &[], "").await;
    assert_eq!(reply.status, StatusCode::NOT_FOUND);

    // Collections themselves cannot be copied or moved.
    let reply = transfer(&app, "MOVE", WORK, "/dav/calendars/alice/work2/", &[]).await;
    assert_eq!(reply.status, StatusCode::FORBIDDEN);
    let reply = transfer(&app, "COPY", CAL, "/dav/calendars/alice/copy/", &[]).await;
    assert_eq!(reply.status, StatusCode::FORBIDDEN);
    // Bob cannot move alice's object, even into his own calendar.
    let reply = send(
        &app,
        BOB,
        "MOVE",
        &source,
        &[("destination", "/dav/calendars/bob/default/a.ics")],
        "",
    )
    .await;
    assert_eq!(reply.status, StatusCode::NOT_FOUND);
    assert_eq!(get(&app, &source).await.status, StatusCode::OK);
}

#[tokio::test]
async fn copy_counts_against_the_quota_move_does_not() {
    let (app, store) = app().await;
    let source = format!("{CAL}a.ics");
    let body = event("quota-uid", "VEVENT");
    put(&app, &source, &body).await;
    let size = i64::try_from(body.len()).unwrap();
    // Room for the object once, not twice.
    store.set_quota_for_tests(size * 3 / 2);
    let reply = transfer(&app, "COPY", &source, &format!("{WORK}copy.ics"), &[]).await;
    assert_eq!(reply.status, StatusCode::INSUFFICIENT_STORAGE);
    assert_eq!(
        get(&app, &format!("{WORK}copy.ics")).await.status,
        StatusCode::NOT_FOUND
    );
    let reply = transfer(&app, "MOVE", &source, &format!("{WORK}moved.ics"), &[]).await;
    assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.body);
    store.set_quota_for_tests(0);
    let reply = transfer(&app, "COPY", &format!("{WORK}moved.ics"), &source, &[]).await;
    assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.body);
}

struct TempDir(PathBuf);

impl TempDir {
    fn new() -> Self {
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let dir = std::env::temp_dir().join(format!(
            "calcraft-move-{}-{}",
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

/// Parallel MOVEs of one object to different places on a file-backed
/// database: exactly one wins, the object exists exactly once afterwards and
/// both change logs agree with that.
#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn parallel_moves_of_one_object() {
    const ROUNDS: usize = 5;
    const MOVERS: usize = 8;
    let dir = TempDir::new();
    let store = Store::open(&dir.0.join("calcraft.db")).await.unwrap();
    let app = app_with(store.clone()).await;
    for round in 0..ROUNDS {
        let source = format!("{CAL}race-{round}.ics");
        let uid = format!("race-uid-{round}");
        put(&app, &source, &event(&uid, "VEVENT")).await;
        let token = sync_token(&sync(&app, WORK, "").await);
        let mut tasks = tokio::task::JoinSet::new();
        for mover in 0..MOVERS {
            let app = app.clone();
            let source = source.clone();
            // Half into the other calendar, half renamed in place.
            let target = if mover % 2 == 0 {
                format!("{WORK}race-{round}-{mover}.ics")
            } else {
                format!("{CAL}race-{round}-{mover}.ics")
            };
            tasks.spawn(async move {
                let reply = transfer(&app, "MOVE", &source, &target, &[]).await;
                (reply.status, target)
            });
        }
        let mut winners = Vec::new();
        while let Some(result) = tasks.join_next().await {
            let (status, target) = result.unwrap();
            match status {
                StatusCode::CREATED => winners.push(target),
                StatusCode::NOT_FOUND => {}
                other => panic!("unexpected {other} for {target}"),
            }
        }
        assert_eq!(winners.len(), 1, "round {round}: {winners:?}");
        let winner = &winners[0];
        assert_eq!(get(&app, &source).await.status, StatusCode::NOT_FOUND);
        assert!(get(&app, winner).await.body.contains(&format!("UID:{uid}")));
        // Exactly one object with the UID across both calendars.
        let mut found = 0;
        for collection in [CAL, WORK] {
            let all = sync(&app, collection, "").await;
            for response in all.body.split("<d:response>").skip(1) {
                let href = between(response, "<d:href>", "</d:href>");
                if href.contains(&format!("race-{round}")) && response.contains("getetag") {
                    found += 1;
                    assert_eq!(href, winner);
                }
            }
        }
        assert_eq!(found, 1, "round {round}");
        let changes = sync(&app, WORK, &token).await;
        let in_work = response_for(&changes.body, winner).is_some();
        assert_eq!(in_work, winner.starts_with(WORK), "{}", changes.body);
    }
}

/// Contacts may be larger than calendar objects (`storage.max_vcard_bytes`);
/// moving one with a big photo must use the address book limit.
#[tokio::test]
async fn move_a_contact_larger_than_a_calendar_object() {
    use base64::Engine as _;
    let (app, _store) = app().await;
    let mut png = b"\x89PNG\r\n\x1a\n".to_vec();
    png.resize(1_100_000, 0);
    let data = base64::engine::general_purpose::STANDARD.encode(&png);
    let line = format!("PHOTO:data:image/png;base64,{data}");
    // Fold at 75 octets (RFC 6350 §3.2).
    let mut folded = String::new();
    for (i, chunk) in line.as_bytes().chunks(74).enumerate() {
        if i > 0 {
            folded.push_str("\r\n ");
        }
        folded.push_str(std::str::from_utf8(chunk).unwrap());
    }
    let card = format!(
        "BEGIN:VCARD\r\nVERSION:4.0\r\nUID:big-photo\r\nFN:Big Photo\r\n{folded}\r\nEND:VCARD\r\n"
    );
    assert!(card.len() > calcraft_ical::MAX_OBJECT_SIZE);
    let source = format!("{BOOK}big.vcf");
    put(&app, &source, &card).await;

    let mkcol = r#"<d:mkcol xmlns:d="DAV:" xmlns:card="urn:ietf:params:xml:ns:carddav"><d:set><d:prop><d:resourcetype><d:collection/><card:addressbook/></d:resourcetype></d:prop></d:set></d:mkcol>"#;
    let book2 = "/dav/addressbooks/alice/second/";
    let reply = send(&app, ALICE, "MKCOL", book2, &[], mkcol).await;
    assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.body);
    let moved = format!("{book2}big.vcf");
    let reply = transfer(&app, "MOVE", &source, &moved, &[]).await;
    assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.body);
    assert_eq!(get(&app, &moved).await.body, card);
}
