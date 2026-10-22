//! RRULE and VTIMEZONE evaluation: indexing and time-range matching must
//! never panic, must stay within their budgets, and the stored index must
//! never exclude a time range the exact evaluation matches.
#![no_main]

use calcraft_ical::{
    Budget, CalendarObject, OBJECT_BUDGET, exact_time_range_match, matches_time_range,
};
use libfuzzer_sys::fuzz_target;

/// The input is tried as a complete object, and split at the first two
/// newlines into an event RRULE, a VTIMEZONE observance RRULE and further
/// event properties.
fn objects(text: &str) -> [String; 2] {
    let mut parts = text.splitn(3, '\n');
    let rule = parts.next().unwrap_or_default().trim_end_matches('\r');
    let zone_rule = parts.next().unwrap_or_default().trim_end_matches('\r');
    let extra = parts.next().unwrap_or_default().replace('\n', "\r\n");
    [
        text.to_owned(),
        format!(
            "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//F//EN\r\n\
             BEGIN:VTIMEZONE\r\nTZID:Z\r\n\
             BEGIN:DAYLIGHT\r\nTZOFFSETFROM:+0100\r\nTZOFFSETTO:+0200\r\n\
             DTSTART:19700329T020000\r\nRRULE:{zone_rule}\r\nEND:DAYLIGHT\r\n\
             BEGIN:STANDARD\r\nTZOFFSETFROM:+0200\r\nTZOFFSETTO:+0100\r\n\
             DTSTART:19701025T030000\r\nRRULE:FREQ=YEARLY;BYMONTH=10;BYDAY=-1SU\r\n\
             END:STANDARD\r\nEND:VTIMEZONE\r\n\
             BEGIN:VEVENT\r\nUID:f\r\nDTSTART;TZID=Z:20260329T023000\r\nDURATION:PT1H\r\n\
             RRULE:{rule}\r\n{extra}\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n"
        ),
    ]
}

fuzz_target!(|data: &[u8]| {
    let Ok(text) = std::str::from_utf8(data) else {
        return;
    };
    // Windows in the past, around DTSTART (moved by the input length), in
    // the far future and open on one side.
    let shift = i64::try_from(data.len()).unwrap_or(0) * 86_400;
    let windows = [
        (Some(0), Some(1_000_000_000)),
        (Some(1_774_000_000 + shift), Some(1_775_000_000 + shift)),
        (Some(4_000_000_000), None),
        (None, Some(1_800_000_000)),
    ];
    for object in objects(text) {
        let Ok(indexed) = CalendarObject::parse(object.as_bytes()) else {
            continue;
        };
        for (start, end) in windows {
            let mut budget = Budget::new(OBJECT_BUDGET);
            let exact = exact_time_range_match(object.as_bytes(), start, end, &mut budget);
            // An exact match must be within the index; inexact answers are
            // conservative "yes" and say nothing about the index.
            assert!(
                exact != Some(true) || indexed.range.overlaps(start, end),
                "the index excludes a matching time range"
            );
            let mut budget = Budget::new(OBJECT_BUDGET);
            let matched = matches_time_range(object.as_bytes(), start, end, &mut budget);
            assert!(exact.is_none() || exact == Some(matched));
        }
    }
});
