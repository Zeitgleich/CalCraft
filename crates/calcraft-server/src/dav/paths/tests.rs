use super::*;

#[test]
fn parses_targets() {
    assert!(matches!(parse_target("/dav"), Some(Target::Root)));
    assert!(matches!(parse_target("/dav/"), Some(Target::Root)));
    assert!(matches!(
        parse_target("/dav/principals/alice/"),
        Some(Target::Principal(_))
    ));
    assert!(matches!(
        parse_target("/dav/calendars/alice"),
        Some(Target::Home(CollectionKind::Calendar, _))
    ));
    assert!(matches!(
        parse_target("/dav/addressbooks/alice/default/"),
        Some(Target::Collection(CollectionKind::Addressbook, _, _))
    ));
    let Some(Target::Item(_, _, _, name)) = parse_target("/dav/calendars/alice/default/a%40b.ics")
    else {
        panic!("expected item");
    };
    assert_eq!(name, "a@b.ics");
}

#[test]
fn rejects_malformed_targets() {
    for path in [
        "/dav/principals",
        "/dav/principals/../admin/",
        "/dav/calendars/alice/../bob/",
        "/dav/calendars//default/",
        "/dav/calendars/Alice/",
        "/dav/calendars/alice/%2e%2e/",
        "/dav/other/alice/",
        "/dav/calendars/alice/default/extra/item/",
        "/dav/calendars/alice/default/..",
        "/dav/calendars/alice/default/.hidden.ics",
        "/dav/calendars/alice/default/..%2fx.ics",
        "/dav/calendars/alice/default/%252e%252e%252f",
        "/dav/calendars/alice/default/a%5cb.ics",
        "/dav/calendars/alice/default/c:%2fx.ics",
        "/dav/calendars/alice/default/a%00.ics",
        "/dav/calendars/alice/default/a%zz.ics",
        "/dav/calendars/alice/default/a%e4.ics",
        "/davx/",
    ] {
        assert!(parse_target(path).is_none(), "{path}");
    }
    let long = format!("/dav/calendars/alice/default/{}.ics", "a".repeat(300));
    assert!(parse_target(&long).is_none());
}

#[test]
fn parses_destinations() {
    let host = Some("cal.example.com");
    let path = "/dav/calendars/alice/work/x.ics";
    for value in [
        path.to_owned(),
        format!("https://cal.example.com{path}"),
        format!("http://cal.example.com{path}"),
        format!("HTTPS://Cal.Example.COM{path}"),
        format!("https://cal.example.com:443{path}"),
        format!("http://cal.example.com:80{path}"),
    ] {
        assert_eq!(destination_path(&value, host), Ok(path), "{value}");
    }
    assert_eq!(
        destination_path("http://localhost:5232/dav/", Some("localhost:5232")),
        Ok("/dav/")
    );
    assert_eq!(destination_path("https://cal.example.com", host), Ok("/"));
    // The request may carry the default port in Host.
    assert_eq!(
        destination_path("https://cal.example.com/dav/", Some("cal.example.com:443")),
        Ok("/dav/")
    );

    use DestinationError::{Malformed, OtherServer};
    for (value, error) in [
        ("", Malformed),
        ("dav/calendars/alice/work/x.ics", Malformed),
        ("//evil.example/dav/", Malformed),
        ("/dav/calendars/alice/work/x.ics?x=1", Malformed),
        ("/dav/calendars/alice/work/x.ics#f", Malformed),
        ("/dav/calendars/alice/../bob/default/x.ics", Malformed),
        ("/dav/calendars/alice/work/./x.ics", Malformed),
        ("/dav/calendars/alice/%2e%2e/bob/default/x.ics", Malformed),
        ("/dav/calendars/alice/work/%2E%2e", Malformed),
        ("/dav/calendars/alice/work/x y.ics", Malformed),
        ("https://alice@cal.example.com/dav/", Malformed),
        ("https:///dav/", Malformed),
        (
            "https://evil.example/dav/calendars/alice/work/x.ics",
            OtherServer,
        ),
        ("https://cal.example.com.evil.example/dav/", OtherServer),
        ("https://cal.example.com:8443/dav/", OtherServer),
        ("http://cal.example.com:443/dav/", OtherServer),
        ("ftp://cal.example.com/dav/", OtherServer),
        ("javascript://cal.example.com/dav/", OtherServer),
    ] {
        assert_eq!(destination_path(value, host), Err(error), "{value}");
    }
    assert_eq!(destination_path(&"/".repeat(3000), host), Err(Malformed));
    // Without a request authority, only paths can be checked.
    assert_eq!(destination_path(path, None), Ok(path));
    assert_eq!(
        destination_path(&format!("https://cal.example.com{path}"), None),
        Err(OtherServer)
    );
}

#[test]
fn extracts_item_names_from_hrefs() {
    let base = "/dav/calendars/alice/default/";
    assert_eq!(
        item_name_from_href("/dav/calendars/alice/default/x.ics", base).as_deref(),
        Some("x.ics")
    );
    assert_eq!(
        item_name_from_href(
            "https://cal.example.com/dav/calendars/alice/default/x.ics",
            base
        )
        .as_deref(),
        Some("x.ics")
    );
    for href in [
        "/dav/calendars/bob/default/x.ics",
        "/dav/calendars/alice/default/sub/x.ics",
        "/dav/calendars/alice/default/../../bob/default/x.ics",
        "/dav/calendars/alice/other/x.ics",
        "/dav/calendars/alice/default/",
    ] {
        assert!(item_name_from_href(href, base).is_none(), "{href}");
    }
}
