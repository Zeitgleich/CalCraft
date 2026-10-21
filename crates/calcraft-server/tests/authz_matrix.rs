//! Authorization matrix.
//!
//! Every DAV method the server routes is sent to every kind of resource
//! path – owned by the requesting user (alice), by another user (bob) and by
//! a user that does not exist (carol), plus path-traversal and
//! percent-encoding tricks – with every kind of credential. For each request
//! the test asserts
//!
//! * the exact status code (a small model of the DAV semantics for alice's
//!   own resources, `404` for everything that is not hers, `401` without
//!   valid credentials),
//! * that all responses for resources alice may not access are
//!   byte-identical, whether they belong to bob, to nobody or are malformed
//!   (no existence oracle), and that all `401` responses are identical,
//! * that no response contains any of the marker strings planted in bob's
//!   data, unless the request itself contained it (echoed hrefs),
//! * that bob's data (collections, ctags, objects, etags, app passwords,
//!   session) is unchanged after every state-changing request.
//!
//! The REST API part does the same for every route in
//! [`calcraft_server::api::ROUTES`]. Both parts check themselves: a DAV
//! method in [`ALLOWED_METHODS`] or an API route without a matrix entry
//! makes the test fail, so new endpoints cannot be added without
//! authorization tests.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeSet;

use axum::{
    Router,
    body::Body,
    http::{Method, Request},
};
use base64::{Engine, engine::general_purpose::STANDARD};
use calcraft_core::CollectionKind;
use calcraft_server::{
    AppState, Config,
    auth::{hash_password, now},
    dav::ALLOWED_METHODS,
    router,
    secrets::{hash_secret, random_token},
};
use calcraft_store::{Collection, NewItem, Precondition, Store, User};
use http_body_util::BodyExt;
use tower::ServiceExt;

const ALICE_PW: &str = "alice password 123";
const BOB_PW: &str = "bob password 1234";
const KINDS: [CollectionKind; 2] = [CollectionKind::Calendar, CollectionKind::Addressbook];
/// Prefix of every string planted in bob's data.
const MARK: &str = "zqbobmark";

// --- fixture ------------------------------------------------------------

/// Names of one user's seeded data.
struct Names {
    prefix: &'static str,
    cal2: &'static str,
    ab2: &'static str,
    event: &'static str,
    event2: &'static str,
    card: &'static str,
}

const ALICE: Names = Names {
    prefix: "alice",
    cal2: "alice-cal2",
    ab2: "alice-ab2",
    event: "alice-ev1.ics",
    event2: "alice-ev2.ics",
    card: "alice-card.vcf",
};

const BOB: Names = Names {
    prefix: MARK,
    cal2: "zqbobmark-cal2",
    ab2: "zqbobmark-ab2",
    event: "zqbobmark-ev1.ics",
    event2: "zqbobmark-ev2.ics",
    card: "zqbobmark-card.vcf",
};

/// A user that does not exist; paths use bob's names.
const NOBODY: &str = "carol";

struct Fixture {
    app: Router,
    store: Store,
    alice: User,
    bob: User,
    /// `ccap_…` app passwords.
    alice_app: String,
    bob_app: String,
    #[cfg_attr(not(feature = "api"), allow(dead_code))]
    bob_app_id: i64,
    /// Raw session tokens (cookie values).
    alice_session: String,
    bob_session: String,
    /// Current sync token of bob's default calendar.
    bob_sync_token: String,
    markers: Vec<String>,
    bob_snapshot: String,
}

fn event(uid: &str, summary: &str) -> String {
    format!(
        "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//Test//EN\r\nBEGIN:VEVENT\r\nUID:{uid}\r\nDTSTAMP:20261001T000000Z\r\nDTSTART:20261021T100000Z\r\nDTEND:20261021T110000Z\r\nSUMMARY:{summary}\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n"
    )
}

fn vcard(uid: &str, name: &str) -> String {
    format!("BEGIN:VCARD\r\nVERSION:4.0\r\nUID:{uid}\r\nFN:{name}\r\nEND:VCARD\r\n")
}

async fn put_seed(store: &Store, collection: &Collection, prefix: &str, name: &str, content: &str) {
    let uid = format!("{prefix}-uid-{name}");
    let etag = format!("\"{prefix}-etag-{name}\"");
    let component = match collection.kind {
        CollectionKind::Calendar => "VEVENT",
        CollectionKind::Addressbook => "VCARD",
    };
    let content = content.replace("@UID@", &uid);
    let item = NewItem {
        uid: &uid,
        component,
        range_start: None,
        range_end: None,
        etag: &etag,
        content: content.as_bytes(),
    };
    store
        .put_item(collection.id, name, &item, &Precondition::None)
        .await
        .unwrap();
}

/// (Re)creates the canonical collections and objects of a user.
async fn seed(store: &Store, user: &User, n: &Names) {
    for kind in KINDS {
        for collection in store.collections(user.id, kind).await.unwrap() {
            store.delete_collection(collection.id).await.unwrap();
        }
    }
    let p = n.prefix;
    let create = |kind, slug: &'static str, label: &'static str| async move {
        let components = match kind {
            CollectionKind::Calendar => "VEVENT,VTODO",
            CollectionKind::Addressbook => "VCARD",
        };
        store
            .create_collection(
                user.id,
                kind,
                &slug.parse().unwrap(),
                Some(&format!("{p}-{label}name")),
                Some(&format!("{p}-{label}desc")),
                Some("#112233"),
                components,
            )
            .await
            .unwrap()
    };
    let cal = create(CollectionKind::Calendar, "default", "cal").await;
    let cal2 = create(CollectionKind::Calendar, n.cal2, "cal2").await;
    let ab = create(CollectionKind::Addressbook, "default", "ab").await;
    create(CollectionKind::Addressbook, n.ab2, "ab2").await;
    put_seed(
        store,
        &cal,
        p,
        n.event,
        &event("@UID@", &format!("{p}-summary-ev1")),
    )
    .await;
    put_seed(
        store,
        &cal2,
        p,
        n.event2,
        &event("@UID@", &format!("{p}-summary-ev2")),
    )
    .await;
    put_seed(store, &ab, p, n.card, &vcard("@UID@", &format!("{p}-fn"))).await;
}

async fn app_password(store: &Store, user: &User, name: &str) -> (i64, String) {
    let secret = random_token::<20>();
    let id = store
        .create_app_password(user.id, name, &hash_secret(&secret), now())
        .await
        .unwrap();
    (id, format!("ccap_{id}_{secret}"))
}

