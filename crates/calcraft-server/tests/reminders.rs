//! E-mail reminders through the server (docs/reminders.md): the REST API
//! (`/api/v1/me/reminders`, `/api/v1/admin/reminders`), 404 while off, and
//! an alarm written by DAV that ends up at a fake SMTP relay, sent to the
//! owner's own address only.

#![cfg(all(feature = "api", feature = "smtp"))]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::{
    net::{IpAddr, Ipv4Addr, SocketAddr},
    sync::{Arc, Mutex},
};

use axum::{
    Router,
    body::Body,
    extract::ConnectInfo,
    http::{Method, Request},
};
use base64::{Engine, engine::general_purpose::STANDARD};
use calcraft_core::api::CSRF_HEADER;
use calcraft_server::{
    AppState, Config,
    auth::{hash_password, now},
    config::SmtpTls,
    reminders::Reminders,
    router,
    secrets::hash_secret,
};
use calcraft_store::{Store, User};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    net::TcpListener,
};
use tower::ServiceExt;

const ALICE_SESSION: &str = "alice-session-token-0123456789abcdef";
const ADMIN_SESSION: &str = "admin-session-token-0123456789abcdef";
const ALICE_PW: &str = "alice password 123";

/// A plain relay on loopback that accepts everything.
async fn relay() -> (u16, Arc<Mutex<Vec<(String, String)>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let mails = Arc::new(Mutex::new(Vec::new()));
    let shared = mails.clone();
    tokio::spawn(async move {
        while let Ok((tcp, _)) = listener.accept().await {
            let mails = shared.clone();
            tokio::spawn(async move {
                let mut stream = BufReader::new(tcp);
                let _ = stream.get_mut().write_all(b"220 relay\r\n").await;
                let mut to = String::new();
                loop {
                    let mut line = String::new();
                    if stream.read_line(&mut line).await.unwrap_or(0) == 0 {
                        return;
                    }
                    let upper = line.trim_end().to_ascii_uppercase();
                    let reply = if upper.starts_with("EHLO") {
                        "250-relay\r\n250 8BITMIME\r\n"
                    } else if upper.starts_with("RCPT TO:") {
                        to = line.trim_end()[8..].to_owned();
                        "250 ok\r\n"
                    } else if upper == "DATA" {
                        let _ = stream.get_mut().write_all(b"354 go\r\n").await;
                        let mut message = String::new();
                        loop {
                            let mut line = String::new();
                            if stream.read_line(&mut line).await.unwrap_or(0) == 0 {
                                return;
                            }
                            if line == ".\r\n" {
                                break;
                            }
                            message.push_str(&line);
                        }
                        mails.lock().unwrap().push((to.clone(), message));
                        "250 ok\r\n"
                    } else if upper == "QUIT" {
                        let _ = stream.get_mut().write_all(b"221 bye\r\n").await;
                        return;
                    } else {
                        "250 ok\r\n"
                    };
                    if stream.get_mut().write_all(reply.as_bytes()).await.is_err() {
                        return;
                    }
                }
            });
        }
    });
    (port, mails)
}

struct Fx {
    app: Router,
    store: Store,
    alice: User,
    reminders: Option<Arc<Reminders>>,
}

async fn fixture(enabled: bool, port: u16) -> Fx {
    let store = Store::in_memory().await.unwrap();
    let alice = store
        .create_user(
            &"alice".parse().unwrap(),
            &hash_password(ALICE_PW).unwrap(),
            None,
        )
        .await
        .unwrap();
    let admin = store
        .create_user_with_role(&"root".parse().unwrap(), "unused", None, true)
        .await
        .unwrap();
    for (user, token) in [(&alice, ALICE_SESSION), (&admin, ADMIN_SESSION)] {
        store
            .create_session(user.id, &hash_secret(token), now(), now() + 3600)
            .await
            .unwrap();
    }
    let mut config = Config::default();
    config.server.client_rate_burst = u32::MAX;
    config.server.client_rate_per_second = u32::MAX;
    let mut state = AppState::new(store.clone()).unwrap();
    let mut reminders = None;
    if enabled {
        config.smtp.enabled = true;
        config.smtp.host = "127.0.0.1".to_owned();
        config.smtp.port = Some(port);
        config.smtp.tls = SmtpTls::None;
        config.smtp.from = "calendar@example.org".to_owned();
        config.reminders.enabled = true;
        let r = Reminders::new(store.clone(), &config).unwrap();
        r.prepare().await.unwrap();
        state.reminders = Some(r.clone());
        reminders = Some(r);
    }
    Fx {
        app: router(&config, state),
        store,
        alice,
        reminders,
    }
}

