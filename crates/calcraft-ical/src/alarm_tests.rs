use super::*;
use crate::{OBJECT_BUDGET, parse};

const BERLIN: &str = "BEGIN:VTIMEZONE\r\nTZID:Europe/Berlin\r\n\
BEGIN:DAYLIGHT\r\nTZOFFSETFROM:+0100\r\nTZOFFSETTO:+0200\r\nTZNAME:CEST\r\n\
DTSTART:19700329T020000\r\nRRULE:FREQ=YEARLY;BYMONTH=3;BYDAY=-1SU\r\nEND:DAYLIGHT\r\n\
BEGIN:STANDARD\r\nTZOFFSETFROM:+0200\r\nTZOFFSETTO:+0100\r\nTZNAME:CET\r\n\
DTSTART:19701025T030000\r\nRRULE:FREQ=YEARLY;BYMONTH=10;BYDAY=-1SU\r\nEND:STANDARD\r\n\
END:VTIMEZONE\r\n";

fn object(body: &str) -> Component {
    let text = format!(
        "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//test//EN\r\n{BERLIN}{}END:VCALENDAR\r\n",
        body.replace('\n', "\r\n").replace("\r\r\n", "\r\n")
    );
    parse(text.as_bytes()).unwrap()
}

fn utc(value: &str) -> i64 {
    crate::parse_utc_datetime(value).unwrap()
}

fn triggers(root: &Component, after: &str, until: &str, selection: Selection) -> Vec<i64> {
    let mut budget = Budget::new(OBJECT_BUDGET);
    alarm_occurrences(root, utc(after), utc(until), None, selection, &mut budget)
        .unwrap()
        .iter()
        .map(|o| o.trigger)
        .collect()
}

fn all(root: &Component, after: &str, until: &str) -> Vec<i64> {
    triggers(root, after, until, Selection::All)
}

#[test]
fn relative_trigger_before_start() {
    let root = object(
        "BEGIN:VEVENT\nUID:a\nDTSTAMP:20260101T000000Z\nDTSTART:20261026T090000Z\nDTEND:20261026T100000Z\n\
BEGIN:VALARM\nACTION:DISPLAY\nTRIGGER:-PT15M\nDESCRIPTION:x\nEND:VALARM\nEND:VEVENT\n",
    );
    assert_eq!(
        all(&root, "20261026T000000Z", "20261027T000000Z"),
        vec![utc("20261026T084500Z")]
    );
    // The window is (after, until]: exactly at `after` is not included.
    assert!(all(&root, "20261026T084500Z", "20261027T000000Z").is_empty());
    assert_eq!(
        all(&root, "20261026T084459Z", "20261026T084500Z"),
        vec![utc("20261026T084500Z")]
    );
}

#[test]
fn related_end_and_positive_offsets() {
    let root = object(
        "BEGIN:VEVENT\nUID:a\nDTSTAMP:20260101T000000Z\nDTSTART:20261026T090000Z\nDURATION:PT2H\n\
BEGIN:VALARM\nACTION:EMAIL\nTRIGGER;RELATED=END:PT5M\nSUMMARY:s\nDESCRIPTION:d\nATTENDEE:mailto:x@example.com\nEND:VALARM\n\
END:VEVENT\n",
    );
    assert_eq!(
        all(&root, "20261026T000000Z", "20261027T000000Z"),
        vec![utc("20261026T110500Z")]
    );
}

#[test]
fn absolute_trigger_fires_once_even_for_series() {
    let root = object(
        "BEGIN:VEVENT\nUID:a\nDTSTAMP:20260101T000000Z\nDTSTART:20261026T090000Z\nRRULE:FREQ=DAILY\n\
BEGIN:VALARM\nACTION:DISPLAY\nTRIGGER;VALUE=DATE-TIME:20261101T070000Z\nEND:VALARM\nEND:VEVENT\n",
    );
    let found = all(&root, "20261001T000000Z", "20261201T000000Z");
    assert_eq!(found, vec![utc("20261101T070000Z")]);
}

