//! The RRULE examples of RFC 5545 §3.8.5.3, expanded in local time (the
//! RFC lists local times in America/New_York).

use super::*;
use crate::recurrence::Budget;

fn format(t: i64) -> String {
    let (y, m, d) = civil_from_days(t.div_euclid(DAY));
    let s = t.rem_euclid(DAY);
    format!(
        "{y:04}{m:02}{d:02}T{:02}{:02}{:02}",
        s / 3600,
        s % 3600 / 60,
        s % 60
    )
}

/// Up to `n` start times; UNTIL is read as local time like the RFC lists.
fn expand(dtstart: &str, rule: &str, n: usize) -> Vec<String> {
    let start = parse_date_or_datetime(dtstart).unwrap();
    let rule = parse_rule(rule).unwrap();
    let until = rule.until.map(|u| u.seconds);
    let mut expansion = Expansion::new(&rule, start.seconds, start.is_date, until).unwrap();
    let mut budget = Budget::new(10_000_000);
    let mut out = Vec::new();
    while out.len() < n {
        match expansion.next(&mut budget).unwrap() {
            Some(t) => out.push(format(t)),
            None => break,
        }
    }
    out
}

/// Dates at 09:00.
fn at9(dates: &str) -> Vec<String> {
    dates
        .split_whitespace()
        .map(|d| format!("{d}T090000"))
        .collect()
}

#[test]
fn daily() {
    assert_eq!(
        expand("19970902T090000", "FREQ=DAILY;COUNT=10", 100),
        at9(
            "19970902 19970903 19970904 19970905 19970906 19970907 19970908 19970909 19970910 19970911"
        )
    );
    let until = expand("19970902T090000", "FREQ=DAILY;UNTIL=19971224T000000Z", 1000);
    assert_eq!(until.len(), 113);
    assert_eq!(until.last().unwrap(), "19971223T090000");
    assert_eq!(
        expand("19970902T090000", "FREQ=DAILY;INTERVAL=2", 5),
        at9("19970902 19970904 19970906 19970908 19970910")
    );
    assert_eq!(
        expand("19970902T090000", "FREQ=DAILY;INTERVAL=10;COUNT=5", 100),
        at9("19970902 19970912 19970922 19971002 19971012")
    );
}

#[test]
fn every_day_in_january_for_three_years() {
    for rule in [
        "FREQ=YEARLY;UNTIL=20000131T140000Z;BYMONTH=1;BYDAY=SU,MO,TU,WE,TH,FR,SA",
        "FREQ=DAILY;UNTIL=20000131T140000Z;BYMONTH=1",
    ] {
        let all = expand("19980101T090000", rule, 1000);
        assert_eq!(all.len(), 93, "{rule}");
        assert_eq!(all[30], "19980131T090000");
        assert_eq!(all[31], "19990101T090000");
        assert_eq!(all[92], "20000131T090000");
    }
}

#[test]
fn weekly() {
    assert_eq!(
        expand("19970902T090000", "FREQ=WEEKLY;COUNT=10", 100),
        at9(
            "19970902 19970909 19970916 19970923 19970930 19971007 19971014 19971021 19971028 19971104"
        )
    );
    let until = expand("19970902T090000", "FREQ=WEEKLY;UNTIL=19971224T000000Z", 100);
    assert_eq!(until.len(), 17);
    assert_eq!(until.last().unwrap(), "19971223T090000");
    assert_eq!(
        expand("19970902T090000", "FREQ=WEEKLY;INTERVAL=2;WKST=SU", 5),
        at9("19970902 19970916 19970930 19971014 19971028")
    );
    let tue_thu = at9(
        "19970902 19970904 19970909 19970911 19970916 19970918 19970923 19970925 19970930 19971002",
    );
    assert_eq!(
        expand(
            "19970902T090000",
            "FREQ=WEEKLY;UNTIL=19971007T000000Z;WKST=SU;BYDAY=TU,TH",
            100
        ),
        tue_thu
    );
    assert_eq!(
        expand(
            "19970902T090000",
            "FREQ=WEEKLY;COUNT=10;WKST=SU;BYDAY=TU,TH",
            100
        ),
        tue_thu
    );
    assert_eq!(
        expand(
            "19970901T090000",
            "FREQ=WEEKLY;INTERVAL=2;UNTIL=19971224T000000Z;WKST=SU;BYDAY=MO,WE,FR",
            100
        ),
        at9(
            "19970901 19970903 19970905 19970915 19970917 19970919 19970929 19971001 19971003 \
             19971013 19971015 19971017 19971027 19971029 19971031 19971110 19971112 19971114 \
             19971124 19971126 19971128 19971208 19971210 19971212 19971222"
        )
    );
    assert_eq!(
        expand(
            "19970902T090000",
            "FREQ=WEEKLY;INTERVAL=2;COUNT=8;WKST=SU;BYDAY=TU,TH",
            100
        ),
        at9("19970902 19970904 19970916 19970918 19970930 19971002 19971014 19971016")
    );
}

