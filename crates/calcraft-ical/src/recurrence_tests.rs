use std::time::{Duration, Instant};

use super::*;
use crate::{CalendarObject, datetime::parse_utc_datetime, parse};

const BERLIN: &str = "BEGIN:VTIMEZONE\r\nTZID:Europe/Berlin\r\n\
BEGIN:DAYLIGHT\r\nTZOFFSETFROM:+0100\r\nTZOFFSETTO:+0200\r\nTZNAME:CEST\r\n\
DTSTART:19700329T020000\r\nRRULE:FREQ=YEARLY;BYMONTH=3;BYDAY=-1SU\r\nEND:DAYLIGHT\r\n\
BEGIN:STANDARD\r\nTZOFFSETFROM:+0200\r\nTZOFFSETTO:+0100\r\nTZNAME:CET\r\n\
DTSTART:19701025T030000\r\nRRULE:FREQ=YEARLY;BYMONTH=10;BYDAY=-1SU\r\nEND:STANDARD\r\n\
END:VTIMEZONE\r\n";

const NEW_YORK: &str = "BEGIN:VTIMEZONE\r\nTZID:America/New_York\r\n\
BEGIN:DAYLIGHT\r\nTZOFFSETFROM:-0500\r\nTZOFFSETTO:-0400\r\nTZNAME:EDT\r\n\
DTSTART:20070311T020000\r\nRRULE:FREQ=YEARLY;BYMONTH=3;BYDAY=2SU\r\nEND:DAYLIGHT\r\n\
BEGIN:STANDARD\r\nTZOFFSETFROM:-0400\r\nTZOFFSETTO:-0500\r\nTZNAME:EST\r\n\
DTSTART:20071104T020000\r\nRRULE:FREQ=YEARLY;BYMONTH=11;BYDAY=1SU\r\nEND:STANDARD\r\n\
END:VTIMEZONE\r\n";

fn utc(value: &str) -> i64 {
    parse_utc_datetime(value).unwrap()
}

fn local(value: &str) -> i64 {
    parse_date_or_datetime(value).unwrap().seconds
}

fn calendar(body: &str) -> String {
    format!("BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//T//EN\r\n{body}END:VCALENDAR\r\n")
}

fn tz(vtimezone: &str) -> Tz {
    let root = parse(calendar(vtimezone).as_bytes()).unwrap();
    Tz::build(&root.components[0], &mut Budget::new(OBJECT_BUDGET))
        .unwrap()
        .unwrap()
}

/// Instances (UTC spans) near `[from, to)`.
fn instances(body: &str, from: &str, to: &str) -> Vec<(i64, i64)> {
    let root = parse(calendar(body).as_bytes()).unwrap();
    let (from, to) = (utc(from), utc(to));
    let mut found = Vec::new();
    walk(
        &root,
        &mut Budget::new(OBJECT_BUDGET),
        Mode::Overlap(Some(from), Some(to)),
        &mut |span| {
            if span.overlaps(Some(from), Some(to)) {
                found.push((span.lo, span.hi));
            }
            false
        },
    )
    .unwrap();
    found.sort_unstable();
    found
}

fn matches(body: &str, from: &str, to: &str) -> bool {
    matches_time_range(
        calendar(body).as_bytes(),
        Some(utc(from)),
        Some(utc(to)),
        &mut Budget::new(OBJECT_BUDGET),
    )
}

fn object(body: &str) -> CalendarObject {
    CalendarObject::parse(calendar(body).as_bytes()).unwrap()
}

fn event(props: &str) -> String {
    format!("BEGIN:VEVENT\r\nUID:e\r\nDTSTAMP:20260101T000000Z\r\n{props}END:VEVENT\r\n")
}

