use super::*;
use crate::PropName;

#[test]
fn parses_multiget() {
    let body = br#"<c:calendar-multiget xmlns:d="DAV:" xmlns:c="urn:ietf:params:xml:ns:caldav">
        <d:prop><d:getetag/><c:calendar-data/></d:prop>
        <d:href>/dav/calendars/alice/default/a.ics</d:href>
        <d:href> /dav/calendars/alice/default/b.ics </d:href>
    </c:calendar-multiget>"#;
    let Report::CalendarMultiget { props, hrefs } = Report::parse(body).unwrap() else {
        panic!("expected multiget");
    };
    assert_eq!(
        props,
        Propfind::Prop(vec![
            PropName::new(NS_DAV, "getetag"),
            PropName::new(NS_CALDAV, "calendar-data")
        ])
    );
    assert_eq!(hrefs.len(), 2);
    assert_eq!(hrefs[1], "/dav/calendars/alice/default/b.ics");
}

#[test]
fn parses_sync_collection() {
    let body = br#"<d:sync-collection xmlns:d="DAV:"><d:sync-token>urn:calcraft:sync:1:5</d:sync-token>
        <d:sync-level>1</d:sync-level><d:limit><d:nresults>10</d:nresults></d:limit><d:prop><d:getetag/></d:prop></d:sync-collection>"#;
    let Report::SyncCollection {
        token,
        limit,
        props,
    } = Report::parse(body).unwrap()
    else {
        panic!("expected sync-collection");
    };
    assert_eq!(token, "urn:calcraft:sync:1:5");
    assert_eq!(limit, Some(10));
    assert_eq!(
        props,
        Propfind::Prop(vec![PropName::new(NS_DAV, "getetag")])
    );

    let initial = br#"<d:sync-collection xmlns:d="DAV:"><d:sync-token/><d:sync-level>1</d:sync-level><d:prop/></d:sync-collection>"#;
    assert!(
        matches!(Report::parse(initial), Ok(Report::SyncCollection { token, .. }) if token.is_empty())
    );
    let infinite = br#"<d:sync-collection xmlns:d="DAV:"><d:sync-token/><d:sync-level>infinite</d:sync-level></d:sync-collection>"#;
    assert!(matches!(
        Report::parse(infinite),
        Err(DavError::Unsupported(_))
    ));
    let no_level = br#"<d:sync-collection xmlns:d="DAV:"><d:sync-token/></d:sync-collection>"#;
    assert!(Report::parse(no_level).is_err());
}

#[test]
fn parses_addressbook_reports() {
    let body = br#"<card:addressbook-multiget xmlns:d="DAV:" xmlns:card="urn:ietf:params:xml:ns:carddav">
        <d:prop><d:getetag/><card:address-data/></d:prop><d:href>/dav/addressbooks/alice/default/a.vcf</d:href></card:addressbook-multiget>"#;
    assert!(
        matches!(Report::parse(body), Ok(Report::AddressbookMultiget { hrefs, .. }) if hrefs.len() == 1)
    );
    let query = br#"<card:addressbook-query xmlns:d="DAV:" xmlns:card="urn:ietf:params:xml:ns:carddav"><d:prop><d:getetag/></d:prop><card:filter/></card:addressbook-query>"#;
    assert!(matches!(
        Report::parse(query),
        Ok(Report::AddressbookQuery { .. })
    ));
}

#[test]
fn parses_time_range_query() {
    let body = br#"<c:calendar-query xmlns:d="DAV:" xmlns:c="urn:ietf:params:xml:ns:caldav">
        <d:prop><d:getetag/></d:prop>
        <c:filter><c:comp-filter name="VCALENDAR"><c:comp-filter name="VEVENT">
            <c:time-range start="20261001T000000Z" end="20261101T000000Z"/>
        </c:comp-filter></c:comp-filter></c:filter>
    </c:calendar-query>"#;
    let Report::CalendarQuery { filter, .. } = Report::parse(body).unwrap() else {
        panic!("expected query");
    };
    assert_eq!(filter.component, Some(ComponentKind::Event));
    assert!(filter.start.unwrap() < filter.end.unwrap());
}

#[test]
fn query_without_component_filter_matches_all() {
    let body = br#"<c:calendar-query xmlns:c="urn:ietf:params:xml:ns:caldav">
        <c:filter><c:comp-filter name="VCALENDAR"/></c:filter></c:calendar-query>"#;
    let Report::CalendarQuery { filter, props } = Report::parse(body).unwrap() else {
        panic!("expected query");
    };
    assert_eq!(filter.component, None);
    assert_eq!(props, Propfind::AllProp);
}

#[test]
fn unsupported_filters_are_reported() {
    let prop_filter = br#"<c:calendar-query xmlns:c="urn:ietf:params:xml:ns:caldav">
        <c:filter><c:comp-filter name="VCALENDAR"><c:comp-filter name="VEVENT">
        <c:prop-filter name="SUMMARY"><c:text-match>x</c:text-match></c:prop-filter>
        </c:comp-filter></c:comp-filter></c:filter></c:calendar-query>"#;
    assert!(matches!(
        Report::parse(prop_filter),
        Err(DavError::Unsupported(_))
    ));
    let other = br#"<d:expand-property xmlns:d="DAV:"/>"#;
    assert!(matches!(
        Report::parse(other),
        Err(DavError::Unsupported(_))
    ));
    let filtered = br#"<card:addressbook-query xmlns:card="urn:ietf:params:xml:ns:carddav"><card:filter><card:prop-filter name="FN"/></card:filter></card:addressbook-query>"#;
    assert!(matches!(
        Report::parse(filtered),
        Err(DavError::Unsupported(_))
    ));
}

#[test]
fn rejects_invalid_queries() {
    for body in [
        &br#"<c:calendar-query xmlns:c="urn:ietf:params:xml:ns:caldav"/>"#[..],
        br#"<c:calendar-query xmlns:c="urn:ietf:params:xml:ns:caldav"><c:filter><c:comp-filter name="VEVENT"/></c:filter></c:calendar-query>"#,
        br#"<c:calendar-query xmlns:c="urn:ietf:params:xml:ns:caldav"><c:filter><c:comp-filter name="VCALENDAR"><c:comp-filter name="VEVENT"><c:time-range/></c:comp-filter></c:comp-filter></c:filter></c:calendar-query>"#,
        br#"<c:calendar-query xmlns:c="urn:ietf:params:xml:ns:caldav"><c:filter><c:comp-filter name="VCALENDAR"><c:comp-filter name="VEVENT"><c:time-range start="20261101T000000Z" end="20261001T000000Z"/></c:comp-filter></c:comp-filter></c:filter></c:calendar-query>"#,
        br#"<c:calendar-query xmlns:c="urn:ietf:params:xml:ns:caldav"><c:filter><c:comp-filter name="VCALENDAR"><c:comp-filter name="VEVENT"><c:time-range start="20261001"/></c:comp-filter></c:comp-filter></c:filter></c:calendar-query>"#,
    ] {
        assert!(Report::parse(body).is_err(), "{}", String::from_utf8_lossy(body));
    }
    let many = format!(
        r#"<c:calendar-multiget xmlns:d="DAV:" xmlns:c="urn:ietf:params:xml:ns:caldav">{}</c:calendar-multiget>"#,
        "<d:href>/x</d:href>".repeat(MAX_HREFS + 1)
    );
    assert!(Report::parse(many.as_bytes()).is_err());
}
