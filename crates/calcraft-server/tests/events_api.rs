//! The web UI's event API (`/api/v1/calendars`, `/api/v1/events`): the
//! listing with recurrence expansion, the dialog's reads and writes, drag
//! and drop, instance edits, deletes and moves, and that every change goes
//! through the DAV write path with its rules: read-only shares and
//! subscriptions, private busy stubs, sharees and scheduling objects,
//! scheduling messages, quota, trash and ETag conflicts.

#![cfg(feature = "api")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use axum::{
    Router,
    body::Body,
    http::{Method, Request},
};
use base64::{Engine, engine::general_purpose::STANDARD};
use calcraft_core::{CollectionKind, api::CSRF_HEADER};
use calcraft_server::{
    AppState, Config,
    auth::now,
    router,
    secrets::{hash_secret, random_token},
};
use calcraft_store::{Store, User};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;

const ALICE_CAL: &str = "/dav/calendars/alice/default/";
const OCT_1: i64 = 1_790_812_800; // 2026-10-01T00:00:00Z
const DAY: i64 = 86_400;

struct Fx {
    app: Router,
    store: Store,
    alice: User,
    bob: User,
    passwords: Vec<(String, String)>,
    sessions: Vec<(String, String)>,
}

#[derive(Debug)]
struct Reply {
    status: u16,
    headers: Vec<(String, String)>,
    body: String,
}

impl Reply {
    fn json(&self) -> Value {
        serde_json::from_str(&self.body).unwrap_or(Value::Null)
    }

    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
    }
}

fn config(scheduling: bool) -> Config {
    let mut config = Config::default();
    config.server.max_concurrent_requests_per_client = 4096;
    config.server.client_rate_burst = u32::MAX;
    config.server.client_rate_per_second = u32::MAX;
    if scheduling {
        config.scheduling.enabled = true;
        config.scheduling.domain = Some("example.org".to_owned());
    }
    config
}

async fn fixture_with(config: Config) -> Fx {
    let store = Store::in_memory().await.unwrap();
    let mut users = Vec::new();
    let mut passwords = Vec::new();
    let mut sessions = Vec::new();
    for name in ["alice", "bob"] {
        let user = store
            .create_user(&name.parse().unwrap(), "unused", None)
            .await
            .unwrap();
        let secret = random_token::<20>();
        let id = store
            .create_app_password(user.id, "test", &hash_secret(&secret), now())
            .await
            .unwrap();
        passwords.push((name.to_owned(), format!("ccap_{id}_{secret}")));
        let token = format!("{name}-session-token-0123456789abcdef");
        store
            .create_session(user.id, &hash_secret(&token), now(), now() + 3600)
            .await
            .unwrap();
        sessions.push((name.to_owned(), token));
        users.push(user);
    }
    let app = router(&config, AppState::new(store.clone()).unwrap());
    let mut users = users.into_iter();
    Fx {
        app,
        store,
        alice: users.next().unwrap(),
        bob: users.next().unwrap(),
        passwords,
        sessions,
    }
}

async fn fixture() -> Fx {
    fixture_with(config(false)).await
}

fn ics(body: &str) -> String {
    format!("BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//Test//EN\r\n{body}END:VCALENDAR\r\n")
}

fn vevent(uid: &str, props: &str) -> String {
    format!("BEGIN:VEVENT\r\nUID:{uid}\r\nDTSTAMP:20261001T000000Z\r\n{props}END:VEVENT\r\n")
}

fn time(date: &str, time: Option<&str>, tzid: Option<&str>) -> Value {
    json!({"date": date, "time": time, "tzid": tzid})
}

fn input(summary: &str, start: Value, end: Value) -> Value {
    json!({
        "fields": {
            "summary": summary,
            "location": "Room 4",
            "description": "Bring notes",
            "start": start,
            "end": end,
            "alarms": [{"minutes_before": 10}],
        }
    })
}

fn timed(summary: &str, date: &str, from: &str, to: &str) -> Value {
    input(
        summary,
        time(date, Some(from), Some("UTC")),
        time(date, Some(to), Some("UTC")),
    )
}

