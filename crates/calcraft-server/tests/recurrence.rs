//! calendar-query with recurring events: exact instances, time zones from
//! embedded VTIMEZONEs, overrides, budgets and re-indexing on upgrade.
#![allow(clippy::unwrap_used)]

use axum::{
    Router,
    body::Body,
    http::{Method, Request, StatusCode, header},
};
use base64::{Engine, engine::general_purpose::STANDARD};
use calcraft_core::{CollectionKind, Slug};
use calcraft_server::{AppState, Config, auth::hash_password, reindex, router};
use calcraft_store::{DEFAULT_COLLECTION_SLUG, NewItem, Precondition, Store};
use http_body_util::BodyExt;
use tower::ServiceExt;

const ALICE: (&str, &str) = ("alice", "alice password 123");
const CAL: &str = "/dav/calendars/alice/default/";

const BERLIN: &str = "BEGIN:VTIMEZONE\r\nTZID:Europe/Berlin\r\n\
BEGIN:DAYLIGHT\r\nTZOFFSETFROM:+0100\r\nTZOFFSETTO:+0200\r\nTZNAME:CEST\r\n\
DTSTART:19700329T020000\r\nRRULE:FREQ=YEARLY;BYMONTH=3;BYDAY=-1SU\r\nEND:DAYLIGHT\r\n\
BEGIN:STANDARD\r\nTZOFFSETFROM:+0200\r\nTZOFFSETTO:+0100\r\nTZNAME:CET\r\n\
DTSTART:19701025T030000\r\nRRULE:FREQ=YEARLY;BYMONTH=10;BYDAY=-1SU\r\nEND:STANDARD\r\n\
END:VTIMEZONE\r\n";

async fn store() -> Store {
    let store = Store::in_memory().await.unwrap();
    let hash = hash_password(ALICE.1).unwrap();
    store
        .create_user(&ALICE.0.parse().unwrap(), &hash, None)
        .await
        .unwrap();
    store
}

fn app(store: Store) -> Router {
    router(&Config::default(), AppState::new(store).unwrap())
}

async fn send(app: &Router, method: &str, uri: &str, body: &str) -> (StatusCode, String) {
    let auth = format!(
        "Basic {}",
        STANDARD.encode(format!("{}:{}", ALICE.0, ALICE.1))
    );
    let request = Request::builder()
        .method(Method::from_bytes(method.as_bytes()).unwrap())
        .uri(uri)
        .header(header::AUTHORIZATION, auth)
        .header("depth", "1")
        .header(header::CONTENT_TYPE, "text/calendar; charset=utf-8")
        .body(Body::from(body.to_owned()))
        .unwrap();
    let response = app.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let body = response.into_body().collect().await.unwrap().to_bytes();
    (status, String::from_utf8(body.to_vec()).unwrap())
}

fn calendar(body: &str) -> String {
    format!("BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//Test//EN\r\n{body}END:VCALENDAR\r\n")
}

fn event(uid: &str, props: &str) -> String {
    format!("BEGIN:VEVENT\r\nUID:{uid}\r\nDTSTAMP:20260101T000000Z\r\n{props}END:VEVENT\r\n")
}

async fn put(app: &Router, name: &str, body: &str) {
    let (status, reply) = send(app, "PUT", &format!("{CAL}{name}"), &calendar(body)).await;
    assert_eq!(status, StatusCode::CREATED, "{reply}");
}

/// Names of the objects a VEVENT time-range query returns.
async fn query(app: &Router, start: &str, end: &str) -> Vec<String> {
    let body = format!(
        r#"<c:calendar-query xmlns:d="DAV:" xmlns:c="urn:ietf:params:xml:ns:caldav">
        <d:prop><d:getetag/></d:prop>
        <c:filter><c:comp-filter name="VCALENDAR"><c:comp-filter name="VEVENT">
            <c:time-range start="{start}" end="{end}"/>
        </c:comp-filter></c:comp-filter></c:filter>
    </c:calendar-query>"#
    );
    let (status, reply) = send(app, "REPORT", CAL, &body).await;
    assert_eq!(status, StatusCode::MULTI_STATUS, "{reply}");
    let mut names: Vec<String> = reply
        .split("<d:href>")
        .skip(1)
        .filter_map(|part| part.split("</d:href>").next())
        .filter_map(|href| href.strip_prefix(CAL))
        .map(str::to_owned)
        .collect();
    names.sort();
    names
}

