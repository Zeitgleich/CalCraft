use calcraft_core::api::{
    EventDetail, EventFields, EventInstance, EventTime, Frequency, Recurrence,
};

use super::{
    date::*,
    form::{Ends, EventForm, Repeat, describe},
    layout::{Placed, columns, lanes},
    moves::{Change, LocalSpan, changed_span, changed_times, day_piece, local_span, snap},
    nav::{Command, EventKey, View, event_key, grid_move, shortcut},
    zone::{Fixed, Zone, day_start, definition, format_offset, to_local, to_utc, transitions},
};

/// Last `weekday` (0 = Monday) of a month, as a day number.
fn last_weekday(year: i64, month: u32, wanted: usize) -> i64 {
    let last = days_from_civil(year, month, days_in_month(year, month));
    last - (weekday(last) as i64 - wanted as i64).rem_euclid(7)
}

/// Europe/Berlin: CET, CEST from the last Sunday in March 01:00 UTC to the
/// last Sunday in October 01:00 UTC.
struct Berlin;

impl Zone for Berlin {
    fn offset(&self, utc: i64) -> i64 {
        let (year, ..) = civil(utc.div_euclid(DAY));
        let start = last_weekday(year, 3, 6) * DAY + 3600;
        let end = last_weekday(year, 10, 6) * DAY + 3600;
        if (start..end).contains(&utc) {
            7200
        } else {
            3600
        }
    }
}

/// America/New_York: EST, EDT from the second Sunday in March 07:00 UTC
/// to the first Sunday in November 06:00 UTC.
struct NewYork;

impl Zone for NewYork {
    fn offset(&self, utc: i64) -> i64 {
        let (year, ..) = civil(utc.div_euclid(DAY));
        let nth_sunday = |month: u32, n: i64| {
            let first = days_from_civil(year, month, 1);
            first + (6 - weekday(first) as i64).rem_euclid(7) + (n - 1) * 7
        };
        let start = nth_sunday(3, 2) * DAY + 7 * 3600;
        let end = nth_sunday(11, 1) * DAY + 6 * 3600;
        if (start..end).contains(&utc) {
            -4 * 3600
        } else {
            -5 * 3600
        }
    }
}

fn day(value: &str) -> i64 {
    parse_date(value).unwrap()
}

fn at(date: &str, time: &str) -> i64 {
    day(date) * DAY + parse_time(time).unwrap() * 60
}

// ------------------------------------------------------------------ dates

#[test]
fn civil_dates_round_trip() {
    for days in [-1_000_000, -1, 0, 1, 59, 60, 11_016, 20_754, 2_932_896] {
        let (y, m, d) = civil(days);
        assert_eq!(days_from_civil(y, m, d), days);
    }
    assert_eq!(format_date(0), "1970-01-01");
    assert_eq!(day("2000-02-29"), 11_016);
    assert_eq!(parse_date("2026-02-29"), None);
    assert_eq!(parse_date("2026-13-01"), None);
    assert_eq!(parse_date("26-01-01"), None);
    assert_eq!(parse_date("2026-1-01"), None);
    assert_eq!(parse_date("2026-01-01x"), None);
    assert_eq!(weekday(day("2026-10-23")), 4, "a Friday");
    assert_eq!(week_start(day("2026-10-25")), day("2026-10-19"));
    assert_eq!(iso_week(day("2026-01-01")), 1);
    assert_eq!(iso_week(day("2027-01-01")), 53);
    assert_eq!(iso_week(day("2026-10-23")), 43);
}

#[test]
fn months_and_grids() {
    assert_eq!(add_months(day("2026-01-31"), 1), day("2026-02-28"));
    assert_eq!(add_months(day("2028-01-31"), 1), day("2028-02-29"));
    assert_eq!(add_months(day("2026-12-15"), 1), day("2027-01-15"));
    assert_eq!(add_months(day("2026-01-15"), -1), day("2025-12-15"));
    // October 2026 starts on a Thursday: the grid starts on Monday 28 Sep.
    assert_eq!(month_grid_start(day("2026-10-23")), day("2026-09-28"));
    assert_eq!(month_title(day("2026-10-23")), "October 2026");
    assert_eq!(short_day(day("2026-10-23")), "Fri 23 Oct");
    assert_eq!(long_day(day("2026-10-23")), "Friday, 23 October 2026");
    assert_eq!(
        range_title(day("2026-10-19"), day("2026-10-25")),
        "19 – 25 Oct 2026"
    );
    assert_eq!(
        range_title(day("2026-09-28"), day("2026-10-04")),
        "28 Sep – 4 Oct 2026"
    );
    assert_eq!(
        range_title(day("2026-12-28"), day("2027-01-03")),
        "28 Dec 2026 – 3 Jan 2027"
    );
}

