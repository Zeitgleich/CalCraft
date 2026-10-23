//! Sharing calendars and address books between users (docs/sharing.md):
//! read and read-write enforcement for every DAV method, the Apple
//! (CalendarServer) share and invite-reply flow, the REST API, acceptance,
//! declining, unsubscribing, revocation (afterwards indistinguishable from
//! a path that never existed), disabled users, quota and trash of the
//! owner, scheduling and attachment rules, and a share removed while the
//! sharee writes. The authorization matrix (`authz_matrix.rs`) covers the
//! same shares against every method and credential.

// The shares and links are set up through the REST API.
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
use tower::ServiceExt;

const NS: &str = r#"xmlns:D="DAV:" xmlns:C="urn:ietf:params:xml:ns:caldav" xmlns:CS="http://calendarserver.org/ns/" xmlns:I="http://apple.com/ns/ical/""#;
const ALICE_CAL: &str = "/dav/calendars/alice/default/";
const ATTACH_BASE: &str = "https://cal.example.org";

/// Method, path, headers, body and expected status of one request.
type Case<'a> = (&'a str, &'a str, Vec<(&'a str, &'a str)>, String, u16);
/// Method, path, headers and body.
type DavRequest = (
    &'static str,
    String,
    Vec<(&'static str, &'static str)>,
    String,
);

struct Fx {
    app: Router,
    store: Store,
    alice: User,
    bob: User,
    carol: User,
    /// App passwords (`ccap_…`), cheap to check.
    passwords: Vec<(String, String)>,
    sessions: Vec<(String, String)>,
}

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

fn config(scheduling: bool, attachments: bool) -> Config {
    let mut config = Config::default();
    config.server.max_concurrent_requests_per_client = 4096;
    config.server.client_rate_burst = u32::MAX;
    config.server.client_rate_per_second = u32::MAX;
    if scheduling {
        config.scheduling.enabled = true;
        config.scheduling.domain = Some("example.org".to_owned());
    }
    if attachments {
        config.attachments.enabled = true;
        config.attachments.base_url = Some(ATTACH_BASE.to_owned());
    }
    config
}

async fn fixture_with(config: Config) -> Fx {
    let store = Store::in_memory().await.unwrap();
    let mut users = Vec::new();
    let mut passwords = Vec::new();
    let mut sessions = Vec::new();
    for name in ["alice", "bob", "carol"] {
        let user = store
            .create_user(
                &name.parse().unwrap(),
                "unused",
                Some(&format!("{name} person")),
            )
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
        carol: users.next().unwrap(),
        passwords,
        sessions,
    }
}

async fn fixture() -> Fx {
    fixture_with(config(false, false)).await
}

/// The hrefs of the invitations in a notification listing.
fn notifications(body: &str) -> Vec<String> {
    body.split("<d:href>")
        .skip(1)
        .filter_map(|rest| rest.split_once("</d:href>").map(|(href, _)| href))
        .filter(|href| href.ends_with(".xml"))
        .map(str::to_owned)
        .collect()
}

fn event(uid: &str, summary: &str, extra: &str) -> String {
    format!(
        "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//Test//EN\r\nBEGIN:VEVENT\r\nUID:{uid}\r\nDTSTAMP:20261001T000000Z\r\nDTSTART:20261021T100000Z\r\nDTEND:20261021T110000Z\r\nSUMMARY:{summary}\r\n{extra}END:VEVENT\r\nEND:VCALENDAR\r\n"
    )
}

impl Fx {
    fn password(&self, user: &str) -> String {
        let (_, password) = self.passwords.iter().find(|(n, _)| n == user).unwrap();
        format!("Basic {}", STANDARD.encode(format!("{user}:{password}")))
    }

