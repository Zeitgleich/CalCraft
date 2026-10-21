//! Import/export round trips and hostile input for all formats.
#![allow(clippy::unwrap_used)]

use calcraft_ical::{
    CalendarObject, Exporter, Format, IcalError, ImportContext, MAX_IMPORT_OBJECTS,
    MAX_IMPORT_SIZE, Target, VCard, parse, transfer,
};

const CALENDAR: &str = concat!(
    "BEGIN:VCALENDAR\r\n",
    "VERSION:2.0\r\n",
    "PRODID:-//Example Corp.//Test//EN\r\n",
    "METHOD:PUBLISH\r\n",
    "BEGIN:VTIMEZONE\r\n",
    "TZID:Europe/Berlin\r\n",
    "BEGIN:DAYLIGHT\r\n",
    "TZOFFSETFROM:+0100\r\n",
    "TZOFFSETTO:+0200\r\n",
    "TZNAME:CEST\r\n",
    "DTSTART:19700329T020000\r\n",
    "RRULE:FREQ=YEARLY;BYDAY=-1SU;BYMONTH=3\r\n",
    "END:DAYLIGHT\r\n",
    "BEGIN:STANDARD\r\n",
    "TZOFFSETFROM:+0200\r\n",
    "TZOFFSETTO:+0100\r\n",
    "TZNAME:CET\r\n",
    "DTSTART:19701025T030000\r\n",
    "RRULE:FREQ=YEARLY;BYDAY=-1SU;BYMONTH=10\r\n",
    "END:STANDARD\r\n",
    "END:VTIMEZONE\r\n",
    "BEGIN:VTIMEZONE\r\n",
    "TZID:America/New_York\r\n",
    "BEGIN:STANDARD\r\n",
    "TZOFFSETFROM:-0400\r\n",
    "TZOFFSETTO:-0500\r\n",
    "DTSTART:19701101T020000\r\n",
    "END:STANDARD\r\n",
    "END:VTIMEZONE\r\n",
    // Recurring master; its overrides come later in the file.
    "BEGIN:VEVENT\r\n",
    "UID:weekly@example.com\r\n",
    "DTSTAMP:20261001T080000Z\r\n",
    "DTSTART;TZID=Europe/Berlin:20261021T100000\r\n",
    "DTEND;TZID=Europe/Berlin:20261021T110000\r\n",
    "RRULE:FREQ=WEEKLY;UNTIL=20261231T000000Z;BYDAY=WE\r\n",
    "EXDATE;TZID=Europe/Berlin:20261028T100000,20261104T100000\r\n",
    "SUMMARY:Team meeting\r\n",
    "ATTENDEE;CN=\"Doe, Jane\";ROLE=REQ-PARTICIPANT;PARTSTAT=ACCEPTED:mailto:jane@example.com\r\n",
    "ORGANIZER;CN=Boss:mailto:boss@example.com\r\n",
    "BEGIN:VALARM\r\n",
    "ACTION:DISPLAY\r\n",
    "DESCRIPTION:Reminder\r\n",
    "TRIGGER:-PT15M\r\n",
    "END:VALARM\r\n",
    "END:VEVENT\r\n",
    "BEGIN:VEVENT\r\n",
    "UID:allday@example.com\r\n",
    "DTSTAMP:20261001T080000Z\r\n",
    "DTSTART;VALUE=DATE:20261224\r\n",
    "DTEND;VALUE=DATE:20261227\r\n",
    "SUMMARY:Holidays 🎄 Weihnachten – Ferien\r\n",
    "CATEGORIES:Family,Holiday\\, public,Travel\r\n",
    "GEO:52.520008;13.404954\r\n",
    "TRANSP:TRANSPARENT\r\n",
    "END:VEVENT\r\n",
    "BEGIN:VEVENT\r\n",
    "UID:weekly@example.com\r\n",
    "DTSTAMP:20261001T080000Z\r\n",
    "RECURRENCE-ID;TZID=Europe/Berlin:20261111T100000\r\n",
    "DTSTART;TZID=Europe/Berlin:20261111T120000\r\n",
    "DTEND;TZID=Europe/Berlin:20261111T130000\r\n",
    "SUMMARY:Team meeting (moved)\r\n",
    "END:VEVENT\r\n",
    "BEGIN:VEVENT\r\n",
    "UID:escapes@example.com\r\n",
    "DTSTAMP:20261001T080000Z\r\n",
    "DTSTART:20261030T150000Z\r\n",
    "DURATION:PT1H30M\r\n",
    "SUMMARY:Review\\; part 1\\, 2 \\\\ more\r\n",
    "DESCRIPTION:Line one\\nLine two with a very long text that certainly needs t\r\n",
    " o be folded because it is much longer than seventy-five octets – ÄÖÜ äöü ß\r\n",
    "  €€€ 日本語のテキスト\r\n",
    "LOCATION:Room \"A\"\r\n",
    "X-CUSTOM-PROP;X-PARAM=a,b:some\\,raw\\nvalue\r\n",
    "PRIORITY:5\r\n",
    "SEQUENCE:2\r\n",
    "CLASS:PUBLIC\r\n",
    "URL:https://example.com/a?b=c&d=e\r\n",
    "END:VEVENT\r\n",
    "BEGIN:VEVENT\r\n",
    "UID:weekly@example.com\r\n",
    "DTSTAMP:20261001T080000Z\r\n",
    "RECURRENCE-ID;TZID=Europe/Berlin:20261118T100000\r\n",
    "DTSTART;TZID=Europe/Berlin:20261118T100000\r\n",
    "DTEND;TZID=Europe/Berlin:20261118T113000\r\n",
    "SUMMARY:Team meeting (long)\r\n",
    "END:VEVENT\r\n",
    "BEGIN:VTODO\r\n",
    "UID:todo@example.com\r\n",
    "DTSTAMP:20261001T080000Z\r\n",
    "DUE;TZID=America/New_York:20261101T170000\r\n",
    "SUMMARY:File taxes\r\n",
    "PERCENT-COMPLETE:40\r\n",
    "STATUS:IN-PROCESS\r\n",
    "END:VTODO\r\n",
    "END:VCALENDAR\r\n",
    // A second calendar in the same file.
    "BEGIN:VCALENDAR\r\n",
    "VERSION:2.0\r\n",
    "PRODID:-//Other//EN\r\n",
    "BEGIN:VJOURNAL\r\n",
    "UID:journal@example.com\r\n",
    "DTSTAMP:20261001T080000Z\r\n",
    "DTSTART;VALUE=DATE:20261020\r\n",
    "SUMMARY:Notes\r\n",
    "END:VJOURNAL\r\n",
    "BEGIN:VFREEBUSY\r\n",
    "UID:fb@example.com\r\n",
    "FREEBUSY:20261021T100000Z/PT1H,20261021T150000Z/20261021T160000Z\r\n",
    "END:VFREEBUSY\r\n",
    "END:VCALENDAR\r\n",
);

