//! The JSON logs, at the most verbose level, contain no passwords, app
//! passwords, TOTP secrets or codes, recovery codes, session tokens,
//! credentials headers, query strings or calendar and contact contents.
//!
//! This binary holds a single test because it installs the global
//! subscriber (the same one `logging.format = "json"` uses), which also
//! sees events from blocking and worker threads.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::{
    io::Write,
    sync::{Arc, Mutex},
};

use axum::{
    Router,
    body::Body,
    http::{HeaderMap, Method, Request, StatusCode},
};
use base64::{Engine, engine::general_purpose::STANDARD};
use calcraft_server::{AppState, Config, auth::hash_password, router, telemetry};
use calcraft_store::Store;
use http_body_util::BodyExt;
use tower::ServiceExt;
use tracing_subscriber::{EnvFilter, util::SubscriberInitExt};

const ALICE: (&str, &str) = ("alice", "Alice-Secret-Pw-4711");
const MARKER: &str = "Zq7Marker";

/// Collects everything the subscriber writes.
#[derive(Clone, Default)]
struct Capture(Arc<Mutex<Vec<u8>>>);

impl Write for Capture {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

struct Reply {
    status: StatusCode,
    // Only the account flows (feature `api`) read cookies.
    #[cfg_attr(not(feature = "api"), allow(dead_code))]
    headers: HeaderMap,
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
        .uri(uri);
    for (name, value) in headers {
        builder = builder.header(*name, *value);
    }
    let response = app
        .clone()
        .oneshot(builder.body(Body::from(body)).unwrap())
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

fn basic(user: &str, password: &str) -> String {
    format!("Basic {}", STANDARD.encode(format!("{user}:{password}")))
}

async fn dav(
    app: &Router,
    auth: &str,
    method: &str,
    uri: &str,
    content_type: &str,
    body: String,
) -> Reply {
    send(
        app,
        method,
        uri,
        &[
            ("authorization", auth),
            ("content-type", content_type),
            ("depth", "0"),
        ],
        body,
    )
    .await
}

/// Writes events and contacts with marked contents, valid and invalid.
async fn dav_flows(app: &Router, auth: &str, secrets: &mut Vec<String>) {
    let summary = format!("{MARKER}Summary");
    let description = format!("{MARKER}Description");
    let location = format!("{MARKER}Location");
    let event = format!(
        "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//Test//EN\r\nBEGIN:VEVENT\r\nUID:{MARKER}Uid\r\nDTSTAMP:20261001T000000Z\r\nDTSTART:20261021T100000Z\r\nDTEND:20261021T110000Z\r\nSUMMARY:{summary}\r\nDESCRIPTION:{description}\r\nLOCATION:{location}\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n"
    );
    let uri = "/dav/calendars/alice/default/marked.ics";
    let reply = dav(app, auth, "PUT", uri, "text/calendar", event.clone()).await;
    assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.body);
    let reply = dav(app, auth, "GET", uri, "text/calendar", String::new()).await;
    assert!(reply.body.contains(&summary));
    // Rejected objects must not be quoted in the logs either.
    let broken = event.replace(
        "DTSTART:20261021T100000Z",
        &format!("DTSTART:{MARKER}Broken"),
    );
    let reply = dav(
        app,
        auth,
        "PUT",
        "/dav/calendars/alice/default/broken.ics",
        "text/calendar",
        broken,
    )
    .await;
    assert_eq!(reply.status, StatusCode::FORBIDDEN);
    let garbage = format!("BEGIN:VCALENDAR\r\n{MARKER}Garbage\r\n");
    let reply = dav(
        app,
        auth,
        "PUT",
        "/dav/calendars/alice/default/garbage.ics",
        "text/calendar",
        garbage,
    )
    .await;
    assert_eq!(reply.status, StatusCode::FORBIDDEN);

    let card = format!(
        "BEGIN:VCARD\r\nVERSION:4.0\r\nUID:{MARKER}CardUid\r\nFN:{MARKER}Name\r\nEMAIL:{MARKER}mail@example.com\r\nTEL:+49 30 {MARKER}Phone\r\nEND:VCARD\r\n"
    );
    let reply = dav(
        app,
        auth,
        "PUT",
        "/dav/addressbooks/alice/default/marked.vcf",
        "text/vcard",
        card,
    )
    .await;
    assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.body);
    let report = format!(
        r#"<c:calendar-query xmlns:d="DAV:" xmlns:c="urn:ietf:params:xml:ns:caldav"><d:prop><c:calendar-data/></d:prop><c:filter><c:comp-filter name="VCALENDAR"><c:comp-filter name="VEVENT"><c:prop-filter name="SUMMARY"><c:text-match>{MARKER}Search</c:text-match></c:prop-filter></c:comp-filter></c:comp-filter></c:filter></c:calendar-query>"#
    );
    dav(
        app,
        auth,
        "REPORT",
        "/dav/calendars/alice/default/",
        "application/xml",
        report,
    )
    .await;
    let rename = format!(
        r#"<d:propertyupdate xmlns:d="DAV:"><d:set><d:prop><d:displayname>{MARKER}Calendar</d:displayname></d:prop></d:set></d:propertyupdate>"#
    );
    let reply = dav(
        app,
        auth,
        "PROPPATCH",
        "/dav/calendars/alice/default/",
        "application/xml",
        rename,
    )
    .await;
    assert_eq!(reply.status, StatusCode::MULTI_STATUS);

