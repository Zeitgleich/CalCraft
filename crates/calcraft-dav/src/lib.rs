//! WebDAV (RFC 4918), CalDAV (RFC 4791) and CardDAV (RFC 6352) protocol
//! handling for CalCraft.
//!
//! This crate contains the transport-independent parts: request parsing with
//! hard resource limits and response serialisation. Routing, authentication
//! and storage access live in the server.

mod depth;
mod multistatus;
mod propfind;
mod proppatch;
mod report;
pub mod tree;
pub mod xml;

pub use depth::Depth;
pub use multistatus::{Element, Multistatus, PropValue, Response};
pub use propfind::{MAX_PROPFIND_BODY, PropName, Propfind};
pub use proppatch::{MAX_PROPPATCH_BODY, PropertyUpdate};
pub use report::{CalendarFilter, MAX_REPORT_BODY, Report};

/// Errors caused by malformed or hostile client requests.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DavError {
    #[error("bad request: {0}")]
    BadRequest(&'static str),
    #[error("request body too large")]
    TooLarge,
    /// A REPORT or filter that is valid but not implemented.
    #[error("unsupported: {0}")]
    Unsupported(&'static str),
}
