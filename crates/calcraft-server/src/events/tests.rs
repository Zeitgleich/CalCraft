use calcraft_core::api::Observance;
use calcraft_ical::{parse, write::to_string};

use super::*;

const NOW: i64 = 1_790_000_000;

const BERLIN: &str = "BEGIN:VTIMEZONE\r\nTZID:Europe/Berlin\r\n\
BEGIN:DAYLIGHT\r\nTZOFFSETFROM:+0100\r\nTZOFFSETTO:+0200\r\nTZNAME:CEST\r\n\
DTSTART:19700329T020000\r\nRRULE:FREQ=YEARLY;BYMONTH=3;BYDAY=-1SU\r\nEND:DAYLIGHT\r\n\
BEGIN:STANDARD\r\nTZOFFSETFROM:+0200\r\nTZOFFSETTO:+0100\r\nTZNAME:CET\r\n\
DTSTART:19701025T030000\r\nRRULE:FREQ=YEARLY;BYMONTH=10;BYDAY=-1SU\r\nEND:STANDARD\r\n\
END:VTIMEZONE\r\n";

fn root(body: &str) -> Component {
    parse(
        format!("BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//T//EN\r\n{body}END:VCALENDAR\r\n")
            .as_bytes(),
    )
    .unwrap()
}

fn berlin_info() -> TimeZoneInfo {
    TimeZoneInfo {
        tzid: "Europe/Berlin".to_owned(),
        observances: vec![
            Observance {
                daylight: true,
                start: "19700329T020000".to_owned(),
                offset_from: "+0100".to_owned(),
                offset_to: "+0200".to_owned(),
                rrule: Some("FREQ=YEARLY;BYMONTH=3;BYDAY=-1SU".to_owned()),
                name: Some("CEST".to_owned()),
            },
            Observance {
                daylight: false,
                start: "19701025T030000".to_owned(),
                offset_from: "+0200".to_owned(),
                offset_to: "+0100".to_owned(),
                rrule: Some("FREQ=YEARLY;BYMONTH=10;BYDAY=-1SU".to_owned()),
                name: Some("CET".to_owned()),
            },
        ],
    }
}

fn time(date: &str, time: Option<&str>, tzid: Option<&str>) -> EventTime {
    EventTime {
        date: date.to_owned(),
        time: time.map(str::to_owned),
        tzid: tzid.map(str::to_owned),
    }
}

fn input(start: EventTime, end: EventTime) -> EventInput {
    EventInput {
        recurrence_id: None,
        fields: EventFields {
            summary: "Planning".to_owned(),
            location: "Room 1".to_owned(),
            description: "Line one\nline; two".to_owned(),
            start,
            end,
            alarms: vec![Alarm { minutes_before: 15 }],
            private: false,
        },
        recurrence: None,
        keep_recurrence: false,
        timezones: vec![berlin_info()],
    }
}

fn text_of(root: &Component) -> String {
    to_string(root).unwrap()
}

fn valid(root: &Component) {
    calcraft_ical::CalendarObject::parse(text_of(root).as_bytes()).unwrap();
}

#[test]
fn new_event_with_zone_and_alarm() {
    let input = input(
        time("2026-10-23", Some("09:00"), Some("Europe/Berlin")),
        time("2026-10-23", Some("10:30"), Some("Europe/Berlin")),
    );
    let root = new_object(&input, "uid-1", NOW).unwrap();
    valid(&root);
    let text = text_of(&root);
    assert!(
        text.contains("BEGIN:VTIMEZONE\r\nTZID:Europe/Berlin"),
        "{text}"
    );
    assert!(
        text.contains("DTSTART;TZID=Europe/Berlin:20261023T090000"),
        "{text}"
    );
    assert!(
        text.contains("DTEND;TZID=Europe/Berlin:20261023T103000"),
        "{text}"
    );
    assert!(
        text.contains("DESCRIPTION:Line one\\nline\\; two"),
        "{text}"
    );
    assert!(text.contains("TRIGGER:-PT15M"), "{text}");
    assert!(!text.contains("SEQUENCE"), "{text}");
    // Read back as the dialog shows it.
    let detail = detail(&root, None).unwrap();
    assert_eq!(detail.uid, "uid-1");
    assert_eq!(detail.fields, input.fields);
}