impl Fx {
    fn session(&self, user: &str) -> &str {
        &self.sessions.iter().find(|(n, _)| n == user).unwrap().1
    }

    fn password(&self, user: &str) -> String {
        let (_, password) = self.passwords.iter().find(|(n, _)| n == user).unwrap();
        format!("Basic {}", STANDARD.encode(format!("{user}:{password}")))
    }

    async fn raw(&self, method: &str, uri: &str, headers: &[(&str, String)], body: &str) -> Reply {
        let mut builder = Request::builder()
            .method(Method::from_bytes(method.as_bytes()).unwrap())
            .uri(uri)
            .header("host", "localhost");
        for (name, value) in headers {
            builder = builder.header(*name, value.as_str());
        }
        let response = self
            .app
            .clone()
            .oneshot(builder.body(Body::from(body.to_owned())).unwrap())
            .await
            .unwrap();
        let status = response.status().as_u16();
        let headers = response
            .headers()
            .iter()
            .map(|(k, v)| {
                (
                    k.to_string(),
                    String::from_utf8_lossy(v.as_bytes()).into_owned(),
                )
            })
            .collect();
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        Reply {
            status,
            headers,
            body: String::from_utf8_lossy(&bytes).into_owned(),
        }
    }

    async fn api_with(
        &self,
        user: &str,
        method: &str,
        uri: &str,
        etag: Option<&str>,
        body: Option<&Value>,
    ) -> Reply {
        let mut headers: Vec<(&str, String)> = vec![
            ("cookie", format!("calcraft_session={}", self.session(user))),
            (CSRF_HEADER, "1".to_owned()),
        ];
        if let Some(etag) = etag {
            headers.push(("if-match", etag.to_owned()));
        }
        let text = body.map(Value::to_string).unwrap_or_default();
        if body.is_some() {
            headers.push(("content-type", "application/json".to_owned()));
        }
        self.raw(method, &format!("/api{uri}"), &headers, &text)
            .await
    }

    async fn api(&self, user: &str, method: &str, uri: &str, body: Option<&Value>) -> Reply {
        self.api_with(user, method, uri, None, body).await
    }

    async fn dav(
        &self,
        user: &str,
        method: &str,
        uri: &str,
        headers: &[(&str, &str)],
        body: &str,
    ) -> Reply {
        let mut all: Vec<(&str, String)> = vec![("authorization", self.password(user))];
        all.extend(headers.iter().map(|(k, v)| (*k, (*v).to_owned())));
        self.raw(method, uri, &all, body).await
    }

    async fn put_ics(&self, user: &str, uri: &str, body: &str) -> Reply {
        self.dav(user, "PUT", uri, &[("content-type", "text/calendar")], body)
            .await
    }

    async fn list(&self, user: &str, start: i64, end: i64) -> Value {
        let reply = self
            .api(
                user,
                "GET",
                &format!("/v1/events?start={start}&end={end}"),
                None,
            )
            .await;
        assert_eq!(reply.status, 200, "{reply:?}");
        reply.json()
    }

    async fn create(&self, user: &str, calendar: &str, body: &Value) -> Value {
        let reply = self
            .api(
                user,
                "POST",
                &format!("/v1/calendars/{calendar}/events"),
                Some(body),
            )
            .await;
        assert_eq!(reply.status, 201, "{reply:?}");
        reply.json()
    }

    /// Shares alice's default calendar with bob and lets him accept;
    /// returns bob's slug of it.
    async fn share(&self, access: &str) -> String {
        let reply = self
            .api(
                "alice",
                "PUT",
                "/v1/collections/calendar/default/shares/bob",
                Some(&json!({"access": access})),
            )
            .await;
        assert!(matches!(reply.status, 200 | 201), "{reply:?}");
        let shared = self.api("bob", "GET", "/v1/shared", None).await.json();
        let id = shared
            .as_array()
            .unwrap()
            .iter()
            .find(|s| s["accepted"] == false)
            .unwrap()["id"]
            .as_str()
            .unwrap()
            .to_owned();
        let reply = self
            .api("bob", "POST", &format!("/v1/shared/{id}/accept"), None)
            .await;
        assert_eq!(reply.status, 200, "{reply:?}");
        let href = reply.json()["href"].as_str().unwrap().to_owned();
        href.trim_end_matches('/')
            .rsplit('/')
            .next()
            .unwrap()
            .to_owned()
    }
}

