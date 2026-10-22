use super::*;
use crate::parse;

#[test]
fn escapes_and_unescapes_text() {
    let text = "a\\b;c,d\r\ne\nf\rg\u{0}h\ti";
    let escaped = escape_text(text);
    assert_eq!(escaped, "a\\\\b\\;c\\,d\\ne\\nf\\ngh\ti");
    assert_eq!(unescape_text(&escaped), "a\\b;c,d\ne\nf\ngh\ti");
    assert_eq!(unescape_text("x\\Ny\\q\\"), "x\ny\\q\\");
}

#[test]
fn splits_at_unescaped_separators() {
    assert_eq!(
        split_unescaped("a\\,b,c,,d\\\\,e", ','),
        ["a\\,b", "c", "", "d\\\\", "e"]
    );
    assert_eq!(split_unescaped("", ';'), [""]);
}

#[test]
fn folds_at_75_octets_without_splitting_characters() {
    let summary = "ä".repeat(100);
    let property = Property {
        name: "SUMMARY".into(),
        parameters: vec![],
        value: summary.clone(),
    };
    let mut out = String::new();
    write_property(&property, &mut out).unwrap();
    for line in out.split("\r\n").filter(|l| !l.is_empty()) {
        assert!(line.len() <= 75, "{} octets", line.len());
    }
    let wrapped = format!("BEGIN:X\r\n{out}END:X\r\n");
    let parsed = parse(wrapped.as_bytes()).unwrap();
    assert_eq!(parsed.properties[0].value, summary);
}

#[test]
fn rejects_injection() {
    let bad_value = Property {
        name: "SUMMARY".into(),
        parameters: vec![],
        value: "x\r\nBEGIN:VEVENT".into(),
    };
    assert!(write_property(&bad_value, &mut String::new()).is_err());
    let bad_name = Property {
        name: "X:Y".into(),
        parameters: vec![],
        value: String::new(),
    };
    assert!(write_property(&bad_name, &mut String::new()).is_err());
    let quoted = Property {
        name: "ATTENDEE".into(),
        parameters: vec![Parameter {
            name: "CN".into(),
            values: vec!["Doe, \"J\": x".into()],
        }],
        value: "mailto:j@example.com".into(),
    };
    let mut out = String::new();
    write_property(&quoted, &mut out).unwrap();
    assert_eq!(out, "ATTENDEE;CN=\"Doe, 'J': x\":mailto:j@example.com\r\n");
}