const CARDS: &str = concat!(
    "BEGIN:VCARD\r\n",
    "VERSION:4.0\r\n",
    "UID:urn:uuid:4fbe8971-0bc3-424c-9c26-36c3e1eff6b1\r\n",
    "FN:Dr. Jürgen Müller-Lüdenscheidt\r\n",
    "N:Müller-Lüdenscheidt;Jürgen;Q.,R.;Dr.;\r\n",
    "NICKNAME:Jürg,JM\r\n",
    "EMAIL;TYPE=work:juergen@example.com\r\n",
    "EMAIL;TYPE=home;PREF=1:j.m@example.org\r\n",
    "TEL;VALUE=uri;TYPE=\"voice,cell\":tel:+49-170-1234567\r\n",
    "ADR;TYPE=work:;;Hauptstraße 1;Berlin;;10115;Germany\r\n",
    "ORG:ABC\\, Inc.;Research\r\n",
    "BDAY:19850412\r\n",
    "ANNIVERSARY:--0415\r\n",
    "REV:20261001T080000Z\r\n",
    "NOTE:Met at the conference\\, room 3\\nSecond line\\; with semicolon\r\n",
    "CATEGORIES:friends,work\r\n",
    "URL:https://example.com/~jm\r\n",
    "END:VCARD\r\n",
    "BEGIN:VCARD\r\n",
    "VERSION:3.0\r\n",
    "UID:v3-card\r\n",
    "FN:Bob\r\n",
    "N:Bob;;;;\r\n",
    "TEL;TYPE=CELL:+49 123 456\r\n",
    "PHOTO;ENCODING=b;TYPE=JPEG:/9j/4AAQSkZJRgABAQEASABIAAD/2wBDAAoHBwgHBgoICAgLCgoLDhgQDg0NDh0VFhEYIx8lJCIfIiEmKzcvJik0KSEiMEExNDk7Pj4+JS5ESUM8SDc9Pjv/\r\n",
    "END:VCARD\r\n",
);

