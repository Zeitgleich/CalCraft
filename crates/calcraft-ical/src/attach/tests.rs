use super::*;
use crate::{parse, write::to_string};

const SERIES: &str = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//Test//EN\r\n\
BEGIN:VEVENT\r\nUID:s1\r\nDTSTAMP:20260101T000000Z\r\nDTSTART:20260105T100000Z\r\n\
DTEND:20260105T110000Z\r\nRRULE:FREQ=DAILY;COUNT=5\r\nSUMMARY:Series\r\n\
BEGIN:VALARM\r\nACTION:DISPLAY\r\nTRIGGER:-PT5M\r\nDESCRIPTION:x\r\nEND:VALARM\r\n\
END:VEVENT\r\n\
BEGIN:VEVENT\r\nUID:s1\r\nDTSTAMP:20260101T000000Z\r\nRECURRENCE-ID:20260106T100000Z\r\n\
DTSTART:20260106T120000Z\r\nDTEND:20260106T130000Z\r\nSUMMARY:Moved\r\nEND:VEVENT\r\n\
END:VCALENDAR\r\n";

fn attach(id: &str) -> Property {
    managed_property(
        &format!("https://cal.example/dav/attachments/{id}"),
        id,
        "report.pdf",
        "application/pdf",
        42,
    )
}

fn ids_of(component: &Component) -> Vec<String> {
    component
        .properties
        .iter()
        .filter_map(managed_id)
        .map(str::to_owned)
        .collect()
}

#[test]
fn add_without_rid_reaches_every_instance() {
    let mut calendar = parse(SERIES.as_bytes()).unwrap();
    add(&mut calendar, &attach("a1"), None).unwrap();
    assert_eq!(managed_ids(&calendar), ["a1"]);
    for component in &calendar.components {
        assert_eq!(ids_of(component), ["a1"]);
    }
    let text = to_string(&calendar).unwrap();
    assert!(text.contains("MANAGED-ID=a1"), "{text}");
    assert!(text.contains("FILENAME=report.pdf"), "{text}");
    assert!(text.contains("SIZE=42"), "{text}");
    assert!(crate::CalendarObject::parse(text.as_bytes()).is_ok());
    assert_eq!(
        crate::CalendarObject::parse(text.as_bytes())
            .unwrap()
            .managed_ids,
        ["a1"]
    );
}

#[test]
fn rid_selects_master_overrides_and_creates_new_ones() {
    let mut calendar = parse(SERIES.as_bytes()).unwrap();
    let rids = parse_rids("M").unwrap();
    add(&mut calendar, &attach("a1"), Some(&rids)).unwrap();
    assert_eq!(ids_of(&calendar.components[0]), ["a1"]);
    assert!(ids_of(&calendar.components[1]).is_empty());

    // An existing override and a new one (20260108), in one request.
    let rids = parse_rids("20260106T100000Z,20260108T100000Z").unwrap();
    add(&mut calendar, &attach("a2"), Some(&rids)).unwrap();
    assert_eq!(calendar.components.len(), 3);
    assert_eq!(ids_of(&calendar.components[1]), ["a2"]);
    let created = &calendar.components[2];
    assert_eq!(
        created.property("RECURRENCE-ID").unwrap().value,
        "20260108T100000Z"
    );
    assert_eq!(
        created.property("DTSTART").unwrap().value,
        "20260108T100000Z"
    );
    assert!(created.property("RRULE").is_none());
    // The new override keeps the master's alarm and its attachments.
    assert!(created.components.iter().any(|c| c.name == "VALARM"));
    assert_eq!(ids_of(created), ["a1", "a2"]);
    let text = to_string(&calendar).unwrap();
    assert!(
        crate::CalendarObject::parse(text.as_bytes()).is_ok(),
        "{text}"
    );
}

