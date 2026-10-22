use super::*;
use crate::xml::NS_DAV;

#[test]
fn builds_tree_with_text_and_attributes() {
    let xml = br#"<?xml version="1.0"?><d:a xmlns:d="DAV:"><d:b x="1 &amp; 2">A &amp; B &#x41;<![CDATA[<c>]]></d:b></d:a>"#;
    let root = parse(xml, 1024).unwrap();
    assert!(root.is(NS_DAV, "a"));
    let b = root.child(NS_DAV, "b").unwrap();
    assert_eq!(b.text, "A & B A<c>");
    assert_eq!(b.attribute("x"), Some("1 & 2"));
}

#[test]
fn rejects_entities_and_doctype() {
    let laughs = br#"<!DOCTYPE l [<!ENTITY a "aa">]><d:a xmlns:d="DAV:">&a;</d:a>"#;
    assert!(parse(laughs, 1024).is_err());
    assert!(parse(br#"<d:a xmlns:d="DAV:">&custom;</d:a>"#, 1024).is_err());
}

#[test]
fn enforces_limits() {
    let deep = format!(
        "{}{}",
        "<a>".repeat(MAX_DEPTH + 1),
        "</a>".repeat(MAX_DEPTH + 1)
    );
    assert!(parse(deep.as_bytes(), 1 << 20).is_err());
    let wide = format!("<a>{}</a>", "<b/>".repeat(MAX_NODES));
    assert!(parse(wide.as_bytes(), 1 << 20).is_err());
    assert_eq!(parse(b"<a/>", 2), Err(DavError::TooLarge));
    let many_attrs = format!(
        "<a {}/>",
        (0..20)
            .map(|i| format!("a{i}=\"1\""))
            .collect::<Vec<_>>()
            .join(" ")
    );
    assert!(parse(many_attrs.as_bytes(), 1024).is_err());
}

#[test]
fn rejects_invalid_names() {
    for xml in [&b"<a&b/>"[..], b"<1a/>", b"<-a/>", b"<a x&y=\"1\"/>"] {
        assert!(
            parse(xml, 1024).is_err(),
            "{}",
            String::from_utf8_lossy(xml)
        );
    }
    assert!(parse("<\u{00e4}rger/>".as_bytes(), 1024).is_ok());
}

#[test]
fn rejects_malformed_documents() {
    for xml in [
        &b""[..],
        b"<a>",
        b"<a></b>",
        b"<a/><b/>",
        b"<x:a/>",
        b"\xff",
    ] {
        assert!(parse(xml, 1024).is_err(), "{xml:?}");
    }
}