async fn session(store: &Store, user: &User) -> String {
    let token = format!("{}-session-token-0123456789abcdef", user.username);
    store
        .create_session(user.id, &hash_secret(&token), now(), now() + 3600)
        .await
        .unwrap();
    token
}

/// Everything stored for a user, for before/after comparisons.
async fn snapshot(store: &Store, user: &User, session: &str) -> String {
    let mut out = format!("{:?}\n", store.user_by_id(user.id).await.unwrap());
    for kind in KINDS {
        for collection in store.collections(user.id, kind).await.unwrap() {
            out += &format!("{collection:?}\n");
            for meta in store.items(collection.id).await.unwrap() {
                let item = store.item(collection.id, &meta.name).await.unwrap();
                out += &format!("{item:?}\n");
            }
        }
    }
    out += &format!("{:?}\n", store.app_passwords(user.id).await.unwrap());
    out += &format!(
        "{:?}\n",
        store.session(&hash_secret(session), now()).await.unwrap()
    );
    out += &format!("{:?}\n", store.totp_state(user.id).await.unwrap());
    out
}

async fn fixture() -> Fixture {
    let store = Store::in_memory().await.unwrap();
    let alice = store
        .create_user(
            &"alice".parse().unwrap(),
            &hash_password(ALICE_PW).unwrap(),
            Some("alice-person"),
        )
        .await
        .unwrap();
    let bob = store
        .create_user(
            &"bob".parse().unwrap(),
            &hash_password(BOB_PW).unwrap(),
            Some("zqbobmark-person"),
        )
        .await
        .unwrap();
    seed(&store, &alice, &ALICE).await;
    seed(&store, &bob, &BOB).await;
    let (_, alice_app) = app_password(&store, &alice, "alice-apppw").await;
    let (bob_app_id, bob_app) = app_password(&store, &bob, "zqbobmark-apppw").await;
    let alice_session = session(&store, &alice).await;
    let bob_session = session(&store, &bob).await;

    let mut markers: Vec<String> = [
        MARK,
        "zqbobmark-person",
        "zqbobmark-calname",
        "zqbobmark-caldesc",
        "zqbobmark-cal2",
        "zqbobmark-cal2name",
        "zqbobmark-cal2desc",
        "zqbobmark-abname",
        "zqbobmark-abdesc",
        "zqbobmark-ab2",
        "zqbobmark-ab2name",
        "zqbobmark-summary-ev1",
        "zqbobmark-summary-ev2",
        "zqbobmark-fn",
        "/principals/bob",
        "/calendars/bob",
        "/addressbooks/bob",
    ]
    .iter()
    .map(|m| (*m).to_owned())
    .collect();
    markers.extend([bob_app.clone(), bob_session.clone()]);
    let mut bob_sync_token = String::new();
    for kind in KINDS {
        for collection in store.collections(bob.id, kind).await.unwrap() {
            // The collection identity inside sync tokens.
            markers.push(format!("{}-{}", collection.id, collection.token_salt));
            if kind == CollectionKind::Calendar && collection.slug.as_str() == "default" {
                bob_sync_token = format!(
                    "urn:calcraft:sync:{}-{}:{}",
                    collection.id, collection.token_salt, collection.sync_revision
                );
            }
            for meta in store.items(collection.id).await.unwrap() {
                markers.push(meta.name.clone());
                markers.push(format!("{MARK}-uid-{}", meta.name));
                markers.push(meta.etag.clone());
            }
        }
    }
    assert!(!bob_sync_token.is_empty());

    let bob_snapshot = snapshot(&store, &bob, &bob_session).await;
    let app = router(&Config::default(), AppState::new(store.clone()).unwrap());
    Fixture {
        app,
        store,
        alice,
        bob,
        alice_app,
        bob_app,
        bob_app_id,
        alice_session,
        bob_session,
        bob_sync_token,
        markers,
        bob_snapshot,
    }
}

impl Fixture {
    async fn assert_bob_unchanged(&self, context: &str) {
        let now = snapshot(&self.store, &self.bob, &self.bob_session).await;
        assert_eq!(now, self.bob_snapshot, "bob's data changed after {context}");
    }

    /// Fails if a response contains a marker of bob's data that the request
    /// did not contain itself.
    fn assert_no_leak(&self, request: &str, reply: &Reply, context: &str) {
        let haystack = format!("{:?}\n{}", reply.headers, reply.body);
        for marker in &self.markers {
            assert!(
                request.contains(marker.as_str()) || !haystack.contains(marker.as_str()),
                "{context}: response leaks {marker:?}:\n{haystack}"
            );
        }
    }
}

// --- HTTP helpers -------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
struct Reply {
    status: u16,
    headers: Vec<(String, String)>,
    body: String,
}

