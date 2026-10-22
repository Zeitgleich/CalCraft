use super::*;

#[test]
fn parses_and_validates() {
    let rule = parse_rule("FREQ=MONTHLY;BYDAY=-1FR,2MO;BYMONTHDAY=-3;wkst=su").unwrap();
    assert_eq!(rule.freq, Freq::Monthly);
    assert_eq!(rule.by_day, vec![(-1, 4), (2, 0)]);
    assert_eq!(rule.by_month_day, vec![-3]);
    assert_eq!(rule.wkst, 6);
    assert!(rule.supported);
    for bad in [
        "",
        "COUNT=3",
        "FREQ=FORTNIGHTLY",
        "FREQ=DAILY;INTERVAL=0",
        "FREQ=DAILY;INTERVAL=-1",
        "FREQ=DAILY;COUNT=-1",
        "FREQ=DAILY;COUNT=0",
        "FREQ=DAILY;COUNT=2;UNTIL=20261101T000000Z",
        "FREQ=DAILY;COUNT=2;COUNT=3;UNTIL=20261101T000000Z",
        "FREQ=DAILY;FREQ=FORTNIGHTLY",
        "FREQ=DAILY;UNTIL=99999",
        "FREQ=DAILY;",
        "FREQ=DAILY;BYHOUR",
    ] {
        assert!(parse_rule(bad).is_err(), "{bad:?}");
    }
    // Odd but harmless: accepted, not expanded.
    for odd in [
        "FREQ=DAILY;FREQ=WEEKLY",
        "FREQ=DAILY;BYHOUR=24",
        "FREQ=DAILY;BYSECOND=-1",
        "FREQ=MONTHLY;BYMONTHDAY=0",
        "FREQ=MONTHLY;BYMONTHDAY=32",
        "FREQ=MONTHLY;BYMONTHDAY=+0001",
        "FREQ=YEARLY;BYYEARDAY=367",
        "FREQ=YEARLY;BYWEEKNO=54",
        "FREQ=YEARLY;BYMONTH=13",
        "FREQ=YEARLY;BYMONTH=+1",
        "FREQ=YEARLY;BYSETPOS=0",
        "FREQ=MONTHLY;BYDAY=0MO",
        "FREQ=MONTHLY;BYDAY=54MO",
        "FREQ=MONTHLY;BYDAY=XX",
        "FREQ=MONTHLY;BYDAY=",
        "FREQ=MONTHLY;BYDAY=MO,",
        "FREQ=DAILY;WKST=XY",
        "FREQ=DAILY;BYHOUR=1;BYHOUR=2",
    ] {
        assert!(!parse_rule(odd).unwrap().supported, "{odd:?}");
    }
    assert_eq!(parse_rule("FREQ=DAILY;COUNT=+3").unwrap().count, Some(3));
    assert!(
        !parse_rule(&format!(
            "FREQ=YEARLY;BYYEARDAY={}",
            vec!["1"; 1001].join(",")
        ))
        .unwrap()
        .supported
    );
    assert!(
        !parse_rule(&format!("FREQ=DAILY{}", ";X-A=1".repeat(40)))
            .unwrap()
            .supported
    );
    for unsupported in [
        "FREQ=DAILY;X-FOO=1",
        "RSCALE=HEBREW;FREQ=YEARLY",
        "FREQ=MONTHLY;BYWEEKNO=1",
        "FREQ=WEEKLY;BYMONTHDAY=1",
        "FREQ=DAILY;BYYEARDAY=1",
        "FREQ=WEEKLY;BYDAY=1MO",
        "FREQ=YEARLY;BYWEEKNO=1;BYDAY=1MO",
    ] {
        assert!(!parse_rule(unsupported).unwrap().supported, "{unsupported}");
    }
}

#[test]
fn week_numbers_follow_iso_with_monday_start() {
    // 2027-01-01 (Friday) belongs to week 53 of 2026.
    let day = days_from_civil(2027, 1, 1);
    assert_eq!(week_number(day, 2027, 0), (53, 53));
    // 2026-12-28 (Monday) starts week 53.
    assert_eq!(week_number(days_from_civil(2026, 12, 28), 2026, 0).0, 53);
    // 2024-12-30 (Monday) is in week 1 of 2025.
    assert_eq!(week_number(days_from_civil(2024, 12, 30), 2024, 0), (1, 52));
}