#[test]
fn berlin_dst_transitions() {
    let berlin = tz(BERLIN);
    // Last Sunday of March 2026: 02:00 CET becomes 03:00 CEST.
    assert_eq!(
        berlin.to_utc(local("20260329T013000")),
        Some(utc("20260329T003000Z"))
    );
    // Nonexistent local time: shifted forward by the gap.
    assert_eq!(
        berlin.to_utc(local("20260329T023000")),
        Some(utc("20260329T013000Z"))
    );
    assert_eq!(
        berlin.to_utc(local("20260329T033000")),
        Some(utc("20260329T013000Z"))
    );
    // Last Sunday of October 2026: 03:00 CEST becomes 02:00 CET.
    // Ambiguous local time: the first occurrence (CEST).
    assert_eq!(
        berlin.to_utc(local("20261025T023000")),
        Some(utc("20261025T003000Z"))
    );
    assert_eq!(
        berlin.to_utc(local("20261025T033000")),
        Some(utc("20261025T023000Z"))
    );
    assert_eq!(
        berlin.to_utc(local("20260701T120000")),
        Some(utc("20260701T100000Z"))
    );
    assert_eq!(
        berlin.to_utc(local("21000115T120000")),
        Some(utc("21000115T110000Z"))
    );
    // Before the first onset and beyond the horizon: unknown.
    assert_eq!(berlin.to_utc(local("19650101T000000")), None);
    assert_eq!(berlin.to_utc(local("22500101T000000")), None);
    assert_eq!(
        berlin.to_local(utc("20261025T010000Z")),
        Some(local("20261025T020000"))
    );
}

#[test]
fn new_york_dst_transitions() {
    let new_york = tz(NEW_YORK);
    // Second Sunday of March 2026 (8th): 02:00 EST becomes 03:00 EDT.
    assert_eq!(
        new_york.to_utc(local("20260308T013000")),
        Some(utc("20260308T063000Z"))
    );
    assert_eq!(
        new_york.to_utc(local("20260308T023000")),
        Some(utc("20260308T073000Z"))
    );
    assert_eq!(
        new_york.to_utc(local("20260308T033000")),
        Some(utc("20260308T073000Z"))
    );
    // First Sunday of November 2026 (1st): 02:00 EDT becomes 01:00 EST.
    assert_eq!(
        new_york.to_utc(local("20261101T013000")),
        Some(utc("20261101T053000Z"))
    );
    assert_eq!(
        new_york.to_utc(local("20261101T023000")),
        Some(utc("20261101T073000Z"))
    );
}

#[test]
fn weekly_event_follows_dst_exactly() {
    let body = format!(
        "{BERLIN}{}",
        event(
            "DTSTART;TZID=Europe/Berlin:20260322T100000\r\nDURATION:PT1H\r\nRRULE:FREQ=WEEKLY;COUNT=3\r\n"
        )
    );
    assert_eq!(
        instances(&body, "20260301T000000Z", "20260501T000000Z"),
        [
            (utc("20260322T090000Z"), utc("20260322T100000Z")),
            (utc("20260329T080000Z"), utc("20260329T090000Z")),
            (utc("20260405T080000Z"), utc("20260405T090000Z")),
        ]
    );
    // The hour after the DST switch is free; the hour before is not.
    assert!(!matches(&body, "20260329T090000Z", "20260329T100000Z"));
    assert!(matches(&body, "20260329T083000Z", "20260329T084500Z"));
    // Exact index: first start and last end.
    let indexed = object(&body);
    assert!(indexed.recurring);
    assert_eq!(indexed.range.start, Some(utc("20260322T090000Z")));
    assert_eq!(indexed.range.end, Some(utc("20260405T090000Z")));

    let body = format!(
        "{NEW_YORK}{}",
        event(
            "DTSTART;TZID=America/New_York:20261025T090000\r\nDTEND;TZID=America/New_York:20261025T100000\r\nRRULE:FREQ=WEEKLY;UNTIL=20261101T140000Z\r\n"
        )
    );
    let indexed = object(&body);
    assert_eq!(indexed.range.start, Some(utc("20261025T130000Z")));
    assert_eq!(indexed.range.end, Some(utc("20261101T150000Z")));
    assert!(!matches(&body, "20261101T130000Z", "20261101T140000Z"));
    assert!(matches(&body, "20261101T140000Z", "20261101T141500Z"));
}

