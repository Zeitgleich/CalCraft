//! `calcraft migrate` against an in-process fake CalDAV/CardDAV server
//! (axum on 127.0.0.1; TLS through a small rustls proxy with a test CA).
//! Nothing here contacts a real server.

#![cfg(feature = "migrate")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::{
    io::{ErrorKind, Read, Write},
    net::{SocketAddr, TcpListener as StdListener, TcpStream},
    path::PathBuf,
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicBool, AtomicU32, Ordering},
    },
    time::Duration,
};

use axum::{
    Router,
    body::Bytes,
    extract::State,
    http::{HeaderMap, Method, StatusCode, Uri, header},
    response::{IntoResponse, Response},
};
use base64::{Engine, engine::general_purpose::STANDARD};
use calcraft_core::CollectionKind;
use calcraft_server::{
    auth::hash_password,
    migrate::{self, Options, Password, Summary},
    telemetry,
    transfer::OnConflict,
};
use calcraft_store::{Collection, Store};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, pem::PemObject};
use tracing_subscriber::{EnvFilter, util::SubscriberInitExt};

const SOURCE_PASSWORD: &str = "Src-Secret-Pw-0815";
const LOCAL_USER: &str = "alice";

// Test-only CA and a certificate for 127.0.0.1 signed by it (P-256, valid
// until 2126). Not used anywhere outside these tests.
const CA_PEM: &str = "-----BEGIN CERTIFICATE-----
MIIBnTCCAUOgAwIBAgIUbB+9FSKYSj5PYe2UpKP6E6Kr6BcwCgYIKoZIzj0EAwIw
GzEZMBcGA1UEAwwQQ2FsQ3JhZnQgVGVzdCBDQTAgFw0yNjEwMjIxNDIyMzRaGA8y
MTI2MDkyODE0MjIzNFowGzEZMBcGA1UEAwwQQ2FsQ3JhZnQgVGVzdCBDQTBZMBMG
ByqGSM49AgEGCCqGSM49AwEHA0IABFgJb2BvsKCAhIC4m+Ocnko23Arg5Edg3G0Z
4Dho3tgzabfUNuMgcYvNQYjSxHDw/fNlEBNsimC8FHM1I1LBXtCjYzBhMB0GA1Ud
DgQWBBSd2y4yhYUJtXKISvKxUHt55uyDcTAfBgNVHSMEGDAWgBSd2y4yhYUJtXKI
SvKxUHt55uyDcTAPBgNVHRMBAf8EBTADAQH/MA4GA1UdDwEB/wQEAwIBBjAKBggq
hkjOPQQDAgNIADBFAiAGThhgpWqM4W6j1gTZpe4a1olSddCM9w16I5/Ha9nqKAIh
AJNdwM964228jQZrre5vf08W3zyDFX5rwYeQO867UnC3
-----END CERTIFICATE-----
";

const SERVER_PEM: &str = "-----BEGIN CERTIFICATE-----
MIIBqTCCAU+gAwIBAgIULQZgkvv3Xons0LCdF4XGo16nVE0wCgYIKoZIzj0EAwIw
GzEZMBcGA1UEAwwQQ2FsQ3JhZnQgVGVzdCBDQTAgFw0yNjEwMjIxNDIyMzRaGA8y
MTI2MDkyODE0MjIzNFowFDESMBAGA1UEAwwJMTI3LjAuMC4xMFkwEwYHKoZIzj0C
AQYIKoZIzj0DAQcDQgAEAV9zFymWaG/lEA4XCp2c47OfLGoq8nMoiUo4ENj7zfr1
PqCU6jr/Md1ntZR8Smu28pPjfF07etCWEJ1h9/NdJKN2MHQwDAYDVR0TAQH/BAIw
ADATBgNVHSUEDDAKBggrBgEFBQcDATAPBgNVHREECDAGhwR/AAABMB0GA1UdDgQW
BBQT/P6x990FqJ3P524+J6YaJjyrHzAfBgNVHSMEGDAWgBSd2y4yhYUJtXKISvKx
UHt55uyDcTAKBggqhkjOPQQDAgNIADBFAiEAhdU82tNNAKNNtz18yRr0bUava5Ys
CL3myu8fY4tmqDQCIAo4R3ZNjhFO8QfhIR72o8OFPoDwlVPV8o0KJHLsnjVu
-----END CERTIFICATE-----
";

const SERVER_KEY: &str = "-----BEGIN PRIVATE KEY-----
MIGHAgEAMBMGByqGSM49AgEGCCqGSM49AwEHBG0wawIBAQQghROnV7ygevzZtQ0G
rYtbrTCTGsSVdzVM4WWmPlCdDX6hRANCAAQBX3MXKZZob+UQDhcKnZzjs58sairy
cyiJSjgQ2PvN+vU+oJTqOv8x3We1lHxKa7byk+N8XTt60JYQnWH3810k
-----END PRIVATE KEY-----
";