#[test]
fn invalid_rids_are_refused() {
    let mut calendar = parse(SERIES.as_bytes()).unwrap();
    for rid in [
        "20260120T100000Z",
        "20260105T100001Z",
        "x",
        "M,M",
        "M,m",
        "",
        "20260106T100000Z,20260106T100000Z",
    ] {
        let result = parse_rids(rid).and_then(|rids| add(&mut calendar, &attach("a"), Some(&rids)));
        assert_eq!(result, Err(AttachError::InvalidRid), "{rid}");
    }
    assert_eq!(calendar, parse(SERIES.as_bytes()).unwrap());
    assert!(parse_rids(&vec!["20260105T100000Z"; MAX_RIDS + 1].join(",")).is_err());
    // A single event has no instances besides its master.
    let mut single = parse(
        b"BEGIN:VCALENDAR\r\nVERSION:2.0\r\nBEGIN:VEVENT\r\nUID:x\r\nDTSTART:20260105T100000Z\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n",
    )
    .unwrap();
    let rids = parse_rids("20260105T100000Z").unwrap();
    // The only instance is the master's own start.
    assert!(add(&mut single, &attach("a"), Some(&rids)).is_ok());
    let rids = parse_rids("20260106T100000Z").unwrap();
    assert_eq!(
        add(&mut single, &attach("a"), Some(&rids)),
        Err(AttachError::InvalidRid)
    );
}

#[test]
fn remove_and_replace() {
    let mut calendar = parse(SERIES.as_bytes()).unwrap();
    add(&mut calendar, &attach("a1"), None).unwrap();
    add(&mut calendar, &attach("a2"), None).unwrap();
    // Remove one instance only: a new override without it.
    let rids = parse_rids("20260107T100000Z").unwrap();
    remove(&mut calendar, "a1", Some(&rids)).unwrap();
    assert_eq!(ids_of(&calendar.components[2]), ["a2"]);
    assert_eq!(ids_of(&calendar.components[0]), ["a1", "a2"]);
    // Not on the master: no override is created for it.
    let rids = parse_rids("20260109T100000Z").unwrap();
    assert_eq!(
        remove(&mut calendar, "zz", Some(&rids)),
        Err(AttachError::UnknownManagedId)
    );
    assert_eq!(calendar.components.len(), 3);

    replace(&mut calendar, "a2", &attach("a3")).unwrap();
    assert_eq!(managed_ids(&calendar), ["a1", "a3"]);
    assert_eq!(
        replace(&mut calendar, "a2", &attach("a4")),
        Err(AttachError::UnknownManagedId)
    );
    remove(&mut calendar, "a1", None).unwrap();
    assert_eq!(managed_ids(&calendar), ["a3"]);
    assert_eq!(
        remove(&mut calendar, "a1", None),
        Err(AttachError::UnknownManagedId)
    );
    assert!(strip_managed(&mut calendar));
    assert!(managed_ids(&calendar).is_empty());
    assert!(!strip_managed(&mut calendar));
}

#[test]
fn canonicalize_rewrites_known_attachments_only() {
    let text = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nBEGIN:VEVENT\r\nUID:x\r\nDTSTART:20260105T100000Z\r\n\
ATTACH;MANAGED-ID=k1;SIZE=1;FILENAME=evil.exe:https://evil.example/x\r\n\
ATTACH;MANAGED-ID=u1:https://other.example/y\r\nATTACH:https://plain.example/z\r\n\
END:VEVENT\r\nEND:VCALENDAR\r\n";
    let mut calendar = parse(text.as_bytes()).unwrap();
    assert_eq!(managed_ids(&calendar), ["k1", "u1"]);
    let changed = canonicalize(&mut calendar, &|id| (id == "k1").then(|| attach("k1")));
    assert!(changed);
    let event = &calendar.components[0];
    let attaches: Vec<&Property> = event.properties_named("ATTACH").collect();
    assert_eq!(*attaches[0], attach("k1"));
    assert_eq!(attaches[1].value, "https://other.example/y");
    assert_eq!(attaches[2].value, "https://plain.example/z");
    assert!(!canonicalize(&mut calendar, &|id| (id == "k1").then(|| attach("k1"))));
}