fn instances(list: &Value) -> &Vec<Value> {
    list["instances"].as_array().unwrap()
}

#[tokio::test]
async fn create_list_read_change_delete() {
    let fx = fixture().await;
    let calendars = fx.api("alice", "GET", "/v1/calendars", None).await.json();
    assert_eq!(calendars[0]["slug"], "default");
    assert_eq!(calendars[0]["source"], "own");
    assert_eq!(calendars[0]["writable"], true);

    let saved = fx
        .create(
            "alice",
            "default",
            &timed("Planning", "2026-10-05", "09:00", "10:00"),
        )
        .await;
    let name = saved["name"].as_str().unwrap().to_owned();
    let etag = saved["etag"].as_str().unwrap().to_owned();
    // A real DAV object, visible to DAV clients with the same ETag.
    let dav = fx
        .dav("alice", "GET", &format!("{ALICE_CAL}{name}"), &[], "")
        .await;
    assert_eq!(dav.status, 200);
    assert_eq!(dav.header("etag"), Some(etag.as_str()));
    assert!(dav.body.contains("SUMMARY:Planning"), "{}", dav.body);
    assert!(dav.body.contains("TRIGGER:-PT10M"), "{}", dav.body);

    let list = fx.list("alice", OCT_1, OCT_1 + 31 * DAY).await;
    let first = &instances(&list)[0];
    assert_eq!(first["summary"], "Planning");
    assert_eq!(first["start"], OCT_1 + 4 * DAY + 9 * 3600);
    assert_eq!(first["end"], OCT_1 + 4 * DAY + 10 * 3600);
    assert_eq!(first["all_day"], false);
    assert_eq!(first["writable"], true);
    assert_eq!(first["etag"], etag.as_str());
    // Outside the range: nothing.
    assert!(instances(&fx.list("alice", OCT_1 + 10 * DAY, OCT_1 + 11 * DAY).await).is_empty());

    let path = format!("/v1/calendars/default/events/{name}");
    let detail = fx.api("alice", "GET", &path, None).await.json();
    assert_eq!(detail["fields"]["location"], "Room 4");
    assert_eq!(detail["fields"]["start"]["tzid"], "UTC");
    assert_eq!(detail["writable"], true);

    // Changes need the current ETag.
    let change = timed("Planning v2", "2026-10-05", "11:00", "12:00");
    let reply = fx.api("alice", "PUT", &path, Some(&change)).await;
    assert_eq!(reply.status, 400, "no If-Match: {reply:?}");
    let reply = fx
        .api_with("alice", "PUT", &path, Some("\"stale\""), Some(&change))
        .await;
    assert_eq!(reply.status, 412, "{reply:?}");
    assert_eq!(reply.json()["error"], "precondition_failed");
    let reply = fx
        .api_with("alice", "PUT", &path, Some(&etag), Some(&change))
        .await;
    assert_eq!(reply.status, 200, "{reply:?}");
    let etag2 = reply.json()["etag"].as_str().unwrap().to_owned();
    assert_ne!(etag2, etag);
    // The old ETag no longer works (another tab's change).
    let reply = fx
        .api_with("alice", "PUT", &path, Some(&etag), Some(&change))
        .await;
    assert_eq!(reply.status, 412);

    // Drag and drop: one hour later.
    let start = OCT_1 + 4 * DAY + 12 * 3600;
    let reply = fx
        .api_with(
            "alice",
            "PATCH",
            &path,
            Some(&etag2),
            Some(&json!({"start": start, "end": start + 1800})),
        )
        .await;
    assert_eq!(reply.status, 200, "{reply:?}");
    let etag3 = reply.json()["etag"].as_str().unwrap().to_owned();
    let list = fx.list("alice", OCT_1, OCT_1 + 31 * DAY).await;
    assert_eq!(instances(&list)[0]["start"], start);
    assert_eq!(instances(&list)[0]["end"], start + 1800);

    // Without the CSRF header: refused before anything else.
    let reply = fx
        .raw(
            "DELETE",
            &format!("/api{path}"),
            &[
                (
                    "cookie",
                    format!("calcraft_session={}", fx.session("alice")),
                ),
                ("if-match", etag3.clone()),
            ],
            "",
        )
        .await;
    assert_eq!(reply.status, 403);

    // Delete: into the trash, like a DAV DELETE.
    let reply = fx
        .api_with("alice", "DELETE", &path, Some(&etag3), None)
        .await;
    assert_eq!(reply.status, 204, "{reply:?}");
    assert!(instances(&fx.list("alice", OCT_1, OCT_1 + 31 * DAY).await).is_empty());
    let trash = fx.api("alice", "GET", "/v1/trash", None).await.json();
    assert_eq!(trash["entries"][0]["summary"], "Planning v2", "{trash}");
    assert_eq!(fx.api("alice", "GET", &path, None).await.status, 404);
}