#[tokio::test]
async fn recurring_events_match_only_their_instances() {
    let app = app(store().await);
    // Every Monday 10:00 Berlin time, open-ended.
    put(
        &app,
        "weekly.ics",
        &format!(
            "{BERLIN}{}",
            event(
                "weekly",
                "DTSTART;TZID=Europe/Berlin:20260302T100000\r\nDURATION:PT1H\r\nRRULE:FREQ=WEEKLY;BYDAY=MO\r\n"
            )
        ),
    )
    .await;
    // Daily at 12:00 UTC, five times, with an excluded day and an
    // instance moved from 3 to 20 October.
    put(
        &app,
        "daily.ics",
        &format!(
            "{}{}",
            event(
                "daily",
                "DTSTART:20261001T120000Z\r\nDURATION:PT1H\r\nRRULE:FREQ=DAILY;COUNT=5\r\nEXDATE:20261002T120000Z\r\n"
            ),
            event(
                "daily",
                "RECURRENCE-ID:20261003T120000Z\r\nDTSTART:20261020T120000Z\r\nDURATION:PT1H\r\n"
            )
        ),
    )
    .await;
    // A single event for comparison.
    put(
        &app,
        "single.ics",
        &event(
            "single",
            "DTSTART:20261006T080000Z\r\nDTEND:20261006T090000Z\r\n",
        ),
    )
    .await;

    // Monday 5 October 2026, 10:00 CEST = 08:00 UTC.
    assert_eq!(
        query(&app, "20261005T080000Z", "20261005T083000Z").await,
        ["weekly.ics"]
    );
    // Tuesday: neither the weekly event nor anything else.
    assert_eq!(
        query(&app, "20261006T100000Z", "20261006T110000Z").await,
        Vec::<String>::new()
    );
    // DST began on 29 March 2026: the 10:00 instance on Monday 30 March is
    // at 08:00 UTC, not 09:00.
    assert_eq!(
        query(&app, "20260330T090000Z", "20260330T093000Z").await,
        Vec::<String>::new()
    );
    assert_eq!(
        query(&app, "20260330T080000Z", "20260330T083000Z").await,
        ["weekly.ics"]
    );
    // In winter (CET) it is 09:00 UTC.
    assert_eq!(
        query(&app, "20261102T090000Z", "20261102T093000Z").await,
        ["weekly.ics"]
    );
    // EXDATE and the moved instance leave 2 and 3 October free.
    assert_eq!(
        query(&app, "20261002T000000Z", "20261004T000000Z").await,
        Vec::<String>::new()
    );
    assert_eq!(
        query(&app, "20261004T120000Z", "20261004T123000Z").await,
        ["daily.ics"]
    );
    assert_eq!(
        query(&app, "20261020T123000Z", "20261020T124000Z").await,
        ["daily.ics"]
    );
    // After the last instance (5 October) and before the override.
    assert_eq!(
        query(&app, "20261010T000000Z", "20261011T000000Z").await,
        Vec::<String>::new()
    );
    // A whole week: everything.
    assert_eq!(
        query(&app, "20261001T000000Z", "20261008T000000Z").await,
        ["daily.ics", "single.ics", "weekly.ics"]
    );
}

#[tokio::test]
async fn too_large_rules_are_returned_conservatively() {
    let app = app(store().await);
    put(
        &app,
        "minutely.ics",
        &event(
            "minutely",
            "DTSTART:20261001T000000Z\r\nRRULE:FREQ=MINUTELY;INTERVAL=7;COUNT=2000000\r\n",
        ),
    )
    .await;
    put(
        &app,
        "secondly.ics",
        &event(
            "secondly",
            "DTSTART:20261001T000000Z\r\nRRULE:FREQ=SECONDLY\r\n",
        ),
    )
    .await;
    // Far beyond what the budget can expand: might match, so returned.
    let started = std::time::Instant::now();
    assert_eq!(
        query(&app, "20500101T000000Z", "20500101T000001Z").await,
        ["minutely.ics", "secondly.ics"]
    );
    assert!(started.elapsed() < std::time::Duration::from_secs(10));
    // Before DTSTART nothing matches.
    assert_eq!(
        query(&app, "20250101T000000Z", "20250102T000000Z").await,
        Vec::<String>::new()
    );
}

