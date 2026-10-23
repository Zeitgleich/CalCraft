use axum::http::Method;

use super::*;

#[test]
fn managed_ids_are_random_hex() {
    let a = new_managed_id();
    let b = new_managed_id();
    assert_ne!(a, b);
    assert!(calcraft_store::valid_managed_id(&a), "{a}");
}

#[test]
fn only_uploads_get_the_larger_body_limit() {
    let post = Method::POST;
    assert!(is_upload(
        &post,
        "/dav/calendars/a/default/e.ics",
        Some("action=attachment-add")
    ));
    assert!(is_upload(
        &post,
        "/dav/calendars/a/default/e.ics",
        Some("action=attachment-update&managed-id=x")
    ));
    assert!(!is_upload(
        &post,
        "/dav/calendars/a/default/e.ics",
        Some("action=attachment-remove")
    ));
    assert!(!is_upload(&post, "/dav/calendars/a/default/e.ics", None));
    assert!(!is_upload(
        &post,
        "/api/v1/import",
        Some("action=attachment-add")
    ));
    assert!(!is_upload(
        &Method::PUT,
        "/dav/x",
        Some("action=attachment-add")
    ));
    assert!(!is_upload(&post, "/dav/x", Some("xaction=attachment-add")));
}

#[test]
fn urls_come_from_the_configured_base() {
    let config = AttachmentsConfig {
        enabled: true,
        base_url: Some("https://cal.example.org/".to_owned()),
        ..AttachmentsConfig::default()
    };
    let attachments = Attachments::new(&config);
    assert_eq!(
        attachments.url("0123"),
        "https://cal.example.org/dav/attachments/0123"
    );
}

#[test]
fn slots_are_bounded_per_user_and_server_wide() {
    let slots = Slots::new(3, 2);
    let a1 = slots.try_take(1).unwrap();
    let a2 = slots.try_take(1).unwrap();
    // One user cannot take more than two …
    assert!(slots.try_take(1).is_none());
    // … others still can, up to the server-wide limit.
    let b1 = slots.try_take(2).unwrap();
    assert!(slots.try_take(3).is_none());
    drop(a1);
    let c1 = slots.try_take(3).unwrap();
    // Server-wide limit again.
    assert!(slots.try_take(1).is_none());
    drop((a2, b1, c1));
    let again: Vec<_> = (0..2).map(|_| slots.try_take(1).unwrap()).collect();
    assert_eq!(again.len(), 2);
}
