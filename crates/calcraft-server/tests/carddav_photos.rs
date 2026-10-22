//! CardDAV contact photos (size limit, validation, byte-exact storage) and
//! groups (vCard 4.0 `KIND:group`, Apple's `X-ADDRESSBOOKSERVER-KIND`).
#![allow(clippy::unwrap_used)]

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
const BOOK: &str = "/dav/addressbooks/alice/default/";
const MIB: usize = 1024 * 1024;

async fn store_and_app(config: &Config) -> (Store, Router) {
    let store = Store::in_memory().await.unwrap();
    let (user, password) = ALICE;
    let hash = hash_password(password).unwrap();
    store
        .create_user(&user.parse().unwrap(), &hash, None)
        .await
        .unwrap();
    let app = router(config, AppState::new(store.clone()).unwrap());
    (store, app)
}

async fn app_with(config: &Config) -> Router {
    store_and_app(config).await.1
}

async fn app() -> Router {
    app_with(&Config::default()).await
}

fn basic((user, password): (&str, &str)) -> String {
    format!("Basic {}", STANDARD.encode(format!("{user}:{password}")))
}

struct Reply {
    status: StatusCode,
    headers: HeaderMap,
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
        .header(header::AUTHORIZATION, basic(ALICE));
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

async fn put_card(app: &Router, uri: &str, body: &str) -> Reply {
    send(
        app,
        "PUT",
        uri,
        &[("content-type", "text/vcard; charset=utf-8")],
        body,
    )
    .await
}

async fn get(app: &Router, uri: &str) -> Reply {
    send(app, "GET", uri, &[], "").await
}

/// `len` bytes of deterministic pseudo-random "image" data, base64-encoded.
/// It starts with the JPEG magic number, which the server checks.
fn photo_base64(len: usize) -> String {
    let mut state = 0x2545_f491_u32;
    let mut bytes = vec![0xFF, 0xD8, 0xFF, 0xE0];
    bytes.extend((4..len).map(|_| {
        state ^= state << 13;
        state ^= state >> 17;
        state ^= state << 5;
        state.to_le_bytes()[0]
    }));
    STANDARD.encode(bytes)
}

/// Folds a content line at 75 octets (RFC 6350 §3.2), as clients do.
fn fold(line: &str) -> String {
    let mut out = String::new();
    let mut rest = line;
    let mut width = 75;
    while rest.len() > width {
        out.push_str(&rest[..width]);
        out.push_str("\r\n ");
        rest = &rest[width..];
        width = 74;
    }
    out.push_str(rest);
    out.push_str("\r\n");
    out
}

fn v3_with_photo(uid: &str, photo: &str) -> String {
    format!(
        "BEGIN:VCARD\r\nVERSION:3.0\r\nPRODID:-//Apple Inc.//iPhone OS 26.0//EN\r\nN:Example;Paula;;;\r\nFN:Paula Example\r\nUID:{uid}\r\n{}END:VCARD\r\n",
        fold(&format!("PHOTO;ENCODING=b;TYPE=JPEG:{photo}"))
    )
}

/// Base64 of the PNG signature plus one zero byte (12 characters).
const PNG_MAGIC: &str = "iVBORw0KGgoA";

/// A vCard 3.0 with an unfolded PNG photo of exactly `size` bytes.
fn card_of_size(uid: &str, size: usize) -> String {
    let head = format!("BEGIN:VCARD\r\nVERSION:3.0\r\nFN:Big\r\nUID:{uid}\r\nNOTE:");
    let photo = "\r\nPHOTO;ENCODING=b;TYPE=PNG:";
    let tail = "\r\nEND:VCARD\r\n";
    let room = size - head.len() - photo.len() - tail.len();
    let data = room - room % 4;
    let card = format!(
        "{head}{}{photo}{PNG_MAGIC}{}{tail}",
        "x".repeat(room - data),
        "A".repeat(data - PNG_MAGIC.len())
    );
    assert_eq!(card.len(), size);
    card
}

fn is_forbidden_with(reply: &Reply, condition: &str) -> bool {
    reply.status == StatusCode::FORBIDDEN && reply.body.contains(condition)
}

#[tokio::test]
async fn photos_round_trip_byte_exact() {
    let app = app().await;
    // About 750 KiB of base64, folded into ~10 000 continuation lines.
    let v3 = v3_with_photo("photo-v3", &photo_base64(560 * 1024));
    let uri = format!("{BOOK}photo-v3.vcf");
    assert_eq!(put_card(&app, &uri, &v3).await.status, StatusCode::CREATED);
    let fetched = get(&app, &uri).await;
    assert_eq!(fetched.status, StatusCode::OK);
    assert!(fetched.body == v3, "photo vCard was changed");

    // vCard 4.0: data: URI (unfolded, as some clients write it) and a
    // reference that is stored but never fetched.
    let v4 = format!(
        "BEGIN:VCARD\r\nVERSION:4.0\r\nUID:urn:uuid:photo-v4\r\nFN:Quentin\r\nPHOTO:data:image/png;base64,{}\r\nLOGO;MEDIATYPE=image/webp:https://example.com/logo.webp\r\nEND:VCARD\r\n",
        photo_base64(4096)
    );
    let uri = format!("{BOOK}photo-v4.vcf");
    assert_eq!(put_card(&app, &uri, &v4).await.status, StatusCode::CREATED);
    assert!(get(&app, &uri).await.body == v4);

    // The address book advertises the vCard limit, not the calendar one.
    let props = r#"<d:propfind xmlns:d="DAV:" xmlns:card="urn:ietf:params:xml:ns:carddav"><d:prop><card:max-resource-size/></d:prop></d:propfind>"#;
    let reply = send(&app, "PROPFIND", BOOK, &[("depth", "0")], props).await;
    assert!(
        reply
            .body
            .contains(&format!("<card:max-resource-size>{}<", 2 * MIB)),
        "{}",
        reply.body
    );
}

#[tokio::test]
async fn photo_size_limit_is_enforced_and_configurable() {
    let app = app().await;
    let limit = Config::default().max_vcard_bytes();
    assert_eq!(limit, 2 * MIB);

    // Larger than a calendar object may be, exactly at the vCard limit.
    let at_limit = card_of_size("at-limit", limit);
    let uri = format!("{BOOK}at-limit.vcf");
    assert_eq!(
        put_card(&app, &uri, &at_limit).await.status,
        StatusCode::CREATED
    );
    assert!(get(&app, &uri).await.body == at_limit);

    // One byte more is rejected like an oversized calendar object, and
    // nothing is stored.
    let uri = format!("{BOOK}over.vcf");
    let reply = put_card(&app, &uri, &card_of_size("over", limit + 1)).await;
    assert!(
        is_forbidden_with(&reply, "<card:max-resource-size/>"),
        "{} {}",
        reply.status,
        reply.body
    );
    assert_eq!(get(&app, &uri).await.status, StatusCode::NOT_FOUND);

    // A lower limit applies to PUT and is advertised.
    let mut config = Config::default();
    config.storage.max_vcard_bytes = Some(256 * 1024);
    config.validate().unwrap();
    let small = app_with(&config).await;
    let reply = put_card(&small, &uri, &card_of_size("over", 256 * 1024 + 1)).await;
    assert!(is_forbidden_with(&reply, "<card:max-resource-size/>"));
    let reply = put_card(&small, &uri, &card_of_size("fits", 256 * 1024)).await;
    assert_eq!(reply.status, StatusCode::CREATED);
    let props = r#"<d:propfind xmlns:d="DAV:" xmlns:card="urn:ietf:params:xml:ns:carddav"><d:prop><card:max-resource-size/></d:prop></d:propfind>"#;
    let reply = send(&small, "PROPFIND", BOOK, &[("depth", "0")], props).await;
    assert!(
        reply.body.contains("<card:max-resource-size>262144<"),
        "{}",
        reply.body
    );

    // Without an explicit setting, a smaller body limit also caps vCards
    // (no configuration error, so older configurations keep working).
    let mut config = Config::default();
    config.server.max_body_bytes = 512 * 1024;
    config.validate().unwrap();
    let capped = app_with(&config).await;
    let reply = send(&capped, "PROPFIND", BOOK, &[("depth", "0")], props).await;
    assert!(
        reply.body.contains("<card:max-resource-size>524288<"),
        "{}",
        reply.body
    );
    let reply = put_card(&capped, &uri, &card_of_size("over", 512 * 1024 + 1)).await;
    assert!(!reply.status.is_success(), "{}", reply.status);
    let reply = put_card(&capped, &uri, &card_of_size("fits", 512 * 1024)).await;
    assert_eq!(reply.status, StatusCode::CREATED);
    // An explicit value above the body limit is rejected.
    config.storage.max_vcard_bytes = Some(MIB);
    assert!(config.validate().is_err());

    // Calendar objects keep their own 1 MiB limit.
    let event = format!(
        "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//Test//EN\r\nBEGIN:VEVENT\r\nUID:e\r\nDTSTAMP:20261001T000000Z\r\nDTSTART:20261021T100000Z\r\nDESCRIPTION:{}\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n",
        "x".repeat(MIB + 1)
    );
    let reply = send(
        &app,
        "PUT",
        "/dav/calendars/alice/default/e.ics",
        &[("content-type", "text/calendar")],
        &event,
    )
    .await;
    assert!(is_forbidden_with(&reply, "<c:max-resource-size/>"));
}

#[tokio::test]
async fn invalid_or_unsafe_photos_are_rejected() {
    let app = app().await;
    let uri = format!("{BOOK}bad.vcf");
    for (version, photo) in [
        ("3.0", "PHOTO;ENCODING=b;TYPE=JPEG:this is not base64"),
        ("3.0", "PHOTO;ENCODING=b;TYPE=JPEG:/9j/4AAQ="),
        (
            "3.0",
            "PHOTO;ENCODING=b;TYPE=SVG:PHN2ZyBvbmxvYWQ9YWxlcnQoMSk+",
        ),
        ("3.0", "PHOTO;ENCODING=b;TYPE=image/svg+xml:PHN2Zz4="),
        // Untyped embedded data must look like an allowed image.
        ("3.0", "PHOTO;ENCODING=b:PHN2ZyBvbmxvYWQ9YWxlcnQoMSk+"),
        ("3.0", "PHOTO;ENCODING=b:AAAAAAAAAAAAAAAA"),
        ("3.0", "PHOTO;VALUE=uri:javascript:alert(1)"),
        ("4.0", "PHOTO:data:text/html;base64,PHNjcmlwdD4="),
        ("4.0", "PHOTO:data:image/svg+xml;base64,PHN2Zz4="),
        ("4.0", "PHOTO:data:image/jpeg;base64,not*base64"),
        ("4.0", "PHOTO:javascript:alert(document.cookie)"),
        ("4.0", "PHOTO:file:///etc/passwd"),
        ("4.0", "LOGO:ftp://example.com/logo.png"),
        // Every TYPE counts, and the declared type never overrides the bytes.
        ("3.0", "PHOTO;ENCODING=b;TYPE=JPEG;TYPE=SVG+XML:/9j/4AAQ"),
        ("3.0", "PHOTO;ENCODING=b;TYPE=JPEG,SVG:/9j/4AAQ"),
        (
            "3.0",
            "PHOTO;ENCODING=b;TYPE=JPEG:PHN2ZyBvbmxvYWQ9YWxlcnQoMSk+",
        ),
        (
            "3.0",
            "PHOTO;ENCODING=b;ENCODING=QUOTED-PRINTABLE;TYPE=JPEG:/9j/4AAQ",
        ),
        ("3.0", "PHOTO;ENCODING=b;VALUE=uri;TYPE=JPEG:/9j/4AAQ"),
        (
            "4.0",
            "PHOTO:data:image/jpeg;base64,PHN2ZyBvbmxvYWQ9YWxlcnQoMSk+",
        ),
        ("4.0", "PHOTO:data:jpeg;base64,/9j/4AAQ"),
    ] {
        let card = format!(
            "BEGIN:VCARD\r\nVERSION:{version}\r\nUID:bad\r\nFN:Bad\r\n{photo}\r\nEND:VCARD\r\n"
        );
        let reply = put_card(&app, &uri, &card).await;
        assert!(
            is_forbidden_with(&reply, "<card:valid-address-data/>"),
            "{photo}: {} {}",
            reply.status,
            reply.body
        );
    }
    assert_eq!(get(&app, &uri).await.status, StatusCode::NOT_FOUND);
}

/// Exports from iOS, Android and Synology contain empty photos and vCard 3.0
/// photos without `TYPE`; both are stored unchanged.
#[tokio::test]
async fn empty_and_untyped_photos_are_accepted() {
    let app = app().await;
    let jpeg = format!("/9j/4AAQSkZJRgABAQAAAQABAAD{}", photo_base64(3000));
    let png = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR4nGNgYGD4DwABBAEAwS2OUAAAAABJRU5ErkJggg==";
    for (name, photo) in [
        ("empty-v3", "PHOTO;ENCODING=b;TYPE=JPEG:".to_owned()),
        ("empty-v4", "PHOTO:".to_owned()),
        ("empty-logo", "LOGO;VALUE=uri:".to_owned()),
        (
            "untyped-jpeg",
            fold(&format!("PHOTO;ENCODING=b:{jpeg}"))
                .trim_end()
                .to_owned(),
        ),
        ("untyped-png", format!("PHOTO;ENCODING=BASE64:{png}")),
    ] {
        let card = format!(
            "BEGIN:VCARD\r\nVERSION:3.0\r\nFN:{name}\r\nUID:{name}\r\n{photo}\r\nEND:VCARD\r\n"
        );
        let uri = format!("{BOOK}{name}.vcf");
        let reply = put_card(&app, &uri, &card).await;
        assert_eq!(reply.status, StatusCode::CREATED, "{name}: {}", reply.body);
        assert!(get(&app, &uri).await.body == card, "{name} was changed");
    }
}

const APPLE_GROUP: &str = "BEGIN:VCARD\r\nVERSION:3.0\r\nPRODID:-//Apple Inc.//AddressBook 26.0//EN\r\nN:Family\r\nFN:Family\r\nX-ADDRESSBOOKSERVER-KIND:group\r\nX-ADDRESSBOOKSERVER-MEMBER:urn:uuid:2D7B4A8C-0F3E-4E43-9C2B-5A7E1B3D9F01\r\nX-ADDRESSBOOKSERVER-MEMBER:urn:uuid:8E0C5D21-6B4F-4A7D-8E3A-1C9F2B6D4E02\r\nREV:2026-10-22T08:00:00Z\r\nUID:6F1A3B5C-7D9E-4F21-A3B5-C7D9E1F3A5B7\r\nEND:VCARD\r\n";

const V4_GROUP: &str = "BEGIN:VCARD\r\nVERSION:4.0\r\nUID:urn:uuid:3b1c0e6a-9f42-4d1e-8a7b-2c5d6e7f8091\r\nKIND:group\r\nFN:Project team\r\nMEMBER:urn:uuid:2D7B4A8C-0F3E-4E43-9C2B-5A7E1B3D9F01\r\nMEMBER:mailto:external@example.com\r\nEND:VCARD\r\n";

fn member(uid: &str, name: &str) -> String {
    format!("BEGIN:VCARD\r\nVERSION:3.0\r\nN:{name};;;;\r\nFN:{name}\r\nUID:{uid}\r\nEND:VCARD\r\n")
}

fn token_of(body: &str) -> String {
    let start = body.find("<d:sync-token>").unwrap() + "<d:sync-token>".len();
    let end = body[start..].find("</d:sync-token>").unwrap() + start;
    body[start..end].to_owned()
}

fn sync_body(token: &str) -> String {
    format!(
        r#"<d:sync-collection xmlns:d="DAV:" xmlns:card="urn:ietf:params:xml:ns:carddav"><d:sync-token>{token}</d:sync-token><d:sync-level>1</d:sync-level><d:prop><d:getetag/><card:address-data/></d:prop></d:sync-collection>"#
    )
}

#[tokio::test]
async fn groups_round_trip_and_sync() {
    let app = app().await;
    let initial = send(&app, "REPORT", BOOK, &[], &sync_body("")).await;
    let token = token_of(&initial.body);

    // Members and groups; Apple names group resources after their UID.
    for (name, card) in [
        (
            "2D7B4A8C-0F3E-4E43-9C2B-5A7E1B3D9F01.vcf",
            member("2D7B4A8C-0F3E-4E43-9C2B-5A7E1B3D9F01", "Anna"),
        ),
        (
            "8E0C5D21-6B4F-4A7D-8E3A-1C9F2B6D4E02.vcf",
            member("8E0C5D21-6B4F-4A7D-8E3A-1C9F2B6D4E02", "Ben"),
        ),
        (
            "6F1A3B5C-7D9E-4F21-A3B5-C7D9E1F3A5B7.vcf",
            APPLE_GROUP.to_owned(),
        ),
        ("team.vcf", V4_GROUP.to_owned()),
    ] {
        let uri = format!("{BOOK}{name}");
        let reply = put_card(&app, &uri, &card).await;
        assert_eq!(reply.status, StatusCode::CREATED, "{name}: {}", reply.body);
        let etag = reply.headers[header::ETAG].to_str().unwrap().to_owned();
        let fetched = get(&app, &uri).await;
        assert_eq!(fetched.body, card, "{name}");
        assert_eq!(fetched.headers[header::ETAG], etag.as_str());
    }

    // Groups appear with their members in sync-collection and multiget.
    let delta = send(&app, "REPORT", BOOK, &[], &sync_body(&token)).await;
    assert_eq!(delta.status, StatusCode::MULTI_STATUS);
    for expected in [
        "6F1A3B5C-7D9E-4F21-A3B5-C7D9E1F3A5B7.vcf</d:href>",
        "team.vcf</d:href>",
        "X-ADDRESSBOOKSERVER-KIND:group",
        "X-ADDRESSBOOKSERVER-MEMBER:urn:uuid:8E0C5D21-6B4F-4A7D-8E3A-1C9F2B6D4E02",
        "MEMBER:mailto:external@example.com",
    ] {
        assert!(delta.body.contains(expected), "{expected}: {}", delta.body);
    }
    let multiget = r#"<card:addressbook-multiget xmlns:d="DAV:" xmlns:card="urn:ietf:params:xml:ns:carddav">
        <d:prop><d:getetag/><card:address-data/></d:prop>
        <d:href>/dav/addressbooks/alice/default/6F1A3B5C-7D9E-4F21-A3B5-C7D9E1F3A5B7.vcf</d:href>
        <d:href>/dav/addressbooks/alice/default/team.vcf</d:href>
    </card:addressbook-multiget>"#;
    let reply = send(&app, "REPORT", BOOK, &[("depth", "1")], multiget).await;
    assert_eq!(reply.status, StatusCode::MULTI_STATUS);
    assert!(reply.body.contains("KIND:group"), "{}", reply.body);
    assert!(
        reply
            .body
            .contains("X-ADDRESSBOOKSERVER-MEMBER:urn:uuid:2D7B"),
        "{}",
        reply.body
    );
    assert!(!reply.body.contains("404"), "{}", reply.body);

    // Changing the membership is an ordinary update and shows up in the
    // next delta, as does removing a group.
    let token = token_of(&delta.body);
    let fewer = APPLE_GROUP.replace(
        "X-ADDRESSBOOKSERVER-MEMBER:urn:uuid:8E0C5D21-6B4F-4A7D-8E3A-1C9F2B6D4E02\r\n",
        "",
    );
    let uri = format!("{BOOK}6F1A3B5C-7D9E-4F21-A3B5-C7D9E1F3A5B7.vcf");
    assert_eq!(
        put_card(&app, &uri, &fewer).await.status,
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        send(&app, "DELETE", &format!("{BOOK}team.vcf"), &[], "")
            .await
            .status,
        StatusCode::NO_CONTENT
    );
    let delta = send(&app, "REPORT", BOOK, &[], &sync_body(&token)).await;
    assert!(
        !delta
            .body
            .contains("X-ADDRESSBOOKSERVER-MEMBER:urn:uuid:8E0C"),
        "{}",
        delta.body
    );
    assert!(delta.body.contains("<d:href>/dav/addressbooks/alice/default/team.vcf</d:href><d:status>HTTP/1.1 404 Not Found</d:status>"), "{}", delta.body);
    assert!(!delta.body.contains("Anna"), "{}", delta.body);

    // The group UID is unique within the address book like any other.
    let reply = put_card(&app, &format!("{BOOK}copy.vcf"), &fewer).await;
    assert!(is_forbidden_with(&reply, "<card:no-uid-conflict>"));

    // vCard 4.0 MEMBER outside a group is invalid.
    let reply = put_card(
        &app,
        &format!("{BOOK}not-a-group.vcf"),
        &V4_GROUP
            .replace("KIND:group\r\n", "")
            .replace("urn:uuid:3b1c", "urn:uuid:4b1c"),
    )
    .await;
    assert!(is_forbidden_with(&reply, "<card:valid-address-data/>"));
}

/// The quota counts the stored bytes, base64 photos included. A replaced
/// photo counts once, however often it is replaced; distinct large
/// contacts do run into the quota (507). The quota is lowered to 3 MiB via
/// the store's test hook, so the test need not write 512 MiB.
#[tokio::test]
async fn photos_count_towards_the_quota() {
    let (store, app) = store_and_app(&Config::default()).await;
    store.set_quota_for_tests(i64::try_from(3 * MIB).unwrap());

    let uri = format!("{BOOK}portrait.vcf");
    let card = card_of_size("portrait", 2 * MIB);
    // 10 × 2 MiB written in total, far more than the quota.
    for round in 0..10 {
        let reply = put_card(&app, &uri, &card).await;
        assert!(reply.status.is_success(), "round {round}: {}", reply.status);
    }
    let props = r#"<d:propfind xmlns:d="DAV:"><d:prop><d:getcontentlength/></d:prop></d:propfind>"#;
    let reply = send(&app, "PROPFIND", &uri, &[("depth", "0")], props).await;
    assert!(
        reply
            .body
            .contains(&format!("<d:getcontentlength>{}<", 2 * MIB)),
        "{}",
        reply.body
    );

    // Negative control: a second large contact one byte beyond the quota
    // is refused and not stored; exactly up to the quota fits.
    let second = format!("{BOOK}second.vcf");
    let reply = put_card(&app, &second, &card_of_size("second", MIB + 1)).await;
    assert_eq!(reply.status, StatusCode::INSUFFICIENT_STORAGE);
    assert_eq!(get(&app, &second).await.status, StatusCode::NOT_FOUND);
    let reply = put_card(&app, &second, &card_of_size("second", MIB)).await;
    assert_eq!(reply.status, StatusCode::CREATED);
    let reply = put_card(
        &app,
        &format!("{BOOK}third.vcf"),
        &v3_with_photo("third", &photo_base64(16)),
    )
    .await;
    assert_eq!(reply.status, StatusCode::INSUFFICIENT_STORAGE);

    // Deleting moves the contact to the trash, which still counts towards
    // the quota; emptying the trash frees the space.
    assert_eq!(
        send(&app, "DELETE", &uri, &[], "").await.status,
        StatusCode::NO_CONTENT
    );
    let third = v3_with_photo("third", &photo_base64(16));
    let reply = put_card(&app, &format!("{BOOK}third.vcf"), &third).await;
    assert_eq!(reply.status, StatusCode::INSUFFICIENT_STORAGE);
    let alice = store
        .list_users()
        .await
        .unwrap()
        .into_iter()
        .find(|user| user.username.as_str() == "alice")
        .unwrap();
    store.empty_trash(alice.id).await.unwrap();
    let reply = put_card(
        &app,
        &format!("{BOOK}third.vcf"),
        &v3_with_photo("third", &photo_base64(16)),
    )
    .await;
    assert_eq!(reply.status, StatusCode::CREATED);
}