#[test]
fn single_events_with_embedded_timezone_are_exact() {
    let body = format!(
        "{BERLIN}{}",
        event(
            "DTSTART;TZID=Europe/Berlin:20261021T100000\r\nDTEND;TZID=Europe/Berlin:20261021T110000\r\n"
        )
    );
    let indexed = object(&body);
    assert!(!indexed.recurring);
    assert_eq!(indexed.range.start, Some(utc("20261021T080000Z")));
    assert_eq!(indexed.range.end, Some(utc("20261021T090000Z")));
}

#[test]
fn unknown_zones_and_floating_times_are_widened() {
    // TZID without VTIMEZONE (RFC 7809 style), floating and all-day values.
    for props in [
        "DTSTART;TZID=Europe/Berlin:20261021T100000\r\nDURATION:PT1H\r\n",
        "DTSTART:20261021T100000\r\nDURATION:PT1H\r\n",
    ] {
        let indexed = object(&event(props));
        assert_eq!(indexed.range.start, Some(utc("20261021T100000Z") - SLACK));
        assert_eq!(indexed.range.end, Some(utc("20261021T110000Z") + SLACK));
    }
    // A VTIMEZONE that cannot be evaluated counts as unknown.
    let broken = "BEGIN:VTIMEZONE\r\nTZID:X\r\nBEGIN:STANDARD\r\nDTSTART:19700101T000000\r\nTZOFFSETTO:+0100\r\nEND:STANDARD\r\nEND:VTIMEZONE\r\n";
    let indexed = object(&format!(
        "{broken}{}",
        event("DTSTART;TZID=X:20261021T100000\r\nDURATION:PT1H\r\n")
    ));
    assert_eq!(indexed.range.start, Some(utc("20261021T100000Z") - SLACK));
}

#[test]
fn all_day_recurring_with_exdate_and_rdate() {
    let body = event(
        "DTSTART;VALUE=DATE:20261005\r\nRRULE:FREQ=WEEKLY;COUNT=4\r\nEXDATE;VALUE=DATE:20261012\r\nRDATE;VALUE=DATE:20261103\r\n",
    );
    let days: Vec<i64> = instances(&body, "20261001T000000Z", "20261201T000000Z")
        .into_iter()
        .map(|(lo, _)| lo + SLACK)
        .collect();
    assert_eq!(
        days,
        [
            utc("20261005T000000Z"),
            utc("20261019T000000Z"),
            utc("20261026T000000Z"),
            utc("20261103T000000Z"),
        ]
    );
    // The excluded day is free (allowing for the ±14 h of an all-day date).
    assert!(!matches(&body, "20261012T150000Z", "20261013T090000Z"));
    assert!(matches(&body, "20261019T150000Z", "20261020T090000Z"));
    let indexed = object(&body);
    assert_eq!(indexed.range.start, Some(utc("20261005T000000Z") - SLACK));
    assert_eq!(indexed.range.end, Some(utc("20261104T000000Z") + SLACK));
}

#[test]
fn utc_exdate_and_period_rdate() {
    let body = event(
        "DTSTART:20261001T100000Z\r\nDTEND:20261001T110000Z\r\nRRULE:FREQ=DAILY;COUNT=5\r\n\
         EXDATE:20261002T100000Z,20261003T100000Z\r\nRDATE;VALUE=PERIOD:20261020T080000Z/PT3H\r\n",
    );
    assert_eq!(
        instances(&body, "20261001T000000Z", "20261101T000000Z"),
        [
            (utc("20261001T100000Z"), utc("20261001T110000Z")),
            (utc("20261004T100000Z"), utc("20261004T110000Z")),
            (utc("20261005T100000Z"), utc("20261005T110000Z")),
            (utc("20261020T080000Z"), utc("20261020T110000Z")),
        ]
    );
    assert!(!matches(&body, "20261002T000000Z", "20261004T000000Z"));
    assert!(matches(&body, "20261020T103000Z", "20261020T104000Z"));
    assert_eq!(object(&body).range.end, Some(utc("20261020T110000Z")));
}

