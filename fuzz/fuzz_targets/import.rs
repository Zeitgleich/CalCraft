//! Import of whole files in every format (iCalendar, vCard, jCal, jCard,
//! xCal, CSV): conversion must never panic, and every object it produces
//! is either rejected by the PUT validation or exports without panicking.
#![no_main]

use calcraft_ical::{CalendarObject, Exporter, Format, ImportContext, Target, VCard, transfer};
use libfuzzer_sys::fuzz_target;

const CASES: [(Format, Target); 8] = [
    (Format::ICalendar, Target::Calendar),
    (Format::JCal, Target::Calendar),
    (Format::XCal, Target::Calendar),
    (Format::Csv, Target::Calendar),
    (Format::VCard, Target::Addressbook),
    (Format::JCard, Target::Addressbook),
    (Format::Csv, Target::Addressbook),
    (Format::ICalendar, Target::Addressbook),
];

fuzz_target!(|data: &[u8]| {
    let _ = calcraft_ical::csv::parse(&String::from_utf8_lossy(data), 1000);
    for (format, target) in CASES {
        let mut counter = 0u32;
        let mut new_uid = || {
            counter += 1;
            format!("fuzz-{counter}")
        };
        let mut ctx = ImportContext {
            dtstamp: "20261021T120000Z",
            new_uid: &mut new_uid,
        };
        let Ok(objects) = transfer::import(data, format, target, &mut ctx) else {
            continue;
        };
        for object in objects {
            let Ok(bytes) = object.data else { continue };
            let valid = match target {
                Target::Calendar => CalendarObject::parse(&bytes).is_ok(),
                Target::Addressbook => VCard::parse(&bytes).is_ok(),
            };
            if !valid {
                continue;
            }
            for export in [
                Format::ICalendar,
                Format::JCal,
                Format::XCal,
                Format::VCard,
                Format::JCard,
                Format::Csv,
            ] {
                if let Ok(mut exporter) = Exporter::new(export, target) {
                    let _ = exporter.begin(Some("fuzz"));
                    let _ = exporter.object(&bytes);
                    let _ = exporter.end();
                }
            }
        }
    }
});
