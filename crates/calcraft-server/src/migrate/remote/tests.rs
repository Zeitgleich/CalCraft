use super::*;

fn uri(text: &str) -> Uri {
    text.parse().unwrap()
}

fn origin(text: &str) -> Origin {
    Origin::of(&uri(text)).unwrap()
}

fn remote(start: &str) -> Remote {
    let http = HttpClient::new(None, true).unwrap();
    let limits = Limits {
        timeout: Duration::from_secs(1),
        max_body: 1024,
    };
    Remote::new(
        http,
        &uri(start),
        "alice",
        &Password::new("pw"),
        limits,
        Duration::ZERO,
    )
    .unwrap()
}

#[test]
fn origins_compare_scheme_host_and_port() {
    assert_eq!(
        origin("https://NAS.example.com/"),
        origin("https://nas.example.com:443/x")
    );
    assert_ne!(
        origin("https://nas.example.com/"),
        origin("https://nas.example.com:5001/")
    );
    assert_ne!(
        origin("http://nas.example.com/"),
        origin("https://nas.example.com/")
    );
    // Upgrades: same host and port, or the default ports 80 to 443.
    assert!(origin("http://nas:5000/").is_upgrade_to(&origin("https://nas:5000/")));
    assert!(origin("http://nas/").is_upgrade_to(&origin("https://nas/")));
    assert!(!origin("http://nas:5000/").is_upgrade_to(&origin("https://nas:5001/")));
    assert!(!origin("http://nas:5000/").is_upgrade_to(&origin("https://nas/")));
    assert!(!origin("http://nas/").is_upgrade_to(&origin("https://nas:8443/")));
    assert!(!origin("http://nas/").is_upgrade_to(&origin("https://evil/")));
    assert!(!origin("https://nas/").is_upgrade_to(&origin("http://nas/")));
    assert!(Origin::of(&uri("https://u:p@nas/")).is_err());
    assert!(Origin::of(&uri("/relative")).is_err());
}

#[test]
fn hrefs_resolve_within_the_origin() {
    let remote = remote("https://nas:5001/caldav/");
    let base = uri("https://nas:5001/caldav/alice/");
    let resolve = |href| remote.resolve(&base, href).map(|u| u.to_string());
    assert_eq!(
        resolve("/caldav/alice/work/").as_deref(),
        Some("https://nas:5001/caldav/alice/work/")
    );
    assert_eq!(
        resolve("work/").as_deref(),
        Some("https://nas:5001/caldav/alice/work/")
    );
    assert_eq!(
        resolve("https://NAS:5001/caldav/alice/x.ics").as_deref(),
        Some("https://nas:5001/caldav/alice/x.ics")
    );
    assert_eq!(
        resolve("/caldav/My Calendar/").as_deref(),
        Some("https://nas:5001/caldav/My%20Calendar/")
    );
    // Other origins and path tricks are ignored.
    assert_eq!(resolve("https://evil.example.com/caldav/"), None);
    assert_eq!(resolve("http://nas:5001/caldav/"), None);
    assert_eq!(resolve("/caldav/../etc/"), None);
    assert_eq!(resolve("/caldav/%2e%2e/etc/"), None);
    assert_eq!(resolve(""), None);
}

#[test]
fn redirects_follow_the_origin_rules() {
    let mut remote = remote("http://nas:5000/caldav/");
    let current = uri("http://nas:5000/caldav/");
    assert_eq!(
        remote
            .follow(&current, "/caldav/alice/")
            .unwrap()
            .to_string(),
        "http://nas:5000/caldav/alice/"
    );
    assert!(matches!(
        remote.follow(&current, "https://evil.example.com/"),
        Err(RemoteError::Redirect(_))
    ));
    // Another port is another server: refused, without query in the message.
    let err = remote
        .follow(&current, "https://nas:5001/caldav/?token=abc#x")
        .unwrap_err()
        .to_string();
    assert!(err.contains("https://nas:5001"), "{err}");
    assert!(!err.contains("token"), "{err}");
    assert!(!remote.upgraded);
    assert_eq!(
        remote
            .follow(&current, "https://nas:5000/caldav/")
            .unwrap()
            .to_string(),
        "https://nas:5000/caldav/"
    );
    assert!(remote.upgraded);
    // Afterwards, URLs of the old http origin go to https.
    assert_eq!(
        remote
            .rebase(&uri("http://nas:5000/.well-known/caldav"))
            .unwrap()
            .to_string(),
        "https://nas:5000/.well-known/caldav"
    );
    // No way back to http.
    assert!(remote.follow(&current, "http://nas:5000/x").is_err());
}

#[test]
fn multistatus_is_parsed() {
    let body = br#"<?xml version="1.0"?>
<d:multistatus xmlns:d="DAV:" xmlns:c="urn:ietf:params:xml:ns:caldav">
 <d:response>
  <d:href> /cal/a.ics </d:href>
  <d:propstat><d:prop><d:getetag>"1"</d:getetag><c:calendar-data>BEGIN:VCALENDAR
END:VCALENDAR
</c:calendar-data></d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat>
  <d:propstat><d:prop><d:displayname/></d:prop><d:status>HTTP/1.1 404 Not Found</d:status></d:propstat>
 </d:response>
 <d:response><d:href>/cal/gone.ics</d:href><d:status>HTTP/1.1 404 Not Found</d:status></d:response>
</d:multistatus>"#;
    let entries = multistatus(body, 1 << 20).unwrap();
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0].href, "/cal/a.ics");
    assert_eq!(entries[0].text(NS_DAV, "getetag"), Some("\"1\""));
    assert!(entries[0].prop(NS_DAV, "displayname").is_none());
    assert!(entries[0].prop(NS_CALDAV, "calendar-data").is_some());
    assert_eq!(entries[1].status, Some(404));

    assert!(multistatus(body, 100).is_err());
    assert!(multistatus(b"<d:prop xmlns:d=\"DAV:\"/>", 1 << 20).is_err());
    let doctype = b"<!DOCTYPE x [<!ENTITY a \"b\">]><d:multistatus xmlns:d=\"DAV:\"/>";
    assert!(multistatus(doctype, 1 << 20).is_err());
}

#[test]
fn percent_decoding() {
    assert_eq!(percent_decode("a%40b%20c"), "a@b c");
    assert_eq!(percent_decode("100%"), "100%");
    assert_eq!(percent_decode("%zz"), "%zz");
    assert_eq!(percent_decode("%ff"), "%ff");
    assert_eq!(path_key(&uri("https://h/a%20b/")), "/a b");
}

#[test]
fn credentials_are_marked_sensitive() {
    let remote = remote("https://nas/");
    assert_eq!(remote.authorization.as_str(), "Basic YWxpY2U6cHc=");
    let header = remote.authorization_header().unwrap();
    assert!(header.is_sensitive());
    assert!(!format!("{header:?}").contains("YWxpY2U6cHc"));
    assert_eq!(format!("{:?}", Password::new("pw")), "<redacted>");
}

#[test]
fn urls_in_messages_are_redacted() {
    assert_eq!(
        redact_url(" https://u:p@nas:5001/caldav/?token=1#frag "),
        "https://nas:5001/caldav/"
    );
    assert_eq!(redact_url("/a/b?x=1"), "/a/b");
    assert_eq!(redact_url("https://nas/\u{1b}x"), "https://nas/\u{fffd}x");
}
