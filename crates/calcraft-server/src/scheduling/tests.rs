use super::*;

#[test]
fn addresses_are_normalised() {
    assert_eq!(
        normalize("MAILTO:Alice@Example.ORG"),
        "mailto:alice@example.org"
    );
    assert_eq!(normalize(" mailto:bob@x.org "), "mailto:bob@x.org");
    assert_eq!(normalize("/dav/principals/alice/"), "principal:alice");
    assert_eq!(normalize("/dav/principals/alice"), "principal:alice");
    assert_eq!(
        normalize("https://cal.example.org/dav/principals/alice/"),
        "principal:alice"
    );
    // Not a valid user name: stays as it is (and matches nobody).
    assert_eq!(
        normalize("/dav/principals/Alice/"),
        "/dav/principals/alice/"
    );
    assert_eq!(normalize("/dav/principals/a/b/"), "/dav/principals/a/b/");
    assert_eq!(normalize("urn:uuid:ABC"), "urn:uuid:abc");
}

#[test]
fn emails_are_validated() {
    assert_eq!(
        normalize_email("Alice.Smith@Example.org").as_deref(),
        Some("alice.smith@example.org")
    );
    assert_eq!(
        normalize_email("mailto:a+b@x.example").as_deref(),
        Some("a+b@x.example")
    );
    for bad in [
        "",
        "alice",
        "@example.org",
        "alice@",
        "alice@localhost",
        "a..b@example.org",
        ".a@example.org",
        "a@-x.org",
        "a b@example.org",
        "ä@example.org",
        "a@exämple.org",
        "a@b@c.org",
        "a\r\n@example.org",
    ] {
        assert_eq!(normalize_email(bad), None, "{bad:?}");
    }
    let long = format!("{}@example.org", "a".repeat(65));
    assert_eq!(normalize_email(&long), None);
}

#[test]
fn rate_limit_per_user() {
    let scheduling = Scheduling::new(&SchedulingConfig {
        enabled: true,
        max_recipients_per_hour: 5,
        ..SchedulingConfig::default()
    });
    assert_eq!(scheduling.allow(1, 3), 3);
    assert_eq!(scheduling.allow(1, 3), 2);
    assert_eq!(scheduling.allow(1, 1), 0);
    // Other users have their own budget.
    assert_eq!(scheduling.allow(2, 5), 5);
}

#[test]
fn derived_addresses_need_a_domain() {
    let name: Username = "alice".parse().unwrap();
    let without = Scheduling::new(&SchedulingConfig::default());
    assert_eq!(without.derived_address(&name), None);
    let with = Scheduling::new(&SchedulingConfig {
        domain: Some("example.org".into()),
        ..SchedulingConfig::default()
    });
    assert_eq!(
        with.derived_address(&name).as_deref(),
        Some("mailto:alice@example.org")
    );
}

#[test]
fn generated_values_are_random() {
    assert_ne!(new_schedule_tag(), new_schedule_tag());
    assert!(new_resource_name().ends_with(".ics"));
    assert_eq!(dtstamp().len(), 16);
}

#[test]
fn retries_of_one_request_are_charged_once() {
    let scheduling = Scheduling::new(&SchedulingConfig {
        enabled: true,
        max_recipients_per_hour: 5,
        ..SchedulingConfig::default()
    });
    let mut grant = Grant::default();
    assert_eq!(grant.take(&scheduling, 1, 3), 3);
    // The same three recipients again (a retry): nothing more is charged.
    assert_eq!(grant.take(&scheduling, 1, 3), 3);
    assert_eq!(scheduling.allow(1, 5), 2);
}

#[test]
fn delivered_bytes_are_limited_per_recipient() {
    let scheduling = Scheduling::new(&SchedulingConfig {
        enabled: true,
        max_delivered_bytes_per_hour: 100,
        ..SchedulingConfig::default()
    });
    assert!(scheduling.allow_bytes(7, 60));
    // All or nothing.
    assert!(!scheduling.allow_bytes(7, 60));
    assert!(scheduling.allow_bytes(7, 40));
    assert!(scheduling.allow_bytes(8, 100));
}

#[test]
fn too_many_busy_periods_never_become_free() {
    use calcraft_ical::itip::BusyType;
    let periods: Vec<(i64, i64, BusyType)> = (0..20_000)
        .map(|i| (i * 10, i * 10 + 5, BusyType::Busy))
        .collect();
    let bounded = freebusy::bounded(periods);
    assert!(bounded.len() <= 10_000);
    let last = bounded.last().unwrap();
    assert_eq!(last.1, 199_995);
    // Everything after the cut is covered by the last period.
    assert!(last.0 <= 9_999 * 10);
}
