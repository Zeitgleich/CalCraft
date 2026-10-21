//! The single XML parser of CalCraft: builds a small element tree with hard
//! resource limits. All request bodies (PROPFIND, REPORT, PROPPATCH,
//! MKCALENDAR) go through it.
//!
//! Document type declarations are rejected outright, so there is no entity
//! expansion (XXE, billion laughs). Only the five predefined entities and
//! character references are resolved.

use quick_xml::{
    NsReader, XmlVersion,
    escape::resolve_xml_entity,
    events::{BytesStart, Event},
    name::ResolveResult,
};

use crate::DavError;

const MAX_DEPTH: usize = 24;
const MAX_NODES: usize = 20_000;
const MAX_NAME_LEN: usize = 256;
const MAX_ATTRIBUTES: usize = 16;

/// An element with its namespace URI, unqualified attributes and text.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Node {
    pub namespace: String,
    pub name: String,
    pub attributes: Vec<(String, String)>,
    pub children: Vec<Node>,
    /// Concatenated text content directly inside this element.
    pub text: String,
}

impl Node {
    pub fn is(&self, namespace: &str, name: &str) -> bool {
        self.namespace == namespace && self.name == name
    }

    pub fn child(&self, namespace: &str, name: &str) -> Option<&Node> {
        self.children.iter().find(|c| c.is(namespace, name))
    }

    pub fn children_named<'a>(
        &'a self,
        namespace: &'a str,
        name: &'a str,
    ) -> impl Iterator<Item = &'a Node> {
        self.children.iter().filter(move |c| c.is(namespace, name))
    }

    pub fn attribute(&self, name: &str) -> Option<&str> {
        self.attributes
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, v)| v.as_str())
    }
}

fn check_len(value: &str) -> Result<(), DavError> {
    if value.len() > MAX_NAME_LEN {
        return Err(DavError::BadRequest("XML name or value too long"));
    }
    Ok(())
}

/// Element and attribute names are echoed into responses, so they must be
/// valid XML names (letters, digits, `_`, `-`, `.`; no leading digit,
/// hyphen or dot). quick-xml does not check this itself.
fn check_name(name: &str) -> Result<(), DavError> {
    check_len(name)?;
    let mut chars = name.chars();
    let valid_start = chars.next().is_some_and(|c| c.is_alphabetic() || c == '_');
    if !valid_start || !chars.all(|c| c.is_alphanumeric() || matches!(c, '_' | '-' | '.')) {
        return Err(DavError::BadRequest("invalid XML name"));
    }
    Ok(())
}

fn start_node(ns: ResolveResult<'_>, start: &BytesStart<'_>) -> Result<Node, DavError> {
    let namespace = match ns {
        ResolveResult::Bound(ns) => ns.0.to_owned(),
        ResolveResult::Unbound => String::new(),
        ResolveResult::Unknown(_) => {
            return Err(DavError::BadRequest("undeclared namespace prefix"));
        }
    };
    check_len(&namespace)?;
    let local_name = start.local_name();
    let name: &str = local_name.into_inner();
    check_name(name)?;

    let mut attributes = Vec::new();
    for attribute in start.attributes() {
        let attribute = attribute.map_err(|_| DavError::BadRequest("malformed attribute"))?;
        let key: &str = attribute.key.into_inner();
        // Namespace declarations and prefixed attributes are not needed.
        if key.starts_with("xmlns") || key.contains(':') {
            continue;
        }
        if attributes.len() >= MAX_ATTRIBUTES {
            return Err(DavError::BadRequest("too many attributes"));
        }
        check_name(key)?;
        let value = attribute
            .normalized_value(XmlVersion::Implicit1_0)
            .map_err(|_| DavError::BadRequest("malformed attribute value"))?;
        check_len(&value)?;
        attributes.push((key.to_owned(), value.into_owned()));
    }
    Ok(Node {
        namespace,
        name: name.to_owned(),
        attributes,
        ..Node::default()
    })
}

/// Parses a request body into its root element.
pub fn parse(body: &[u8], max_len: usize) -> Result<Node, DavError> {
    if body.len() > max_len {
        return Err(DavError::TooLarge);
    }
    let text = std::str::from_utf8(body).map_err(|_| DavError::BadRequest("body is not UTF-8"))?;
    let mut reader = NsReader::from_str(text);
    reader.config_mut().expand_empty_elements = true;

    let mut stack: Vec<Node> = Vec::new();
    let mut root: Option<Node> = None;
    let mut nodes = 0usize;

    loop {
        let (ns, event) = reader
            .read_resolved_event()
            .map_err(|_| DavError::BadRequest("malformed XML"))?;
        match event {
            Event::Start(start) => {
                if root.is_some() {
                    return Err(DavError::BadRequest("more than one root element"));
                }
                if stack.len() >= MAX_DEPTH {
                    return Err(DavError::BadRequest("XML nested too deeply"));
                }
                nodes += 1;
                if nodes > MAX_NODES {
                    return Err(DavError::BadRequest("too many XML elements"));
                }
                stack.push(start_node(ns, &start)?);
            }
            Event::End(_) => {
                let node = stack.pop().ok_or(DavError::BadRequest("malformed XML"))?;
                match stack.last_mut() {
                    Some(parent) => parent.children.push(node),
                    None => root = Some(node),
                }
            }
            Event::Text(text) => {
                if let Some(node) = stack.last_mut() {
                    node.text.push_str(&text.xml10_content());
                }
            }
            Event::CData(data) => {
                if let Some(node) = stack.last_mut() {
                    node.text.push_str(&data);
                }
            }
            Event::GeneralRef(reference) => {
                let resolved = match reference
                    .resolve_char_ref()
                    .map_err(|_| DavError::BadRequest("invalid character reference"))?
                {
                    Some(c) => c.to_string(),
                    None => resolve_xml_entity(&reference.xml10_content())
                        .ok_or(DavError::BadRequest("unknown entity reference"))?
                        .to_owned(),
                };
                if let Some(node) = stack.last_mut() {
                    node.text.push_str(&resolved);
                }
            }
            Event::DocType(_) => {
                return Err(DavError::BadRequest(
                    "document type declarations are not allowed",
                ));
            }
            Event::Eof if stack.is_empty() => break,
            Event::Eof => return Err(DavError::BadRequest("unexpected end of XML")),
            Event::Comment(_) | Event::Decl(_) | Event::PI(_) | Event::Empty(_) => {}
        }
    }
    root.ok_or(DavError::BadRequest("empty XML document"))
}

#[cfg(test)]
mod tests {
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
}
