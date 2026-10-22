use super::*;

#[test]
fn generates_uuids_and_timestamps() {
    let uid = new_uid();
    assert_eq!(uid.len(), 36);
    assert_eq!(&uid[14..15], "4");
    assert_ne!(uid, new_uid());
    let stamp = dtstamp();
    assert!(calcraft_ical::parse_utc_datetime(&stamp).is_ok(), "{stamp}");
}

#[test]
fn reports_are_printable() {
    assert_eq!(
        printable("a\u{1b}[31mb\r\n\u{202e}c"),
        "a\u{fffd}[31mb\u{fffd}\u{fffd}\u{fffd}c"
    );
    let mut report = ImportReport::default();
    reject(&mut report, Some("x\u{7}y"), "bad\u{1b}");
    assert_eq!(report.errors[0].uid.as_deref(), Some("x\u{fffd}y"));
    assert_eq!(report.errors[0].reason, "bad\u{fffd}");
}

#[test]
fn formats_fit_the_collection() {
    assert_eq!(
        format_for(CollectionKind::Calendar, None).unwrap(),
        Format::ICalendar
    );
    assert_eq!(
        format_for(CollectionKind::Addressbook, Some("csv")).unwrap(),
        Format::Csv
    );
    assert!(format_for(CollectionKind::Addressbook, Some("ics")).is_err());
    assert!(format_for(CollectionKind::Calendar, Some("pdf")).is_err());
}
