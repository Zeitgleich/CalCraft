//! Fetcher and service tests against a local HTTP server. Loopback is
//! normally refused; these tests exempt 127.0.0.1 through
//! `Policy::test_exempt`, which only exists in test builds.

use std::{
    collections::VecDeque,
    net::{IpAddr, Ipv4Addr},
    sync::atomic::{AtomicUsize, Ordering},
};

use axum::{
    Router,
    body::Body,
    http::{Method, Request},
};
use calcraft_core::CollectionKind;
use http_body_util::BodyExt;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};
use tower::ServiceExt;

use super::*;
use crate::{AppState, Config, router, secrets::hash_secret};

const LOCAL: IpAddr = IpAddr::V4(Ipv4Addr::LOCALHOST);
const SECRET: &str = "Zq9FeedSecret";

// --- local feed server -----------------------------------------------------

enum Reply {
    Raw(Vec<u8>),
    /// Accept the request and never answer.
    Hang,
}

type Responder = Box<dyn Fn(&str) -> Reply + Send>;

struct Server {
    port: u16,
    requests: Arc<Mutex<Vec<String>>>,
    responder: Arc<Mutex<Responder>>,
}

impl Server {
    async fn start() -> Self {
        let listener = TcpListener::bind((LOCAL, 0)).await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let responder: Arc<Mutex<Responder>> =
            Arc::new(Mutex::new(Box::new(|_: &str| Reply::Raw(Vec::new()))));
        tokio::spawn({
            let requests = requests.clone();
            let responder = responder.clone();
            async move {
                loop {
                    let Ok((mut socket, _)) = listener.accept().await else {
                        return;
                    };
                    let requests = requests.clone();
                    let responder = responder.clone();
                    tokio::spawn(async move {
                        let mut buf = Vec::new();
                        let mut chunk = [0u8; 4096];
                        while !buf.windows(4).any(|w| w == b"\r\n\r\n") && buf.len() < 65536 {
                            match socket.read(&mut chunk).await {
                                Ok(0) | Err(_) => return,
                                Ok(n) => buf.extend_from_slice(&chunk[..n]),
                            }
                        }
                        let request = String::from_utf8_lossy(&buf).into_owned();
                        requests.lock().unwrap().push(request.clone());
                        let reply = (responder.lock().unwrap())(&request);
                        match reply {
                            Reply::Raw(bytes) => {
                                let _ = socket.write_all(&bytes).await;
                                let _ = socket.shutdown().await;
                            }
                            Reply::Hang => tokio::time::sleep(Duration::from_secs(60)).await,
                        }
                    });
                }
            }
        });
        Self {
            port,
            requests,
            responder,
        }
    }

    fn respond(&self, responder: impl Fn(&str) -> Reply + Send + 'static) {
        *self.responder.lock().unwrap() = Box::new(responder);
    }

    fn requests(&self) -> Vec<String> {
        self.requests.lock().unwrap().clone()
    }

    fn url(&self, host: &str, path: &str) -> String {
        format!("http://{host}:{}{path}", self.port)
    }
}

fn response(status: &str, headers: &str, body: &[u8]) -> Reply {
    let mut out = format!(
        "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n{headers}\r\n",
        body.len()
    )
    .into_bytes();
    out.extend_from_slice(body);
    Reply::Raw(out)
}

fn calendar(body: &str, headers: &str) -> Reply {
    response(
        "200 OK",
        &format!("Content-Type: text/calendar; charset=utf-8\r\n{headers}"),
        body.as_bytes(),
    )
}

fn feed(events: &[(&str, &str)]) -> String {
    let mut out = String::from(
        "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//Test//EN\r\nX-PUBLISHED-TTL:PT2H\r\n",
    );
    for (uid, summary) in events {
        out.push_str(&format!(
            "BEGIN:VEVENT\r\nUID:{uid}\r\nDTSTAMP:20261001T000000Z\r\n\
             DTSTART:20261021T100000Z\r\nDTEND:20261021T110000Z\r\nSUMMARY:{summary}\r\nEND:VEVENT\r\n"
        ));
    }
    out.push_str("END:VCALENDAR\r\n");
    out
}

// --- resolver --------------------------------------------------------------

/// Answers lookups from a queue, then with `fallback`; counts calls.
struct TestResolver {
    answers: Mutex<VecDeque<Vec<IpAddr>>>,
    fallback: Vec<IpAddr>,
    calls: AtomicUsize,
}

impl TestResolver {
    fn new(answers: Vec<Vec<IpAddr>>, fallback: Vec<IpAddr>) -> Arc<Self> {
        Arc::new(Self {
            answers: Mutex::new(answers.into()),
            fallback,
            calls: AtomicUsize::new(0),
        })
    }

    fn local() -> Arc<Self> {
        Self::new(Vec::new(), vec![LOCAL])
    }

    fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

impl Resolver for TestResolver {
    fn lookup<'a>(&'a self, _host: &'a str, _port: u16) -> Lookup<'a> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let answer = self
            .answers
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or_else(|| self.fallback.clone());
        Box::pin(async move { Ok(answer) })
    }
}

