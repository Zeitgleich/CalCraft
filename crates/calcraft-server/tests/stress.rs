//! Many concurrent DAV clients against one file-backed SQLite database:
//! no 500 under contention (`SQLITE_BUSY`), no lost updates, and a change
//! log that reproduces the final state.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::{
    collections::{BTreeMap, BTreeSet},
    path::PathBuf,
    sync::atomic::{AtomicU32, Ordering},
    time::Instant,
};

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
const CAL: &str = "/dav/calendars/alice/default/";
const COUNTER: &str = "/dav/calendars/alice/default/counter.ics";

struct TempDir(PathBuf);

impl TempDir {
    fn new(name: &str) -> Self {
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let dir = std::env::temp_dir().join(format!(
            "calcraft-stress-{name}-{}-{}",
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

async fn app(dir: &TempDir) -> Router {
    let store = Store::open(&dir.0.join("calcraft.db")).await.unwrap();
    let hash = hash_password(ALICE.1).unwrap();
    store
        .create_user(&ALICE.0.parse().unwrap(), &hash, None)
        .await
        .unwrap();
    // All simulated clients share one address here; the per-client request
    // limits (tested in tests/limits.rs) would otherwise throttle them.
    let mut config = Config::default();
    config.server.max_concurrent_requests_per_client = 1024;
    config.server.client_rate_burst = 1_000_000;
    config.server.client_rate_per_second = 1_000_000;
    router(&config, AppState::new(store).unwrap())
}

struct Reply {
    status: StatusCode,
    etag: Option<String>,
    body: String,
}

async fn send(
    app: &Router,
    method: &str,
    uri: &str,
    headers: &[(&str, &str)],
    body: String,
) -> Reply {
    let mut builder = Request::builder()
        .method(Method::from_bytes(method.as_bytes()).unwrap())
        .uri(uri)
        .header(
            header::AUTHORIZATION,
            format!(
                "Basic {}",
                STANDARD.encode(format!("{}:{}", ALICE.0, ALICE.1))
            ),
        );
    for (name, value) in headers {
        builder = builder.header(*name, *value);
    }
    let response = app
        .clone()
        .oneshot(builder.body(Body::from(body)).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let etag = response
        .headers()
        .get(header::ETAG)
        .map(|v| v.to_str().unwrap().to_owned());
    let body = response.into_body().collect().await.unwrap().to_bytes();
    let reply = Reply {
        status,
        etag,
        body: String::from_utf8(body.to_vec()).unwrap(),
    };
    assert!(
        !reply.status.is_server_error(),
        "{method} {uri}: {} {}",
        reply.status,
        reply.body
    );
    reply
}

fn event(uid: &str, summary: &str) -> String {
    format!(
        "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//Test//EN\r\nBEGIN:VEVENT\r\nUID:{uid}\r\nDTSTAMP:20261001T000000Z\r\nDTSTART:20261021T100000Z\r\nDTEND:20261021T110000Z\r\nSUMMARY:{summary}\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n"
    )
}

async fn put(app: &Router, uri: &str, body: String, condition: (&str, &str)) -> Reply {
    send(
        app,
        "PUT",
        uri,
        &[("content-type", "text/calendar"), condition],
        body,
    )
    .await
}

fn between<'a>(body: &'a str, open: &str, close: &str) -> &'a str {
    let start = body
        .find(open)
        .unwrap_or_else(|| panic!("{open} in {body}"))
        + open.len();
    let end = body[start..].find(close).unwrap() + start;
    &body[start..end]
}

/// Revision part of a sync token (`urn:calcraft:sync:<id>-<salt>:<revision>`).
fn token_revision(token: &str) -> i64 {
    token.rsplit(':').next().unwrap().parse().unwrap()
}

async fn sync(app: &Router, token: &str) -> Reply {
    let body = format!(
        r#"<d:sync-collection xmlns:d="DAV:"><d:sync-token>{token}</d:sync-token><d:sync-level>1</d:sync-level><d:prop><d:getetag/></d:prop></d:sync-collection>"#
    );
    let reply = send(app, "REPORT", CAL, &[], body).await;
    assert_eq!(reply.status, StatusCode::MULTI_STATUS, "{}", reply.body);
    reply
}

async fn ctag(app: &Router) -> i64 {
    let body = r#"<d:propfind xmlns:d="DAV:" xmlns:cs="http://calendarserver.org/ns/"><d:prop><cs:getctag/></d:prop></d:propfind>"#;
    let reply = send(app, "PROPFIND", CAL, &[("depth", "0")], body.to_owned()).await;
    between(&reply.body, "<cs:getctag>", "</cs:getctag>")
        .parse()
        .unwrap()
}

/// `href → etag` of all objects in a multistatus body that have an etag.
fn etags(body: &str) -> BTreeMap<String, String> {
    body.split("<d:response>")
        .skip(1)
        .filter(|r| r.contains("<d:getetag>"))
        .map(|r| {
            (
                between(r, "<d:href>", "</d:href>").to_owned(),
                between(r, "<d:getetag>", "</d:getetag>").replace("&quot;", "\""),
            )
        })
        .collect()
}

/// What one client task did that must show up in the final state.
#[derive(Default)]
struct Outcome {
    increments: u32,
    increment_conflicts: u32,
    live: BTreeSet<String>,
    race_wins: Vec<usize>,
    shared_calendar: bool,
}

/// Reads the counter, writes it back incremented with `If-Match`, retrying on 412.
async fn increment(app: &Router, outcome: &mut Outcome) {
    loop {
        let current = send(app, "GET", COUNTER, &[], String::new()).await;
        assert_eq!(current.status, StatusCode::OK);
        let value: u32 = between(&current.body, "SUMMARY:count=", "\r\n")
            .parse()
            .unwrap();
        let etag = current.etag.unwrap();
        let reply = put(
            app,
            COUNTER,
            event("counter", &format!("count={}", value + 1)),
            ("if-match", &etag),
        )
        .await;
        match reply.status {
            StatusCode::NO_CONTENT => {
                assert_ne!(reply.etag.as_deref(), Some(etag.as_str()));
                outcome.increments += 1;
                return;
            }
            StatusCode::PRECONDITION_FAILED => outcome.increment_conflicts += 1,
            status => panic!("unexpected {status} for conditional PUT"),
        }
    }
}

async fn client(app: Router, task: usize, rounds: usize, races: usize) -> Outcome {
    let mut outcome = Outcome::default();
    let mut last_ctag = 0;
    let mut last_token: Option<String> = None;
    for round in 0..rounds {
        increment(&app, &mut outcome).await;

        // Own object: create, conditional update, a stale update (412), sometimes delete.
        let uri = format!("{CAL}t{task}-{round}.ics");
        let uid = format!("t{task}-{round}");
        let created = put(&app, &uri, event(&uid, "v1"), ("if-none-match", "*")).await;
        assert_eq!(created.status, StatusCode::CREATED, "{}", created.body);
        let again = put(&app, &uri, event(&uid, "v1"), ("if-none-match", "*")).await;
        assert_eq!(again.status, StatusCode::PRECONDITION_FAILED);
        let v1 = created.etag.unwrap();
        let updated = put(&app, &uri, event(&uid, "v2"), ("if-match", &v1)).await;
        assert_eq!(updated.status, StatusCode::NO_CONTENT);
        let stale = put(&app, &uri, event(&uid, "v3"), ("if-match", &v1)).await;
        assert_eq!(stale.status, StatusCode::PRECONDITION_FAILED);
        if round % 3 == 0 {
            let stale = send(&app, "DELETE", &uri, &[("if-match", &v1)], String::new()).await;
            assert_eq!(stale.status, StatusCode::PRECONDITION_FAILED);
            let v2 = updated.etag.unwrap();
            let deleted = send(&app, "DELETE", &uri, &[("if-match", &v2)], String::new()).await;
            assert_eq!(deleted.status, StatusCode::NO_CONTENT);
        } else {
            outcome.live.insert(uri);
        }

        // Every task races for the same new names: exactly one wins each.
        let race = (task + round) % races;
        let uri = format!("{CAL}race-{race}.ics");
        let reply = put(
            &app,
            &uri,
            event(&format!("race-{race}"), &format!("by {task}")),
            ("if-none-match", "*"),
        )
        .await;
        match reply.status {
            StatusCode::CREATED => outcome.race_wins.push(race),
            StatusCode::PRECONDITION_FAILED => {}
            status => panic!("unexpected {status} for If-None-Match: *"),
        }

        let rename = format!(
            r#"<d:propertyupdate xmlns:d="DAV:"><d:set><d:prop><d:displayname>by {task} in {round}</d:displayname></d:prop></d:set></d:propertyupdate>"#
        );
        let reply = send(&app, "PROPPATCH", CAL, &[], rename).await;
        assert_eq!(reply.status, StatusCode::MULTI_STATUS);
        assert!(reply.body.contains("HTTP/1.1 200 OK"), "{}", reply.body);

        if round == 0 {
            let own = format!("/dav/calendars/alice/t{task}/");
            let reply = send(&app, "MKCALENDAR", &own, &[], String::new()).await;
            assert_eq!(reply.status, StatusCode::CREATED);
            let shared = send(
                &app,
                "MKCALENDAR",
                "/dav/calendars/alice/shared/",
                &[],
                String::new(),
            )
            .await;
            match shared.status {
                StatusCode::CREATED => outcome.shared_calendar = true,
                StatusCode::METHOD_NOT_ALLOWED => {}
                status => panic!("unexpected {status} for MKCALENDAR"),
            }
        }

        // Change indicators only move forward for every observer.
        let ctag = ctag(&app).await;
        assert!(ctag >= last_ctag, "ctag went back: {last_ctag} -> {ctag}");
        last_ctag = ctag;
        let reply = sync(&app, last_token.as_deref().unwrap_or("")).await;
        let token = between(&reply.body, "<d:sync-token>", "</d:sync-token>").to_owned();
        if let Some(last) = &last_token {
            assert!(token_revision(&token) >= token_revision(last));
        }
        last_token = Some(token);
    }
    outcome
}

async fn run(tasks: usize, rounds: usize) {
    let dir = TempDir::new("writers");
    let app = app(&dir).await;
    let races = 8;
    let started = Instant::now();

    // Warm the credential cache, then take the initial sync token.
    let initial = sync(&app, "").await;
    let initial_token = between(&initial.body, "<d:sync-token>", "</d:sync-token>").to_owned();
    let created = put(
        &app,
        COUNTER,
        event("counter", "count=0"),
        ("if-none-match", "*"),
    )
    .await;
    assert_eq!(created.status, StatusCode::CREATED);

    let handles: Vec<_> = (0..tasks)
        .map(|task| tokio::spawn(client(app.clone(), task, rounds, races)))
        .collect();
    let mut outcomes = Vec::new();
    for handle in handles {
        outcomes.push(handle.await.unwrap());
    }

    // No lost update: the counter equals the number of successful increments.
    let increments: u32 = outcomes.iter().map(|o| o.increments).sum();
    assert_eq!(increments as usize, tasks * rounds);
    let counter = send(&app, "GET", COUNTER, &[], String::new()).await;
    assert!(
        counter
            .body
            .contains(&format!("SUMMARY:count={increments}\r\n")),
        "{}",
        counter.body
    );

    // Every raced name was won exactly once.
    let mut wins: Vec<usize> = outcomes.iter().flat_map(|o| o.race_wins.clone()).collect();
    wins.sort_unstable();
    assert_eq!(wins, (0..races.min(tasks * rounds)).collect::<Vec<_>>());
    assert_eq!(outcomes.iter().filter(|o| o.shared_calendar).count(), 1);

    // Final state = successful writes.
    let mut expected: BTreeSet<String> = outcomes.iter().flat_map(|o| o.live.clone()).collect();
    expected.insert(COUNTER.to_owned());
    expected.extend((0..races).map(|race| format!("{CAL}race-{race}.ics")));
    let listing = send(&app, "PROPFIND", CAL, &[("depth", "1")], String::new()).await;
    let current = etags(&listing.body);
    assert_eq!(current.keys().cloned().collect::<BTreeSet<_>>(), expected);

    // Replaying the change log from the initial token yields exactly the
    // final objects with their current entity tags.
    let replay = sync(&app, &initial_token).await;
    assert!(!replay.body.contains(" 507 "), "sync was truncated");
    assert_eq!(etags(&replay.body), current);
    let final_token = between(&replay.body, "<d:sync-token>", "</d:sync-token>");
    let empty = sync(&app, final_token).await;
    assert_eq!(etags(&empty.body).len(), 0, "{}", empty.body);

    let calendars = send(
        &app,
        "PROPFIND",
        "/dav/calendars/alice/",
        &[("depth", "1")],
        String::new(),
    )
    .await;
    for task in 0..tasks {
        let href = format!("<d:href>/dav/calendars/alice/t{task}/</d:href>");
        assert!(calendars.body.contains(&href), "{href}");
    }

    let conflicts: u32 = outcomes.iter().map(|o| o.increment_conflicts).sum();
    eprintln!(
        "{tasks} clients x {rounds} rounds: {increments} increments, {conflicts} retried after 412, {:?}",
        started.elapsed()
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn parallel_writers_lose_no_updates() {
    run(32, 15).await;
}

/// Heavier variant for nightly runs: `cargo test -- --ignored`.
#[tokio::test(flavor = "multi_thread", worker_threads = 16)]
#[ignore = "slow; run nightly"]
async fn parallel_writers_lose_no_updates_heavy() {
    run(64, 25).await;
}
