//! CalDAV REPORT bodies (RFC 4791 §7.8, §7.9).

use calcraft_ical::{ComponentKind, parse_utc_datetime};

use crate::{
    DavError, Propfind,
    tree::{self, Node},
    xml::{NS_CALDAV, NS_DAV},
};

/// Largest accepted REPORT body; a multiget for 2000 objects fits easily.
pub const MAX_REPORT_BODY: usize = 512 * 1024;
const MAX_HREFS: usize = 2_000;
const MAX_HREF_LEN: usize = 1_024;

/// Supported subset of `CALDAV:filter`: component type plus an optional
/// time range. Anything else is reported as unsupported instead of being
/// silently ignored.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CalendarFilter {
    /// `None` matches every component type.
    pub component: Option<ComponentKind>,
    pub start: Option<i64>,
    pub end: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Report {
    CalendarMultiget {
        props: Propfind,
        hrefs: Vec<String>,
    },
    CalendarQuery {
        props: Propfind,
        filter: CalendarFilter,
    },
}

impl Report {
    pub fn parse(body: &[u8]) -> Result<Self, DavError> {
        let root = tree::parse(body, MAX_REPORT_BODY)?;
        let props = Propfind::from_parent(&root)?.unwrap_or(Propfind::AllProp);
        if root.is(NS_CALDAV, "calendar-multiget") {
            let hrefs: Vec<String> = root
                .children_named(NS_DAV, "href")
                .map(|href| href.text.trim().to_owned())
                .collect();
            if hrefs.len() > MAX_HREFS {
                return Err(DavError::BadRequest("too many hrefs"));
            }
            if hrefs.iter().any(|h| h.is_empty() || h.len() > MAX_HREF_LEN) {
                return Err(DavError::BadRequest("invalid href"));
            }
            Ok(Self::CalendarMultiget { props, hrefs })
        } else if root.is(NS_CALDAV, "calendar-query") {
            let filter = root
                .child(NS_CALDAV, "filter")
                .ok_or(DavError::BadRequest("calendar-query without filter"))?;
            Ok(Self::CalendarQuery {
                props,
                filter: parse_filter(filter)?,
            })
        } else {
            Err(DavError::Unsupported("report"))
        }
    }
}

fn single_comp_filter(node: &Node) -> Result<Option<&Node>, DavError> {
    let mut filters = node.children_named(NS_CALDAV, "comp-filter");
    let first = filters.next();
    if filters.next().is_some() {
        return Err(DavError::Unsupported("more than one comp-filter"));
    }
    Ok(first)
}

fn parse_filter(filter: &Node) -> Result<CalendarFilter, DavError> {
    if filter.children.len() != 1 {
        return Err(DavError::BadRequest("filter needs exactly one comp-filter"));
    }
    let calendar = single_comp_filter(filter)?
        .filter(|c| c.attribute("name") == Some("VCALENDAR"))
        .ok_or(DavError::BadRequest("filter must start with VCALENDAR"))?;
    if calendar
        .children
        .iter()
        .any(|c| !c.is(NS_CALDAV, "comp-filter"))
    {
        return Err(DavError::Unsupported("filter on VCALENDAR properties"));
    }

    let Some(component) = single_comp_filter(calendar)? else {
        return Ok(CalendarFilter {
            component: None,
            start: None,
            end: None,
        });
    };
    let kind = component
        .attribute("name")
        .and_then(ComponentKind::from_name)
        .ok_or(DavError::Unsupported("component type in comp-filter"))?;

    let mut start = None;
    let mut end = None;
    let mut time_ranges = 0;
    for child in &component.children {
        if !child.is(NS_CALDAV, "time-range") {
            return Err(DavError::Unsupported("filter other than time-range"));
        }
        time_ranges += 1;
        if time_ranges > 1 {
            return Err(DavError::BadRequest("more than one time-range"));
        }
        let parse = |name: &str| {
            child
                .attribute(name)
                .map(|value| {
                    parse_utc_datetime(value)
                        .map_err(|_| DavError::BadRequest("invalid time-range value"))
                })
                .transpose()
        };
        start = parse("start")?;
        end = parse("end")?;
        if start.is_none() && end.is_none() {
            return Err(DavError::BadRequest("time-range without start or end"));
        }
        if start.zip(end).is_some_and(|(s, e)| s >= e) {
            return Err(DavError::BadRequest("time-range end before start"));
        }
    }
    Ok(CalendarFilter {
        component: Some(kind),
        start,
        end,
    })
}

#[cfg(test)]
mod tests {
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
        let other = br#"<d:sync-collection xmlns:d="DAV:"/>"#;
        assert!(matches!(
            Report::parse(other),
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
}
