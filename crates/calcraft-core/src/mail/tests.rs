use super::*;

#[test]
fn single_and_multi_line_replies() {
    let replies = parse_replies(b"220 mail.example.org ESMTP\r\n").unwrap();
    assert_eq!(replies.len(), 1);
    assert_eq!(replies[0].code, 220);
    assert!(replies[0].is_positive());

    let replies = parse_replies(
        b"250-mail.example.org\r\n250-SIZE 1000000\r\n250-STARTTLS\r\n250-AUTH LOGIN plain\r\n250 8BITMIME\r\n",
    )
    .unwrap();
    assert_eq!(replies.len(), 1);
    assert_eq!(replies[0].lines.len(), 5);
    let ext = Extensions::from_ehlo(&replies[0]);
    assert!(ext.starttls && ext.auth_plain && ext.auth_login);
    assert_eq!(ext.size, Some(1_000_000));

    // Bare LF and a code without text are accepted.
    let replies = parse_replies(b"451\n354 go ahead\n").unwrap();
    assert_eq!(replies[0].code, 451);
    assert!(replies[0].is_transient());
    assert_eq!(replies[1].code, 354);
}

#[test]
fn malformed_replies_are_rejected() {
    for bad in [
        &b"25 short\r\n"[..],
        b"abc text\r\n",
        b"250xtext\r\n",
        b"100 informational\r\n",
        b"600 out of range\r\n",
        b"250-first\r\n251 second\r\n",
        b"250 a\rb\r\n",
        b"250 nul\0\r\n",
    ] {
        assert!(
            parse_replies(bad).is_err(),
            "{:?}",
            String::from_utf8_lossy(bad)
        );
    }
    let long = format!("250 {}\r\n", "x".repeat(MAX_REPLY_LINE));
    assert_eq!(parse_replies(long.as_bytes()), Err(ReplyError::LineTooLong));
    let many = "250-x\r\n".repeat(MAX_REPLY_LINES + 1);
    assert_eq!(
        parse_replies(many.as_bytes()),
        Err(ReplyError::TooManyLines)
    );
}

#[test]
fn addresses() {
    assert_eq!(
        normalize_address("Alice.Smith+cal@Example.ORG").as_deref(),
        Some("alice.smith+cal@example.org")
    );
    for bad in [
        "alice",
        "alice@localhost",
        "@example.org",
        "alice@example.org>\r\nRCPT TO:<x@y.z",
        "alice@example.org\r\n",
        " alice@example.org",
        "a lice@example.org",
        "\"a\"@example.org",
        "alice@exa_mple.org",
        "ålice@example.org",
        "alice@-example.org",
        ".alice@example.org",
        "al..ice@example.org",
        "<alice@example.org>",
    ] {
        assert!(normalize_address(bad).is_none(), "{bad:?}");
    }
    let long = format!("{}@{}.org", "a".repeat(64), "b".repeat(200));
    assert!(normalize_address(&long).is_none());
}

#[test]
fn single_line_removes_control_and_invisible_characters() {
    assert_eq!(
        single_line("  Team\r\nBcc: evil@example.org\tmeeting \u{202E}x\u{200B} "),
        "Team Bcc: evil@example.org meeting x"
    );
    assert_eq!(single_line("\r\n"), "");
}

#[test]
fn truncation_counts_characters() {
    assert_eq!(truncate_chars("äöü", 3), "äöü");
    assert_eq!(truncate_chars("äöüß", 3), "äö…");
}

#[test]
fn base64_matches_rfc_4648_vectors() {
    for (input, expected) in [
        ("", ""),
        ("f", "Zg=="),
        ("fo", "Zm8="),
        ("foo", "Zm9v"),
        ("foob", "Zm9vYg=="),
        ("fooba", "Zm9vYmE="),
        ("foobar", "Zm9vYmFy"),
    ] {
        assert_eq!(base64(input.as_bytes()), expected);
    }
}

#[test]
fn encoded_words_for_non_ascii_subjects() {
    assert_eq!(encode_header_text("Weekly sync", 9), "Weekly sync");
    let encoded = encode_header_text("Grüße aus Köln", 9);
    assert_eq!(encoded, "=?UTF-8?B?R3LDvMOfZSBhdXMgS8O2bG4=?="); // fits one word
    // A lookalike of an encoded word is encoded itself.
    assert!(encode_header_text("=?UTF-8?B?x?=", 9).starts_with("=?UTF-8?B?PT9"));
    // Long text is split into words of at most 75 characters without
    // splitting characters.
    let long = "ä".repeat(100);
    let encoded = encode_header_text(&long, 9);
    for word in encoded.split("\r\n ") {
        assert!(word.len() <= 75, "{word}");
        assert!(word.starts_with("=?UTF-8?B?") && word.ends_with("?="));
    }
    assert_eq!(encoded.split("\r\n ").count(), 6);
}

#[test]
fn quoted_printable_body() {
    assert_eq!(quoted_printable("plain text"), "plain text");
    assert_eq!(quoted_printable("a=b"), "a=3Db");
    assert_eq!(quoted_printable("Grüße"), "Gr=C3=BC=C3=9Fe");
    assert_eq!(
        quoted_printable("line \nnext\r\nlast\t"),
        "line=20\r\nnext\r\nlast=09"
    );
    // A dot at the start of a line is encoded, so no line starts with one.
    assert_eq!(quoted_printable(".\n..x"), "=2E\r\n=2E.x");
    // Long lines get soft breaks below 77 characters.
    let encoded = quoted_printable(&"x".repeat(200));
    for line in encoded.split("\r\n") {
        assert!(line.len() <= 76, "{line}");
    }
    let joined: String = encoded.replace("=\r\n", "");
    assert_eq!(joined, "x".repeat(200));
    // A dot that lands at the start of a continuation line is encoded too.
    let encoded = quoted_printable(&format!("{}.", "x".repeat(75)));
    assert!(
        encoded.split("\r\n").all(|l| !l.starts_with('.')),
        "{encoded}"
    );
}

