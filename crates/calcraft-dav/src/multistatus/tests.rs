use super::*;
use crate::xml::{NS_CALDAV, NS_DAV};

#[test]
fn serialises_found_and_missing_props() {
    let mut response = Response::new("/dav/calendars/alice/default/");
    response.found.push((
        PropName::new(NS_DAV, "displayname"),
        PropValue::Text("<Work & Life>".into()),
    ));
    response.found.push((
        PropName::new(NS_DAV, "resourcetype"),
        PropValue::Elements(vec![
            Element::new(NS_DAV, "collection"),
            Element::new(NS_CALDAV, "calendar"),
        ]),
    ));
    response
        .not_found
        .push(PropName::new("http://example.com/\"ns", "foo"));
    let xml = Multistatus {
        responses: vec![response],
        sync_token: None,
    }
    .to_xml();

    assert!(xml.contains("<d:displayname>&lt;Work &amp; Life&gt;</d:displayname>"));
    assert!(xml.contains("<d:resourcetype><d:collection/><c:calendar/></d:resourcetype>"));
    assert!(xml.contains(r#"<x:foo xmlns:x="http://example.com/&quot;ns"/>"#));
    assert!(xml.contains("HTTP/1.1 404 Not Found"));
}

#[test]
fn writes_status_and_failed_props() {
    let mut failed = Response::new("/a/");
    failed.failed.push((PropName::new(NS_DAV, "owner"), 403));
    failed
        .failed
        .push((PropName::new(NS_DAV, "displayname"), 424));
    let xml = Multistatus {
        responses: vec![Response::with_status("/missing.ics", 404), failed],
        sync_token: Some("urn:t:1".into()),
    }
    .to_xml();
    assert!(xml.contains(
        "<d:href>/missing.ics</d:href><d:status>HTTP/1.1 404 Not Found</d:status></d:response>"
    ));
    assert!(xml.contains("<d:owner/></d:prop><d:status>HTTP/1.1 403 Forbidden</d:status>"));
    assert!(
        xml.contains(
            "<d:displayname/></d:prop><d:status>HTTP/1.1 424 Failed Dependency</d:status>"
        )
    );
}

#[test]
fn output_is_well_formed() {
    let mut response = Response::new("/a&b/");
    response.found.push((
        PropName::new(NS_DAV, "current-user-principal"),
        PropValue::Href("/dav/principals/alice/".into()),
    ));
    let xml = Multistatus {
        responses: vec![response],
        sync_token: None,
    }
    .to_xml();
    let mut reader = quick_xml::Reader::from_str(&xml);
    while !matches!(reader.read_event().unwrap(), quick_xml::events::Event::Eof) {}
}
