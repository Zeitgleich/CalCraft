use super::*;

const LIMITS: Limits = Limits {
    max_size: 1 << 20,
    max_components: 1000,
    max_properties: 10_000,
};

#[test]
fn rejects_dtd_entities_and_foreign_roots() {
    for bad in [
        format!(
            "<!DOCTYPE i [<!ENTITY a \"aaaa\">]><icalendar xmlns=\"{NS_XCAL}\">&a;</icalendar>"
        ),
        format!("<!DOCTYPE i SYSTEM \"file:///etc/passwd\"><icalendar xmlns=\"{NS_XCAL}\"/>"),
        "<icalendar/>".to_owned(),
        format!("<icalendar xmlns=\"{NS_XCAL}\"><vcalendar><bogus/></vcalendar></icalendar>"),
        format!(
            "<icalendar xmlns=\"{NS_XCAL}\"><vcalendar><properties><summary/></properties></vcalendar></icalendar>"
        ),
    ] {
        assert!(read(bad.as_bytes(), &LIMITS).is_err(), "{bad}");
    }
    let deep = format!(
        "<icalendar xmlns=\"{NS_XCAL}\">{}{}</icalendar>",
        "<vcalendar><components>".repeat(20),
        "</components></vcalendar>".repeat(20)
    );
    assert!(read(deep.as_bytes(), &LIMITS).is_err());
}
