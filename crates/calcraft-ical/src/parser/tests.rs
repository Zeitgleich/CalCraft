use super::*;

const EVENT: &str = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//Test//EN\r\nBEGIN:VEVENT\r\nUID:abc@example.com\r\nDTSTART;TZID=\"Europe/Berlin\":20261021T100000\r\nSUMMARY:Long \r\n summary\r\nATTENDEE;ROLE=REQ-PARTICIPANT;DELEGATED-FROM=\"mailto:a@x\",\"mailto:b@x\":mailto:c@x\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";

#[test]
fn parses_event() {
    let cal = parse(EVENT.as_bytes()).unwrap();
    assert_eq!(cal.name, "VCALENDAR");
    let event = &cal.components[0];
    assert_eq!(event.name, "VEVENT");
    assert_eq!(event.property("SUMMARY").unwrap().value, "Long summary");
    let dtstart = event.property("DTSTART").unwrap();
    assert_eq!(dtstart.parameter("TZID").unwrap().values, ["Europe/Berlin"]);
    assert_eq!(dtstart.value, "20261021T100000");
    let attendee = event.property("ATTENDEE").unwrap();
    assert_eq!(
        attendee.parameter("DELEGATED-FROM").unwrap().values,
        ["mailto:a@x", "mailto:b@x"]
    );
    assert_eq!(attendee.value, "mailto:c@x");
}

#[test]
fn accepts_lf_and_lowercase() {
    let data = "begin:vcalendar\nversion:2.0\nbegin:vevent\nuid:1\nend:vevent\nend:vcalendar\n";
    let cal = parse(data.as_bytes()).unwrap();
    assert_eq!(cal.components[0].property("UID").unwrap().value, "1");
}

#[test]
fn rejects_malformed_input() {
    for data in [
        "",
        "BEGIN:VCALENDAR\r\n",
        "END:VCALENDAR\r\n",
        "BEGIN:VCALENDAR\r\nEND:VEVENT\r\n",
        "BEGIN:VCALENDAR\r\nNOCOLON\r\nEND:VCALENDAR\r\n",
        "BEGIN:VCALENDAR\r\nX;P=\"open:value\r\nEND:VCALENDAR\r\n",
        "BEGIN:VCALENDAR\r\nSUMMARY:a\u{0}b\r\nEND:VCALENDAR\r\n",
        "BEGIN:VCALENDAR\r\nEND:VCALENDAR\r\nBEGIN:VCALENDAR\r\nEND:VCALENDAR\r\n",
        "UID:outside\r\n",
        "BEGIN:VCAL ENDAR\r\nEND:VCAL ENDAR\r\n",
    ] {
        assert!(parse(data.as_bytes()).is_err(), "{data:?}");
    }
    assert!(parse(b"\xff\xfe").is_err());
}

#[test]
fn enforces_limits() {
    let deep = format!(
        "{}{}",
        "BEGIN:X\r\n".repeat(MAX_DEPTH + 1),
        "END:X\r\n".repeat(MAX_DEPTH + 1)
    );
    assert_eq!(
        parse(deep.as_bytes()),
        Err(IcalError::Limit("components nested too deeply"))
    );

    let many = format!(
        "BEGIN:VCALENDAR\r\n{}END:VCALENDAR\r\n",
        "X:1\r\n".repeat(MAX_PROPERTIES + 1)
    );
    assert_eq!(
        parse(many.as_bytes()),
        Err(IcalError::Limit("too many properties"))
    );

    assert_eq!(
        parse(&vec![b'a'; MAX_OBJECT_SIZE + 1]),
        Err(IcalError::TooLarge)
    );
}
