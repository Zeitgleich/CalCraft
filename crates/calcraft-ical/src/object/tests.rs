use super::*;

fn object(body: &str) -> Result<CalendarObject, IcalError> {
    let data =
        format!("BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//T//EN\r\n{body}END:VCALENDAR\r\n");
    CalendarObject::parse(data.as_bytes())
}

#[test]
fn simple_utc_event() {
    let obj = object("BEGIN:VEVENT\r\nUID:a\r\nDTSTART:20261021T100000Z\r\nDTEND:20261021T110000Z\r\nEND:VEVENT\r\n").unwrap();
    assert_eq!(obj.uid, "a");
    assert_eq!(obj.kind, ComponentKind::Event);
    assert_eq!(obj.range.start, Some(1_792_576_800));
    assert_eq!(obj.range.end, Some(1_792_580_400));
    assert!(obj.range.overlaps(Some(1_792_576_800), Some(1_792_576_801)));
    assert!(!obj.range.overlaps(Some(1_792_580_400), None));
    assert!(!obj.range.overlaps(None, Some(1_792_576_800)));
}

#[test]
fn local_time_is_widened() {
    let obj = object("BEGIN:VEVENT\r\nUID:a\r\nDTSTART;TZID=Europe/Berlin:20261021T100000\r\nDURATION:PT1H\r\nEND:VEVENT\r\n").unwrap();
    assert_eq!(obj.range.start, Some(1_792_576_800 - LOCAL_TIME_SLACK));
    assert_eq!(obj.range.end, Some(1_792_580_400 + LOCAL_TIME_SLACK));
}

#[test]
fn all_day_event_lasts_one_day() {
    let obj =
        object("BEGIN:VEVENT\r\nUID:a\r\nDTSTART;VALUE=DATE:20261021\r\nEND:VEVENT\r\n").unwrap();
    assert_eq!(
        obj.range.end.unwrap() - obj.range.start.unwrap(),
        86_400 + 2 * LOCAL_TIME_SLACK
    );
}

#[test]
fn recurrence_ranges() {
    let open = object(
        "BEGIN:VEVENT\r\nUID:a\r\nDTSTART:20261021T100000Z\r\nRRULE:FREQ=WEEKLY\r\nEND:VEVENT\r\n",
    )
    .unwrap();
    assert_eq!(open.range.end, None);
    let counted = object("BEGIN:VEVENT\r\nUID:a\r\nDTSTART:20261021T100000Z\r\nRRULE:FREQ=DAILY;COUNT=3\r\nEND:VEVENT\r\n").unwrap();
    // COUNT is expanded: the third instance (zero length) ends the span.
    assert_eq!(counted.range.end, Some(1_792_749_600 + 1));
    assert!(counted.recurring);
    let until = object("BEGIN:VEVENT\r\nUID:a\r\nDTSTART:20261021T100000Z\r\nDTEND:20261021T110000Z\r\nRRULE:FREQ=DAILY;UNTIL=20261031T100000Z\r\nEND:VEVENT\r\n").unwrap();
    assert_eq!(until.range.end, Some(1_793_440_800 + 3600));
}

#[test]
fn overrides_share_uid() {
    let obj = object("BEGIN:VEVENT\r\nUID:a\r\nDTSTART:20261021T100000Z\r\nRRULE:FREQ=DAILY\r\nEND:VEVENT\r\nBEGIN:VEVENT\r\nUID:a\r\nRECURRENCE-ID:20261022T100000Z\r\nDTSTART:20261022T120000Z\r\nEND:VEVENT\r\n").unwrap();
    assert_eq!(obj.uid, "a");
}

#[test]
fn todo_without_dates_is_unbounded() {
    let obj = object("BEGIN:VTODO\r\nUID:t\r\nSUMMARY:x\r\nEND:VTODO\r\n").unwrap();
    assert_eq!(obj.kind, ComponentKind::Todo);
    assert_eq!(obj.range, TimeRange::UNBOUNDED);
}

#[test]
fn rejects_invalid_objects() {
    let cases = [
        "BEGIN:VEVENT\r\nDTSTART:20261021T100000Z\r\nEND:VEVENT\r\n",
        "BEGIN:VEVENT\r\nUID:a\r\nUID:b\r\nEND:VEVENT\r\n",
        "BEGIN:VEVENT\r\nUID:a\r\nEND:VEVENT\r\nBEGIN:VEVENT\r\nUID:b\r\nEND:VEVENT\r\n",
        "BEGIN:VEVENT\r\nUID:a\r\nEND:VEVENT\r\nBEGIN:VTODO\r\nUID:a\r\nEND:VTODO\r\n",
        "BEGIN:VEVENT\r\nUID:a\r\nEND:VEVENT\r\nBEGIN:VEVENT\r\nUID:a\r\nEND:VEVENT\r\n",
        "BEGIN:VFREEBUSY\r\nUID:a\r\nEND:VFREEBUSY\r\n",
        "BEGIN:VEVENT\r\nUID:a\r\nDTSTART:20261021T100000Z\r\nDTEND:20261020T100000Z\r\nEND:VEVENT\r\n",
        "BEGIN:VEVENT\r\nUID:a\r\nDTSTART:20261021T100000Z\r\nDTEND:20261021T110000Z\r\nDURATION:PT1H\r\nEND:VEVENT\r\n",
        "BEGIN:VEVENT\r\nUID:a\r\nDTSTART:20261021T100000Z\r\nRRULE:FREQ=DAILY;INTERVAL=0\r\nEND:VEVENT\r\n",
        "BEGIN:VEVENT\r\nUID:a\r\nDTSTART:20261021T100000Z\r\nRRULE:COUNT=3\r\nEND:VEVENT\r\n",
        "BEGIN:VEVENT\r\nUID:a\r\nDTSTART:20261021T100000Z\r\nRRULE:FREQ=DAILY;COUNT=2;UNTIL=20261101T000000Z\r\nEND:VEVENT\r\n",
        "BEGIN:VEVENT\r\nUID:a\r\nDTSTART:2026-10-21\r\nEND:VEVENT\r\n",
        "",
    ];
    for body in cases {
        assert!(object(body).is_err(), "{body:?}");
    }
    assert!(CalendarObject::parse(b"BEGIN:VCALENDAR\r\nVERSION:2.0\r\nMETHOD:REQUEST\r\nBEGIN:VEVENT\r\nUID:a\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n").is_err());
    assert!(
        CalendarObject::parse(
            b"BEGIN:VCALENDAR\r\nBEGIN:VEVENT\r\nUID:a\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n"
        )
        .is_err()
    );
    assert!(CalendarObject::parse(b"BEGIN:VCALENDAR\r\nVERSION:2.0\r\nVERSION:3.0\r\nBEGIN:VEVENT\r\nUID:a\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n").is_err());
    assert!(CalendarObject::parse(b"BEGIN:VCARD\r\nVERSION:4.0\r\nEND:VCARD\r\n").is_err());
}