#[tokio::test]
async fn recurring_series_instances_and_overrides() {
    let fx = fixture().await;
    let mut weekly = timed("Standup", "2026-10-05", "09:00", "09:15");
    weekly["recurrence"] = json!({"freq": "daily", "count": 5});
    let saved = fx.create("alice", "default", &weekly).await;
    let name = saved["name"].as_str().unwrap().to_owned();
    let path = format!("/v1/calendars/default/events/{name}");

    let list = fx.list("alice", OCT_1, OCT_1 + 31 * DAY).await;
    let all = instances(&list);
    assert_eq!(all.len(), 5);
    assert!(all.iter().all(|i| i["recurring"] == true));
    assert_eq!(all[2]["recurrence_id"], "20261007T090000Z");

    // Change only the third instance.
    let etag = all[2]["etag"].as_str().unwrap().to_owned();
    let mut one = timed("Late standup", "2026-10-07", "15:00", "15:30");
    one["recurrence_id"] = json!("20261007T090000Z");
    let reply = fx
        .api_with("alice", "PUT", &path, Some(&etag), Some(&one))
        .await;
    assert_eq!(reply.status, 200, "{reply:?}");
    let etag = reply.json()["etag"].as_str().unwrap().to_owned();
    let detail = fx
        .api(
            "alice",
            "GET",
            &format!("{path}?recurrence_id=20261007T090000Z"),
            None,
        )
        .await
        .json();
    assert_eq!(detail["is_override"], true);
    assert_eq!(detail["fields"]["summary"], "Late standup");
    assert_eq!(detail["recurrence"]["freq"], "daily");
    let list = fx.list("alice", OCT_1, OCT_1 + 31 * DAY).await;
    let summaries: Vec<&str> = instances(&list)
        .iter()
        .map(|i| i["summary"].as_str().unwrap())
        .collect();
    assert_eq!(
        summaries,
        ["Standup", "Standup", "Late standup", "Standup", "Standup"]
    );

    // Delete the second instance only.
    let reply = fx
        .api_with(
            "alice",
            "DELETE",
            &format!("{path}?recurrence_id=20261006T090000Z"),
            Some(&etag),
            None,
        )
        .await;
    assert_eq!(reply.status, 200, "{reply:?}");
    let list = fx.list("alice", OCT_1, OCT_1 + 31 * DAY).await;
    assert_eq!(instances(&list).len(), 4);
    // An instance the series does not have.
    let etag = instances(&list)[0]["etag"].as_str().unwrap().to_owned();
    let reply = fx
        .api_with(
            "alice",
            "DELETE",
            &format!("{path}?recurrence_id=20261020T090000Z"),
            Some(&etag),
            None,
        )
        .await;
    assert_eq!(reply.status, 404);

    // Invalid ranges.
    for query in ["start=10&end=5", "start=0&end=100000000", "start=x&end=1"] {
        let reply = fx
            .api("alice", "GET", &format!("/v1/events?{query}"), None)
            .await;
        assert_eq!(reply.status, 400, "{query}");
    }
}