#[test]
fn absolute_trigger_in_local_time_is_ignored() {
    let root = object(
        "BEGIN:VEVENT\nUID:a\nDTSTAMP:20260101T000000Z\nDTSTART:20261026T090000Z\n\
BEGIN:VALARM\nACTION:DISPLAY\nTRIGGER;VALUE=DATE-TIME:20261026T070000\nEND:VALARM\nEND:VEVENT\n",
    );
    assert!(all(&root, "20261001T000000Z", "20261201T000000Z").is_empty());
}

#[test]
fn repeat_and_duration() {
    let root = object(
        "BEGIN:VEVENT\nUID:a\nDTSTAMP:20260101T000000Z\nDTSTART:20261026T090000Z\n\
BEGIN:VALARM\nACTION:DISPLAY\nTRIGGER:-PT30M\nREPEAT:2\nDURATION:PT10M\nEND:VALARM\nEND:VEVENT\n",
    );
    assert_eq!(
        all(&root, "20261026T000000Z", "20261027T000000Z"),
        vec![
            utc("20261026T083000Z"),
            utc("20261026T084000Z"),
            utc("20261026T085000Z")
        ]
    );
    // REPEAT without DURATION, and absurd repetitions, are bounded.
    let root = object(
        "BEGIN:VEVENT\nUID:a\nDTSTAMP:20260101T000000Z\nDTSTART:20261026T090000Z\n\
BEGIN:VALARM\nACTION:DISPLAY\nTRIGGER:-PT30M\nREPEAT:5\nEND:VALARM\n\
BEGIN:VALARM\nACTION:DISPLAY\nTRIGGER:PT0S\nREPEAT:4000000000\nDURATION:PT1M\nEND:VALARM\n\
BEGIN:VALARM\nACTION:DISPLAY\nTRIGGER:PT1H\nREPEAT:10\nDURATION:PT1S\nEND:VALARM\nEND:VEVENT\n",
    );
    let found = all(&root, "20261026T000000Z", "20261027T000000Z");
    assert_eq!(found.len(), 1 + 1 + MAX_REPEAT as usize + 1);
}

#[test]
fn selection_filters_actions() {
    let root = object(
        "BEGIN:VEVENT\nUID:a\nDTSTAMP:20260101T000000Z\nDTSTART:20261026T090000Z\n\
BEGIN:VALARM\nACTION:DISPLAY\nTRIGGER:-PT15M\nEND:VALARM\n\
BEGIN:VALARM\nACTION:AUDIO\nTRIGGER:-PT10M\nEND:VALARM\n\
BEGIN:VALARM\nACTION:PROCEDURE\nTRIGGER:-PT9M\nEND:VALARM\n\
BEGIN:VALARM\nACTION:EMAIL\nTRIGGER:-PT5M\nEND:VALARM\nEND:VEVENT\n",
    );
    assert_eq!(
        triggers(
            &root,
            "20261026T000000Z",
            "20261027T000000Z",
            Selection::EmailOnly
        ),
        vec![utc("20261026T085500Z")]
    );
    assert_eq!(all(&root, "20261026T000000Z", "20261027T000000Z").len(), 3);
}

#[test]
fn same_moment_counts_once_with_email_kept() {
    let root = object(
        "BEGIN:VEVENT\nUID:a\nDTSTAMP:20260101T000000Z\nDTSTART:20261026T090000Z\n\
BEGIN:VALARM\nACTION:DISPLAY\nTRIGGER:-PT15M\nEND:VALARM\n\
BEGIN:VALARM\nACTION:EMAIL\nTRIGGER:-PT15M\nEND:VALARM\nEND:VEVENT\n",
    );
    let mut budget = Budget::new(OBJECT_BUDGET);
    let found = alarm_occurrences(
        &root,
        utc("20261026T000000Z"),
        utc("20261027T000000Z"),
        None,
        Selection::All,
        &mut budget,
    )
    .unwrap();
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].action, AlarmAction::Email);
}

