use super::*;
use crate::xml::NS_APPLE_ICAL;

#[test]
fn parses_proppatch_in_order() {
    let body = br#"<d:propertyupdate xmlns:d="DAV:" xmlns:ic="http://apple.com/ns/ical/">
        <d:set><d:prop><d:displayname>Work &amp; Life</d:displayname><ic:calendar-color>#FF0000FF</ic:calendar-color></d:prop></d:set>
        <d:remove><d:prop><d:displayname/></d:prop></d:remove>
    </d:propertyupdate>"#;
    let update = PropertyUpdate::parse_proppatch(body).unwrap();
    assert_eq!(update.operations.len(), 3);
    assert_eq!(update.operations[0].1.as_ref().unwrap().text, "Work & Life");
    assert!(update.operations[1].0.is(NS_APPLE_ICAL, "calendar-color"));
    assert!(update.operations[2].1.is_none());
}

#[test]
fn parses_mkcalendar() {
    assert_eq!(
        PropertyUpdate::parse_mkcalendar(b""),
        Ok(PropertyUpdate::default())
    );
    let body = br#"<c:mkcalendar xmlns:d="DAV:" xmlns:c="urn:ietf:params:xml:ns:caldav">
        <d:set><d:prop><d:displayname>Team</d:displayname></d:prop></d:set></c:mkcalendar>"#;
    let update = PropertyUpdate::parse_mkcalendar(body).unwrap();
    assert!(update.operations[0].0.is(NS_DAV, "displayname"));
}

#[test]
fn rejects_invalid_bodies() {
    assert!(PropertyUpdate::parse_proppatch(br#"<d:propfind xmlns:d="DAV:"/>"#).is_err());
    assert!(PropertyUpdate::parse_proppatch(br#"<d:propertyupdate xmlns:d="DAV:"/>"#).is_err());
    assert!(
        PropertyUpdate::parse_proppatch(
            br#"<d:propertyupdate xmlns:d="DAV:"><d:other/></d:propertyupdate>"#
        )
        .is_err()
    );
    let many = format!(
        r#"<d:propertyupdate xmlns:d="DAV:"><d:set><d:prop>{}</d:prop></d:set></d:propertyupdate>"#,
        "<d:x/>".repeat(MAX_OPERATIONS + 1)
    );
    assert!(PropertyUpdate::parse_proppatch(many.as_bytes()).is_err());
    let remove = br#"<c:mkcalendar xmlns:d="DAV:" xmlns:c="urn:ietf:params:xml:ns:caldav"><d:remove><d:prop><d:displayname/></d:prop></d:remove></c:mkcalendar>"#;
    assert!(PropertyUpdate::parse_mkcalendar(remove).is_err());
}
