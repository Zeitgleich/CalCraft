//! Trash: DAV DELETE moves objects and collections into the trash, sync
//! clients still see deletions, the REST API lists, restores and purges
//! entries.

#![cfg(feature = "api")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use axum::{
    Router,
    body::Body,
    http::{Method, Request, StatusCode, header},
};
use base64::{Engine, engine::general_purpose::STANDARD};
use calcraft_server::{
    AppState, Config,
    auth::{hash_password, now},
    router,
    secrets::hash_secret,
};
use calcraft_store::Store;
use http_body_util::BodyExt;
use serde_json::Value;
use tower::ServiceExt;

const ALICE: (&str, &str) = ("alice", "alice password 123");
const BOB: (&str, &str) = ("bob", "bob password 1234");
const CAL: &str = "/dav/calendars/alice/default/";
const BOOK: &str = "/dav/addressbooks/alice/default/";
const DAY: i64 = 86_400;

struct Env {
    app: Router,
    store: Store,
    alice: String,
    bob: String,
}

async fn env() -> Env {
    env_with(Config::default()).await
}

async fn env_with(config: Config) -> Env {
    let store = Store::in_memory().await.unwrap();
    store.set_trash_retention_days(config.storage.trash_retention_days);
    let mut cookies = Vec::new();
    for (user, password) in [ALICE, BOB] {
        let user = store
            .create_user(
                &user.parse().unwrap(),
                &hash_password(password).unwrap(),
                None,
            )
            .await
            .unwrap();
        let token = format!("{}-session-token-0123456789abcdef", user.username);
        store
            .create_session(user.id, &hash_secret(&token), now(), now() + 3600)
            .await
            .unwrap();
        cookies.push(format!("calcraft_session={token}"));
    }
    let app = router(&config, AppState::new(store.clone()).unwrap());
    let bob = cookies.pop().unwrap();
    let alice = cookies.pop().unwrap();
    Env {
        app,
        store,
        alice,
        bob,
    }
}

struct Reply {
    status: StatusCode,
    etag: Option<String>,
    body: String,
}

