use super::*;

#[test]
fn parses_dates_and_times() {
    assert_eq!(parse_date_or_datetime("19700101").unwrap().seconds, 0);
    let dt = parse_date_or_datetime("20261021T100000Z").unwrap();
    assert_eq!(dt.seconds, 1_792_576_800);
    assert!(dt.is_utc && !dt.is_date);
    let local = parse_date_or_datetime("20261021T100000").unwrap();
    assert!(!local.is_utc);
    assert_eq!(
        parse_date_or_datetime("20240229").unwrap().seconds,
        1_709_164_800
    );
    assert_eq!(
        parse_utc_datetime("20261021T100000Z").unwrap(),
        1_792_576_800
    );
}

#[test]
fn rejects_invalid_dates() {
    for value in [
        "",
        "2026",
        "20261321",
        "20230229",
        "20261021T250000",
        "20261021T100000X",
        "20261021 100000",
        "2026-10-21",
        "２０２６１０２１",
        "+0261021",
    ] {
        assert!(parse_date_or_datetime(value).is_err(), "{value:?}");
    }
    assert!(parse_utc_datetime("20261021").is_err());
    assert!(parse_utc_datetime("20261021T100000").is_err());
}

#[test]
fn civil_roundtrip_and_weekdays() {
    for days in [-719_468, -1, 0, 1, 10_957, 19_782, 20_000, 2_932_896] {
        let (y, m, d) = civil_from_days(days);
        assert_eq!(days_from_civil(y, m, d), days);
    }
    assert_eq!(civil_from_days(0), (1970, 1, 1));
    assert_eq!(weekday(0), 3);
    // 2026-10-22 is a Thursday.
    assert_eq!(weekday(days_from_civil(2026, 10, 22)), 3);
    assert_eq!(MAX_TIME, days_from_civil(10_000, 1, 1) * 86_400);
    assert_eq!(parse_utc_offset("+0100"), Some(3600));
    assert_eq!(parse_utc_offset("-053000"), Some(-19_800));
    for bad in ["", "0100", "+01", "+2400", "+0160", "+01000", "+01:00"] {
        assert_eq!(parse_utc_offset(bad), None, "{bad:?}");
    }
}

#[test]
fn parses_durations() {
    assert_eq!(parse_duration("PT1H30M").unwrap(), 5400);
    assert_eq!(parse_duration("P1W").unwrap(), 604_800);
    assert_eq!(parse_duration("-P1DT2S").unwrap(), -86_402);
    assert_eq!(parse_duration("P0D").unwrap(), 0);
    for value in [
        "",
        "P",
        "PT",
        "1H",
        "P1H",
        "PT1D",
        "P1",
        "P999999999999W",
        "P1DT",
        "PTT1H",
    ] {
        assert!(parse_duration(value).is_err(), "{value:?}");
    }
}