#[test]
fn week_start_changes_results() {
    assert_eq!(
        expand(
            "19970805T090000",
            "FREQ=WEEKLY;INTERVAL=2;COUNT=4;BYDAY=TU,SU;WKST=MO",
            100
        ),
        at9("19970805 19970810 19970819 19970824")
    );
    assert_eq!(
        expand(
            "19970805T090000",
            "FREQ=WEEKLY;INTERVAL=2;COUNT=4;BYDAY=TU,SU;WKST=SU",
            100
        ),
        at9("19970805 19970817 19970819 19970831")
    );
}

#[test]
fn monthly_by_day() {
    assert_eq!(
        expand("19970905T090000", "FREQ=MONTHLY;COUNT=10;BYDAY=1FR", 100),
        at9(
            "19970905 19971003 19971107 19971205 19980102 19980206 19980306 19980403 19980501 19980605"
        )
    );
    assert_eq!(
        expand(
            "19970905T090000",
            "FREQ=MONTHLY;UNTIL=19971224T000000Z;BYDAY=1FR",
            100
        ),
        at9("19970905 19971003 19971107 19971205")
    );
    assert_eq!(
        expand(
            "19970907T090000",
            "FREQ=MONTHLY;INTERVAL=2;COUNT=10;BYDAY=1SU,-1SU",
            100
        ),
        at9(
            "19970907 19970928 19971102 19971130 19980104 19980125 19980301 19980329 19980503 19980531"
        )
    );
    assert_eq!(
        expand("19970922T090000", "FREQ=MONTHLY;COUNT=6;BYDAY=-2MO", 100),
        at9("19970922 19971020 19971117 19971222 19980119 19980216")
    );
    assert_eq!(
        expand("19970902T090000", "FREQ=MONTHLY;INTERVAL=2;BYDAY=TU", 10),
        at9(
            "19970902 19970909 19970916 19970923 19970930 19971104 19971111 19971118 19971125 19980106"
        )
    );
}

#[test]
fn monthly_by_month_day() {
    assert_eq!(
        expand("19970928T090000", "FREQ=MONTHLY;BYMONTHDAY=-3", 6),
        at9("19970928 19971029 19971128 19971229 19980129 19980226")
    );
    assert_eq!(
        expand(
            "19970902T090000",
            "FREQ=MONTHLY;COUNT=10;BYMONTHDAY=2,15",
            100
        ),
        at9(
            "19970902 19970915 19971002 19971015 19971102 19971115 19971202 19971215 19980102 19980115"
        )
    );
    assert_eq!(
        expand(
            "19970930T090000",
            "FREQ=MONTHLY;COUNT=10;BYMONTHDAY=1,-1",
            100
        ),
        at9(
            "19970930 19971001 19971031 19971101 19971130 19971201 19971231 19980101 19980131 19980201"
        )
    );
    assert_eq!(
        expand(
            "19970910T090000",
            "FREQ=MONTHLY;INTERVAL=18;COUNT=10;BYMONTHDAY=10,11,12,13,14,15",
            100
        ),
        at9(
            "19970910 19970911 19970912 19970913 19970914 19970915 19990310 19990311 19990312 19990313"
        )
    );
    // Invalid dates (February 30) are ignored, not moved.
    assert_eq!(
        expand(
            "20070115T090000",
            "FREQ=MONTHLY;BYMONTHDAY=15,30;COUNT=5",
            100
        ),
        at9("20070115 20070130 20070215 20070315 20070330")
    );
}

