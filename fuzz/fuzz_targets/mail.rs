//! E-mail building blocks: SMTP reply parsing and message generation must
//! never panic, and
//!
//! * parsed replies have codes 200–599 and at most `MAX_REPLY_LINES` lines;
//! * a rendered message passes `check_message`, consists only of the fixed
//!   set of headers (nothing from the subject, the display name or the
//!   body can add one), and survives dot-stuffing: no line starts with a
//!   lone `.`, and removing the stuffing gives the message back.
//!
//! The input is server replies, a line `--`, a display name, a line `--`,
//! a subject, a line `--` and a body.
#![no_main]

use calcraft_core::mail::{
    Extensions, MAX_REPLY_LINES, Message, check_message, dot_stuff, parse_replies,
};
use libfuzzer_sys::fuzz_target;

const HEADERS: [&str; 10] = [
    "Date",
    "From",
    "To",
    "Subject",
    "Message-ID",
    "MIME-Version",
    "Content-Type",
    "Content-Transfer-Encoding",
    "Auto-Submitted",
    "X-Auto-Response-Suppress",
];

fuzz_target!(|data: &[u8]| {
    let (replies, rest) = match data.windows(4).position(|w| w == b"\n--\n") {
        Some(at) => (&data[..at], &data[at + 4..]),
        None => (data, &b""[..]),
    };
    if let Ok(parsed) = parse_replies(replies) {
        for reply in &parsed {
            assert!((200..=599).contains(&reply.code));
            assert!(!reply.lines.is_empty() && reply.lines.len() <= MAX_REPLY_LINES);
            let _ = Extensions::from_ehlo(reply);
        }
    }

    let Ok(text) = std::str::from_utf8(rest) else {
        return;
    };
    let mut parts = text.splitn(3, "\n--\n");
    let from_name = parts.next().unwrap_or_default();
    let subject = parts.next().unwrap_or_default();
    let body = parts.next().unwrap_or_default();
    let message = Message {
        from_name,
        from: "calendar@example.org",
        to: "alice@example.org",
        subject,
        body,
        date: 1_800_000_000,
        id: "fuzz",
    };
    let Ok(rendered) = message.render() else {
        return;
    };
    check_message(&rendered).expect("rendered message is valid");
    let text = std::str::from_utf8(&rendered).expect("ASCII message");
    assert!(text.is_ascii());
    let (head, _) = text.split_once("\r\n\r\n").expect("header section");
    for line in head.split("\r\n") {
        if line.starts_with(' ') {
            continue;
        }
        let name = line.split(':').next().unwrap_or_default();
        assert!(HEADERS.contains(&name), "unexpected header {name:?}");
    }
    let stuffed = dot_stuff(&rendered).expect("dot-stuffing");
    assert!(stuffed.ends_with(b"\r\n.\r\n"));
    let body = &stuffed[..stuffed.len() - 3];
    let mut unstuffed = Vec::new();
    for line in body.split_inclusive(|&b| b == b'\n') {
        assert_ne!(line, b".\r\n", "a lone dot would end DATA early");
        unstuffed.extend_from_slice(line.strip_prefix(b".").unwrap_or(line));
    }
    assert_eq!(unstuffed, rendered);
});
