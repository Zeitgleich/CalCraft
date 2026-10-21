//! REST API import and export.
#![cfg(feature = "api")]
#![allow(clippy::unwrap_used)]

use axum::{
    Router,
    body::Body,
    http::{HeaderMap, Method, Request, StatusCode, header},
};
use calcraft_server::{AppState, Config, auth::hash_password, router};
use calcraft_store::Store;
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;

const ALICE: (&str, &str) = ("alice", "alice password 123");
const BOB: (&str, &str) = ("bob", "bob password 1234");

const CALENDAR: &str = concat!(
    "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//Test//EN\r\n",
    "BEGIN:VTIMEZONE\r\nTZID:Europe/Berlin\r\nBEGIN:STANDARD\r\nDTSTART:19701025T030000\r\n",
    "TZOFFSETFROM:+0200\r\nTZOFFSETTO:+0100\r\nEND:STANDARD\r\nEND:VTIMEZONE\r\n",
    "BEGIN:VEVENT\r\nUID:one@example.com\r\nDTSTAMP:20261001T080000Z\r\n",
    "DTSTART;TZID=Europe/Berlin:20261021T100000\r\nDURATION:PT1H\r\n",
    "RRULE:FREQ=WEEKLY;COUNT=4\r\nSUMMARY:Weekly\r\nEND:VEVENT\r\n",
    "BEGIN:VEVENT\r\nUID:one@example.com\r\nDTSTAMP:20261001T080000Z\r\n",
    "RECURRENCE-ID;TZID=Europe/Berlin:20261028T100000\r\n",
    "DTSTART;TZID=Europe/Berlin:20261028T120000\r\nDURATION:PT1H\r\nSUMMARY:Moved\r\nEND:VEVENT\r\n",
    "BEGIN:VEVENT\r\nUID:two@example.com\r\nDTSTAMP:20261001T080000Z\r\n",
    "DTSTART;VALUE=DATE:20261224\r\nSUMMARY:=1+1\r\nEND:VEVENT\r\n",
    "BEGIN:VTODO\r\nUID:three@example.com\r\nDTSTAMP:20261001T080000Z\r\nSUMMARY:Task\r\nEND:VTODO\r\n",
    // Rejected by validation: end before start.
    "BEGIN:VEVENT\r\nUID:bad@example.com\r\nDTSTAMP:20261001T080000Z\r\n",
    "DTSTART:20261021T100000Z\r\nDTEND:20261020T100000Z\r\nEND:VEVENT\r\n",
    // Rejected: journals are not in the calendar's component set.
    "BEGIN:VJOURNAL\r\nUID:journal@example.com\r\nDTSTAMP:20261001T080000Z\r\nEND:VJOURNAL\r\n",
    "END:VCALENDAR\r\n",
);

async fn app_with(config: &Config) -> Router {
    let store = Store::in_memory().await.unwrap();
    for (user, password) in [ALICE, BOB] {
        let hash = hash_password(password).unwrap();
        store
            .create_user(&user.parse().unwrap(), &hash, None)
            .await
            .unwrap();
    }
    router(config, AppState::new(store).unwrap())
}

async fn app() -> Router {
    app_with(&Config::default()).await
}

struct Reply {
    status: StatusCode,
    headers: HeaderMap,
    body: Vec<u8>,
}

impl Reply {
    fn json(&self) -> Value {
        serde_json::from_slice(&self.body).unwrap_or(Value::Null)
    }

    fn text(&self) -> &str {
        std::str::from_utf8(&self.body).unwrap()
    }
}

async fn send(
    app: &Router,
    method: Method,
    uri: &str,
    cookie: Option<&str>,
    csrf: bool,
    body: Body,
) -> Reply {
    let mut builder = Request::builder().method(method).uri(uri);
    if csrf {
        builder = builder.header("x-calcraft-request", "1");
    }
    if let Some(cookie) = cookie {
        builder = builder.header(header::COOKIE, cookie);
    }
    let response = app
        .clone()
        .oneshot(builder.body(body).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let headers = response.headers().clone();
    let body = response
        .into_body()
        .collect()
        .await
        .unwrap()
        .to_bytes()
        .to_vec();
    Reply {
        status,
        headers,
        body,
    }
}

async fn login(app: &Router, (user, password): (&str, &str)) -> String {
    let request = Request::builder()
        .method(Method::POST)
        .uri("/api/v1/auth/login")
        .header("x-calcraft-request", "1")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(
            json!({"username": user, "password": password}).to_string(),
        ))
        .unwrap();
    let response = app.clone().oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let set_cookie = response.headers()[header::SET_COOKIE].to_str().unwrap();
    set_cookie.split(';').next().unwrap().to_owned()
}