#[test]
fn unknown_or_bad_zones_are_refused() {
    let mut no_rules = input(
        time("2026-10-23", Some("09:00"), Some("Mars/Olympus")),
        time("2026-10-23", Some("10:00"), Some("Mars/Olympus")),
    );
    no_rules.timezones.clear();
    assert_eq!(
        new_object(&no_rules, "u", NOW),
        Err(EditError::Invalid("unknown time zone"))
    );
    let mut injected = input(
        time("2026-10-23", Some("09:00"), Some("Europe/Berlin\r\nX:1")),
        time("2026-10-23", Some("10:00"), None),
    );
    injected.timezones.clear();
    assert!(new_object(&injected, "u", NOW).is_err());
    let mut bad = input(
        time("2026-10-23", Some("09:00"), Some("Europe/Berlin")),
        time("2026-10-23", Some("10:00"), Some("Europe/Berlin")),
    );
    bad.timezones[0].observances[0].offset_to = "+99:00".to_owned();
    assert!(new_object(&bad, "u", NOW).is_err());
}

#[test]
fn all_day_floating_utc_and_order() {
    let all_day = input(
        time("2026-10-23", None, None),
        time("2026-10-25", None, None),
    );
    let text = text_of(&new_object(&all_day, "u", NOW).unwrap());
    assert!(text.contains("DTSTART;VALUE=DATE:20261023"), "{text}");
    assert!(text.contains("DTEND;VALUE=DATE:20261025"), "{text}");

    let utc = input(
        time("2026-10-23", Some("09:00"), Some("UTC")),
        time("2026-10-23", Some("09:00"), Some("UTC")),
    );
    let text = text_of(&new_object(&utc, "u", NOW).unwrap());
    assert!(text.contains("DTSTART:20261023T090000Z"), "{text}");

    let floating = input(
        time("2026-10-23", Some("09:00"), None),
        time("2026-10-23", Some("08:00"), None),
    );
    assert_eq!(
        new_object(&floating, "u", NOW),
        Err(EditError::Invalid("the event ends before it starts"))
    );
    let mixed = input(
        time("2026-10-23", None, None),
        time("2026-10-23", Some("08:00"), None),
    );
    assert!(new_object(&mixed, "u", NOW).is_err());
    let empty_all_day = input(
        time("2026-10-23", None, None),
        time("2026-10-23", None, None),
    );
    assert!(new_object(&empty_all_day, "u", NOW).is_err());
    for bad in ["2026-13-01", "2026-02-30", "20261023", "2026-1-01"] {
        assert!(
            new_object(
                &input(time(bad, None, None), time(bad, None, None)),
                "u",
                NOW
            )
            .is_err()
        );
    }
}

#[test]
fn keeps_what_the_dialog_does_not_own() {
    let mut root = root(&format!(
        "{BERLIN}BEGIN:VEVENT\r\nUID:x\r\nDTSTAMP:20260101T000000Z\r\n\
         DTSTART;TZID=Europe/Berlin:20261023T090000\r\nDURATION:PT1H\r\nSUMMARY:Old\r\n\
         ORGANIZER;CN=Alice:mailto:alice@example.org\r\n\
         ATTENDEE;CN=Bob;PARTSTAT=ACCEPTED;ROLE=REQ-PARTICIPANT:mailto:bob@example.org\r\n\
         ATTACH;MANAGED-ID=aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa;FILENAME=a.pdf;SIZE=10:https://x/dav/attachments/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\r\n\
         ATTACH:javascript:alert(1)\r\nX-CUSTOM:keep me\r\nSEQUENCE:3\r\n\
         BEGIN:VALARM\r\nACTION:EMAIL\r\nTRIGGER:-PT1H\r\nDESCRIPTION:mail\r\nEND:VALARM\r\n\
         BEGIN:VALARM\r\nACTION:DISPLAY\r\nTRIGGER:-PT10M\r\nDESCRIPTION:pop\r\nEND:VALARM\r\n\
         END:VEVENT\r\n"
    ));
    let detail = detail(&root, None).unwrap();
    assert_eq!(detail.fields.alarms, vec![Alarm { minutes_before: 10 }]);
    assert_eq!(detail.organizer.unwrap().address, "alice@example.org");
    assert_eq!(detail.attendees[0].status.as_deref(), Some("ACCEPTED"));
    assert_eq!(detail.attachments.len(), 1, "only safe links");
    assert_eq!(
        detail.attachments[0].url,
        "/api/v1/attachments/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
    );
    let mut change = input(
        time("2026-10-24", Some("09:00"), Some("Europe/Berlin")),
        time("2026-10-24", Some("10:00"), Some("Europe/Berlin")),
    );
    change.fields.alarms.clear();
    apply(&mut root, &change, NOW).unwrap();
    valid(&root);
    let text = text_of(&root);
    for kept in [
        "ORGANIZER;CN=Alice",
        "ATTENDEE;CN=Bob",
        "MANAGED-ID=aaaa",
        "X-CUSTOM:keep me",
        "ACTION:EMAIL",
        "SEQUENCE:4",
    ] {
        assert!(text.contains(kept), "{kept} missing:\n{text}");
    }
    assert!(!text.contains("ACTION:DISPLAY"), "{text}");
    assert!(!text.contains("DURATION"), "{text}");
    assert_eq!(text.matches("BEGIN:VTIMEZONE").count(), 1);
}

