//! Groups and sharing with groups (docs/sharing.md, "Groups"): group
//! management by administrators (REST API and CLI), each member's own
//! invitation, joining (invitation) and leaving (access ends at once,
//! afterwards indistinguishable from a path that never existed, also for
//! sync-collection and attachments), direct and group shares together
//! (maximum, removing one keeps the other), deleting a group, declines
//! remembered per member, limits, disabled users, who may share with a
//! group (members and administrators only), Apple `CS:share` with group
//! principals, and a member removed while writing. The authorization matrix
//! (`authz_matrix.rs`) covers group shares against every method.

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
use serde_json::Value;
use tower::ServiceExt;

const NS: &str = r#"xmlns:D="DAV:" xmlns:CS="http://calendarserver.org/ns/""#;
const ALICE_CAL: &str = "/dav/calendars/alice/default/";
const ATTACH_BASE: &str = "https://cal.example.org";

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

    fn json(&self) -> Value {
        serde_json::from_str(&self.body).unwrap()
    }
}

fn config() -> Config {
    let mut config = Config::default();
    config.server.max_concurrent_requests_per_client = 4096;
    config.server.client_rate_burst = u32::MAX;
    config.server.client_rate_per_second = u32::MAX;
    config.attachments.enabled = true;
    config.attachments.base_url = Some(ATTACH_BASE.to_owned());
    config
}

