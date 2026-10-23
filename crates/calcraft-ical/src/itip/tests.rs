use super::*;
use crate::{parse, write::to_string};

fn cal(body: &str) -> Component {
    let text =
        format!("BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//T//EN\r\n{body}END:VCALENDAR\r\n");
    parse(text.as_bytes()).unwrap()
}

fn is(address: &'static str) -> impl Fn(&str) -> bool {
    move |value: &str| value.eq_ignore_ascii_case(address)
}

const SERIES: &str = "BEGIN:VEVENT\r\nUID:s1\r\nDTSTAMP:20260101T000000Z\r\nDTSTART:20261001T100000Z\r\nDTEND:20261001T110000Z\r\nRRULE:FREQ=DAILY;COUNT=5\r\nSUMMARY:Daily\r\nORGANIZER:mailto:alice@example.org\r\nATTENDEE;PARTSTAT=ACCEPTED:mailto:alice@example.org\r\nATTENDEE;PARTSTAT=NEEDS-ACTION;RSVP=TRUE:mailto:bob@example.org\r\nATTENDEE:mailto:carol@example.org\r\nBEGIN:VALARM\r\nACTION:DISPLAY\r\nTRIGGER:-PT5M\r\nDESCRIPTION:x\r\nEND:VALARM\r\nEND:VEVENT\r\n";

#[test]
fn organizer_must_agree() {
    let ok = cal(SERIES);
    assert_eq!(
        organizer(&ok).unwrap().unwrap().value,
        "mailto:alice@example.org"
    );
    let bad = cal(&format!(
        "{SERIES}BEGIN:VEVENT\r\nUID:s1\r\nRECURRENCE-ID:20261002T100000Z\r\nDTSTART:20261002T120000Z\r\nORGANIZER:mailto:mallory@example.org\r\nEND:VEVENT\r\n"
    ));
    assert_eq!(organizer(&bad), Err(ItipError::DifferentOrganizers));
    let none = cal("BEGIN:VEVENT\r\nUID:x\r\nDTSTART:20261001T100000Z\r\nEND:VEVENT\r\n");
    assert_eq!(organizer(&none).unwrap(), None);
}

#[test]
fn agent_defaults_and_unknown_values() {
    let p = |params: &str| {
        let c = cal(&format!(
            "BEGIN:VEVENT\r\nUID:x\r\nATTENDEE{params}:mailto:a@b\r\nEND:VEVENT\r\n"
        ));
        agent(attendees(&c).next().unwrap())
    };
    assert_eq!(p(""), Agent::Server);
    assert_eq!(p(";SCHEDULE-AGENT=SERVER"), Agent::Server);
    assert_eq!(p(";SCHEDULE-AGENT=client"), Agent::Client);
    assert_eq!(p(";SCHEDULE-AGENT=NONE"), Agent::None);
    assert_eq!(p(";SCHEDULE-AGENT=X-OTHER"), Agent::None);
}

#[test]
fn projection_excludes_uninvited_instances_and_alarms() {
    let c = cal(&format!(
        "{SERIES}BEGIN:VEVENT\r\nUID:s1\r\nRECURRENCE-ID:20261003T100000Z\r\nDTSTART:20261003T150000Z\r\nDTEND:20261003T160000Z\r\nORGANIZER:mailto:alice@example.org\r\nATTENDEE:mailto:alice@example.org\r\nATTENDEE;SCHEDULE-STATUS=1.2:mailto:carol@example.org\r\nEND:VEVENT\r\n"
    ));
    let bob = project(&c, &is("mailto:bob@example.org")).unwrap();
    let text = to_string(&bob).unwrap();
    assert!(text.contains("EXDATE:20261003T100000Z"), "{text}");
    assert!(!text.contains("VALARM"));
    assert!(!text.contains("20261003T150000Z"));
    let carol = project(&c, &is("mailto:carol@example.org")).unwrap();
    let text = to_string(&carol).unwrap();
    assert!(text.contains("RECURRENCE-ID:20261003T100000Z"));
    assert!(!text.contains("EXDATE"));
    assert!(!text.contains("SCHEDULE-STATUS"));
    assert!(project(&c, &is("mailto:dave@example.org")).is_none());
}