#[test]
fn dates() {
    assert_eq!(rfc5322_date(0), "Thu, 01 Jan 1970 00:00:00 +0000");
    assert_eq!(
        rfc5322_date(1_792_929_600),
        "Sun, 25 Oct 2026 12:00:00 +0000"
    );
    assert_eq!(civil_from_days(20_751), (2026, 10, 25));
    assert_eq!(weekday_name(20_751), "Sun");
}

fn message<'a>(subject: &'a str, body: &'a str) -> Message<'a> {
    Message {
        from_name: "CalCraft",
        from: "reminders@example.org",
        to: "alice@example.org",
        subject,
        body,
        date: 1_792_929_600,
        id: "abc123",
    }
}

fn headers(rendered: &[u8]) -> String {
    let text = String::from_utf8(rendered.to_vec()).unwrap();
    text.split("\r\n\r\n").next().unwrap().to_owned()
}

#[test]
fn rendered_message() {
    let rendered = message("Reminder: Weekly sync", "Starts at 10:00.\n.\nBye")
        .render()
        .unwrap();
    let text = String::from_utf8(rendered.clone()).unwrap();
    assert!(text.starts_with("Date: "));
    assert!(text.contains("\r\nFrom: \"CalCraft\" <reminders@example.org>\r\n"));
    assert!(text.contains("\r\nTo: <alice@example.org>\r\n"));
    assert!(text.contains("\r\nSubject: Reminder: Weekly sync\r\n"));
    assert!(text.contains("\r\nMessage-ID: <abc123@example.org>\r\n"));
    assert!(text.contains("\r\nAuto-Submitted: auto-generated\r\n"));
    assert!(text.contains("\r\nContent-Type: text/plain; charset=utf-8\r\n"));
    assert!(text.ends_with("\r\n\r\nStarts at 10:00.\r\n=2E\r\nBye\r\n"));
    check_message(&rendered).unwrap();
}

#[test]
fn header_injection_is_impossible() {
    // Through the subject: CR/LF become spaces; the subject stays one header.
    let rendered = message("Hi\r\nBcc: evil@example.org\r\n\r\nInjected body", "x")
        .render()
        .unwrap();
    let head = headers(&rendered);
    assert!(!head.contains("\r\nBcc:"), "{head}");
    assert_eq!(head.matches("\r\n").count(), 9);
    // Through the addresses: refused.
    for to in [
        "alice@example.org\r\nBcc: evil@example.org",
        "alice@example.org>, <evil@example.org",
        "alice@example.org\n",
    ] {
        let mut m = message("s", "b");
        m.to = to;
        assert_eq!(m.render(), Err(MailError::InvalidAddress), "{to:?}");
    }
    // Through the display name: quoted or encoded, never raw.
    let mut m = message("s", "b");
    m.from_name = "Cal\"Craft\r\nBcc: x@y.z";
    let head = headers(&m.render().unwrap());
    assert!(head.contains("From: =?UTF-8?B?"), "{head}");
    assert!(!head.contains("Bcc"));
    // Through the message id.
    let mut m = message("s", "b");
    m.id = "a>\r\nBcc: x";
    assert_eq!(m.render(), Err(MailError::InvalidMessageId));
}

#[test]
fn non_ascii_subjects_and_long_lines() {
    let subject = format!("Erinnerung: {}", "Größenwahn ".repeat(30));
    let rendered = message(&subject, &"lang ".repeat(500)).render().unwrap();
    check_message(&rendered).unwrap();
    let head = headers(&rendered);
    assert!(head.contains("Subject: =?UTF-8?B?"));
    for line in String::from_utf8(rendered).unwrap().split("\r\n") {
        assert!(line.len() <= 78, "{line}");
    }
}

#[test]
fn message_checks() {
    assert!(check_message(b"Subject: x\r\n\r\nbody\r\n").is_ok());
    assert!(check_message(b"Subject: x\r\n folded\r\n\r\nbody\r\n").is_ok());
    for bad in [
        &b"Subject: x\r\n\r\nbare\nlf\r\n"[..],
        b"Subject: x\r\n\r\nbare\rcr\r\n",
        b"Subject: x\r\n\r\nno final crlf",
        b"no colon\r\n\r\nbody\r\n",
        b" continuation first\r\n\r\nbody\r\n",
        b"\r\nbody\r\n",
        b"Subject: x\r\n",
        b"Sub ject: x\r\n\r\nb\r\n",
        b"Subject: \0\r\n\r\nb\r\n",
    ] {
        assert!(
            check_message(bad).is_err(),
            "{:?}",
            String::from_utf8_lossy(bad)
        );
    }
    let long = format!("Subject: x\r\n\r\n{}\r\n", "y".repeat(MAX_LINE + 1));
    assert_eq!(check_message(long.as_bytes()), Err(MailError::LineTooLong));
}

#[test]
fn dot_stuffing() {
    assert_eq!(
        dot_stuff(b"a\r\n.\r\n..b\r\nc\r\n").unwrap(),
        b"a\r\n..\r\n...b\r\nc\r\n.\r\n"
    );
    assert_eq!(dot_stuff(b"").unwrap(), b".\r\n");
    assert!(dot_stuff(b"a\nb\r\n").is_err());
    assert!(dot_stuff(b"a\r\nb").is_err());
    assert!(dot_stuff(b"a\0\r\n").is_err());
    let long = format!("{}\r\n", "x".repeat(MAX_LINE + 1));
    assert_eq!(dot_stuff(long.as_bytes()), Err(MailError::LineTooLong));
}