    fn session(&self, user: &str) -> &str {
        &self.sessions.iter().find(|(n, _)| n == user).unwrap().1
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

    /// A DAV request as `user`.
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

    /// A REST request with `user`'s session.
    async fn api(&self, user: &str, method: &str, uri: &str, body: Option<&str>) -> Reply {
        let mut headers: Vec<(&str, String)> = vec![
            ("cookie", format!("calcraft_session={}", self.session(user))),
            (CSRF_HEADER, "1".to_owned()),
        ];
        if body.is_some() {
            headers.push(("content-type", "application/json".to_owned()));
        }
        self.raw(method, uri, &headers, body.unwrap_or_default())
            .await
    }

    async fn put(&self, user: &str, uri: &str, body: &str) -> Reply {
        self.dav(user, "PUT", uri, &[("content-type", "text/calendar")], body)
            .await
    }

    async fn propfind(&self, user: &str, uri: &str, depth: &str) -> Reply {
        self.dav(user, "PROPFIND", uri, &[("depth", depth)], "")
            .await
    }

    /// Shares alice's default collection of `kind` with `sharee` through the
    /// REST API and lets them accept; returns the path in their home.
    async fn share(&self, kind: &str, sharee: &str, access: &str) -> String {
        let reply = self
            .api(
                "alice",
                "PUT",
                &format!("/api/v1/collections/{kind}/default/shares/{sharee}"),
                Some(&format!(r#"{{"access":"{access}"}}"#)),
            )
            .await;
        assert!(matches!(reply.status, 200 | 201), "{reply:?}");
        let id = self.pending_id(sharee).await;
        let reply = self
            .api(sharee, "POST", &format!("/api/v1/shared/{id}/accept"), None)
            .await;
        assert_eq!(reply.status, 200, "{reply:?}");
        let info: serde_json::Value = serde_json::from_str(&reply.body).unwrap();
        info["href"].as_str().unwrap().to_owned()
    }

    async fn pending_id(&self, sharee: &str) -> String {
        let reply = self.api(sharee, "GET", "/api/v1/shared", None).await;
        assert_eq!(reply.status, 200);
        let list: serde_json::Value = serde_json::from_str(&reply.body).unwrap();
        list.as_array()
            .unwrap()
            .iter()
            .find(|s| s["accepted"] == false)
            .unwrap()["id"]
            .as_str()
            .unwrap()
            .to_owned()
    }

    async fn unshare(&self, kind: &str, sharee: &str) {
        let reply = self
            .api(
                "alice",
                "DELETE",
                &format!("/api/v1/collections/{kind}/default/shares/{sharee}"),
                None,
            )
            .await;
        assert_eq!(reply.status, 204, "{reply:?}");
    }
}

// --- read shares --------------------------------------------------------

#[tokio::test]
async fn read_share_allows_reading_only() {
    let fx = fixture().await;
    assert_eq!(
        fx.put(
            "alice",
            &format!("{ALICE_CAL}a.ics"),
            &event("ua", "Alice's", "")
        )
        .await
        .status,
        201
    );
    let own = fx
        .dav("alice", "GET", &format!("{ALICE_CAL}a.ics"), &[], "")
        .await;

    // Pending: not in bob's home, not reachable.
    let reply = fx
        .api(
            "alice",
            "PUT",
            "/api/v1/collections/calendar/default/shares/bob",
            Some(r#"{"access":"read"}"#),
        )
        .await;
    assert_eq!(reply.status, 201, "{reply:?}");
    let home = fx.propfind("bob", "/dav/calendars/bob/", "1").await;
    assert!(!home.body.contains("alice-default"), "{}", home.body);
    assert_eq!(
        fx.propfind("bob", "/dav/calendars/bob/alice-default/", "0")
            .await
            .status,
        404
    );

    let id = fx.pending_id("bob").await;
    let reply = fx
        .api("bob", "POST", &format!("/api/v1/shared/{id}/accept"), None)
        .await;
    assert_eq!(reply.status, 200);
    let share = "/dav/calendars/bob/alice-default/";

    // Listed in bob's home with the owner, CS:shared and read privileges.
    let home = fx.propfind("bob", "/dav/calendars/bob/", "1").await;
    assert_eq!(home.status, 207);
    assert!(home.body.contains(share), "{}", home.body);
    let props = fx.propfind("bob", share, "0").await;
    assert!(props.body.contains("<cs:shared/>"), "{}", props.body);
    assert!(
        props
            .body
            .contains("<d:owner><d:href>/dav/principals/alice/</d:href></d:owner>"),
        "{}",
        props.body
    );
    assert!(props.body.contains("<d:privilege><d:read/></d:privilege>"));
    assert!(!props.body.contains("<d:write-content/>"), "{}", props.body);
    assert!(props.body.contains("<cs:organizer>"), "{}", props.body);
    // Alice's view shows bob as accepted sharee.
    let owner = fx.propfind("alice", ALICE_CAL, "0").await;
    assert!(owner.body.contains("<cs:shared-owner/>"), "{}", owner.body);
    assert!(
        owner.body.contains("/dav/principals/bob/"),
        "{}",
        owner.body
    );
    assert!(
        owner.body.contains("<cs:invite-accepted/>"),
        "{}",
        owner.body
    );

    // Same object, same ETag.
    let get = fx
        .dav("bob", "GET", &format!("{share}a.ics"), &[], "")
        .await;
    assert_eq!(get.status, 200);
    assert_eq!(get.body, own.body);
    assert_eq!(get.header("etag"), own.header("etag"));

    // Reports: multiget with the share's hrefs, sync-collection with the
    // owner's token.
    let multiget = format!(
        "<C:calendar-multiget {NS}><D:prop><D:getetag/><C:calendar-data/></D:prop><D:href>{share}a.ics</D:href></C:calendar-multiget>"
    );
    let reply = fx
        .dav("bob", "REPORT", share, &[("depth", "1")], &multiget)
        .await;
    assert_eq!(reply.status, 207);
    assert!(
        reply.body.contains("Alice&apos;s") || reply.body.contains("Alice's"),
        "{}",
        reply.body
    );
    let sync = format!(
        "<D:sync-collection {NS}><D:sync-token/><D:sync-level>1</D:sync-level><D:prop><D:getetag/></D:prop></D:sync-collection>"
    );
    let theirs = fx.dav("bob", "REPORT", share, &[], &sync).await;
    let mine = fx.dav("alice", "REPORT", ALICE_CAL, &[], &sync).await;
    assert_eq!(theirs.status, 207);
    let token = |body: &str| {
        let start = body.rfind("<d:sync-token>").unwrap();
        body[start..].to_owned()
    };
    assert_eq!(token(&theirs.body), token(&mine.body));
    assert!(theirs.body.contains(&format!("{share}a.ics")));

    // Every write is refused, and nothing changes for alice.
    let before = fx.propfind("alice", ALICE_CAL, "1").await;
    let item = format!("{share}a.ics");
    let new_item = format!("{share}new.ics");
    let cases: Vec<Case<'_>> = vec![
        (
            "PUT",
            &item,
            vec![("content-type", "text/calendar")],
            event("ua", "x", ""),
            403,
        ),
        (
            "PUT",
            &new_item,
            vec![("content-type", "text/calendar")],
            event("un", "x", ""),
            403,
        ),
        ("DELETE", &item, vec![], String::new(), 403),
        (
            "PROPPATCH",
            share,
            vec![],
            format!(
                "<D:propertyupdate {NS}><D:set><D:prop><C:calendar-description>x</C:calendar-description></D:prop></D:set></D:propertyupdate>"
            ),
            207,
        ),
        ("MKCALENDAR", share, vec![], String::new(), 405),
        ("MKCOL", share, vec![], String::new(), 405),
        (
            "MOVE",
            &item,
            vec![("destination", "/dav/calendars/bob/default/m.ics")],
            String::new(),
            403,
        ),
        (
            "MOVE",
            &item,
            vec![("destination", "/dav/calendars/bob/alice-default/m.ics")],
            String::new(),
            403,
        ),
        (
            "COPY",
            &item,
            vec![("destination", "/dav/calendars/bob/default/c.ics")],
            String::new(),
            403,
        ),
        (
            "COPY",
            &item,
            vec![("destination", "/dav/calendars/bob/alice-default/c.ics")],
            String::new(),
            403,
        ),
        (
            "COPY",
            "/dav/calendars/bob/default/",
            vec![("destination", "/dav/calendars/bob/alice-default/c.ics")],
            String::new(),
            403,
        ),
        ("LOCK", &item, vec![], String::new(), 405),
        ("ACL", share, vec![], String::new(), 405),
        (
            "POST",
            share,
            vec![("content-type", "application/xml")],
            format!(
                "<CS:share {NS}><CS:set><D:href>/dav/principals/carol/</D:href><CS:read/></CS:set></CS:share>"
            ),
            403,
        ),
    ];
    for (method, uri, headers, body, status) in &cases {
        let reply = fx.dav("bob", method, uri, headers, body).await;
        assert_eq!(reply.status, *status, "{method} {uri}: {}", reply.body);
        if *method == "PROPPATCH" {
            assert!(reply.body.contains("403"), "{}", reply.body);
        }
    }
    // COPY of bob's own object into the read share: refused (bind).
    fx.put(
        "bob",
        "/dav/calendars/bob/default/b.ics",
        &event("ub", "b", ""),
    )
    .await;
    let reply = fx
        .dav(
            "bob",
            "COPY",
            "/dav/calendars/bob/default/b.ics",
            &[("destination", "/dav/calendars/bob/alice-default/b.ics")],
            "",
        )
        .await;
    assert_eq!(reply.status, 403);
    assert_eq!(fx.propfind("alice", ALICE_CAL, "1").await, before);
    assert!(
        fx.store
            .shares_of_collection(
                fx.store
                    .collections(fx.alice.id, CollectionKind::Calendar)
                    .await
                    .unwrap()[0]
                    .id
            )
            .await
            .unwrap()
            .iter()
            .all(|s| s.sharee_id != fx.carol.id),
        "a sharee cannot re-share"
    );

    // Bob's own name and colour, for himself only.
    let patch = format!(
        "<D:propertyupdate {NS}><D:set><D:prop><D:displayname>Alice (shared)</D:displayname><I:calendar-color>#00FF00</I:calendar-color><I:calendar-order>4</I:calendar-order></D:prop></D:set></D:propertyupdate>"
    );
    let reply = fx.dav("bob", "PROPPATCH", share, &[], &patch).await;
    assert_eq!(reply.status, 207);
    assert!(
        reply.body.contains("200 OK") && !reply.body.contains("403"),
        "{}",
        reply.body
    );
    let props = fx.propfind("bob", share, "0").await;
    assert!(props.body.contains("Alice (shared)"), "{}", props.body);
    assert!(props.body.contains("#00FF00"));
    assert!(props.body.contains(">4<"), "{}", props.body);
    assert_eq!(fx.propfind("alice", ALICE_CAL, "1").await, before);
    // Carol sees nothing of it.
    assert_eq!(fx.propfind("carol", share, "0").await.status, 404);
    assert_eq!(
        fx.propfind("carol", "/dav/calendars/carol/alice-default/", "0")
            .await
            .status,
        404
    );
}

#[tokio::test]
async fn read_write_share_writes_to_the_owner() {
    let fx = fixture().await;
    let share = fx.share("calendar", "bob", "read-write").await;
    assert_eq!(share, "/dav/calendars/bob/alice-default/");
    let props = fx.propfind("bob", &share, "0").await;
    assert!(props.body.contains("<d:write-content/>"), "{}", props.body);

    // Bob's PUT is alice's object with the same ETag.
    let reply = fx
        .put("bob", &format!("{share}b.ics"), &event("ub", "By bob", ""))
        .await;
    assert_eq!(reply.status, 201, "{}", reply.body);
    let mine = fx
        .dav("alice", "GET", &format!("{ALICE_CAL}b.ics"), &[], "")
        .await;
    assert_eq!(mine.status, 200);
    assert_eq!(mine.header("etag"), reply.header("etag"));
    // Conditional writes work as usual.
    let etag = reply.header("etag").unwrap().to_owned();
    let stale = fx
        .dav(
            "bob",
            "PUT",
            &format!("{share}b.ics"),
            &[("content-type", "text/calendar"), ("if-match", "\"stale\"")],
            &event("ub", "x", ""),
        )
        .await;
    assert_eq!(stale.status, 412);
    let ok = fx
        .dav(
            "bob",
            "PUT",
            &format!("{share}b.ics"),
            &[("content-type", "text/calendar"), ("if-match", &etag)],
            &event("ub", "By bob 2", ""),
        )
        .await;
    assert_eq!(ok.status, 204);

    // MOVE within the share works; out of it or into bob's own is refused.
    let reply = fx
        .dav(
            "bob",
            "MOVE",
            &format!("{share}b.ics"),
            &[("destination", &format!("{share}b2.ics"))],
            "",
        )
        .await;
    assert_eq!(reply.status, 201, "{}", reply.body);
    for (method, destination) in [
        ("MOVE", "/dav/calendars/bob/default/m.ics"),
        ("COPY", "/dav/calendars/bob/default/m.ics"),
    ] {
        let reply = fx
            .dav(
                "bob",
                method,
                &format!("{share}b2.ics"),
                &[("destination", destination)],
                "",
            )
            .await;
        assert_eq!(reply.status, 403, "{method}");
    }
    // The owner's collection settings stay the owner's.
    let patch = format!(
        "<D:propertyupdate {NS}><D:set><D:prop><D:displayname>mine</D:displayname><C:calendar-timezone>x</C:calendar-timezone></D:prop></D:set></D:propertyupdate>"
    );
    let reply = fx.dav("bob", "PROPPATCH", &share, &[], &patch).await;
    assert!(
        reply.body.contains("403") && reply.body.contains("424"),
        "{}",
        reply.body
    );

    // DELETE goes into alice's trash.
    let reply = fx
        .dav("bob", "DELETE", &format!("{share}b2.ics"), &[], "")
        .await;
    assert_eq!(reply.status, 204);
    let trash = fx.store.trash_entries(fx.alice.id, 10, 0).await.unwrap();
    assert_eq!(trash.entries.len(), 1);
    assert!(
        fx.store
            .trash_entries(fx.bob.id, 10, 0)
            .await
            .unwrap()
            .entries
            .is_empty()
    );
    assert_eq!(
        fx.dav("bob", "DELETE", &format!("{share}b2.ics"), &[], "")
            .await
            .status,
        404
    );

    // Bob deletes the shared collection: he unsubscribes, alice keeps it.
    let reply = fx.dav("bob", "DELETE", &share, &[], "").await;
    assert_eq!(reply.status, 204);
    assert_eq!(fx.propfind("bob", &share, "0").await.status, 404);
    assert_eq!(fx.propfind("alice", ALICE_CAL, "0").await.status, 207);
    assert!(
        fx.store
            .shares_of_owner(fx.alice.id, CollectionKind::Calendar)
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn sharee_writes_count_against_the_owners_quota() {
    let fx = fixture().await;
    let share = fx.share("addressbook", "bob", "read-write").await;
    assert_eq!(share, "/dav/addressbooks/bob/alice-default/");
    let card = |uid: &str| {
        format!(
            "BEGIN:VCARD\r\nVERSION:4.0\r\nUID:{uid}\r\nFN:{}\r\nEND:VCARD\r\n",
            "x".repeat(2000)
        )
    };
    fx.store.set_quota_for_tests(5000);
    let put = |uri: String, body: String| {
        let fx = &fx;
        async move {
            fx.dav("bob", "PUT", &uri, &[("content-type", "text/vcard")], &body)
                .await
                .status
        }
    };
    assert_eq!(put(format!("{share}1.vcf"), card("1")).await, 201);
    assert_eq!(put(format!("{share}2.vcf"), card("2")).await, 201);
    // Alice is full now; bob's own address book is not.
    assert_eq!(put(format!("{share}3.vcf"), card("3")).await, 507);
    assert_eq!(
        put("/dav/addressbooks/bob/default/3.vcf".to_owned(), card("3")).await,
        201
    );
    fx.store.set_quota_for_tests(0);
}

// --- Apple sharing flow -------------------------------------------------

#[tokio::test]
async fn apple_share_invite_and_reply() {
    let fx = fixture_with(config(true, false)).await;
    fx.store
        .set_user_email(fx.bob.id, Some("bob@mail.example"))
        .await
        .unwrap();
    // Discovery: the DAV class and the owner's sharing properties.
    let options = fx.dav("alice", "OPTIONS", ALICE_CAL, &[], "").await;
    assert!(
        options
            .header("dav")
            .unwrap()
            .contains("calendarserver-sharing")
    );
    let props = fx.propfind("alice", ALICE_CAL, "0").await;
    assert!(props.body.contains("<cs:can-be-shared/>"), "{}", props.body);
    let principal = fx.propfind("bob", "/dav/principals/bob/", "0").await;
    assert!(
        principal.body.contains("/dav/notifications/bob/"),
        "{}",
        principal.body
    );

    // An unknown sharee refuses the whole request.
    let share = |set: &str| {
        format!(
            "<CS:share {NS}><CS:set><D:href>{set}</D:href><CS:summary>Team</CS:summary><CS:read-write/></CS:set></CS:share>"
        )
    };
    let xml = [("content-type", "text/xml; charset=utf-8")];
    for href in [
        "mailto:nobody@example.org",
        "mailto:alice@example.org",
        "/dav/principals/nobody/",
    ] {
        let reply = fx.dav("alice", "POST", ALICE_CAL, &xml, &share(href)).await;
        assert_eq!(reply.status, 403, "{href}");
        assert!(reply.body.contains("valid-sharee"));
    }
    // By the e-mail address set by the administrator.
    let reply = fx
        .dav(
            "alice",
            "POST",
            ALICE_CAL,
            &xml,
            &share("mailto:bob@mail.example"),
        )
        .await;
    assert_eq!(reply.status, 200, "{}", reply.body);
    // And carol by her principal URL, read-only.
    let reply = fx
        .dav(
            "alice",
            "POST",
            ALICE_CAL,
            &xml,
            &format!("<CS:share {NS}><CS:set><D:href>http://localhost/dav/principals/carol/</D:href><CS:read/></CS:set></CS:share>"),
        )
        .await;
    assert_eq!(reply.status, 200);
    let props = fx.propfind("alice", ALICE_CAL, "0").await;
    assert!(
        props.body.contains("<cs:invite-noresponse/>"),
        "{}",
        props.body
    );
    assert!(props.body.contains("<cs:read-write/>") && props.body.contains("<cs:read/>"));

    // Bob's notification collection has the invitation.
    let list = fx.propfind("bob", "/dav/notifications/bob/", "1").await;
    assert_eq!(list.status, 207);
    assert!(list.body.contains("<cs:notification/>"), "{}", list.body);
    assert!(list.body.contains("invite-notification"), "{}", list.body);
    let found = notifications(&list.body);
    assert_eq!(found.len(), 1, "{}", list.body);
    let notification = found[0].clone();
    assert!(notification.starts_with("/dav/notifications/bob/"));
    let body = fx.dav("bob", "GET", &notification, &[], "").await;
    assert_eq!(body.status, 200);
    assert!(
        body.body
            .contains("<cs:hosturl><d:href>/dav/calendars/alice/default/</d:href></cs:hosturl>"),
        "{}",
        body.body
    );
    assert!(body.body.contains("<cs:read-write/>"));
    let uid_start = body.body.find("<cs:uid>").unwrap() + 8;
    let uid = body.body[uid_start..uid_start + 32].to_owned();
    // Not visible to anybody else.
    assert_eq!(
        fx.dav("carol", "GET", &notification, &[], "").await.status,
        404
    );
    assert_eq!(
        fx.dav(
            "carol",
            "GET",
            &notification.replace("/bob/", "/carol/"),
            &[],
            ""
        )
        .await
        .status,
        404
    );

    // Bob accepts with an invite reply to his calendar home.
    let reply_body = format!(
        "<CS:invite-reply {NS}><D:href>mailto:bob@mail.example</D:href><CS:invite-accepted/><CS:hosturl><D:href>/dav/calendars/alice/default/</D:href></CS:hosturl><CS:in-reply-to>{uid}</CS:in-reply-to><CS:summary>Team</CS:summary></CS:invite-reply>"
    );
    let reply = fx
        .dav("bob", "POST", "/dav/calendars/bob/", &xml, &reply_body)
        .await;
    assert_eq!(reply.status, 200, "{}", reply.body);
    assert!(
        reply
            .body
            .contains("<d:href>/dav/calendars/bob/alice-default/</d:href></cs:shared-as>"),
        "{}",
        reply.body
    );
    // The invitation is gone, the calendar is there.
    let list = fx.propfind("bob", "/dav/notifications/bob/", "1").await;
    assert!(!list.body.contains(".xml"), "{}", list.body);
    assert_eq!(
        fx.dav("bob", "GET", &notification, &[], "").await.status,
        404
    );
    assert_eq!(
        fx.propfind("bob", "/dav/calendars/bob/alice-default/", "0")
            .await
            .status,
        207
    );
    // Carol answers to an invitation that is not hers: nothing happens.
    let reply = fx
        .dav("carol", "POST", "/dav/calendars/carol/", &xml, &reply_body)
        .await;
    assert_eq!(reply.status, 403);

    // Carol declines by hosturl only.
    let decline = format!(
        "<CS:invite-reply {NS}><CS:invite-declined/><CS:hosturl><D:href>http://localhost/dav/calendars/alice/default/</D:href></CS:hosturl></CS:invite-reply>"
    );
    let reply = fx
        .dav("carol", "POST", "/dav/calendars/carol/", &xml, &decline)
        .await;
    assert_eq!(reply.status, 204, "{}", reply.body);
    let props = fx.propfind("alice", ALICE_CAL, "0").await;
    assert!(
        !props.body.contains("/dav/principals/carol/"),
        "{}",
        props.body
    );

    // Alice removes bob again with CS:remove.
    let remove = format!(
        "<CS:share {NS}><CS:remove><D:href>mailto:bob@mail.example</D:href></CS:remove></CS:share>"
    );
    assert_eq!(
        fx.dav("alice", "POST", ALICE_CAL, &xml, &remove)
            .await
            .status,
        200
    );
    assert_eq!(
        fx.propfind("bob", "/dav/calendars/bob/alice-default/", "0")
            .await
            .status,
        404
    );

    // Declining by deleting the notification.
    fx.dav(
        "alice",
        "POST",
        ALICE_CAL,
        &xml,
        &share("/dav/principals/bob/"),
    )
    .await;
    let list = fx.propfind("bob", "/dav/notifications/bob/", "1").await;
    let notification = notifications(&list.body).remove(0);
    assert_eq!(
        fx.dav("bob", "DELETE", &notification, &[], "").await.status,
        204
    );
    assert!(
        fx.store
            .incoming_shares(fx.bob.id)
            .await
            .unwrap()
            .is_empty()
    );
    // Malformed sharing bodies.
    for body in ["<x/>", "not xml", &format!("<CS:share {NS}/>")] {
        assert_eq!(
            fx.dav("alice", "POST", ALICE_CAL, &xml, body).await.status,
            400
        );
    }
}

// --- revocation ---------------------------------------------------------

#[tokio::test]
async fn revoked_share_is_indistinguishable_from_nothing() {
    let fx = fixture_with(config(false, true)).await;
    fx.put("alice", &format!("{ALICE_CAL}a.ics"), &event("ua", "a", ""))
        .await;
    // An attachment on alice's event.
    let reply = fx
        .dav(
            "alice",
            "POST",
            &format!("{ALICE_CAL}a.ics?action=attachment-add"),
            &[
                ("content-type", "text/plain"),
                ("content-disposition", "attachment; filename=a.txt"),
            ],
            "secret file",
        )
        .await;
    assert_eq!(reply.status, 201, "{}", reply.body);
    let managed = reply.header("cal-managed-id").unwrap().to_owned();
    let attachment = format!("/dav/attachments/{managed}");
    let share = fx.share("calendar", "bob", "read").await;
    // While shared, bob reads the attachment.
    assert_eq!(fx.dav("bob", "GET", &attachment, &[], "").await.status, 200);

    let sync = format!(
        "<D:sync-collection {NS}><D:sync-token/><D:sync-level>1</D:sync-level><D:prop><D:getetag/></D:prop></D:sync-collection>"
    );
    let requests = |base: &str| -> Vec<DavRequest> {
        vec![
            (
                "PROPFIND",
                base.to_owned(),
                vec![("depth", "1")],
                String::new(),
            ),
            ("GET", format!("{base}a.ics"), vec![], String::new()),
            (
                "PUT",
                format!("{base}a.ics"),
                vec![("content-type", "text/calendar")],
                event("ua", "x", ""),
            ),
            ("DELETE", format!("{base}a.ics"), vec![], String::new()),
            ("REPORT", base.to_owned(), vec![], sync.clone()),
            (
                "PROPPATCH",
                base.to_owned(),
                vec![],
                format!(
                    "<D:propertyupdate {NS}><D:set><D:prop><D:displayname>x</D:displayname></D:prop></D:set></D:propertyupdate>"
                ),
            ),
            (
                "COPY",
                format!("{base}a.ics"),
                vec![("destination", "/dav/calendars/bob/default/x.ics")],
                String::new(),
            ),
        ]
    };
    fx.unshare("calendar", "bob").await;
    let never = "/dav/calendars/bob/never-existed/";
    for ((method, uri, headers, body), (_, never_uri, _, _)) in
        requests(&share).into_iter().zip(requests(never))
    {
        let revoked = fx.dav("bob", method, &uri, &headers, &body).await;
        let nothing = fx.dav("bob", method, &never_uri, &headers, &body).await;
        assert_eq!(revoked.status, nothing.status, "{method} {uri}");
        assert_eq!(
            revoked.body.replace(&share, never),
            nothing.body,
            "{method} {uri}"
        );
        assert_ne!(revoked.status / 100, 2, "{method} {uri}: {}", revoked.body);
    }
    // The attachment too: like an id that was never issued.
    let revoked = fx.dav("bob", "GET", &attachment, &[], "").await;
    let missing = fx
        .dav(
            "bob",
            "GET",
            "/dav/attachments/0123456789abcdef0123456789abcdef",
            &[],
            "",
        )
        .await;
    assert_eq!(revoked, missing);
    // Not in bob's home any more.
    let home = fx.propfind("bob", "/dav/calendars/bob/", "1").await;
    assert!(!home.body.contains("alice-default"));
    // Alice's data is untouched.
    assert_eq!(
        fx.dav("alice", "GET", &format!("{ALICE_CAL}a.ics"), &[], "")
            .await
            .status,
        200
    );
}

#[tokio::test]
async fn disabled_users_and_disabled_sharing_hide_shares() {
    let fx = fixture().await;
    let share = fx.share("calendar", "bob", "read-write").await;
    fx.store
        .set_disabled(&fx.alice.username, true)
        .await
        .unwrap();
    assert_eq!(fx.propfind("bob", &share, "0").await.status, 404);
    let list = fx.api("bob", "GET", "/api/v1/shared", None).await;
    assert_eq!(list.body, "[]");
    fx.store
        .set_disabled(&fx.alice.username, false)
        .await
        .unwrap();
    assert_eq!(fx.propfind("bob", &share, "0").await.status, 207);

    // With sharing switched off, the same database shows nothing.
    let mut off = config(false, false);
    off.sharing.enabled = false;
    let app = router(&off, AppState::new(fx.store.clone()).unwrap());
    let fx2 = Fx {
        app,
        store: fx.store.clone(),
        alice: fx.alice.clone(),
        bob: fx.bob.clone(),
        carol: fx.carol.clone(),
        passwords: fx.passwords.clone(),
        sessions: fx.sessions.clone(),
    };
    assert_eq!(fx2.propfind("bob", &share, "0").await.status, 404);
    let options = fx2.dav("bob", "OPTIONS", "/dav/", &[], "").await;
    assert!(!options.header("dav").unwrap().contains("sharing"));
    assert_eq!(
        fx2.api("bob", "GET", "/api/v1/shared", None).await.status,
        404
    );
    assert_eq!(
        fx2.propfind("bob", "/dav/notifications/bob/", "0")
            .await
            .status,
        404
    );
}

// --- REST API -----------------------------------------------------------

#[tokio::test]
async fn rest_api_manages_shares() {
    let fx = fixture().await;
    let put = |user: &str, target: &str, access: &str| {
        let fx = &fx;
        let (user, target, access) = (user.to_owned(), target.to_owned(), access.to_owned());
        async move {
            fx.api(
                &user,
                "PUT",
                &format!("/api/v1/collections/calendar/default/shares/{target}"),
                Some(&format!(r#"{{"access":"{access}"}}"#)),
            )
            .await
        }
    };
    assert_eq!(put("alice", "bob", "read").await.status, 201);
    assert_eq!(put("alice", "bob", "read-write").await.status, 200);
    assert_eq!(put("alice", "alice", "read").await.status, 400);
    assert_eq!(put("alice", "nobody", "read").await.status, 404);
    assert_eq!(put("alice", "bob", "write").await.status, 400);
    fx.store
        .set_disabled(&fx.carol.username, true)
        .await
        .unwrap();
    assert_eq!(put("alice", "carol", "read").await.status, 404);
    let list = fx
        .api(
            "alice",
            "GET",
            "/api/v1/collections/calendar/default/shares",
            None,
        )
        .await;
    let list: serde_json::Value = serde_json::from_str(&list.body).unwrap();
    assert_eq!(list[0]["username"], "bob");
    assert_eq!(list[0]["access"], "read-write");
    assert_eq!(list[0]["accepted"], false);
    // Bob cannot manage alice's shares; his own calendar has none.
    let reply = fx
        .api(
            "bob",
            "GET",
            "/api/v1/collections/calendar/default/shares",
            None,
        )
        .await;
    assert_eq!(reply.body, "[]");
    // Accepting somebody else's share id is "not found".
    let id = fx.pending_id("bob").await;
    let reply = fx
        .api(
            "alice",
            "POST",
            &format!("/api/v1/shared/{id}/accept"),
            None,
        )
        .await;
    assert_eq!(reply.status, 404);
    let reply = fx
        .api("bob", "POST", &format!("/api/v1/shared/{id}/accept"), None)
        .await;
    assert_eq!(reply.status, 200);
    let info: serde_json::Value = serde_json::from_str(&reply.body).unwrap();
    assert_eq!(info["href"], "/dav/calendars/bob/alice-default/");
    assert_eq!(info["owner"], "alice");
    // Administrators see who shares what with whom.
    let admin = fx
        .store
        .create_user_with_role(&"root".parse().unwrap(), "x", None, true)
        .await
        .unwrap();
    let token = "root-session-token-0123456789abcdef";
    fx.store
        .create_session(admin.id, &hash_secret(token), now(), now() + 3600)
        .await
        .unwrap();
    let reply = fx
        .raw(
            "GET",
            "/api/v1/admin/shares",
            &[("cookie", format!("calcraft_session={token}"))],
            "",
        )
        .await;
    assert_eq!(reply.status, 200);
    assert!(reply.body.contains(r#""sharee":"bob""#), "{}", reply.body);
    assert_eq!(
        fx.api("bob", "GET", "/api/v1/admin/shares", None)
            .await
            .status,
        403
    );
    // Bob leaves.
    assert_eq!(
        fx.api("bob", "DELETE", &format!("/api/v1/shared/{id}"), None)
            .await
            .status,
        204
    );
    assert_eq!(
        fx.api("bob", "DELETE", &format!("/api/v1/shared/{id}"), None)
            .await
            .status,
        404
    );
    // Write access to a subscription is refused, read access is fine.
    sqlx_set_read_only(&fx.store, fx.alice.id).await;
    let reply = put("alice", "bob", "read-write").await;
    assert_eq!(reply.status, 403, "{}", reply.body);
    // Bob left a share of alice: she cannot invite him again for a day.
    let reply = put("alice", "bob", "read").await;
    assert_eq!(reply.status, 409, "{}", reply.body);
    fx.store.clear_share_declines_for_tests().await.unwrap();
    assert_eq!(put("alice", "bob", "read").await.status, 201);
}

/// Marks alice's default calendar read-only like a subscription.
async fn sqlx_set_read_only(store: &Store, alice: i64) {
    // There is no public API for that besides creating a subscription;
    // a read-only share of the real thing is tested in the store tests.
    let cal = store
        .collection(alice, CollectionKind::Calendar, &"default".parse().unwrap())
        .await
        .unwrap()
        .unwrap();
    store.mark_read_only_for_tests(cal.id).await.unwrap();
}

// --- scheduling and attachments -----------------------------------------

#[tokio::test]
async fn sharees_never_schedule_in_the_owners_name() {
    let fx = fixture_with(config(true, true)).await;
    let share = fx.share("calendar", "bob", "read-write").await;
    // Bob writes a meeting "organized" by alice with carol: stored, but no
    // invitation reaches carol.
    let meeting = event(
        "um",
        "Meeting",
        "ORGANIZER:mailto:alice@example.org\r\nATTENDEE;PARTSTAT=NEEDS-ACTION:mailto:carol@example.org\r\n",
    );
    let reply = fx.put("bob", &format!("{share}m.ics"), &meeting).await;
    assert_eq!(reply.status, 201, "{}", reply.body);
    assert_eq!(fx.store.inbox_revision(fx.carol.id).await.unwrap(), 0);
    let stored = fx
        .dav("alice", "GET", &format!("{ALICE_CAL}m.ics"), &[], "")
        .await;
    assert!(stored.header("schedule-tag").is_none());

    // Alice's own meeting is a scheduling object: bob may neither change
    // nor delete it, nor its attachments.
    let reply = fx.put("alice", &format!("{ALICE_CAL}own.ics"), &event(
        "uo",
        "Own meeting",
        "ORGANIZER:mailto:alice@example.org\r\nATTENDEE;PARTSTAT=NEEDS-ACTION:mailto:carol@example.org\r\n",
    )).await;
    assert_eq!(reply.status, 201);
    assert!(fx.store.inbox_revision(fx.carol.id).await.unwrap() > 0);
    let item = format!("{share}own.ics");
    assert_eq!(
        fx.put("bob", &item, &event("uo", "changed", ""))
            .await
            .status,
        403
    );
    assert_eq!(fx.dav("bob", "DELETE", &item, &[], "").await.status, 403);
    // Nor move it away, nor move or copy a plain object over it.
    let before = fx
        .dav("alice", "GET", &format!("{ALICE_CAL}own.ics"), &[], "")
        .await;
    for (method, from, to) in [
        ("MOVE", item.clone(), format!("{share}moved.ics")),
        ("MOVE", format!("{share}m.ics"), item.clone()),
        ("COPY", format!("{share}m.ics"), item.clone()),
    ] {
        let reply = fx
            .dav(
                "bob",
                method,
                &from,
                &[("destination", &to), ("overwrite", "T")],
                "",
            )
            .await;
        assert_eq!(reply.status, 403, "{method} {from} -> {to}: {}", reply.body);
    }
    let after = fx
        .dav("alice", "GET", &format!("{ALICE_CAL}own.ics"), &[], "")
        .await;
    assert_eq!(before, after);
    assert!(after.header("schedule-tag").is_some());
    let reply = fx
        .dav(
            "bob",
            "POST",
            &format!("{item}?action=attachment-add"),
            &[("content-type", "text/plain")],
            "file",
        )
        .await;
    assert_eq!(reply.status, 403);
    assert_eq!(fx.store.attachment_usage(fx.alice.id).await.unwrap().0, 0);

    // On a plain object bob adds an attachment: alice owns it.
    let reply = fx
        .dav(
            "bob",
            "POST",
            &format!("{share}m.ics?action=attachment-add"),
            &[("content-type", "text/plain")],
            "file",
        )
        .await;
    assert_eq!(reply.status, 201, "{}", reply.body);
    assert_eq!(fx.store.attachment_usage(fx.alice.id).await.unwrap().0, 1);
    assert_eq!(fx.store.attachment_usage(fx.bob.id).await.unwrap().0, 0);
    let managed = reply.header("cal-managed-id").unwrap().to_owned();
    assert_eq!(
        fx.dav(
            "bob",
            "GET",
            &format!("/dav/attachments/{managed}"),
            &[],
            ""
        )
        .await
        .status,
        200
    );
    assert_eq!(
        fx.dav(
            "carol",
            "GET",
            &format!("/dav/attachments/{managed}"),
            &[],
            ""
        )
        .await
        .status,
        404
    );

    // Bob's default calendar for invitations stays his own.
    let inbox = fx
        .dav(
            "bob",
            "PROPFIND",
            "/dav/scheduling/bob/inbox/",
            &[("depth", "0")],
            &format!(
                "<D:propfind {NS}><D:prop><C:schedule-default-calendar-URL/></D:prop></D:propfind>"
            ),
        )
        .await;
    assert!(
        inbox.body.contains("/dav/calendars/bob/default/"),
        "{}",
        inbox.body
    );
}

// --- concurrency --------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn share_removed_while_the_sharee_writes() {
    let fx = fixture().await;
    let share = fx.share("calendar", "bob", "read-write").await;
    let fx = std::sync::Arc::new(fx);
    let mut tasks = tokio::task::JoinSet::new();
    for n in 0..40 {
        let fx = fx.clone();
        let share = share.clone();
        tasks.spawn(async move {
            let uri = format!("{share}c{n}.ics");
            let reply = fx.put("bob", &uri, &event(&format!("c{n}"), "c", "")).await;
            (n, reply.status)
        });
    }
    // Remove the share while the writes run.
    tokio::task::yield_now().await;
    fx.unshare("calendar", "bob").await;
    let mut written = Vec::new();
    while let Some(result) = tasks.join_next().await {
        let (n, status) = result.unwrap();
        assert!(matches!(status, 201 | 404), "c{n}: {status}");
        if status == 201 {
            written.push(format!("c{n}.ics"));
        }
    }
    // Exactly the writes that succeeded are stored; none after the removal.
    let cal = fx
        .store
        .collection(
            fx.alice.id,
            CollectionKind::Calendar,
            &"default".parse().unwrap(),
        )
        .await
        .unwrap()
        .unwrap();
    let mut stored: Vec<String> = fx
        .store
        .items(cal.id)
        .await
        .unwrap()
        .into_iter()
        .map(|m| m.name)
        .collect();
    stored.sort();
    written.sort();
    assert_eq!(stored, written);
    let after = fx
        .put("bob", &format!("{share}late.ics"), &event("late", "x", ""))
        .await;
    assert_eq!(after.status, 404);
}

// --- private events -----------------------------------------------------

/// The value of the first `<{tag}>` element.
fn element<'a>(body: &'a str, tag: &str) -> &'a str {
    let open = format!("<{tag}>");
    let start = body.find(&open).unwrap() + open.len();
    let end = body[start..].find('<').unwrap() + start;
    &body[start..end]
}

#[tokio::test]
async fn read_sharees_see_private_events_as_busy_stubs() {
    let fx = fixture_with(config(false, true)).await;
    let private = event(
        "up",
        "Doctor",
        "CLASS:PRIVATE\r\nRRULE:FREQ=WEEKLY;COUNT=3\r\nDESCRIPTION:Diagnosis\r\nLOCATION:Clinic\r\nATTENDEE:mailto:doc@example.org\r\nURL:https://clinic.example/x\r\nX-NOTE:secret\r\nBEGIN:VALARM\r\nACTION:DISPLAY\r\nDESCRIPTION:Pills\r\nTRIGGER:-PT5M\r\nEND:VALARM\r\n",
    );
    assert_eq!(
        fx.put("alice", &format!("{ALICE_CAL}p.ics"), &private)
            .await
            .status,
        201
    );
    assert_eq!(
        fx.put(
            "alice",
            &format!("{ALICE_CAL}o.ics"),
            &event("uo", "Open meeting", "DESCRIPTION:Agenda\r\n")
        )
        .await
        .status,
        201
    );
    let reply = fx
        .dav(
            "alice",
            "POST",
            &format!("{ALICE_CAL}p.ics?action=attachment-add"),
            &[("content-type", "text/plain")],
            "lab results",
        )
        .await;
    assert_eq!(reply.status, 201);
    let managed = reply.header("cal-managed-id").unwrap().to_owned();
    let share = fx.share("calendar", "bob", "read").await;
    let rw = fx.share("calendar", "carol", "read-write").await;

    // GET: a stub.
    let stub = fx
        .dav("bob", "GET", &format!("{share}p.ics"), &[], "")
        .await;
    assert_eq!(stub.status, 200);
    for kept in [
        "UID:up",
        "DTSTART:20261021T100000Z",
        "RRULE:FREQ=WEEKLY",
        "CLASS:PRIVATE",
        "SUMMARY:Busy",
    ] {
        assert!(stub.body.contains(kept), "{kept}:\n{}", stub.body);
    }
    for hidden in [
        "Doctor",
        "Diagnosis",
        "Clinic",
        "doc@example.org",
        "clinic.example",
        "secret",
        "VALARM",
        "Pills",
        "ATTACH",
        &managed,
    ] {
        assert!(!stub.body.contains(hidden), "{hidden}:\n{}", stub.body);
    }
    let full = fx
        .dav("alice", "GET", &format!("{ALICE_CAL}p.ics"), &[], "")
        .await;
    assert!(full.body.contains("Diagnosis") && full.body.contains("ATTACH"));
    let etag = stub.header("etag").unwrap().to_owned();
    assert_ne!(Some(etag.as_str()), full.header("etag"));
    // The read-write sharee and the owner see everything.
    let theirs = fx.dav("carol", "GET", &format!("{rw}p.ics"), &[], "").await;
    assert_eq!(theirs.body, full.body);
    assert_eq!(theirs.header("etag"), full.header("etag"));
    // Public events of the read share are unchanged.
    let open = fx
        .dav("bob", "GET", &format!("{share}o.ics"), &[], "")
        .await;
    assert!(open.body.contains("Agenda"));

    // PROPFIND (object and listing), multiget, query and sync agree on the
    // stub's ETag and length.
    let length = stub.body.len().to_string();
    let props = "<D:prop><D:getetag/><D:getcontentlength/></D:prop>";
    let d0 = fx
        .dav(
            "bob",
            "PROPFIND",
            &format!("{share}p.ics"),
            &[("depth", "0")],
            &format!("<D:propfind {NS}>{props}</D:propfind>"),
        )
        .await;
    assert_eq!(element(&d0.body, "d:getetag").replace("&quot;", "\""), etag);
    assert_eq!(element(&d0.body, "d:getcontentlength"), length);
    let d1 = fx
        .dav(
            "bob",
            "PROPFIND",
            &share,
            &[("depth", "1")],
            &format!("<D:propfind {NS}>{props}</D:propfind>"),
        )
        .await;
    let entry = &d1.body[d1.body.find(&format!("{share}p.ics")).unwrap()..];
    assert_eq!(element(entry, "d:getetag").replace("&quot;", "\""), etag);
    assert_eq!(element(entry, "d:getcontentlength"), length);
    let multiget = format!(
        "<C:calendar-multiget {NS}><D:prop><D:getetag/><C:calendar-data/></D:prop><D:href>{share}p.ics</D:href></C:calendar-multiget>"
    );
    let report = fx
        .dav("bob", "REPORT", &share, &[("depth", "1")], &multiget)
        .await;
    assert_eq!(
        element(&report.body, "d:getetag").replace("&quot;", "\""),
        etag
    );
    assert!(report.body.contains("SUMMARY:Busy") && !report.body.contains("Diagnosis"));
    let query = |start: &str, end: &str| {
        format!(
            r#"<C:calendar-query {NS}><D:prop><D:getetag/><C:calendar-data/></D:prop><C:filter><C:comp-filter name="VCALENDAR"><C:comp-filter name="VEVENT"><C:time-range start="{start}" end="{end}"/></C:comp-filter></C:comp-filter></C:filter></C:calendar-query>"#
        )
    };
    // The second instance, from the recurrence rule the stub keeps.
    let hit = fx
        .dav(
            "bob",
            "REPORT",
            &share,
            &[("depth", "1")],
            &query("20261028T000000Z", "20261029T000000Z"),
        )
        .await;
    assert!(
        hit.body.contains(&format!("{share}p.ics")) && !hit.body.contains("Diagnosis"),
        "{}",
        hit.body
    );
    let owner_hit = fx
        .dav(
            "alice",
            "REPORT",
            ALICE_CAL,
            &[("depth", "1")],
            &query("20261028T000000Z", "20261029T000000Z"),
        )
        .await;
    assert!(owner_hit.body.contains("p.ics"));
    let miss = fx
        .dav(
            "bob",
            "REPORT",
            &share,
            &[("depth", "1")],
            &query("20271028T000000Z", "20271029T000000Z"),
        )
        .await;
    assert!(!miss.body.contains("p.ics"));
    let sync = format!(
        "<D:sync-collection {NS}><D:sync-token/><D:sync-level>1</D:sync-level><D:prop><D:getetag/><C:calendar-data/></D:prop></D:sync-collection>"
    );
    let synced = fx.dav("bob", "REPORT", &share, &[], &sync).await;
    let entry = &synced.body[synced.body.find(&format!("{share}p.ics")).unwrap()..];
    assert_eq!(element(entry, "d:getetag").replace("&quot;", "\""), etag);
    assert!(!synced.body.contains("Diagnosis"));

    // No oracle: a text-match on the hidden text answers exactly like one
    // on text that exists nowhere (and like on the user's own calendar).
    let text_match = |text: &str| {
        format!(
            r#"<C:calendar-query {NS}><D:prop><D:getetag/></D:prop><C:filter><C:comp-filter name="VCALENDAR"><C:comp-filter name="VEVENT"><C:prop-filter name="DESCRIPTION"><C:text-match>{text}</C:text-match></C:prop-filter></C:comp-filter></C:comp-filter></C:filter></C:calendar-query>"#
        )
    };
    let hidden = fx
        .dav(
            "bob",
            "REPORT",
            &share,
            &[("depth", "1")],
            &text_match("Diagnosis"),
        )
        .await;
    let absent = fx
        .dav(
            "bob",
            "REPORT",
            &share,
            &[("depth", "1")],
            &text_match("zz-nowhere"),
        )
        .await;
    let own = fx
        .dav(
            "bob",
            "REPORT",
            "/dav/calendars/bob/default/",
            &[("depth", "1")],
            &text_match("Diagnosis"),
        )
        .await;
    assert_eq!(hidden, absent);
    assert_eq!(hidden, own);
    assert!(!hidden.body.contains("p.ics"));

    // The attachment of the private event: not for the read sharee, like
    // an id that was never issued; the read-write sharee may.
    let attachment = format!("/dav/attachments/{managed}");
    let denied = fx.dav("bob", "GET", &attachment, &[], "").await;
    let missing = fx
        .dav(
            "bob",
            "GET",
            "/dav/attachments/0123456789abcdef0123456789abcdef",
            &[],
            "",
        )
        .await;
    assert_eq!(denied, missing);
    assert_eq!(
        fx.dav("carol", "GET", &attachment, &[], "").await.status,
        200
    );

    // A change of hidden details only keeps the stub (and its ETag); a new
    // time changes both.
    let changed = private.replace("Diagnosis", "Other diagnosis");
    fx.put("alice", &format!("{ALICE_CAL}p.ics"), &changed)
        .await;
    let again = fx
        .dav("bob", "GET", &format!("{share}p.ics"), &[], "")
        .await;
    assert_eq!(again.header("etag"), Some(etag.as_str()));
    assert_eq!(again.body, stub.body);
    let moved = changed.replace("DTSTART:20261021T100000Z", "DTSTART:20261021T090000Z");
    fx.put("alice", &format!("{ALICE_CAL}p.ics"), &moved).await;
    let again = fx
        .dav("bob", "GET", &format!("{share}p.ics"), &[], "")
        .await;
    assert_ne!(again.header("etag"), Some(etag.as_str()));
    assert!(again.body.contains("DTSTART:20261021T090000Z"));
}

#[tokio::test]
async fn a_failing_entry_makes_a_share_request_change_nothing() {
    let fx = fixture().await;
    // Alice has as many invitations pending with bob as one owner may.
    for n in 0..calcraft_store::MAX_PENDING_PER_OWNER {
        let collection = fx
            .store
            .create_collection(
                fx.alice.id,
                CollectionKind::Calendar,
                &format!("c{n}").parse().unwrap(),
                None,
                None,
                None,
                "VEVENT",
            )
            .await
            .unwrap();
        fx.store
            .set_share(
                collection.id,
                fx.alice.id,
                fx.bob.id,
                calcraft_store::ShareAccess::Read,
                now(),
            )
            .await
            .unwrap();
    }
    let xml = [("content-type", "application/xml")];
    let body = format!(
        "<CS:share {NS}><CS:set><D:href>/dav/principals/carol/</D:href><CS:read/></CS:set><CS:set><D:href>/dav/principals/bob/</D:href><CS:read/></CS:set></CS:share>"
    );
    let reply = fx.dav("alice", "POST", ALICE_CAL, &xml, &body).await;
    assert_eq!(reply.status, 403, "{}", reply.body);
    assert!(
        fx.store
            .incoming_shares(fx.carol.id)
            .await
            .unwrap()
            .is_empty()
    );
    // A sharee who declined cannot be invited again by the same owner for a
    // day.
    let carol = format!(
        "<CS:share {NS}><CS:set><D:href>/dav/principals/carol/</D:href><CS:read/></CS:set></CS:share>"
    );
    assert_eq!(
        fx.dav("alice", "POST", ALICE_CAL, &xml, &carol)
            .await
            .status,
        200
    );
    let id = fx.store.incoming_shares(fx.carol.id).await.unwrap()[0]
        .public_id
        .clone();
    assert_eq!(
        fx.api("carol", "DELETE", &format!("/api/v1/shared/{id}"), None)
            .await
            .status,
        204
    );
    let reply = fx.dav("alice", "POST", ALICE_CAL, &xml, &carol).await;
    assert_eq!(reply.status, 403);
    assert!(reply.body.contains("invite-declined-recently"));
    assert!(
        fx.store
            .incoming_shares(fx.carol.id)
            .await
            .unwrap()
            .is_empty()
    );
}
