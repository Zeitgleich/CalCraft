use super::*;

#[test]
fn summaries() {
    let event = b"BEGIN:VCALENDAR\r\nVERSION:2.0\r\nBEGIN:VTIMEZONE\r\nTZID:X\r\nEND:VTIMEZONE\r\nBEGIN:VEVENT\r\nUID:a\r\nSUMMARY:Lunch\\, with Bob\\nand Eve\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";
    assert_eq!(
        summary(CollectionKind::Calendar, event).as_deref(),
        Some("Lunch, with Bob and Eve")
    );
    let card = b"BEGIN:VCARD\r\nVERSION:4.0\r\nUID:c\r\nFN:Erika Mustermann\r\nEND:VCARD\r\n";
    assert_eq!(
        summary(CollectionKind::Addressbook, card).as_deref(),
        Some("Erika Mustermann")
    );
    let hidden = "BEGIN:VCARD\r\nVERSION:4.0\r\nUID:c\r\nFN:a\u{202E}b\r\nEND:VCARD\r\n";
    assert_eq!(
        summary(CollectionKind::Addressbook, hidden.as_bytes()),
        None
    );
    let long = format!(
        "BEGIN:VCARD\r\nVERSION:4.0\r\nUID:c\r\nFN:{}\r\nEND:VCARD\r\n",
        "x".repeat(500)
    );
    assert_eq!(
        summary(CollectionKind::Addressbook, long.as_bytes())
            .unwrap()
            .len(),
        200
    );
    assert_eq!(summary(CollectionKind::Calendar, b"garbage"), None);
}
