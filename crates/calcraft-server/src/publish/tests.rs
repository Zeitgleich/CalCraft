use super::*;

fn link(busy_only: bool, private_events: PrivateEvents, include_attendees: bool) -> PublishLink {
    PublishLink {
        id: 1,
        public_id: "0123456789abcdef0123456789abcdef".to_owned(),
        collection_id: 1,
        busy_only,
        private_events,
        include_attendees,
        created_at: 0,
    }
}

fn event(extra: &str) -> Component {
    let text = format!(
        "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:x\r\nBEGIN:VEVENT\r\nUID:u1\r\nDTSTAMP:20261001T000000Z\r\nDTSTART:20261021T100000Z\r\nDTEND:20261021T110000Z\r\nSUMMARY:Title\r\n{extra}END:VEVENT\r\nEND:VCALENDAR\r\n"
    );
    parse(text.as_bytes()).unwrap().components.remove(0)
}

fn names(component: &Component) -> Vec<&str> {
    component
        .properties
        .iter()
        .map(|p| p.name.as_str())
        .collect()
}

#[test]
fn tokens_are_exactly_64_lower_hex_digits() {
    let token = "a".repeat(64);
    assert_eq!(token_of(&format!("{token}.ics")), Some(token.as_str()));
    for file in [
        "a".repeat(64),
        format!("{}.ics", "A".repeat(64)),
        format!("{}.ics", "a".repeat(63)),
        format!("{}.ics", "a".repeat(65)),
        format!("{}.ics", "g".repeat(64)),
        format!("{}.ICS", "a".repeat(64)),
        ".ics".to_owned(),
    ] {
        assert_eq!(token_of(&file), None, "{file}");
    }
    let (token, hash) = new_token();
    assert!(token_of(&format!("{token}.ics")).is_some());
    assert_eq!(hash, hash_secret(&token));
    assert_ne!(new_token().0, token);
}

#[test]
fn classes_other_than_public_are_private() {
    assert!(!is_private(&event("")));
    assert!(!is_private(&event("CLASS:PUBLIC\r\n")));
    assert!(!is_private(&event("CLASS:public\r\n")));
    for class in ["PRIVATE", "CONFIDENTIAL", "X-SECRET"] {
        assert!(is_private(&event(&format!("CLASS:{class}\r\n"))), "{class}");
    }
}

#[test]
fn busy_blocks_keep_times_only() {
    let full = event(
        "RRULE:FREQ=WEEKLY;COUNT=3\r\nEXDATE:20261028T100000Z\r\nLOCATION:Here\r\nATTENDEE:mailto:a@b\r\nX-SECRET:1\r\n",
    );
    let busy = publish_event(&link(true, PrivateEvents::Show, true), &full).unwrap();
    assert_eq!(
        names(&busy),
        [
            "UID", "DTSTAMP", "DTSTART", "DTEND", "RRULE", "EXDATE", "SUMMARY"
        ]
    );
    assert_eq!(busy.property("SUMMARY").unwrap().value, "Busy");
    assert!(
        busy.property("UID")
            .unwrap()
            .value
            .ends_with("@calcraft-published")
    );
    assert_ne!(busy.property("UID").unwrap().value, "u1");
    for skipped in ["TRANSP:TRANSPARENT\r\n", "STATUS:CANCELLED\r\n"] {
        assert!(publish_event(&link(true, PrivateEvents::Show, true), &event(skipped)).is_none());
    }
}

#[test]
fn full_events_lose_attachments_alarms_and_optionally_addresses() {
    let full = event(
        "ATTACH:https://x/a\r\nORGANIZER:mailto:o@x\r\nATTENDEE:mailto:a@x\r\nBEGIN:VALARM\r\nACTION:DISPLAY\r\nDESCRIPTION:x\r\nTRIGGER:-PT5M\r\nEND:VALARM\r\n",
    );
    let out = publish_event(&link(false, PrivateEvents::Busy, false), &full).unwrap();
    assert_eq!(
        names(&out),
        ["UID", "DTSTAMP", "DTSTART", "DTEND", "SUMMARY"]
    );
    assert!(out.components.is_empty());
    let out = publish_event(&link(false, PrivateEvents::Busy, true), &full).unwrap();
    assert!(names(&out).contains(&"ATTENDEE") && names(&out).contains(&"ORGANIZER"));
    assert!(!names(&out).contains(&"ATTACH"));
    // Private events by the link's choice.
    let private = event("CLASS:PRIVATE\r\nLOCATION:Clinic\r\n");
    let busy = publish_event(&link(false, PrivateEvents::Busy, false), &private).unwrap();
    assert_eq!(busy.property("SUMMARY").unwrap().value, "Busy");
    assert!(busy.property("LOCATION").is_none() && busy.property("CLASS").is_none());
    assert!(publish_event(&link(false, PrivateEvents::Hide, false), &private).is_none());
    let shown = publish_event(&link(false, PrivateEvents::Show, false), &private).unwrap();
    assert_eq!(shown.property("LOCATION").unwrap().value, "Clinic");
}

#[test]
fn urls_come_from_the_configured_base() {
    let config = PublishConfig {
        enabled: true,
        base_url: Some("https://cal.example.org/".to_owned()),
        ..PublishConfig::default()
    };
    let publish = Publish::new(&config);
    let (https, webcal) = publish.urls("t");
    assert_eq!(https, "https://cal.example.org/public/t.ics");
    assert_eq!(webcal, "webcal://cal.example.org/public/t.ics");
}
