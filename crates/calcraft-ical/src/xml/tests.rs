use super::*;

const LIMITS: XmlLimits = XmlLimits {
    max_len: 1 << 20,
    max_depth: 24,
    max_nodes: 20_000,
};

#[test]
fn builds_tree_with_text_and_attributes() {
    let xml = br#"<?xml version="1.0"?><d:a xmlns:d="DAV:"><d:b x="1 &amp; 2">A &amp; B &#x41;<![CDATA[<c>]]></d:b></d:a>"#;
    let root = parse(xml, &LIMITS).unwrap();
    assert!(root.is("DAV:", "a"));
    let b = root.child("DAV:", "b").unwrap();
    assert_eq!(b.text, "A & B A<c>");
    assert_eq!(b.attribute("x"), Some("1 & 2"));
}

#[test]
fn rejects_entities_and_doctype() {
    let laughs = br#"<!DOCTYPE l [<!ENTITY a "aa">]><d:a xmlns:d="DAV:">&a;</d:a>"#;
    assert!(parse(laughs, &LIMITS).is_err());
    let external = br#"<?xml version="1.0"?><!DOCTYPE x SYSTEM "file:///etc/passwd"><x/>"#;
    assert!(parse(external, &LIMITS).is_err());
    assert!(parse(br#"<d:a xmlns:d="DAV:">&custom;</d:a>"#, &LIMITS).is_err());
}

#[test]
fn enforces_limits() {
    let deep = format!("{}{}", "<a>".repeat(25), "</a>".repeat(25));
    assert!(parse(deep.as_bytes(), &LIMITS).is_err());
    let wide = format!("<a>{}</a>", "<b/>".repeat(20_000));
    assert!(parse(wide.as_bytes(), &LIMITS).is_err());
    let tiny = XmlLimits {
        max_len: 2,
        ..LIMITS
    };
    assert_eq!(parse(b"<a/>", &tiny), Err(XmlError::TooLarge));
}