async fn send(
    app: &Router,
    method: &str,
    uri: &str,
    headers: &[(&str, &str)],
    body: &str,
) -> (u16, String) {
    let mut builder = Request::builder()
        .method(Method::from_bytes(method.as_bytes()).unwrap())
        .uri(uri);
    for (name, value) in headers {
        builder = builder.header(*name, *value);
    }
    let mut request = builder.body(Body::from(body.to_owned())).unwrap();
    request.extensions_mut().insert(ConnectInfo(SocketAddr::new(
        IpAddr::V4(Ipv4Addr::new(192, 0, 2, 1)),
        40_000,
    )));
    let response = app.clone().oneshot(request).await.unwrap();
    let status = response.status().as_u16();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (status, String::from_utf8_lossy(&bytes).into_owned())
}

impl Fx {
    async fn api(
        &self,
        session: &str,
        method: &str,
        uri: &str,
        body: Option<Value>,
    ) -> (u16, Value) {
        let cookie = format!("calcraft_session={session}");
        let mut headers = vec![("cookie", cookie.as_str()), (CSRF_HEADER, "1")];
        if body.is_some() {
            headers.push(("content-type", "application/json"));
        }
        let body = body.map(|b| b.to_string()).unwrap_or_default();
        let (status, text) = send(&self.app, method, uri, &headers, &body).await;
        (status, serde_json::from_str(&text).unwrap_or(Value::Null))
    }
}

#[tokio::test]
async fn routes_are_404_while_reminders_are_off() {
    let fx = fixture(false, 9).await;
    for (method, uri) in [
        ("GET", "/api/v1/me/reminders"),
        ("PATCH", "/api/v1/me/reminders"),
        ("POST", "/api/v1/me/reminders/test"),
    ] {
        let body = (method == "PATCH").then(|| json!({"mode": "all"}));
        let (status, _) = fx.api(ALICE_SESSION, method, uri, body).await;
        assert_eq!(status, 404, "{method} {uri}");
    }
    let (status, _) = fx
        .api(ADMIN_SESSION, "GET", "/api/v1/admin/reminders", None)
        .await;
    assert_eq!(status, 404);
}