    // Query strings are never logged (they can carry OIDC codes).
    for uri in [
        format!("/dav/calendars/alice/default/marked.ics?token={MARKER}Query1"),
        format!("/healthz?secret={MARKER}Query2"),
        format!("/nope?code={MARKER}Query3"),
    ] {
        dav(app, auth, "GET", &uri, "text/plain", String::new()).await;
    }
    secrets.push(MARKER.to_owned());
}

#[cfg(feature = "api")]
mod api {
    use super::*;
    use axum::http::header;
    use calcraft_server::secrets::{TOTP_STEP, base32_decode, totp_code};
    use serde_json::{Value, json};

    pub fn now() -> i64 {
        calcraft_server::auth::now()
    }

    pub async fn call(
        app: &Router,
        method: &str,
        uri: &str,
        cookie: Option<&str>,
        body: Value,
    ) -> (Reply, Value) {
        let mut headers = vec![
            ("x-calcraft-request", "1"),
            ("content-type", "application/json"),
        ];
        if let Some(cookie) = cookie {
            headers.push(("cookie", cookie));
        }
        let reply = send(app, method, uri, &headers, body.to_string()).await;
        let json = serde_json::from_str(&reply.body).unwrap_or(Value::Null);
        (reply, json)
    }

    pub async fn login(app: &Router, user: &str, password: &str, code: Option<&str>) -> Reply {
        let mut body = json!({"username": user, "password": password});
        if let Some(code) = code {
            body["code"] = json!(code);
        }
        call(app, "POST", "/api/v1/auth/login", None, body).await.0
    }

    /// The `calcraft_session=…` pair; records the token as a secret.
    pub fn cookie(reply: &Reply, secrets: &mut Vec<String>) -> String {
        let set_cookie = reply.headers[header::SET_COOKIE].to_str().unwrap();
        let pair = set_cookie.split(';').next().unwrap().to_owned();
        secrets.push(pair.split_once('=').unwrap().1.to_owned());
        pair
    }

    /// Runs the account flows; returns an app password for DAV.
    pub async fn flows(app: &Router, secrets: &mut Vec<String>) -> String {
        let wrong = format!("{MARKER}WrongPassword");
        secrets.push(wrong.clone());
        assert_eq!(
            login(app, ALICE.0, &wrong, None).await.status,
            StatusCode::UNAUTHORIZED
        );
        let session = cookie(&login(app, ALICE.0, ALICE.1, None).await, secrets);
        let (me, _) = call(
            app,
            "GET",
            &format!("/api/v1/me?x={MARKER}Q"),
            Some(&session),
            Value::Null,
        )
        .await;
        assert_eq!(me.status, StatusCode::OK);

        let (created, json) = call(
            app,
            "POST",
            "/api/v1/app-passwords",
            Some(&session),
            json!({"name": "Phone"}),
        )
        .await;
        assert_eq!(created.status, StatusCode::CREATED);
        let app_password = json["password"].as_str().unwrap().to_owned();
        secrets.push(app_password.clone());
        secrets.push(app_password.rsplit('_').next().unwrap().to_owned());

        // TOTP: setup, enable, login with code, recovery code, wrong code.
        let (setup, json) = call(
            app,
            "POST",
            "/api/v1/me/totp/setup",
            Some(&session),
            Value::Null,
        )
        .await;
        assert_eq!(setup.status, StatusCode::OK);
        let secret_b32 = json["secret"].as_str().unwrap().to_owned();
        secrets.push(secret_b32.clone());
        secrets.push(json["uri"].as_str().unwrap().to_owned());
        let secret = base32_decode(&secret_b32).unwrap();
        let enable_code = totp_code(&secret, now() / TOTP_STEP - 1);
        secrets.push(enable_code.clone());
        let (enabled, json) = call(
            app,
            "POST",
            "/api/v1/me/totp/enable",
            Some(&session),
            json!({"code": enable_code}),
        )
        .await;
        assert_eq!(enabled.status, StatusCode::OK, "{}", enabled.body);
        let recovery: Vec<String> = json["codes"]
            .as_array()
            .unwrap()
            .iter()
            .map(|c| c.as_str().unwrap().to_owned())
            .collect();
        secrets.extend(recovery.iter().cloned());

        let code = totp_code(&secret, now() / TOTP_STEP);
        secrets.push(code.clone());
        let with_code = login(app, ALICE.0, ALICE.1, Some(&code)).await;
        assert_eq!(with_code.status, StatusCode::OK, "{}", with_code.body);
        let session = cookie(&with_code, secrets);
        let replay = login(app, ALICE.0, ALICE.1, Some(&code)).await;
        assert_eq!(replay.status, StatusCode::UNAUTHORIZED);
        let with_recovery = login(app, ALICE.0, ALICE.1, Some(&recovery[0])).await;
        assert_eq!(with_recovery.status, StatusCode::OK);
        cookie(&with_recovery, secrets);

        // Password change (needs a fresh code with 2FA) and admin user creation.
        let new_password = format!("{MARKER}NewPassword");
        secrets.push(new_password.clone());
        let next = totp_code(&secret, now() / TOTP_STEP + 1);
        secrets.push(next.clone());
        let (changed, _) = call(
            app,
            "POST",
            "/api/v1/me/password",
            Some(&session),
            json!({"current": ALICE.1, "new": new_password, "code": next}),
        )
        .await;
        assert_eq!(changed.status, StatusCode::NO_CONTENT, "{}", changed.body);
        let session = cookie(
            &login(app, ALICE.0, &new_password, Some(&recovery[1])).await,
            secrets,
        );
        let carol = format!("{MARKER}CarolPassword");
        secrets.push(carol.clone());
        let (created, _) = call(
            app,
            "POST",
            "/api/v1/admin/users",
            Some(&session),
            json!({"username": "carol", "password": carol}),
        )
        .await;
        assert_eq!(created.status, StatusCode::CREATED, "{}", created.body);
        let (forged, _) = call(
            app,
            "GET",
            "/api/v1/me",
            Some(&format!("calcraft_session={MARKER}Forged")),
            Value::Null,
        )
        .await;
        assert_eq!(forged.status, StatusCode::UNAUTHORIZED);
        app_password
    }
}

