//! The single XML parser of CalCraft: builds a small element tree with hard
//! resource limits. All DAV request bodies (PROPFIND, REPORT, PROPPATCH,
//! MKCALENDAR, via `calcraft-dav`) and xCal imports go through it.
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

const MAX_NAME_LEN: usize = 256;
const MAX_ATTRIBUTES: usize = 16;

/// Resource limits for one document.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct XmlLimits {
    /// Maximum document size in bytes.
    pub max_len: usize,
    /// Maximum element nesting depth.
    pub max_depth: usize,
    /// Maximum number of elements.
    pub max_nodes: usize,
}

/// Errors for rejected XML documents.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum XmlError {
    #[error("XML document too large")]
    TooLarge,
    #[error("invalid XML: {0}")]
    Invalid(&'static str),
}

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

fn check_len(value: &str) -> Result<(), XmlError> {
    if value.len() > MAX_NAME_LEN {
        return Err(XmlError::Invalid("XML name or value too long"));
    }
    Ok(())
}

/// Element and attribute names are echoed into responses, so they must be
/// valid XML names (letters, digits, `_`, `-`, `.`; no leading digit,
/// hyphen or dot). quick-xml does not check this itself.
fn check_name(name: &str) -> Result<(), XmlError> {
    check_len(name)?;
    if !is_xml_name(name) {
        return Err(XmlError::Invalid("invalid XML name"));
    }
    Ok(())
}

/// Whether `name` is an element or attribute name this parser accepts.
pub fn is_xml_name(name: &str) -> bool {
    let mut chars = name.chars();
    let valid_start = chars.next().is_some_and(|c| c.is_alphabetic() || c == '_');
    valid_start && chars.all(|c| c.is_alphanumeric() || matches!(c, '_' | '-' | '.'))
}

fn start_node(ns: ResolveResult<'_>, start: &BytesStart<'_>) -> Result<Node, XmlError> {
    let namespace = match ns {
        ResolveResult::Bound(ns) => ns.0.to_owned(),
        ResolveResult::Unbound => String::new(),
        ResolveResult::Unknown(_) => {
            return Err(XmlError::Invalid("undeclared namespace prefix"));
        }
    };
    check_len(&namespace)?;
    let local_name = start.local_name();
    let name: &str = local_name.into_inner();
    check_name(name)?;

    let mut attributes = Vec::new();
    for attribute in start.attributes() {
        let attribute = attribute.map_err(|_| XmlError::Invalid("malformed attribute"))?;
        let key: &str = attribute.key.into_inner();
        // Namespace declarations and prefixed attributes are not needed.
        if key.starts_with("xmlns") || key.contains(':') {
            continue;
        }
        if attributes.len() >= MAX_ATTRIBUTES {
            return Err(XmlError::Invalid("too many attributes"));
        }
        check_name(key)?;
        let value = attribute
            .normalized_value(XmlVersion::Implicit1_0)
            .map_err(|_| XmlError::Invalid("malformed attribute value"))?;
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

/// Parses a document into its root element.
pub fn parse(body: &[u8], limits: &XmlLimits) -> Result<Node, XmlError> {
    if body.len() > limits.max_len {
        return Err(XmlError::TooLarge);
    }
    let text = std::str::from_utf8(body).map_err(|_| XmlError::Invalid("body is not UTF-8"))?;
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let mut reader = NsReader::from_str(text);
    reader.config_mut().expand_empty_elements = true;

    let mut stack: Vec<Node> = Vec::new();
    let mut root: Option<Node> = None;
    let mut nodes = 0usize;

    loop {
        let (ns, event) = reader
            .read_resolved_event()
            .map_err(|_| XmlError::Invalid("malformed XML"))?;
        match event {
            Event::Start(start) => {
                if root.is_some() {
                    return Err(XmlError::Invalid("more than one root element"));
                }
                if stack.len() >= limits.max_depth {
                    return Err(XmlError::Invalid("XML nested too deeply"));
                }
                nodes += 1;
                if nodes > limits.max_nodes {
                    return Err(XmlError::Invalid("too many XML elements"));
                }
                stack.push(start_node(ns, &start)?);
            }
            Event::End(_) => {
                let node = stack.pop().ok_or(XmlError::Invalid("malformed XML"))?;
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
                    .map_err(|_| XmlError::Invalid("invalid character reference"))?
                {
                    Some(c) => c.to_string(),
                    None => resolve_xml_entity(&reference.xml10_content())
                        .ok_or(XmlError::Invalid("unknown entity reference"))?
                        .to_owned(),
                };
                if let Some(node) = stack.last_mut() {
                    node.text.push_str(&resolved);
                }
            }
            Event::DocType(_) => {
                return Err(XmlError::Invalid(
                    "document type declarations are not allowed",
                ));
            }
            Event::Eof if stack.is_empty() => break,
            Event::Eof => return Err(XmlError::Invalid("unexpected end of XML")),
            Event::Comment(_) | Event::Decl(_) | Event::PI(_) | Event::Empty(_) => {}
        }
    }
    root.ok_or(XmlError::Invalid("empty XML document"))
}

/// Escapes text content and attribute values.
pub fn escape(text: &str) -> std::borrow::Cow<'_, str> {
    quick_xml::escape::escape(text)
}

#[cfg(test)]
mod tests;