#[test]
fn significance_ignores_stamps_and_partstat() {
    let a = cal(SERIES);
    let b = cal(&SERIES
        .replace("DTSTAMP:20260101T000000Z", "DTSTAMP:20260202T000000Z")
        .replace("PARTSTAT=NEEDS-ACTION", "PARTSTAT=ACCEPTED")
        .replace("SUMMARY:Daily", "X-APPLE-THING:1\r\nSUMMARY:Daily"));
    let bob = is("mailto:bob@example.org");
    assert!(same_for_attendee(
        &project(&a, &bob).unwrap(),
        &project(&b, &bob).unwrap()
    ));
    let moved = cal(&SERIES.replace("DTSTART:20261001T100000Z", "DTSTART:20261001T090000Z"));
    assert!(!same_for_attendee(
        &project(&a, &bob).unwrap(),
        &project(&moved, &bob).unwrap()
    ));
}

#[test]
fn reply_changes_only_own_partstat() {
    let mut organizer_copy = cal(SERIES);
    let bob = is("mailto:bob@example.org");
    let attendee = cal(&SERIES
        .replace(
            "ATTENDEE;PARTSTAT=NEEDS-ACTION;RSVP=TRUE:mailto:bob@example.org",
            "ATTENDEE;PARTSTAT=ACCEPTED:mailto:bob@example.org",
        )
        // Tampering with everything else has no effect.
        .replace("SUMMARY:Daily", "SUMMARY:Hacked")
        .replace(
            "ATTENDEE:mailto:carol@example.org",
            "ATTENDEE;PARTSTAT=DECLINED:mailto:carol@example.org",
        ));
    let replies = replies(&attendee, &organizer_copy, &bob);
    assert_eq!(replies.len(), 1);
    let changed = apply_replies(&mut organizer_copy, &bob, &replies);
    assert_eq!(changed.len(), 1);
    let text = to_string(&organizer_copy).unwrap();
    assert!(
        text.contains("PARTSTAT=ACCEPTED;RSVP=TRUE:mailto:bob@example.org"),
        "{text}"
    );
    assert!(text.contains("SUMMARY:Daily"));
    assert!(text.contains("ATTENDEE:mailto:carol@example.org"));
    // Applying it again changes nothing.
    assert!(apply_replies(&mut organizer_copy, &bob, &replies).is_empty());
    let message = to_string(&reply_message(
        &organizer_copy,
        &changed,
        &bob,
        "20261001T000000Z",
    ))
    .unwrap();
    assert!(message.contains("METHOD:REPLY"));
    assert!(!message.contains("carol"));
}

#[test]
fn reply_for_one_instance_adds_an_override() {
    let mut organizer_copy = cal(SERIES);
    let bob = is("mailto:bob@example.org");
    let attendee = cal(&format!(
        "{SERIES}BEGIN:VEVENT\r\nUID:s1\r\nRECURRENCE-ID:20261002T100000Z\r\nDTSTART:20261002T100000Z\r\nDTEND:20261002T110000Z\r\nORGANIZER:mailto:alice@example.org\r\nATTENDEE;PARTSTAT=DECLINED:mailto:bob@example.org\r\nEND:VEVENT\r\n"
    ));
    let replies = replies(&attendee, &organizer_copy, &bob);
    let changed = apply_replies(&mut organizer_copy, &bob, &replies);
    assert_eq!(changed.len(), 1);
    let text = to_string(&organizer_copy).unwrap();
    assert!(text.contains("RECURRENCE-ID:20261002T100000Z"), "{text}");
    assert!(text.contains("DURATION:PT3600S"), "{text}");
    // The new override keeps the other attendees and the organizer.
    assert_eq!(text.matches("ATTENDEE:mailto:carol@example.org").count(), 2);
}