#[test]
fn times() {
    assert_eq!(parse_time("09:05"), Some(545));
    assert_eq!(parse_time("9:05"), Some(545));
    assert_eq!(parse_time("24:00"), None);
    assert_eq!(parse_time("12:60"), None);
    assert_eq!(parse_time("12:5"), None);
    assert_eq!(format_time(545), "09:05");
    assert_eq!(clock(at("2026-10-23", "23:59")), "23:59");
}

// ------------------------------------------------------------------ zones

#[test]
fn dst_conversions() {
    // Summer and winter.
    assert_eq!(
        to_local(&Berlin, at("2026-07-01", "07:00")),
        at("2026-07-01", "09:00")
    );
    assert_eq!(
        to_utc(&Berlin, at("2026-12-01", "09:00")),
        at("2026-12-01", "08:00")
    );
    // Spring gap (29 Mar 2026, 02:00 → 03:00): 02:30 does not exist and is
    // shifted forward to 03:30 CEST.
    assert_eq!(
        to_utc(&Berlin, at("2026-03-29", "02:30")),
        at("2026-03-29", "01:30")
    );
    assert_eq!(
        to_local(&Berlin, at("2026-03-29", "01:30")),
        at("2026-03-29", "03:30")
    );
    // Autumn overlap (25 Oct 2026, 03:00 → 02:00): 02:30 means the first,
    // CEST one.
    assert_eq!(
        to_utc(&Berlin, at("2026-10-25", "02:30")),
        at("2026-10-25", "00:30")
    );
    // Days have 23 and 25 hours.
    let spring = day("2026-03-29");
    assert_eq!(
        day_start(&Berlin, spring + 1) - day_start(&Berlin, spring),
        23 * 3600
    );
    let autumn = day("2026-10-25");
    assert_eq!(
        day_start(&Berlin, autumn + 1) - day_start(&Berlin, autumn),
        25 * 3600
    );
    assert_eq!(
        to_utc(&NewYork, at("2026-03-08", "02:30")),
        at("2026-03-08", "07:30")
    );
    assert_eq!(to_utc(&Fixed(0), 12345), 12345);
}

#[test]
fn zone_definitions() {
    let found = transitions(&Berlin, 2026);
    assert_eq!(found.len(), 2);
    assert_eq!(found[0].at, at("2026-03-29", "01:00"));
    assert_eq!((found[0].from, found[0].to), (3600, 7200));
    let info = definition(&Berlin, "Europe/Berlin", 2026);
    assert_eq!(info.tzid, "Europe/Berlin");
    let summer = &info.observances[0];
    assert!(summer.daylight);
    assert_eq!(summer.start, "19700329T020000");
    assert_eq!(summer.offset_from, "+0100");
    assert_eq!(summer.offset_to, "+0200");
    assert_eq!(
        summer.rrule.as_deref(),
        Some("FREQ=YEARLY;BYMONTH=3;BYDAY=-1SU")
    );
    let winter = &info.observances[1];
    assert!(!winter.daylight);
    assert_eq!(winter.start, "19701025T030000");
    assert_eq!(
        winter.rrule.as_deref(),
        Some("FREQ=YEARLY;BYMONTH=10;BYDAY=-1SU")
    );

    let ny = definition(&NewYork, "America/New_York", 2026);
    assert_eq!(
        ny.observances[0].rrule.as_deref(),
        Some("FREQ=YEARLY;BYMONTH=3;BYDAY=2SU")
    );
    assert_eq!(ny.observances[0].start, "19700308T020000");
    assert_eq!(
        ny.observances[1].rrule.as_deref(),
        Some("FREQ=YEARLY;BYMONTH=11;BYDAY=1SU")
    );
    assert_eq!(ny.observances[1].offset_to, "-0500");

    let fixed = definition(&Fixed(19_800), "Asia/Kolkata", 2026);
    assert_eq!(fixed.observances.len(), 1);
    assert_eq!(fixed.observances[0].offset_to, "+0530");
    assert_eq!(format_offset(-12_600), "-0330");
}

// ----------------------------------------------------------------- layout

