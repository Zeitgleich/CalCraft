//! DAV request bodies are parsed with the single XML parser of CalCraft
//! ([`calcraft_ical::xml`]), with the limits for request bodies.
//!
//! Document type declarations are rejected outright, so there is no entity
//! expansion (XXE, billion laughs). Only the five predefined entities and
//! character references are resolved.

pub use calcraft_ical::xml::Node;
use calcraft_ical::xml::{XmlError, XmlLimits};

use crate::DavError;

const MAX_DEPTH: usize = 24;
const MAX_NODES: usize = 20_000;

/// Parses a request body into its root element.
pub fn parse(body: &[u8], max_len: usize) -> Result<Node, DavError> {
    let limits = XmlLimits {
        max_len,
        max_depth: MAX_DEPTH,
        max_nodes: MAX_NODES,
    };
    calcraft_ical::xml::parse(body, &limits).map_err(|err| match err {
        XmlError::TooLarge => DavError::TooLarge,
        XmlError::Invalid(message) => DavError::BadRequest(message),
    })
}

#[cfg(test)]
mod tests;