#[test]
fn simple_and_complex_rules() {
    let mut weekly = input(
        time("2026-10-19", Some("09:00"), Some("Europe/Berlin")),
        time("2026-10-19", Some("10:00"), Some("Europe/Berlin")),
    );
    weekly.recurrence = Some(Recurrence {
        freq: Frequency::Weekly,
        interval: 2,
        count: None,
        until: Some("2026-12-31".to_owned()),
        weekdays: vec!["WE".to_owned(), "MO".to_owned()],
    });
    let root_weekly = new_object(&weekly, "u", NOW).unwrap();
    let text = text_of(&root_weekly);
    // UNTIL is UTC for a zoned start: the end of the day in Berlin.
    assert!(
        text.contains("RRULE:FREQ=WEEKLY;INTERVAL=2;UNTIL=20261231T225959Z;BYDAY=MO,WE"),
        "{text}"
    );
    let read = detail(&root_weekly, None).unwrap();
    let mut expected = weekly.recurrence.clone().unwrap();
    expected.weekdays = vec!["MO".to_owned(), "WE".to_owned()];
    assert_eq!(read.recurrence, Some(expected));

    let complex = root(
        "BEGIN:VEVENT\r\nUID:c\r\nDTSTAMP:20260101T000000Z\r\nDTSTART:20261001T090000Z\r\n\
         RRULE:FREQ=MONTHLY;BYDAY=2TU\r\nEND:VEVENT\r\n",
    );
    let read = detail(&complex, None).unwrap();
    assert_eq!(read.recurrence, None);
    assert_eq!(
        read.recurrence_rule.as_deref(),
        Some("FREQ=MONTHLY;BYDAY=2TU")
    );
    // Kept on edit when asked to.
    let mut keep = input(
        time("2026-10-01", Some("10:00"), Some("UTC")),
        time("2026-10-01", Some("11:00"), Some("UTC")),
    );
    keep.keep_recurrence = true;
    let mut changed = complex.clone();
    apply(&mut changed, &keep, NOW).unwrap();
    assert!(text_of(&changed).contains("RRULE:FREQ=MONTHLY;BYDAY=2TU"));

    let mut bad = weekly.clone();
    bad.recurrence.as_mut().unwrap().count = Some(3);
    assert!(new_object(&bad, "u", NOW).is_err(), "count and until");
    let mut bad = weekly.clone();
    bad.recurrence.as_mut().unwrap().weekdays = vec!["XX".to_owned()];
    assert!(new_object(&bad, "u", NOW).is_err());
    let mut bad = weekly;
    bad.recurrence.as_mut().unwrap().interval = 0;
    assert!(new_object(&bad, "u", NOW).is_err());
}

fn daily_series() -> Component {
    root(&format!(
        "{BERLIN}BEGIN:VEVENT\r\nUID:s\r\nDTSTAMP:20260101T000000Z\r\n\
         DTSTART;TZID=Europe/Berlin:20261020T090000\r\n\
         DTEND;TZID=Europe/Berlin:20261020T100000\r\nRRULE:FREQ=DAILY;COUNT=10\r\n\
         SUMMARY:Standup\r\nEXDATE;TZID=Europe/Berlin:20261022T090000\r\nEND:VEVENT\r\n"
    ))
}