#[test]
fn overrides_move_instances_in_and_out() {
    let body = format!(
        "{}{}",
        event("DTSTART:20261001T100000Z\r\nDURATION:PT1H\r\nRRULE:FREQ=DAILY\r\n"),
        // The 3 October instance moves to 20 December.
        "BEGIN:VEVENT\r\nUID:e\r\nRECURRENCE-ID:20261003T100000Z\r\nDTSTART:20261220T100000Z\r\nDURATION:PT1H\r\nEND:VEVENT\r\n",
    );
    assert!(!matches(&body, "20261003T000000Z", "20261004T000000Z"));
    assert!(matches(&body, "20261004T000000Z", "20261005T000000Z"));
    assert!(matches(&body, "20261220T103000Z", "20261220T104000Z"));
    // An instance moved into a free slot.
    let body = format!(
        "{}{}",
        event("DTSTART:20261001T100000Z\r\nDURATION:PT1H\r\nRRULE:FREQ=WEEKLY;COUNT=3\r\n"),
        "BEGIN:VEVENT\r\nUID:e\r\nRECURRENCE-ID:20261008T100000Z\r\nDTSTART:20261009T180000Z\r\nDURATION:PT1H\r\nEND:VEVENT\r\n",
    );
    assert!(matches(&body, "20261009T180000Z", "20261009T183000Z"));
    assert!(!matches(&body, "20261008T000000Z", "20261009T000000Z"));
    let indexed = object(&body);
    assert_eq!(indexed.range.start, Some(utc("20261001T100000Z")));
    assert_eq!(indexed.range.end, Some(utc("20261015T110000Z")));
}

#[test]
fn this_and_future_is_conservative() {
    let body = format!(
        "{}{}",
        event("DTSTART:20261001T100000Z\r\nDURATION:PT1H\r\nRRULE:FREQ=DAILY;COUNT=10\r\n"),
        "BEGIN:VEVENT\r\nUID:e\r\nRECURRENCE-ID;RANGE=THISANDFUTURE:20261005T100000Z\r\nDTSTART:20261005T150000Z\r\nDURATION:PT1H\r\nEND:VEVENT\r\n",
    );
    // Later instances may have moved to 15:00: both slots match.
    assert!(matches(&body, "20261007T150000Z", "20261007T153000Z"));
    assert!(matches(&body, "20261007T100000Z", "20261007T103000Z"));
    // Earlier instances are unaffected.
    assert!(!matches(&body, "20261002T150000Z", "20261002T153000Z"));
    assert!(object(&body).range.end.unwrap() >= utc("20261010T160000Z"));
}

#[test]
fn count_until_and_open_ended_spans() {
    let counted = object(&event(
        "DTSTART:20261021T100000Z\r\nRRULE:FREQ=DAILY;COUNT=3\r\n",
    ));
    assert_eq!(counted.range.start, Some(utc("20261021T100000Z")));
    assert_eq!(counted.range.end, Some(utc("20261023T100000Z") + 1));
    // UNTIL is inclusive.
    let until = object(&event(
        "DTSTART:20261021T100000Z\r\nDURATION:PT1H\r\nRRULE:FREQ=DAILY;UNTIL=20261025T100000Z\r\n",
    ));
    assert_eq!(until.range.end, Some(utc("20261025T110000Z")));
    let before = object(&event(
        "DTSTART:20261021T100000Z\r\nDURATION:PT1H\r\nRRULE:FREQ=DAILY;UNTIL=20261025T095959Z\r\n",
    ));
    assert_eq!(before.range.end, Some(utc("20261024T110000Z")));
    let open = object(&event("DTSTART:20261021T100000Z\r\nRRULE:FREQ=YEARLY\r\n"));
    assert_eq!(open.range.start, Some(utc("20261021T100000Z")));
    assert_eq!(open.range.end, None);
    // A yearly rule on 29 February only occurs in leap years.
    let leap = event("DTSTART;VALUE=DATE:20240229\r\nRRULE:FREQ=YEARLY\r\n");
    assert!(matches(&leap, "20280229T000000Z", "20280301T000000Z"));
    assert!(!matches(&leap, "20270227T000000Z", "20270302T000000Z"));
    // Open-ended rules are found far in the future without expanding
    // everything before.
    let weekly =
        event("DTSTART:20000103T090000Z\r\nDURATION:PT1H\r\nRRULE:FREQ=WEEKLY;BYDAY=MO\r\n");
    assert!(matches(&weekly, "20991102T090000Z", "20991102T091500Z"));
    assert!(!matches(&weekly, "20991103T090000Z", "20991103T091500Z"));
}