fn config(port: u16) -> SubscriptionsConfig {
    SubscriptionsConfig {
        enabled: true,
        allow_http: true,
        allowed_ports: vec![port],
        max_bytes: 64 * 1024,
        max_per_user: 2,
        ..SubscriptionsConfig::default()
    }
}

fn fetcher_with(
    port: u16,
    resolver: Arc<TestResolver>,
    timeout: Duration,
    max_redirects: u8,
) -> Fetcher {
    let mut policy = Policy::new(&config(port));
    policy.test_exempt = vec![LOCAL];
    let tls = crate::https::tls_config(None, true).unwrap();
    Fetcher::new(
        policy,
        resolver,
        Some(tls),
        64 * 1024,
        timeout,
        max_redirects,
    )
}

fn fetcher(port: u16, resolver: Arc<TestResolver>) -> Fetcher {
    fetcher_with(port, resolver, Duration::from_secs(5), 3)
}

async fn service(server: &Server, resolver: Arc<TestResolver>) -> (Arc<Subscriptions>, User) {
    let store = Store::in_memory().await.unwrap();
    let user = store
        .create_user(&"alice".parse().unwrap(), "h", None)
        .await
        .unwrap();
    let service = Subscriptions::without_rate_limit(
        store,
        &config(server.port),
        fetcher(server.port, resolver),
    );
    (service, user)
}

fn request<'a>(url: &'a str) -> CreateRequest<'a> {
    CreateRequest {
        url,
        slug: Some("holidays".parse().unwrap()),
        display_name: Some("Holidays".to_owned()),
        color: None,
        refresh_secs: None,
    }
}

fn feed_error(err: ServiceError) -> SubscriptionError {
    match err {
        ServiceError::Feed(err) => err,
        err => panic!("expected a feed error, got {err:?}"),
    }
}

// --- fetcher ---------------------------------------------------------------

#[tokio::test]
async fn http_and_subscriptions_are_off_by_default() {
    let config = SubscriptionsConfig::default();
    assert!(!config.enabled);
    assert!(!config.allow_http);
    assert_eq!(config.ports(), vec![443]);
    assert_eq!(config.max_bytes, 10 * 1024 * 1024);
    assert_eq!(config.max_redirects, 3);
    let policy = Policy::new(&config);
    assert!(policy.parse_url("http://example.org/cal.ics").is_err());
    assert!(
        policy.check_ip(LOCAL).is_err(),
        "no exemption outside tests"
    );
}

#[tokio::test]
async fn fetch_sends_no_credentials_and_a_fixed_user_agent() {
    let server = Server::start().await;
    server.respond(|_| calendar(&feed(&[("a", "A")]), "ETag: \"v1\"\r\n"));
    let fetcher = fetcher(server.port, TestResolver::local());
    let url = fetcher
        .policy()
        .parse_url(&server.url("feed.example", &format!("/cal.ics?token={SECRET}")))
        .unwrap();
    let fetched = fetcher.fetch(&url, &Conditional::default()).await.unwrap();
    let Fetched::Body { etag, .. } = fetched else {
        panic!("expected a body");
    };
    assert_eq!(etag.as_deref(), Some("\"v1\""));
    let requests = server.requests();
    assert_eq!(requests.len(), 1);
    let request = requests[0].to_ascii_lowercase();
    assert!(request.starts_with(&format!(
        "get /cal.ics?token={} http/1.1\r\n",
        SECRET.to_ascii_lowercase()
    )));
    assert!(request.contains(&format!("host: feed.example:{}\r\n", server.port)));
    assert!(request.contains(&format!(
        "user-agent: calcraft/{}\r\n",
        env!("CARGO_PKG_VERSION")
    )));
    assert!(request.contains("accept-encoding: identity\r\n"));
    for header in [
        "cookie",
        "authorization",
        "if-none-match",
        "if-modified-since",
    ] {
        assert!(
            !request.contains(&format!("{header}:")),
            "{header} sent: {request}"
        );
    }
}

#[tokio::test]
async fn dns_rebinding_uses_the_pinned_address() {
    let server = Server::start().await;
    server.respond(|_| calendar(&feed(&[("a", "A")]), ""));
    // First answer public (exempt loopback in this test), every later
    // answer private: the connection must use the first one, and the name
    // must be resolved exactly once.
    let resolver = TestResolver::new(vec![vec![LOCAL]], vec!["10.0.0.1".parse().unwrap()]);
    let fetcher = fetcher(server.port, resolver.clone());
    let url = fetcher
        .policy()
        .parse_url(&server.url("rebind.example", "/cal.ics"))
        .unwrap();
    assert!(matches!(
        fetcher.fetch(&url, &Conditional::default()).await,
        Ok(Fetched::Body { .. })
    ));
    assert_eq!(resolver.calls(), 1);
    assert_eq!(server.requests().len(), 1);
    // The next lookup answers with the private address: refused before any
    // connection is made.
    assert_eq!(
        fetcher.fetch(&url, &Conditional::default()).await,
        Err(SubscriptionError::BlockedAddress)
    );
    assert_eq!(server.requests().len(), 1);
}