#[test]
fn overlapping_events_share_columns() {
    // 09-10, 09:30-11, 10-10:30, 12-13 (alone), 12:00 empty.
    let spans = [(540, 600), (570, 660), (600, 630), (720, 780), (720, 720)];
    let placed = columns(&spans, 15);
    let p = |column, columns| Placed { column, columns };
    assert_eq!(placed[0], p(0, 2));
    assert_eq!(placed[1], p(1, 2));
    // Takes the column the first event freed at 10:00.
    assert_eq!(placed[2], p(0, 2));
    assert_eq!(placed[3], p(0, 2));
    assert_eq!(placed[4], p(1, 2));
    assert!(columns(&[], 15).is_empty());
    // Three at once.
    let three = columns(&[(0, 60), (0, 60), (30, 90)], 15);
    assert_eq!(
        three.iter().map(|p| p.columns).collect::<Vec<_>>(),
        [3, 3, 3]
    );
    let mut used: Vec<_> = three.iter().map(|p| p.column).collect();
    used.sort_unstable();
    assert_eq!(used, [0, 1, 2]);
    // Back to back is no overlap.
    let chain = columns(&[(0, 60), (60, 120)], 15);
    assert_eq!(chain, vec![p(0, 1), p(0, 1)]);
}

#[test]
fn all_day_lanes() {
    // Mon–Wed, Tue, Thu, Mon–Sun.
    assert_eq!(lanes(&[(0, 2), (1, 1), (3, 3), (0, 6)]), vec![1, 2, 1, 0]);
    assert_eq!(lanes(&[(0, 0), (1, 1)]), vec![0, 0]);
}

// -------------------------------------------------------------- navigation

#[test]
fn views_and_periods() {
    let friday = day("2026-10-23");
    assert_eq!(
        View::Month.range(friday),
        (day("2026-09-28"), day("2026-11-09"))
    );
    assert_eq!(
        View::Week.range(friday),
        (day("2026-10-19"), day("2026-10-26"))
    );
    assert_eq!(View::Day.range(friday), (friday, friday + 1));
    assert_eq!(View::Agenda.range(friday).1 - friday, 30);
    assert_eq!(View::Month.step(day("2026-01-31"), 1), day("2026-02-01"));
    assert_eq!(View::Week.step(friday, -1), friday - 7);
    for view in View::ALL {
        assert_eq!(View::parse(view.as_str()), Some(view));
    }
}

#[test]
fn keyboard_model() {
    assert_eq!(shortcut("t", false), Some(Command::Today));
    assert_eq!(shortcut("w", false), Some(Command::Show(View::Week)));
    assert_eq!(shortcut("j", false), Some(Command::Next));
    assert_eq!(shortcut("n", true), None, "Ctrl+N stays the browser's");
    assert_eq!(shortcut("x", false), None);
    let friday = day("2026-10-23");
    assert_eq!(grid_move("ArrowDown", friday), Some(friday + 7));
    assert_eq!(grid_move("Home", friday), Some(day("2026-10-19")));
    assert_eq!(grid_move("End", friday), Some(day("2026-10-25")));
    assert_eq!(grid_move("a", friday), None);
    assert_eq!(
        event_key("ArrowDown", true, false, false),
        Some(EventKey::Move {
            days: 0,
            minutes: 15
        })
    );
    assert_eq!(
        event_key("ArrowDown", true, false, true),
        Some(EventKey::Move {
            days: 7,
            minutes: 0
        })
    );
    assert_eq!(
        event_key("ArrowUp", false, true, false),
        Some(EventKey::Resize { minutes: -15 })
    );
    assert_eq!(event_key("ArrowUp", false, true, true), None);
    assert_eq!(
        event_key("ArrowUp", false, false, false),
        None,
        "plain arrows navigate"
    );
    assert_eq!(
        event_key("Enter", false, false, false),
        Some(EventKey::Open)
    );
}

// ------------------------------------------------------------------ moves

fn instance(start: i64, end: i64, all_day: bool) -> EventInstance {
    EventInstance {
        calendar: "default".to_owned(),
        name: "a.ics".to_owned(),
        etag: "\"e\"".to_owned(),
        recurrence_id: Some("rid".to_owned()),
        summary: Some("A".to_owned()),
        location: None,
        all_day,
        floating: false,
        start,
        end,
        busy: false,
        cancelled: false,
        recurring: true,
        writable: true,
    }
}

