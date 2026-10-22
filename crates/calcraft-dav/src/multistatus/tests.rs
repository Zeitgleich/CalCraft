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
    failed.failed.push((
        PropName::new(NS_DAV, "owner"),
        403,
        Some("cannot-modify-protected-property"),
    ));
    failed
        .failed
        .push((PropName::new(NS_DAV, "displayname"), 424, None));
    let xml = Multistatus {
        responses: vec![Response::with_status("/missing.ics", 404), failed],
        sync_token: Some("urn:t:1".into()),
    }
    .to_xml();
    assert!(xml.contains(
        "<d:href>/missing.ics</d:href><d:status>HTTP/1.1 404 Not Found</d:status></d:response>"
    ));
    assert!(xml.contains(
        "<d:owner/></d:prop><d:status>HTTP/1.1 403 Forbidden</d:status>\
         <d:error><d:cannot-modify-protected-property/></d:error></d:propstat>"
    ));
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

#[test]
fn dead_property_values_keep_their_namespaces() {
    let fragment =
        XmlFragment::from_stored(r#"<a xmlns="urn:x" k="v"><b xmlns="">t</b></a>"#).unwrap();
    let mut response = Response::new("/a/");
    response
        .found
        .push((PropName::new("urn:p", "prop"), PropValue::Xml(fragment)));
    response.found.push((
        PropName::new("", "plain"),
        PropValue::Xml(XmlFragment::from_stored("1").unwrap()),
    ));
    let xml = Multistatus {
        responses: vec![response],
        sync_token: None,
    }
    .to_xml();
    assert!(xml.contains(
        r#"<x:prop xmlns:x="urn:p"><a xmlns="urn:x" k="v"><b xmlns="">t</b></a></x:prop>"#
    ));
    assert!(xml.contains("<plain>1</plain>"));
    // The namespaces resolve as stored.
    let root = crate::tree::parse(xml.as_bytes(), xml.len()).unwrap();
    let prop = &root.children[0].children[1].children[0];
    let value = &prop.children[0].children[0];
    assert!(value.is("urn:x", "a"));
    assert!(value.children[0].is("", "b"));
    assert!(prop.children[1].is("", "plain"));
}