#[test]
fn bysetpos_and_negative_month_day() {
    let body = event(
        "DTSTART:20260130T120000Z\r\nRRULE:FREQ=MONTHLY;BYDAY=MO,TU,WE,TH,FR;BYSETPOS=-1;COUNT=3\r\n",
    );
    // Last weekday of January, February and March 2026.
    let starts: Vec<i64> = instances(&body, "20260101T000000Z", "20260501T000000Z")
        .into_iter()
        .map(|(lo, _)| lo)
        .collect();
    assert_eq!(
        starts,
        [
            utc("20260130T120000Z"),
            utc("20260227T120000Z"),
            utc("20260331T120000Z")
        ]
    );
    let body = event("DTSTART:20260131T120000Z\r\nRRULE:FREQ=MONTHLY;BYMONTHDAY=-1;COUNT=2\r\n");
    assert_eq!(object(&body).range.end, Some(utc("20260228T120000Z") + 1));
}

#[test]
fn todo_boundaries_are_inclusive() {
    let due = "BEGIN:VTODO\r\nUID:t\r\nDUE:20261021T100000Z\r\nEND:VTODO\r\n";
    // RFC 4791 §9.9: (start < DUE) AND (end >= DUE).
    assert!(matches(due, "20261021T090000Z", "20261021T100000Z"));
    assert!(matches(due, "20261021T095959Z", "20261021T100000Z"));
    let recurring = "BEGIN:VTODO\r\nUID:t\r\nDTSTART:20261021T090000Z\r\nDUE:20261021T100000Z\r\nRRULE:FREQ=WEEKLY;COUNT=2\r\nEND:VTODO\r\n";
    assert!(matches(recurring, "20261028T100000Z", "20261028T110000Z"));
    assert!(!matches(recurring, "20261104T080000Z", "20261104T120000Z"));
}

#[test]
fn budget_exhaustion_falls_back_to_conservative() {
    // Too many instances to expand: the index stays open, queries match.
    let body = event("DTSTART:20261021T100000Z\r\nRRULE:FREQ=MINUTELY;COUNT=1000000\r\n");
    let indexed = object(&body);
    assert_eq!(indexed.range.start, Some(utc("20261021T100000Z")));
    assert_eq!(indexed.range.end, None);
    assert!(matches(&body, "20500101T000000Z", "20500102T000000Z"));
    // UNTIL still bounds the conservative range.
    let body = event("DTSTART:20261021T100000Z\r\nRRULE:FREQ=SECONDLY;UNTIL=20301021T100000Z\r\n");
    let indexed = object(&body);
    assert!(indexed.range.end.unwrap() >= utc("20301021T100000Z"));
    assert!(indexed.range.end.unwrap() < utc("20301030T000000Z"));
    // A tiny budget: always a match, never a wrong "no".
    let weekly = event("DTSTART:20261021T100000Z\r\nRRULE:FREQ=WEEKLY;COUNT=500\r\n");
    assert!(matches_time_range(
        calendar(&weekly).as_bytes(),
        Some(utc("20270101T000000Z")),
        Some(utc("20270102T000000Z")),
        &mut Budget::new(10),
    ));
    // Unsupported rules (RSCALE) are matched conservatively.
    let rscale = event("DTSTART:20261021T100000Z\r\nRRULE:RSCALE=HEBREW;FREQ=YEARLY;COUNT=2\r\n");
    assert!(matches(&rscale, "20500101T000000Z", "20500102T000000Z"));
    assert_eq!(object(&rscale).range.end, None);
}

