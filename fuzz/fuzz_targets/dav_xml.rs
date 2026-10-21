//! All XML request bodies go through the same tree parser.
#![no_main]

use calcraft_dav::{PropertyUpdate, Propfind, Report};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = Propfind::parse(data);
    let _ = Report::parse(data);
    let _ = PropertyUpdate::parse_proppatch(data);
    let _ = PropertyUpdate::parse_mkcalendar(data);
});