#[tokio::test]
async fn settings_api() {
    let (port, _) = relay().await;
    let fx = fixture(true, port).await;
    let (status, info) = fx
        .api(ALICE_SESSION, "GET", "/api/v1/me/reminders", None)
        .await;
    assert_eq!(status, 200);
    assert_eq!(info["mode"], "email");
    assert_eq!(info["custom"], false);
    assert_eq!(info["default_mode"], "email");
    assert_eq!(info["include_description"], false);
    assert_eq!(info["address"], Value::Null);
    assert_eq!(info["status"]["sent"], 0);

    let (status, _) = fx
        .api(
            ALICE_SESSION,
            "PATCH",
            "/api/v1/me/reminders",
            Some(json!({"mode": "all", "include_description": true})),
        )
        .await;
    assert_eq!(status, 204);
    let (_, info) = fx
        .api(ALICE_SESSION, "GET", "/api/v1/me/reminders", None)
        .await;
    assert_eq!(
        (info["mode"].clone(), info["custom"].clone()),
        (json!("all"), json!(true))
    );
    assert_eq!(info["include_description"], true);
    let (status, _) = fx
        .api(
            ALICE_SESSION,
            "PATCH",
            "/api/v1/me/reminders",
            Some(json!({"mode": "default"})),
        )
        .await;
    assert_eq!(status, 204);
    let (_, info) = fx
        .api(ALICE_SESSION, "GET", "/api/v1/me/reminders", None)
        .await;
    assert_eq!(
        (info["mode"].clone(), info["custom"].clone()),
        (json!("email"), json!(false))
    );
    // The description setting is kept.
    assert_eq!(info["include_description"], true);

    for bad in [
        json!({"mode": "sometimes"}),
        json!({"mode": "all", "to": "evil@example.org"}),
        json!({"include_description": "yes"}),
    ] {
        let (status, _) = fx
            .api(
                ALICE_SESSION,
                "PATCH",
                "/api/v1/me/reminders",
                Some(bad.clone()),
            )
            .await;
        assert_eq!(status, 400, "{bad}");
    }

    // Test mails need an address.
    let (status, body) = fx
        .api(ALICE_SESSION, "POST", "/api/v1/me/reminders/test", None)
        .await;
    assert_eq!(status, 409, "{body}");
    fx.store
        .set_user_email(fx.alice.id, Some("alice@example.org"))
        .await
        .unwrap();
    let (status, body) = fx
        .api(ALICE_SESSION, "POST", "/api/v1/me/reminders/test", None)
        .await;
    assert_eq!(status, 202);
    assert_eq!(body["queued"], true);
    let (_, info) = fx
        .api(ALICE_SESSION, "GET", "/api/v1/me/reminders", None)
        .await;
    assert_eq!(info["address"], "alice@example.org");
    assert_eq!(info["status"]["pending"], 1);

    // The overview is for administrators only and shows counts only.
    let (status, _) = fx
        .api(ALICE_SESSION, "GET", "/api/v1/admin/reminders", None)
        .await;
    assert_eq!(status, 403);
    let (status, overview) = fx
        .api(ADMIN_SESSION, "GET", "/api/v1/admin/reminders", None)
        .await;
    assert_eq!(status, 200);
    assert_eq!(overview["status"]["pending"], 1);
    assert!(!overview.to_string().contains("alice"));
}

#[tokio::test]
async fn an_alarm_written_by_dav_reaches_the_owner_only() {
    let (port, mails) = relay().await;
    let fx = fixture(true, port).await;
    fx.store
        .set_user_email(fx.alice.id, Some("alice@example.org"))
        .await
        .unwrap();
    // Bob may write into alice's calendar through a share; his address
    // must not receive anything either.
    let event = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//Test//EN\r\nBEGIN:VEVENT\r\n\
        UID:dav-reminder\r\nDTSTAMP:20261001T000000Z\r\nDTSTART:20261201T100000Z\r\n\
        DURATION:PT1H\r\nSUMMARY:Dentist\r\nATTENDEE:mailto:bob@example.org\r\n\
        BEGIN:VALARM\r\nACTION:EMAIL\r\nTRIGGER:-PT1H\r\nSUMMARY:Dentist\r\nDESCRIPTION:x\r\n\
        ATTENDEE:mailto:evil@attacker.example\r\nEND:VALARM\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";
    let auth = format!("Basic {}", STANDARD.encode(format!("alice:{ALICE_PW}")));
    let (status, body) = send(
        &fx.app,
        "PUT",
        "/dav/calendars/alice/default/dentist.ics",
        &[("authorization", &auth), ("content-type", "text/calendar")],
        event,
    )
    .await;
    assert_eq!(status, 201, "{body}");
    let reminders = fx.reminders.as_ref().unwrap();
    // 2026-12-01 10:00 UTC; the alarm fires at 09:00.
    let start = 1_796_119_200;
    reminders.run(start - 7200).await.unwrap();
    let report = reminders.run(start - 3600).await.unwrap();
    assert_eq!(report.sent, 1, "{report:?}");
    let mails = mails.lock().unwrap().clone();
    assert_eq!(mails.len(), 1);
    assert_eq!(mails[0].0, "<alice@example.org>");
    assert!(mails[0].1.contains("Subject: Reminder: Dentist"));
    assert!(!mails[0].1.contains("attacker") && !mails[0].1.contains("bob@"));

    // The status shows it.
    let (_, info) = fx
        .api(ALICE_SESSION, "GET", "/api/v1/me/reminders", None)
        .await;
    assert_eq!(info["status"]["sent"], 1);
}