async fn send(
    app: &Router,
    method: &str,
    uri: &str,
    headers: &[(String, String)],
    body: &str,
) -> Reply {
    let mut builder = Request::builder()
        .method(Method::from_bytes(method.as_bytes()).unwrap())
        .uri(uri);
    for (name, value) in headers {
        builder = builder.header(name.as_str(), value.as_str());
    }
    let response = app
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

fn basic(user: &str, password: &str) -> String {
    format!("Basic {}", STANDARD.encode(format!("{user}:{password}")))
}

fn header(reply: &Reply, name: &str) -> Option<String> {
    reply
        .headers
        .iter()
        .find(|(k, _)| k == name)
        .map(|(_, v)| v.clone())
}

// --- DAV matrix ---------------------------------------------------------

/// Kinds of DAV resources, seen from alice.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Res {
    Root,
    Principal,
    CalHome,
    AbHome,
    Calendar,
    Addressbook,
    CalObject,
    CardObject,
    UnmappedCal,
    UnmappedAb,
    MissingCalObject,
    MissingCardObject,
    /// Does not resolve for alice: malformed, traversal, or below a
    /// collection that does not exist.
    Invalid,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Owner {
    /// Alice's own resource (or the shared root).
    Own,
    /// Bob's resource.
    Other,
    /// Resource of a user that does not exist.
    Nobody,
    /// Malformed or traversal path.
    Tricky,
}

#[derive(Debug)]
struct Target {
    path: String,
    res: Res,
    owner: Owner,
}

impl Target {
    fn new(path: impl Into<String>, res: Res, owner: Owner) -> Self {
        Self {
            path: path.into(),
            res,
            owner,
        }
    }

    /// Whether alice can reach the resource at all.
    fn resolvable(&self) -> bool {
        self.owner == Owner::Own && self.res != Res::Invalid
    }

    fn is_addressbook_path(&self) -> bool {
        self.path.contains("/addressbooks/")
    }
}

/// All resource paths of one user (`user` in the path, names from `n`).
fn user_targets(user: &str, n: &Names, owner: Owner) -> Vec<Target> {
    let encoded = |name: &str| name.replace('-', "%2D").replace('.', "%2E");
    let cal = format!("/dav/calendars/{user}/default/");
    let ab = format!("/dav/addressbooks/{user}/default/");
    vec![
        Target::new(format!("/dav/principals/{user}/"), Res::Principal, owner),
        Target::new(format!("/dav/principals/{user}"), Res::Principal, owner),
        Target::new(format!("/dav/calendars/{user}/"), Res::CalHome, owner),
        Target::new(format!("/dav/addressbooks/{user}"), Res::AbHome, owner),
        Target::new(cal.clone(), Res::Calendar, owner),
        Target::new(cal.trim_end_matches('/'), Res::Calendar, owner),
        Target::new(
            format!("/dav/calendars/{user}/{}/", n.cal2),
            Res::Calendar,
            owner,
        ),
        Target::new(ab.clone(), Res::Addressbook, owner),
        Target::new(
            format!("/dav/addressbooks/{user}/{}/", n.ab2),
            Res::Addressbook,
            owner,
        ),
        Target::new(format!("{cal}{}", n.event), Res::CalObject, owner),
        Target::new(
            format!("/dav/calendars/{user}/{}/{}", n.cal2, n.event2),
            Res::CalObject,
            owner,
        ),
        Target::new(format!("{cal}{}", encoded(n.event)), Res::CalObject, owner),
        Target::new(format!("{ab}{}", n.card), Res::CardObject, owner),
        Target::new(format!("{ab}{}", encoded(n.card)), Res::CardObject, owner),
        Target::new(
            format!("/dav/calendars/{user}/zz-new/"),
            Res::UnmappedCal,
            owner,
        ),
        Target::new(
            format!("/dav/addressbooks/{user}/zz-new/"),
            Res::UnmappedAb,
            owner,
        ),
        Target::new(format!("{cal}missing.ics"), Res::MissingCalObject, owner),
        // An object name of the other kind is just a missing object.
        Target::new(format!("{ab}{}", n.event), Res::MissingCardObject, owner),
        Target::new(format!("{ab}missing.vcf"), Res::MissingCardObject, owner),
        Target::new(
            format!("/dav/calendars/{user}/zz-new/{}", n.event),
            Res::Invalid,
            owner,
        ),
    ]
}

fn all_targets() -> Vec<Target> {
    let mut targets = vec![
        Target::new("/dav", Res::Root, Owner::Own),
        Target::new("/dav/", Res::Root, Owner::Own),
    ];
    targets.extend(user_targets("alice", &ALICE, Owner::Own));
    targets.extend(user_targets("bob", &BOB, Owner::Other));
    targets.extend(user_targets(NOBODY, &BOB, Owner::Nobody));
    for path in [
        "/dav/principals/",
        "/dav/principals/../calendars/bob/",
        "/dav/calendars/alice/../bob/default/",
        "/dav/calendars/alice/default/../../bob/default/zqbobmark-ev1.ics",
        "/dav/calendars/alice/default/../../../calendars/bob/default/zqbobmark-ev1.ics",
        "/dav/calendars/alice/%2e%2e/bob/default/",
        "/dav/calendars/alice/default/..%2f..%2fbob%2fdefault%2fzqbobmark-ev1.ics",
        "/dav/calendars/alice/default/%2e%2e%2f%2e%2e%2fbob%2fdefault%2fzqbobmark-ev1.ics",
        "/dav/calendars/alice/default/%252e%252e%252fbob",
        "/dav/addressbooks/alice/default/..%5c..%5cbob%5cdefault%5czqbobmark-card.vcf",
        "/dav/calendars/alice/default/zqbobmark-ev1.ics%2f..",
        "/dav/calendars/alice/default/./alice-ev1.ics",
        "/dav/calendars/alice/default/.alice-ev1.ics",
        "/dav/calendars/alice/default/a%00.ics",
        "/dav/calendars/%62ob/default/",
        "/dav/calendars/%62ob/default/zqbobmark-ev1.ics",
        "/dav/calendars/%61lice/default/",
        "/dav/calendars/BOB/default/zqbobmark-ev1.ics",
        "/dav/calendars/Alice/default/",
        "/dav//calendars/bob/default/",
        "/dav/calendars//default/",
        "/dav/calendars/bob/default/zqbobmark-ev1.ics/extra",
        "/dav/calendars/alice/default/x/alice-ev1.ics",
        "/dav/other/bob/default/",
        "/dav/calendars/bob%2fdefault/",
        "/dav/calendars/alice/zqbobmark-cal2/zqbobmark-ev2.ics",
    ] {
        let res = Res::Invalid;
        targets.push(Target::new(path, res, Owner::Tricky));
    }
    targets
}

/// One DAV request kind (method plus body).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Case {
    Options,
    Get,
    Head,
    Put,
    PutIfNoneMatch,
    Delete,
    DeleteIfMatch,
    PropfindD0,
    PropfindD1,
    PropfindAllpropD1,
    Proppatch,
    CalMultiget,
    CalQuery,
    AbMultiget,
    AbQuery,
    SyncInitial,
    SyncForeignToken,
    Mkcalendar,
    MkcolExtended,
    MkcolPlain,
    /// A method the server does not implement.
    Unknown(&'static str),
}

const READ_CASES: &[Case] = &[
    Case::Options,
    Case::Get,
    Case::Head,
    Case::PropfindD0,
    Case::PropfindD1,
    Case::PropfindAllpropD1,
];
const WRITE_CASES: &[Case] = &[
    Case::Put,
    Case::PutIfNoneMatch,
    Case::Delete,
    Case::DeleteIfMatch,
    Case::Proppatch,
    Case::Mkcalendar,
    Case::MkcolExtended,
    Case::MkcolPlain,
];
const REPORT_CASES: &[Case] = &[
    Case::CalMultiget,
    Case::CalQuery,
    Case::AbMultiget,
    Case::AbQuery,
    Case::SyncInitial,
    Case::SyncForeignToken,
];
/// Unimplemented methods, sent to every target.
const UNKNOWN_CASES: &[Case] = &[
    Case::Unknown("MOVE"),
    Case::Unknown("COPY"),
    Case::Unknown("LOCK"),
    Case::Unknown("UNLOCK"),
    Case::Unknown("POST"),
    Case::Unknown("PATCH"),
    Case::Unknown("ACL"),
    Case::Unknown("SEARCH"),
    Case::Unknown("BIND"),
    // Methods are case-sensitive: this is not GET.
    Case::Unknown("get"),
];
/// Further registered HTTP/WebDAV methods (IANA), sent to fewer targets.
const MORE_UNKNOWN_METHODS: &[&str] = &[
    "TRACE",
    "QUERY",
    "MKREDIRECTREF",
    "UPDATEREDIRECTREF",
    "ORDERPATCH",
    "REBIND",
    "UNBIND",
    "LINK",
    "UNLINK",
    "VERSION-CONTROL",
    "REPORTX",
    "CHECKIN",
    "CHECKOUT",
    "UNCHECKOUT",
    "MKWORKSPACE",
    "UPDATE",
    "LABEL",
    "MERGE",
    "BASELINE-CONTROL",
    "MKACTIVITY",
    "PROPFINDX",
    "propfind",
    "put",
    "delete",
];

const PROPS: &str = r#"<d:prop><d:displayname/><d:resourcetype/><d:owner/><d:current-user-principal/><d:principal-URL/><d:sync-token/><cs:getctag/><d:getetag/><d:getcontentlength/><c:calendar-home-set/><card:addressbook-home-set/><c:calendar-description/><card:addressbook-description/><ical:calendar-color/><c:calendar-data/><card:address-data/></d:prop>"#;
const NS: &str = r#"xmlns:d="DAV:" xmlns:c="urn:ietf:params:xml:ns:caldav" xmlns:card="urn:ietf:params:xml:ns:carddav" xmlns:cs="http://calendarserver.org/ns/" xmlns:ical="http://apple.com/ns/ical/""#;
const ATTACK_UID: &str = "zzattack-uid";

impl Case {
    fn method(self) -> &'static str {
        match self {
            Self::Options => "OPTIONS",
            Self::Get => "GET",
            Self::Head => "HEAD",
            Self::Put | Self::PutIfNoneMatch => "PUT",
            Self::Delete | Self::DeleteIfMatch => "DELETE",
            Self::PropfindD0 | Self::PropfindD1 | Self::PropfindAllpropD1 => "PROPFIND",
            Self::Proppatch => "PROPPATCH",
            Self::CalMultiget
            | Self::CalQuery
            | Self::AbMultiget
            | Self::AbQuery
            | Self::SyncInitial
            | Self::SyncForeignToken => "REPORT",
            Self::Mkcalendar => "MKCALENDAR",
            Self::MkcolExtended | Self::MkcolPlain => "MKCOL",
            Self::Unknown(method) => method,
        }
    }

    /// Whether the request could change state if it were authorized.
    fn mutating(self) -> bool {
        !matches!(
            self.method(),
            "OPTIONS" | "GET" | "HEAD" | "PROPFIND" | "REPORT"
        )
    }

    /// Headers and body for a request to `target`.
    fn request(self, fx: &Fixture, target: &Target) -> (Vec<(String, String)>, String) {
        let h = |pairs: &[(&str, &str)]| -> Vec<(String, String)> {
            pairs
                .iter()
                .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
                .collect()
        };
        let xml = ("content-type", "application/xml; charset=utf-8");
        let card = target.is_addressbook_path();
        let hrefs: String = [
            "/dav/calendars/bob/default/zqbobmark-ev1.ics",
            "/dav/calendars/bob/zqbobmark-cal2/zqbobmark-ev2.ics",
            "/dav/addressbooks/bob/default/zqbobmark-card.vcf",
            "http://localhost/dav/calendars/bob/default/zqbobmark-ev1.ics",
            "/dav/calendars/alice/default/../../bob/default/zqbobmark-ev1.ics",
            "/dav/calendars/alice/default/..%2f..%2fbob%2fdefault%2fzqbobmark-ev1.ics",
            "/dav/calendars/alice/default/zqbobmark-ev1.ics",
            "/dav/addressbooks/alice/default/zqbobmark-card.vcf",
            "/dav/calendars/carol/default/zqbobmark-ev1.ics",
            "/dav/calendars/alice/default/alice-ev1.ics",
            "/dav/addressbooks/alice/default/alice-card.vcf",
        ]
        .iter()
        .map(|href| format!("<d:href>{href}</d:href>"))
        .collect();
        let sync = |token: &str| {
            format!(
                r#"<d:sync-collection {NS}><d:sync-token>{token}</d:sync-token><d:sync-level>1</d:sync-level>{PROPS}</d:sync-collection>"#
            )
        };
        match self {
            Self::Options | Self::Get | Self::Head | Self::Delete | Self::MkcolPlain => {
                (Vec::new(), String::new())
            }
            Self::Unknown(_) => (
                h(&[(
                    "destination",
                    "http://localhost/dav/calendars/alice/default/zzmoved.ics",
                )]),
                String::new(),
            ),
            Self::DeleteIfMatch => (h(&[("if-match", "*")]), String::new()),
            Self::Put | Self::PutIfNoneMatch => {
                let (content_type, body) = if card {
                    ("text/vcard", vcard(ATTACK_UID, "zzattack"))
                } else {
                    ("text/calendar", event(ATTACK_UID, "zzattack"))
                };
                let mut headers = vec![("content-type", content_type)];
                if self == Self::PutIfNoneMatch {
                    headers.push(("if-none-match", "*"));
                }
                (h(&headers), body)
            }
            Self::PropfindD0 => (
                h(&[("depth", "0"), xml]),
                format!("<d:propfind {NS}>{PROPS}</d:propfind>"),
            ),
            Self::PropfindD1 => (
                h(&[("depth", "1"), xml]),
                format!("<d:propfind {NS}>{PROPS}</d:propfind>"),
            ),
            Self::PropfindAllpropD1 => (h(&[("depth", "1")]), String::new()),
            Self::Proppatch => (
                h(&[xml]),
                format!(
                    "<d:propertyupdate {NS}><d:set><d:prop><d:displayname>zzattack</d:displayname></d:prop></d:set></d:propertyupdate>"
                ),
            ),
            Self::CalMultiget => (
                h(&[("depth", "1"), xml]),
                format!(
                    "<c:calendar-multiget {NS}><d:prop><d:getetag/><c:calendar-data/></d:prop>{hrefs}</c:calendar-multiget>"
                ),
            ),
            Self::CalQuery => (
                h(&[("depth", "1"), xml]),
                format!(
                    r#"<c:calendar-query {NS}><d:prop><d:getetag/><c:calendar-data/></d:prop><c:filter><c:comp-filter name="VCALENDAR"><c:comp-filter name="VEVENT"/></c:comp-filter></c:filter></c:calendar-query>"#
                ),
            ),
            Self::AbMultiget => (
                h(&[("depth", "1"), xml]),
                format!(
                    "<card:addressbook-multiget {NS}><d:prop><d:getetag/><card:address-data/></d:prop>{hrefs}</card:addressbook-multiget>"
                ),
            ),
            Self::AbQuery => (
                h(&[("depth", "1"), xml]),
                format!(
                    "<card:addressbook-query {NS}><d:prop><d:getetag/><card:address-data/></d:prop></card:addressbook-query>"
                ),
            ),
            Self::SyncInitial => (h(&[xml]), sync("")),
            Self::SyncForeignToken => (h(&[xml]), sync(&fx.bob_sync_token)),
            Self::Mkcalendar => (
                h(&[xml]),
                format!(
                    "<c:mkcalendar {NS}><d:set><d:prop><d:displayname>zzattack</d:displayname></d:prop></d:set></c:mkcalendar>"
                ),
            ),
            Self::MkcolExtended => {
                let resourcetype = if card {
                    "<card:addressbook/>"
                } else {
                    "<c:calendar/>"
                };
                (
                    h(&[xml]),
                    format!(
                        "<d:mkcol {NS}><d:set><d:prop><d:resourcetype><d:collection/>{resourcetype}</d:resourcetype><d:displayname>zzattack</d:displayname></d:prop></d:set></d:mkcol>"
                    ),
                )
            }
        }
    }

    /// Expected status for an authenticated request by alice to her own
    /// resource of kind `res`.
    fn own_status(self, res: Res) -> u16 {
        use Res::*;
        let object = matches!(res, CalObject | CardObject);
        let missing = matches!(res, MissingCalObject | MissingCardObject);
        let collection = matches!(res, Calendar | Addressbook);
        let unmapped = matches!(res, UnmappedCal | UnmappedAb);
        if res == Invalid {
            return if self == Self::Options { 200 } else { 404 };
        }
        match self {
            Self::Options => 200,
            Self::Get | Self::Head => match () {
                () if object => 200,
                () if missing => 404,
                () => 405,
            },
            Self::Put => match () {
                () if object => 204,
                () if missing => 201,
                () => 405,
            },
            Self::PutIfNoneMatch => match () {
                () if object => 412,
                () if missing => 201,
                () => 405,
            },
            Self::Delete | Self::DeleteIfMatch => match () {
                () if object || collection => 204,
                () if missing || unmapped => 404,
                () => 405,
            },
            Self::PropfindD0 | Self::PropfindD1 | Self::PropfindAllpropD1 => {
                if missing || unmapped { 404 } else { 207 }
            }
            Self::Proppatch => {
                if collection {
                    207
                } else {
                    405
                }
            }
            Self::CalMultiget | Self::CalQuery => {
                if res == Calendar {
                    207
                } else {
                    403
                }
            }
            Self::AbMultiget | Self::AbQuery => {
                if res == Addressbook {
                    207
                } else {
                    403
                }
            }
            Self::SyncInitial => {
                if collection {
                    207
                } else {
                    403
                }
            }
            // Bob's token is never accepted for alice's collections.
            Self::SyncForeignToken => 403,
            Self::Mkcalendar => {
                if res == UnmappedCal {
                    201
                } else {
                    405
                }
            }
            Self::MkcolExtended | Self::MkcolPlain => {
                if unmapped {
                    201
                } else {
                    405
                }
            }
            Self::Unknown(_) => 405,
        }
    }
}