#[tokio::test]
async fn foreign_events_are_not_found() {
    let fx = fixture().await;
    let saved = fx
        .create(
            "alice",
            "default",
            &timed("Secret", "2026-10-05", "09:00", "10:00"),
        )
        .await;
    let name = saved["name"].as_str().unwrap().to_owned();
    let etag = saved["etag"].as_str().unwrap().to_owned();
    // "default" in bob's home is bob's own calendar: alice's object is not there.
    let path = format!("/v1/calendars/default/events/{name}");
    assert_eq!(fx.api("bob", "GET", &path, None).await.status, 404);
    let change = timed("Mine now", "2026-10-05", "09:00", "10:00");
    for method in ["PUT", "DELETE"] {
        let reply = fx
            .api_with("bob", method, &path, Some(&etag), Some(&change))
            .await;
        assert_eq!(reply.status, 404, "{method}");
    }
    assert_eq!(
        fx.api(
            "bob",
            "GET",
            "/v1/calendars/alice-default/events/x.ics",
            None
        )
        .await
        .status,
        404
    );
    assert!(instances(&fx.list("bob", OCT_1, OCT_1 + 31 * DAY).await).is_empty());
    // Path tricks do not resolve.
    for bad in ["..%2Fx.ics", ".hidden", "a%00b.ics"] {
        let reply = fx
            .api(
                "alice",
                "GET",
                &format!("/v1/calendars/default/events/{bad}"),
                None,
            )
            .await;
        assert_eq!(reply.status, 404, "{bad}");
    }
    // Without a session.
    let reply = fx.raw("GET", "/api/v1/calendars", &[], "").await;
    assert_eq!(reply.status, 401);
}

#[tokio::test]
async fn read_share_shows_private_events_as_busy_and_refuses_changes() {
    let fx = fixture().await;
    fx.put_ics(
        "alice",
        &format!("{ALICE_CAL}public.ics"),
        &ics(&vevent(
            "pub",
            "DTSTART:20261005T090000Z\r\nDTEND:20261005T100000Z\r\nSUMMARY:Lunch\r\nLOCATION:Cafe\r\n",
        )),
    )
    .await;
    fx.put_ics(
        "alice",
        &format!("{ALICE_CAL}private.ics"),
        &ics(&vevent(
            "priv",
            "DTSTART:20261006T090000Z\r\nDTEND:20261006T100000Z\r\nSUMMARY:Doctor\r\n\
             LOCATION:Clinic\r\nDESCRIPTION:Very private\r\nCLASS:PRIVATE\r\n",
        )),
    )
    .await;
    let slug = fx.share("read").await;
    let calendars = fx.api("bob", "GET", "/v1/calendars", None).await.json();
    let shared = calendars
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["slug"] == slug.as_str())
        .unwrap();
    assert_eq!(shared["source"], "shared");
    assert_eq!(shared["writable"], false);
    assert_eq!(shared["owner"], "alice");
    assert_eq!(shared["private_hidden"], true);

    let list = fx.list("bob", OCT_1, OCT_1 + 31 * DAY).await;
    let all = instances(&list);
    assert_eq!(all.len(), 2);
    assert_eq!(all[0]["summary"], "Lunch");
    assert_eq!(all[0]["writable"], false);
    assert_eq!(all[1]["busy"], true);
    assert_eq!(all[1]["summary"], "Busy");
    assert_eq!(all[1]["location"], Value::Null);
    let text = list.to_string();
    for secret in ["Doctor", "Clinic", "Very private"] {
        assert!(!text.contains(secret), "{secret} leaked: {text}");
    }
    let detail = fx
        .api(
            "bob",
            "GET",
            &format!("/v1/calendars/{slug}/events/private.ics"),
            None,
        )
        .await;
    assert_eq!(detail.status, 200);
    for secret in ["Doctor", "Clinic", "Very private"] {
        assert!(
            !detail.body.contains(secret),
            "{secret} leaked: {}",
            detail.body
        );
    }
    assert_eq!(detail.json()["busy"], true);
    assert_eq!(detail.json()["writable"], false);

    // No change of any kind; the owner's objects stay as they are.
    let etag = all[0]["etag"].as_str().unwrap().to_owned();
    let path = format!("/v1/calendars/{slug}/events/public.ics");
    let change = timed("Hijacked", "2026-10-05", "09:00", "10:00");
    let reply = fx
        .api_with("bob", "PUT", &path, Some(&etag), Some(&change))
        .await;
    assert_eq!(reply.status, 403, "{reply:?}");
    let reply = fx
        .api_with(
            "bob",
            "PATCH",
            &path,
            Some(&etag),
            Some(&json!({"start": 0, "end": 60})),
        )
        .await;
    assert_eq!(reply.status, 403);
    let reply = fx.api_with("bob", "DELETE", &path, Some(&etag), None).await;
    assert_eq!(reply.status, 403);
    let reply = fx
        .api(
            "bob",
            "POST",
            &format!("/v1/calendars/{slug}/events"),
            Some(&change),
        )
        .await;
    assert_eq!(reply.status, 403);
    let reply = fx
        .api_with(
            "bob",
            "POST",
            &format!("{path}/move"),
            Some(&etag),
            Some(&json!({"calendar": "default"})),
        )
        .await;
    assert_eq!(reply.status, 403);
    let own = fx
        .dav("alice", "GET", &format!("{ALICE_CAL}public.ics"), &[], "")
        .await;
    assert!(own.body.contains("SUMMARY:Lunch"));
}