#[test]
fn local_spans_and_pieces() {
    // 22:00–02:00 Berlin time in winter (21:00–01:00 UTC).
    let night = instance(at("2026-12-01", "21:00"), at("2026-12-02", "01:00"), false);
    let span = local_span(&night, &Berlin);
    assert_eq!(span.start, at("2026-12-01", "22:00"));
    assert_eq!(span.days(), (day("2026-12-01"), day("2026-12-02")));
    assert_eq!(
        day_piece(&span, day("2026-12-01")),
        Some((22 * 60, 24 * 60))
    );
    assert_eq!(day_piece(&span, day("2026-12-02")), Some((0, 120)));
    assert_eq!(day_piece(&span, day("2026-12-03")), None);
    // Ending at midnight does not touch the next day.
    let to_midnight = LocalSpan {
        start: at("2026-12-01", "23:00"),
        end: at("2026-12-02", "00:00"),
        all_day: false,
    };
    assert_eq!(to_midnight.days(), (day("2026-12-01"), day("2026-12-01")));
    // All-day: dates as they are, end exclusive.
    let holiday = instance(day("2026-12-24") * DAY, day("2026-12-27") * DAY, true);
    let span = local_span(&holiday, &Berlin);
    assert_eq!(span.days(), (day("2026-12-24"), day("2026-12-26")));
    assert_eq!(
        day_piece(&span, day("2026-12-24")),
        None,
        "not in the time grid"
    );
    assert_eq!(snap(22, 15), 15);
    assert_eq!(snap(23, 15), 30);
    assert_eq!(snap(-8, 15), -15);
}

#[test]
fn moves_keep_wall_time_across_dst() {
    // 09:00 CEST on Fri 23 Oct (07:00 UTC), moved 3 days to Mon 26 Oct
    // (CET): still 09:00 local, now 08:00 UTC.
    let meeting = instance(at("2026-10-23", "07:00"), at("2026-10-23", "08:00"), false);
    let times = changed_times(
        &meeting,
        &Berlin,
        Change {
            days: 3,
            minutes: 0,
            resize: 0,
        },
    );
    assert_eq!(times.start, at("2026-10-26", "08:00"));
    assert_eq!(times.end, at("2026-10-26", "09:00"));
    assert_eq!(times.recurrence_id.as_deref(), Some("rid"));
    // 30 minutes later and 45 minutes longer.
    let times = changed_times(
        &meeting,
        &Berlin,
        Change {
            days: 0,
            minutes: 30,
            resize: 45,
        },
    );
    assert_eq!(times.start, at("2026-10-23", "07:30"));
    assert_eq!(times.end, at("2026-10-23", "09:15"));
    // A resize never goes below 15 minutes.
    let short = changed_span(
        &local_span(&meeting, &Berlin),
        Change {
            days: 0,
            minutes: 0,
            resize: -120,
        },
    );
    assert_eq!(short.end - short.start, 15 * 60);
    // All-day events move by days; minutes are ignored.
    let holiday = instance(day("2026-12-24") * DAY, day("2026-12-25") * DAY, true);
    let times = changed_times(
        &holiday,
        &Berlin,
        Change {
            days: 2,
            minutes: 90,
            resize: 0,
        },
    );
    assert_eq!(
        (times.start, times.end),
        (day("2026-12-26") * DAY, day("2026-12-27") * DAY)
    );
    // Floating: local arithmetic.
    let mut floating = instance(at("2026-10-23", "09:00"), at("2026-10-23", "10:00"), false);
    floating.floating = true;
    let times = changed_times(
        &floating,
        &NewYork,
        Change {
            days: 1,
            minutes: 0,
            resize: 0,
        },
    );
    assert_eq!(times.start, at("2026-10-24", "09:00"));
}

// ------------------------------------------------------------------- form

fn detail(start: EventTime, end: EventTime, recurrence: Option<Recurrence>) -> EventDetail {
    EventDetail {
        calendar: "work".to_owned(),
        name: "a.ics".to_owned(),
        etag: "\"e\"".to_owned(),
        uid: "u".to_owned(),
        writable: true,
        busy: false,
        fields: EventFields {
            summary: "Review".to_owned(),
            location: String::new(),
            description: String::new(),
            start,
            end,
            alarms: vec![calcraft_core::api::Alarm { minutes_before: 30 }],
            private: false,
        },
        recurrence_id: None,
        is_override: false,
        recurrence,
        recurrence_rule: None,
        organizer: None,
        attendees: Vec::new(),
        attachments: Vec::new(),
    }
}

fn time(date: &str, time: Option<&str>, tzid: Option<&str>) -> EventTime {
    EventTime {
        date: date.to_owned(),
        time: time.map(str::to_owned),
        tzid: tzid.map(str::to_owned),
    }
}

