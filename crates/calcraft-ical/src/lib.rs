//! iCalendar (RFC 5545) and vCard (RFC 6350, RFC 2426) handling for CalCraft.
//!
//! The parser enforces hard resource limits and never panics on input. It
//! is the only iCalendar parser in the project: validation, indexing and
//! (later) recurrence expansion all work on its output, so they can never
//! disagree about what an object contains.
//!
//! The alternative formats (jCal, jCard, xCal, CSV) are converted to and
//! from the same component tree; imports therefore end up as iCalendar or
//! vCard text that is validated like any PUT ([`transfer`]).
//!
//! This crate is free of I/O so it can be compiled for `wasm32`.

pub mod csv;
mod datetime;
mod jcal;
mod object;
mod parser;
pub mod transfer;
mod values;
mod vcard;
pub mod write;
mod xcal;
pub mod xml;

pub use datetime::{DateTime, parse_date_or_datetime, parse_duration, parse_utc_datetime};
pub use object::{CalendarObject, ComponentKind, TimeRange};
pub use parser::{Component, Parameter, Property, parse};
pub use transfer::{
    Exporter, Format, ImportContext, ImportedObject, MAX_IMPORT_OBJECTS, MAX_IMPORT_SIZE, Target,
};
pub use vcard::VCard;
pub use xcal::NS_XCAL;

/// Largest accepted calendar object in bytes (`CALDAV:max-resource-size`).
pub const MAX_OBJECT_SIZE: usize = 1024 * 1024;

/// Hard upper bound for one vCard in bytes. vCards carry embedded photos,
/// so they may be larger than calendar objects; the server's configurable
/// limit (`CARDDAV:max-resource-size`) can only be lower.
pub const MAX_VCARD_SIZE: usize = 10 * 1024 * 1024;

/// Errors for malformed or unacceptable iCalendar data.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum IcalError {
    #[error("object too large")]
    TooLarge,
    #[error("malformed iCalendar data: {0}")]
    Malformed(&'static str),
    #[error("limit exceeded: {0}")]
    Limit(&'static str),
    #[error("invalid calendar object: {0}")]
    Invalid(&'static str),
}