#[tokio::test]
async fn every_address_of_a_name_is_checked() {
    let server = Server::start().await;
    server.respond(|_| calendar(&feed(&[]), ""));
    for answer in [
        vec![LOCAL, "10.0.0.1".parse().unwrap()],
        vec!["169.254.169.254".parse().unwrap(), LOCAL],
        vec!["::ffff:192.168.1.1".parse().unwrap()],
        vec!["fd00:ec2::254".parse().unwrap()],
        vec![],
    ] {
        let empty = answer.is_empty();
        let fetcher = fetcher(server.port, TestResolver::new(vec![answer], Vec::new()));
        let url = fetcher
            .policy()
            .parse_url(&server.url("multi.example", "/"))
            .unwrap();
        let err = fetcher
            .fetch(&url, &Conditional::default())
            .await
            .unwrap_err();
        if empty {
            assert_eq!(err, SubscriptionError::FetchFailed(Some("host not found")));
        } else {
            assert_eq!(err, SubscriptionError::BlockedAddress);
        }
    }
    assert!(server.requests().is_empty());
}

#[tokio::test]
async fn redirects_are_followed_and_checked_again() {
    let server = Server::start().await;
    let port = server.port;
    server.respond(move |request| {
        let path = request.split(' ').nth(1).unwrap_or_default().to_owned();
        match path.as_str() {
            "/start" => response("302 Found", "Location: /middle\r\n", b""),
            "/middle" => response(
                "301 Moved Permanently",
                &format!("Location: http://other.example:{port}/final.ics\r\n"),
                b"",
            ),
            "/final.ics" => calendar(&feed(&[("a", "A")]), ""),
            "/to-private" => response(
                "307 Temporary Redirect",
                "Location: http://10.0.0.1/\r\n",
                b"",
            ),
            "/to-localhost" => response(
                "308 Permanent Redirect",
                &format!("Location: http://localhost:{port}/\r\n"),
                b"",
            ),
            "/to-file" => response("302 Found", "Location: file:///etc/passwd\r\n", b""),
            "/to-port" => response("302 Found", "Location: http://other.example:9/\r\n", b""),
            "/to-rebind" => response(
                "302 Found",
                &format!("Location: http://rebind.example:{port}/final.ics\r\n"),
                b"",
            ),
            "/no-location" => response("302 Found", "", b""),
            _ => response("302 Found", "Location: /loop\r\n", b""),
        }
    });
    let fetcher = fetcher(port, TestResolver::local());
    let fetch = |path: &'static str| {
        let fetcher = fetcher.clone();
        let url = server.url("feed.example", path);
        async move {
            let url = fetcher.policy().parse_url(&url).unwrap();
            fetcher.fetch(&url, &Conditional::default()).await
        }
    };
    assert!(matches!(fetch("/start").await, Ok(Fetched::Body { .. })));
    let hosts: Vec<String> = server
        .requests()
        .iter()
        .filter_map(|r| {
            r.lines()
                .find(|l| l.to_ascii_lowercase().starts_with("host:"))
                .map(str::to_owned)
        })
        .collect();
    assert_eq!(
        hosts,
        [
            format!("host: feed.example:{port}"),
            format!("host: feed.example:{port}"),
            format!("host: other.example:{port}")
        ]
    );
    assert_eq!(
        fetch("/to-private").await,
        Err(SubscriptionError::BlockedAddress)
    );
    assert_eq!(
        fetch("/to-localhost").await,
        Err(SubscriptionError::BlockedAddress)
    );
    assert!(matches!(
        fetch("/to-file").await,
        Err(SubscriptionError::FetchFailed(_))
    ));
    assert!(matches!(
        fetch("/to-port").await,
        Err(SubscriptionError::FetchFailed(_))
    ));
    assert_eq!(
        fetch("/no-location").await,
        Err(SubscriptionError::FetchFailed(Some("invalid redirect")))
    );
    assert_eq!(
        fetch("/loop").await,
        Err(SubscriptionError::FetchFailed(Some("too many redirects")))
    );
    // Each hop resolves its host again and checks the answer.
    let resolver = TestResolver::new(vec![vec![LOCAL]], vec!["192.168.0.10".parse().unwrap()]);
    let rebinding = fetcher_with(port, resolver.clone(), Duration::from_secs(5), 3);
    let url = rebinding
        .policy()
        .parse_url(&server.url("feed.example", "/to-rebind"))
        .unwrap();
    assert_eq!(
        rebinding.fetch(&url, &Conditional::default()).await,
        Err(SubscriptionError::BlockedAddress)
    );
    assert_eq!(resolver.calls(), 2);
    // No redirects allowed at all.
    let strict = fetcher_with(port, TestResolver::local(), Duration::from_secs(5), 0);
    let url = strict
        .policy()
        .parse_url(&server.url("feed.example", "/start"))
        .unwrap();
    assert_eq!(
        strict.fetch(&url, &Conditional::default()).await,
        Err(SubscriptionError::FetchFailed(Some("too many redirects")))
    );
}

