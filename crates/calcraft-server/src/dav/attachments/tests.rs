use super::*;

#[test]
fn queries_are_parsed_strictly() {
    let query = parse_query("action=attachment-add").unwrap();
    assert_eq!(query.action, Action::Add);
    assert!(query.managed_id.is_none() && query.rids.is_none());

    let query =
        parse_query("action=attachment-remove&managed-id=abc&rid=M%2C20260105T100000Z").unwrap();
    assert_eq!(query.action, Action::Remove);
    assert_eq!(query.managed_id.as_deref(), Some("abc"));
    assert_eq!(
        query.rids.unwrap(),
        [Rid::Master, Rid::Instance("20260105T100000Z".to_owned())]
    );

    for (bad, status) in [
        ("action=attachment-delete", 403),
        ("action=attachment-add&action=attachment-add", 403),
        ("managed-id=x", 403),
        ("action=attachment-add&managed-id=x", 400),
        ("action=attachment-update", 403),
        ("action=attachment-remove", 403),
        ("action=attachment-update&managed-id=x&rid=M", 403),
        ("action=attachment-add&rid=bogus", 403),
        ("action=attachment-add&rid=M&rid=M", 403),
        ("action=attachment-add&rid=%0a", 400),
        ("action=attachment-add&rid=%zz", 400),
    ] {
        let response = parse_query(bad).unwrap_err();
        assert_eq!(response.status().as_u16(), status, "{bad}");
    }
    assert!(parse_query(&"a=b&".repeat(2000)).is_err());
}

#[test]
fn attachment_posts_are_recognised_by_their_action() {
    assert!(is_attachment_post(Some("action=attachment-add")));
    assert!(is_attachment_post(Some("x=1&action")));
    assert!(!is_attachment_post(Some("actions=1")));
    assert!(!is_attachment_post(None));
}
