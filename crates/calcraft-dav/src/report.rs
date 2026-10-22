//! REPORT bodies: CalDAV (RFC 4791 §7.8, §7.9), CardDAV (RFC 6352 §8.6,
//! §8.7) and sync-collection (RFC 6578 §3.2).

use calcraft_ical::{ComponentKind, parse_utc_datetime};

use crate::{
    DavError, Propfind,
    tree::{self, Node},
    xml::{NS_CALDAV, NS_CARDDAV, NS_DAV},
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
    AddressbookMultiget {
        props: Propfind,
        hrefs: Vec<String>,
    },
    /// Only an empty filter (all address objects) is supported.
    AddressbookQuery {
        props: Propfind,
    },
    SyncCollection {
        /// Empty for the initial synchronisation.
        token: String,
        props: Propfind,
        /// Client-requested maximum number of results.
        limit: Option<usize>,
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
            Ok(Self::CalendarMultiget {
                props,
                hrefs: hrefs(&root)?,
            })
        } else if root.is(NS_CARDDAV, "addressbook-multiget") {
            Ok(Self::AddressbookMultiget {
                props,
                hrefs: hrefs(&root)?,
            })
        } else if root.is(NS_CARDDAV, "addressbook-query") {
            if root
                .child(NS_CARDDAV, "filter")
                .is_some_and(|filter| !filter.children.is_empty())
            {
                return Err(DavError::Unsupported("address book filter"));
            }
            Ok(Self::AddressbookQuery { props })
        } else if root.is(NS_DAV, "sync-collection") {
            parse_sync(&root, props)
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

fn hrefs(root: &Node) -> Result<Vec<String>, DavError> {
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
    Ok(hrefs)
}

fn parse_sync(root: &Node, props: Propfind) -> Result<Report, DavError> {
    let token = root
        .child(NS_DAV, "sync-token")
        .map(|t| t.text.trim().to_owned())
        .unwrap_or_default();
    if token.len() > MAX_HREF_LEN {
        return Err(DavError::BadRequest("sync-token too long"));
    }
    match root.child(NS_DAV, "sync-level").map(|l| l.text.trim()) {
        Some("1") => {}
        Some("infinite") => return Err(DavError::Unsupported("sync-level infinite")),
        _ => return Err(DavError::BadRequest("sync-level is required")),
    }
    let limit = root
        .child(NS_DAV, "limit")
        .and_then(|l| l.child(NS_DAV, "nresults"))
        .map(|n| {
            n.text
                .trim()
                .parse::<usize>()
                .ok()
                .filter(|n| *n > 0)
                .ok_or(DavError::BadRequest("invalid nresults"))
        })
        .transpose()?;
    Ok(Report::SyncCollection {
        token,
        props,
        limit,
    })
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
mod tests;
