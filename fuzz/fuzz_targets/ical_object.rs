//! Calendar and address objects as uploaded via PUT: parsing and
//! validation must never panic, whatever the input.
#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = calcraft_ical::parse(data);
    let _ = calcraft_ical::VCard::parse(data);
    if let Ok(object) = calcraft_ical::CalendarObject::parse(data) {
        let _ = object.range.overlaps(Some(0), Some(i64::MAX));
    }
});