async fn create_calendar(app: &Router, cookie: &str, slug: &str) {
    let request = Request::builder()
        .method(Method::POST)
        .uri("/api/v1/collections")
        .header("x-calcraft-request", "1")
        .header(header::COOKIE, cookie)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(
            json!({"kind": "calendar", "slug": slug}).to_string(),
        ))
        .unwrap();
    assert_eq!(
        app.clone().oneshot(request).await.unwrap().status(),
        StatusCode::CREATED
    );
}

async fn import(app: &Router, cookie: &str, path: &str, query: &str, data: &str) -> Reply {
    send(
        app,
        Method::POST,
        &format!("/api/v1/collections/{path}/import{query}"),
        Some(cookie),
        true,
        Body::from(data.to_owned()),
    )
    .await
}

async fn export(app: &Router, cookie: &str, path: &str, query: &str) -> Reply {
    send(
        app,
        Method::GET,
        &format!("/api/v1/collections/{path}/export{query}"),
        Some(cookie),
        false,
        Body::empty(),
    )
    .await
}

#[tokio::test]
async fn import_validates_each_object_and_reports() {
    let app = app().await;
    let alice = login(&app, ALICE).await;

    let reply = import(&app, &alice, "calendar/default", "?format=ics", CALENDAR).await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.text());
    let report = reply.json();
    assert_eq!(report["created"], 3);
    assert_eq!(report["rejected"], 2);
    assert_eq!(report["skipped"], 0);
    let errors = report["errors"].as_array().unwrap();
    assert_eq!(errors.len(), 2);
    assert_eq!(errors[0]["uid"], "bad@example.com");
    assert!(
        errors[0]["reason"]
            .as_str()
            .unwrap()
            .contains("end before start")
    );
    assert!(
        errors[1]["reason"]
            .as_str()
            .unwrap()
            .contains("not supported")
    );

    // Same file again: existing UIDs are skipped.
    let again = import(&app, &alice, "calendar/default", "", CALENDAR)
        .await
        .json();
    assert_eq!(
        (again["created"].clone(), again["skipped"].clone()),
        (json!(0), json!(3))
    );

    // Replace updates the existing objects.
    let changed = CALENDAR.replace("SUMMARY:Weekly", "SUMMARY:Weekly (new)");
    let replaced = import(
        &app,
        &alice,
        "calendar/default",
        "?on_conflict=replace",
        &changed,
    )
    .await
    .json();
    assert_eq!(replaced["replaced"], 3);
    let ics = export(&app, &alice, "calendar/default", "").await;
    assert!(ics.text().contains("SUMMARY:Weekly (new)"));
}

#[tokio::test]
async fn export_in_every_format() {
    let app = app().await;
    let alice = login(&app, ALICE).await;
    import(&app, &alice, "calendar/default", "", CALENDAR).await;

    for (format, content_type, extension) in [
        ("ics", "text/calendar; charset=utf-8", "ics"),
        ("jcal", "application/calendar+json", "json"),
        ("xcal", "application/calendar+xml; charset=utf-8", "xml"),
        ("csv", "text/csv; charset=utf-8", "csv"),
    ] {
        let reply = export(
            &app,
            &alice,
            "calendar/default",
            &format!("?format={format}"),
        )
        .await;
        assert_eq!(reply.status, StatusCode::OK, "{format}");
        assert_eq!(reply.headers[header::CONTENT_TYPE], content_type);
        assert_eq!(
            reply.headers[header::CONTENT_DISPOSITION],
            format!("attachment; filename=\"default.{extension}\"").as_str()
        );
        assert_eq!(reply.headers[header::X_CONTENT_TYPE_OPTIONS], "nosniff");
        assert_eq!(reply.headers[header::CACHE_CONTROL], "no-store");

        // Every export can be imported again into a fresh calendar.
        create_calendar(&app, &alice, format).await;
        let report = import(
            &app,
            &alice,
            &format!("calendar/{format}"),
            &format!("?format={format}"),
            reply.text(),
        )
        .await
        .json();
        // CSV holds events only, so the to-do is not part of it.
        let expected = if format == "csv" { 2 } else { 3 };
        assert_eq!(report["created"], expected, "{format}: {report}");
        assert_eq!(report["rejected"], 0, "{format}: {report}");
    }

    let ics = export(&app, &alice, "calendar/default", "").await;
    let text = ics.text();
    assert!(text.starts_with("BEGIN:VCALENDAR\r\n"));
    assert_eq!(text.matches("BEGIN:VTIMEZONE").count(), 1);
    assert_eq!(text.matches("BEGIN:VEVENT").count(), 3);

    let csv = export(&app, &alice, "calendar/default", "?format=csv").await;
    assert!(csv.text().contains("\"'=1+1\""));

    let jcal = export(&app, &alice, "calendar/default", "?format=jcal").await;
    let value: Value = serde_json::from_slice(&jcal.body).unwrap();
    assert_eq!(value[0], "vcalendar");
}

