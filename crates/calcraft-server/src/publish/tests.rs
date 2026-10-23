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
fn busy_blocks_keep_times_only() {
    let full = event(
        "RRULE:FREQ=WEEKLY;COUNT=3\r\nEXDATE:20261028T100000Z\r\nLOCATION:Here\r\nATTENDEE:mailto:a@b\r\nX-SECRET:1\r\n",
    );
    let busy = publish_event(&link(true, PrivateEvents::Show, true), &full, false).unwrap();
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
        assert!(
            publish_event(
                &link(true, PrivateEvents::Show, true),
                &event(skipped),
                false
            )
            .is_none()
        );
    }
}

#[test]
fn full_events_lose_attachments_alarms_and_optionally_addresses() {
    let full = event(
        "ATTACH:https://x/a\r\nORGANIZER:mailto:o@x\r\nATTENDEE:mailto:a@x\r\nBEGIN:VALARM\r\nACTION:DISPLAY\r\nDESCRIPTION:x\r\nTRIGGER:-PT5M\r\nEND:VALARM\r\n",
    );
    let out = publish_event(&link(false, PrivateEvents::Busy, false), &full, false).unwrap();
    assert_eq!(
        names(&out),
        ["UID", "DTSTAMP", "DTSTART", "DTEND", "SUMMARY"]
    );
    assert!(out.components.is_empty());
    let out = publish_event(&link(false, PrivateEvents::Busy, true), &full, false).unwrap();
    assert!(names(&out).contains(&"ATTENDEE") && names(&out).contains(&"ORGANIZER"));
    assert!(!names(&out).contains(&"ATTACH"));
    // Private events by the link's choice.
    let private = event("CLASS:PRIVATE\r\nLOCATION:Clinic\r\n");
    let busy = publish_event(&link(false, PrivateEvents::Busy, false), &private, false).unwrap();
    assert_eq!(busy.property("SUMMARY").unwrap().value, "Busy");
    assert!(busy.property("LOCATION").is_none() && busy.property("CLASS").is_none());
    assert!(publish_event(&link(false, PrivateEvents::Hide, false), &private, false).is_none());
    let shown = publish_event(&link(false, PrivateEvents::Show, false), &private, false).unwrap();
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

#[test]
fn full_events_are_a_whitelist() {
    let full = event(
        "DESCRIPTION:Agenda\r\nRELATED-TO:parent-uid@x\r\nURL:https://meet.example/secret\r\nCONFERENCE;VALUE=URI:https://c.example\r\nCONTACT:Jim\r\nCOMMENT:note\r\nX-MS-OLK:1\r\nCATEGORIES:Work\r\n",
    );
    let out = publish_event(&link(false, PrivateEvents::Busy, true), &full, false).unwrap();
    assert_eq!(
        names(&out),
        [
            "UID",
            "DTSTAMP",
            "DTSTART",
            "DTEND",
            "SUMMARY",
            "DESCRIPTION",
            "CATEGORIES"
        ]
    );
}

fn tree(body: &str) -> Component {
    parse(
        format!("BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:x\r\n{body}END:VCALENDAR\r\n").as_bytes(),
    )
    .unwrap()
}

const MASTER: &str = "BEGIN:VEVENT\r\nUID:s\r\nDTSTAMP:20261001T000000Z\r\nDTSTART;TZID=Europe/Berlin:20261021T100000\r\nDURATION:PT1H\r\nRRULE:FREQ=WEEKLY;COUNT=3\r\nSUMMARY:Series\r\nEND:VEVENT\r\n";

#[test]
fn overrides_inherit_class_and_left_out_ones_become_exdates() {
    // A private master: its override without CLASS is private too.
    let private = MASTER.replace("SUMMARY:Series", "SUMMARY:Series\r\nCLASS:PRIVATE");
    let moved = "BEGIN:VEVENT\r\nUID:s\r\nDTSTAMP:20261001T000000Z\r\nRECURRENCE-ID;TZID=Europe/Berlin:20261028T100000\r\nDTSTART;TZID=Europe/Berlin:20261028T150000\r\nDURATION:PT1H\r\nSUMMARY:Moved\r\nEND:VEVENT\r\n";
    let out = publish_object(
        &link(false, PrivateEvents::Busy, false),
        &tree(&format!("{private}{moved}")),
    );
    assert_eq!(out.len(), 2);
    assert!(
        out.iter()
            .all(|c| c.property("SUMMARY").unwrap().value == "Busy")
    );
    // Hidden: the whole series goes.
    let out = publish_object(
        &link(false, PrivateEvents::Hide, false),
        &tree(&format!("{private}{moved}")),
    );
    assert!(out.is_empty());
    // A cancelled instance in a busy-only link: excluded from the master,
    // not left busy at its original time.
    let cancelled = moved.replace("SUMMARY:Moved", "SUMMARY:Moved\r\nSTATUS:CANCELLED");
    let out = publish_object(
        &link(true, PrivateEvents::Show, false),
        &tree(&format!("{MASTER}{cancelled}")),
    );
    assert_eq!(out.len(), 1);
    let exdate = out[0].property("EXDATE").unwrap();
    assert_eq!(exdate.value, "20261028T100000");
    assert_eq!(exdate.parameter("TZID").unwrap().values, ["Europe/Berlin"]);
    // A hidden private override of a public series: excluded as well.
    let secret = moved.replace("SUMMARY:Moved", "SUMMARY:Moved\r\nCLASS:CONFIDENTIAL");
    let out = publish_object(
        &link(false, PrivateEvents::Hide, false),
        &tree(&format!("{MASTER}{secret}")),
    );
    assert_eq!(out.len(), 1);
    assert!(out[0].property("EXDATE").is_some());
    assert!(out[0].properties.iter().all(|p| p.value != "Moved"));
}