#[test]
fn reply_for_a_fake_instance_is_ignored() {
    let mut organizer_copy = cal(SERIES);
    let bob = is("mailto:bob@example.org");
    for rid in ["20261002T103000Z", "20261009T100000Z", "20260930T100000Z"] {
        let attendee = cal(&format!(
            "{SERIES}BEGIN:VEVENT\r\nUID:s1\r\nRECURRENCE-ID:{rid}\r\nDTSTART:{rid}\r\nORGANIZER:mailto:alice@example.org\r\nATTENDEE;PARTSTAT=DECLINED:mailto:bob@example.org\r\nEND:VEVENT\r\n"
        ));
        let replies = replies(&attendee, &organizer_copy, &bob);
        assert!(
            apply_replies(&mut organizer_copy, &bob, &replies).is_empty(),
            "{rid}"
        );
    }
    assert_eq!(organizer_copy.components.len(), 1);
}

#[test]
fn exdate_in_attendee_copy_declines_the_instance() {
    let mut organizer_copy = cal(SERIES);
    let bob = is("mailto:bob@example.org");
    let attendee =
        cal(&SERIES.replace("SUMMARY:Daily", "EXDATE:20261004T100000Z\r\nSUMMARY:Daily"));
    let replies = replies(&attendee, &organizer_copy, &bob);
    let changed = apply_replies(&mut organizer_copy, &bob, &replies);
    assert_eq!(changed.len(), 1);
    let text = to_string(&organizer_copy).unwrap();
    assert!(text.contains("RECURRENCE-ID:20261004T100000Z"), "{text}");
}

#[test]
fn uninvited_attendee_changes_nothing() {
    let mut organizer_copy = cal(SERIES);
    let mallory = is("mailto:mallory@example.org");
    let attendee = cal(&SERIES.replace(
        "ATTENDEE:mailto:carol@example.org",
        "ATTENDEE;PARTSTAT=ACCEPTED:mailto:mallory@example.org",
    ));
    let replies = replies(&attendee, &organizer_copy, &mallory);
    assert!(apply_replies(&mut organizer_copy, &mallory, &replies).is_empty());
    assert!(!to_string(&organizer_copy).unwrap().contains("mallory"));
}

#[test]
fn decline_all_declines_every_listed_component() {
    let mut organizer_copy = cal(SERIES);
    let bob = is("mailto:bob@example.org");
    let replies = decline_all(&organizer_copy);
    let changed = apply_replies(&mut organizer_copy, &bob, &replies);
    assert_eq!(changed, [0]);
    assert!(
        to_string(&organizer_copy)
            .unwrap()
            .contains("PARTSTAT=DECLINED;RSVP=TRUE:mailto:bob@example.org")
    );
}

#[test]
fn preserve_partstats_keeps_recorded_replies() {
    let old = cal(&SERIES.replace("PARTSTAT=NEEDS-ACTION", "PARTSTAT=ACCEPTED"));
    let mut new = cal(SERIES);
    let bob = is("mailto:bob@example.org");
    preserve_partstats(&mut new, &old, &bob, &|_| false);
    assert!(
        to_string(&new)
            .unwrap()
            .contains("PARTSTAT=ACCEPTED;RSVP=TRUE:mailto:bob@example.org")
    );
    let mut reset = cal(SERIES);
    preserve_partstats(&mut reset, &old, &bob, &|_| true);
    assert!(to_string(&reset).unwrap().contains("PARTSTAT=NEEDS-ACTION"));
}

#[test]
fn statuses_and_force_send() {
    let mut c = cal(&SERIES.replace(
        "ATTENDEE:mailto:carol@example.org",
        "ATTENDEE;SCHEDULE-FORCE-SEND=REQUEST;SCHEDULE-STATUS=9.9:mailto:carol@example.org",
    ));
    let carol = is("mailto:carol@example.org");
    assert!(force_send(&c, "ATTENDEE", &carol, "REQUEST"));
    strip_requests(&mut c);
    assert!(!force_send(&c, "ATTENDEE", &carol, "REQUEST"));
    // Client-sent statuses are dropped unless kept.
    restore_statuses(&mut c, None, &|_| false);
    assert!(!to_string(&c).unwrap().contains("9.9"));
    set_parameter(
        &mut c,
        "ATTENDEE",
        &carol,
        "SCHEDULE-STATUS",
        Some(status::DELIVERED),
    );
    assert!(
        to_string(&c)
            .unwrap()
            .contains("SCHEDULE-STATUS=1.2:mailto:carol@example.org")
    );
}