#[test]
fn one_instance_becomes_an_override() {
    let mut root = daily_series();
    let mut change = input(
        time("2026-10-23", Some("14:00"), Some("Europe/Berlin")),
        time("2026-10-23", Some("15:00"), Some("Europe/Berlin")),
    );
    change.recurrence_id = Some("20261023T090000".to_owned());
    change.fields.summary = "Moved standup".to_owned();
    apply(&mut root, &change, NOW).unwrap();
    valid(&root);
    let text = text_of(&root);
    assert!(
        text.contains("RECURRENCE-ID;TZID=Europe/Berlin:20261023T090000"),
        "{text}"
    );
    assert!(
        text.contains("RRULE:FREQ=DAILY;COUNT=10"),
        "master keeps the rule"
    );
    assert_eq!(text.matches("RRULE:FREQ=DAILY").count(), 1, "{text}");
    // Reading the instance gives the override; the series is unchanged.
    let instance = detail(&root, Some("20261023T090000")).unwrap();
    assert!(instance.is_override);
    assert_eq!(instance.fields.summary, "Moved standup");
    assert_eq!(detail(&root, None).unwrap().fields.summary, "Standup");
    // A second change updates the same override.
    change.fields.summary = "Again".to_owned();
    apply(&mut root, &change, NOW).unwrap();
    assert_eq!(text_of(&root).matches("RECURRENCE-ID").count(), 1);
    // An instance the series does not have.
    change.recurrence_id = Some("20261022T090000".to_owned());
    assert_eq!(apply(&mut root, &change, NOW), Err(EditError::NotFound));
    change.recurrence_id = Some("20261023T091500".to_owned());
    assert_eq!(apply(&mut root, &change, NOW), Err(EditError::NotFound));
}

#[test]
fn unchanged_instance_reads_like_the_series() {
    let root = daily_series();
    let instance = detail(&root, Some("20261024T090000")).unwrap();
    assert!(!instance.is_override);
    assert_eq!(
        instance.fields.start,
        time("2026-10-24", Some("09:00"), Some("Europe/Berlin"))
    );
    assert_eq!(
        instance.fields.end,
        time("2026-10-24", Some("10:00"), Some("Europe/Berlin"))
    );
}

#[test]
fn moving_the_series_shifts_exdates_and_overrides() {
    let mut root = daily_series();
    let mut one = input(
        time("2026-10-25", Some("12:00"), Some("Europe/Berlin")),
        time("2026-10-25", Some("13:00"), Some("Europe/Berlin")),
    );
    one.recurrence_id = Some("20261025T090000".to_owned());
    apply(&mut root, &one, NOW).unwrap();
    let mut series = input(
        time("2026-10-20", Some("10:30"), Some("Europe/Berlin")),
        time("2026-10-20", Some("11:00"), Some("Europe/Berlin")),
    );
    series.keep_recurrence = true;
    apply(&mut root, &series, NOW).unwrap();
    valid(&root);
    let text = text_of(&root);
    assert!(
        text.contains("EXDATE;TZID=Europe/Berlin:20261022T103000"),
        "{text}"
    );
    assert!(
        text.contains("RECURRENCE-ID;TZID=Europe/Berlin:20261025T103000"),
        "{text}"
    );
    assert!(override_index(&root, "20261025T103000").is_some());
    // Now all-day: the instances cannot be named any more.
    let mut all_day = input(
        time("2026-10-20", None, None),
        time("2026-10-21", None, None),
    );
    all_day.keep_recurrence = true;
    apply(&mut root, &all_day, NOW).unwrap();
    let text = text_of(&root);
    assert!(!text.contains("EXDATE"), "{text}");
    assert!(!text.contains("RECURRENCE-ID"), "{text}");
    // No rule: no longer recurring.
    let single = input(
        time("2026-10-20", None, None),
        time("2026-10-21", None, None),
    );
    apply(&mut root, &single, NOW).unwrap();
    assert!(!is_recurring(&root));
}