#[tokio::test]
async fn queries_page_through_many_candidates() {
    let store = store().await;
    let user = store
        .user_credentials(&ALICE.0.parse().unwrap())
        .await
        .unwrap()
        .unwrap()
        .user;
    let slug: Slug = DEFAULT_COLLECTION_SLUG.parse().unwrap();
    let collection = store
        .collection(user.id, CollectionKind::Calendar, &slug)
        .await
        .unwrap()
        .unwrap();
    // 1 200 weekly series, half on Mondays and half on Tuesdays, all with
    // the same (hull) index range: more candidates than one storage page.
    for n in 0..1200u32 {
        let (day, weekday) = if n.is_multiple_of(2) {
            ("05", "MO")
        } else {
            ("06", "TU")
        };
        let uid = format!("s{n:04}");
        let content = calendar(&event(
            &uid,
            &format!(
                "DTSTART:202610{day}T100000Z\r\nDURATION:PT1H\r\nRRULE:FREQ=WEEKLY;BYDAY={weekday};COUNT=10\r\n"
            ),
        ));
        let object = calcraft_ical::CalendarObject::parse(content.as_bytes()).unwrap();
        store
            .put_item(
                collection.id,
                &format!("{uid}.ics"),
                &NewItem {
                    uid: &uid,
                    component: "VEVENT",
                    range_start: object.range.start,
                    range_end: object.range.end,
                    recurring: object.recurring,
                    index_version: calcraft_ical::INDEX_VERSION,
                    etag: "\"e\"",
                    content: content.as_bytes(),
                },
                &Precondition::None,
            )
            .await
            .unwrap();
    }
    let app = app(store);
    let mondays = query(&app, "20261012T100000Z", "20261012T110000Z").await;
    assert_eq!(mondays.len(), 600);
    assert!(mondays.iter().all(|name| {
        let n: usize = name[1..5].parse().unwrap();
        n.is_multiple_of(2)
    }));
}

#[tokio::test]
async fn upgrade_reindexes_old_rows() {
    let store = store().await;
    let user = store
        .user_credentials(&ALICE.0.parse().unwrap())
        .await
        .unwrap()
        .unwrap()
        .user;
    let slug: Slug = DEFAULT_COLLECTION_SLUG.parse().unwrap();
    let collection = store
        .collection(user.id, CollectionKind::Calendar, &slug)
        .await
        .unwrap()
        .unwrap();
    // As stored by an older version: unbounded range, index version 0.
    let content = calendar(&event(
        "old",
        "DTSTART:20261001T120000Z\r\nDURATION:PT1H\r\nRRULE:FREQ=DAILY;COUNT=3\r\n",
    ));
    store
        .put_item(
            collection.id,
            "old.ics",
            &NewItem {
                uid: "old",
                component: "VEVENT",
                range_start: None,
                range_end: None,
                recurring: false,
                index_version: 0,
                etag: "\"old\"",
                content: content.as_bytes(),
            },
            &Precondition::None,
        )
        .await
        .unwrap();
    let app = app(store.clone());
    // Before re-indexing: conservative, the object is returned.
    assert_eq!(
        query(&app, "20261101T000000Z", "20261102T000000Z").await,
        ["old.ics"]
    );
    assert_eq!(reindex::run(&store).await.unwrap(), 1);
    // Idempotent.
    assert_eq!(reindex::run(&store).await.unwrap(), 0);
    assert_eq!(
        query(&app, "20261101T000000Z", "20261102T000000Z").await,
        Vec::<String>::new()
    );
    assert_eq!(
        query(&app, "20261003T120000Z", "20261003T130000Z").await,
        ["old.ics"]
    );
    // The object itself is unchanged.
    let (status, body) = send(&app, "GET", &format!("{CAL}old.ics"), "").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, content);
}
