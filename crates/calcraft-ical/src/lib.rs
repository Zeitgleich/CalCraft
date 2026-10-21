//! iCalendar (RFC 5545) handling for CalCraft.
//!
//! The parser enforces hard resource limits and never panics on input. It
//! is the only iCalendar parser in the project: validation, indexing and
//! (later) recurrence expansion all work on its output, so they can never
//! disagree about what an object contains.
//!
//! This crate is free of I/O so it can be compiled for `wasm32`.

mod datetime;
mod object;
mod parser;

pub use datetime::{DateTime, parse_date_or_datetime, parse_duration, parse_utc_datetime};
pub use object::{CalendarObject, ComponentKind, TimeRange};
pub use parser::{Component, Parameter, Property, parse};

/// Largest accepted calendar object in bytes (`CALDAV:max-resource-size`).
pub const MAX_OBJECT_SIZE: usize = 1024 * 1024;

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