#[tokio::test]
async fn read_write_share_writes_like_dav() {
    let fx = fixture_with(config(true)).await;
    let slug = fx.share("read-write").await;
    let saved = fx
        .create(
            "bob",
            &slug,
            &timed("Bob's entry", "2026-10-05", "09:00", "10:00"),
        )
        .await;
    let name = saved["name"].as_str().unwrap();
    // Stored in alice's calendar.
    let own = fx
        .dav("alice", "GET", &format!("{ALICE_CAL}{name}"), &[], "")
        .await;
    assert!(own.body.contains("SUMMARY:Bob's entry"), "{}", own.body);

    // Alice's meeting is a scheduling object: sharees may not change it
    // (as over DAV).
    let meeting = ics(&vevent(
        "meet",
        "DTSTART:20261007T090000Z\r\nDURATION:PT1H\r\nSUMMARY:Meeting\r\n\
         ORGANIZER:mailto:alice@example.org\r\nATTENDEE;PARTSTAT=ACCEPTED:mailto:alice@example.org\r\n\
         ATTENDEE;PARTSTAT=NEEDS-ACTION;RSVP=TRUE:mailto:carol@example.org\r\n",
    ));
    assert_eq!(
        fx.put_ics("alice", &format!("{ALICE_CAL}meet.ics"), &meeting)
            .await
            .status,
        201
    );
    let path = format!("/v1/calendars/{slug}/events/meet.ics");
    let detail = fx.api("bob", "GET", &path, None).await.json();
    assert_eq!(detail["writable"], false);
    let etag = detail["etag"].as_str().unwrap().to_owned();
    let reply = fx
        .api_with(
            "bob",
            "PUT",
            &path,
            Some(&etag),
            Some(&timed("Mine", "2026-10-07", "09:00", "10:00")),
        )
        .await;
    assert_eq!(reply.status, 403, "{reply:?}");
    let reply = fx.api_with("bob", "DELETE", &path, Some(&etag), None).await;
    assert_eq!(reply.status, 403, "{reply:?}");
}