/// Credentials sent with a DAV request.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Cred {
    /// Alice's account password.
    Password,
    /// Alice's app password.
    AppPassword,
    None,
    /// Alice's web session cookie (DAV must ignore it).
    SessionCookie,
    /// Alice's name with a wrong app-password secret.
    WrongAppSecret,
    /// Alice's name with bob's (valid) app password.
    OtherUsersAppPassword,
    /// Not Basic, or not decodable.
    Malformed,
    /// Bearer token carrying bob's app password.
    Bearer,
}

/// Cheap credentials (no Argon2 run), in an order that never accumulates
/// enough failures to lock alice out: every failure is followed by a
/// success that resets the counter.
const CREDS: &[Cred] = &[
    Cred::Password,
    Cred::None,
    Cred::WrongAppSecret,
    Cred::AppPassword,
    Cred::OtherUsersAppPassword,
    Cred::SessionCookie,
    Cred::Malformed,
    Cred::Bearer,
];

impl Cred {
    fn valid(self) -> bool {
        matches!(self, Self::Password | Self::AppPassword)
    }

    fn headers(self, fx: &Fixture) -> Vec<(String, String)> {
        let auth = |value: String| vec![("authorization".to_owned(), value)];
        match self {
            Self::Password => auth(basic("alice", ALICE_PW)),
            Self::AppPassword => auth(basic("alice", &fx.alice_app)),
            Self::None => Vec::new(),
            Self::SessionCookie => vec![(
                "cookie".to_owned(),
                format!("calcraft_session={}", fx.alice_session),
            )],
            Self::WrongAppSecret => {
                let id = fx.alice_app.split('_').nth(1).unwrap();
                auth(basic("alice", &format!("ccap_{id}_{}", "a".repeat(32))))
            }
            Self::OtherUsersAppPassword => auth(basic("alice", &fx.bob_app)),
            Self::Malformed => auth("Basic !!!not-base64!!!".to_owned()),
            Self::Bearer => auth(format!("Bearer {}", fx.bob_app)),
        }
    }
}