#[test]
fn recurring_with_override_and_exdate() {
    let root = object(
        "BEGIN:VEVENT\nUID:a\nDTSTAMP:20260101T000000Z\nDTSTART;TZID=Europe/Berlin:20261020T090000\n\
DTEND;TZID=Europe/Berlin:20261020T100000\nRRULE:FREQ=DAILY;COUNT=10\n\
EXDATE;TZID=Europe/Berlin:20261022T090000\n\
BEGIN:VALARM\nACTION:DISPLAY\nTRIGGER:-PT15M\nEND:VALARM\nEND:VEVENT\n\
BEGIN:VEVENT\nUID:a\nDTSTAMP:20260101T000000Z\nRECURRENCE-ID;TZID=Europe/Berlin:20261023T090000\n\
DTSTART;TZID=Europe/Berlin:20261023T140000\nDTEND;TZID=Europe/Berlin:20261023T150000\n\
SUMMARY:moved\nBEGIN:VALARM\nACTION:DISPLAY\nTRIGGER:-PT1H\nEND:VALARM\nEND:VEVENT\n",
    );
    let mut budget = Budget::new(OBJECT_BUDGET);
    let found = alarm_occurrences(
        &root,
        utc("20261021T000000Z"),
        utc("20261024T000000Z"),
        None,
        Selection::All,
        &mut budget,
    )
    .unwrap();
    let times: Vec<i64> = found.iter().map(|o| o.trigger).collect();
    // 21st 08:45 CEST = 06:45Z; 22nd excluded; 23rd overridden: 14:00 CEST - 1 h = 11:00Z.
    assert_eq!(
        times,
        vec![utc("20261021T064500Z"), utc("20261023T110000Z")]
    );
    // The override's component is reported for its instance.
    assert_eq!(found[1].component, 2);
    assert_eq!(found[1].instance.start, utc("20261023T120000Z"));
}

#[test]
fn daylight_saving_time_change() {
    // Berlin leaves summer time on 2026-10-25: 09:00 local is 07:00Z
    // before and 08:00Z after.
    let root = object(
        "BEGIN:VEVENT\nUID:a\nDTSTAMP:20260101T000000Z\nDTSTART;TZID=Europe/Berlin:20261024T090000\n\
RRULE:FREQ=DAILY;COUNT=3\nBEGIN:VALARM\nACTION:DISPLAY\nTRIGGER:-PT10M\nEND:VALARM\nEND:VEVENT\n",
    );
    assert_eq!(
        all(&root, "20261024T000000Z", "20261027T000000Z"),
        vec![
            utc("20261024T065000Z"),
            utc("20261025T075000Z"),
            utc("20261026T075000Z")
        ]
    );
}

#[test]
fn floating_and_all_day_use_the_calendar_time_zone() {
    let root = object(
        "BEGIN:VEVENT\nUID:a\nDTSTAMP:20260101T000000Z\nDTSTART;VALUE=DATE:20261026\n\
BEGIN:VALARM\nACTION:DISPLAY\nTRIGGER:-PT15H\nEND:VALARM\nEND:VEVENT\n",
    );
    // Without a calendar time zone: as UTC (09:00Z on the day before).
    assert_eq!(
        all(&root, "20261025T000000Z", "20261026T000000Z"),
        vec![utc("20261025T090000Z")]
    );
    let zone = calendar_timezone(&format!(
        "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:x\r\n{BERLIN}END:VCALENDAR\r\n"
    ))
    .unwrap();
    let mut budget = Budget::new(OBJECT_BUDGET);
    let found = alarm_occurrences(
        &root,
        utc("20261025T000000Z"),
        utc("20261026T000000Z"),
        Some(&zone),
        Selection::All,
        &mut budget,
    )
    .unwrap();
    // Midnight Berlin (CET after the change) is 23:00Z; 15 h before: 08:00Z.
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].trigger, utc("20261025T080000Z"));
    assert!(found[0].instance.is_date);
}

