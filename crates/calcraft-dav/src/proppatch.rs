//! PROPPATCH (RFC 4918 §9.2) and MKCALENDAR (RFC 4791 §5.3.1) bodies.

use crate::{
    DavError, PropName,
    tree::{self, Node},
    xml::{NS_CALDAV, NS_DAV},
};

pub const MAX_PROPPATCH_BODY: usize = 64 * 1024;
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
mod tests {
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
}