const EVENT_ONE: &str = concat!(
    "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//Synology//Calendar//EN\r\n",
    "BEGIN:VTIMEZONE\r\nTZID:Europe/Berlin\r\nBEGIN:STANDARD\r\nDTSTART:19701025T030000\r\n",
    "TZOFFSETFROM:+0200\r\nTZOFFSETTO:+0100\r\nEND:STANDARD\r\nEND:VTIMEZONE\r\n",
    "BEGIN:VEVENT\r\nUID:one@example.com\r\nDTSTAMP:20261001T080000Z\r\n",
    "DTSTART;TZID=Europe/Berlin:20261021T100000\r\nDURATION:PT1H\r\n",
    "RRULE:FREQ=WEEKLY;COUNT=4\r\nSUMMARY:Weekly <sync> & more\r\nEND:VEVENT\r\n",
    "END:VCALENDAR\r\n",
);

const EVENT_TWO: &str = concat!(
    "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//Test//EN\r\n",
    "BEGIN:VEVENT\r\nUID:two@example.com\r\nDTSTAMP:20261001T080000Z\r\n",
    "DTSTART;VALUE=DATE:20261224\r\nSUMMARY:Holiday\r\nEND:VEVENT\r\n",
    "END:VCALENDAR\r\n",
);

/// Rejected by validation: ends before it starts.
const EVENT_BAD: &str = concat!(
    "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//Test//EN\r\n",
    "BEGIN:VEVENT\r\nUID:bad@example.com\r\nDTSTAMP:20261001T080000Z\r\n",
    "DTSTART:20261021T100000Z\r\nDTEND:20261020T100000Z\r\nEND:VEVENT\r\n",
    "END:VCALENDAR\r\n",
);

const TODO: &str = concat!(
    "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//Test//EN\r\n",
    "BEGIN:VTODO\r\nUID:todo@example.com\r\nDTSTAMP:20261001T080000Z\r\n",
    "SUMMARY:Buy milk\r\nEND:VTODO\r\nEND:VCALENDAR\r\n",
);

const SHARED_EVENT: &str = concat!(
    "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//Test//EN\r\n",
    "BEGIN:VEVENT\r\nUID:shared@example.com\r\nDTSTAMP:20261001T080000Z\r\n",
    "DTSTART;VALUE=DATE:20261101\r\nSUMMARY:Bob's\r\nEND:VEVENT\r\n",
    "END:VCALENDAR\r\n",
);

const CARD: &str = concat!(
    "BEGIN:VCARD\r\nVERSION:3.0\r\nN:Muster;Max;;;\r\nFN:Max Muster\r\n",
    "UID:card-max\r\nEMAIL:max@example.com\r\nEND:VCARD\r\n",
);

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

fn photo_card() -> String {
    let photo = format!("/9j/4AAQSkZJRgABAQAA{}", "A".repeat(8000));
    format!(
        "BEGIN:VCARD\r\nVERSION:3.0\r\nN:Example;Paula;;;\r\nFN:Paula Example\r\nUID:card-paula\r\n{}END:VCARD\r\n",
        fold(&format!("PHOTO;ENCODING=b;TYPE=JPEG:{photo}"))
    )
}

/// A collection on the fake server.
struct FakeCollection {
    /// `calendars` or `addressbooks`.
    home: &'static str,
    slug: &'static str,
    /// Extra resourcetype elements and properties.
    resourcetype: &'static str,
    props: &'static str,
    /// Object name (unencoded) and content.
    objects: Vec<(&'static str, String)>,
}

struct Fake {
    prefix: &'static str,
    multiget: bool,
    /// Multiget answers also repeat hrefs (with other data) and add hrefs
    /// that were not asked for.
    noisy: AtomicBool,
    /// Where `/.well-known/*` redirects to.
    well_known: Mutex<String>,
    collections: Vec<FakeCollection>,
    /// `METHOD host path` of every request.
    log: Mutex<Vec<String>>,
}

impl Fake {
    fn new(prefix: &'static str, multiget: bool) -> Self {
        Self {
            prefix,
            multiget,
            noisy: AtomicBool::new(false),
            well_known: Mutex::new(format!("{prefix}/")),
            collections: vec![
                FakeCollection {
                    home: "calendars",
                    slug: "work",
                    resourcetype: "<c:calendar/>",
                    props: "<d:displayname>Work</d:displayname>\
                            <ic:calendar-color>#FF8800FF</ic:calendar-color>\
                            <c:calendar-description>Job &amp; meetings</c:calendar-description>\
                            <c:supported-calendar-component-set><c:comp name=\"VEVENT\"/></c:supported-calendar-component-set>\
                            <c:calendar-timezone>BEGIN:VCALENDAR&#13;\nEND:VCALENDAR&#13;\n</c:calendar-timezone>",
                    objects: vec![
                        ("one.ics", EVENT_ONE.to_owned()),
                        ("two@example.com.ics", EVENT_TWO.to_owned()),
                        ("bad.ics", EVENT_BAD.to_owned()),
                    ],
                },
                FakeCollection {
                    home: "calendars",
                    slug: "Tasks List",
                    resourcetype: "<c:calendar/>",
                    props: "<d:displayname>Tasks</d:displayname>\
                            <ic:calendar-color>00AA00</ic:calendar-color>\
                            <c:supported-calendar-component-set><c:comp name=\"VTODO\"/></c:supported-calendar-component-set>",
                    objects: vec![("todo.ics", TODO.to_owned())],
                },
                FakeCollection {
                    home: "calendars",
                    slug: "bobs",
                    resourcetype: "<c:calendar/><cs:shared/>",
                    props: "<d:displayname>Bob</d:displayname>",
                    objects: vec![("shared.ics", SHARED_EVENT.to_owned())],
                },
                FakeCollection {
                    home: "calendars",
                    slug: "inbox",
                    resourcetype: "<c:schedule-inbox/>",
                    props: "",
                    objects: Vec::new(),
                },
                FakeCollection {
                    home: "addressbooks",
                    slug: "contacts",
                    resourcetype: "<card:addressbook/>",
                    props: "<d:displayname>Contacts</d:displayname>\
                            <card:addressbook-description>Friends</card:addressbook-description>",
                    objects: vec![("paula.vcf", photo_card()), ("max.vcf", CARD.to_owned())],
                },
            ],
            log: Mutex::new(Vec::new()),
        }
    }

