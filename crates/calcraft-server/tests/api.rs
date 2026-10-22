#![cfg(feature = "api")]
#![allow(clippy::unwrap_used)]

use axum::{
    Router,
    body::Body,
    http::{HeaderMap, Method, Request, StatusCode, header},
};
use base64::{Engine, engine::general_purpose::STANDARD};
use calcraft_server::{
    AppState, Config,
    auth::hash_password,
    router,
    secrets::{TOTP_STEP, base32_decode, totp_code},
};
use calcraft_store::Store;
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;

const ALICE: (&str, &str) = ("alice", "alice password 123");
const BOB: (&str, &str) = ("bob", "bob password 1234");

async fn app() -> Router {
    let store = Store::in_memory().await.unwrap();
    for (user, password) in [ALICE, BOB] {
        let hash = hash_password(password).unwrap();
        store
            .create_user(&user.parse().unwrap(), &hash, None)
            .await
            .unwrap();
    }
    store
        .set_admin(&"alice".parse().unwrap(), true)
        .await
        .unwrap();
    router(&Config::default(), AppState::new(store).unwrap())
}

struct Reply {
    status: StatusCode,
    headers: HeaderMap,
    json: Value,
}

async fn call(
    app: &Router,
    method: Method,
    uri: &str,
    cookie: Option<&str>,
    body: Option<Value>,
) -> Reply {
    let mut builder = Request::builder()
        .method(method)
        .uri(uri)
        .header("x-calcraft-request", "1");
    if let Some(cookie) = cookie {
        builder = builder.header(header::COOKIE, cookie);
    }
    let body = match body {
        Some(body) => {
            builder = builder.header(header::CONTENT_TYPE, "application/json");
            Body::from(body.to_string())
        }
        None => Body::empty(),
    };
    let response = app
        .clone()
        .oneshot(builder.body(body).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let headers = response.headers().clone();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let json = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    Reply {
        status,
        headers,
        json,
    }
}

/// Logs in and returns the cookie pair `calcraft_session=…`.
async fn login(app: &Router, (user, password): (&str, &str), code: Option<&str>) -> Reply {
    let mut body = json!({"username": user, "password": password});
    if let Some(code) = code {
        body["code"] = json!(code);
    }
    call(app, Method::POST, "/api/v1/auth/login", None, Some(body)).await
}

fn cookie_of(reply: &Reply) -> String {
    let set_cookie = reply.headers[header::SET_COOKIE].to_str().unwrap();
    set_cookie.split(';').next().unwrap().to_owned()
}

async fn dav(app: &Router, user: &str, password: &str) -> StatusCode {
    let request = Request::builder()
        .method(Method::from_bytes(b"PROPFIND").unwrap())
        .uri("/dav/")
        .header("depth", "0")
        .header(
            header::AUTHORIZATION,
            format!("Basic {}", STANDARD.encode(format!("{user}:{password}"))),
        )
        .body(Body::empty())
        .unwrap();
    app.clone().oneshot(request).await.unwrap().status()
}

fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs()
        .try_into()
        .unwrap()
}