fn context_import(data: &[u8], format: Format, target: Target) -> Vec<Result<Vec<u8>, IcalError>> {
    let mut counter = 0;
    let mut new_uid = || {
        counter += 1;
        format!("generated-{counter}")
    };
    let mut ctx = ImportContext {
        dtstamp: "20261021T120000Z",
        new_uid: &mut new_uid,
    };
    transfer::import(data, format, target, &mut ctx)
        .unwrap()
        .into_iter()
        .map(|o| o.data)
        .collect()
}

/// Imports and validates every object like a PUT would.
fn import(data: &[u8], format: Format, target: Target) -> Vec<Vec<u8>> {
    context_import(data, format, target)
        .into_iter()
        .filter_map(Result::ok)
        .inspect(|object| match target {
            Target::Calendar => {
                CalendarObject::parse(object).unwrap();
            }
            Target::Addressbook => {
                VCard::parse(object).unwrap();
            }
        })
        .collect()
}

fn export(objects: &[Vec<u8>], format: Format, target: Target) -> String {
    let mut exporter = Exporter::new(format, target).unwrap();
    let mut out = exporter.begin(Some("My calendar")).unwrap();
    for object in objects {
        out.push_str(&exporter.object(object).unwrap());
    }
    out.push_str(&exporter.end());
    out
}

fn text(object: &[u8]) -> &str {
    std::str::from_utf8(object).unwrap()
}

#[test]
fn ics_import_groups_by_uid_and_copies_timezones() {
    let results = context_import(CALENDAR.as_bytes(), Format::ICalendar, Target::Calendar);
    // The VFREEBUSY is rejected, everything else becomes one object per UID.
    assert_eq!(results.iter().filter(|r| r.is_err()).count(), 1);
    let objects = import(CALENDAR.as_bytes(), Format::ICalendar, Target::Calendar);
    assert_eq!(objects.len(), 5);

    let weekly = text(&objects[0]);
    assert_eq!(weekly.matches("BEGIN:VEVENT").count(), 3);
    assert!(weekly.contains("TZID:Europe/Berlin"));
    assert!(!weekly.contains("America/New_York"));
    assert!(!weekly.contains("METHOD"));
    assert!(weekly.contains("PRODID:-//Example Corp.//Test//EN"));

    let allday = text(&objects[1]);
    assert!(!allday.contains("VTIMEZONE"));
    let todo = text(&objects[3]);
    assert!(todo.contains("TZID:America/New_York") && !todo.contains("Europe/Berlin"));
    assert!(text(&objects[4]).contains("BEGIN:VJOURNAL"));

    for object in &objects {
        for line in text(object).split("\r\n") {
            assert!(line.len() <= 75, "{line}");
        }
    }
}

#[test]
fn calendar_round_trips_through_all_formats() {
    let original = import(CALENDAR.as_bytes(), Format::ICalendar, Target::Calendar);
    // Normalise once through our own export (PRODID, folding).
    let ics = export(&original, Format::ICalendar, Target::Calendar);
    let objects = import(ics.as_bytes(), Format::ICalendar, Target::Calendar);
    assert_eq!(objects.len(), original.len());

    for format in [Format::JCal, Format::XCal, Format::ICalendar] {
        let exported = export(&objects, format, Target::Calendar);
        let again = import(exported.as_bytes(), format, Target::Calendar);
        assert_eq!(again.len(), objects.len(), "{format:?}");
        for (a, b) in objects.iter().zip(&again) {
            assert_eq!(text(a), text(b), "{format:?}");
        }
    }

    // The content survived exactly, apart from PRODID.
    let original_tree = parse(&original[2]).unwrap();
    let final_tree = parse(&objects[2]).unwrap();
    assert_eq!(original_tree.components, final_tree.components);
}

#[test]
fn jcal_and_xcal_are_valid_documents() {
    let objects = import(CALENDAR.as_bytes(), Format::ICalendar, Target::Calendar);
    let jcal = export(&objects, Format::JCal, Target::Calendar);
    let value: serde_json::Value = serde_json::from_str(&jcal).unwrap();
    assert_eq!(value[0], "vcalendar");
    // Time zones are written once even though two objects use Berlin.
    let names: Vec<&str> = value[2]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c[0].as_str().unwrap())
        .collect();
    assert_eq!(names.iter().filter(|n| **n == "vtimezone").count(), 2);
    assert_eq!(names.iter().filter(|n| **n == "vevent").count(), 5);

    let xcal = export(&objects, Format::XCal, Target::Calendar);
    assert!(xcal.starts_with("<?xml"));
    assert!(xcal.contains("<categories><text>Family</text><text>Holiday, public</text>"));
    assert!(
        xcal.contains("<geo><latitude>52.520008</latitude><longitude>13.404954</longitude></geo>")
    );
    assert!(
        xcal.contains("<location><text>Room \"A\"</text></location>")
            || xcal.contains("Room &quot;A&quot;")
    );
}