/// Counts assertions, for the summary line.
#[derive(Default)]
struct Stats {
    requests: usize,
    assertions: usize,
}

/// Runs `cases` against every target with every cheap credential.
async fn run_dav_matrix(cases: &[Case]) -> Stats {
    let fx = fixture().await;
    let targets = all_targets();
    let mut stats = Stats::default();
    // HEAD responses have no body, so they form their own group.
    let mut unauthorized: [Option<Reply>; 2] = [None, None];
    for &case in cases {
        // Responses for everything alice may not access, per credential.
        let mut denied: Vec<(Cred, Reply)> = Vec::new();
        let mut options: Option<Reply> = None;
        for target in &targets {
            for &cred in CREDS {
                let (mut headers, body) = case.request(&fx, target);
                headers.extend(cred.headers(&fx));
                let method = case.method();
                let reply = send(&fx.app, method, &target.path, &headers, &body).await;
                stats.requests += 1;
                let context = format!("{case:?} {} as {cred:?} ({target:?})", target.path);
                let request = format!("{method} {} {headers:?} {body}", target.path);

                let expected = if case == Case::Options {
                    200
                } else if !cred.valid() {
                    401
                } else if target.resolvable() {
                    case.own_status(target.res)
                } else {
                    404
                };
                assert_eq!(reply.status, expected, "{context}: {}", reply.body);
                fx.assert_no_leak(&request, &reply, &context);
                stats.assertions += 2;

                if case == Case::Options {
                    // OPTIONS is public and identical everywhere.
                    assert_eq!(
                        header(&reply, "allow").as_deref(),
                        Some(ALLOWED_METHODS),
                        "{context}"
                    );
                    match &options {
                        Some(first) => assert_eq!(&reply, first, "{context}"),
                        None => options = Some(reply.clone()),
                    }
                    stats.assertions += 2;
                } else if !cred.valid() {
                    // One indistinguishable 401 for every method, path and credential.
                    assert!(header(&reply, "www-authenticate").is_some(), "{context}");
                    match &mut unauthorized[usize::from(method == "HEAD")] {
                        Some(first) => assert_eq!(&reply, first, "{context}"),
                        slot => *slot = Some(reply.clone()),
                    }
                    stats.assertions += 2;
                } else if !target.resolvable() {
                    denied.push((cred, reply.clone()));
                }
                if reply.status == 405 {
                    assert_eq!(
                        header(&reply, "allow").as_deref(),
                        Some(ALLOWED_METHODS),
                        "{context}"
                    );
                    stats.assertions += 1;
                }
                if case == Case::Get && reply.status == 200 {
                    assert!(reply.body.contains("BEGIN:"), "{context}");
                    assert!(reply.body.contains("alice"), "{context}");
                    stats.assertions += 2;
                }

                // Only authenticated requests reach a handler; the others
                // are checked once per target below.
                if cred.valid() && (case.mutating() || target.owner != Owner::Own) {
                    fx.assert_bob_unchanged(&context).await;
                    stats.assertions += 1;
                }
                if case.mutating() && cred.valid() && target.owner == Owner::Own {
                    seed(&fx.store, &fx.alice, &ALICE).await;
                }
            }
            if case.mutating() {
                fx.assert_bob_unchanged(&format!("{case:?} {}", target.path))
                    .await;
                stats.assertions += 1;
            }
        }
        // Bob's resources, resources of nobody and malformed paths: the same
        // response, byte for byte.
        let Some((first_cred, first)) = denied.first() else {
            assert_eq!(case, Case::Options);
            continue;
        };
        for (cred, reply) in &denied {
            assert_eq!(
                reply, first,
                "{case:?}: response as {cred:?} differs from {first_cred:?}"
            );
            stats.assertions += 1;
        }
    }
    fx.assert_bob_unchanged("the matrix").await;
    stats
}