#[tokio::test]
async fn scheduling_messages_are_those_of_dav() {
    let fx = fixture_with(config(true)).await;
    let meeting = ics(&vevent(
        "m1",
        "DTSTART:20261007T090000Z\r\nDURATION:PT1H\r\nSUMMARY:Planning\r\n\
         ORGANIZER:mailto:alice@example.org\r\nATTENDEE;PARTSTAT=ACCEPTED:mailto:alice@example.org\r\n\
         ATTENDEE;PARTSTAT=NEEDS-ACTION;RSVP=TRUE:mailto:bob@example.org\r\n",
    ));
    let reply = fx
        .put_ics("alice", &format!("{ALICE_CAL}m1.ics"), &meeting)
        .await;
    assert_eq!(reply.status, 201);
    let invitations = fx.store.inbox_messages(fx.bob.id, 100).await.unwrap().len();
    assert_eq!(invitations, 1);

    // The organizer moves the meeting in the web UI: bob gets the update.
    let path = "/v1/calendars/default/events/m1.ics";
    let detail = fx.api("alice", "GET", path, None).await.json();
    assert_eq!(detail["attendees"].as_array().unwrap().len(), 2);
    assert_eq!(detail["organizer"]["address"], "alice@example.org");
    let etag = detail["etag"].as_str().unwrap().to_owned();
    let start = OCT_1 + 7 * DAY + 14 * 3600;
    let reply = fx
        .api_with(
            "alice",
            "PATCH",
            path,
            Some(&etag),
            Some(&json!({"start": start, "end": start + 3600})),
        )
        .await;
    assert_eq!(reply.status, 200, "{reply:?}");
    let mut texts = Vec::new();
    for meta in fx.store.inbox_messages(fx.bob.id, 100).await.unwrap() {
        let message = fx
            .store
            .inbox_message(fx.bob.id, &meta.name)
            .await
            .unwrap()
            .unwrap();
        texts.push(String::from_utf8(message.content).unwrap());
    }
    assert!(
        texts
            .iter()
            .any(|t| t.contains("METHOD:REQUEST") && t.contains("DTSTART:20261008T140000Z")),
        "an update was sent: {texts:?}"
    );
    let copy = fx
        .store
        .scheduling_object(fx.bob.id, "m1")
        .await
        .unwrap()
        .unwrap();
    let copy = String::from_utf8(copy.content).unwrap();
    assert!(copy.contains("DTSTART:20261008T140000Z"), "{copy}");
    // The attendees are kept.
    let organizer = fx
        .dav("alice", "GET", &format!("{ALICE_CAL}m1.ics"), &[], "")
        .await;
    assert!(
        organizer
            .body
            .replace("\r\n ", "")
            .contains("mailto:bob@example.org")
    );

    // Deleting it in the web UI cancels it for bob.
    let etag = fx.api("alice", "GET", path, None).await.json()["etag"]
        .as_str()
        .unwrap()
        .to_owned();
    let reply = fx
        .api_with("alice", "DELETE", path, Some(&etag), None)
        .await;
    assert_eq!(reply.status, 204, "{reply:?}");
    let messages = fx.store.inbox_messages(fx.bob.id, 100).await.unwrap();
    let last = fx
        .store
        .inbox_message(fx.bob.id, &messages.last().unwrap().name)
        .await
        .unwrap()
        .unwrap();
    assert!(
        String::from_utf8(last.content)
            .unwrap()
            .contains("METHOD:CANCEL")
    );
}

