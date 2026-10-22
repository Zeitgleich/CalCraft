//! Dead properties (RFC 4918 §4): client-defined properties that the server
//! stores and returns without interpreting them.
//!
//! A value is never stored as the bytes the client sent. It is the parsed
//! element tree (from the hardened parser in [`crate::tree`]) serialised
//! again by [`XmlFragment`]: every element declares its own default
//! namespace, names were validated by the parser, and text and attribute
//! values are escaped. A fragment is therefore well-formed and
//! self-contained wherever it is embedded in a response, and values read
//! back from the database are parsed and serialised the same way again
//! before they are sent.
//!
//! What the element tree does not keep is not stored either: comments,
//! processing instructions, prefixed (namespaced) attributes and the
//! position of text between child elements (mixed content keeps its text,
//! in front of the children).

use std::fmt::Write as _;

use crate::{
    PropName,
    tree::{self, Node},
};

/// Longest namespace URI of a dead property.
pub const MAX_NAMESPACE_LEN: usize = 256;
/// Longest local name of a dead property.
pub const MAX_LOCAL_NAME_LEN: usize = 128;
/// Largest serialised value of one dead property.
pub const MAX_VALUE_BYTES: usize = 16 * 1024;
/// Largest serialised value of `CALDAV:calendar-timezone`: a VTIMEZONE with
/// its full history can exceed [`MAX_VALUE_BYTES`].
pub const MAX_TIMEZONE_BYTES: usize = 64 * 1024;

/// The value limit for a property name.
pub fn max_value_bytes(name: &PropName) -> usize {
    if name.is(crate::xml::NS_CALDAV, "calendar-timezone") {
        MAX_TIMEZONE_BYTES
    } else {
        MAX_VALUE_BYTES
    }
}

/// Why a property name cannot be used for a dead property.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NameError {
    TooLong,
    /// The namespace is neither empty nor an absolute URI (RFC 4918 §21).
    NotAbsolute,
}

/// Checks the name of a dead property. The empty namespace (`xmlns=""`)
/// is allowed; any other namespace must be an absolute URI.
pub fn check_name(name: &PropName) -> Result<(), NameError> {
    if name.namespace.len() > MAX_NAMESPACE_LEN || name.name.len() > MAX_LOCAL_NAME_LEN {
        return Err(NameError::TooLong);
    }
    if name.namespace.is_empty() || is_absolute_uri(&name.namespace) {
        Ok(())
    } else {
        Err(NameError::NotAbsolute)
    }
}

/// `scheme ":" rest` (RFC 3986 §3.1) without whitespace or control characters.
fn is_absolute_uri(value: &str) -> bool {
    let Some((scheme, _)) = value.split_once(':') else {
        return false;
    };
    let mut chars = scheme.chars();
    chars.next().is_some_and(|c| c.is_ascii_alphabetic())
        && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
        && !value.chars().any(|c| c.is_whitespace() || c.is_control())
}

/// Why a value cannot be stored.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ValueError {
    /// Larger than the limit once serialised.
    TooLarge,
    /// Contains a character XML 1.0 does not allow (e.g. from `&#1;`).
    InvalidCharacter,
}

/// The content of a property element, serialised canonically.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct XmlFragment(String);

impl XmlFragment {
    /// Serialises the content (text and child elements) of `property`,
    /// the property element of a PROPPATCH or MKCALENDAR body, with the
    /// value limit for its name ([`max_value_bytes`]).
    pub fn from_property(property: &Node) -> Result<Self, ValueError> {
        let name = PropName::new(&property.namespace, &property.name);
        Self::with_limit(property, max_value_bytes(&name))
    }

    fn with_limit(property: &Node, max: usize) -> Result<Self, ValueError> {
        let mut writer = Writer {
            out: String::new(),
            max,
        };
        writer.content(property, None)?;
        Ok(Self(writer.out))
    }

    /// Re-validates a value read from storage: it is parsed and serialised
    /// again, so only output of [`XmlFragment::from_property`] survives
    /// unchanged. `None` if it is not a valid fragment.
    pub fn from_stored(stored: &str) -> Option<Self> {
        if stored.len() > MAX_TIMEZONE_BYTES {
            return None;
        }
        let wrapped = format!("<v xmlns=\"\">{stored}</v>");
        let root = tree::parse(wrapped.as_bytes(), wrapped.len()).ok()?;
        Self::with_limit(&root, MAX_TIMEZONE_BYTES).ok()
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn into_string(self) -> String {
        self.0
    }
}

fn is_xml_char(c: char) -> bool {
    !matches!(c, '\u{0}'..='\u{8}' | '\u{B}' | '\u{C}' | '\u{E}'..='\u{1F}' | '\u{FFFE}' | '\u{FFFF}')
}

/// Serialises element content up to `max` bytes.
struct Writer {
    out: String,
    max: usize,
}

impl Writer {
    fn check(&self) -> Result<(), ValueError> {
        if self.out.len() > self.max {
            Err(ValueError::TooLarge)
        } else {
            Ok(())
        }
    }

    /// Escapes text or an attribute value. Whitespace that the parser would
    /// normalise is written as character references, so values round-trip.
    fn escape(&mut self, text: &str, attribute: bool) -> Result<(), ValueError> {
        for c in text.chars() {
            match c {
                '<' => self.out.push_str("&lt;"),
                '>' => self.out.push_str("&gt;"),
                '&' => self.out.push_str("&amp;"),
                '"' => self.out.push_str("&quot;"),
                '\'' => self.out.push_str("&apos;"),
                '\r' => self.out.push_str("&#13;"),
                '\n' if attribute => self.out.push_str("&#10;"),
                '\t' if attribute => self.out.push_str("&#9;"),
                c if is_xml_char(c) => self.out.push(c),
                _ => return Err(ValueError::InvalidCharacter),
            }
            self.check()?;
        }
        Ok(())
    }

    fn content(&mut self, node: &Node, namespace: Option<&str>) -> Result<(), ValueError> {
        // Whitespace between child elements is formatting, not content.
        if node.children.is_empty() || !node.text.trim().is_empty() {
            self.escape(&node.text, false)?;
        }
        for child in &node.children {
            self.element(child, namespace)?;
        }
        Ok(())
    }

    /// Writes `node`; its namespace is declared unless the parent element
    /// already has it as default namespace.
    fn element(&mut self, node: &Node, parent: Option<&str>) -> Result<(), ValueError> {
        // Names were checked by the parser (`calcraft_ical::xml::is_xml_name`).
        self.out.push('<');
        self.out.push_str(&node.name);
        if parent != Some(node.namespace.as_str()) {
            self.out.push_str(" xmlns=\"");
            self.escape(&node.namespace, true)?;
            self.out.push('"');
        }
        for (name, value) in &node.attributes {
            let _ = write!(self.out, " {name}=\"");
            self.escape(value, true)?;
            self.out.push('"');
        }
        if node.children.is_empty() && node.text.is_empty() {
            self.out.push_str("/>");
        } else {
            self.out.push('>');
            self.content(node, Some(&node.namespace))?;
            let _ = write!(self.out, "</{}>", node.name);
        }
        self.check()
    }
}

#[cfg(test)]
mod tests;