#[test]
fn hostile_inputs_are_bounded_in_time() {
    let hostile = [
        event("DTSTART:20261021T100000Z\r\nRRULE:FREQ=SECONDLY;COUNT=4294967295\r\n"),
        event("DTSTART:20261021T100000Z\r\nRRULE:FREQ=SECONDLY\r\n"),
        event("DTSTART:00010101T000000Z\r\nRRULE:FREQ=SECONDLY;BYMONTH=2;BYMONTHDAY=30\r\n"),
        event("DTSTART:00010101T000000Z\r\nRRULE:FREQ=YEARLY;BYMONTH=2;BYMONTHDAY=30;COUNT=5\r\n"),
        event(&format!(
            "DTSTART:20261021T100000Z\r\nRRULE:FREQ=YEARLY;COUNT=100000;BYMONTH=1,2,3,4,5,6,7,8,9,10,11,12;BYDAY=MO,TU,WE,TH,FR,SA,SU;BYHOUR={};BYMINUTE={};BYSECOND={}\r\n",
            (0..24).map(|n| n.to_string()).collect::<Vec<_>>().join(","),
            (0..60).map(|n| n.to_string()).collect::<Vec<_>>().join(","),
            (0..61).map(|n| n.to_string()).collect::<Vec<_>>().join(","),
        )),
        event(&format!(
            "DTSTART:20261021T100000Z\r\nRRULE:FREQ=DAILY;COUNT=9000\r\nEXDATE:{}\r\n",
            vec!["20261022T100000Z"; 9_999].join(",")
        )),
        format!(
            "BEGIN:VTIMEZONE\r\nTZID:H\r\nBEGIN:STANDARD\r\nDTSTART:19000101T000000\r\nTZOFFSETFROM:+0100\r\nTZOFFSETTO:+0100\r\nRRULE:FREQ=DAILY\r\nEND:STANDARD\r\nEND:VTIMEZONE\r\n{}",
            event("DTSTART;TZID=H:20261021T100000\r\nRRULE:FREQ=DAILY;COUNT=5000\r\n")
        ),
        format!(
            "BEGIN:VTIMEZONE\r\nTZID:H\r\nBEGIN:STANDARD\r\nDTSTART:19000101T000000\r\nTZOFFSETFROM:+0100\r\nTZOFFSETTO:+0100\r\nRRULE:FREQ=SECONDLY\r\nEND:STANDARD\r\nEND:VTIMEZONE\r\n{}",
            event("DTSTART;TZID=H:20261021T100000\r\n")
        ),
    ];
    for body in &hostile {
        let started = Instant::now();
        let indexed = CalendarObject::parse(calendar(body).as_bytes());
        let matched = matches(body, "20500101T000000Z", "20500102T000000Z");
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "{:?} for {}",
            started.elapsed(),
            &body[..body.len().min(200)]
        );
        if let Ok(indexed) = indexed {
            // Whatever the outcome, a match in 2050 is not ruled out wrongly
            // by the index when the evaluation says it might match.
            if matched {
                assert!(
                    indexed
                        .range
                        .overlaps(Some(utc("20500101T000000Z")), Some(utc("20500102T000000Z")))
                );
            }
        }
    }
}

#[test]
fn span_never_excludes_an_instance() {
    // Cross-check: every instance found in a window lies within the index.
    let bodies = [
        format!(
            "{BERLIN}{}",
            event(
                "DTSTART;TZID=Europe/Berlin:20260301T023000\r\nDURATION:PT2H\r\nRRULE:FREQ=WEEKLY;BYDAY=SU;COUNT=40\r\n"
            )
        ),
        format!(
            "{NEW_YORK}{}",
            event(
                "DTSTART;TZID=America/New_York:20260101T013000\r\nRRULE:FREQ=MONTHLY;BYDAY=1SU,-1SA;UNTIL=20271231T000000Z\r\n"
            )
        ),
        event(
            "DTSTART;VALUE=DATE:20260101\r\nRRULE:FREQ=YEARLY;BYWEEKNO=1,-1;BYDAY=MO;COUNT=8\r\n",
        ),
        event(
            "DTSTART:20260101T000000\r\nRRULE:FREQ=HOURLY;INTERVAL=7;COUNT=300\r\nRDATE:20250101T000000\r\n",
        ),
    ];
    for body in &bodies {
        let indexed = object(body);
        for (lo, hi) in instances(body, "20240101T000000Z", "20300101T000000Z") {
            assert!(
                indexed.range.start.is_none_or(|s| s <= lo)
                    && indexed.range.end.is_none_or(|e| e >= hi),
                "{body}"
            );
        }
    }
}