#[test]
fn unknown_tzid_without_vtimezone_has_no_alarms() {
    let root = object(
        "BEGIN:VEVENT\nUID:a\nDTSTAMP:20260101T000000Z\nDTSTART;TZID=Mars/Olympus:20261026T090000\n\
BEGIN:VALARM\nACTION:DISPLAY\nTRIGGER:-PT15M\nEND:VALARM\nEND:VEVENT\n",
    );
    assert!(all(&root, "20261025T000000Z", "20261028T000000Z").is_empty());
}

#[test]
fn acknowledged_alarms_are_skipped() {
    let root = object(
        "BEGIN:VEVENT\nUID:a\nDTSTAMP:20260101T000000Z\nDTSTART:20261026T090000Z\nRRULE:FREQ=DAILY;COUNT=3\n\
BEGIN:VALARM\nACTION:DISPLAY\nTRIGGER:-PT15M\nACKNOWLEDGED:20261027T084600Z\nEND:VALARM\nEND:VEVENT\n",
    );
    assert_eq!(
        all(&root, "20261025T000000Z", "20261030T000000Z"),
        vec![utc("20261028T084500Z")]
    );
    let root = object(
        "BEGIN:VEVENT\nUID:a\nDTSTAMP:20260101T000000Z\nDTSTART:20261026T090000Z\n\
BEGIN:VALARM\nACTION:DISPLAY\nTRIGGER:-PT15M\nX-MOZ-LASTACK:20261026T084500Z\nEND:VALARM\nEND:VEVENT\n",
    );
    assert!(all(&root, "20261025T000000Z", "20261030T000000Z").is_empty());
}

#[test]
fn cancelled_events_and_completed_tasks_have_no_alarms() {
    let root = object(
        "BEGIN:VEVENT\nUID:a\nDTSTAMP:20260101T000000Z\nDTSTART:20261026T090000Z\nSTATUS:CANCELLED\n\
BEGIN:VALARM\nACTION:DISPLAY\nTRIGGER:-PT15M\nEND:VALARM\nEND:VEVENT\n",
    );
    assert!(all(&root, "20261025T000000Z", "20261030T000000Z").is_empty());
    let root = object(
        "BEGIN:VTODO\nUID:t\nDTSTAMP:20260101T000000Z\nDUE:20261026T090000Z\nCOMPLETED:20261020T000000Z\n\
BEGIN:VALARM\nACTION:DISPLAY\nTRIGGER:-PT15M\nEND:VALARM\nEND:VTODO\n",
    );
    assert!(all(&root, "20261025T000000Z", "20261030T000000Z").is_empty());
}

#[test]
fn todo_alarms_relate_to_due() {
    let root = object(
        "BEGIN:VTODO\nUID:t\nDTSTAMP:20260101T000000Z\nDTSTART:20261026T080000Z\nDUE:20261026T090000Z\n\
BEGIN:VALARM\nACTION:DISPLAY\nTRIGGER;RELATED=END:-PT15M\nEND:VALARM\n\
BEGIN:VALARM\nACTION:EMAIL\nTRIGGER:PT0S\nEND:VALARM\nEND:VTODO\n",
    );
    let mut budget = Budget::new(OBJECT_BUDGET);
    let found = alarm_occurrences(
        &root,
        utc("20261025T000000Z"),
        utc("20261030T000000Z"),
        None,
        Selection::All,
        &mut budget,
    )
    .unwrap();
    let times: Vec<i64> = found.iter().map(|o| o.trigger).collect();
    assert_eq!(
        times,
        vec![utc("20261026T080000Z"), utc("20261026T084500Z")]
    );
    assert!(found.iter().all(|o| o.kind == ComponentKind::Todo));
    // Only DUE: both relate to it.
    let root = object(
        "BEGIN:VTODO\nUID:t\nDTSTAMP:20260101T000000Z\nDUE:20261026T090000Z\n\
BEGIN:VALARM\nACTION:DISPLAY\nTRIGGER:-PT15M\nEND:VALARM\nEND:VTODO\n",
    );
    assert_eq!(
        all(&root, "20261025T000000Z", "20261030T000000Z"),
        vec![utc("20261026T084500Z")]
    );
}

