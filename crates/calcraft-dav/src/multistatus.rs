//! `207 Multi-Status` response bodies (RFC 4918 §13).

use std::fmt::Write as _;

use crate::{
    PropName,
    xml::{escape_text, prefix_for, root_namespaces},
};

/// Value of a property in a response.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PropValue {
    /// Element without content.
    Empty,
    /// Escaped text content.
    Text(String),
    /// One `DAV:href` child.
    Href(String),
    /// Child elements in known namespaces, e.g. `resourcetype` or
    /// `supported-calendar-component-set`.
    Elements(Vec<Element>),
}

/// An empty child element with optional attributes, e.g. `<c:comp name="VEVENT"/>`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Element {
    pub namespace: &'static str,
    pub name: &'static str,
    pub attributes: Vec<(&'static str, String)>,
    pub children: Vec<Element>,
}

impl Element {
    pub fn new(namespace: &'static str, name: &'static str) -> Self {
        Self {
            namespace,
            name,
            attributes: Vec::new(),
            children: Vec::new(),
        }
    }

    pub fn attr(mut self, name: &'static str, value: impl Into<String>) -> Self {
        self.attributes.push((name, value.into()));
        self
    }

    pub fn child(mut self, child: Element) -> Self {
        self.children.push(child);
        self
    }
}

/// One `DAV:response` element.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Response {
    pub href: String,
    /// A status for the whole resource (e.g. 404 in a multiget); when set,
    /// no properties are written.
    pub status: Option<u16>,
    pub found: Vec<(PropName, PropValue)>,
    pub not_found: Vec<PropName>,
    /// Properties that could not be changed, with their status code.
    pub failed: Vec<(PropName, u16)>,
}

impl Response {
    pub fn new(href: impl Into<String>) -> Self {
        Self {
            href: href.into(),
            ..Self::default()
        }
    }

    pub fn with_status(href: impl Into<String>, status: u16) -> Self {
        Self {
            href: href.into(),
            status: Some(status),
            ..Self::default()
        }
    }
}

fn status_line(code: u16) -> String {
    let reason = match code {
        200 => "OK",
        403 => "Forbidden",
        404 => "Not Found",
        409 => "Conflict",
        424 => "Failed Dependency",
        507 => "Insufficient Storage",
        _ => "Error",
    };
    format!("HTTP/1.1 {code} {reason}")
}

/// A complete multistatus document.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Multistatus {
    pub responses: Vec<Response>,
    /// New sync token for sync-collection responses (RFC 6578 §6.2).
    pub sync_token: Option<String>,
}

impl Multistatus {
    pub fn to_xml(&self) -> String {
        let mut out = String::from(r#"<?xml version="1.0" encoding="utf-8"?>"#);
        let _ = write!(out, "<d:multistatus{}>", root_namespaces());
        for response in &self.responses {
            out.push_str("<d:response>");
            let _ = write!(out, "<d:href>{}</d:href>", escape_text(&response.href));
            if let Some(status) = response.status {
                let _ = write!(out, "<d:status>{}</d:status>", status_line(status));
                out.push_str("</d:response>");
                continue;
            }
            if !response.found.is_empty() {
                out.push_str("<d:propstat><d:prop>");
                for (name, value) in &response.found {
                    write_prop(&mut out, name, value);
                }
                out.push_str("</d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat>");
            }
            if !response.not_found.is_empty() {
                out.push_str("<d:propstat><d:prop>");
                for name in &response.not_found {
                    write_prop(&mut out, name, &PropValue::Empty);
                }
                out.push_str("</d:prop><d:status>HTTP/1.1 404 Not Found</d:status></d:propstat>");
            }
            let mut codes: Vec<u16> = response.failed.iter().map(|(_, code)| *code).collect();
            codes.sort_unstable();
            codes.dedup();
            for code in codes {
                out.push_str("<d:propstat><d:prop>");
                for (name, _) in response.failed.iter().filter(|(_, c)| *c == code) {
                    write_prop(&mut out, name, &PropValue::Empty);
                }
                let _ = write!(
                    out,
                    "</d:prop><d:status>{}</d:status></d:propstat>",
                    status_line(code)
                );
            }
            out.push_str("</d:response>");
        }
        if let Some(token) = &self.sync_token {
            let _ = write!(out, "<d:sync-token>{}</d:sync-token>", escape_text(token));
        }
        out.push_str("</d:multistatus>");
        out
    }
}

/// Writes a property element. Names in unknown namespaces get a local
/// `xmlns` declaration; names come from parsed XML and are well-formed.
fn write_prop(out: &mut String, name: &PropName, value: &PropValue) {
    let (open, close) = match prefix_for(&name.namespace) {
        Some(prefix) => (
            format!("{prefix}:{}", name.name),
            format!("{prefix}:{}", name.name),
        ),
        None if name.namespace.is_empty() => (name.name.clone(), name.name.clone()),
        None => (
            format!(
                "x:{} xmlns:x=\"{}\"",
                name.name,
                escape_text(&name.namespace)
            ),
            format!("x:{}", name.name),
        ),
    };
    match value {
        PropValue::Empty => {
            let _ = write!(out, "<{open}/>");
        }
        PropValue::Text(text) => {
            let _ = write!(out, "<{open}>{}</{close}>", escape_text(text));
        }
        PropValue::Href(href) => {
            let _ = write!(
                out,
                "<{open}><d:href>{}</d:href></{close}>",
                escape_text(href)
            );
        }
        PropValue::Elements(elements) => {
            let _ = write!(out, "<{open}>");
            for element in elements {
                write_element(out, element);
            }
            let _ = write!(out, "</{close}>");
        }
    }
}

fn write_element(out: &mut String, element: &Element) {
    let prefix = prefix_for(element.namespace).unwrap_or("d");
    let _ = write!(out, "<{prefix}:{}", element.name);
    for (name, value) in &element.attributes {
        let _ = write!(out, " {name}=\"{}\"", escape_text(value));
    }
    if element.children.is_empty() {
        out.push_str("/>");
    } else {
        out.push('>');
        for child in &element.children {
            write_element(out, child);
        }
        let _ = write!(out, "</{prefix}:{}>", element.name);
    }
}

#[cfg(test)]
mod tests {
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
        failed.failed.push((PropName::new(NS_DAV, "owner"), 403));
        failed
            .failed
            .push((PropName::new(NS_DAV, "displayname"), 424));
        let xml = Multistatus {
            responses: vec![Response::with_status("/missing.ics", 404), failed],
            sync_token: Some("urn:t:1".into()),
        }
        .to_xml();
        assert!(xml.contains(
            "<d:href>/missing.ics</d:href><d:status>HTTP/1.1 404 Not Found</d:status></d:response>"
        ));
        assert!(xml.contains("<d:owner/></d:prop><d:status>HTTP/1.1 403 Forbidden</d:status>"));
        assert!(xml.contains(
            "<d:displayname/></d:prop><d:status>HTTP/1.1 424 Failed Dependency</d:status>"
        ));
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
}