    fn expected_auth() -> String {
        format!(
            "Basic {}",
            STANDARD.encode(format!("alice-src:{SOURCE_PASSWORD}"))
        )
    }

    fn requests(&self) -> Vec<String> {
        self.log.lock().unwrap().clone()
    }
}

fn encode(segment: &str) -> String {
    segment
        .replace('%', "%25")
        .replace(' ', "%20")
        .replace('@', "%40")
}

fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

fn multistatus(responses: &str) -> Response {
    (
        StatusCode::MULTI_STATUS,
        [(header::CONTENT_TYPE, "application/xml; charset=utf-8")],
        format!(
            "<?xml version=\"1.0\" encoding=\"utf-8\"?>\n<d:multistatus xmlns:d=\"DAV:\" \
             xmlns:c=\"urn:ietf:params:xml:ns:caldav\" xmlns:card=\"urn:ietf:params:xml:ns:carddav\" \
             xmlns:cs=\"http://calendarserver.org/ns/\" xmlns:ic=\"http://apple.com/ns/ical/\">\n\
             {responses}</d:multistatus>\n"
        ),
    )
        .into_response()
}

fn response(href: &str, props: &str) -> String {
    format!(
        "<d:response><d:href>{href}</d:href><d:propstat><d:prop>{props}</d:prop>\
         <d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response>\n"
    )
}

fn etag(content: &str) -> String {
    format!("\"{}\"", content.len())
}

async fn handle(
    State(fake): State<Arc<Fake>>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let host = headers
        .get(header::HOST)
        .and_then(|h| h.to_str().ok())
        .unwrap_or("")
        .to_owned();
    let path = uri.path().to_owned();
    fake.log
        .lock()
        .unwrap()
        .push(format!("{method} {host} {path}"));
    if path.starts_with("/.well-known/") {
        let target = fake.well_known.lock().unwrap().clone();
        return (StatusCode::MOVED_PERMANENTLY, [(header::LOCATION, target)]).into_response();
    }
    if path.starts_with("/loop/") {
        return (StatusCode::FOUND, [(header::LOCATION, path)]).into_response();
    }
    let p = fake.prefix;
    if !path.starts_with(&format!("{p}/")) {
        return StatusCode::NOT_FOUND.into_response();
    }
    let authorized = headers
        .get(header::AUTHORIZATION)
        .and_then(|h| h.to_str().ok())
        == Some(Fake::expected_auth().as_str());
    if !authorized {
        return (
            StatusCode::UNAUTHORIZED,
            [(header::WWW_AUTHENTICATE, "Basic realm=\"fake\"")],
        )
            .into_response();
    }
    let principal = format!("{p}/principals/alice-src/");
    match method.as_str() {
        "PROPFIND" if path == format!("{p}/") => multistatus(&response(
            &path,
            &format!(
                "<d:current-user-principal><d:href>{principal}</d:href></d:current-user-principal>"
            ),
        )),
        "PROPFIND" if path == principal => multistatus(&response(
            &path,
            &format!(
                "<c:calendar-home-set><d:href>{p}/calendars/alice-src/</d:href></c:calendar-home-set>\
                 <card:addressbook-home-set><d:href>{p}/addressbooks/alice-src/</d:href></card:addressbook-home-set>"
            ),
        )),
        "PROPFIND" => propfind(&fake, &path),
        "REPORT" if fake.multiget => multiget(&fake, &path, &body),
        // Synology-like: no multiget.
        "REPORT" => StatusCode::NOT_IMPLEMENTED.into_response(),
        "GET" => match find_object(&fake, &path) {
            Some(content) => (
                StatusCode::OK,
                [(header::CONTENT_TYPE, "text/calendar; charset=utf-8")],
                content,
            )
                .into_response(),
            None => StatusCode::NOT_FOUND.into_response(),
        },
        _ => StatusCode::METHOD_NOT_ALLOWED.into_response(),
    }
}

fn collection_href(fake: &Fake, c: &FakeCollection) -> String {
    format!("{}/{}/alice-src/{}/", fake.prefix, c.home, encode(c.slug))
}

fn propfind(fake: &Fake, path: &str) -> Response {
    let p = fake.prefix;
    for home in ["calendars", "addressbooks"] {
        let home_href = format!("{p}/{home}/alice-src/");
        if path == home_href {
            let mut out = response(
                &home_href,
                "<d:resourcetype><d:collection/></d:resourcetype>",
            );
            for c in fake.collections.iter().filter(|c| c.home == home) {
                out.push_str(&response(
                    &collection_href(fake, c),
                    &format!(
                        "<d:resourcetype><d:collection/>{}</d:resourcetype>{}",
                        c.resourcetype, c.props
                    ),
                ));
            }
            return multistatus(&out);
        }
    }
    for c in &fake.collections {
        let href = collection_href(fake, c);
        if path == href {
            let mut out = response(&href, "<d:resourcetype><d:collection/></d:resourcetype>");
            for (name, content) in &c.objects {
                out.push_str(&response(
                    &format!("{href}{}", encode(name)),
                    &format!("<d:resourcetype/><d:getetag>{}</d:getetag>", etag(content)),
                ));
            }
            return multistatus(&out);
        }
    }
    StatusCode::NOT_FOUND.into_response()
}

fn find_object(fake: &Fake, path: &str) -> Option<String> {
    fake.collections.iter().find_map(|c| {
        let href = collection_href(fake, c);
        let name = path.strip_prefix(&href)?;
        c.objects
            .iter()
            .find(|(n, _)| encode(n) == name)
            .map(|(_, content)| content.clone())
    })
}

fn multiget(fake: &Fake, path: &str, body: &[u8]) -> Response {
    let limits = calcraft_ical::xml::XmlLimits {
        max_len: 1 << 20,
        max_depth: 8,
        max_nodes: 10_000,
    };
    let Ok(root) = calcraft_ical::xml::parse(body, &limits) else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    let data = if root.name == "calendar-multiget" {
        "c:calendar-data"
    } else {
        "card:address-data"
    };
    let mut out = String::new();
    for href in root.children_named("DAV:", "href") {
        let href = href.text.trim();
        if !href.starts_with(path) {
            continue;
        }
        match find_object(fake, href) {
            // Raw CRLF in the XML text, as Sabre/DAV and Radicale send it.
            Some(content) => out.push_str(&response(
                href,
                &format!(
                    "<d:getetag>{}</d:getetag><{data}>{}</{data}>",
                    etag(&content),
                    escape(&content)
                ),
            )),
            None => out.push_str(&format!(
                "<d:response><d:href>{href}</d:href><d:status>HTTP/1.1 404 Not Found</d:status></d:response>"
            )),
        }
        if fake.noisy.load(Ordering::Relaxed) {
            // The same href again with other data, and objects not asked for.
            out.push_str(&response(
                href,
                &format!("<{data}>{}</{data}>", escape(SHARED_EVENT)),
            ));
            out.push_str(&response(
                &format!("{}/calendars/alice-src/bobs/shared.ics", fake.prefix),
                &format!("<{data}>{}</{data}>", escape(SHARED_EVENT)),
            ));
            out.push_str(&response(
                &format!("{path}unasked.ics"),
                &format!("<{data}>{}</{data}>", escape(SHARED_EVENT)),
            ));
        }
    }
    multistatus(&out)
}

async fn serve_plain(router: Router) -> SocketAddr {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    addr
}

/// Terminates TLS with the test certificate and forwards to `upstream`.
fn tls_proxy(upstream: SocketAddr) -> SocketAddr {
    let certs: Vec<CertificateDer<'static>> = CertificateDer::pem_slice_iter(SERVER_PEM.as_bytes())
        .collect::<Result<_, _>>()
        .unwrap();
    let key = PrivateKeyDer::from_pem_slice(SERVER_KEY.as_bytes()).unwrap();
    let config = Arc::new(
        rustls::ServerConfig::builder_with_provider(Arc::new(
            rustls::crypto::ring::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_no_client_auth()
        .with_single_cert(certs, key)
        .unwrap(),
    );
    let listener = StdListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let config = config.clone();
            std::thread::spawn(move || relay(stream, config, upstream));
        }
    });
    addr
}