#[tokio::test]
async fn body_size_time_and_content_are_limited() {
    let server = Server::start().await;
    let big = format!("BEGIN:VCALENDAR\r\nX-PAD:{}\r\n", "x".repeat(70 * 1024));
    server.respond(move |request| {
        let path = request.split(' ').nth(1).unwrap_or_default();
        match path {
            "/declared" => calendar(&big, ""),
            "/chunked" => {
                // No Content-Length: the limit applies while reading.
                let mut out = b"HTTP/1.1 200 OK\r\nContent-Type: text/calendar\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n".to_vec();
                for _ in 0..80 {
                    out.extend_from_slice(format!("400\r\n{}\r\n", "y".repeat(1024)).as_bytes());
                }
                out.extend_from_slice(b"0\r\n\r\n");
                Reply::Raw(out)
            }
            "/hang" => Reply::Hang,
            "/html" => response("200 OK", "Content-Type: text/html\r\n", b"BEGIN:VCALENDAR\r\n"),
            "/not-ical" => response("200 OK", "Content-Type: text/plain\r\n", b"<html>hello</html>"),
            "/gzip" => response("200 OK", "Content-Encoding: gzip\r\n", b"BEGIN:VCALENDAR\r\n"),
            "/missing" => response("404 Not Found", "", b"nope"),
            "/unmodified" => response("304 Not Modified", "", b""),
            _ => calendar(&feed(&[]), ""),
        }
    });
    let fetcher = fetcher_with(
        server.port,
        TestResolver::local(),
        Duration::from_millis(500),
        3,
    );
    let fetch = |path: &'static str| {
        let fetcher = fetcher.clone();
        let url = server.url("feed.example", path);
        async move {
            let url = fetcher.policy().parse_url(&url).unwrap();
            fetcher.fetch(&url, &Conditional::default()).await
        }
    };
    assert_eq!(fetch("/declared").await, Err(SubscriptionError::TooLarge));
    assert_eq!(fetch("/chunked").await, Err(SubscriptionError::TooLarge));
    let started = std::time::Instant::now();
    assert_eq!(
        fetch("/hang").await,
        Err(SubscriptionError::FetchFailed(Some("timeout")))
    );
    assert!(started.elapsed() < Duration::from_secs(3));
    assert_eq!(fetch("/html").await, Err(SubscriptionError::NotICalendar));
    assert_eq!(
        fetch("/not-ical").await,
        Err(SubscriptionError::NotICalendar)
    );
    assert!(matches!(
        fetch("/gzip").await,
        Err(SubscriptionError::FetchFailed(_))
    ));
    assert_eq!(
        fetch("/missing").await,
        Err(SubscriptionError::HttpStatus(404))
    );
    // 304 without a conditional request is an error.
    assert_eq!(
        fetch("/unmodified").await,
        Err(SubscriptionError::HttpStatus(304))
    );
    assert!(matches!(fetch("/ok").await, Ok(Fetched::Body { .. })));
    // Nothing listens on port 9 of the exempt address: generic failure.
    let closed = fetcher_with(9, TestResolver::local(), Duration::from_secs(5), 3);
    let url = closed.policy().parse_url("http://feed.example:9/").unwrap();
    assert_eq!(
        closed.fetch(&url, &Conditional::default()).await,
        Err(SubscriptionError::FetchFailed(None))
    );
}

// --- service ---------------------------------------------------------------

#[tokio::test]
async fn create_refresh_and_sync_semantics() {
    let server = Server::start().await;
    let body = Arc::new(Mutex::new(feed(&[("a", "Alpha"), ("b", "Beta")])));
    server.respond({
        let body = body.clone();
        move |request| {
            let current = body.lock().unwrap().clone();
            let etag = format!("\"{}\"", blake3::hash(current.as_bytes()).to_hex());
            if request.contains(&format!("If-None-Match: {etag}"))
                || request.contains(&format!("if-none-match: {etag}"))
            {
                return response("304 Not Modified", "", b"");
            }
            calendar(&current, &format!("ETag: {etag}\r\n"))
        }
    });
    let (service, user) = service(&server, TestResolver::local()).await;
    let url = server.url("feed.example", &format!("/cal.ics?token={SECRET}"));
    let sub = service.create(&user, request(&url)).await.unwrap();
    assert_eq!(sub.items, 2);
    assert!(sub.collection.read_only);
    assert_eq!(sub.source_url, url);
    assert_eq!(sub.feed_ttl_secs, Some(7200));
    assert_eq!(service.interval(sub.refresh_secs, sub.feed_ttl_secs), 7200);
    let store = service.store().clone();
    let id = sub.collection.id;
    let revision = || {
        let store = store.clone();
        async move {
            store
                .subscription_by_collection(id)
                .await
                .unwrap()
                .unwrap()
                .collection
                .sync_revision
        }
    };
    let etags = || {
        let store = store.clone();
        async move {
            store
                .items(id)
                .await
                .unwrap()
                .into_iter()
                .map(|m| (m.name, m.etag))
                .collect::<std::collections::BTreeMap<_, _>>()
        }
    };
    let r1 = revision().await;
    let before = etags().await;

    // Unchanged feed: 304 (the ETag is sent back), no new revision.
    let sub = service.refresh(id).await.unwrap();
    assert_eq!(sub.last_error, None);
    assert_eq!(revision().await, r1);
    assert!(
        server.requests().last().unwrap().contains("if-none-match")
            || server.requests().last().unwrap().contains("If-None-Match")
    );

    // Same body without validators: recognised by its hash.
    server.respond({
        let body = body.clone();
        move |_| calendar(&body.lock().unwrap(), "")
    });
    service.refresh(id).await.unwrap();
    assert_eq!(revision().await, r1);
    assert_eq!(etags().await, before);

    // b changes, a disappears, c is new: one new revision, a changed only
    // b's entity tag, a's tombstone and c in the change log.
    *body.lock().unwrap() = feed(&[("b", "Beta 2"), ("c", "Gamma")]);
    service.refresh(id).await.unwrap();
    let r2 = revision().await;
    assert_eq!(r2, r1 + 1);
    let after = etags().await;
    assert_eq!(after.len(), 2);
    let changes = store.changes_since(id, r1, 100).await.unwrap();
    assert_eq!(changes.len(), 3);
    assert_eq!(changes.iter().filter(|c| c.meta.is_none()).count(), 1);
    // Stable names: b keeps its name, with a new entity tag.
    let b_name = before.keys().find(|n| after.contains_key(*n)).unwrap();
    assert_ne!(before[b_name], after[b_name]);

    // A new feed with b unchanged and c changed: only c changes.
    *body.lock().unwrap() = feed(&[("b", "Beta 2"), ("c", "Gamma 2")]);
    service.refresh(id).await.unwrap();
    let changes = store.changes_since(id, r2, 100).await.unwrap();
    assert_eq!(changes.len(), 1);
    assert_eq!(etags().await[b_name], after[b_name]);
}