fn report(name: &str, stats: &Stats) {
    println!(
        "authz matrix {name}: {} requests, {} assertions",
        stats.requests, stats.assertions
    );
}

#[tokio::test]
async fn dav_matrix_read_methods() {
    report("read", &run_dav_matrix(READ_CASES).await);
}

#[tokio::test]
async fn dav_matrix_write_methods() {
    report("write", &run_dav_matrix(WRITE_CASES).await);
}

#[tokio::test]
async fn dav_matrix_reports() {
    report("report", &run_dav_matrix(REPORT_CASES).await);
}

#[tokio::test]
async fn dav_matrix_unknown_methods() {
    report("unknown", &run_dav_matrix(UNKNOWN_CASES).await);
}

/// The registered methods the server does not implement must be refused
/// (405 on own resources, 404 elsewhere, 401 without credentials), so that a
/// handler added without updating `ALLOWED_METHODS` is caught here.
#[tokio::test]
async fn dav_other_registered_methods_are_refused() {
    let fx = fixture().await;
    let targets = all_targets();
    let mut stats = Stats::default();
    for method in MORE_UNKNOWN_METHODS {
        assert!(
            !ALLOWED_METHODS.split(", ").any(|m| m == *method),
            "{method} is allowed: add it to the matrix"
        );
        for target in &targets {
            for cred in [Cred::Password, Cred::None] {
                let headers = cred.headers(&fx);
                let reply = send(&fx.app, method, &target.path, &headers, "").await;
                let expected = match () {
                    () if !cred.valid() => 401,
                    () if target.resolvable() => 405,
                    () => 404,
                };
                assert_eq!(
                    reply.status, expected,
                    "{method} {} as {cred:?}",
                    target.path
                );
                fx.assert_no_leak(&target.path, &reply, method);
                stats.requests += 1;
                stats.assertions += 2;
            }
        }
    }
    fx.assert_bob_unchanged("other methods").await;
    report("other methods", &stats);
}

/// Every method in `ALLOWED_METHODS` must be covered by the matrix, and
/// the "unknown" methods must really be unknown.
#[test]
fn dav_matrix_covers_every_allowed_method() {
    let covered: BTreeSet<&str> = [READ_CASES, WRITE_CASES, REPORT_CASES]
        .concat()
        .iter()
        .map(|case| case.method())
        .collect();
    let allowed: BTreeSet<&str> = ALLOWED_METHODS.split(", ").collect();
    let missing: Vec<_> = allowed.difference(&covered).collect();
    assert!(
        missing.is_empty(),
        "DAV methods without authorization matrix coverage: {missing:?}"
    );
    let extra: Vec<_> = covered.difference(&allowed).collect();
    assert!(
        extra.is_empty(),
        "matrix covers methods not routed: {extra:?}"
    );
    for case in UNKNOWN_CASES {
        assert!(!allowed.contains(case.method()), "{case:?} is routed");
    }
}

