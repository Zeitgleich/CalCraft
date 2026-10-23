use super::*;

fn object(body: &str) -> Vec<u8> {
    format!(
        "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:x\r\nX-WR-SECRET:cal\r\n{body}END:VCALENDAR\r\n"
    )
    .into_bytes()
}

const PRIVATE_MASTER: &str = "BEGIN:VEVENT\r\nUID:u\r\nDTSTAMP:20261001T000000Z\r\nDTSTART:20261021T100000Z\r\nDTEND:20261021T110000Z\r\nRRULE:FREQ=WEEKLY;COUNT=3\r\nCLASS:PRIVATE\r\nSUMMARY:Doctor\r\nDESCRIPTION:Secret\r\nLOCATION:Clinic\r\nATTENDEE:mailto:a@b\r\nATTACH:https://x/y\r\nURL:https://x\r\nX-SECRET:1\r\nRELATED-TO:other\r\nBEGIN:VALARM\r\nACTION:DISPLAY\r\nDESCRIPTION:Alarm\r\nTRIGGER:-PT5M\r\nEND:VALARM\r\nEND:VEVENT\r\n";

#[test]
fn public_objects_are_unchanged() {
    let public = object(
        "BEGIN:VEVENT\r\nUID:u\r\nDTSTAMP:20261001T000000Z\r\nDTSTART:20261021T100000Z\r\nSUMMARY:Open\r\nCLASS:PUBLIC\r\nEND:VEVENT\r\n",
    );
    assert!(sharee_view(&public).is_none());
    let none = object(
        "BEGIN:VEVENT\r\nUID:u\r\nDTSTAMP:20261001T000000Z\r\nDTSTART:20261021T100000Z\r\nSUMMARY:Open\r\nEND:VEVENT\r\n",
    );
    assert!(sharee_view(&none).is_none());
    assert!(sharee_view(b"not icalendar").is_none());
}

#[test]
fn private_events_become_busy_stubs() {
    let stub = String::from_utf8(sharee_view(&object(PRIVATE_MASTER)).unwrap()).unwrap();
    for kept in [
        "UID:u",
        "DTSTART:20261021T100000Z",
        "DTEND:",
        "RRULE:",
        "CLASS:PRIVATE",
        "SUMMARY:Busy",
        "VERSION:2.0",
    ] {
        assert!(stub.contains(kept), "{kept} missing:\n{stub}");
    }
    for hidden in [
        "Doctor",
        "Secret",
        "Clinic",
        "ATTENDEE",
        "ATTACH",
        "URL",
        "X-SECRET",
        "X-WR-SECRET",
        "RELATED-TO",
        "VALARM",
        "Alarm",
    ] {
        assert!(!stub.contains(hidden), "{hidden} visible:\n{stub}");
    }
    assert!(parse(stub.as_bytes()).is_ok());
}

#[test]
fn overrides_inherit_the_masters_class() {
    let inherited = "BEGIN:VEVENT\r\nUID:u\r\nDTSTAMP:20261001T000000Z\r\nRECURRENCE-ID:20261028T100000Z\r\nDTSTART:20261028T120000Z\r\nDTEND:20261028T130000Z\r\nSUMMARY:Moved doctor\r\nEND:VEVENT\r\n";
    let public = "BEGIN:VEVENT\r\nUID:u\r\nDTSTAMP:20261001T000000Z\r\nRECURRENCE-ID:20261104T100000Z\r\nDTSTART:20261104T100000Z\r\nCLASS:PUBLIC\r\nSUMMARY:Open session\r\nEND:VEVENT\r\n";
    let stub = String::from_utf8(
        sharee_view(&object(&format!("{PRIVATE_MASTER}{inherited}{public}"))).unwrap(),
    )
    .unwrap();
    assert!(!stub.contains("Moved doctor"), "{stub}");
    assert!(stub.contains("RECURRENCE-ID:20261028T100000Z"));
    // An override that is explicitly public stays visible.
    assert!(stub.contains("Open session"));
    // A private override of a public master is hidden too.
    let master = "BEGIN:VEVENT\r\nUID:v\r\nDTSTAMP:20261001T000000Z\r\nDTSTART:20261021T100000Z\r\nRRULE:FREQ=DAILY;COUNT=2\r\nSUMMARY:Standup\r\nEND:VEVENT\r\n";
    let private = "BEGIN:VEVENT\r\nUID:v\r\nDTSTAMP:20261001T000000Z\r\nRECURRENCE-ID:20261022T100000Z\r\nDTSTART:20261022T100000Z\r\nCLASS:CONFIDENTIAL\r\nSUMMARY:Review\r\nEND:VEVENT\r\n";
    let stub =
        String::from_utf8(sharee_view(&object(&format!("{master}{private}"))).unwrap()).unwrap();
    assert!(
        stub.contains("Standup") && !stub.contains("Review"),
        "{stub}"
    );
    // Unknown classes are private.
    let unknown = object(
        "BEGIN:VTODO\r\nUID:t\r\nDTSTAMP:20261001T000000Z\r\nDUE:20261021T100000Z\r\nCLASS:X-TEAM\r\nSUMMARY:Task\r\nEND:VTODO\r\n",
    );
    let stub = String::from_utf8(sharee_view(&unknown).unwrap()).unwrap();
    assert!(stub.contains("DUE:") && !stub.contains("Task"), "{stub}");
}