#[tokio::test]
async fn address_books() {
    let app = app().await;
    let alice = login(&app, ALICE).await;
    let cards = "BEGIN:VCARD\r\nVERSION:4.0\r\nUID:c1\r\nFN:Alice\r\nEMAIL:a@example.com\r\nEND:VCARD\r\nBEGIN:VCARD\r\nVERSION:3.0\r\nFN:No UID\r\nN:UID;No;;;\r\nEND:VCARD\r\nBEGIN:VCARD\r\nVERSION:4.0\r\nUID:c3\r\nEND:VCARD\r\n";
    let report = import(&app, &alice, "addressbook/default", "", cards)
        .await
        .json();
    assert_eq!(report["created"], 2, "{report}");
    assert_eq!(report["rejected"], 1);
    assert!(
        report["errors"][0]["reason"]
            .as_str()
            .unwrap()
            .contains("FN")
    );

    for (format, content_type) in [
        ("vcf", "text/vcard; charset=utf-8"),
        ("jcard", "application/vcard+json"),
        ("csv", "text/csv; charset=utf-8"),
    ] {
        let reply = export(
            &app,
            &alice,
            "addressbook/default",
            &format!("?format={format}"),
        )
        .await;
        assert_eq!(reply.status, StatusCode::OK);
        assert_eq!(reply.headers[header::CONTENT_TYPE], content_type);
        let again = import(
            &app,
            &alice,
            "addressbook/default",
            &format!("?format={format}"),
            reply.text(),
        )
        .await
        .json();
        assert_eq!(again["skipped"], 2, "{format}: {again}");
    }

    // Calendar formats do not fit address books and vice versa.
    for (path, format) in [("addressbook/default", "ics"), ("calendar/default", "vcf")] {
        let reply = export(&app, &alice, path, &format!("?format={format}")).await;
        assert_eq!(reply.status, StatusCode::BAD_REQUEST);
        let reply = import(&app, &alice, path, &format!("?format={format}"), "").await;
        assert_eq!(reply.status, StatusCode::BAD_REQUEST);
    }
}