#[test]
fn content_disposition_file_names() {
    let cases: &[(&str, Option<&str>)] = &[
        ("attachment; filename=report.pdf", Some("report.pdf")),
        ("attachment;filename=\"a b.pdf\"", Some("a b.pdf")),
        (
            "attachment; filename=\"q\\\"uote.txt\"",
            Some("q\"uote.txt"),
        ),
        (
            "attachment; filename=\"fallback.txt\"; filename*=UTF-8''%C3%B6l.txt",
            Some("öl.txt"),
        ),
        (
            "inline; filename*=utf-8'de'r%C3%A9sum%C3%A9.pdf",
            Some("résumé.pdf"),
        ),
        ("attachment; filename*=ISO-8859-1''%E4.txt", Some("ä.txt")),
        // Broken extended value: the plain one is used.
        (
            "attachment; filename*=UTF-8''%ZZ; filename=x.txt",
            Some("x.txt"),
        ),
        ("attachment; filename*=KOI8-R''%E4", None),
        ("attachment", None),
        ("attachment; filename=\"unterminated", None),
        ("; filename=x", None),
        ("attachment; filename=a;b", Some("a")),
        ("attachment; filename=\"a;b\"", Some("a;b")),
        ("attachment; filename=", None),
    ];
    for (header, expected) in cases {
        assert_eq!(
            disposition_filename(header).as_deref(),
            *expected,
            "{header}"
        );
    }
    assert!(disposition_filename(&format!("attachment; filename={}", "a".repeat(5000))).is_none());
}

#[test]
fn file_names_are_sanitised() {
    let cases: &[(&str, &str)] = &[
        ("report.pdf", "report.pdf"),
        ("../../etc/passwd", "passwd"),
        ("C:\\Users\\x\\evil.bat", "evil.bat"),
        ("..", DEFAULT_FILENAME),
        ("", DEFAULT_FILENAME),
        ("   ", DEFAULT_FILENAME),
        (".htaccess", "htaccess"),
        ("a\r\nSet-Cookie: x=1", "aSet-Cookie_ x=1"),
        ("doc\u{202E}fdp.exe", "docfdp.exe"),
        ("in\u{200B}visible\u{FEFF}.txt", "invisible.txt"),
        ("tab\there.txt", "tabhere.txt"),
        ("no\u{A0}break\u{2028}line.txt", "no break line.txt"),
        ("q\"uote.txt", "q_uote.txt"),
        ("a<b>c|d?e*f.txt", "a_b_c_d_e_f.txt"),
        ("nul\0.txt", "nul.txt"),
        ("trailing. . .", "trailing"),
    ];
    for (raw, expected) in cases {
        assert_eq!(sanitize_filename(raw), *expected, "{raw:?}");
    }
    let long = "ä".repeat(300);
    let name = sanitize_filename(&long);
    assert!(name.len() <= MAX_FILENAME_LEN);
    assert!(name.chars().all(|c| c == 'ä'));
}

#[test]
fn disposition_headers_are_plain_ascii() {
    for name in [
        "report.pdf",
        "öl \"quoted\" \\ back.txt",
        "a\r\nb",
        "日本語.txt",
        "%41.txt",
    ] {
        let header = disposition_header(name);
        assert!(header.starts_with("attachment; filename=\""), "{header}");
        assert!(
            header.bytes().all(|b| b.is_ascii_graphic() || b == b' '),
            "{header}"
        );
        // The fallback stays one quoted string.
        let fallback = header
            .strip_prefix("attachment; filename=\"")
            .unwrap()
            .split_once("\"; filename*=UTF-8''")
            .unwrap();
        assert!(!fallback.0.contains(['"', '\\']), "{header}");
        // The exact name round-trips through the RFC 8187 value.
        let parsed = disposition_filename(&header).unwrap();
        assert_eq!(parsed, name, "{header}");
    }
}

#[test]
fn media_types() {
    assert_eq!(
        media_type("application/PDF").as_deref(),
        Some("application/pdf")
    );
    assert_eq!(
        media_type("text/plain; charset=utf-8").as_deref(),
        Some("text/plain")
    );
    for bad in [
        "",
        "text",
        "text/",
        "/plain",
        "te xt/plain",
        "a/b/c",
        "text/pl\"ain",
    ] {
        assert_eq!(media_type(bad), None, "{bad}");
    }
    assert_eq!(served_media_type("application/pdf"), "application/pdf");
    for dangerous in [
        "text/html",
        "image/svg+xml",
        "application/xhtml+xml",
        "text/xml",
        "application/xml",
        "application/javascript",
        "text/javascript",
        "application/x-unknown",
    ] {
        assert_eq!(served_media_type(dangerous), OCTET_STREAM, "{dangerous}");
    }
}