#[test]
fn vcards_round_trip_through_jcard() {
    let original = import(CARDS.as_bytes(), Format::VCard, Target::Addressbook);
    assert_eq!(original.len(), 2);
    for format in [Format::JCard, Format::VCard] {
        let exported = export(&original, format, Target::Addressbook);
        let again = import(exported.as_bytes(), format, Target::Addressbook);
        assert_eq!(again, original, "{format:?}");
    }
    let jcard = export(&original, Format::JCard, Target::Addressbook);
    let value: serde_json::Value = serde_json::from_str(&jcard).unwrap();
    let props = &value[0][1];
    let n = props
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p[0] == "n")
        .unwrap();
    assert_eq!(
        n[3],
        serde_json::json!(["Müller-Lüdenscheidt", "Jürgen", ["Q.", "R."], "Dr.", ""])
    );
    let bday = props
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p[0] == "bday")
        .unwrap();
    assert_eq!(bday[3], "1985-04-12");
    // A single jCard (not wrapped in an array) is accepted as well.
    let single = value[1].to_string();
    assert_eq!(
        import(single.as_bytes(), Format::JCard, Target::Addressbook).len(),
        1
    );
}

#[test]
fn csv_export_neutralises_formulas_and_round_trips() {
    let calendar = concat!(
        "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:x\r\n",
        "BEGIN:VEVENT\r\nUID:f1\r\nDTSTAMP:20261001T080000Z\r\n",
        "DTSTART:20261021T100000Z\r\nDTEND:20261021T110000Z\r\n",
        "SUMMARY:=HYPERLINK(\"http://evil.example\"\\,\"click\")\r\n",
        "LOCATION:@SUM(A1:A2)\r\n",
        "DESCRIPTION:+cmd|' /C calc'!A0\\nsecond \"line\"\\, with comma\r\n",
        "END:VEVENT\r\n",
        "BEGIN:VEVENT\r\nUID:-dash\r\nDTSTAMP:20261001T080000Z\r\n",
        "DTSTART;VALUE=DATE:20261224\r\nDURATION:P2D\r\nSUMMARY:\tTabbed\r\n",
        "END:VEVENT\r\nEND:VCALENDAR\r\n",
    );
    let objects = import(calendar.as_bytes(), Format::ICalendar, Target::Calendar);
    let csv = export(&objects, Format::Csv, Target::Calendar);
    assert!(csv.starts_with("\u{feff}summary,start,end,all_day,location,description,uid\r\n"));
    assert!(csv.contains("\"'=HYPERLINK(\"\"http://evil.example\"\",\"\"click\"\")\""));
    assert!(csv.contains("\"'@SUM(A1:A2)\""));
    assert!(csv.contains("\"'+cmd|' /C calc'!A0\nsecond \"\"line\"\", with comma\""));
    assert!(csv.contains("\"'\tTabbed\",2026-12-24,2026-12-26,true"));
    assert!(csv.contains(",\"'-dash\""));
    // No unprotected cell starts with a formula character.
    for line in csv.lines() {
        for cell in line.split(',') {
            assert!(!cell.starts_with(['=', '+', '@']), "{cell}");
        }
    }

    let again = import(csv.as_bytes(), Format::Csv, Target::Calendar);
    assert_eq!(again.len(), 2);
    let first = parse(&again[0]).unwrap();
    let event = &first.components[0];
    assert_eq!(
        event.property("SUMMARY").unwrap().value,
        "=HYPERLINK(\"http://evil.example\"\\,\"click\")"
    );
    assert_eq!(event.property("DTSTART").unwrap().value, "20261021T100000Z");
    let second = parse(&again[1]).unwrap();
    assert_eq!(second.components[0].property("UID").unwrap().value, "-dash");
    assert_eq!(
        second.components[0].property("DTEND").unwrap().value,
        "20261226"
    );
}