/// Credentials that need an Argon2 run (wrong password, other user's
/// password, unknown user, alice's password for bob). Authentication
/// happens before any path is looked at, and each request costs an Argon2
/// run, so every method is checked once and every credential on a set of
/// representative paths; all must give the same 401.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn dav_wrong_passwords_are_rejected_everywhere() {
    let fx = fixture().await;
    let targets = all_targets();
    let representative = [
        "/dav/",
        "/dav/calendars/alice/default/",
        "/dav/calendars/alice/default/alice-ev1.ics",
        "/dav/calendars/bob/default/zqbobmark-ev1.ics",
        "/dav/addressbooks/bob/default/",
        "/dav/calendars/carol/default/zqbobmark-ev1.ics",
    ];
    let all_cases: Vec<Case> = [READ_CASES, WRITE_CASES, REPORT_CASES, UNKNOWN_CASES].concat();
    let representative: Vec<&Target> = targets
        .iter()
        .filter(|t| representative.contains(&t.path.as_str()))
        .collect();
    let creds = [
        basic("alice", "wrong password 123"),
        basic("alice", BOB_PW),
        basic(NOBODY, ALICE_PW),
        basic("bob", ALICE_PW),
    ];
    // Every method once (rotating credential and path), and every
    // credential on every representative path. Each request costs an Argon2
    // run, so this is kept small.
    let mut jobs: Vec<(Case, &Target, &String)> = Vec::new();
    for (i, case) in all_cases
        .iter()
        .filter(|c| **c != Case::Options)
        .enumerate()
    {
        jobs.push((
            *case,
            representative[i % representative.len()],
            &creds[i % creds.len()],
        ));
    }
    for target in &representative {
        for cred in &creds {
            jobs.push((Case::PropfindD1, target, cred));
        }
    }
    let mut set = tokio::task::JoinSet::new();
    let mut count = 0;
    for (case, target, cred) in jobs {
        let (mut headers, body) = case.request(&fx, target);
        headers.push(("authorization".to_owned(), cred.clone()));
        let app = fx.app.clone();
        let path = target.path.clone();
        let method = case.method();
        count += 1;
        set.spawn(async move {
            let reply = send(&app, method, &path, &headers, &body).await;
            (method, format!("{method} {path} {headers:?} {body}"), reply)
        });
    }
    let mut first: [Option<Reply>; 2] = [None, None];
    while let Some(result) = set.join_next().await {
        let (method, request, reply) = result.unwrap();
        assert_eq!(reply.status, 401, "{request}");
        fx.assert_no_leak(&request, &reply, &request);
        match &mut first[usize::from(method == "HEAD")] {
            Some(first) => assert_eq!(&reply, first, "{request}"),
            slot => *slot = Some(reply),
        }
    }
    fx.assert_bob_unchanged("wrong passwords").await;
    println!("authz matrix wrong passwords: {count} requests");
}

// --- REST API matrix ----------------------------------------------------

#[cfg(feature = "api")]
mod api_matrix {
    use std::collections::BTreeSet;

    use calcraft_core::api::CSRF_HEADER;
    use calcraft_server::{api::ROUTES, auth::hash_password};
    use serde_json::{Value, json};

    use super::{BOB_PW, Fixture, Reply, fixture, send, session, snapshot};

    /// One REST route, as alice (no admin) would use it against bob.
    struct ApiCase {
        method: &'static str,
        route: &'static str,
        /// Concrete URI, pointing at bob's resource where the route has one.
        uri: String,
        /// The same request for a resource that does not exist: the answer
        /// must be identical.
        missing: Option<String>,
        body: Option<Value>,
        /// Reachable without a session.
        public: bool,
        /// Status without a session.
        anonymous: u16,
        /// Status with alice's session.
        alice: u16,
    }

    fn case(method: &'static str, route: &'static str, alice: u16) -> ApiCase {
        ApiCase {
            method,
            route,
            uri: format!("/api{route}"),
            missing: None,
            body: None,
            public: false,
            anonymous: 401,
            alice,
        }
    }

    impl ApiCase {
        fn uri(mut self, uri: String, missing: String) -> Self {
            self.uri = uri;
            self.missing = Some(missing);
            self
        }

        fn body(mut self, body: Value) -> Self {
            self.body = Some(body);
            self
        }

        fn public(mut self, anonymous: u16) -> Self {
            self.public = true;
            self.anonymous = anonymous;
            self
        }
    }

    /// The matrix, in execution order. Every route of `ROUTES` must appear
    /// exactly once; logout comes last because it ends alice's session.
    fn cases(bob_app: &str, bob_app_id: i64) -> Vec<ApiCase> {
        let bob_app_uri = format!("/api/v1/app-passwords/{bob_app_id}");
        let bob_cal = "/api/v1/collections/calendar/zqbobmark-cal2".to_owned();
        let no_cal = "/api/v1/collections/calendar/zz-missing".to_owned();
        vec![
            case("get", "/v1/version", 200).public(200),
            // Web login never accepts app passwords.
            case("post", "/v1/auth/login", 401)
                .public(401)
                .body(json!({"username": "bob", "password": bob_app})),
            case("get", "/v1/me", 200),
            // Another user's password does not authorize a change.
            case("post", "/v1/me/password", 401)
                .body(json!({"current": BOB_PW, "new": "zzattack password 123"})),
            case("post", "/v1/me/totp/setup", 200),
            case("post", "/v1/me/totp/enable", 401).body(json!({"code": "zzzzzz"})),
            case("post", "/v1/me/totp/disable", 409)
                .body(json!({"password": BOB_PW, "code": "123456"})),
            case("get", "/v1/app-passwords", 200),
            case("post", "/v1/app-passwords", 201).body(json!({"name": "zzattack"})),
            case("delete", "/v1/app-passwords/{id}", 404)
                .uri(bob_app_uri, "/api/v1/app-passwords/999999".to_owned()),
            case("get", "/v1/collections", 200),
            // Slugs are per user: alice gets her own collection.
            case("post", "/v1/collections", 201)
                .body(json!({"kind": "addressbook", "slug": "zqbobmark-ab2"})),
            case("patch", "/v1/collections/{kind}/{slug}", 404)
                .uri(bob_cal.clone(), no_cal.clone())
                .body(json!({"display_name": "zzattack"})),
            case("delete", "/v1/collections/{kind}/{slug}", 404).uri(bob_cal, no_cal),
            case("get", "/v1/admin/users", 403),
            case("post", "/v1/admin/users", 403).body(json!({
                "username": "mallory",
                "password": "mallory password 1",
                "is_admin": true
            })),
            case("patch", "/v1/admin/users/{username}", 403)
                .uri(
                    "/api/v1/admin/users/bob".to_owned(),
                    "/api/v1/admin/users/nobody-zz".to_owned(),
                )
                .body(json!({"disabled": true})),
            case("post", "/v1/auth/logout", 204),
        ]
    }

    async fn call(
        fx: &Fixture,
        method: &str,
        uri: &str,
        cookie: Option<&str>,
        csrf: bool,
        body: Option<&Value>,
    ) -> (String, Reply) {
        let mut headers = Vec::new();
        if csrf {
            headers.push((CSRF_HEADER.to_owned(), "1".to_owned()));
        }
        if let Some(token) = cookie {
            headers.push(("cookie".to_owned(), format!("calcraft_session={token}")));
        }
        let body = body.map(Value::to_string).unwrap_or_default();
        if !body.is_empty() {
            headers.push(("content-type".to_owned(), "application/json".to_owned()));
        }
        let method = method.to_ascii_uppercase();
        let reply = send(&fx.app, &method, uri, &headers, &body).await;
        (format!("{method} {uri} {headers:?} {body}"), reply)
    }

