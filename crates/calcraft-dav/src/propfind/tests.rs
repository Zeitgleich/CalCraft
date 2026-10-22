use super::*;
use crate::xml::NS_CALDAV;

#[test]
fn empty_body_is_allprop() {
    assert_eq!(Propfind::parse(b""), Ok(Propfind::AllProp));
    assert_eq!(Propfind::parse(b"  \n"), Ok(Propfind::AllProp));
}

#[test]
fn parses_prop_with_namespaces() {
    let body = br#"<?xml version="1.0" encoding="utf-8"?>
        <d:propfind xmlns:d="DAV:" xmlns:c="urn:ietf:params:xml:ns:caldav">
          <d:prop>
            <d:current-user-principal/>
            <c:calendar-home-set/>
            <x:unknown xmlns:x="http://example.com/ns"/>
          </d:prop>
        </d:propfind>"#;
    let Propfind::Prop(props) = Propfind::parse(body).unwrap() else {
        panic!("expected prop");
    };
    assert_eq!(
        props,
        vec![
            PropName::new(NS_DAV, "current-user-principal"),
            PropName::new(NS_CALDAV, "calendar-home-set"),
            PropName::new("http://example.com/ns", "unknown"),
        ]
    );
}

#[test]
fn default_namespace_works() {
    let body = br#"<propfind xmlns="DAV:"><prop><displayname/></prop></propfind>"#;
    assert_eq!(
        Propfind::parse(body),
        Ok(Propfind::Prop(vec![PropName::new(NS_DAV, "displayname")]))
    );
}

#[test]
fn parses_allprop_and_propname() {
    assert_eq!(
        Propfind::parse(br#"<d:propfind xmlns:d="DAV:"><d:allprop/></d:propfind>"#),
        Ok(Propfind::AllProp)
    );
    assert_eq!(
        Propfind::parse(br#"<d:propfind xmlns:d="DAV:"><d:propname/></d:propfind>"#),
        Ok(Propfind::PropName)
    );
}

#[test]
fn rejects_wrong_root() {
    assert!(Propfind::parse(br#"<propfind><prop/></propfind>"#).is_err());
    assert!(Propfind::parse(br#"<d:other xmlns:d="DAV:"/>"#).is_err());
}

#[test]
fn rejects_doctype_and_entities() {
    let billion_laughs = br#"<?xml version="1.0"?>
        <!DOCTYPE lolz [<!ENTITY lol "lol"><!ENTITY lol2 "&lol;&lol;">]>
        <d:propfind xmlns:d="DAV:"><d:prop><d:displayname>&lol2;</d:displayname></d:prop></d:propfind>"#;
    assert!(Propfind::parse(billion_laughs).is_err());

    let xxe = br#"<?xml version="1.0"?>
        <!DOCTYPE d [<!ENTITY xxe SYSTEM "file:///etc/passwd">]>
        <d:propfind xmlns:d="DAV:"><d:prop>&xxe;</d:prop></d:propfind>"#;
    assert!(Propfind::parse(xxe).is_err());
}

#[test]
fn rejects_deep_nesting() {
    let mut body = String::from(r#"<d:propfind xmlns:d="DAV:"><d:prop>"#);
    body.push_str(&"<a>".repeat(100));
    body.push_str(&"</a>".repeat(100));
    body.push_str("</d:prop></d:propfind>");
    assert!(Propfind::parse(body.as_bytes()).is_err());
}

#[test]
fn rejects_too_many_props_and_large_bodies() {
    let mut body = String::from(r#"<d:propfind xmlns:d="DAV:"><d:prop>"#);
    body.push_str(&"<d:x/>".repeat(MAX_PROPS + 1));
    body.push_str("</d:prop></d:propfind>");
    assert!(Propfind::parse(body.as_bytes()).is_err());

    assert_eq!(
        Propfind::parse(&vec![b' '; MAX_PROPFIND_BODY + 1]),
        Err(DavError::TooLarge)
    );
}

#[test]
fn rejects_malformed_xml() {
    for body in [
        &b"<d:propfind xmlns:d=\"DAV:\"><d:prop>"[..],
        b"<d:propfind xmlns:d=\"DAV:\"></d:prop>",
        b"<x:propfind/>",
        b"\xff\xfe",
    ] {
        assert!(Propfind::parse(body).is_err(), "{body:?}");
    }
}
