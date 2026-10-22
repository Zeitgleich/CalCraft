//! PROPFIND request bodies (RFC 4918 §9.1).

use crate::{
    DavError,
    tree::{self, Node},
    xml::NS_DAV,
};

/// Largest accepted PROPFIND body. Real clients send a few kilobytes.
pub const MAX_PROPFIND_BODY: usize = 64 * 1024;

const MAX_PROPS: usize = 256;

/// A property name: namespace URI plus local name.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PropName {
    pub namespace: String,
    pub name: String,
}

impl PropName {
    pub fn new(namespace: &str, name: &str) -> Self {
        Self {
            namespace: namespace.to_owned(),
            name: name.to_owned(),
        }
    }

    pub fn is(&self, namespace: &str, name: &str) -> bool {
        self.namespace == namespace && self.name == name
    }
}

/// What a PROPFIND asks for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Propfind {
    AllProp,
    PropName,
    Prop(Vec<PropName>),
}

impl Propfind {
    /// Parses a PROPFIND body. An empty body means `allprop`.
    pub fn parse(body: &[u8]) -> Result<Self, DavError> {
        if body.len() > MAX_PROPFIND_BODY {
            return Err(DavError::TooLarge);
        }
        if body.trim_ascii().is_empty() {
            return Ok(Self::AllProp);
        }
        let root = tree::parse(body, MAX_PROPFIND_BODY)?;
        if !root.is(NS_DAV, "propfind") {
            return Err(DavError::BadRequest("root element must be DAV:propfind"));
        }
        Self::from_parent(&root)?.ok_or(DavError::BadRequest(
            "propfind without allprop, propname or prop",
        ))
    }

    /// Reads `allprop`, `propname` or `prop` from the children of `parent`.
    /// Shared by PROPFIND and REPORT bodies.
    pub(crate) fn from_parent(parent: &Node) -> Result<Option<Self>, DavError> {
        let mut found = parent.children.iter().filter_map(|child| {
            if child.is(NS_DAV, "allprop") {
                Some(Ok(Self::AllProp))
            } else if child.is(NS_DAV, "propname") {
                Some(Ok(Self::PropName))
            } else if child.is(NS_DAV, "prop") {
                Some(prop_names(child).map(Self::Prop))
            } else {
                None
            }
        });
        let first = found.next().transpose()?;
        if found.next().is_some() {
            return Err(DavError::BadRequest(
                "more than one of allprop, propname and prop",
            ));
        }
        Ok(first)
    }
}

/// The property names inside a `DAV:prop` element.
pub(crate) fn prop_names(prop: &Node) -> Result<Vec<PropName>, DavError> {
    if prop.children.len() > MAX_PROPS {
        return Err(DavError::BadRequest("too many properties"));
    }
    Ok(prop
        .children
        .iter()
        .map(|child| PropName::new(&child.namespace, &child.name))
        .collect())
}

#[cfg(test)]
mod tests;