#[test]
fn messages_cancel_and_copies() {
    let c = cal(SERIES);
    let bob = is("mailto:bob@example.org");
    let projection = project(&c, &bob).unwrap();
    let request = to_string(&message("REQUEST", &projection, "20261001T000000Z")).unwrap();
    assert!(request.contains("METHOD:REQUEST"));
    assert!(request.contains("DTSTAMP:20261001T000000Z"));
    let cancel = cancelled(&projection);
    assert!(is_cancelled(&cancel));
    assert!(!is_cancelled(&projection));
    let old = cal(SERIES);
    let copy = attendee_copy(&projection, Some(&old));
    assert!(to_string(&copy).unwrap().contains("VALARM"));
    assert_eq!(uid(&c), Some("s1"));
}

#[test]
fn busy_periods_respect_transparency_status_and_partstat() {
    let user = is("mailto:bob@example.org");
    let event = |extra: &str| {
        format!(
            "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nBEGIN:VEVENT\r\nUID:e\r\nDTSTART:20261001T100000Z\r\nDTEND:20261001T110000Z\r\n{extra}END:VEVENT\r\nEND:VCALENDAR\r\n"
        )
    };
    let start = parse_date_or_datetime("20261001T000000Z").unwrap().seconds;
    let end = start + 86_400;
    let periods = |extra: &str| {
        let mut budget = Budget::new(OBJECT_BUDGET);
        busy_periods(event(extra).as_bytes(), start, end, &user, &mut budget).unwrap()
    };
    assert_eq!(periods("").len(), 1);
    assert_eq!(periods("").first().unwrap().2, BusyType::Busy);
    assert!(periods("TRANSP:TRANSPARENT\r\n").is_empty());
    assert!(periods("STATUS:CANCELLED\r\n").is_empty());
    assert!(periods("ATTENDEE;PARTSTAT=DECLINED:mailto:bob@example.org\r\n").is_empty());
    assert_eq!(
        periods("ATTENDEE:mailto:bob@example.org\r\n")[0].2,
        BusyType::Tentative
    );
    // Clipped to the range.
    let mut budget = Budget::new(OBJECT_BUDGET);
    let clipped = busy_periods(
        event("").as_bytes(),
        start + 37_800,
        end,
        &user,
        &mut budget,
    )
    .unwrap();
    assert_eq!(clipped[0].0, start + 37_800);
    // Floating times are read as UTC, not widened.
    let floating = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nBEGIN:VEVENT\r\nUID:f\r\nDTSTART:20261001T100000\r\nDTEND:20261001T110000\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";
    let mut budget = Budget::new(OBJECT_BUDGET);
    let p = busy_periods(floating.as_bytes(), start, end, &user, &mut budget).unwrap();
    assert_eq!(p, [(start + 36_000, start + 39_600, BusyType::Busy)]);
}

#[test]
fn merging_periods() {
    let merged = merge_periods(vec![
        (5, 10, BusyType::Busy),
        (0, 6, BusyType::Busy),
        (20, 30, BusyType::Tentative),
        (12, 15, BusyType::Busy),
    ]);
    assert_eq!(
        merged,
        [
            (0, 10, BusyType::Busy),
            (12, 15, BusyType::Busy),
            (20, 30, BusyType::Tentative)
        ]
    );
    assert_eq!(utc_stamp(0).as_deref(), Some("19700101T000000Z"));
}

/// The series of SERIES, but daily for a long time.
fn long_series() -> Component {
    cal(&SERIES.replace("RRULE:FREQ=DAILY;COUNT=5", "RRULE:FREQ=DAILY;COUNT=5000"))
}