impl Reply {
    fn json(&self) -> Value {
        serde_json::from_str(&self.body).unwrap_or(Value::Null)
    }
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
        .uri(uri);
    for (name, value) in headers {
        builder = builder.header(*name, *value);
    }
    let response = app
        .clone()
        .oneshot(builder.body(Body::from(body.to_owned())).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let etag = response
        .headers()
        .get(header::ETAG)
        .map(|v| v.to_str().unwrap().to_owned());
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    Reply {
        status,
        etag,
        body: String::from_utf8_lossy(&bytes).into_owned(),
    }
}

fn basic((user, password): (&str, &str)) -> String {
    format!("Basic {}", STANDARD.encode(format!("{user}:{password}")))
}

async fn dav(app: &Router, method: &str, uri: &str, body: &str) -> Reply {
    let auth = basic(ALICE);
    let content_type = if uri.ends_with(".vcf") {
        "text/vcard"
    } else if uri.ends_with(".ics") {
        "text/calendar"
    } else {
        "application/xml"
    };
    send(
        app,
        method,
        uri,
        &[("authorization", &auth), ("content-type", content_type)],
        body,
    )
    .await
}

async fn api(app: &Router, cookie: &str, method: &str, uri: &str) -> Reply {
    send(
        app,
        method,
        &format!("/api{uri}"),
        &[("cookie", cookie), ("x-calcraft-request", "1")],
        "",
    )
    .await
}

fn event(uid: &str, summary: &str) -> String {
    format!(
        "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//Test//EN\r\nBEGIN:VEVENT\r\nUID:{uid}\r\nDTSTAMP:20261001T000000Z\r\nDTSTART:20261021T100000Z\r\nDTEND:20261021T110000Z\r\nSUMMARY:{summary}\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n"
    )
}

fn vcard(uid: &str, name: &str) -> String {
    format!("BEGIN:VCARD\r\nVERSION:4.0\r\nUID:{uid}\r\nFN:{name}\r\nEND:VCARD\r\n")
}

fn token_of(body: &str) -> String {
    let start = body.find("<d:sync-token>").unwrap() + "<d:sync-token>".len();
    let end = body[start..].find("</d:sync-token>").unwrap() + start;
    body[start..end].to_owned()
}

async fn sync(app: &Router, collection: &str, token: &str) -> Reply {
    let body = format!(
        r#"<d:sync-collection xmlns:d="DAV:"><d:sync-token>{token}</d:sync-token><d:sync-level>1</d:sync-level><d:prop><d:getetag/></d:prop></d:sync-collection>"#
    );
    let reply = dav(app, "REPORT", collection, &body).await;
    assert_eq!(reply.status, StatusCode::MULTI_STATUS, "{}", reply.body);
    reply
}

async fn trash(env: &Env, cookie: &str) -> Value {
    let reply = api(&env.app, cookie, "GET", "/v1/trash").await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
    reply.json()
}

fn ids(list: &Value) -> Vec<String> {
    list["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["id"].as_str().unwrap().to_owned())
        .collect()
}

#[tokio::test]
async fn delete_and_restore_round_trip_through_sync() {
    let env = env().await;
    let uri = format!("{CAL}lunch.ics");
    let put = dav(&env.app, "PUT", &uri, &event("lunch", "Lunch with Bob")).await;
    assert_eq!(put.status, StatusCode::CREATED);
    let etag = put.etag.unwrap();
    let token = token_of(&sync(&env.app, CAL, "").await.body);

    let deleted = dav(&env.app, "DELETE", &uri, "").await;
    assert_eq!(deleted.status, StatusCode::NO_CONTENT);
    assert_eq!(
        dav(&env.app, "GET", &uri, "").await.status,
        StatusCode::NOT_FOUND
    );
    // Sync clients see a deletion, exactly as before.
    let delta = sync(&env.app, CAL, &token).await;
    assert!(
        delta.body.contains(
            "<d:href>/dav/calendars/alice/default/lunch.ics</d:href><d:status>HTTP/1.1 404 Not Found</d:status>"
        ),
        "{}",
        delta.body
    );
    let token = token_of(&delta.body);

    let list = trash(&env, &env.alice).await;
    assert_eq!(list["total"], 1);
    assert_eq!(list["retention_days"], 30);
    let entry = &list["entries"][0];
    assert_eq!(entry["kind"], "item");
    assert_eq!(entry["collection_kind"], "calendar");
    assert_eq!(entry["collection"], "default");
    assert_eq!(entry["name"], "lunch.ics");
    assert_eq!(entry["summary"], "Lunch with Bob");
    assert_eq!(
        entry["expires_at"].as_i64().unwrap() - entry["deleted_at"].as_i64().unwrap(),
        30 * DAY
    );
    // The object's content is never part of the listing.
    assert!(!list.to_string().contains("BEGIN:VCALENDAR"));

    let id = ids(&list).remove(0);
    let restored = api(
        &env.app,
        &env.alice,
        "POST",
        &format!("/v1/trash/{id}/restore"),
    )
    .await;
    assert_eq!(restored.status, StatusCode::OK, "{}", restored.body);
    let json = restored.json();
    assert_eq!(json["etag"], etag.as_str());
    assert_eq!(json["href"], "/dav/calendars/alice/default/lunch.ics");
    let get = dav(&env.app, "GET", &uri, "").await;
    assert_eq!(get.status, StatusCode::OK);
    assert_eq!(get.etag.as_deref(), Some(etag.as_str()));
    assert!(get.body.contains("Lunch with Bob"));
    // Clients sync it back in.
    let delta = sync(&env.app, CAL, &token).await;
    assert!(
        delta
            .body
            .contains("<d:href>/dav/calendars/alice/default/lunch.ics</d:href><d:propstat>"),
        "{}",
        delta.body
    );
    assert!(
        delta.body.contains(etag.trim_matches('"')),
        "{}",
        delta.body
    );
    assert_eq!(trash(&env, &env.alice).await["total"], 0);
    // Gone from the trash: a second restore is 404.
    let again = api(
        &env.app,
        &env.alice,
        "POST",
        &format!("/v1/trash/{id}/restore"),
    )
    .await;
    assert_eq!(again.status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn a_new_put_after_delete_works_and_blocks_the_restore() {
    let env = env().await;
    let uri = format!("{BOOK}erika.vcf");
    dav(&env.app, "PUT", &uri, &vcard("erika", "Erika")).await;
    dav(&env.app, "DELETE", &uri, "").await;
    let other = format!("{BOOK}other.vcf");
    dav(&env.app, "PUT", &other, &vcard("other", "Other")).await;
    dav(&env.app, "DELETE", &other, "").await;
    let list = trash(&env, &env.alice).await;
    assert_eq!(list["entries"][0]["summary"], "Other");
    let (other_id, erika_id) = (ids(&list)[0].clone(), ids(&list)[1].clone());

    // Same name again (with a new UID): allowed, and the restore conflicts.
    let put = dav(&env.app, "PUT", &uri, &vcard("erika-2", "Erika 2")).await;
    assert_eq!(put.status, StatusCode::CREATED);
    let reply = api(
        &env.app,
        &env.alice,
        "POST",
        &format!("/v1/trash/{erika_id}/restore"),
    )
    .await;
    assert_eq!(reply.status, StatusCode::CONFLICT);
    assert_eq!(reply.json()["error"], "name_taken");

    // Same UID under another name.
    let put = dav(
        &env.app,
        "PUT",
        &format!("{BOOK}renamed.vcf"),
        &vcard("other", "Other"),
    )
    .await;
    assert_eq!(put.status, StatusCode::CREATED);
    let reply = api(
        &env.app,
        &env.alice,
        "POST",
        &format!("/v1/trash/{other_id}/restore"),
    )
    .await;
    assert_eq!(reply.status, StatusCode::CONFLICT);
    assert_eq!(reply.json()["error"], "uid_taken");
    assert_eq!(trash(&env, &env.alice).await["total"], 2);
}

#[tokio::test]
async fn collections_are_trashed_and_restored_as_a_unit() {
    let env = env().await;
    let work = "/dav/calendars/alice/work/";
    let mk = dav(&env.app, "MKCALENDAR", work, "").await;
    assert_eq!(mk.status, StatusCode::CREATED);
    for name in ["a", "b", "c"] {
        let put = dav(
            &env.app,
            "PUT",
            &format!("{work}{name}.ics"),
            &event(name, name),
        )
        .await;
        assert_eq!(put.status, StatusCode::CREATED);
    }
    let old_token = token_of(&sync(&env.app, work, "").await.body);
    // c first on its own, then the whole calendar.
    dav(&env.app, "DELETE", &format!("{work}c.ics"), "").await;
    assert_eq!(
        dav(&env.app, "DELETE", work, "").await.status,
        StatusCode::NO_CONTENT
    );
    assert_ne!(
        dav(&env.app, "PROPFIND", work, "").await.status,
        StatusCode::MULTI_STATUS
    );

    let list = trash(&env, &env.alice).await;
    assert_eq!(list["total"], 2);
    let unit = &list["entries"][0];
    assert_eq!(unit["kind"], "collection");
    assert_eq!(unit["collection"], "work");
    assert_eq!(unit["items"], 2);
    assert!(unit["name"].is_null());
    let (unit_id, single_id) = (ids(&list)[0].clone(), ids(&list)[1].clone());

    let reply = api(
        &env.app,
        &env.alice,
        "POST",
        &format!("/v1/trash/{single_id}/restore"),
    )
    .await;
    assert_eq!(reply.status, StatusCode::CONFLICT);
    assert_eq!(reply.json()["error"], "collection_in_trash");

    // A new calendar with the same name blocks the restore.
    dav(&env.app, "MKCALENDAR", work, "").await;
    let reply = api(
        &env.app,
        &env.alice,
        "POST",
        &format!("/v1/trash/{unit_id}/restore"),
    )
    .await;
    assert_eq!(reply.status, StatusCode::CONFLICT);
    assert_eq!(reply.json()["error"], "collection_exists");
    let delete = api(
        &env.app,
        &env.alice,
        "DELETE",
        "/v1/collections/calendar/work",
    )
    .await;
    assert_eq!(delete.status, StatusCode::NO_CONTENT);
    // That empty calendar went to the trash too.
    assert_eq!(trash(&env, &env.alice).await["total"], 3);

    let reply = api(
        &env.app,
        &env.alice,
        "POST",
        &format!("/v1/trash/{unit_id}/restore"),
    )
    .await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
    assert_eq!(reply.json()["href"], "/dav/calendars/alice/work/");
    // Old sync tokens of the deleted calendar are not accepted.
    let body = format!(
        r#"<d:sync-collection xmlns:d="DAV:"><d:sync-token>{old_token}</d:sync-token><d:sync-level>1</d:sync-level><d:prop><d:getetag/></d:prop></d:sync-collection>"#
    );
    assert_eq!(
        dav(&env.app, "REPORT", work, &body).await.status,
        StatusCode::FORBIDDEN
    );
    let initial = sync(&env.app, work, "").await;
    assert!(initial.body.contains("a.ics") && initial.body.contains("b.ics"));
    let reply = api(
        &env.app,
        &env.alice,
        "POST",
        &format!("/v1/trash/{single_id}/restore"),
    )
    .await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
    assert_eq!(
        dav(&env.app, "GET", &format!("{work}c.ics"), "")
            .await
            .status,
        StatusCode::OK
    );
}

#[tokio::test]
async fn object_of_a_purged_collection_answers_collection_deleted() {
    let env = env().await;
    let work = "/dav/addressbooks/alice/work/";
    let mkcol = send(
        &env.app,
        "MKCOL",
        work,
        &[("authorization", &basic(ALICE))],
        r#"<d:mkcol xmlns:d="DAV:" xmlns:card="urn:ietf:params:xml:ns:carddav"><d:set><d:prop><d:resourcetype><d:collection/><card:addressbook/></d:resourcetype></d:prop></d:set></d:mkcol>"#,
    )
    .await;
    assert_eq!(mkcol.status, StatusCode::CREATED, "{}", mkcol.body);
    dav(&env.app, "PUT", &format!("{work}x.vcf"), &vcard("x", "X")).await;
    dav(&env.app, "DELETE", &format!("{work}x.vcf"), "").await;
    dav(&env.app, "DELETE", work, "").await;
    let list = trash(&env, &env.alice).await;
    let (unit_id, single_id) = (ids(&list)[0].clone(), ids(&list)[1].clone());
    let reply = api(
        &env.app,
        &env.alice,
        "DELETE",
        &format!("/v1/trash/{unit_id}"),
    )
    .await;
    assert_eq!(reply.status, StatusCode::NO_CONTENT);
    let reply = api(
        &env.app,
        &env.alice,
        "POST",
        &format!("/v1/trash/{single_id}/restore"),
    )
    .await;
    assert_eq!(reply.status, StatusCode::CONFLICT);
    assert_eq!(reply.json()["error"], "collection_deleted");
    // Way out: restore it into another address book.
    for bad in ["nope", "..", "%2E%2E"] {
        let reply = api(
            &env.app,
            &env.alice,
            "POST",
            &format!("/v1/trash/{single_id}/restore?collection={bad}"),
        )
        .await;
        assert_eq!(reply.status, StatusCode::CONFLICT, "{bad}");
        assert_eq!(reply.json()["error"], "invalid_target", "{bad}");
    }
    let reply = api(
        &env.app,
        &env.alice,
        "POST",
        &format!("/v1/trash/{single_id}/restore?collection=default"),
    )
    .await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
    assert_eq!(
        reply.json()["href"],
        "/dav/addressbooks/alice/default/x.vcf"
    );
    assert_eq!(
        dav(&env.app, "GET", &format!("{BOOK}x.vcf"), "")
            .await
            .status,
        StatusCode::OK
    );
    // Emptying removes the rest.
    let reply = api(&env.app, &env.alice, "DELETE", "/v1/trash").await;
    assert_eq!(reply.status, StatusCode::NO_CONTENT);
    assert_eq!(trash(&env, &env.alice).await["total"], 0);
}

#[tokio::test]
async fn expired_entries_are_purged() {
    let env = env().await;
    for name in ["old", "new"] {
        let uri = format!("{CAL}{name}.ics");
        dav(&env.app, "PUT", &uri, &event(name, name)).await;
    }
    dav(&env.app, "DELETE", &format!("{CAL}old.ics"), "").await;
    // Nothing is due yet.
    assert_eq!(
        calcraft_server::trash::purge_expired(&env.store)
            .await
            .unwrap(),
        0
    );
    assert_eq!(
        env.store
            .purge_expired_trash(now() + 30 * DAY + 10, 500)
            .await
            .unwrap(),
        1
    );
    dav(&env.app, "DELETE", &format!("{CAL}new.ics"), "").await;
    let list = trash(&env, &env.alice).await;
    assert_eq!(list["total"], 1);
    assert_eq!(list["entries"][0]["name"], "new.ics");
}

#[tokio::test]
async fn disabled_trash_deletes_for_good() {
    let mut config = Config::default();
    config.storage.trash_retention_days = 0;
    let env = env_with(config).await;
    let uri = format!("{CAL}gone.ics");
    dav(&env.app, "PUT", &uri, &event("gone", "Gone")).await;
    let token = token_of(&sync(&env.app, CAL, "").await.body);
    assert_eq!(
        dav(&env.app, "DELETE", &uri, "").await.status,
        StatusCode::NO_CONTENT
    );
    let delta = sync(&env.app, CAL, &token).await;
    assert!(delta.body.contains("404 Not Found"), "{}", delta.body);
    dav(&env.app, "MKCALENDAR", "/dav/calendars/alice/work/", "").await;
    dav(&env.app, "DELETE", "/dav/calendars/alice/work/", "").await;
    let list = trash(&env, &env.alice).await;
    assert_eq!(list["retention_days"], 0);
    assert_eq!(list["total"], 0);
    // The same name can be used again.
    let put = dav(&env.app, "PUT", &uri, &event("gone", "Again")).await;
    assert_eq!(put.status, StatusCode::CREATED);
}

#[tokio::test]
async fn the_trash_counts_towards_the_quota_until_emptied() {
    let env = env().await;
    let body = event("big", "Big");
    let size = i64::try_from(body.len()).unwrap();
    env.store
        .set_limits_for_testing(size * 2 + 10, calcraft_store::MAX_TRASH_ROWS_PER_USER);
    let one = format!("{CAL}one.ics");
    assert_eq!(
        dav(&env.app, "PUT", &one, &body).await.status,
        StatusCode::CREATED
    );
    dav(&env.app, "DELETE", &one, "").await;
    let two = format!("{CAL}two.ics");
    assert_eq!(
        dav(&env.app, "PUT", &two, &body).await.status,
        StatusCode::CREATED
    );
    dav(&env.app, "DELETE", &two, "").await;
    // Both are in the trash: no room for a third.
    let three = format!("{CAL}three.ics");
    let reply = dav(&env.app, "PUT", &three, &event("three", "Big")).await;
    assert_eq!(reply.status, StatusCode::INSUFFICIENT_STORAGE);
    assert_eq!(trash(&env, &env.alice).await["bytes"], size * 2);
    // A restore needs no extra space.
    let id = ids(&trash(&env, &env.alice).await).remove(0);
    let reply = api(
        &env.app,
        &env.alice,
        "POST",
        &format!("/v1/trash/{id}/restore"),
    )
    .await;
    assert_eq!(reply.status, StatusCode::OK);
    let reply = api(&env.app, &env.alice, "DELETE", "/v1/trash").await;
    assert_eq!(reply.status, StatusCode::NO_CONTENT);
    let reply = dav(&env.app, "PUT", &three, &event("three", "Big")).await;
    assert_eq!(reply.status, StatusCode::CREATED);
}

#[tokio::test]
async fn other_users_cannot_see_or_touch_entries() {
    let env = env().await;
    let uri = format!("{CAL}private.ics");
    dav(&env.app, "PUT", &uri, &event("private", "Secret meeting")).await;
    dav(&env.app, "DELETE", &uri, "").await;
    let id = ids(&trash(&env, &env.alice).await).remove(0);

    let bobs = trash(&env, &env.bob).await;
    assert_eq!(bobs["total"], 0);
    assert!(!bobs.to_string().contains("Secret meeting"));
    for (method, path) in [
        ("POST", format!("/v1/trash/{id}/restore")),
        ("DELETE", format!("/v1/trash/{id}")),
    ] {
        let reply = api(&env.app, &env.bob, method, &path).await;
        assert_eq!(reply.status, StatusCode::NOT_FOUND, "{method} {path}");
        assert_eq!(reply.json()["error"], "not_found");
    }
    assert_eq!(
        api(&env.app, &env.bob, "DELETE", "/v1/trash").await.status,
        StatusCode::NO_CONTENT
    );
    // Without session or CSRF header nothing happens either.
    let reply = send(
        &env.app,
        "DELETE",
        "/api/v1/trash",
        &[("x-calcraft-request", "1")],
        "",
    )
    .await;
    assert_eq!(reply.status, StatusCode::UNAUTHORIZED);
    let reply = send(
        &env.app,
        "DELETE",
        "/api/v1/trash",
        &[("cookie", &env.alice)],
        "",
    )
    .await;
    assert_eq!(reply.status, StatusCode::FORBIDDEN);
    // Malformed ids are just unknown.
    let reply = api(&env.app, &env.alice, "POST", "/v1/trash/..%2F1/restore").await;
    assert_eq!(reply.status, StatusCode::NOT_FOUND);
    assert_eq!(trash(&env, &env.alice).await["total"], 1);
}

#[tokio::test]
async fn listing_is_paginated() {
    let env = env().await;
    for i in 0..5 {
        let uri = format!("{CAL}{i}.ics");
        dav(&env.app, "PUT", &uri, &event(&i.to_string(), "x")).await;
        dav(&env.app, "DELETE", &uri, "").await;
    }
    let page = api(&env.app, &env.alice, "GET", "/v1/trash?limit=2&offset=2").await;
    assert_eq!(page.status, StatusCode::OK);
    let json = page.json();
    assert_eq!(json["total"], 5);
    assert_eq!(json["entries"].as_array().unwrap().len(), 2);
    assert_eq!(json["limit"], 2);
    let page = api(&env.app, &env.alice, "GET", "/v1/trash?limit=100000").await;
    assert_eq!(page.json()["limit"], 200);
    let page = api(&env.app, &env.alice, "GET", "/v1/trash?sort=name").await;
    assert_eq!(page.status, StatusCode::BAD_REQUEST);
}
