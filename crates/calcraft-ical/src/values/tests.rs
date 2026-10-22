use super::*;

fn prop(line: &str) -> Property {
    let data = format!("BEGIN:X\r\n{line}\r\nEND:X\r\n");
    crate::parse(data.as_bytes()).unwrap().properties.remove(0)
}

fn round_trip(dialect: Dialect, line: &str) -> TypedProperty {
    let original = prop(line);
    let typed = to_typed(&original, dialect);
    let back = from_typed(&typed, dialect).unwrap();
    assert_eq!(back, original, "{line}");
    typed
}

#[test]
fn ical_types() {
    let t = round_trip(Dialect::ICal, "DTSTART;TZID=Europe/Berlin:20261021T100000");
    assert_eq!(t.vtype, "date-time");
    assert_eq!(t.values, [TypedValue::Text("2026-10-21T10:00:00".into())]);
    let t = round_trip(Dialect::ICal, "DTSTART;VALUE=DATE:20261021");
    assert_eq!(t.vtype, "date");
    assert!(t.params.is_empty());
    let t = round_trip(Dialect::ICal, "SUMMARY:a\\, b\\; c\\nd\\\\");
    assert_eq!(t.values, [TypedValue::Text("a, b; c\nd\\".into())]);
    let t = round_trip(Dialect::ICal, "CATEGORIES:x\\,y,z");
    assert_eq!(t.values.len(), 2);
    let t = round_trip(
        Dialect::ICal,
        "RRULE:FREQ=WEEKLY;UNTIL=20261231T000000Z;BYDAY=MO,WE",
    );
    assert_eq!(
        t.values,
        [TypedValue::Recur(vec![
            ("freq".into(), vec!["WEEKLY".into()]),
            ("until".into(), vec!["2026-12-31T00:00:00Z".into()]),
            ("byday".into(), vec!["MO".into(), "WE".into()]),
        ])]
    );
    round_trip(Dialect::ICal, "EXDATE:20261028T100000Z,20261104T100000Z");
    round_trip(Dialect::ICal, "RDATE;VALUE=PERIOD:20261021T100000Z/PT1H");
    round_trip(Dialect::ICal, "TZOFFSETFROM:+0200");
    round_trip(Dialect::ICal, "GEO:37.386013;-122.082932");
    round_trip(Dialect::ICal, "PRIORITY:5");
    round_trip(Dialect::ICal, "TRIGGER:-PT15M");
    round_trip(Dialect::ICal, "TRIGGER;VALUE=DATE-TIME:20261021T090000Z");
    round_trip(Dialect::ICal, "REQUEST-STATUS:2.0;Success");
    round_trip(Dialect::ICal, "X-CUSTOM;X-P=1:raw\\nvalue");
    // A value that does not match its type survives as unknown.
    let t = round_trip(Dialect::ICal, "DTSTART:garbage");
    assert_eq!(t.vtype, "unknown");
    let t = round_trip(Dialect::ICal, "DTSTART;VALUE=DATE:garbage");
    assert_eq!(t.vtype, "unknown");
}

#[test]
fn vcard_types() {
    let t = round_trip(Dialect::VCard, "N:Doe;John;Q.,R.;Dr.;");
    assert_eq!(
        t.values,
        [TypedValue::Structured(vec![
            vec!["Doe".into()],
            vec!["John".into()],
            vec!["Q.".into(), "R.".into()],
            vec!["Dr.".into()],
            vec![String::new()],
        ])]
    );
    let t = round_trip(Dialect::VCard, "BDAY:19850412");
    assert_eq!(t.values, [TypedValue::Text("1985-04-12".into())]);
    let t = round_trip(Dialect::VCard, "BDAY:--0412");
    assert_eq!(t.values, [TypedValue::Text("--04-12".into())]);
    let t = round_trip(Dialect::VCard, "REV:19951031T222710Z");
    assert_eq!(t.values, [TypedValue::Text("1995-10-31T22:27:10Z".into())]);
    round_trip(Dialect::VCard, "ANNIVERSARY:19960415T140000-0500");
    round_trip(
        Dialect::VCard,
        "TEL;VALUE=uri;TYPE=work:tel:+1-555-555-5555",
    );
    round_trip(Dialect::VCard, "ORG:ABC\\, Inc.;North American Division");
    round_trip(Dialect::VCard, "NICKNAME:Jim,Jimmie");
}

#[test]
fn rejects_bad_names_and_injection() {
    let typed = TypedProperty {
        name: "summary\r\nx".into(),
        params: vec![],
        vtype: "text".into(),
        values: vec![TypedValue::Text("x".into())],
    };
    assert!(from_typed(&typed, Dialect::ICal).is_err());
    // TEXT newlines are escaped, so they cannot start a new line.
    let typed = TypedProperty {
        name: "summary".into(),
        params: vec![],
        vtype: "text".into(),
        values: vec![TypedValue::Text("x\r\nBEGIN:VEVENT".into())],
    };
    assert_eq!(
        from_typed(&typed, Dialect::ICal).unwrap().value,
        "x\\nBEGIN:VEVENT"
    );
}

#[test]
fn rejects_values_that_do_not_match_their_type() {
    let bad = |vtype: &str, value: TypedValue| {
        let typed = TypedProperty {
            name: "x-test".into(),
            params: vec![],
            vtype: vtype.into(),
            values: vec![value],
        };
        from_typed(&typed, Dialect::ICal).is_err()
    };
    let recur = |key: &str, value: &str| {
        TypedValue::Recur(vec![
            ("freq".into(), vec!["DAILY".into()]),
            (key.into(), vec![value.into()]),
        ])
    };
    assert!(bad("recur", recur("x-evil", "1")));
    assert!(bad("recur", recur("count", "1;BYDAY=MO")));
    assert!(bad("recur", recur("byday", "MO:x")));
    assert!(bad("recur", recur("until", "2026-10-21;X=1")));
    assert!(!bad("recur", recur("byday", "-1SU")));
    assert!(bad("integer", TypedValue::Number("5;X".into())));
    assert!(bad("float", TypedValue::Geo("1.5".into(), "2;3".into())));
    assert!(bad(
        "date-time",
        TypedValue::Text("2026-10-21T10:00:00;x".into())
    ));
    assert!(bad(
        "period",
        TypedValue::Period("20261021T100000Z".into(), "P1D,x".into())
    ));
    // Unknown rule parts in stored data survive as an `unknown` value.
    let t = round_trip(Dialect::ICal, "RRULE:FREQ=DAILY;X-NAME=1");
    assert_eq!(t.vtype, "unknown");
}

#[test]
fn json_numbers() {
    for ok in ["0", "5", "-1", "0.5", "37.386013"] {
        assert!(is_json_number(ok), "{ok}");
    }
    for bad in ["", "+1", "01", "1.", ".5", "1e5", "x"] {
        assert!(!is_json_number(bad), "{bad}");
    }
}
