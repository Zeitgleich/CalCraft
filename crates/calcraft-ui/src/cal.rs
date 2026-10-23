//! Plain logic of the calendar views (UI-2 … UI-4), independent of the
//! browser so that it is tested natively:
//!
//! - [`date`]: civil dates (days since 1970-01-01), month grids, parsing
//!   and formatting;
//! - [`zone`]: local time of a time zone given as a UTC-offset function
//!   (the browser's zone in the app, fixed rules in tests), DST-safe
//!   conversions, and the VTIMEZONE definition of a zone;
//! - [`layout`]: columns of overlapping events and lanes of all-day events;
//! - [`nav`]: views, periods and the keyboard model;
//! - [`moves`]: instances in local time, drag and drop and keyboard moves;
//! - [`form`]: the event dialog's state and its conversion to the API.

pub mod date;
pub mod form;
pub mod layout;
pub mod moves;
pub mod nav;
pub mod zone;

#[cfg(test)]
mod tests;