/// Users `root` (admin), alice, bob, carol and dave; the group `team`
/// with alice, bob and carol.
async fn fixture_with(config: Config) -> Fx {
    let store = Store::in_memory().await.unwrap();
    let mut users = Vec::new();
    let mut passwords = Vec::new();
    let mut sessions = Vec::new();
    for name in ["root", "alice", "bob", "carol", "dave"] {
        let user = store
            .create_user_with_role(&name.parse().unwrap(), "unused", None, name == "root")
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
    let fx = Fx {
        app,
        store,
        alice: users[1].clone(),
        bob: users[2].clone(),
        passwords,
        sessions,
    };
    let reply = fx
        .api(
            "root",
            "POST",
            "/api/v1/admin/groups",
            Some(r#"{"name":"team","display_name":"The team"}"#),
        )
        .await;
    assert_eq!(reply.status, 201, "{reply:?}");
    for member in ["alice", "bob", "carol"] {
        fx.join("team", member).await;
    }
    fx
}

async fn fixture() -> Fx {
    fixture_with(config()).await
}

fn event(uid: &str, summary: &str) -> String {
    format!(
        "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//Test//EN\r\nBEGIN:VEVENT\r\nUID:{uid}\r\nDTSTAMP:20261001T000000Z\r\nDTSTART:20261021T100000Z\r\nDTEND:20261021T110000Z\r\nSUMMARY:{summary}\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n"
    )
}

impl Fx {
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

    async fn dav(
        &self,
        user: &str,
        method: &str,
        uri: &str,
        headers: &[(&str, &str)],
        body: &str,
    ) -> Reply {
        let (_, password) = self.passwords.iter().find(|(n, _)| n == user).unwrap();
        let auth = format!("Basic {}", STANDARD.encode(format!("{user}:{password}")));
        let mut all: Vec<(&str, String)> = vec![("authorization", auth)];
        all.extend(headers.iter().map(|(k, v)| (*k, (*v).to_owned())));
        self.raw(method, uri, &all, body).await
    }

    async fn api(&self, user: &str, method: &str, uri: &str, body: Option<&str>) -> Reply {
        let session = &self.sessions.iter().find(|(n, _)| n == user).unwrap().1;
        let mut headers: Vec<(&str, String)> = vec![
            ("cookie", format!("calcraft_session={session}")),
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

    async fn join(&self, group: &str, user: &str) -> Value {
        let reply = self
            .api(
                "root",
                "PUT",
                &format!("/api/v1/admin/groups/{group}/members/{user}"),
                None,
            )
            .await;
        assert_eq!(reply.status, 200, "{reply:?}");
        reply.json()
    }

    async fn leave_group(&self, group: &str, user: &str) {
        let reply = self
            .api(
                "root",
                "DELETE",
                &format!("/api/v1/admin/groups/{group}/members/{user}"),
                None,
            )
            .await;
        assert_eq!(reply.status, 204, "{reply:?}");
    }

    /// Alice shares her default calendar with a group.
    async fn share_with_group(&self, group: &str, access: &str) -> Reply {
        self.api(
            "alice",
            "PUT",
            &format!("/api/v1/collections/calendar/default/group-shares/{group}"),
            Some(&format!(r#"{{"access":"{access}"}}"#)),
        )
        .await
    }

    async fn incoming(&self, user: &str) -> Vec<Value> {
        let reply = self.api(user, "GET", "/api/v1/shared", None).await;
        assert_eq!(reply.status, 200);
        reply.json().as_array().unwrap().clone()
    }

    /// Accepts the user's one invitation; returns the path in their home.
    async fn accept(&self, user: &str) -> String {
        let list = self.incoming(user).await;
        let pending: Vec<&Value> = list.iter().filter(|s| s["accepted"] == false).collect();
        assert_eq!(pending.len(), 1, "{list:?}");
        let id = pending[0]["id"].as_str().unwrap();
        let reply = self
            .api(user, "POST", &format!("/api/v1/shared/{id}/accept"), None)
            .await;
        assert_eq!(reply.status, 200, "{reply:?}");
        reply.json()["href"].as_str().unwrap().to_owned()
    }

    async fn calendar_id(&self) -> i64 {
        self.store
            .collection(
                self.alice.id,
                CollectionKind::Calendar,
                &"default".parse().unwrap(),
            )
            .await
            .unwrap()
            .unwrap()
            .id
    }
}

// --- management ----------------------------------------------------------

#[tokio::test]
async fn administrators_manage_groups() {
    let fx = fixture().await;
    let list = fx.api("root", "GET", "/api/v1/admin/groups", None).await;
    assert_eq!(list.status, 200);
    assert_eq!(list.json()[0]["name"], "team");
    assert_eq!(list.json()[0]["member_count"], 3);
    let team = fx
        .api("root", "GET", "/api/v1/admin/groups/team", None)
        .await;
    let team = team.json();
    let members: Vec<&str> = team["members"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["username"].as_str().unwrap())
        .collect();
    assert_eq!(members, ["alice", "bob", "carol"]);
    // Names like usernames, unique.
    for (body, status) in [
        (r#"{"name":"Team"}"#, 400),
        (r#"{"name":"a/b"}"#, 400),
        (r#"{"name":"team"}"#, 409),
        (r#"{"name":"x","extra":1}"#, 400),
    ] {
        let reply = fx
            .api("root", "POST", "/api/v1/admin/groups", Some(body))
            .await;
        assert_eq!(reply.status, status, "{body}: {}", reply.body);
    }
    // Adding twice, unknown users and groups.
    assert_eq!(fx.join("team", "bob").await["added"], false);
    for uri in [
        "/api/v1/admin/groups/team/members/nobody",
        "/api/v1/admin/groups/nogroup/members/bob",
    ] {
        assert_eq!(fx.api("root", "PUT", uri, None).await.status, 404, "{uri}");
    }
    assert_eq!(
        fx.api(
            "root",
            "DELETE",
            "/api/v1/admin/groups/team/members/dave",
            None
        )
        .await
        .status,
        404
    );
    // Members see their groups with the enabled members; others nothing.
    let mine = fx.api("bob", "GET", "/api/v1/me/groups", None).await;
    assert_eq!(mine.json()[0]["name"], "team");
    assert_eq!(mine.json()[0]["members"].as_array().unwrap().len(), 3);
    fx.store
        .set_disabled(&"carol".parse().unwrap(), true)
        .await
        .unwrap();
    let mine = fx.api("bob", "GET", "/api/v1/me/groups", None).await;
    assert_eq!(mine.json()[0]["members"].as_array().unwrap().len(), 2);
    assert!(!mine.body.contains("carol"));
    let none = fx.api("dave", "GET", "/api/v1/me/groups", None).await;
    assert_eq!(none.body, "[]");
    // Non-admins are refused.
    assert_eq!(
        fx.api("alice", "GET", "/api/v1/admin/groups", None)
            .await
            .status,
        403
    );
    // Deleting.
    assert_eq!(
        fx.api("root", "DELETE", "/api/v1/admin/groups/team", None)
            .await
            .status,
        204
    );
    assert_eq!(
        fx.api("root", "DELETE", "/api/v1/admin/groups/team", None)
            .await
            .status,
        404
    );
}

#[tokio::test]
async fn group_and_member_limits_come_from_the_configuration() {
    // The fixture's group has three members: full.
    let mut config = config();
    config.groups.max_members = 3;
    let fx = fixture_with(config).await;
    let reply = fx
        .api(
            "root",
            "PUT",
            "/api/v1/admin/groups/team/members/dave",
            None,
        )
        .await;
    assert_eq!(reply.status, 507, "{reply:?}");
    assert_eq!(reply.json()["error"], "limit");
    // An existing member is no new one.
    assert_eq!(fx.join("team", "bob").await["added"], false);
    fx.leave_group("team", "carol").await;
    assert_eq!(fx.join("team", "dave").await["added"], true);
    let mut config = self::config();
    config.groups.max_groups = 1;
    let fx = fixture_with(config).await;
    let reply = fx
        .api(
            "root",
            "POST",
            "/api/v1/admin/groups",
            Some(r#"{"name":"two"}"#),
        )
        .await;
    assert_eq!(reply.status, 507, "{reply:?}");
}

// --- sharing with a group -------------------------------------------------

#[tokio::test]
async fn every_member_gets_an_own_invitation() {
    let fx = fixture().await;
    let reply = fx.share_with_group("team", "read").await;
    assert_eq!(reply.status, 201, "{reply:?}");
    // Bob and carol; not alice herself.
    assert_eq!(reply.json()["invited"], 2);
    assert_eq!(reply.json()["share"]["members"], 2);
    assert!(fx.incoming("alice").await.is_empty());
    // Pending until accepted, per member.
    assert_eq!(
        fx.dav(
            "bob",
            "PROPFIND",
            "/dav/calendars/bob/alice-default/",
            &[("depth", "0")],
            ""
        )
        .await
        .status,
        404
    );
    let list = fx.incoming("bob").await;
    assert_eq!(list[0]["groups"][0], "team");
    let notifications = fx
        .dav(
            "bob",
            "PROPFIND",
            "/dav/notifications/bob/",
            &[("depth", "1")],
            "",
        )
        .await;
    assert!(
        notifications.body.contains("invite-notification"),
        "{}",
        notifications.body
    );
    let bob = fx.accept("bob").await;
    assert_eq!(
        fx.dav("bob", "PROPFIND", &bob, &[("depth", "0")], "")
            .await
            .status,
        207
    );
    // Carol has not accepted: nothing in her home; her own name and colour
    // are hers.
    assert_eq!(
        fx.dav(
            "carol",
            "PROPFIND",
            "/dav/calendars/carol/alice-default/",
            &[("depth", "0")],
            ""
        )
        .await
        .status,
        404
    );
    let proppatch = format!(
        "<D:propertyupdate {NS}><D:set><D:prop><D:displayname>Bob's view</D:displayname></D:prop></D:set></D:propertyupdate>"
    );
    assert_eq!(
        fx.dav("bob", "PROPPATCH", &bob, &[], &proppatch)
            .await
            .status,
        207
    );
    let carol = fx.accept("carol").await;
    let props = fx
        .dav("carol", "PROPFIND", &carol, &[("depth", "0")], "")
        .await;
    assert!(!props.body.contains("Bob's view"), "{}", props.body);
    // Read only.
    assert_eq!(
        fx.put("bob", &format!("{bob}x.ics"), &event("x", "x"))
            .await
            .status,
        403
    );
    // The owner sees both members' shares and the group share.
    let shares = fx
        .api(
            "alice",
            "GET",
            "/api/v1/collections/calendar/default/shares",
            None,
        )
        .await;
    assert_eq!(shares.json().as_array().unwrap().len(), 2);
    assert_eq!(shares.json()[0]["direct"], Value::Null);
    let groups = fx
        .api(
            "alice",
            "GET",
            "/api/v1/collections/calendar/default/group-shares",
            None,
        )
        .await;
    assert_eq!(groups.json()[0]["group"], "team");
    assert_eq!(groups.json()[0]["access"], "read");
    // Upgrading the group share needs no new acceptance.
    let reply = fx.share_with_group("team", "read-write").await;
    assert_eq!(reply.status, 200);
    assert_eq!(reply.json()["invited"], 0);
    assert_eq!(
        fx.put("bob", &format!("{bob}x.ics"), &event("x", "x"))
            .await
            .status,
        201
    );
}

#[tokio::test]
async fn only_members_and_administrators_share_with_a_group() {
    let fx = fixture().await;
    fx.api(
        "root",
        "POST",
        "/api/v1/admin/groups",
        Some(r#"{"name":"others"}"#),
    )
    .await;
    fx.join("others", "dave").await;
    // Alice is no member of `others`: like a group that does not exist.
    let other = fx.share_with_group("others", "read").await;
    let missing = fx.share_with_group("nogroup", "read").await;
    assert_eq!(other.status, 404);
    assert_eq!(other, missing);
    assert!(fx.incoming("dave").await.is_empty());
    // An administrator may share with any group.
    let reply = fx
        .api(
            "root",
            "PUT",
            "/api/v1/collections/calendar/default/group-shares/others",
            Some(r#"{"access":"read"}"#),
        )
        .await;
    assert_eq!(reply.status, 201, "{reply:?}");
    assert_eq!(fx.incoming("dave").await.len(), 1);
    // Only the owner; a sharee cannot pass a shared collection on.
    fx.share_with_group("team", "read-write").await;
    fx.accept("bob").await;
    let reply = fx
        .api(
            "bob",
            "PUT",
            "/api/v1/collections/calendar/alice-default/group-shares/team",
            Some(r#"{"access":"read"}"#),
        )
        .await;
    assert_eq!(reply.status, 404);
    // Subscriptions only read-only.
    let cal = fx.calendar_id().await;
    fx.store.mark_read_only_for_tests(cal).await.unwrap();
    let reply = fx.share_with_group("team", "read-write").await;
    assert_eq!(reply.status, 403, "{reply:?}");
}

#[tokio::test]
async fn leaving_the_group_ends_access_like_it_never_existed() {
    let fx = fixture().await;
    fx.put("alice", &format!("{ALICE_CAL}a.ics"), &event("ua", "a"))
        .await;
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
    let attachment = format!(
        "/dav/attachments/{}",
        reply.header("cal-managed-id").unwrap()
    );
    fx.share_with_group("team", "read-write").await;
    let share = fx.accept("bob").await;
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
                event("ua", "x"),
            ),
            ("DELETE", format!("{base}a.ics"), vec![], String::new()),
            ("REPORT", base.to_owned(), vec![], sync.clone()),
        ]
    };
    fx.leave_group("team", "bob").await;
    let never = "/dav/calendars/bob/never-existed/";
    for ((method, uri, headers, body), (_, never_uri, _, _)) in
        requests(&share).into_iter().zip(requests(never))
    {
        let gone = fx.dav("bob", method, &uri, &headers, &body).await;
        let nothing = fx.dav("bob", method, &never_uri, &headers, &body).await;
        assert_eq!(gone.status, nothing.status, "{method} {uri}");
        assert_eq!(gone.body.replace(&share, never), nothing.body, "{method}");
        assert_ne!(gone.status / 100, 2, "{method} {uri}");
    }
    let gone = fx.dav("bob", "GET", &attachment, &[], "").await;
    let missing = fx
        .dav(
            "bob",
            "GET",
            "/dav/attachments/0123456789abcdef0123456789abcdef",
            &[],
            "",
        )
        .await;
    assert_eq!(gone, missing);
    assert!(fx.incoming("bob").await.is_empty());
    // Carol keeps her invitation; alice's data is untouched.
    assert_eq!(fx.incoming("carol").await.len(), 1);
    assert_eq!(
        fx.dav("alice", "GET", &format!("{ALICE_CAL}a.ics"), &[], "")
            .await
            .status,
        200
    );
    // Rejoining is a new invitation.
    assert_eq!(fx.join("team", "bob").await["invited"], 1);
    assert_eq!(fx.incoming("bob").await[0]["accepted"], false);
}

#[tokio::test]
async fn joining_the_group_brings_an_invitation() {
    let fx = fixture().await;
    fx.share_with_group("team", "read").await;
    assert!(fx.incoming("dave").await.is_empty());
    assert_eq!(fx.join("team", "dave").await["invited"], 1);
    let list = fx.incoming("dave").await;
    assert_eq!(list.len(), 1);
    assert_eq!(list[0]["accepted"], false);
    let path = fx.accept("dave").await;
    assert_eq!(path, "/dav/calendars/dave/alice-default/");
}

#[tokio::test]
async fn direct_and_group_shares_combine() {
    let fx = fixture().await;
    let reply = fx
        .api(
            "alice",
            "PUT",
            "/api/v1/collections/calendar/default/shares/bob",
            Some(r#"{"access":"read"}"#),
        )
        .await;
    assert_eq!(reply.status, 201);
    fx.share_with_group("team", "read-write").await;
    // One invitation for bob, with the higher access.
    let list = fx.incoming("bob").await;
    assert_eq!(list.len(), 1);
    assert_eq!(list[0]["access"], "read-write");
    let share = fx.accept("bob").await;
    assert_eq!(
        fx.put("bob", &format!("{share}a.ics"), &event("a", "a"))
            .await
            .status,
        201
    );
    // Ending the group share keeps the direct one: read only.
    assert_eq!(
        fx.api(
            "alice",
            "DELETE",
            "/api/v1/collections/calendar/default/group-shares/team",
            None
        )
        .await
        .status,
        204
    );
    assert_eq!(
        fx.put("bob", &format!("{share}b.ics"), &event("b", "b"))
            .await
            .status,
        403
    );
    assert_eq!(
        fx.dav("bob", "GET", &format!("{share}a.ics"), &[], "")
            .await
            .status,
        200
    );
    // Group again, then removing the direct share keeps the group's.
    fx.share_with_group("team", "read-write").await;
    assert_eq!(
        fx.api(
            "alice",
            "DELETE",
            "/api/v1/collections/calendar/default/shares/bob",
            None
        )
        .await
        .status,
        204
    );
    assert_eq!(
        fx.put("bob", &format!("{share}c.ics"), &event("c", "c"))
            .await
            .status,
        201
    );
    // Removing the direct share of a group-only sharee: 404.
    assert_eq!(
        fx.api(
            "alice",
            "DELETE",
            "/api/v1/collections/calendar/default/shares/carol",
            None
        )
        .await
        .status,
        404
    );
    // Leaving the group ends it.
    fx.leave_group("team", "bob").await;
    assert_eq!(
        fx.put("bob", &format!("{share}d.ics"), &event("d", "d"))
            .await
            .status,
        404
    );
}

#[tokio::test]
async fn deleting_the_group_revokes_every_member() {
    let fx = fixture().await;
    fx.share_with_group("team", "read").await;
    let bob = fx.accept("bob").await;
    let carol = fx.accept("carol").await;
    assert_eq!(
        fx.api("root", "DELETE", "/api/v1/admin/groups/team", None)
            .await
            .status,
        204
    );
    for (user, path) in [("bob", &bob), ("carol", &carol)] {
        assert_eq!(
            fx.dav(user, "PROPFIND", path, &[("depth", "0")], "")
                .await
                .status,
            404
        );
        assert!(fx.incoming(user).await.is_empty());
    }
    let groups = fx
        .api(
            "alice",
            "GET",
            "/api/v1/collections/calendar/default/group-shares",
            None,
        )
        .await;
    assert_eq!(groups.body, "[]");
}

#[tokio::test]
async fn a_declined_group_invitation_is_remembered() {
    let fx = fixture().await;
    fx.share_with_group("team", "read").await;
    let id = fx.incoming("bob").await[0]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_eq!(
        fx.api("bob", "DELETE", &format!("/api/v1/shared/{id}"), None)
            .await
            .status,
        204
    );
    // Saving the group share again, leaving and rejoining: no new
    // invitation for bob.
    let reply = fx.share_with_group("team", "read-write").await;
    assert_eq!(reply.json()["invited"], 0);
    fx.leave_group("team", "bob").await;
    assert_eq!(fx.join("team", "bob").await["invited"], 0);
    assert!(fx.incoming("bob").await.is_empty());
    // A new group share within a day: bob is skipped.
    fx.api(
        "alice",
        "DELETE",
        "/api/v1/collections/calendar/default/group-shares/team",
        None,
    )
    .await;
    let reply = fx.share_with_group("team", "read").await;
    assert_eq!(reply.json()["skipped"], 1, "{}", reply.body);
    assert!(fx.incoming("bob").await.is_empty());
    // Carol was not affected.
    assert_eq!(fx.incoming("carol").await.len(), 1);
}

#[tokio::test]
async fn disabled_members_and_owners_see_nothing() {
    let fx = fixture().await;
    fx.share_with_group("team", "read").await;
    let share = fx.accept("bob").await;
    fx.store
        .set_disabled(&fx.alice.username, true)
        .await
        .unwrap();
    assert_eq!(
        fx.dav("bob", "PROPFIND", &share, &[("depth", "0")], "")
            .await
            .status,
        404
    );
    fx.store
        .set_disabled(&fx.alice.username, false)
        .await
        .unwrap();
    assert_eq!(
        fx.dav("bob", "PROPFIND", &share, &[("depth", "0")], "")
            .await
            .status,
        207
    );
    // A disabled member does not count as a fellow member.
    fx.store.set_disabled(&fx.bob.username, true).await.unwrap();
    let shares = fx
        .api(
            "alice",
            "GET",
            "/api/v1/collections/calendar/default/shares",
            None,
        )
        .await;
    assert!(!shares.body.contains("\"bob\""), "{}", shares.body);
}

#[tokio::test]
async fn sharing_switched_off_hides_group_shares() {
    let mut config = config();
    config.sharing.enabled = false;
    let fx = fixture_with(config).await;
    assert_eq!(fx.share_with_group("team", "read").await.status, 404);
    assert_eq!(
        fx.api(
            "alice",
            "GET",
            "/api/v1/collections/calendar/default/group-shares",
            None
        )
        .await
        .status,
        404
    );
    // Groups themselves stay manageable.
    assert_eq!(
        fx.api("bob", "GET", "/api/v1/me/groups", None).await.status,
        200
    );
}

// --- Apple CS:share --------------------------------------------------------

fn cs_share(set: &str, remove: &str) -> String {
    format!("<CS:share {NS}>{set}{remove}</CS:share>")
}

fn cs_set(href: &str, access: &str) -> String {
    format!("<CS:set><D:href>{href}</D:href><CS:{access}/></CS:set>")
}

#[tokio::test]
async fn apple_clients_share_with_group_principals() {
    let fx = fixture().await;
    fx.api(
        "root",
        "POST",
        "/api/v1/admin/groups",
        Some(r#"{"name":"others"}"#),
    )
    .await;
    fx.join("others", "dave").await;
    let xml = [("content-type", "text/xml; charset=utf-8")];
    // Not a member of `others`, or no such group: the same refusal, and
    // nothing changes (also not the valid entry next to it).
    let mut refusals = Vec::new();
    for href in [
        "/dav/principals/groups/others/",
        "/dav/principals/groups/nogroup/",
        "/dav/principals/groups/Bad%2Fname/",
    ] {
        let body = cs_share(
            &format!(
                "{}{}",
                cs_set("/dav/principals/groups/team/", "read"),
                cs_set(href, "read")
            ),
            "",
        );
        let reply = fx.dav("alice", "POST", ALICE_CAL, &xml, &body).await;
        assert_eq!(reply.status, 403, "{href}");
        assert!(reply.body.contains("valid-sharee"));
        refusals.push(reply);
    }
    assert!(refusals.windows(2).all(|pair| pair[0] == pair[1]));
    assert!(fx.incoming("bob").await.is_empty());
    assert!(fx.incoming("dave").await.is_empty());
    // A group of alice's, as an absolute URL too.
    let body = cs_share(
        &cs_set("http://localhost/dav/principals/groups/team/", "read-write"),
        "",
    );
    let reply = fx.dav("alice", "POST", ALICE_CAL, &xml, &body).await;
    assert_eq!(reply.status, 200, "{}", reply.body);
    assert_eq!(fx.incoming("bob").await.len(), 1);
    assert_eq!(fx.incoming("carol").await.len(), 1);
    // CS:invite lists every member.
    let props = fx
        .dav("alice", "PROPFIND", ALICE_CAL, &[("depth", "0")], "")
        .await;
    assert!(
        props.body.contains("/dav/principals/bob/"),
        "{}",
        props.body
    );
    assert!(props.body.contains("/dav/principals/carol/"));
    // A sharee cannot share.
    let share = fx.accept("bob").await;
    let reply = fx.dav("bob", "POST", &share, &xml, &body).await;
    assert_eq!(reply.status, 403);
    // Group principals are no DAV resources.
    assert_eq!(
        fx.dav(
            "alice",
            "PROPFIND",
            "/dav/principals/groups/team/",
            &[("depth", "0")],
            ""
        )
        .await
        .status,
        404
    );
    // CS:remove ends the group share.
    let body = cs_share(
        "",
        "<CS:remove><D:href>/dav/principals/groups/team/</D:href></CS:remove>",
    );
    let reply = fx.dav("alice", "POST", ALICE_CAL, &xml, &body).await;
    assert_eq!(reply.status, 200, "{}", reply.body);
    assert!(fx.incoming("bob").await.is_empty());
    assert!(fx.incoming("carol").await.is_empty());
}

// --- concurrency ------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn member_removed_while_writing() {
    let fx = fixture().await;
    fx.share_with_group("team", "read-write").await;
    let share = fx.accept("bob").await;
    let fx = std::sync::Arc::new(fx);
    let mut tasks = tokio::task::JoinSet::new();
    for n in 0..40 {
        let fx = fx.clone();
        let share = share.clone();
        tasks.spawn(async move {
            let uri = format!("{share}c{n}.ics");
            let reply = fx.put("bob", &uri, &event(&format!("c{n}"), "c")).await;
            (n, reply.status)
        });
    }
    tokio::task::yield_now().await;
    fx.leave_group("team", "bob").await;
    let mut written = Vec::new();
    while let Some(result) = tasks.join_next().await {
        let (n, status) = result.unwrap();
        assert!(matches!(status, 201 | 404), "c{n}: {status}");
        if status == 201 {
            written.push(format!("c{n}.ics"));
        }
    }
    let cal = fx.calendar_id().await;
    let mut stored: Vec<String> = fx
        .store
        .items(cal)
        .await
        .unwrap()
        .into_iter()
        .map(|m| m.name)
        .collect();
    stored.sort();
    written.sort();
    assert_eq!(stored, written);
    assert_eq!(
        fx.put("bob", &format!("{share}late.ics"), &event("late", "x"))
            .await
            .status,
        404
    );
}

// --- CLI ------------------------------------------------------------------

#[test]
fn cli_manages_groups_without_echoing_names() {
    let dir = std::env::temp_dir().join(format!("calcraft-groups-cli-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let db = dir.join("calcraft.db");
    let run = |args: &[&str], stdin: &str| {
        use std::io::Write;
        let mut child = std::process::Command::new(env!("CARGO_BIN_EXE_calcraft"))
            .env_remove("CALCRAFT_CONFIG")
            .env("CALCRAFT_STORAGE__DATABASE", &db)
            .args(args)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(stdin.as_bytes())
            .unwrap();
        let output = child.wait_with_output().unwrap();
        (
            output.status.success(),
            format!(
                "{}{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            ),
        )
    };
    let (ok, out) = run(
        &["user", "add", "zquser", "--password-stdin"],
        "a long password 123\n",
    );
    assert!(ok, "{out}");
    let changes: [&[&str]; 3] = [
        &["group", "add", "zqgroup", "--display-name", "Zq group"],
        &["group", "add-member", "zqgroup", "zquser"],
        &["group", "remove-member", "zqgroup", "zquser"],
    ];
    for args in changes {
        let (ok, out) = run(args, "");
        assert!(ok, "{args:?}: {out}");
        assert!(!out.contains("zqgroup") && !out.contains("zquser"), "{out}");
    }
    let (ok, out) = run(&["group", "add", "zqgroup"], "");
    assert!(!ok && !out.contains("zqgroup"), "{out}");
    let (ok, out) = run(&["group", "add", "Bad/Name"], "");
    assert!(!ok, "{out}");
    run(&["group", "add-member", "zqgroup", "zquser"], "");
    let (ok, out) = run(&["group", "list"], "");
    assert!(ok && out.contains("zqgroup\t1\tZq group"), "{out}");
    let (ok, out) = run(&["group", "list", "zqgroup"], "");
    assert!(ok && out.trim() == "zquser", "{out}");
    let (ok, out) = run(&["group", "remove-member", "zqgroup", "nobody"], "");
    assert!(!ok && !out.contains("nobody"), "{out}");
    let (ok, out) = run(&["group", "remove", "zqgroup"], "");
    assert!(ok && !out.contains("zqgroup"), "{out}");
    let (ok, out) = run(&["group", "list"], "");
    assert!(ok && out.trim().is_empty(), "{out}");
    let _ = std::fs::remove_dir_all(&dir);
}
