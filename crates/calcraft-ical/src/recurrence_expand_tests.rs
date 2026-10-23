use super::*;
use crate::{datetime::parse_utc_datetime, parse};

const BERLIN: &str = "BEGIN:VTIMEZONE\r\nTZID:Europe/Berlin\r\n\
BEGIN:DAYLIGHT\r\nTZOFFSETFROM:+0100\r\nTZOFFSETTO:+0200\r\nTZNAME:CEST\r\n\
DTSTART:19700329T020000\r\nRRULE:FREQ=YEARLY;BYMONTH=3;BYDAY=-1SU\r\nEND:DAYLIGHT\r\n\
BEGIN:STANDARD\r\nTZOFFSETFROM:+0200\r\nTZOFFSETTO:+0100\r\nTZNAME:CET\r\n\
DTSTART:19701025T030000\r\nRRULE:FREQ=YEARLY;BYMONTH=10;BYDAY=-1SU\r\nEND:STANDARD\r\n\
END:VTIMEZONE\r\n";

fn utc(value: &str) -> i64 {
    parse_utc_datetime(value).unwrap()
}

fn local(value: &str) -> i64 {
    crate::parse_date_or_datetime(value).unwrap().seconds
}

fn root(body: &str) -> Component {
    parse(
        format!("BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//T//EN\r\n{body}END:VCALENDAR\r\n")
            .as_bytes(),
    )
    .unwrap()
}

fn event(props: &str) -> String {
    format!("BEGIN:VEVENT\r\nUID:e\r\nDTSTAMP:20260101T000000Z\r\n{props}END:VEVENT\r\n")
}

fn expand(body: &str, from: &str, to: &str) -> Expanded {
    instances(
        &root(body),
        utc(from),
        utc(to),
        &mut Budget::new(crate::OBJECT_BUDGET),
        1000,
    )
    .unwrap()
}

#[test]
fn single_utc_event() {
    let found = expand(
        &event("DTSTART:20261023T090000Z\r\nDTEND:20261023T100000Z\r\n"),
        "20261023T000000Z",
        "20261024T000000Z",
    );
    assert!(found.complete);
    assert_eq!(
        found.instances,
        vec![Instance {
            component: 0,
            start: InstanceTime::Utc(utc("20261023T090000Z")),
            end: InstanceTime::Utc(utc("20261023T100000Z")),
            recurrence_id: None,
        }]
    );
    // Outside the range: nothing.
    let none = expand(
        &event("DTSTART:20261023T090000Z\r\nDTEND:20261023T100000Z\r\n"),
        "20261024T000000Z",
        "20261025T000000Z",
    );
    assert!(none.instances.is_empty());
}

#[test]
fn weekly_series_keeps_local_time_across_dst() {
    // 09:00 Berlin every week: 07:00 UTC in summer, 08:00 UTC in winter.
    let body = format!(
        "{BERLIN}{}",
        event(
            "DTSTART;TZID=Europe/Berlin:20261018T090000\r\n\
             DTEND;TZID=Europe/Berlin:20261018T100000\r\nRRULE:FREQ=WEEKLY;COUNT=3\r\n"
        )
    );
    let found = expand(&body, "20261001T000000Z", "20261201T000000Z");
    let starts: Vec<_> = found.instances.iter().map(|i| i.start).collect();
    assert_eq!(
        starts,
        vec![
            InstanceTime::Utc(utc("20261018T070000Z")),
            InstanceTime::Utc(utc("20261025T080000Z")),
            InstanceTime::Utc(utc("20261101T080000Z")),
        ]
    );
    assert_eq!(
        found.instances[1].recurrence_id.as_deref(),
        Some("20261025T090000")
    );
    assert_eq!(
        found.instances[1].end,
        InstanceTime::Utc(utc("20261025T090000Z"))
    );
}

#[test]
fn all_day_and_floating() {
    let found = expand(
        &event("DTSTART;VALUE=DATE:20261023\r\nDTEND;VALUE=DATE:20261025\r\n"),
        "20261020T000000Z",
        "20261030T000000Z",
    );
    assert_eq!(
        found.instances[0].start,
        InstanceTime::Date(local("20261023"))
    );
    assert_eq!(
        found.instances[0].end,
        InstanceTime::Date(local("20261025"))
    );

    let found = expand(
        &event("DTSTART;VALUE=DATE:20261023\r\n"),
        "20261020T000000Z",
        "20261030T000000Z",
    );
    assert_eq!(
        found.instances[0].end,
        InstanceTime::Date(local("20261024"))
    );

    // Floating, and a TZID without VTIMEZONE: local time.
    for start in [
        "DTSTART:20261023T090000",
        "DTSTART;TZID=Nowhere/Else:20261023T090000",
    ] {
        let found = expand(
            &event(&format!("{start}\r\nDURATION:PT30M\r\n")),
            "20261023T000000Z",
            "20261024T000000Z",
        );
        assert_eq!(
            found.instances[0].start,
            InstanceTime::Floating(local("20261023T090000"))
        );
        assert_eq!(
            found.instances[0].end,
            InstanceTime::Floating(local("20261023T093000"))
        );
    }
}