#[test]
fn csv_contacts_round_trip() {
    let original = import(CARDS.as_bytes(), Format::VCard, Target::Addressbook);
    let csv = export(&original, Format::Csv, Target::Addressbook);
    assert!(csv.contains("juergen@example.com; j.m@example.org"));
    assert!(csv.contains("\"'+49-170-1234567\""));
    assert!(csv.contains("\"ABC, Inc.; Research\""));
    let again = import(csv.as_bytes(), Format::Csv, Target::Addressbook);
    assert_eq!(again.len(), 2);
    let card = parse(&again[0]).unwrap();
    assert_eq!(card.properties_named("EMAIL").count(), 2);
    assert_eq!(card.property("TEL").unwrap().value, "+49-170-1234567");
    assert_eq!(card.property("ORG").unwrap().value, "ABC\\, Inc.;Research");
    assert_eq!(
        card.property("UID").unwrap().value,
        "urn:uuid:4fbe8971-0bc3-424c-9c26-36c3e1eff6b1"
    );
}

#[test]
fn csv_import_of_simple_events() {
    let csv = "Title,Start,End,All Day,Location\n\
               Lunch,2026-10-21 12:00,2026-10-21 13:00,,Canteen\n\
               Trip,2026-11-01,2026-11-03,yes,\n\
               Broken,yesterday,,,\n\
               ,,,,\n\
               \"Multi\nline\",20261021T100000Z,,,\"a \"\"quoted\"\" place\"\n";
    let results = context_import(csv.as_bytes(), Format::Csv, Target::Calendar);
    assert_eq!(results.len(), 4);
    assert_eq!(results.iter().filter(|r| r.is_err()).count(), 1);
    let objects = import(csv.as_bytes(), Format::Csv, Target::Calendar);
    let lunch = text(&objects[0]);
    assert!(lunch.contains("DTSTART:20261021T120000\r\n"), "{lunch}");
    assert!(lunch.contains("UID:generated-1"));
    assert!(lunch.contains("DTSTAMP:20261021T120000Z"));
    let trip = text(&objects[1]);
    assert!(
        trip.contains("DTSTART;VALUE=DATE:20261101") && trip.contains("DTEND;VALUE=DATE:20261103")
    );
    let multi = text(&objects[2]);
    assert!(
        multi.contains("SUMMARY:Multi\\nline") && multi.contains("LOCATION:a \"quoted\" place")
    );
    assert_eq!(multi.matches("BEGIN:VEVENT").count(), 1);

    let no_start = "summary\nx\n";
    let mut new_uid = String::new;
    let mut ctx = ImportContext {
        dtstamp: "20261021T120000Z",
        new_uid: &mut new_uid,
    };
    assert!(
        transfer::import(no_start.as_bytes(), Format::Csv, Target::Calendar, &mut ctx).is_err()
    );
}

#[test]
fn injection_attempts_stay_inside_their_value() {
    // jCal/xCal text with CRLF must not create extra lines or events.
    let jcal = r#"["vcalendar",[["version",{},"text","2.0"]],[["vevent",[
        ["uid",{},"text","a"],
        ["summary",{"x-p":"v\"a:l;u,e"},"text","x\r\nEND:VEVENT\r\nBEGIN:VEVENT\r\nUID:b"],
        ["dtstart",{},"date-time","2026-10-21T10:00:00Z"]],[]]]]"#;
    let objects = import(jcal.as_bytes(), Format::JCal, Target::Calendar);
    assert_eq!(objects.len(), 1);
    let object = text(&objects[0]);
    assert_eq!(
        object
            .split("\r\n")
            .filter(|l| *l == "BEGIN:VEVENT")
            .count(),
        1
    );
    assert!(object.contains("SUMMARY;X-P=\"v'a:l;u,e\":x\\nEND:VEVENT\\nBEGIN:VEVENT\\nUID:b"));

    // Raw (non-TEXT) values with line breaks are rejected, not passed on.
    let raw = r#"["vcalendar",[],[["vevent",[["uid",{},"text","a"],
        ["url",{},"uri","http://x\r\nBEGIN:VEVENT"]],[]]]]"#;
    let results = context_import(raw.as_bytes(), Format::JCal, Target::Calendar);
    assert!(results[0].is_err());

    // A display name with CRLF in an export stays one line.
    let mut exporter = Exporter::new(Format::ICalendar, Target::Calendar).unwrap();
    let head = exporter
        .begin(Some("Evil\r\nBEGIN:VEVENT\r\nUID:x"))
        .unwrap();
    assert!(head.contains("X-WR-CALNAME:Evil\\nBEGIN:VEVENT\\nUID:x\r\n"));
    let mut xcal = Exporter::new(Format::XCal, Target::Calendar).unwrap();
    let head = xcal.begin(Some("</text><evil/>")).unwrap();
    assert!(head.contains("&lt;/text&gt;&lt;evil/&gt;"));
}