#[tokio::test]
async fn broken_feeds_keep_the_last_good_content() {
    let server = Server::start().await;
    server.respond(|_| calendar(&feed(&[("a", "Alpha")]), ""));
    let (service, user) = service(&server, TestResolver::local()).await;
    let url = server.url("feed.example", "/cal.ics");
    let sub = service.create(&user, request(&url)).await.unwrap();
    let id = sub.collection.id;
    let revision = sub.collection.sync_revision;
    let broken: [(Reply, &str); 4] = [
        (
            response("500 Internal Server Error", "", b"oops"),
            "fetch_failed",
        ),
        (
            response("200 OK", "Content-Type: text/html\r\n", b"<html></html>"),
            "not_icalendar",
        ),
        (
            calendar(
                "BEGIN:VCALENDAR\r\nBEGIN:VEVENT\r\nUID:x\r\nDTSTART:garbage\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n",
                "",
            ),
            "not_icalendar",
        ),
        (
            calendar("BEGIN:VCALENDAR\r\nBEGIN:VEVENT\r\n", ""),
            "not_icalendar",
        ),
    ];
    for (failures, (reply, code)) in broken.into_iter().enumerate() {
        let reply = Mutex::new(Some(reply));
        server.respond(move |_| reply.lock().unwrap().take().unwrap_or(Reply::Hang));
        let sub = service.refresh(id).await.unwrap();
        assert_eq!(sub.last_error.as_deref(), Some(code));
        assert_eq!(sub.failures, i64::try_from(failures).unwrap() + 1);
        assert_eq!(sub.items, 1, "{code}: last good content kept");
        assert_eq!(sub.collection.sync_revision, revision);
        // Back-off: 15 minutes, doubling.
        let delay = sub.next_refresh_at - sub.last_attempt_at.unwrap();
        assert_eq!(delay, backoff(7200, sub.failures));
    }
    assert_eq!(backoff(7200, 1), 900);
    assert_eq!(backoff(7200, 2), 1800);
    assert_eq!(backoff(7200, 30), 86_400);
    assert_eq!(
        backoff(MAX_SUBSCRIPTION_REFRESH_SECS, 30),
        MAX_SUBSCRIPTION_REFRESH_SECS
    );
    // Recovery clears the error.
    server.respond(|_| calendar(&feed(&[("a", "Alpha"), ("b", "Beta")]), ""));
    let sub = service.refresh(id).await.unwrap();
    assert_eq!((sub.last_error, sub.failures, sub.items), (None, 0, 2));
    // Partly broken feeds keep the valid objects and count the others.
    server.respond(|_| {
        calendar(
            "BEGIN:VCALENDAR\r\nBEGIN:VEVENT\r\nUID:ok\r\nDTSTAMP:20261001T000000Z\r\nDTSTART:20261021T100000Z\r\nEND:VEVENT\r\n\
             BEGIN:VEVENT\r\nUID:bad\r\nDTSTART:garbage\r\nEND:VEVENT\r\nBEGIN:VJOURNAL\r\nUID:j\r\nDTSTAMP:20261001T000000Z\r\nEND:VJOURNAL\r\nEND:VCALENDAR\r\n",
            "",
        )
    });
    let sub = service.refresh(id).await.unwrap();
    assert_eq!((sub.items, sub.skipped, sub.last_error), (1, 2, None));
}

