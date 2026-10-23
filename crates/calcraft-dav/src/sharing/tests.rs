use super::*;

const NS: &str = r#"xmlns:D="DAV:" xmlns:CS="http://calendarserver.org/ns/""#;

#[test]
fn parses_share_set_and_remove() {
    let body = format!(
        "<CS:share {NS}><CS:set><D:href>mailto:bob@example.org</D:href><CS:common-name>Bob</CS:common-name>\
         <CS:summary>Team</CS:summary><CS:read-write/></CS:set>\
         <CS:set><D:href> /dav/principals/carol/ </D:href><CS:read/></CS:set>\
         <CS:remove><D:href>mailto:dave@example.org</D:href></CS:remove></CS:share>"
    );
    let SharingPost::Share(request) = SharingPost::parse(body.as_bytes()).unwrap() else {
        panic!("not a share request");
    };
    assert_eq!(
        request.set,
        vec![
            ShareSet {
                href: "mailto:bob@example.org".to_owned(),
                read_write: true
            },
            ShareSet {
                href: "/dav/principals/carol/".to_owned(),
                read_write: false
            },
        ]
    );
    assert_eq!(request.remove, vec!["mailto:dave@example.org".to_owned()]);
}

#[test]
fn parses_invite_reply() {
    let body = format!(
        "<CS:invite-reply {NS}><D:href>mailto:alice@example.org</D:href><CS:invite-accepted/>\
         <CS:hosturl><D:href>/dav/calendars/bob/team/</D:href></CS:hosturl>\
         <CS:in-reply-to>0123456789abcdef0123456789abcdef</CS:in-reply-to><CS:summary>x</CS:summary></CS:invite-reply>"
    );
    assert_eq!(
        SharingPost::parse(body.as_bytes()).unwrap(),
        SharingPost::InviteReply(InviteReply {
            accepted: true,
            host_url: Some("/dav/calendars/bob/team/".to_owned()),
            in_reply_to: Some("0123456789abcdef0123456789abcdef".to_owned()),
        })
    );
    let declined = format!(
        "<CS:invite-reply {NS}><CS:invite-declined/><CS:in-reply-to>x</CS:in-reply-to></CS:invite-reply>"
    );
    assert!(matches!(
        SharingPost::parse(declined.as_bytes()).unwrap(),
        SharingPost::InviteReply(InviteReply {
            accepted: false,
            ..
        })
    ));
}

#[test]
fn rejects_malformed_bodies() {
    let bad = [
        // Wrong root, no namespace, empty.
        format!("<D:propfind {NS}/>"),
        "<share/>".to_owned(),
        format!("<CS:share {NS}/>"),
        // Both or neither access level.
        format!("<CS:share {NS}><CS:set><D:href>mailto:a@b</D:href></CS:set></CS:share>"),
        format!(
            "<CS:share {NS}><CS:set><D:href>mailto:a@b</D:href><CS:read/><CS:read-write/></CS:set></CS:share>"
        ),
        // No, two or empty hrefs, control characters.
        format!("<CS:share {NS}><CS:set><CS:read/></CS:set></CS:share>"),
        format!(
            "<CS:share {NS}><CS:remove><D:href>a</D:href><D:href>b</D:href></CS:remove></CS:share>"
        ),
        format!("<CS:share {NS}><CS:remove><D:href> </D:href></CS:remove></CS:share>"),
        format!("<CS:share {NS}><CS:remove><D:href>a&#10;b</D:href></CS:remove></CS:share>"),
        // Unknown elements.
        format!("<CS:share {NS}><CS:other/></CS:share>"),
        // Invite replies without a decision or without a target.
        format!("<CS:invite-reply {NS}><CS:in-reply-to>x</CS:in-reply-to></CS:invite-reply>"),
        format!("<CS:invite-reply {NS}><CS:invite-accepted/></CS:invite-reply>"),
        format!(
            "<CS:invite-reply {NS}><CS:invite-accepted/><CS:invite-declined/><CS:in-reply-to>x</CS:in-reply-to></CS:invite-reply>"
        ),
        // Entities are refused by the parser.
        format!(
            "<!DOCTYPE x [<!ENTITY e \"y\">]><CS:share {NS}><CS:remove><D:href>&e;</D:href></CS:remove></CS:share>"
        ),
    ];
    for body in bad {
        assert!(SharingPost::parse(body.as_bytes()).is_err(), "{body}");
    }
}

#[test]
fn limits_size_and_entries() {
    let entry = "<CS:remove><D:href>mailto:a@b</D:href></CS:remove>";
    let many = format!(
        "<CS:share {NS}>{}</CS:share>",
        entry.repeat(MAX_SHARE_ENTRIES + 1)
    );
    assert!(SharingPost::parse(many.as_bytes()).is_err());
    let ok = format!(
        "<CS:share {NS}>{}</CS:share>",
        entry.repeat(MAX_SHARE_ENTRIES)
    );
    assert!(SharingPost::parse(ok.as_bytes()).is_ok());
    let long = format!(
        "<CS:share {NS}><CS:remove><D:href>{}</D:href></CS:remove></CS:share>",
        "a".repeat(MAX_VALUE_LEN + 1)
    );
    assert!(SharingPost::parse(long.as_bytes()).is_err());
    let huge = vec![b' '; MAX_SHARE_BODY + 1];
    assert_eq!(SharingPost::parse(&huge), Err(DavError::TooLarge));
}