#[tokio::test]
async fn login_session_and_logout() {
    let app = app().await;
    let wrong = login(&app, ("alice", "wrong password!!"), None).await;
    assert_eq!(wrong.status, StatusCode::UNAUTHORIZED);
    assert_eq!(wrong.json["error"], "invalid_credentials");

    let ok = login(&app, ALICE, None).await;
    assert_eq!(ok.status, StatusCode::OK);
    assert_eq!(ok.json["username"], "alice");
    let set_cookie = ok.headers[header::SET_COOKIE].to_str().unwrap();
    for attr in ["HttpOnly", "SameSite=Strict", "Secure", "Path=/"] {
        assert!(set_cookie.contains(attr), "{set_cookie}");
    }
    let cookie = cookie_of(&ok);

    let me = call(&app, Method::GET, "/api/v1/me", Some(&cookie), None).await;
    assert_eq!(me.status, StatusCode::OK);
    assert_eq!(me.json["is_admin"], true);

    let out = call(
        &app,
        Method::POST,
        "/api/v1/auth/logout",
        Some(&cookie),
        None,
    )
    .await;
    assert_eq!(out.status, StatusCode::NO_CONTENT);
    let me = call(&app, Method::GET, "/api/v1/me", Some(&cookie), None).await;
    assert_eq!(me.status, StatusCode::UNAUTHORIZED);
    let forged = call(
        &app,
        Method::GET,
        "/api/v1/me",
        Some("calcraft_session=forged"),
        None,
    )
    .await;
    assert_eq!(forged.status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn csrf_header_and_json_are_enforced() {
    let app = app().await;
    let request = Request::post("/api/v1/auth/login")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(
            json!({"username": "alice", "password": ALICE.1}).to_string(),
        ))
        .unwrap();
    let response = app.clone().oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);

    let unknown = call(
        &app,
        Method::POST,
        "/api/v1/auth/login",
        None,
        Some(json!({"username": "alice", "password": ALICE.1, "is_admin": true})),
    )
    .await;
    assert_eq!(unknown.status, StatusCode::BAD_REQUEST);
    assert_eq!(unknown.json["error"], "invalid_json");

    let request = Request::post("/api/v1/auth/login")
        .header("x-calcraft-request", "1")
        .header(header::CONTENT_TYPE, "text/plain")
        .body(Body::from("username=alice"))
        .unwrap();
    assert_eq!(
        app.clone().oneshot(request).await.unwrap().status(),
        StatusCode::BAD_REQUEST
    );
}

#[tokio::test]
async fn dav_ignores_session_cookies() {
    let app = app().await;
    let cookie = cookie_of(&login(&app, ALICE, None).await);
    let request = Request::builder()
        .method(Method::from_bytes(b"PROPFIND").unwrap())
        .uri("/dav/")
        .header("depth", "0")
        .header(header::COOKIE, cookie)
        .body(Body::empty())
        .unwrap();
    assert_eq!(
        app.clone().oneshot(request).await.unwrap().status(),
        StatusCode::UNAUTHORIZED
    );
}

#[tokio::test]
async fn collections_crud_is_scoped_to_the_user() {
    let app = app().await;
    let alice = cookie_of(&login(&app, ALICE, None).await);
    let bob = cookie_of(&login(&app, BOB, None).await);

    let list = call(&app, Method::GET, "/api/v1/collections", Some(&alice), None).await;
    assert_eq!(list.json.as_array().unwrap().len(), 2);
    assert_eq!(list.json[0]["href"], "/dav/calendars/alice/default/");

    let created = call(
        &app,
        Method::POST,
        "/api/v1/collections",
        Some(&alice),
        Some(
            json!({"kind": "calendar", "slug": "work", "display_name": "Work", "color": "#336699"}),
        ),
    )
    .await;
    assert_eq!(created.status, StatusCode::CREATED);
    assert_eq!(created.json["components"], json!(["VEVENT", "VTODO"]));

    for (body, field) in [
        (json!({"kind": "calendar", "slug": "../x"}), "slug"),
        (
            json!({"kind": "calendar", "slug": "y", "color": "red"}),
            "color",
        ),
        (
            json!({"kind": "calendar", "slug": "z", "display_name": "a\u{202E}b"}),
            "name",
        ),
    ] {
        let reply = call(
            &app,
            Method::POST,
            "/api/v1/collections",
            Some(&alice),
            Some(body),
        )
        .await;
        assert_eq!(reply.status, StatusCode::BAD_REQUEST, "{field}");
    }

    let patch = call(
        &app,
        Method::PATCH,
        "/api/v1/collections/calendar/work",
        Some(&alice),
        Some(json!({"display_name": "Job"})),
    )
    .await;
    assert_eq!(patch.status, StatusCode::NO_CONTENT);
    // Bob cannot see, change or delete Alice's calendar.
    for (method, body) in [
        (Method::PATCH, Some(json!({"display_name": "x"}))),
        (Method::DELETE, None),
    ] {
        let reply = call(
            &app,
            method,
            "/api/v1/collections/calendar/work",
            Some(&bob),
            body,
        )
        .await;
        assert_eq!(reply.status, StatusCode::NOT_FOUND);
    }
    let bob_list = call(&app, Method::GET, "/api/v1/collections", Some(&bob), None).await;
    assert!(!bob_list.json.to_string().contains("work"));

    let delete = call(
        &app,
        Method::DELETE,
        "/api/v1/collections/calendar/work",
        Some(&alice),
        None,
    )
    .await;
    assert_eq!(delete.status, StatusCode::NO_CONTENT);
}