fn relay(tcp: TcpStream, config: Arc<rustls::ServerConfig>, upstream: SocketAddr) {
    let poll = Some(Duration::from_millis(5));
    let Ok(connection) = rustls::ServerConnection::new(config) else {
        return;
    };
    tcp.set_read_timeout(poll).unwrap();
    let mut tls = rustls::StreamOwned::new(connection, tcp);
    let Ok(mut plain) = TcpStream::connect(upstream) else {
        return;
    };
    plain.set_read_timeout(poll).unwrap();
    let idle = |e: &std::io::Error| matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut);
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        match tls.read(&mut buf) {
            Ok(0) => return,
            Ok(n) => {
                if plain.write_all(&buf[..n]).is_err() {
                    return;
                }
            }
            Err(e) if idle(&e) => {}
            Err(_) => return,
        }
        match plain.read(&mut buf) {
            Ok(0) => return,
            Ok(n) => {
                if tls.write_all(&buf[..n]).and_then(|()| tls.flush()).is_err() {
                    return;
                }
            }
            Err(e) if idle(&e) => {}
            Err(_) => return,
        }
    }
}

struct Server {
    fake: Arc<Fake>,
    plain: SocketAddr,
}

async fn start(fake: Fake) -> Server {
    let fake = Arc::new(fake);
    let router = Router::new().fallback(handle).with_state(fake.clone());
    let plain = serve_plain(router).await;
    Server { fake, plain }
}