#[test]
fn rejects_hostile_files() {
    let mut new_uid = String::new;
    let mut ctx = ImportContext {
        dtstamp: "20261021T120000Z",
        new_uid: &mut new_uid,
    };
    let mut import =
        |data: &[u8], format| transfer::import(data, format, Target::Calendar, &mut ctx);

    let huge = vec![b'a'; MAX_IMPORT_SIZE + 1];
    for format in [Format::ICalendar, Format::JCal, Format::XCal, Format::Csv] {
        assert_eq!(import(&huge, format), Err(IcalError::TooLarge));
    }

    let event = "BEGIN:VEVENT\r\nUID:{}\r\nEND:VEVENT\r\n";
    let many = format!(
        "BEGIN:VCALENDAR\r\nVERSION:2.0\r\n{}END:VCALENDAR\r\n",
        (0..=MAX_IMPORT_OBJECTS)
            .map(|i| event.replace("{}", &i.to_string()))
            .collect::<String>()
    );
    assert_eq!(
        import(many.as_bytes(), Format::ICalendar),
        Err(IcalError::Limit("too many objects in import"))
    );
    let many_rows = format!(
        "summary,start\n{}",
        "x,20261021\n".repeat(MAX_IMPORT_OBJECTS + 1)
    );
    assert!(import(many_rows.as_bytes(), Format::Csv).is_err());

    let deep_json = format!("{}{}", "[".repeat(10_000), "]".repeat(10_000));
    assert!(import(deep_json.as_bytes(), Format::JCal).is_err());
    let nested_components = format!("{}{}", "[\"vcalendar\",[],[".repeat(50), "]]".repeat(50));
    assert!(import(nested_components.as_bytes(), Format::JCal).is_err());
    let deep_xml = format!(
        "<icalendar xmlns=\"urn:ietf:params:xml:ns:icalendar-2.0\">{}{}</icalendar>",
        "<a>".repeat(10_000),
        "</a>".repeat(10_000)
    );
    assert!(import(deep_xml.as_bytes(), Format::XCal).is_err());
    let laughs = concat!(
        "<?xml version=\"1.0\"?>",
        "<!DOCTYPE icalendar [<!ENTITY a \"aaaaaaaaaa\"><!ENTITY b \"&a;&a;&a;&a;&a;&a;\">]>",
        "<icalendar xmlns=\"urn:ietf:params:xml:ns:icalendar-2.0\"><vcalendar><properties>",
        "<prodid><text>&b;</text></prodid></properties></vcalendar></icalendar>"
    );
    assert!(import(laughs.as_bytes(), Format::XCal).is_err());
    let external = concat!(
        "<!DOCTYPE icalendar [<!ENTITY x SYSTEM \"file:///etc/passwd\">]>",
        "<icalendar xmlns=\"urn:ietf:params:xml:ns:icalendar-2.0\"><vcalendar><properties>",
        "<prodid><text>&x;</text></prodid></properties></vcalendar></icalendar>"
    );
    assert!(import(external.as_bytes(), Format::XCal).is_err());

    // Wrong format for the collection.
    assert!(import(CARDS.as_bytes(), Format::VCard).is_err());
    // Garbage.
    for format in [Format::ICalendar, Format::JCal, Format::XCal] {
        assert!(import(b"\xff\xfe garbage", format).is_err());
    }
}

#[test]
fn invalid_objects_are_left_to_validation() {
    // Structurally fine, but not acceptable for PUT: the import hands it
    // on and CalendarObject::parse rejects it.
    let calendar = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nBEGIN:VEVENT\r\nUID:x\r\nDTSTART:20261021T100000Z\r\nDTEND:20261020T100000Z\r\nEND:VEVENT\r\nBEGIN:VEVENT\r\nUID:y\r\nRRULE:FREQ=DAILY;INTERVAL=0\r\nDTSTART:20261021T100000Z\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";
    let results = context_import(calendar.as_bytes(), Format::ICalendar, Target::Calendar);
    assert_eq!(results.len(), 2);
    for result in results {
        assert!(CalendarObject::parse(&result.unwrap()).is_err());
    }
}