#[tokio::test]
async fn app_passwords_for_dav_clients() {
    let app = app().await;
    let alice = cookie_of(&login(&app, ALICE, None).await);
    let bob = cookie_of(&login(&app, BOB, None).await);
    let created = call(
        &app,
        Method::POST,
        "/api/v1/app-passwords",
        Some(&alice),
        Some(json!({"name": "Phone"})),
    )
    .await;
    assert_eq!(created.status, StatusCode::CREATED);
    let password = created.json["password"].as_str().unwrap().to_owned();
    assert!(password.starts_with("ccap_"));
    let id = created.json["id"].as_i64().unwrap();

    assert_eq!(
        dav(&app, "alice", &password).await,
        StatusCode::MULTI_STATUS
    );
    // The app password only works for its owner, and only with the full secret.
    assert_eq!(dav(&app, "bob", &password).await, StatusCode::UNAUTHORIZED);
    // Change the last character to a different one (it may already be 'x').
    let last = if password.ends_with('x') { 'y' } else { 'x' };
    let tampered = format!("{}{last}", &password[..password.len() - 1]);
    assert_eq!(
        dav(&app, "alice", &tampered).await,
        StatusCode::UNAUTHORIZED
    );

    let list = call(
        &app,
        Method::GET,
        "/api/v1/app-passwords",
        Some(&alice),
        None,
    )
    .await;
    assert_eq!(list.json[0]["name"], "Phone");
    assert!(!list.json.to_string().contains(&password[10..]));
    assert!(list.json[0]["last_used_at"].is_i64());

    let foreign = call(
        &app,
        Method::DELETE,
        &format!("/api/v1/app-passwords/{id}"),
        Some(&bob),
        None,
    )
    .await;
    assert_eq!(foreign.status, StatusCode::NOT_FOUND);
    let revoked = call(
        &app,
        Method::DELETE,
        &format!("/api/v1/app-passwords/{id}"),
        Some(&alice),
        None,
    )
    .await;
    assert_eq!(revoked.status, StatusCode::NO_CONTENT);
    assert_eq!(
        dav(&app, "alice", &password).await,
        StatusCode::UNAUTHORIZED
    );
}