#[test]
fn yearly() {
    assert_eq!(
        expand("19970610T090000", "FREQ=YEARLY;COUNT=10;BYMONTH=6,7", 100),
        at9(
            "19970610 19970710 19980610 19980710 19990610 19990710 20000610 20000710 20010610 20010710"
        )
    );
    assert_eq!(
        expand(
            "19970310T090000",
            "FREQ=YEARLY;INTERVAL=2;COUNT=10;BYMONTH=1,2,3",
            100
        ),
        at9(
            "19970310 19990110 19990210 19990310 20010110 20010210 20010310 20030110 20030210 20030310"
        )
    );
    assert_eq!(
        expand(
            "19970101T090000",
            "FREQ=YEARLY;INTERVAL=3;COUNT=10;BYYEARDAY=1,100,200",
            100
        ),
        at9(
            "19970101 19970410 19970719 20000101 20000409 20000718 20030101 20030410 20030719 20060101"
        )
    );
    assert_eq!(
        expand("19970519T090000", "FREQ=YEARLY;BYDAY=20MO", 3),
        at9("19970519 19980518 19990517")
    );
    assert_eq!(
        expand("19970512T090000", "FREQ=YEARLY;BYWEEKNO=20;BYDAY=MO", 3),
        at9("19970512 19980511 19990517")
    );
    assert_eq!(
        expand("19970313T090000", "FREQ=YEARLY;BYMONTH=3;BYDAY=TH", 11),
        at9(
            "19970313 19970320 19970327 19980305 19980312 19980319 19980326 19990304 19990311 19990318 19990325"
        )
    );
    assert_eq!(
        expand("19970605T090000", "FREQ=YEARLY;BYDAY=TH;BYMONTH=6,7,8", 14),
        at9(
            "19970605 19970612 19970619 19970626 19970703 19970710 19970717 19970724 19970731 \
             19970807 19970814 19970821 19970828 19980604"
        )
    );
    assert_eq!(
        expand(
            "19961105T090000",
            "FREQ=YEARLY;INTERVAL=4;BYMONTH=11;BYDAY=TU;BYMONTHDAY=2,3,4,5,6,7,8",
            3
        ),
        at9("19961105 20001107 20041102")
    );
}

#[test]
fn combined_limits_and_set_positions() {
    // Every Friday the 13th (the RFC excludes DTSTART itself with EXDATE).
    assert_eq!(
        expand("19970902T090000", "FREQ=MONTHLY;BYDAY=FR;BYMONTHDAY=13", 6)[1..],
        at9("19980213 19980313 19981113 19990813 20001013")
    );
    assert_eq!(
        expand(
            "19970913T090000",
            "FREQ=MONTHLY;BYDAY=SA;BYMONTHDAY=7,8,9,10,11,12,13",
            10
        ),
        at9(
            "19970913 19971011 19971108 19971213 19980110 19980207 19980307 19980411 19980509 19980613"
        )
    );
    assert_eq!(
        expand(
            "19970904T090000",
            "FREQ=MONTHLY;COUNT=3;BYDAY=TU,WE,TH;BYSETPOS=3",
            100
        ),
        at9("19970904 19971007 19971106")
    );
    assert_eq!(
        expand(
            "19970929T090000",
            "FREQ=MONTHLY;BYDAY=MO,TU,WE,TH,FR;BYSETPOS=-2",
            7
        ),
        at9("19970929 19971030 19971127 19971230 19980129 19980226 19980330")
    );
}

#[test]
fn sub_daily() {
    assert_eq!(
        expand(
            "19970902T090000",
            "FREQ=HOURLY;INTERVAL=3;UNTIL=19970902T170000Z",
            100
        ),
        ["19970902T090000", "19970902T120000", "19970902T150000"]
    );
    assert_eq!(
        expand("19970902T090000", "FREQ=MINUTELY;INTERVAL=15;COUNT=6", 100),
        [
            "19970902T090000",
            "19970902T091500",
            "19970902T093000",
            "19970902T094500",
            "19970902T100000",
            "19970902T101500"
        ]
    );
    assert_eq!(
        expand("19970902T090000", "FREQ=MINUTELY;INTERVAL=90;COUNT=4", 100),
        [
            "19970902T090000",
            "19970902T103000",
            "19970902T120000",
            "19970902T133000"
        ]
    );
    let daily = expand(
        "19970902T090000",
        "FREQ=DAILY;BYHOUR=9,10,11,12,13,14,15,16;BYMINUTE=0,20,40",
        30,
    );
    let minutely = expand(
        "19970902T090000",
        "FREQ=MINUTELY;INTERVAL=20;BYHOUR=9,10,11,12,13,14,15,16",
        30,
    );
    assert_eq!(daily, minutely);
    assert_eq!(daily[23], "19970902T164000");
    assert_eq!(daily[24], "19970903T090000");
}