/// A plain-HTTP listener that redirects everything to `https_port`, like a
/// Synology NAS from port 5000 to 5001.
async fn https_redirector(https_port: u16) -> SocketAddr {
    let router = Router::new().fallback(move |uri: Uri| async move {
        (
            StatusCode::PERMANENT_REDIRECT,
            [(
                header::LOCATION,
                format!("https://127.0.0.1:{https_port}{}", uri.path()),
            )],
        )
    });
    serve_plain(router).await
}

struct TempDir(PathBuf);

impl TempDir {
    fn new() -> Self {
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let dir = std::env::temp_dir().join(format!(
            "calcraft-migrate-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }

    fn ca_file(&self) -> PathBuf {
        let path = self.0.join("ca.pem");
        std::fs::write(&path, CA_PEM).unwrap();
        path
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

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

/// Everything logged by any test in this binary, at trace level.
fn logs() -> &'static Capture {
    static LOGS: OnceLock<Capture> = OnceLock::new();
    LOGS.get_or_init(|| {
        let capture = Capture::default();
        let writer = capture.clone();
        telemetry::json_subscriber(EnvFilter::new("trace"), move || writer.clone())
            .try_init()
            .unwrap();
        capture
    })
}

async fn local_store() -> Store {
    logs();
    let store = Store::in_memory().await.unwrap();
    let hash = hash_password("local password 123").unwrap();
    store
        .create_user(&LOCAL_USER.parse().unwrap(), &hash, None)
        .await
        .unwrap();
    store
}

fn options(from: String) -> Options {
    let mut options = Options::new(
        &from,
        "alice-src",
        Password::new(SOURCE_PASSWORD),
        LOCAL_USER,
    );
    options.batch_size = 2;
    options
}

async fn collections(store: &Store) -> Vec<Collection> {
    let user = store
        .user_credentials(&LOCAL_USER.parse().unwrap())
        .await
        .unwrap()
        .unwrap()
        .user;
    let mut all = store
        .collections(user.id, CollectionKind::Calendar)
        .await
        .unwrap();
    all.extend(
        store
            .collections(user.id, CollectionKind::Addressbook)
            .await
            .unwrap(),
    );
    all
}

async fn collection(store: &Store, kind: CollectionKind, slug: &str) -> Collection {
    collections(store)
        .await
        .into_iter()
        .find(|c| c.kind == kind && c.slug.as_str() == slug)
        .unwrap_or_else(|| panic!("no {} {slug}", kind.as_str()))
}

async fn content(store: &Store, collection: &Collection, uid: &str) -> Vec<u8> {
    let name = store
        .item_name_by_uid(collection.id, uid)
        .await
        .unwrap()
        .unwrap_or_else(|| panic!("{uid} missing"));
    store
        .item(collection.id, &name)
        .await
        .unwrap()
        .unwrap()
        .content
}

fn text(summary: &Summary) -> String {
    let mut out = Vec::new();
    summary.write(&mut out).unwrap();
    String::from_utf8(out).unwrap()
}

fn counts(summary: &Summary, slug: &str) -> (u64, u64, u64, u64) {
    let c = summary
        .collections
        .iter()
        .find(|c| c.target.as_str() == slug)
        .unwrap_or_else(|| panic!("no report for {slug}"));
    let r = &c.report;
    (r.created, r.replaced, r.skipped, r.rejected)
}

fn assert_no_password(text: &str) {
    assert!(!text.contains(SOURCE_PASSWORD), "password leaked: {text}");
    let basic = STANDARD.encode(format!("alice-src:{SOURCE_PASSWORD}"));
    assert!(!text.contains(&basic), "credentials leaked: {text}");
}

/// The full migration over TLS with a custom CA: discovery through
/// `.well-known`, collections with their properties, byte-identical
/// objects, a rejected object, skipped inbox and shared calendar, and an
/// idempotent second run.
#[tokio::test]
async fn migrates_over_tls_with_custom_ca() {
    let server = start(Fake::new("/dav", true)).await;
    let tls = tls_proxy(server.plain);
    let store = local_store().await;
    let before = collections(&store).await.len();
    let dir = TempDir::new();

    // Without the CA the self-signed certificate is not trusted.
    let untrusted = migrate::run(
        &store,
        &options(format!("https://127.0.0.1:{}/", tls.port())),
    )
    .await
    .unwrap_err();
    assert!(
        format!("{untrusted:#}").contains("certificate"),
        "{untrusted:#}"
    );
    assert_eq!(collections(&store).await.len(), before);

    let mut opts = options(format!("https://127.0.0.1:{}/", tls.port()));
    opts.ca_file = Some(dir.ca_file());
    let summary = migrate::run(&store, &opts).await.unwrap();
    let report = text(&summary);
    assert!(!summary.failed(), "{report}");
    assert_eq!(counts(&summary, "work"), (2, 0, 0, 1), "{report}");
    assert_eq!(counts(&summary, "tasks-list"), (1, 0, 0, 0), "{report}");
    assert_eq!(counts(&summary, "contacts"), (2, 0, 0, 0), "{report}");
    assert_eq!(summary.collections.len(), 3, "{report}");
    assert!(report.contains("rejected bad.ics: "), "{report}");
    assert!(
        report.contains("/dav/calendars/alice-src/inbox/: scheduling inbox"),
        "{report}"
    );
    assert!(
        report.contains("/dav/calendars/alice-src/bobs/: shared by another user"),
        "{report}"
    );
    // Reports carry no object contents.
    assert!(!report.contains("BEGIN:"), "{report}");

    let work = collection(&store, CollectionKind::Calendar, "work").await;
    assert_eq!(work.display_name.as_deref(), Some("Work"));
    assert_eq!(work.color.as_deref(), Some("#FF8800FF"));
    assert_eq!(work.description.as_deref(), Some("Job & meetings"));
    assert_eq!(work.components, "VEVENT");
    let tasks = collection(&store, CollectionKind::Calendar, "tasks-list").await;
    assert_eq!(tasks.display_name.as_deref(), Some("Tasks"));
    assert_eq!(tasks.color.as_deref(), Some("#00AA00"));
    assert_eq!(tasks.components, "VTODO");
    let contacts = collection(&store, CollectionKind::Addressbook, "contacts").await;
    assert_eq!(contacts.description.as_deref(), Some("Friends"));
    assert_eq!(collections(&store).await.len(), before + 3);

    // Byte-identical, although multiget delivered the data inside XML.
    assert_eq!(
        content(&store, &work, "one@example.com").await,
        EVENT_ONE.as_bytes()
    );
    assert_eq!(
        content(&store, &work, "two@example.com").await,
        EVENT_TWO.as_bytes()
    );
    assert_eq!(
        content(&store, &tasks, "todo@example.com").await,
        TODO.as_bytes()
    );
    assert_eq!(
        content(&store, &contacts, "card-paula").await,
        photo_card().as_bytes()
    );
    assert_eq!(
        content(&store, &contacts, "card-max").await,
        CARD.as_bytes()
    );
    assert!(
        store
            .item_name_by_uid(work.id, "bad@example.com")
            .await
            .unwrap()
            .is_none()
    );
    let requests = server.fake.requests();
    assert!(
        requests.iter().any(|r| r.starts_with("REPORT ")),
        "{requests:?}"
    );
    assert!(
        !requests.iter().any(|r| r.starts_with("GET ")),
        "{requests:?}"
    );
    assert!(
        !requests.iter().any(|r| r.contains("/bobs/shared.ics")),
        "{requests:?}"
    );

    // Second run: nothing new, nothing duplicated.
    let again = migrate::run(&store, &opts).await.unwrap();
    assert_eq!(counts(&again, "work"), (0, 0, 2, 1), "{}", text(&again));
    assert_eq!(counts(&again, "tasks-list"), (0, 0, 1, 0));
    assert_eq!(counts(&again, "contacts"), (0, 0, 2, 0));
    assert_eq!(collections(&store).await.len(), before + 3);
    assert_eq!(store.items(work.id).await.unwrap().len(), 2);

    // Replace overwrites in place.
    opts.on_conflict = OnConflict::Replace;
    let replaced = migrate::run(&store, &opts).await.unwrap();
    assert_eq!(counts(&replaced, "contacts"), (0, 2, 0, 0));
    assert_eq!(store.items(contacts.id).await.unwrap().len(), 2);

    // Shared calendars on request.
    opts.include_shared = true;
    opts.on_conflict = OnConflict::Skip;
    let shared = migrate::run(&store, &opts).await.unwrap();
    assert_eq!(counts(&shared, "bobs"), (1, 0, 0, 0), "{}", text(&shared));

    for summary in [&summary, &again, &replaced, &shared] {
        assert_no_password(&text(summary));
    }
    assert_no_password(&format!("{untrusted:#}"));
    assert_no_password(&String::from_utf8_lossy(&logs().0.lock().unwrap()));
}

/// Synology-like quirks: a path prefix, no multiget (GET per object), and
/// an http port that redirects to https on another port, which is refused
/// (another port is another server); plus `--only` and `--map`.
#[tokio::test]
async fn synology_quirks_prefix_get_fallback_and_port_redirect() {
    let server = start(Fake::new("/webdav/caldav", false)).await;
    let tls = tls_proxy(server.plain);
    let http = https_redirector(tls.port()).await;
    let store = local_store().await;
    let dir = TempDir::new();

    // The http URL is refused without --allow-http …
    let from = format!("http://127.0.0.1:{}/webdav/caldav/", http.port());
    let refused = migrate::run(&store, &options(from.clone()))
        .await
        .unwrap_err();
    assert!(
        format!("{refused:#}").contains("--allow-http"),
        "{refused:#}"
    );
    // … and its redirect to https on another port is not followed.
    let mut opts = options(from);
    opts.allow_http = true;
    opts.ca_file = Some(dir.ca_file());
    let err = migrate::run(&store, &opts).await.unwrap_err();
    let message = format!("{err:#}");
    assert!(message.contains("refused redirect"), "{message}");
    assert!(server.fake.requests().is_empty());

    let mut opts = options(format!("https://127.0.0.1:{}/webdav/caldav/", tls.port()));
    opts.ca_file = Some(dir.ca_file());
    opts.only = Some(CollectionKind::Calendar);
    opts.map = vec!["work=job".to_owned(), "nothing=else".to_owned()];
    let summary = migrate::run(&store, &opts).await.unwrap();
    let report = text(&summary);
    assert!(!summary.failed(), "{report}");
    assert_eq!(counts(&summary, "job"), (2, 0, 0, 1), "{report}");
    assert_eq!(counts(&summary, "tasks-list"), (1, 0, 0, 0), "{report}");
    assert_eq!(summary.collections.len(), 2, "{report}");
    assert!(report.contains("excluded by --only"), "{report}");
    assert!(
        report.contains("--map nothing: no such source collection"),
        "{report}"
    );

    let job = collection(&store, CollectionKind::Calendar, "job").await;
    assert_eq!(
        content(&store, &job, "one@example.com").await,
        EVENT_ONE.as_bytes()
    );
    let requests = server.fake.requests();
    assert!(
        requests.iter().any(|r| r.starts_with("GET ")),
        "{requests:?}"
    );
    // One failed multiget per collection, then GETs only.
    assert_eq!(
        requests.iter().filter(|r| r.starts_with("REPORT ")).count(),
        2,
        "{requests:?}"
    );
    assert_no_password(&report);
}

/// Objects above the size limit are rejected (GET is cut off at the limit),
/// and a response above `max_response` fails its collection.
#[tokio::test]
async fn size_limits_are_enforced() {
    let server = start(Fake::new("/dav", false)).await;
    let store = local_store().await;
    let mut opts = options(format!("http://127.0.0.1:{}/dav/", server.plain.port()));
    opts.allow_http = true;
    opts.only = Some(CollectionKind::Addressbook);
    opts.max_vcard_bytes = 4096;
    let summary = migrate::run(&store, &opts).await.unwrap();
    let report = text(&summary);
    assert_eq!(counts(&summary, "contacts"), (1, 0, 0, 1), "{report}");
    assert!(report.contains("paula.vcf: object too large"), "{report}");

    let server = start(Fake::new("/dav", true)).await;
    let store = local_store().await;
    let mut opts = options(format!("http://127.0.0.1:{}/dav/", server.plain.port()));
    opts.allow_http = true;
    opts.max_response = 6000;
    let summary = migrate::run(&store, &opts).await.unwrap();
    let report = text(&summary);
    assert!(summary.failed(), "{report}");
    let contacts = summary
        .collections
        .iter()
        .find(|c| c.target.as_str() == "contacts")
        .unwrap();
    let error = contacts.error.as_deref().unwrap_or_default();
    assert!(error.contains("too large"), "{report}");
    assert_eq!(counts(&summary, "work"), (2, 0, 0, 1), "{report}");
}

/// Once the quota is reached, the rest of a collection is not fetched.
#[tokio::test]
async fn quota_stops_fetching() {
    let server = start(Fake::new("/dav", true)).await;
    let store = local_store().await;
    store.set_quota_for_tests(i64::try_from(TODO.len() + EVENT_ONE.len() + 10).unwrap());
    let mut opts = options(format!("http://127.0.0.1:{}/dav/", server.plain.port()));
    opts.allow_http = true;
    opts.only = Some(CollectionKind::Calendar);
    opts.batch_size = 1;
    let summary = migrate::run(&store, &opts).await.unwrap();
    let report = text(&summary);
    assert!(summary.failed(), "{report}");
    assert_eq!(counts(&summary, "work"), (1, 0, 1, 1), "{report}");
    assert!(
        report.contains("storage quota exceeded: the remaining 1 objects were not fetched"),
        "{report}"
    );
    let requests = server.fake.requests();
    let reports = requests
        .iter()
        .filter(|r| r.starts_with("REPORT ") && r.ends_with("/work/"))
        .count();
    assert_eq!(reports, 2, "{requests:?}");
}

/// Redirect loops end after a few hops.
#[tokio::test]
async fn redirect_loops_end() {
    let server = start(Fake::new("/dav", true)).await;
    *server.fake.well_known.lock().unwrap() = "/loop/well-known".to_owned();
    let store = local_store().await;
    let mut opts = options(format!("http://127.0.0.1:{}/loop/", server.plain.port()));
    opts.allow_http = true;
    let err = migrate::run(&store, &opts).await.unwrap_err();
    assert!(format!("{err:#}").contains("too many redirects"), "{err:#}");
    let loops = server
        .fake
        .requests()
        .iter()
        .filter(|r| r.contains(" /loop/"))
        .count();
    // Each attempt: the request plus five redirects.
    assert!((6..=24).contains(&loops), "{loops}");
}

/// Repeated and unrequested hrefs in a multiget answer are ignored: the
/// first answer per requested href counts, nothing else is imported.
#[tokio::test]
async fn multiget_noise_is_ignored() {
    let server = start(Fake::new("/dav", true)).await;
    server.fake.noisy.store(true, Ordering::Relaxed);
    let store = local_store().await;
    let mut opts = options(format!("http://127.0.0.1:{}/dav/", server.plain.port()));
    opts.allow_http = true;
    opts.only = Some(CollectionKind::Calendar);
    let summary = migrate::run(&store, &opts).await.unwrap();
    let report = text(&summary);
    assert_eq!(counts(&summary, "work"), (2, 0, 0, 1), "{report}");
    let work = collection(&store, CollectionKind::Calendar, "work").await;
    assert_eq!(store.items(work.id).await.unwrap().len(), 2);
    assert_eq!(
        content(&store, &work, "one@example.com").await,
        EVENT_ONE.as_bytes()
    );
    assert!(
        store
            .item_name_by_uid(work.id, "shared@example.com")
            .await
            .unwrap()
            .is_none()
    );
}

/// A dry run discovers, fetches and validates, but writes nothing.
#[tokio::test]
async fn dry_run_writes_nothing() {
    let server = start(Fake::new("/dav", true)).await;
    let store = local_store().await;
    let before = collections(&store).await;
    let mut opts = options(format!("http://127.0.0.1:{}/dav/", server.plain.port()));
    opts.allow_http = true;
    opts.dry_run = true;
    let summary = migrate::run(&store, &opts).await.unwrap();
    let report = text(&summary);
    assert_eq!(counts(&summary, "work"), (2, 0, 0, 1), "{report}");
    assert_eq!(counts(&summary, "contacts"), (2, 0, 0, 0), "{report}");
    assert!(report.contains("would be created"), "{report}");
    assert!(report.contains("dry run: nothing was written"), "{report}");
    let after = collections(&store).await;
    assert_eq!(after.len(), before.len());
    for c in &after {
        assert!(store.items(c.id).await.unwrap().is_empty());
    }
}

/// A subscription calendar with the target's name is left alone: its
/// content comes from the feed. The other collections are still copied.
#[tokio::test]
async fn subscription_calendars_are_not_migration_targets() {
    let server = start(Fake::new("/dav", true)).await;
    let store = local_store().await;
    let owner_id = store
        .user_credentials(&LOCAL_USER.parse().unwrap())
        .await
        .unwrap()
        .unwrap()
        .user
        .id;
    let slug = "work".parse().unwrap();
    store
        .create_subscription(
            &calcraft_store::NewSubscription {
                owner_id,
                slug: &slug,
                display_name: None,
                color: None,
                components: "VEVENT",
                source_url: "https://feeds.example.org/work.ics",
                refresh_secs: None,
                max_per_user: 20,
            },
            &[],
            &calcraft_store::FeedState {
                feed_ttl_secs: None,
                http_etag: None,
                http_last_modified: None,
                content_hash: "empty",
                skipped: 0,
                now: 0,
                next_refresh_at: 0,
            },
        )
        .await
        .unwrap();
    let mut opts = options(format!("http://127.0.0.1:{}/dav/", server.plain.port()));
    opts.allow_http = true;
    let summary = migrate::run(&store, &opts).await.unwrap();
    let report = text(&summary);
    assert!(
        report.contains("read-only subscription calendar"),
        "{report}"
    );
    let work = collection(&store, CollectionKind::Calendar, "work").await;
    assert!(work.read_only);
    assert!(store.items(work.id).await.unwrap().is_empty());
    let contacts = collection(&store, CollectionKind::Addressbook, "contacts").await;
    assert_eq!(store.items(contacts.id).await.unwrap().len(), 2);
}

/// Wrong credentials stop the migration; redirects to another origin are
/// not followed (the password would go there).
#[tokio::test]
async fn wrong_password_and_foreign_redirects_are_refused() {
    let server = start(Fake::new("/dav", true)).await;
    let store = local_store().await;
    let port = server.plain.port();

    let mut wrong = options(format!("http://127.0.0.1:{port}/dav/"));
    wrong.allow_http = true;
    wrong.password = Password::new("not the password");
    let err = migrate::run(&store, &wrong).await.unwrap_err();
    assert!(
        format!("{err:#}").contains("rejected the credentials"),
        "{err:#}"
    );

    // `.well-known` points to another host name (another origin).
    *server.fake.well_known.lock().unwrap() = format!("http://localhost:{port}/dav/");
    let mut opts = options(format!("http://127.0.0.1:{port}/"));
    opts.allow_http = true;
    let err = migrate::run(&store, &opts).await.unwrap_err();
    assert_no_password(&format!("{err:#}"));
    let requests = server.fake.requests();
    assert!(
        !requests.iter().any(|r| r.contains(" localhost:")),
        "{requests:?}"
    );

    // Credentials in the URL are refused outright.
    let mut inline = options(format!("http://alice-src:pw@127.0.0.1:{port}/dav/"));
    inline.allow_http = true;
    let err = migrate::run(&store, &inline).await.unwrap_err();
    assert!(format!("{err:#}").contains("not accepted"), "{err:#}");
}
