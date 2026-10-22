use super::*;
use crate::PropertyUpdate;

/// The value of the first property set in a PROPPATCH body.
fn value_of(prop: &str) -> Result<XmlFragment, ValueError> {
    let body = format!(
        r#"<d:propertyupdate xmlns:d="DAV:" xmlns:x="urn:x" xmlns:c="urn:ietf:params:xml:ns:caldav"><d:set><d:prop>{prop}</d:prop></d:set></d:propertyupdate>"#
    );
    let update = PropertyUpdate::parse_proppatch(body.as_bytes()).unwrap();
    XmlFragment::from_property(update.operations[0].1.as_ref().unwrap())
}

#[test]
fn text_values_are_kept_exactly() {
    assert_eq!(value_of("<x:order>12</x:order>").unwrap().as_str(), "12");
    assert_eq!(value_of("<x:order/>").unwrap().as_str(), "");
    assert_eq!(
        value_of("<x:t> a &lt;b&gt; &amp; \"c\"\r\n</x:t>")
            .unwrap()
            .as_str(),
        " a &lt;b&gt; &amp; &quot;c&quot;\n"
    );
    assert_eq!(value_of("<x:t>a&#13;b</x:t>").unwrap().as_str(), "a&#13;b");
}

#[test]
fn fragments_declare_their_namespaces() {
    let value = value_of(
        r#"<x:p>
            <x:a k="1 &amp; 2" other:skip="no" xmlns:other="urn:o">text</x:a>
            <o:b xmlns:o="urn:o"><x:c/><n xmlns="">plain</n></o:b>
        </x:p>"#,
    )
    .unwrap();
    assert_eq!(
        value.as_str(),
        r#"<a xmlns="urn:x" k="1 &amp; 2">text</a><b xmlns="urn:o"><c xmlns="urn:x"/><n xmlns="">plain</n></b>"#
    );
    // Stored values are parsed and serialised again unchanged.
    assert_eq!(XmlFragment::from_stored(value.as_str()), Some(value));
}

#[test]
fn stored_values_are_validated() {
    assert_eq!(
        XmlFragment::from_stored("5").map(XmlFragment::into_string),
        Some("5".to_owned())
    );
    for bad in [
        "<a>",
        "</v><evil/><v>",
        "<a xmlns:p=\"u\"/><p:b/>",
        "&bogus;",
    ] {
        assert_eq!(XmlFragment::from_stored(bad), None, "{bad}");
    }
    // A comment is dropped, not passed through.
    assert_eq!(
        XmlFragment::from_stored("a<!-- x -->b").map(XmlFragment::into_string),
        Some("ab".to_owned())
    );
    assert_eq!(
        XmlFragment::from_stored(&"a".repeat(MAX_TIMEZONE_BYTES + 1)),
        None
    );
}

#[test]
fn rejects_invalid_values() {
    assert_eq!(
        value_of("<x:t>a&#1;b</x:t>"),
        Err(ValueError::InvalidCharacter)
    );
    let big = format!("<x:t>{}</x:t>", "&amp;".repeat(MAX_VALUE_BYTES / 4));
    assert_eq!(value_of(&big), Err(ValueError::TooLarge));
    // Time zones may be larger.
    let tz = "t".repeat(MAX_VALUE_BYTES * 2);
    let value = value_of(&format!("<c:calendar-timezone>{tz}</c:calendar-timezone>")).unwrap();
    assert_eq!(value.as_str(), tz);
    assert_eq!(XmlFragment::from_stored(&tz), Some(value));
    let tz = "t".repeat(MAX_TIMEZONE_BYTES + 1);
    assert_eq!(
        value_of(&format!("<c:calendar-timezone>{tz}</c:calendar-timezone>")),
        Err(ValueError::TooLarge)
    );
}

#[test]
fn checks_names() {
    let ok = |ns: &str, name: &str| check_name(&PropName::new(ns, name));
    assert_eq!(ok("", "plain"), Ok(()));
    assert_eq!(ok("http://apple.com/ns/ical/", "calendar-order"), Ok(()));
    assert_eq!(ok("urn:x", "a"), Ok(()));
    assert_eq!(ok("relative", "a"), Err(NameError::NotAbsolute));
    assert_eq!(ok("1http:x", "a"), Err(NameError::NotAbsolute));
    assert_eq!(ok("urn:a b", "a"), Err(NameError::NotAbsolute));
    assert_eq!(
        ok("urn:x", &"a".repeat(MAX_LOCAL_NAME_LEN + 1)),
        Err(NameError::TooLong)
    );
    let long_ns = format!("urn:{}", "a".repeat(MAX_NAMESPACE_LEN));
    assert_eq!(ok(&long_ns, "a"), Err(NameError::TooLong));
}