#[test]
fn many_exdates_are_cheap_and_bounded() {
    let organizer_copy = long_series();
    let bob = is("mailto:bob@example.org");
    // 40 000 EXDATE values (most of them not even instances).
    let start = parse_date_or_datetime("20261001T100000Z").unwrap().seconds;
    let values: Vec<String> = (0..40_000)
        .map(|i| utc_stamp(start + i * 3600).unwrap())
        .collect();
    let attendee = cal(&SERIES.replace(
        "RRULE:FREQ=DAILY;COUNT=5",
        &format!("RRULE:FREQ=DAILY;COUNT=5000\r\nEXDATE:{}", values.join(",")),
    ));
    let started = std::time::Instant::now();
    let replies = replies(&attendee, &organizer_copy, &bob);
    assert!(replies.len() <= MAX_EXDATE_REPLIES + 1, "{}", replies.len());
    let mut merged = organizer_copy.clone();
    let changed = apply_replies(&mut merged, &bob, &replies);
    assert!(started.elapsed() < std::time::Duration::from_secs(5));
    // At most a few overrides per reply: the organizer's object stays small.
    assert!(changed.len() <= MAX_NEW_OVERRIDES);
    assert!(merged.components.len() <= organizer_copy.components.len() + MAX_NEW_OVERRIDES);
}

#[test]
fn overrides_are_only_added_for_a_different_answer() {
    let mut organizer_copy = long_series();
    let bob = is("mailto:bob@example.org");
    // Bob already declined the series: declining single instances again
    // (EXDATEs) adds nothing.
    set_parameter(
        &mut organizer_copy,
        "ATTENDEE",
        &bob,
        "PARTSTAT",
        Some("DECLINED"),
    );
    let attendee = cal(&SERIES.replace(
        "RRULE:FREQ=DAILY;COUNT=5",
        "RRULE:FREQ=DAILY;COUNT=5000\r\nEXDATE:20261002T100000Z,20261003T100000Z",
    ))
    .clone();
    let mut attendee = attendee;
    set_parameter(
        &mut attendee,
        "ATTENDEE",
        &bob,
        "PARTSTAT",
        Some("DECLINED"),
    );
    let replies = replies(&attendee, &organizer_copy, &bob);
    assert!(apply_replies(&mut organizer_copy, &bob, &replies).is_empty());
    assert_eq!(organizer_copy.components.len(), 1);
}

#[test]
fn reset_untouched_and_rescheduled() {
    let mut c = cal(&SERIES.replace("PARTSTAT=NEEDS-ACTION", "PARTSTAT=ACCEPTED"));
    let bob = is("mailto:bob@example.org");
    reset_partstat(&mut c, &bob);
    assert_eq!(
        partstat(&c.components[0], &bob).as_deref(),
        Some("NEEDS-ACTION")
    );
    let view = project(&c, &bob).unwrap();
    assert!(untouched(&view, &view, &bob));
    let mut answered = view.clone();
    set_parameter(
        &mut answered,
        "ATTENDEE",
        &bob,
        "PARTSTAT",
        Some("ACCEPTED"),
    );
    assert!(!untouched(&answered, &view, &bob));
    let with_alarm = attendee_copy(&view, Some(&cal(SERIES)));
    assert!(!untouched(&with_alarm, &view, &bob));
    let renamed = project(
        &cal(&SERIES.replace("SUMMARY:Daily", "SUMMARY:Other")),
        &bob,
    )
    .unwrap();
    assert!(!rescheduled(&renamed, &view));
    let moved = project(
        &cal(&SERIES.replace("DTSTART:20261001T100000Z", "DTSTART:20261001T110000Z")),
        &bob,
    )
    .unwrap();
    assert!(rescheduled(&moved, &view));
    let mut status = cal(SERIES);
    set_parameter(
        &mut status,
        "ORGANIZER",
        &|_| true,
        "SCHEDULE-STATUS",
        Some("5.1"),
    );
    assert_eq!(organizer_status(&status).as_deref(), Some("5.1"));
}