#[test]
fn overrides_and_exdates() {
    let body = format!(
        "{}{}",
        event(
            "DTSTART:20261001T090000Z\r\nDURATION:PT1H\r\nRRULE:FREQ=DAILY;COUNT=5\r\n\
             EXDATE:20261002T090000Z\r\n"
        ),
        event(
            "RECURRENCE-ID:20261003T090000Z\r\nDTSTART:20261003T150000Z\r\n\
             DURATION:PT2H\r\nSUMMARY:Moved\r\n"
        )
    );
    let found = expand(&body, "20261001T000000Z", "20261010T000000Z");
    let got: Vec<_> = found
        .instances
        .iter()
        .map(|i| (i.component, i.start, i.recurrence_id.clone()))
        .collect();
    assert_eq!(
        got,
        vec![
            (
                0,
                InstanceTime::Utc(utc("20261001T090000Z")),
                Some("20261001T090000Z".to_owned())
            ),
            (
                0,
                InstanceTime::Utc(utc("20261004T090000Z")),
                Some("20261004T090000Z".to_owned())
            ),
            (
                0,
                InstanceTime::Utc(utc("20261005T090000Z")),
                Some("20261005T090000Z".to_owned())
            ),
            (
                1,
                InstanceTime::Utc(utc("20261003T150000Z")),
                Some("20261003T090000Z".to_owned())
            ),
        ]
    );
}

#[test]
fn limits() {
    let body = event("DTSTART:20260101T090000Z\r\nDURATION:PT1H\r\nRRULE:FREQ=DAILY\r\n");
    let found = instances(
        &root(&body),
        utc("20260101T000000Z"),
        utc("20270101T000000Z"),
        &mut Budget::new(crate::OBJECT_BUDGET),
        10,
    )
    .unwrap();
    assert_eq!(found.instances.len(), 10);
    assert!(!found.complete);
    // An exhausted budget: no answer instead of a wrong one.
    assert!(
        instances(
            &root(&body),
            utc("20260101T000000Z"),
            utc("20270101T000000Z"),
            &mut Budget::new(10),
            1000,
        )
        .is_none()
    );
    // Open-ended rules far in the future are skipped to, not walked.
    let found = expand(&body, "20990101T000000Z", "20990102T000000Z");
    assert_eq!(found.instances.len(), 1);
}

#[test]
fn instance_ids() {
    let body = format!(
        "{BERLIN}{}{}",
        event(
            "DTSTART;TZID=Europe/Berlin:20261018T090000\r\nDURATION:PT1H\r\n\
             RRULE:FREQ=WEEKLY;COUNT=3\r\nEXDATE;TZID=Europe/Berlin:20261101T090000\r\n"
        ),
        event("RECURRENCE-ID;TZID=Europe/Berlin:20261025T090000\r\nDTSTART:20261025T120000Z\r\n")
    );
    let root = root(&body);
    assert_eq!(
        master_instance_id(&root, "20261025T090000").as_deref(),
        Some("20261025T090000")
    );
    // The same instant in UTC names the same instance.
    assert_eq!(
        master_instance_id(&root, "20261025T080000Z").as_deref(),
        Some("20261025T090000")
    );
    assert_eq!(master_instance_id(&root, "20261025T100000"), None);
    // Excluded.
    assert_eq!(master_instance_id(&root, "20261101T090000"), None);
    assert_eq!(override_index(&root, "20261025T090000"), Some(2));
    assert_eq!(override_index(&root, "20261025T080000Z"), Some(2));
    assert_eq!(override_index(&root, "20261018T090000"), None);
    assert_eq!(
        zone_to_utc(&root, "Europe/Berlin", local("20261025T090000")),
        Some(utc("20261025T080000Z"))
    );
    assert_eq!(
        zone_to_local(&root, "Europe/Berlin", utc("20260701T070000Z")),
        Some(local("20260701T090000"))
    );
    assert_eq!(zone_to_utc(&root, "Nowhere", 0), None);
}