#[test]
fn drag_and_drop_times() {
    let mut series = daily_series();
    // 2026-10-26 is after the switch to CET: 09:00 Berlin = 08:00 UTC.
    let moved = EventTimes {
        recurrence_id: Some("20261026T090000".to_owned()),
        start: 1_793_000_000 - 1_793_000_000 % 3600,
        end: 1_793_000_000 - 1_793_000_000 % 3600 + 1800,
    };
    apply_times(&mut series, &moved, NOW).unwrap();
    valid(&series);
    let index = override_index(&series, "20261026T090000").unwrap();
    let start = series.components[index].property("DTSTART").unwrap();
    assert_eq!(tzid(start), Some("Europe/Berlin"));
    let local = parse_date_or_datetime(&start.value).unwrap().seconds;
    assert_eq!(
        zone_to_utc(&series, "Europe/Berlin", local),
        Some(moved.start)
    );

    // All-day events move by whole days.
    let mut all_day = root(
        "BEGIN:VEVENT\r\nUID:d\r\nDTSTAMP:20260101T000000Z\r\nDTSTART;VALUE=DATE:20261023\r\n\
         END:VEVENT\r\n",
    );
    let day = parse_date_or_datetime("20261030").unwrap().seconds;
    let bad = EventTimes {
        recurrence_id: None,
        start: day + 3600,
        end: day + DAY,
    };
    assert!(apply_times(&mut all_day, &bad, NOW).is_err());
    let good = EventTimes {
        recurrence_id: None,
        start: day,
        end: day + 2 * DAY,
    };
    apply_times(&mut all_day, &good, NOW).unwrap();
    let text = text_of(&all_day);
    assert!(text.contains("DTSTART;VALUE=DATE:20261030"), "{text}");
    assert!(text.contains("DTEND;VALUE=DATE:20261101"), "{text}");
    assert!(text.contains("SEQUENCE:1"), "{text}");
}

#[test]
fn excluding_instances() {
    let mut root = daily_series();
    let mut one = input(
        time("2026-10-25", Some("12:00"), Some("Europe/Berlin")),
        time("2026-10-25", Some("13:00"), Some("Europe/Berlin")),
    );
    one.recurrence_id = Some("20261025T090000".to_owned());
    apply(&mut root, &one, NOW).unwrap();
    exclude(&mut root, "20261025T090000", NOW).unwrap();
    exclude(&mut root, "20261021T090000", NOW).unwrap();
    valid(&root);
    let text = text_of(&root);
    assert!(!text.contains("RECURRENCE-ID"), "{text}");
    assert!(
        text.contains("EXDATE;TZID=Europe/Berlin:20261025T090000"),
        "{text}"
    );
    assert!(
        text.contains("EXDATE;TZID=Europe/Berlin:20261021T090000"),
        "{text}"
    );
    assert_eq!(
        exclude(&mut root, "20261021T090000", NOW),
        Err(EditError::NotFound)
    );
}

#[test]
fn private_class_and_limits() {
    let mut private = input(
        time("2026-10-23", None, None),
        time("2026-10-24", None, None),
    );
    private.fields.private = true;
    let mut root = new_object(&private, "u", NOW).unwrap();
    assert!(text_of(&root).contains("CLASS:PRIVATE"));
    private.fields.private = false;
    apply(&mut root, &private, NOW).unwrap();
    assert!(text_of(&root).contains("CLASS:PUBLIC"));

    let mut long = private.clone();
    long.fields.summary = "x".repeat(MAX_SHORT_TEXT + 1);
    assert!(apply(&mut root, &long, NOW).is_err());
    let mut alarms = private.clone();
    alarms.fields.alarms = vec![Alarm { minutes_before: -5 }];
    assert!(apply(&mut root, &alarms, NOW).is_err());
    alarms.fields.alarms = vec![Alarm { minutes_before: 1 }; MAX_ALARMS + 1];
    assert!(apply(&mut root, &alarms, NOW).is_err());
    // Control characters cannot create new content lines.
    let mut injected = private;
    injected.fields.summary = "a\r\nATTENDEE:mailto:evil@example.org".to_owned();
    apply(&mut root, &injected, NOW).unwrap();
    let text = text_of(&root);
    assert!(!text.contains("\r\nATTENDEE"), "{text}");
}
