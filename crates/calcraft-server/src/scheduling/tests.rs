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