async fn enable_totp(app: &Router, cookie: &str) -> (Vec<u8>, Vec<String>) {
    let setup = call(
        app,
        Method::POST,
        "/api/v1/me/totp/setup",
        Some(cookie),
        None,
    )
    .await;
    assert_eq!(setup.status, StatusCode::OK);
    assert!(
        setup.json["uri"]
            .as_str()
            .unwrap()
            .starts_with("otpauth://totp/CalCraft:")
    );
    let secret = base32_decode(setup.json["secret"].as_str().unwrap()).unwrap();
    let code = totp_code(&secret, now() / TOTP_STEP - 1);
    let enabled = call(
        app,
        Method::POST,
        "/api/v1/me/totp/enable",
        Some(cookie),
        Some(json!({"code": code})),
    )
    .await;
    assert_eq!(enabled.status, StatusCode::OK, "{}", enabled.json);
    let codes = enabled.json["codes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c.as_str().unwrap().to_owned())
        .collect();
    (secret, codes)
}

#[tokio::test]
async fn two_factor_login_and_dav_rules() {
    let app = app().await;
    let cookie = cookie_of(&login(&app, ALICE, None).await);
    let app_password = call(
        &app,
        Method::POST,
        "/api/v1/app-passwords",
        Some(&cookie),
        Some(json!({"name": "Laptop"})),
    )
    .await
    .json["password"]
        .as_str()
        .unwrap()
        .to_owned();
    let (secret, recovery) = enable_totp(&app, &cookie).await;
    assert_eq!(recovery.len(), 10);

    let without = login(&app, ALICE, None).await;
    assert_eq!(without.status, StatusCode::UNAUTHORIZED);
    assert_eq!(without.json["error"], "totp_required");

    let step = now() / TOTP_STEP;
    let with_code = login(&app, ALICE, Some(&totp_code(&secret, step))).await;
    assert_eq!(with_code.status, StatusCode::OK, "{}", with_code.json);
    // The same code cannot be used twice.
    let replay = login(&app, ALICE, Some(&totp_code(&secret, step))).await;
    assert_eq!(replay.status, StatusCode::UNAUTHORIZED);

    let recovered = login(&app, ALICE, Some(&recovery[0].to_uppercase())).await;
    assert_eq!(recovered.status, StatusCode::OK);
    assert_eq!(
        login(&app, ALICE, Some(&recovery[0])).await.status,
        StatusCode::UNAUTHORIZED
    );

    // Changing the password with 2FA requires a code.
    let session = cookie_of(&login(&app, ALICE, Some(&recovery[1])).await);
    let no_code = call(
        &app,
        Method::POST,
        "/api/v1/me/password",
        Some(&session),
        Some(json!({"current": ALICE.1, "new": "another long password"})),
    )
    .await;
    assert_eq!(no_code.json["error"], "totp_required");

    // With 2FA, DAV clients need an app password.
    assert_eq!(dav(&app, "alice", ALICE.1).await, StatusCode::UNAUTHORIZED);
    assert_eq!(
        dav(&app, "alice", &app_password).await,
        StatusCode::MULTI_STATUS
    );
}

#[tokio::test]
async fn wrong_second_factor_counts_towards_lockout() {
    let app = app().await;
    let cookie = cookie_of(&login(&app, ALICE, None).await);
    let (secret, _) = enable_totp(&app, &cookie).await;
    // Correct password with wrong codes must not reset the counter.
    for _ in 0..5 {
        let reply = login(&app, ALICE, Some("000000")).await;
        assert_eq!(reply.status, StatusCode::UNAUTHORIZED);
    }
    let code = totp_code(&secret, now() / TOTP_STEP + 1);
    let locked = login(&app, ALICE, Some(&code)).await;
    assert_eq!(locked.status, StatusCode::UNAUTHORIZED);
    assert_eq!(locked.json["error"], "invalid_credentials");
}

#[tokio::test]
async fn concurrent_attempts_cannot_bypass_the_lockout() {
    let app = app().await;
    // Twenty wrong passwords at once: at most five may be verified.
    let handles: Vec<_> = (0..20)
        .map(|_| {
            let app = app.clone();
            tokio::spawn(async move { dav(&app, "alice", "wrong password!!").await })
        })
        .collect();
    let mut results = Vec::new();
    for handle in handles {
        results.push(handle.await.unwrap());
    }
    assert!(results.iter().all(|s| *s == StatusCode::UNAUTHORIZED));
    // The account is now locked, even for the correct password.
    assert_eq!(dav(&app, "alice", ALICE.1).await, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn password_change_ends_sessions() {
    let app = app().await;
    let cookie = cookie_of(&login(&app, BOB, None).await);
    let short = call(
        &app,
        Method::POST,
        "/api/v1/me/password",
        Some(&cookie),
        Some(json!({"current": BOB.1, "new": "short"})),
    )
    .await;
    assert_eq!(short.status, StatusCode::BAD_REQUEST);
    let wrong = call(
        &app,
        Method::POST,
        "/api/v1/me/password",
        Some(&cookie),
        Some(json!({"current": "nope nope nope", "new": "a brand new password"})),
    )
    .await;
    assert_eq!(wrong.status, StatusCode::UNAUTHORIZED);
    let changed = call(
        &app,
        Method::POST,
        "/api/v1/me/password",
        Some(&cookie),
        Some(json!({"current": BOB.1, "new": "a brand new password"})),
    )
    .await;
    assert_eq!(changed.status, StatusCode::NO_CONTENT);
    assert_eq!(
        call(&app, Method::GET, "/api/v1/me", Some(&cookie), None)
            .await
            .status,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        login(&app, ("bob", "a brand new password"), None)
            .await
            .status,
        StatusCode::OK
    );
}

#[tokio::test]
async fn admin_endpoints() {
    let app = app().await;
    let admin = cookie_of(&login(&app, ALICE, None).await);
    let bob = cookie_of(&login(&app, BOB, None).await);

    assert_eq!(
        call(&app, Method::GET, "/api/v1/admin/users", Some(&bob), None)
            .await
            .status,
        StatusCode::FORBIDDEN
    );
    let users = call(&app, Method::GET, "/api/v1/admin/users", Some(&admin), None).await;
    assert_eq!(users.json.as_array().unwrap().len(), 2);

    let created = call(
        &app,
        Method::POST,
        "/api/v1/admin/users",
        Some(&admin),
        Some(json!({"username": "carol", "password": "carol password 1"})),
    )
    .await;
    assert_eq!(created.status, StatusCode::CREATED);
    let dup = call(
        &app,
        Method::POST,
        "/api/v1/admin/users",
        Some(&admin),
        Some(json!({"username": "carol", "password": "carol password 1"})),
    )
    .await;
    assert_eq!(dup.status, StatusCode::CONFLICT);

    let disable = call(
        &app,
        Method::PATCH,
        "/api/v1/admin/users/bob",
        Some(&admin),
        Some(json!({"disabled": true})),
    )
    .await;
    assert_eq!(disable.status, StatusCode::NO_CONTENT);
    assert_eq!(
        call(&app, Method::GET, "/api/v1/me", Some(&bob), None)
            .await
            .status,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(dav(&app, "bob", BOB.1).await, StatusCode::UNAUTHORIZED);

    let demote_self = call(
        &app,
        Method::PATCH,
        "/api/v1/admin/users/alice",
        Some(&admin),
        Some(json!({"is_admin": false})),
    )
    .await;
    assert_eq!(demote_self.status, StatusCode::CONFLICT);
}

#[tokio::test]
async fn update_check_is_off_by_default_and_admin_only() {
    let app = app().await;
    let admin = cookie_of(&login(&app, ALICE, None).await);
    let bob = cookie_of(&login(&app, BOB, None).await);
    for cookie in [None, Some(bob.as_str())] {
        let reply = call(&app, Method::GET, "/api/v1/admin/update", cookie, None).await;
        assert!(reply.status.is_client_error());
        assert!(reply.json.get("current").is_none());
    }
    let reply = call(
        &app,
        Method::GET,
        "/api/v1/admin/update",
        Some(&admin),
        None,
    )
    .await;
    assert_eq!(reply.status, StatusCode::OK);
    assert_eq!(
        reply.json,
        json!({
            "enabled": false,
            "current": calcraft_core::VERSION,
            "latest": null,
            "newer": false,
            "url": null,
            "checked_at": null
        })
    );
}

/// The check itself is not run (no network in tests): the state is fed a
/// recorded response.
#[cfg(feature = "update-check")]
#[tokio::test]
async fn update_check_reports_a_newer_release() {
    use std::sync::Arc;

    use calcraft_server::updates::UpdateCheck;

    let store = Store::in_memory().await.unwrap();
    store
        .create_user_with_role(
            &"alice".parse().unwrap(),
            &hash_password(ALICE.1).unwrap(),
            None,
            true,
        )
        .await
        .unwrap();
    let check = Arc::new(UpdateCheck::new());
    let mut state = AppState::new(store).unwrap();
    state.updates = Some(check.clone());
    let app = router(&Config::default(), state);
    let admin = cookie_of(&login(&app, ALICE, None).await);

    let reply = call(
        &app,
        Method::GET,
        "/api/v1/admin/update",
        Some(&admin),
        None,
    )
    .await;
    assert_eq!(reply.json["enabled"], true);
    assert_eq!(reply.json["newer"], false);
    assert_eq!(reply.json["checked_at"], Value::Null);

    let page = "https://github.com/Zeitgleich/CalCraft/releases/tag/v9999.1.0";
    let body =
        json!({"tag_name": "v9999.1.0", "html_url": page, "draft": false, "prerelease": false});
    check
        .apply(body.to_string().as_bytes(), 1_700_000_000)
        .unwrap();
    let reply = call(
        &app,
        Method::GET,
        "/api/v1/admin/update",
        Some(&admin),
        None,
    )
    .await;
    assert_eq!(reply.json["latest"], "9999.1.0");
    assert_eq!(reply.json["newer"], true);
    assert_eq!(reply.json["url"], page);
    assert_eq!(reply.json["checked_at"], 1_700_000_000);
}

#[tokio::test]
async fn openapi_is_served() {
    let app = app().await;
    let spec = call(&app, Method::GET, "/api/openapi.json", None, None).await;
    assert_eq!(spec.status, StatusCode::OK);
    assert_eq!(spec.json["openapi"], "3.1.0");
}

#[tokio::test]
async fn totp_enable_guesses_use_the_account_lockout() {
    let app = app().await;
    let cookie = cookie_of(&login(&app, ALICE, None).await);
    let setup = call(
        &app,
        Method::POST,
        "/api/v1/me/totp/setup",
        Some(&cookie),
        None,
    )
    .await;
    let secret = base32_decode(setup.json["secret"].as_str().unwrap()).unwrap();
    let valid = totp_code(&secret, now() / TOTP_STEP);
    // Guaranteed wrong: differs from every code of the accepted window.
    let accepted: Vec<String> = (-1..=1)
        .map(|offset| totp_code(&secret, now() / TOTP_STEP + offset))
        .collect();
    let wrong = (0..1_000_000)
        .map(|n| format!("{n:06}"))
        .find(|code| !accepted.contains(code))
        .unwrap();
    for _ in 0..5 {
        let reply = call(
            &app,
            Method::POST,
            "/api/v1/me/totp/enable",
            Some(&cookie),
            Some(json!({"code": wrong})),
        )
        .await;
        assert_eq!(reply.status, StatusCode::UNAUTHORIZED);
    }
    // Locked: even the right code is refused now, like a wrong one.
    let reply = call(
        &app,
        Method::POST,
        "/api/v1/me/totp/enable",
        Some(&cookie),
        Some(json!({"code": valid})),
    )
    .await;
    assert_eq!(reply.status, StatusCode::UNAUTHORIZED);
    assert_eq!(reply.json["error"], "invalid_credentials");
}

#[tokio::test]
async fn double_submitted_totp_enable_is_a_conflict_not_a_failure() {
    let app = app().await;
    let cookie = cookie_of(&login(&app, ALICE, None).await);
    let setup = call(
        &app,
        Method::POST,
        "/api/v1/me/totp/setup",
        Some(&cookie),
        None,
    )
    .await;
    let secret = base32_decode(setup.json["secret"].as_str().unwrap()).unwrap();
    let code = totp_code(&secret, now() / TOTP_STEP);
    let enable = || {
        call(
            &app,
            Method::POST,
            "/api/v1/me/totp/enable",
            Some(&cookie),
            Some(json!({"code": code})),
        )
    };
    let (first, second) = tokio::join!(enable(), enable());
    let mut statuses = [first.status, second.status];
    statuses.sort();
    assert_eq!(statuses, [StatusCode::OK, StatusCode::CONFLICT]);
}
