#![allow(clippy::unwrap_used)]

//! Request limits: overall and per client, client address resolution with
//! trusted proxies, and failed logins per client.

use std::{
    net::{IpAddr, SocketAddr},
    time::Duration,
};

use axum::{
    Router,
    body::Body,
    extract::ConnectInfo,
    http::{Method, Request, StatusCode, header},
};
use base64::{Engine, engine::general_purpose::STANDARD};
use calcraft_server::{AppState, Config, auth::hash_password, router};
use calcraft_store::Store;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
};
use tower::ServiceExt;

const ALICE: (&str, &str) = ("alice", "alice password 123");

async fn app(config: &Config) -> Router {
    let store = Store::in_memory().await.unwrap();
    let hash = hash_password(ALICE.1).unwrap();
    store
        .create_user(&ALICE.0.parse().unwrap(), &hash, None)
        .await
        .unwrap();
    router(config, AppState::new(store).unwrap())
}

fn basic((user, password): (&str, &str)) -> String {
    format!("Basic {}", STANDARD.encode(format!("{user}:{password}")))
}

/// A request as it arrives from TCP peer `peer`.
fn from(peer: &str, mut request: Request<Body>) -> Request<Body> {
    let ip: IpAddr = peer.parse().unwrap();
    request
        .extensions_mut()
        .insert(ConnectInfo(SocketAddr::new(ip, 40_000)));
    request
}

fn get(uri: &str) -> Request<Body> {
    Request::get(uri).body(Body::empty()).unwrap()
}

fn get_forwarded(uri: &str, forwarded_for: &str) -> Request<Body> {
    Request::get(uri)
        .header("x-forwarded-for", forwarded_for)
        .body(Body::empty())
        .unwrap()
}

async fn status(app: &Router, request: Request<Body>) -> StatusCode {
    app.clone().oneshot(request).await.unwrap().status()
}

fn rate_limited(burst: u32) -> Config {
    let mut config = Config::default();
    config.server.client_rate_burst = burst;
    config.server.client_rate_per_second = 1;
    config
}

#[tokio::test]
async fn request_rate_is_limited_per_client() {
    let app = app(&rate_limited(3)).await;
    for _ in 0..3 {
        assert_eq!(
            status(&app, from("203.0.113.1", get("/healthz"))).await,
            StatusCode::OK
        );
    }
    let response = app
        .clone()
        .oneshot(from("203.0.113.1", get("/dav/")))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(response.headers()[header::RETRY_AFTER], "1");
    // Security headers apply to rejections, too.
    assert!(
        response
            .headers()
            .contains_key(header::CONTENT_SECURITY_POLICY)
    );
    // Other clients are not affected.
    assert_eq!(
        status(&app, from("203.0.113.2", get("/healthz"))).await,
        StatusCode::OK
    );
}

#[tokio::test]
async fn ipv6_clients_share_a_limit_per_network() {
    let app = app(&rate_limited(1)).await;
    assert_eq!(
        status(&app, from("2001:db8::1", get("/healthz"))).await,
        StatusCode::OK
    );
    // Another address of the same /64.
    assert_eq!(
        status(&app, from("2001:db8::ffff:2", get("/healthz"))).await,
        StatusCode::TOO_MANY_REQUESTS
    );
    assert_eq!(
        status(&app, from("2001:db8:0:1::1", get("/healthz"))).await,
        StatusCode::OK
    );
    // An IPv4-mapped address is the IPv4 client.
    assert_eq!(
        status(&app, from("192.0.2.1", get("/healthz"))).await,
        StatusCode::OK
    );
    assert_eq!(
        status(&app, from("::ffff:192.0.2.1", get("/healthz"))).await,
        StatusCode::TOO_MANY_REQUESTS
    );
}

#[tokio::test]
async fn forwarded_for_is_ignored_without_trusted_proxies() {
    let app = app(&rate_limited(1)).await;
    assert_eq!(
        status(
            &app,
            from("127.0.0.1", get_forwarded("/healthz", "198.51.100.1"))
        )
        .await,
        StatusCode::OK
    );
    // A different X-Forwarded-For does not make a different client.
    assert_eq!(
        status(
            &app,
            from("127.0.0.1", get_forwarded("/healthz", "198.51.100.2"))
        )
        .await,
        StatusCode::TOO_MANY_REQUESTS
    );
}

#[tokio::test]
async fn forwarded_for_is_honoured_from_trusted_proxies_only() {
    let mut config = rate_limited(1);
    config.server.trusted_proxies = vec!["127.0.0.1/32".parse().unwrap()];
    let app = app(&config).await;
    for client in ["198.51.100.1", "198.51.100.2"] {
        assert_eq!(
            status(&app, from("127.0.0.1", get_forwarded("/healthz", client))).await,
            StatusCode::OK,
            "{client}"
        );
    }
    // Same client again, behind a spoofed entry it sent itself.
    assert_eq!(
        status(
            &app,
            from(
                "127.0.0.1",
                get_forwarded("/healthz", "203.0.113.77, 198.51.100.1")
            )
        )
        .await,
        StatusCode::TOO_MANY_REQUESTS
    );
    // An untrusted peer cannot pick its client address.
    assert_eq!(
        status(
            &app,
            from("203.0.113.5", get_forwarded("/healthz", "198.51.100.9"))
        )
        .await,
        StatusCode::OK
    );
    assert_eq!(
        status(
            &app,
            from("203.0.113.5", get_forwarded("/healthz", "198.51.100.10"))
        )
        .await,
        StatusCode::TOO_MANY_REQUESTS
    );
}

const PRINCIPAL_QUERY: &str = r#"<?xml version="1.0" encoding="utf-8"?>
<d:propfind xmlns:d="DAV:"><d:prop><d:current-user-principal/></d:prop></d:propfind>"#;