#[tokio::test]
async fn create_errors_store_nothing() {
    let server = Server::start().await;
    let (service, user) = service(&server, TestResolver::local()).await;
    let store = service.store().clone();
    let cases: Vec<(String, Reply, &str)> = vec![
        (
            "https://10.0.0.1/".to_owned(),
            Reply::Hang,
            "blocked_address",
        ),
        ("ftp://feed.example/".to_owned(), Reply::Hang, "invalid_url"),
        (
            server.url("feed.example", "/x"),
            response("403 Forbidden", "", b""),
            "fetch_failed",
        ),
        (
            server.url("feed.example", "/x"),
            response("200 OK", "Content-Type: text/html\r\n", b"<html>"),
            "not_icalendar",
        ),
        (
            server.url("feed.example", "/x"),
            calendar(
                &format!("BEGIN:VCALENDAR\r\nX:{}\r\n", "x".repeat(70_000)),
                "",
            ),
            "too_large",
        ),
    ];
    for (url, reply, code) in cases {
        let reply = Mutex::new(Some(reply));
        server.respond(move |_| reply.lock().unwrap().take().unwrap_or(Reply::Hang));
        let err = feed_error(service.create(&user, request(&url)).await.unwrap_err());
        assert_eq!(err.code(), code, "{url}");
    }
    assert_eq!(store.count_subscriptions(user.id).await.unwrap(), 0);
    let slug: Slug = "holidays".parse().unwrap();
    assert!(
        store
            .collection(user.id, CollectionKind::Calendar, &slug)
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn quota_and_per_user_cap() {
    let server = Server::start().await;
    server.respond(|_| calendar(&feed(&[("a", "Alpha"), ("b", "Beta")]), ""));
    let (service, user) = service(&server, TestResolver::local()).await;
    let store = service.store().clone();
    let url = server.url("feed.example", "/cal.ics");
    store.set_quota_for_tests(200);
    let err = feed_error(service.create(&user, request(&url)).await.unwrap_err());
    assert_eq!(
        err,
        SubscriptionError::LimitReached("storage quota exceeded")
    );
    store.set_quota_for_tests(0);
    let sub = service.create(&user, request(&url)).await.unwrap();
    // A refresh that would exceed the quota fails and keeps the content.
    store.set_quota_for_tests(200);
    let sub = service.refresh(sub.collection.id).await.unwrap();
    assert_eq!(sub.last_error.as_deref(), Some("limit_reached"));
    assert_eq!(sub.items, 2);
    store.set_quota_for_tests(0);
    // Two subscriptions per user in this test.
    let mut second = request(&url);
    second.slug = None;
    service.create(&user, second.clone()).await.unwrap();
    let err = feed_error(service.create(&user, second).await.unwrap_err());
    assert_eq!(
        err,
        SubscriptionError::LimitReached("too many subscriptions")
    );
    assert_eq!(store.count_subscriptions(user.id).await.unwrap(), 2);
}

#[tokio::test]
async fn refresh_now_is_rate_limited() {
    let server = Server::start().await;
    server.respond(|_| calendar(&feed(&[("a", "Alpha")]), ""));
    let store = Store::in_memory().await.unwrap();
    let user = store
        .create_user(&"alice".parse().unwrap(), "h", None)
        .await
        .unwrap();
    let service = Subscriptions::with_fetcher(
        store,
        &config(server.port),
        fetcher(server.port, TestResolver::local()),
    );
    let url = server.url("feed.example", "/cal.ics");
    let sub = service.create(&user, request(&url)).await.unwrap();
    // Creating counts as the first fetch: refreshing right away is refused
    // without contacting the server.
    let before = server.requests().len();
    let err = feed_error(service.refresh(sub.collection.id).await.unwrap_err());
    assert_eq!(err, SubscriptionError::RateLimited);
    assert_eq!(server.requests().len(), before);
    // Also while a refresh is running.
    let _guard = service.claim(sub.collection.id).unwrap();
    let err = feed_error(service.refresh(sub.collection.id).await.unwrap_err());
    assert_eq!(err, SubscriptionError::RateLimited);
}

#[tokio::test]
async fn background_task_refreshes_due_subscriptions() {
    let server = Server::start().await;
    server.respond(|_| calendar(&feed(&[("a", "Alpha")]), ""));
    let (service, user) = service(&server, TestResolver::local()).await;
    let url = server.url("feed.example", "/cal.ics");
    let sub = service.create(&user, request(&url)).await.unwrap();
    assert_eq!(service.run_due().await, 0, "not due yet");
    let id = sub.collection.id;
    service
        .store()
        .update_subscription(id, None, 0)
        .await
        .unwrap();
    server.respond(|_| calendar(&feed(&[("a", "Alpha"), ("b", "Beta")]), ""));
    assert_eq!(service.run_due().await, 1);
    for _ in 0..100 {
        if service
            .store()
            .subscription_by_collection(id)
            .await
            .unwrap()
            .unwrap()
            .items
            == 2
        {
            return;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("the background refresh did not run");
}

#[test]
fn intervals_are_bounded() {
    let config = SubscriptionsConfig::default();
    let store_free = |refresh: Option<i64>, ttl: Option<i64>| {
        clamp_interval(refresh.or(ttl).unwrap_or(config.default_refresh_secs()))
    };
    assert_eq!(store_free(None, None), 6 * 3600);
    assert_eq!(store_free(None, Some(60)), 15 * 60);
    assert_eq!(store_free(None, Some(30 * 86_400)), 7 * 86_400);
    assert_eq!(store_free(Some(3600), Some(60)), 3600);
}

// --- REST API and DAV --------------------------------------------------------

struct Reply2 {
    status: u16,
    body: String,
    headers: axum::http::HeaderMap,
}

async fn call(
    app: &Router,
    method: &str,
    uri: &str,
    headers: &[(&str, &str)],
    body: &str,
) -> Reply2 {
    let mut builder = Request::builder()
        .method(Method::from_bytes(method.as_bytes()).unwrap())
        .uri(uri);
    for (name, value) in headers {
        builder = builder.header(*name, *value);
    }
    let response = app
        .clone()
        .oneshot(builder.body(Body::from(body.to_owned())).unwrap())
        .await
        .unwrap();
    let status = response.status().as_u16();
    let headers = response.headers().clone();
    let body = response.into_body().collect().await.unwrap().to_bytes();
    Reply2 {
        status,
        headers,
        body: String::from_utf8(body.to_vec()).unwrap(),
    }
}

#[tokio::test]
async fn api_and_dav_end_to_end() {
    use base64::Engine;
    let server = Server::start().await;
    server.respond(|_| calendar(&feed(&[("a", "Alpha"), ("b", "Beta")]), ""));
    let (service, _) = service(&server, TestResolver::local()).await;
    let store = service.store().clone();
    let password = "alice password 123";
    store
        .set_password_hash(
            &"alice".parse().unwrap(),
            &crate::auth::hash_password(password).unwrap(),
        )
        .await
        .unwrap();
    let alice = store.user_by_id(1).await.unwrap().unwrap();
    let token = "alice-session-token-0123456789abcdef";
    store
        .create_session(alice.id, &hash_secret(token), now(), now() + 3600)
        .await
        .unwrap();
    let mut state = AppState::new(store.clone()).unwrap();
    state.subscriptions = Some(service.clone());
    let app = router(&Config::default(), state);
    let cookie = format!("calcraft_session={token}");
    let json = [
        ("cookie", cookie.as_str()),
        ("x-calcraft-request", "1"),
        ("content-type", "application/json"),
    ];
    let url = server.url("feed.example", &format!("/cal.ics?token={SECRET}"));

    // Errors come with their codes.
    for (body, status, code) in [
        (r#"{"url": "http://"}"#.to_owned(), 400, "invalid_url"),
        (
            r#"{"url": "https://[::1]/cal.ics"}"#.to_owned(),
            400,
            "blocked_address",
        ),
        (
            r#"{"url": "https://169.254.169.254/latest"}"#.to_owned(),
            400,
            "blocked_address",
        ),
        (
            format!(r#"{{"url": "{url}", "refresh_minutes": 5}}"#),
            400,
            "bad_request",
        ),
    ] {
        let reply = call(&app, "POST", "/api/v1/subscriptions", &json, &body).await;
        assert_eq!(reply.status, status, "{body}: {}", reply.body);
        assert!(
            reply.body.contains(&format!("\"error\":\"{code}\"")),
            "{}",
            reply.body
        );
    }

    let body = format!(
        r##"{{"url": "{url}", "slug": "feed", "display_name": "Feed", "color": "#336699", "refresh_minutes": 60}}"##
    );
    let reply = call(&app, "POST", "/api/v1/subscriptions", &json, &body).await;
    assert_eq!(reply.status, 201, "{}", reply.body);
    let info: calcraft_core::api::SubscriptionInfo = serde_json::from_str(&reply.body).unwrap();
    assert_eq!(info.url, url);
    assert_eq!(info.objects, 2);
    assert_eq!(info.refresh_minutes, Some(60));
    assert_eq!(info.effective_refresh_minutes, 60);
    assert_eq!(info.href, "/dav/calendars/alice/feed/");

    let reply = call(&app, "GET", "/api/v1/subscriptions", &json[..1], "").await;
    assert_eq!(reply.status, 200);
    assert!(reply.body.contains("\"slug\":\"feed\""));
    let reply = call(&app, "GET", "/api/v1/collections", &json[..1], "").await;
    assert!(reply.body.contains("\"read_only\":true"), "{}", reply.body);

    let reply = call(
        &app,
        "PATCH",
        "/api/v1/subscriptions/feed",
        &json,
        r#"{"display_name": "Renamed", "refresh_minutes": 0}"#,
    )
    .await;
    assert_eq!(reply.status, 200, "{}", reply.body);
    let info: calcraft_core::api::SubscriptionInfo = serde_json::from_str(&reply.body).unwrap();
    assert_eq!(info.display_name.as_deref(), Some("Renamed"));
    assert_eq!(
        (info.refresh_minutes, info.effective_refresh_minutes),
        (None, 120)
    );
    // The collection endpoints cannot change the content of a subscription.
    let reply = call(
        &app,
        "PATCH",
        "/api/v1/collections/calendar/feed",
        &json,
        r#"{"description": "x"}"#,
    )
    .await;
    assert_eq!(reply.status, 403);
    let reply = call(
        &app,
        "POST",
        "/api/v1/collections/calendar/feed/import",
        &[json[0], json[1], ("content-type", "text/calendar")],
        &feed(&[("z", "Z")]),
    )
    .await;
    assert_eq!(reply.status, 403, "{}", reply.body);
    assert!(reply.body.contains("read_only"));

    // Refresh now works (no rate limit in this test service)...
    let reply = call(
        &app,
        "POST",
        "/api/v1/subscriptions/feed/refresh",
        &json[..2],
        "",
    )
    .await;
    assert_eq!(reply.status, 200, "{}", reply.body);
    // ...and is answered 429 while one is running.
    let guard = service.claim(
        store
            .subscription(alice.id, &"feed".parse().unwrap())
            .await
            .unwrap()
            .unwrap()
            .collection
            .id,
    );
    let reply = call(
        &app,
        "POST",
        "/api/v1/subscriptions/feed/refresh",
        &json[..2],
        "",
    )
    .await;
    assert_eq!(reply.status, 429);
    assert_eq!(reply.headers.get("retry-after").unwrap(), "60");
    assert!(reply.body.contains("rate_limited"));
    drop(guard);

    // DAV: read-only for every client.
    let basic = format!(
        "Basic {}",
        base64::engine::general_purpose::STANDARD.encode(format!("alice:{password}"))
    );
    let dav = [("authorization", basic.as_str())];
    let items = store.items(info_id(&store, alice.id).await).await.unwrap();
    let object = format!("/dav/calendars/alice/feed/{}", items[0].name);
    let reply = call(&app, "GET", &object, &dav, "").await;
    assert_eq!(reply.status, 200);
    let put_headers = [dav[0], ("content-type", "text/calendar")];
    for (method, uri, headers, body) in [
        ("PUT", object.as_str(), &put_headers[..], feed(&[("a", "Changed")])),
        ("PUT", "/dav/calendars/alice/feed/new.ics", &put_headers[..], feed(&[("n", "New")])),
        ("DELETE", object.as_str(), &dav[..], String::new()),
        (
            "PROPPATCH",
            "/dav/calendars/alice/feed/",
            &dav[..],
            r#"<?xml version="1.0"?><d:propertyupdate xmlns:d="DAV:"><d:set><d:prop><d:displayname>x</d:displayname></d:prop></d:set></d:propertyupdate>"#.to_owned(),
        ),
    ] {
        let reply = call(&app, method, uri, headers, &body).await;
        assert_eq!(reply.status, 403, "{method} {uri}: {}", reply.body);
        assert!(reply.body.contains("need-privileges"), "{method} {uri}: {}", reply.body);
    }
    let move_headers = [
        dav[0],
        ("destination", "/dav/calendars/alice/default/moved.ics"),
    ];
    let reply = call(&app, "MOVE", &object, &move_headers, "").await;
    assert_eq!(reply.status, 403, "{}", reply.body);
    let copy_in = [
        dav[0],
        ("destination", "/dav/calendars/alice/feed/copied.ics"),
    ];
    let reply = call(
        &app,
        "PUT",
        "/dav/calendars/alice/default/own.ics",
        &put_headers,
        &feed(&[("own", "Own")]),
    )
    .await;
    assert_eq!(reply.status, 201);
    let reply = call(
        &app,
        "COPY",
        "/dav/calendars/alice/default/own.ics",
        &copy_in,
        "",
    )
    .await;
    assert_eq!(reply.status, 403, "{}", reply.body);
    // Clients learn that the calendar is read-only.
    let propfind = r#"<?xml version="1.0"?><d:propfind xmlns:d="DAV:"><d:prop><d:current-user-privilege-set/></d:prop></d:propfind>"#;
    let reply = call(
        &app,
        "PROPFIND",
        "/dav/calendars/alice/feed/",
        &[dav[0], ("depth", "1")],
        propfind,
    )
    .await;
    assert_eq!(reply.status, 207);
    assert!(
        reply.body.contains("<d:read/>") || reply.body.contains(":read/>"),
        "{}",
        reply.body
    );
    assert!(!reply.body.contains("write"), "{}", reply.body);
    let reply = call(
        &app,
        "PROPFIND",
        "/dav/calendars/alice/default/",
        &[dav[0], ("depth", "0")],
        propfind,
    )
    .await;
    assert!(reply.body.contains("write-content"), "{}", reply.body);

    // Deleting the subscription removes the calendar for good.
    let reply = call(&app, "DELETE", "/api/v1/subscriptions/feed", &json[..2], "").await;
    assert_eq!(reply.status, 204);
    let reply = call(&app, "GET", "/api/v1/subscriptions", &json[..1], "").await;
    assert_eq!(reply.body, "[]");
    assert_eq!(store.trash_entries(alice.id, 10, 0).await.unwrap().total, 0);
    let reply = call(&app, "DELETE", "/api/v1/subscriptions/feed", &json[..2], "").await;
    assert_eq!(reply.status, 404);
}

async fn info_id(store: &Store, owner: i64) -> i64 {
    store
        .subscription(owner, &"feed".parse().unwrap())
        .await
        .unwrap()
        .unwrap()
        .collection
        .id
}
