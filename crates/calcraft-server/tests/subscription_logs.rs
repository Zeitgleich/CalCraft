//! Feed URLs often carry secret tokens (in the path or the query). The
//! logs, at the most verbose level, name the feed's host only: never the
//! path, the query or credentials, also not for refused URLs, failed
//! refreshes or API errors.
//!
//! This binary holds a single test because it installs the global
//! subscriber (the same one `logging.format = "json"` uses).

#![cfg(feature = "subscriptions")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::{
    io::Write,
    net::IpAddr,
    sync::{Arc, Mutex},
    time::Duration,
};

use axum::{
    body::Body,
    http::{Method, Request},
};
use calcraft_server::{
    AppState, Config,
    auth::now,
    config::SubscriptionsConfig,
    router,
    secrets::hash_secret,
    subscriptions::{Fetcher, Lookup, Policy, Resolver, Subscriptions},
    telemetry,
};
use calcraft_store::{FeedState, NewSubscription, Store};
use http_body_util::BodyExt;
use tower::ServiceExt;
use tracing_subscriber::{EnvFilter, util::SubscriberInitExt};

const SECRET: &str = "Zq8FeedToken";

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

/// Every name resolves to a private address: refused before any request.
struct PrivateResolver;

impl Resolver for PrivateResolver {
    fn lookup<'a>(&'a self, _host: &'a str, _port: u16) -> Lookup<'a> {
        Box::pin(async { Ok(vec!["10.1.2.3".parse::<IpAddr>().unwrap()]) })
    }
}

#[tokio::test]
async fn subscription_logs_name_the_host_only() {
    let capture = Capture::default();
    let writer = capture.clone();
    telemetry::json_subscriber(EnvFilter::new("trace"), move || writer.clone())
        .try_init()
        .unwrap();

    let store = Store::in_memory().await.unwrap();
    let user = store
        .create_user(&"alice".parse().unwrap(), "h", None)
        .await
        .unwrap();
    let token = "alice-session-token-0123456789abcdef";
    store
        .create_session(user.id, &hash_secret(token), now(), now() + 3600)
        .await
        .unwrap();
    let config = SubscriptionsConfig {
        enabled: true,
        ..SubscriptionsConfig::default()
    };
    let fetcher = Fetcher::new(
        Policy::new(&config),
        Arc::new(PrivateResolver),
        None,
        config.max_bytes,
        Duration::from_secs(5),
        3,
    );
    let service = Subscriptions::with_fetcher(store.clone(), &config, fetcher);
    let mut state = AppState::new(store.clone()).unwrap();
    state.subscriptions = Some(service.clone());
    let app = router(&Config::default(), state);

    let cookie = format!("calcraft_session={token}");
    for url in [
        format!("https://feed.zq.example/private/{SECRET}.ics?token={SECRET}"),
        format!("webcal://feed.zq.example/{SECRET}?token={SECRET}"),
        format!("https://169.254.169.254/latest/{SECRET}?token={SECRET}"),
        format!("https://alice:{SECRET}@feed.zq.example/cal.ics"),
        format!("http://feed.zq.example/{SECRET}?token={SECRET}"),
        format!("https://feed.zq.example:8443/{SECRET}?token={SECRET}"),
    ] {
        let request = Request::builder()
            .method(Method::POST)
            .uri("/api/v1/subscriptions")
            .header("cookie", &cookie)
            .header("x-calcraft-request", "1")
            .header("content-type", "application/json")
            .body(Body::from(serde_json::json!({ "url": url }).to_string()))
            .unwrap();
        let response = app.clone().oneshot(request).await.unwrap();
        assert_eq!(response.status(), 400, "{url}");
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let body = String::from_utf8(body.to_vec()).unwrap();
        assert!(!body.contains(SECRET), "error reflects the URL: {body}");
    }

    // A stored subscription whose host now resolves to a private address.
    let slug = "feed".parse().unwrap();
    let source = format!("https://feed.zq.example/private/{SECRET}.ics?token={SECRET}");
    let collection = store
        .create_subscription(
            &NewSubscription {
                owner_id: user.id,
                slug: &slug,
                display_name: None,
                color: None,
                components: "VEVENT,VTODO",
                source_url: &source,
                refresh_secs: None,
                max_per_user: 20,
            },
            &[],
            &FeedState {
                feed_ttl_secs: None,
                http_etag: None,
                http_last_modified: None,
                content_hash: "x",
                skipped: 0,
                now: now() - 3600,
                next_refresh_at: 0,
            },
        )
        .await
        .unwrap();
    // Created an hour ago: the refresh is not rate limited.
    store
        .claim_subscription_refresh(collection.id, now() - 3600, 0, 0)
        .await
        .unwrap();
    let sub = service.refresh(collection.id).await.unwrap();
    assert_eq!(sub.last_error.as_deref(), Some("blocked_address"));

    let logs = String::from_utf8(capture.0.lock().unwrap().clone()).unwrap();
    assert!(
        logs.contains("feed.zq.example"),
        "the host is logged: {logs}"
    );
    assert!(logs.contains("subscription refresh failed"), "{logs}");
    for secret in [SECRET, "/private", "token=", "latest/"] {
        assert!(!logs.contains(secret), "{secret} in logs: {logs}");
    }
}
