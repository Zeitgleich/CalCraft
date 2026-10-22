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

#[test]
fn names_follow_xml_1_0() {
    for good in [
        "a",
        "_x",
        "calendar-order",
        "a.b-c_1",
        "Ärger",
        "日本",
        "a\u{B7}b",
        "x\u{300}",
    ] {
        assert!(is_xml_name(good), "{good}");
    }
    // `²` (U+00B2) is alphanumeric for Rust, but not an XML name character.
    for bad in [
        "",
        "1a",
        "-a",
        ".a",
        "a²",
        "a:b",
        "a b",
        "\u{B7}a",
        "a\u{2028}",
    ] {
        assert!(!is_xml_name(bad), "{bad}");
    }
    assert_eq!(
        parse("<a²/>".as_bytes(), &LIMITS),
        Err(XmlError::Invalid("invalid XML name"))
    );
    assert_eq!(
        parse("<a b²=\"1\"/>".as_bytes(), &LIMITS),
        Err(XmlError::Invalid("invalid XML name"))
    );
}