/// Fails if a secret occurs in the logs. Timestamps are dropped first, so
/// that six-digit TOTP codes cannot match their fractional seconds.
fn assert_absent(logs: &str, secrets: &[String]) {
    let lines: Vec<String> = logs
        .lines()
        .map(|line| {
            let mut value: serde_json::Value =
                serde_json::from_str(line).unwrap_or_else(|_| panic!("not JSON: {line}"));
            if let Some(object) = value.as_object_mut() {
                object.remove("timestamp");
            }
            value.to_string()
        })
        .collect();
    let lowercase: Vec<String> = lines.iter().map(|l| l.to_lowercase()).collect();
    for secret in secrets {
        let needle = secret.to_lowercase();
        for line in &lowercase {
            let found = line.match_indices(&needle).any(|(at, _)| {
                // Short numeric codes must stand alone, not inside a longer number.
                let digits = needle.bytes().all(|b| b.is_ascii_digit());
                let numeric = |b: Option<u8>| b.is_some_and(|b| b.is_ascii_digit() || b == b'.');
                let before = line[..at].bytes().next_back();
                let after = line[at + needle.len()..].bytes().next();
                !digits || !(numeric(before) || numeric(after))
            });
            assert!(!found, "secret {secret:?} logged: {line}");
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn logs_contain_no_secrets_or_contents() {
    let capture = Capture::default();
    let writer = capture.clone();
    telemetry::json_subscriber(EnvFilter::new("trace"), move || writer.clone())
        .try_init()
        .unwrap();

    let store = Store::in_memory().await.unwrap();
    let hash = hash_password(ALICE.1).unwrap();
    store
        .create_user_with_role(&ALICE.0.parse().unwrap(), &hash, None, true)
        .await
        .unwrap();
    let app = router(&Config::default(), AppState::new(store).unwrap());

    let mut secrets = vec![ALICE.1.to_owned()];
    let password_auth = basic(ALICE.0, ALICE.1);
    let wrong_auth = basic(ALICE.0, &format!("{MARKER}WrongBasic"));
    for auth in [&password_auth, &wrong_auth] {
        secrets.push(auth.clone());
        secrets.push(auth["Basic ".len()..].to_owned());
        dav(
            &app,
            auth,
            "PROPFIND",
            "/dav/",
            "application/xml",
            String::new(),
        )
        .await;
    }
    dav_flows(&app, &password_auth, &mut secrets).await;

    #[cfg(feature = "api")]
    {
        let app_password = api::flows(&app, &mut secrets).await;
        let auth = basic(ALICE.0, &app_password);
        secrets.push(auth["Basic ".len()..].to_owned());
        let reply = dav(
            &app,
            &auth,
            "PROPFIND",
            "/dav/",
            "application/xml",
            String::new(),
        )
        .await;
        assert_eq!(reply.status, StatusCode::MULTI_STATUS, "{}", reply.body);
        // 2FA is on now: the account password no longer works for DAV.
        let reply = dav(
            &app,
            &password_auth,
            "PROPFIND",
            "/dav/",
            "application/xml",
            String::new(),
        )
        .await;
        assert_eq!(reply.status, StatusCode::UNAUTHORIZED);
    }

    let logs = String::from_utf8(capture.0.lock().unwrap().clone()).unwrap();
    // The capture works and is verbose: requests and SQL statements are in it.
    assert!(logs.contains("finished processing request"), "{logs}");
    assert!(logs.contains("INSERT INTO items"), "{logs}");
    assert!(logs.lines().count() > 50);
    assert_absent(&logs, &secrets);
}