#[tokio::test]
async fn foreign_and_unknown_collections_are_not_found() {
    let app = app().await;
    let alice = login(&app, ALICE).await;
    let bob = login(&app, BOB).await;
    import(&app, &alice, "calendar/default", "", CALENDAR).await;

    create_calendar(&app, &alice, "private").await;

    for path in [
        "calendar/private",
        "calendar/missing",
        "calendar/..",
        "nothing/default",
    ] {
        let reply = export(&app, &bob, path, "").await;
        assert_eq!(reply.status, StatusCode::NOT_FOUND, "{path}");
        let reply = import(&app, &bob, path, "", CALENDAR).await;
        assert_eq!(reply.status, StatusCode::NOT_FOUND, "{path}");
    }

    // Bob's own calendar is separate: the same UIDs are new there and do
    // not touch Alice's objects.
    let changed = CALENDAR.replace("SUMMARY:Weekly", "SUMMARY:Bob was here");
    let report = import(
        &app,
        &bob,
        "calendar/default",
        "?on_conflict=replace",
        &changed,
    )
    .await
    .json();
    assert_eq!(report["created"], 3);
    assert_eq!(report["replaced"], 0);
    let alice_ics = export(&app, &alice, "calendar/default", "").await;
    assert!(!alice_ics.text().contains("Bob was here"));
    let bob_ics = export(&app, &bob, "calendar/default", "").await;
    assert!(bob_ics.text().contains("Bob was here"));

    // Without a session nothing works.
    let anonymous = send(
        &app,
        Method::GET,
        "/api/v1/collections/calendar/default/export",
        None,
        false,
        Body::empty(),
    )
    .await;
    assert_eq!(anonymous.status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn rejection_reports_contain_no_control_characters() {
    let app = app().await;
    let alice = login(&app, ALICE).await;
    let csv = "name,uid\n,\"\u{1b}]0;pwned\u{7}\u{1b}[2J\r\nx\"\n";
    let report = import(&app, &alice, "addressbook/default", "?format=csv", csv)
        .await
        .json();
    assert_eq!(report["rejected"], 1, "{report}");
    let uid = report["errors"][0]["uid"].as_str().unwrap();
    assert!(!uid.chars().any(char::is_control), "{uid:?}");
    assert!(uid.contains("pwned"));
}

#[tokio::test]
async fn import_requires_csrf_header_and_valid_options() {
    let app = app().await;
    let alice = login(&app, ALICE).await;
    let reply = send(
        &app,
        Method::POST,
        "/api/v1/collections/calendar/default/import",
        Some(&alice),
        false,
        Body::from(CALENDAR),
    )
    .await;
    assert_eq!(reply.status, StatusCode::FORBIDDEN);
    assert_eq!(reply.json()["error"], "csrf");
    let ics = export(&app, &alice, "calendar/default", "").await;
    assert!(!ics.text().contains("BEGIN:VEVENT"));

    for query in ["?on_conflict=overwrite", "?format=pdf", "?format=vcf"] {
        let reply = import(&app, &alice, "calendar/default", query, CALENDAR).await;
        assert_eq!(reply.status, StatusCode::BAD_REQUEST, "{query}");
    }
    let garbage = import(&app, &alice, "calendar/default", "?format=jcal", "[[[").await;
    assert_eq!(garbage.status, StatusCode::BAD_REQUEST);
    assert_eq!(garbage.json()["error"], "invalid_data");
    let xxe = import(
        &app,
        &alice,
        "calendar/default",
        "?format=xcal",
        "<!DOCTYPE i [<!ENTITY x SYSTEM \"file:///etc/passwd\">]><icalendar xmlns=\"urn:ietf:params:xml:ns:icalendar-2.0\">&x;</icalendar>",
    )
    .await;
    assert_eq!(xxe.status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn import_size_limits() {
    // The global body limit applies first (10 MiB by default) …
    let app = app().await;
    let alice = login(&app, ALICE).await;
    let big = "x".repeat(11 * 1024 * 1024);
    let reply = import(&app, &alice, "calendar/default", "", &big).await;
    assert_eq!(reply.status, StatusCode::PAYLOAD_TOO_LARGE);

    // … and the import limit (20 MiB) even if the global limit is higher.
    let mut config = Config::default();
    config.server.max_body_bytes = 64 * 1024 * 1024;
    let app = app_with(&config).await;
    let alice = login(&app, ALICE).await;
    let huge = "x".repeat(calcraft_server::transfer::MAX_IMPORT_SIZE + 1);
    let reply = import(&app, &alice, "calendar/default", "", &huge).await;
    assert_eq!(reply.status, StatusCode::PAYLOAD_TOO_LARGE);
    assert_eq!(reply.json()["error"], "too_large");

    // Too many objects: rejected as a whole, nothing is stored.
    let event = "BEGIN:VEVENT\r\nUID:{}\r\nDTSTAMP:20261001T080000Z\r\nEND:VEVENT\r\n";
    let many = format!(
        "BEGIN:VCALENDAR\r\nVERSION:2.0\r\n{}END:VCALENDAR\r\n",
        (0..=calcraft_server::transfer::MAX_IMPORT_OBJECTS)
            .map(|i| event.replace("{}", &i.to_string()))
            .collect::<String>()
    );
    let reply = import(&app, &alice, "calendar/default", "", &many).await;
    assert_eq!(reply.status, StatusCode::BAD_REQUEST);
    assert!(
        reply.json()["message"]
            .as_str()
            .unwrap()
            .contains("too many objects")
    );
    let ics = export(&app, &alice, "calendar/default", "").await;
    assert!(!ics.text().contains("BEGIN:VEVENT"));
}
