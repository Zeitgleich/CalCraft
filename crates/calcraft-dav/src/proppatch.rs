//! PROPPATCH (RFC 4918 §9.2), MKCALENDAR (RFC 4791 §5.3.1) and extended
//! MKCOL (RFC 5689) bodies.

use crate::{
    DavError, PropName,
    tree::{self, Node},
    xml::{NS_CALDAV, NS_DAV},
};

/// Room for one `calendar-timezone` of up to 64 KiB plus markup.
pub const MAX_PROPPATCH_BODY: usize = 128 * 1024;
const MAX_OPERATIONS: usize = 64;

/// Property changes in document order. `None` removes the property.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PropertyUpdate {
    pub operations: Vec<(PropName, Option<Node>)>,
}

impl PropertyUpdate {
    /// Parses a `DAV:propertyupdate` body.
    pub fn parse_proppatch(body: &[u8]) -> Result<Self, DavError> {
        let root = tree::parse(body, MAX_PROPPATCH_BODY)?;
        if !root.is(NS_DAV, "propertyupdate") {
            return Err(DavError::BadRequest(
                "root element must be DAV:propertyupdate",
            ));
        }
        Self::from_instructions(&root)
    }

    /// Parses an optional `CALDAV:mkcalendar` body.
    pub fn parse_mkcalendar(body: &[u8]) -> Result<Self, DavError> {
        if body.trim_ascii().is_empty() {
            return Ok(Self::default());
        }
        let root = tree::parse(body, MAX_PROPPATCH_BODY)?;
        if !root.is(NS_CALDAV, "mkcalendar") {
            return Err(DavError::BadRequest(
                "root element must be CALDAV:mkcalendar",
            ));
        }
        let update = Self::from_instructions(&root)?;
        if update.operations.iter().any(|(_, value)| value.is_none()) {
            return Err(DavError::BadRequest("mkcalendar cannot remove properties"));
        }
        Ok(update)
    }

    /// Parses an optional extended `DAV:mkcol` body.
    pub fn parse_mkcol(body: &[u8]) -> Result<Self, DavError> {
        if body.trim_ascii().is_empty() {
            return Ok(Self::default());
        }
        let root = tree::parse(body, MAX_PROPPATCH_BODY)?;
        if !root.is(NS_DAV, "mkcol") {
            return Err(DavError::BadRequest("root element must be DAV:mkcol"));
        }
        let update = Self::from_instructions(&root)?;
        if update.operations.iter().any(|(_, value)| value.is_none()) {
            return Err(DavError::BadRequest("mkcol cannot remove properties"));
        }
        Ok(update)
    }

    fn from_instructions(root: &Node) -> Result<Self, DavError> {
        let mut operations = Vec::new();
        for instruction in &root.children {
            let set = if instruction.is(NS_DAV, "set") {
                true
            } else if instruction.is(NS_DAV, "remove") {
                false
            } else {
                return Err(DavError::BadRequest("expected DAV:set or DAV:remove"));
            };
            for prop in instruction.children_named(NS_DAV, "prop") {
                for property in &prop.children {
                    if operations.len() >= MAX_OPERATIONS {
                        return Err(DavError::BadRequest("too many property changes"));
                    }
                    let name = PropName::new(&property.namespace, &property.name);
                    operations.push((name, set.then(|| property.clone())));
                }
            }
        }
        if operations.is_empty() {
            return Err(DavError::BadRequest("no property changes"));
        }
        Ok(Self { operations })
    }
}

#[cfg(test)]
mod tests;