#[test]
fn form_round_trip() {
    let rule = Recurrence {
        freq: Frequency::Weekly,
        interval: 2,
        count: None,
        until: Some("2026-12-31".to_owned()),
        weekdays: vec!["MO".to_owned(), "WE".to_owned()],
    };
    let original = detail(
        time("2026-10-19", Some("09:00"), Some("Europe/Berlin")),
        time("2026-10-19", Some("10:30"), Some("Europe/Berlin")),
        Some(rule.clone()),
    );
    let form = EventForm::from_detail(&original, "Europe/Berlin");
    assert_eq!(form.repeat, Repeat::Weekly);
    assert_eq!(form.ends, Ends::Until);
    assert_eq!(
        form.weekdays,
        [true, false, true, false, false, false, false]
    );
    let input = form.to_input(None, Vec::new()).unwrap();
    assert_eq!(input.fields, original.fields);
    assert_eq!(input.recurrence, Some(rule.clone()));
    assert!(!input.keep_recurrence);
    assert_eq!(form.zone().as_deref(), Some("Europe/Berlin"));
    assert_eq!(
        describe(&rule),
        "every 2 weeks on Mon, Wed, until 2026-12-31"
    );

    // One instance: no rule.
    let one = form
        .to_input(Some("20261021T090000".to_owned()), Vec::new())
        .unwrap();
    assert_eq!(one.recurrence, None);
    assert!(!one.keep_recurrence);
}

#[test]
fn all_day_end_is_inclusive_in_the_dialog() {
    let original = detail(
        time("2026-12-24", None, None),
        time("2026-12-27", None, None),
        None,
    );
    let mut form = EventForm::from_detail(&original, "Europe/Berlin");
    assert!(form.all_day);
    assert_eq!(form.end_date, "2026-12-26");
    let input = form.to_input(None, Vec::new()).unwrap();
    assert_eq!(input.fields.end, time("2026-12-27", None, None));
    assert_eq!(form.zone(), None, "dates have no zone");
    // Switching to timed keeps the days and uses a time zone.
    form.set_all_day(false);
    form.tzid = "UTC".to_owned();
    let input = form.to_input(None, Vec::new()).unwrap();
    assert_eq!(input.fields.start.tzid.as_deref(), Some("UTC"));
    assert_eq!(form.zone(), None, "UTC needs no definition");
}

#[test]
fn form_validation() {
    let base = EventForm::new_at("default", at("2026-10-23", "09:00"), false, "Europe/Berlin");
    assert_eq!(base.end_time, "10:00");
    assert_eq!(
        base.weekdays,
        [false, false, false, false, true, false, false]
    );
    let check = |change: &dyn Fn(&mut EventForm)| {
        let mut form = base.clone();
        change(&mut form);
        form.to_input(None, Vec::new()).err()
    };
    assert_eq!(check(&|_| {}), None);
    assert!(check(&|f| f.end_time = "08:00".to_owned()).is_some());
    assert!(check(&|f| f.start_date = "2026-02-30".to_owned()).is_some());
    assert!(check(&|f| f.start_time = "9".to_owned()).is_some());
    assert!(check(&|f| f.alarms = vec!["-5".to_owned()]).is_some());
    assert!(check(&|f| f.alarms = vec!["abc".to_owned()]).is_some());
    assert!(
        check(&|f| {
            f.repeat = Repeat::Daily;
            f.interval = "0".to_owned();
        })
        .is_some()
    );
    assert!(
        check(&|f| {
            f.repeat = Repeat::Weekly;
            f.weekdays = [false; 7];
        })
        .is_some()
    );
    assert!(
        check(&|f| {
            f.repeat = Repeat::Daily;
            f.ends = Ends::Until;
            f.until = "2026-10-01".to_owned();
        })
        .is_some()
    );
    assert!(
        check(&|f| {
            f.repeat = Repeat::Daily;
            f.ends = Ends::Count;
            f.count = "0".to_owned();
        })
        .is_some()
    );
    // A custom rule is kept.
    let mut custom = base.clone();
    custom.repeat = Repeat::Custom;
    let input = custom.to_input(None, Vec::new()).unwrap();
    assert!(input.keep_recurrence);
    assert_eq!(input.recurrence, None);
    // Floating time: no zone.
    let mut floating = base;
    floating.tzid = String::new();
    let input = floating.to_input(None, Vec::new()).unwrap();
    assert_eq!(input.fields.start.tzid, None);
    assert_eq!(crate::text::alarm_label(0), "At start");
    assert_eq!(crate::text::alarm_label(15), "15 minutes before");
    assert_eq!(crate::text::alarm_label(60), "1 hour before");
    assert_eq!(crate::text::alarm_label(2880), "2 days before");
}