#[test]
fn next_alarm_finds_far_and_near_triggers() {
    let root = object(
        "BEGIN:VEVENT\nUID:a\nDTSTAMP:20260101T000000Z\nDTSTART;VALUE=DATE:20000315\nRRULE:FREQ=YEARLY\n\
BEGIN:VALARM\nACTION:DISPLAY\nTRIGGER:-P1D\nEND:VALARM\nEND:VEVENT\n",
    );
    let mut budget = Budget::new(OBJECT_BUDGET);
    let next = next_alarm(
        &root,
        utc("20261023T000000Z"),
        400 * 86_400,
        None,
        Selection::All,
        &mut budget,
    )
    .unwrap();
    assert_eq!(next, Some(utc("20270314T000000Z")));
    // Nothing within the horizon.
    let mut budget = Budget::new(OBJECT_BUDGET);
    let next = next_alarm(
        &root,
        utc("20261023T000000Z"),
        30 * 86_400,
        None,
        Selection::All,
        &mut budget,
    )
    .unwrap();
    assert_eq!(next, None);
    // No alarms at all.
    let plain = object(
        "BEGIN:VEVENT\nUID:a\nDTSTAMP:20260101T000000Z\nDTSTART:20261026T090000Z\nEND:VEVENT\n",
    );
    let mut budget = Budget::new(OBJECT_BUDGET);
    assert_eq!(
        next_alarm(&plain, 0, 400 * 86_400, None, Selection::All, &mut budget).unwrap(),
        None
    );
}

#[test]
fn budget_exhaustion_is_reported() {
    let root = object(
        "BEGIN:VEVENT\nUID:a\nDTSTAMP:20260101T000000Z\nDTSTART;TZID=Europe/Berlin:20000101T090000\n\
RRULE:FREQ=MINUTELY\nBEGIN:VALARM\nACTION:DISPLAY\nTRIGGER:-PT1M\nEND:VALARM\nEND:VEVENT\n",
    );
    let mut budget = Budget::new(50);
    assert_eq!(
        alarm_occurrences(
            &root,
            utc("20261025T000000Z"),
            utc("20261026T000000Z"),
            None,
            Selection::All,
            &mut budget
        ),
        Err(Unevaluable)
    );
    // With the normal budget the window is bounded, and so is the output.
    let mut budget = Budget::new(OBJECT_BUDGET);
    let found = alarm_occurrences(
        &root,
        utc("20261026T000000Z"),
        utc("20261026T020000Z"),
        None,
        Selection::All,
        &mut budget,
    )
    .unwrap();
    assert_eq!(found.len(), 120);
    let mut budget = Budget::new(OBJECT_BUDGET * 10);
    let found = alarm_occurrences(
        &root,
        utc("20261025T000000Z"),
        utc("20261026T000000Z"),
        None,
        Selection::All,
        &mut budget,
    );
    if let Ok(found) = found {
        assert!(found.len() <= MAX_OCCURRENCES);
    }
}

#[test]
fn too_many_alarms_per_component_are_ignored() {
    let mut body =
        String::from("BEGIN:VEVENT\nUID:a\nDTSTAMP:20260101T000000Z\nDTSTART:20261026T090000Z\n");
    for minutes in 1..=40 {
        body.push_str(&format!(
            "BEGIN:VALARM\nACTION:DISPLAY\nTRIGGER:-PT{minutes}M\nEND:VALARM\n"
        ));
    }
    body.push_str("END:VEVENT\n");
    let root = object(&body);
    assert_eq!(
        all(&root, "20261025T000000Z", "20261030T000000Z").len(),
        MAX_ALARMS_PER_COMPONENT
    );
}