    fn error_code(reply: &Reply) -> String {
        serde_json::from_str::<Value>(&reply.body).unwrap_or(Value::Null)["error"]
            .as_str()
            .unwrap_or_default()
            .to_owned()
    }

    #[test]
    fn api_matrix_covers_every_route() {
        let table = cases("ccap_1_x", 1);
        let covered: Vec<(&str, &str)> = table.iter().map(|c| (c.method, c.route)).collect();
        let unique: BTreeSet<_> = covered.iter().copied().collect();
        assert_eq!(unique.len(), covered.len(), "duplicate matrix entries");
        let routes: BTreeSet<(&str, &str)> = ROUTES.iter().copied().collect();
        let missing: Vec<_> = routes.difference(&unique).collect();
        assert!(
            missing.is_empty(),
            "API routes without authorization matrix coverage: {missing:?}"
        );
        let extra: Vec<_> = unique.difference(&routes).collect();
        assert!(extra.is_empty(), "matrix covers unknown routes: {extra:?}");
    }

    #[tokio::test]
    async fn api_matrix() {
        let fx = fixture().await;
        let admin = fx
            .store
            .create_user_with_role(
                &"root".parse().unwrap(),
                &hash_password("root password 123").unwrap(),
                None,
                true,
            )
            .await
            .unwrap();
        let admin_session = session(&fx.store, &admin).await;
        let mut requests = 0;
        let mut assertions = 0;
        let mut unauthorized: Option<Reply> = None;
        let mut csrf: Option<Reply> = None;

        for case in cases(&fx.bob_app, fx.bob_app_id) {
            let method = case.method;
            let body = case.body.as_ref();
            let context = format!("{method} {}", case.route);

            // No session, and a forged session cookie.
            for cookie in [None, Some("forged-session-token")] {
                let (request, reply) = call(&fx, method, &case.uri, cookie, true, body).await;
                requests += 1;
                fx.assert_no_leak(&request, &reply, &context);
                assert_eq!(
                    reply.status, case.anonymous,
                    "{context} anonymous: {}",
                    reply.body
                );
                assertions += 2;
                if !case.public {
                    assert_eq!(error_code(&reply), "unauthorized", "{context}");
                    match &unauthorized {
                        Some(first) => assert_eq!(&reply, first, "{context}"),
                        None => unauthorized = Some(reply.clone()),
                    }
                    assertions += 2;
                }
            }

            // Missing CSRF header: refused before anything else, with or
            // without session, and nothing changes.
            if method != "get" {
                let alice_before = snapshot(&fx.store, &fx.alice, &fx.alice_session).await;
                let admin_before = snapshot(&fx.store, &admin, &admin_session).await;
                for cookie in [None, Some(fx.alice_session.as_str()), Some(&admin_session)] {
                    let (request, reply) = call(&fx, method, &case.uri, cookie, false, body).await;
                    requests += 1;
                    fx.assert_no_leak(&request, &reply, &context);
                    assert_eq!(reply.status, 403, "{context} without CSRF header");
                    assert_eq!(error_code(&reply), "csrf", "{context}");
                    match &csrf {
                        Some(first) => assert_eq!(&reply, first, "{context}"),
                        None => csrf = Some(reply.clone()),
                    }
                    assertions += 4;
                }
                let alice_after = snapshot(&fx.store, &fx.alice, &fx.alice_session).await;
                assert_eq!(alice_before, alice_after, "{context} without CSRF header");
                let admin_after = snapshot(&fx.store, &admin, &admin_session).await;
                assert_eq!(admin_before, admin_after, "{context} without CSRF header");
                fx.assert_bob_unchanged(&format!("{context} without CSRF header"))
                    .await;
                assertions += 3;
            }

            // Alice's session against bob's resources, and against
            // resources that do not exist: identical answers.
            let missing = match &case.missing {
                Some(uri) => {
                    Some(call(&fx, method, uri, Some(&fx.alice_session), true, body).await)
                }
                None => None,
            };
            let (request, reply) =
                call(&fx, method, &case.uri, Some(&fx.alice_session), true, body).await;
            requests += 1;
            fx.assert_no_leak(&request, &reply, &context);
            assert_eq!(
                reply.status, case.alice,
                "{context} as alice: {}",
                reply.body
            );
            fx.assert_bob_unchanged(&format!("{context} as alice"))
                .await;
            assertions += 3;
            if let Some((missing_request, missing_reply)) = missing {
                requests += 1;
                fx.assert_no_leak(&missing_request, &missing_reply, &context);
                assert_eq!(
                    reply, missing_reply,
                    "{context}: bob's resource is distinguishable from a missing one"
                );
                assertions += 2;
            }
        }

        // Alice's session has ended with logout.
        let (_, me) = call(
            &fx,
            "get",
            "/api/v1/me",
            Some(&fx.alice_session),
            true,
            None,
        )
        .await;
        assert_eq!(me.status, 401);

        // A non-admin cannot promote herself.
        let alice2 = session(&fx.store, &fx.alice).await;
        let promote = json!({"is_admin": true});
        let (_, reply) = call(
            &fx,
            "patch",
            "/api/v1/admin/users/alice",
            Some(&alice2),
            true,
            Some(&promote),
        )
        .await;
        assert_eq!(reply.status, 403);
        let alice = fx.store.user_by_id(fx.alice.id).await.unwrap().unwrap();
        assert!(!alice.is_admin);

        // The admin routes do work for an admin, so the 403s above are
        // authorization decisions.
        let (_, reply) = call(
            &fx,
            "get",
            "/api/v1/admin/users",
            Some(&admin_session),
            true,
            None,
        )
        .await;
        assert_eq!(reply.status, 200);
        let (_, reply) = call(
            &fx,
            "patch",
            "/api/v1/admin/users/nobody-zz",
            Some(&admin_session),
            true,
            Some(&json!({"disabled": true})),
        )
        .await;
        assert_eq!(reply.status, 404);

        // Bob's session still works.
        let (_, me) = call(&fx, "get", "/api/v1/me", Some(&fx.bob_session), true, None).await;
        assert_eq!(me.status, 200);
        assert!(me.body.contains("\"bob\""));
        fx.assert_bob_unchanged("the API matrix").await;
        println!("authz matrix api: {requests} requests, {assertions} assertions");
    }
}