#[test]
fn leap_day_and_dates() {
    assert_eq!(
        expand("20240229", "FREQ=YEARLY", 4),
        [
            "20240229T000000",
            "20280229T000000",
            "20320229T000000",
            "20360229T000000"
        ]
    );
    // 2100 is not a leap year.
    assert_eq!(
        expand("20960229", "FREQ=YEARLY;COUNT=3", 10),
        ["20960229T000000", "21040229T000000", "21080229T000000"]
    );
    assert_eq!(
        expand("20261021", "FREQ=WEEKLY;UNTIL=20261104", 10),
        ["20261021T000000", "20261028T000000", "20261104T000000"]
    );
}

#[test]
fn dtstart_counts_even_if_unsynchronised() {
    // DTSTART (a Wednesday) is returned but not counted against COUNT.
    assert_eq!(
        expand("20261021T100000", "FREQ=WEEKLY;BYDAY=MO;COUNT=2", 10),
        ["20261021T100000", "20261026T100000", "20261102T100000"]
    );
}

#[test]
fn empty_rules_terminate_within_budget() {
    let rule = parse_rule("FREQ=YEARLY;BYMONTH=2;BYMONTHDAY=30").unwrap();
    let start = parse_date_or_datetime("20260101T000000").unwrap().seconds;
    let mut expansion = Expansion::new(&rule, start, false, None).unwrap();
    let mut budget = Budget::new(1_000_000);
    assert_eq!(expansion.next(&mut budget), Ok(Some(start)));
    // Ends at year 9999 without an instance, well within the budget.
    assert_eq!(expansion.next(&mut budget), Ok(None));

    let rule = parse_rule("FREQ=SECONDLY;BYMONTH=1").unwrap();
    let start = parse_date_or_datetime("20260201T000000").unwrap().seconds;
    let mut expansion = Expansion::new(&rule, start, false, None).unwrap();
    let mut budget = Budget::new(100_000);
    expansion.next(&mut budget).unwrap();
    // Non-matching days are skipped as a whole: January 2027 is found.
    assert_eq!(
        expansion.next(&mut budget).unwrap().map(format).as_deref(),
        Some("20270101T000000")
    );

    let rule = parse_rule("FREQ=SECONDLY").unwrap();
    let mut expansion = Expansion::new(&rule, start, false, None).unwrap();
    let mut budget = Budget::new(1_000);
    let result = (0..10_000).try_for_each(|_| expansion.next(&mut budget).map(|_| ()));
    assert_eq!(result, Err(Inexact));
}

#[test]
fn skipping_ahead_matches_full_expansion() {
    let rule = parse_rule("FREQ=WEEKLY;INTERVAL=3;BYDAY=MO,FR").unwrap();
    let start = parse_date_or_datetime("20000103T080000").unwrap().seconds;
    let target = parse_date_or_datetime("20261001T000000").unwrap().seconds;
    let mut budget = Budget::new(10_000_000);
    let mut full = Expansion::new(&rule, start, false, None).unwrap();
    let expected: Vec<i64> = std::iter::from_fn(|| full.next(&mut budget).unwrap())
        .filter(|&t| t >= target)
        .take(5)
        .collect();
    let mut skipping = Expansion::new(&rule, start, false, None).unwrap();
    skipping.skip_to(target);
    let mut budget = Budget::new(100);
    let got: Vec<i64> = std::iter::from_fn(|| skipping.next(&mut budget).unwrap())
        .filter(|&t| t >= target)
        .take(5)
        .collect();
    assert_eq!(got, expected);
}
