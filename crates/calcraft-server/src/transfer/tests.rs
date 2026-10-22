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

#[cfg(feature = "migrate")]
fn calendar() -> Collection {
    Collection {
        id: 1,
        owner_id: 1,
        kind: CollectionKind::Calendar,
        slug: "work".parse().unwrap(),
        display_name: None,
        description: None,
        color: None,
        sync_revision: 0,
        components: "VEVENT,VTODO".to_owned(),
        sync_floor: 0,
        token_salt: 0,
        read_only: false,
    }
}

#[cfg(feature = "migrate")]
fn uids(prepared: &[Prepared]) -> Vec<Option<String>> {
    prepared.iter().map(|p| p.uid.clone()).collect()
}

/// Valid objects are kept byte for byte; others are normalised like an
/// import (split per UID, stable UIDs for objects without one) or rejected
/// with the source name and reason.
#[cfg(feature = "migrate")]
#[test]
fn fetched_objects_are_kept_or_normalised() {
    let valid = b"BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:x\r\nBEGIN:VEVENT\r\nUID:a\r\nDTSTAMP:20261001T080000Z\r\nDTSTART:20261001T080000Z\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";
    let prepared = prepare_fetched(&calendar(), "a.ics", valid.to_vec(), 1 << 20);
    assert_eq!(uids(&prepared), [Some("a".to_owned())]);
    let Ok((_, bytes)) = &prepared[0].result else {
        panic!("rejected");
    };
    assert_eq!(bytes, valid);

    // Two UIDs in one resource (some servers export like that).
    let two = b"BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:x\r\nBEGIN:VEVENT\r\nUID:a\r\nDTSTAMP:20261001T080000Z\r\nDTSTART:20261001T080000Z\r\nEND:VEVENT\r\nBEGIN:VEVENT\r\nUID:b\r\nDTSTAMP:20261001T080000Z\r\nDTSTART:20261002T080000Z\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";
    let prepared = prepare_fetched(&calendar(), "two.ics", two.to_vec(), 1 << 20);
    assert_eq!(
        uids(&prepared),
        [Some("a".to_owned()), Some("b".to_owned())]
    );

    // Without UID: derived from the source name, the same on every run.
    let anonymous = b"BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:x\r\nBEGIN:VEVENT\r\nDTSTAMP:20261001T080000Z\r\nDTSTART:20261001T080000Z\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";
    let first = prepare_fetched(&calendar(), "n.ics", anonymous.to_vec(), 1 << 20);
    let second = prepare_fetched(&calendar(), "n.ics", anonymous.to_vec(), 1 << 20);
    assert_eq!(uids(&first), uids(&second));
    assert_eq!(first[0].uid.as_ref().map(String::len), Some(36));

    let journal = b"BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:x\r\nBEGIN:VJOURNAL\r\nUID:j\r\nDTSTAMP:20261001T080000Z\r\nEND:VJOURNAL\r\nEND:VCALENDAR\r\n";
    let prepared = prepare_fetched(&calendar(), "j\u{1b}.ics", journal.to_vec(), 1 << 20);
    let Err(reason) = &prepared[0].result else {
        panic!("accepted");
    };
    assert_eq!(
        reason,
        "j\u{fffd}.ics: component type not supported by this calendar"
    );
}
