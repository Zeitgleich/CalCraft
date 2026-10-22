use super::*;
use crate::{parse, write::to_string};

const LIMITS: Limits = Limits {
    max_size: 1 << 20,
    max_components: 1000,
    max_properties: 10_000,
};

fn json_of(ical: &str, dialect: Dialect) -> String {
    let mut out = String::new();
    write_component(&parse(ical.as_bytes()).unwrap(), dialect, &mut out);
    out
}

#[test]
fn writes_rfc_7265_shapes() {
    let json = json_of(
        "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nBEGIN:VEVENT\r\nUID:a\r\nDTSTART;TZID=Europe/Berlin:20261021T100000\r\nRRULE:FREQ=DAILY;COUNT=5\r\nGEO:1.5;-2\r\nSUMMARY:\"Q\"\\nR\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n",
        Dialect::ICal,
    );
    let value: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(value[0], "vcalendar");
    let event = &value[2][0];
    assert_eq!(event[0], "vevent");
    assert_eq!(
        event[1][1],
        serde_json::json!(["dtstart", {"tzid": "Europe/Berlin"}, "date-time", "2026-10-21T10:00:00"])
    );
    assert_eq!(
        event[1][2],
        serde_json::json!(["rrule", {}, "recur", {"freq": "DAILY", "count": 5}])
    );
    assert_eq!(
        event[1][3],
        serde_json::json!(["geo", {}, "float", [1.5, -2]])
    );
    assert_eq!(event[1][4][3], "\"Q\"\nR");
}

#[test]
fn rejects_hostile_json() {
    let deep = format!("{}{}", "[".repeat(100_000), "]".repeat(100_000));
    assert_eq!(
        read(deep.as_bytes(), Dialect::ICal, &LIMITS),
        Err(IcalError::Limit("JSON nested too deeply"))
    );
    let wide = format!("[\"vcalendar\",[{}],[]]", "0,".repeat(100_000));
    assert!(read(wide.as_bytes(), Dialect::ICal, &LIMITS).is_err());
    for bad in [
        &b"{}"[..],
        b"[\"vcalendar\"]",
        b"[\"vcalendar\",[[\"summary\",{},\"text\"]],[]]",
        b"[\"vcal endar\",[],[]]",
        b"[\"vcalendar\",[[\"x\\r\\ny\",{},\"text\",\"v\"]],[]]",
        b"[\"vcalendar\",[[\"x-a\",{},\"unknown\",\"v\\r\\nBEGIN:VEVENT\"]],[]]",
        b"[\"vcalendar\",[[\"summary\",{},\"text\",null]],[]]",
        b"not json",
    ] {
        let result = read(bad, Dialect::ICal, &LIMITS)
            .and_then(|c| c.iter().map(to_string).collect::<Result<Vec<_>, _>>());
        assert!(result.is_err(), "{}", String::from_utf8_lossy(bad));
    }
}