fn propfind(credentials: (&str, &str)) -> Request<Body> {
    Request::builder()
        .method(Method::from_bytes(b"PROPFIND").unwrap())
        .uri("/dav/")
        .header("depth", "0")
        .header(header::AUTHORIZATION, basic(credentials))
        .body(Body::from(PRINCIPAL_QUERY))
        .unwrap()
}

#[tokio::test]
async fn failed_logins_are_limited_per_client() {
    let mut config = Config::default();
    config.server.max_login_failures_per_client = 2;
    let app = app(&config).await;
    // Wrong password and unknown user: both count for the client.
    for credentials in [("alice", "wrong password 1"), ("nobody", ALICE.1)] {
        assert_eq!(
            status(&app, from("203.0.113.1", propfind(credentials))).await,
            StatusCode::UNAUTHORIZED
        );
    }
    let blocked = app
        .clone()
        .oneshot(from("203.0.113.1", propfind(ALICE)))
        .await
        .unwrap();
    // Exactly like wrong credentials.
    assert_eq!(blocked.status(), StatusCode::UNAUTHORIZED);
    assert!(blocked.headers().contains_key(header::WWW_AUTHENTICATE));
    // The account is not locked for other clients.
    assert_eq!(
        status(&app, from("203.0.113.2", propfind(ALICE))).await,
        StatusCode::MULTI_STATUS
    );

    #[cfg(feature = "api")]
    {
        use calcraft_core::api::CSRF_HEADER;
        use http_body_util::BodyExt;

        let login = |peer: &str| {
            from(
                peer,
                Request::post("/api/v1/auth/login")
                    .header(header::CONTENT_TYPE, "application/json")
                    .header(CSRF_HEADER, "1")
                    .body(Body::from(format!(
                        r#"{{"username":"alice","password":"{}"}}"#,
                        ALICE.1
                    )))
                    .unwrap(),
            )
        };
        let response = app.clone().oneshot(login("203.0.113.1")).await.unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        assert!(String::from_utf8_lossy(&body).contains("invalid_credentials"));
        assert_eq!(status(&app, login("203.0.113.3")).await, StatusCode::OK);
    }
}

// --- Over TCP: requests held in flight ------------------------------------

/// Serves `app` on a loopback port, with peer addresses like `calcraft serve`.
async fn serve(app: Router) -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .await
        .unwrap();
    });
    addr
}

/// Starts a PUT whose body never arrives, so that the request stays in
/// flight until the returned connection is dropped.
async fn hold(addr: SocketAddr) -> TcpStream {
    let mut stream = TcpStream::connect(addr).await.unwrap();
    let request = format!(
        "PUT /dav/calendars/alice/default/held.ics HTTP/1.1\r\nHost: localhost\r\n\
         Authorization: {}\r\nContent-Type: text/calendar\r\nContent-Length: 100\r\n\r\nBEGIN",
        basic(ALICE)
    );
    stream.write_all(request.as_bytes()).await.unwrap();
    stream
}

/// Sends a GET and returns the response head, lowercased.
async fn raw_get(addr: SocketAddr, path: &str, extra: &str) -> String {
    let mut stream = TcpStream::connect(addr).await.unwrap();
    let request =
        format!("GET {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n{extra}\r\n");
    stream.write_all(request.as_bytes()).await.unwrap();
    let mut response = Vec::new();
    tokio::time::timeout(Duration::from_secs(10), stream.read_to_end(&mut response))
        .await
        .unwrap()
        .unwrap();
    String::from_utf8_lossy(&response).to_ascii_lowercase()
}

fn status_of(response: &str) -> u16 {
    response
        .split(' ')
        .nth(1)
        .and_then(|code| code.parse().ok())
        .unwrap()
}

/// Repeats the GET until `done` accepts the status (the held request may
/// take a moment to reach the limits middleware); returns the response.
async fn until(addr: SocketAddr, path: &str, extra: &str, done: impl Fn(u16) -> bool) -> String {
    for _ in 0..500 {
        let response = raw_get(addr, path, extra).await;
        if done(status_of(&response)) {
            return response;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("condition not reached for {path}");
}

#[tokio::test]
async fn overload_returns_503_quickly_and_recovers() {
    let mut config = Config::default();
    config.server.max_concurrent_requests = 1;
    let addr = serve(app(&config).await).await;

    let held = hold(addr).await;
    let response = until(addr, "/dav/", "", |status| status == 503).await;
    assert!(response.contains("\r\nretry-after: 1\r\n"), "{response}");
    // Health checks still answer.
    for path in ["/healthz", "/readyz"] {
        assert_eq!(status_of(&raw_get(addr, path, "").await), 200, "{path}");
    }
    // When the held request ends, requests are served again.
    drop(held);
    until(addr, "/dav/", "", |status| status == 401).await;
}

#[tokio::test]
async fn concurrent_requests_are_limited_per_client() {
    let mut config = Config::default();
    config.server.max_concurrent_requests_per_client = 1;
    config.server.trusted_proxies = vec!["127.0.0.1/32".parse().unwrap()];
    let addr = serve(app(&config).await).await;

    let held = hold(addr).await;
    let response = until(addr, "/dav/", "", |status| status == 429).await;
    assert!(response.contains("\r\nretry-after: 1\r\n"), "{response}");
    // Another client behind the trusted proxy is not affected.
    let other = raw_get(addr, "/dav/", "X-Forwarded-For: 198.51.100.1\r\n").await;
    assert_eq!(status_of(&other), 401);
    drop(held);
    until(addr, "/dav/", "", |status| status == 401).await;
}