#[tokio::test]
async fn quota_subscriptions_and_moves() {
    let fx = fixture().await;
    // A second calendar to move to.
    let reply = fx
        .api(
            "alice",
            "POST",
            "/v1/collections",
            Some(&json!({"kind": "calendar", "slug": "work"})),
        )
        .await;
    assert_eq!(reply.status, 201, "{reply:?}");
    let saved = fx
        .create(
            "alice",
            "default",
            &timed("Move me", "2026-10-05", "09:00", "10:00"),
        )
        .await;
    let name = saved["name"].as_str().unwrap().to_owned();
    let etag = saved["etag"].as_str().unwrap().to_owned();
    let path = format!("/v1/calendars/default/events/{name}");
    let reply = fx
        .api_with(
            "alice",
            "POST",
            &format!("{path}/move"),
            Some(&etag),
            Some(&json!({"calendar": "nowhere"})),
        )
        .await;
    assert_eq!(reply.status, 404);
    let reply = fx
        .api_with(
            "alice",
            "POST",
            &format!("{path}/move"),
            Some(&etag),
            Some(&json!({"calendar": "work"})),
        )
        .await;
    assert_eq!(reply.status, 200, "{reply:?}");
    assert_eq!(reply.json()["calendar"], "work");
    let list = fx.list("alice", OCT_1, OCT_1 + 31 * DAY).await;
    assert_eq!(instances(&list)[0]["calendar"], "work");
    assert_eq!(fx.api("alice", "GET", &path, None).await.status, 404);

    // A subscription (read-only calendar): listed, never written.
    let work = fx
        .store
        .collection(
            fx.alice.id,
            CollectionKind::Calendar,
            &"work".parse().unwrap(),
        )
        .await
        .unwrap()
        .unwrap();
    fx.store.mark_read_only_for_tests(work.id).await.unwrap();
    let calendars = fx.api("alice", "GET", "/v1/calendars", None).await.json();
    let work_view = calendars
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["slug"] == "work")
        .unwrap();
    assert_eq!(work_view["source"], "subscription");
    assert_eq!(work_view["writable"], false);
    let moved = format!("/v1/calendars/work/events/{name}");
    let etag = fx.api("alice", "GET", &moved, None).await.json()["etag"]
        .as_str()
        .unwrap()
        .to_owned();
    let reply = fx
        .api_with(
            "alice",
            "PUT",
            &moved,
            Some(&etag),
            Some(&timed("x", "2026-10-05", "09:00", "10:00")),
        )
        .await;
    assert_eq!(reply.status, 403);
    let reply = fx
        .api(
            "alice",
            "POST",
            "/v1/calendars/work/events",
            Some(&timed("x", "2026-10-05", "09:00", "10:00")),
        )
        .await;
    assert_eq!(reply.status, 403);

    // Quota: refused like a DAV PUT.
    fx.store.set_quota_for_tests(100);
    let reply = fx
        .api(
            "alice",
            "POST",
            "/v1/calendars/default/events",
            Some(&timed("Too big", "2026-10-05", "09:00", "10:00")),
        )
        .await;
    assert_eq!(reply.status, 507, "{reply:?}");
    assert_eq!(reply.json()["error"], "limit");
}

#[tokio::test]
async fn invalid_input_is_refused() {
    let fx = fixture().await;
    for body in [
        timed("Backwards", "2026-10-05", "10:00", "09:00"),
        input(
            "Bad date",
            time("2026-02-30", None, None),
            time("2026-03-01", None, None),
        ),
        input(
            "Unknown zone",
            time("2026-10-05", Some("09:00"), Some("Mars/Base")),
            time("2026-10-05", Some("10:00"), Some("Mars/Base")),
        ),
        json!({"fields": {"start": time("2026-10-05", None, None), "end": time("2026-10-06", None, None)}, "unknown": 1}),
    ] {
        let reply = fx
            .api("alice", "POST", "/v1/calendars/default/events", Some(&body))
            .await;
        assert_eq!(reply.status, 400, "{body}: {reply:?}");
    }
    // A zone the browser defined.
    let mut zoned = input(
        "Berlin",
        time("2026-10-05", Some("09:00"), Some("Europe/Berlin")),
        time("2026-10-05", Some("10:00"), Some("Europe/Berlin")),
    );
    zoned["timezones"] = json!([{
        "tzid": "Europe/Berlin",
        "observances": [
            {"daylight": true, "start": "19700329T020000", "offset_from": "+0100", "offset_to": "+0200", "rrule": "FREQ=YEARLY;BYMONTH=3;BYDAY=-1SU", "name": "CEST"},
            {"daylight": false, "start": "19701025T030000", "offset_from": "+0200", "offset_to": "+0100", "rrule": "FREQ=YEARLY;BYMONTH=10;BYDAY=-1SU", "name": "CET"}
        ]
    }]);
    fx.create("alice", "default", &zoned).await;
    let list = fx.list("alice", OCT_1, OCT_1 + 31 * DAY).await;
    // 09:00 CEST is 07:00 UTC.
    assert_eq!(instances(&list)[0]["start"], OCT_1 + 4 * DAY + 7 * 3600);
    assert_eq!(fx.alice.username.as_str(), "alice");
}
